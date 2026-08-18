//! 打印脚本生成：纸张、边距、任务监听规则都是业务逻辑，集中在 qr_service 维护，
//! 经 /ws/agent 的 render_ok 消息随标签 PNG 一起下发给 print-agent 执行。
//! 调整打印行为只需升级服务器，工位 agent 不用动。

use crate::config::CONFIG;

/// 生成完整打印脚本（含任务监听）。printer_name 为空时打印到系统默认打印机。
/// 纸张宽高（cm）可由工位级设置覆盖；None 时回退服务器 config.toml 的 [print] 段
/// （cm → 1/100 英寸换算与原 C# 一致）
pub fn build_print_script(
    printer_name: &str,
    paper_width: Option<f64>,
    paper_height: Option<f64>,
) -> String {
    let print_config = &CONFIG.print;
    let paper_width = (paper_width.unwrap_or(print_config.paper_width) / 2.54 * 100.0) as i32;
    let paper_height = (paper_height.unwrap_or(print_config.paper_height) / 2.54 * 100.0) as i32;
    build_script(printer_name, paper_width, paper_height)
}

/// 打印逻辑移植自原 C# 打印服务的 Print(byte[] images)：
/// 自定义纸张 + StandardPrintController（跳过打印对话框），按可用宽度缩放、垂直偏移居中；
/// 28 与 780 为原 C# 代码中的固定边距/垂直偏移（1/100 英寸单位），保持原值。
/// 图片数据以 Base64 经标准输入传给 PowerShell 进程，全程在内存中传递，不落盘。
///
/// 任务级监听：$pd.Print() 返回仅表示任务已进打印队列，不代表真的打出来了——
/// 脱机/缺纸/卡纸时任务会带错误状态滞留在队列里。给任务起唯一 DocumentName 后轮询队列：
/// 出队=成功；带错误状态=失败（exit 1）；60s 未出队=超时失败（exit 2）。
/// 小图片可能瞬间打完、首轮轮询前就已出队，2 秒宽限期内观察不到任务视为成功
fn build_script(printer_name: &str, paper_width: i32, paper_height: i32) -> String {
    // PowerShell 单引号字符串内的单引号需要双写转义
    let escaped_printer_name = printer_name.replace('\'', "''");
    format!(
        r#"[Console]::OutputEncoding = [Text.Encoding]::UTF8;
Add-Type -AssemblyName System.Drawing;
$bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd());
$script:img = New-Object System.Drawing.Bitmap([System.IO.MemoryStream]::new($bytes));
$printerName = '{escaped_printer_name}';
if (-not $printerName) {{ $printerName = (Get-CimInstance Win32_Printer | Where-Object {{ $_.Default }}).Name }}
$pd = New-Object System.Drawing.Printing.PrintDocument;
$pd.PrinterSettings.PrinterName = $printerName;
$pd.DocumentName = 'print-agent-' + [guid]::NewGuid().ToString('N');
$pd.PrintController = New-Object System.Drawing.Printing.StandardPrintController;
$pd.DefaultPageSettings.PaperSize = New-Object System.Drawing.Printing.PaperSize('CustomSize', {paper_width}, {paper_height});
$pd.add_PrintPage({{
    param($s, $e)
    $area = $e.PageBounds;
    $sc = ($area.Width - 28) / $script:img.Width;
    $w = [int]($script:img.Width * $sc);
    $h = [int]($script:img.Height * $sc);
    $y = [int](($area.Height - $h - 780) / 2);
    $e.Graphics.DrawImage($script:img, 0, $y, $w, $h);
}});
$pd.Print();
$script:img.Dispose();
$deadline = (Get-Date).AddSeconds(60);
$graceUntil = (Get-Date).AddSeconds(2);
$seen = $false;
while ((Get-Date) -lt $deadline) {{
    $job = @(Get-PrintJob -PrinterName $printerName -ErrorAction SilentlyContinue | Where-Object {{ $_.DocumentName -eq $pd.DocumentName }});
    if ($job.Count -gt 0) {{
        $seen = $true;
        $status = $job[0].JobStatus.ToString();
        if ($status -match 'Error|Offline|PaperOut|NoToner|DoorOpen|Paused|Deleted|Blocked') {{
            [Console]::Error.WriteLine("打印任务失败: $status");
            exit 1;
        }}
    }} elseif ($seen) {{
        exit 0;
    }} elseif ((Get-Date) -gt $graceUntil) {{
        exit 0;
    }}
    Start-Sleep -Milliseconds 500;
}}
[Console]::Error.WriteLine('打印任务超时（60 秒）仍未完成，请检查打印机状态');
exit 2;"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_paper_override_wins_over_global_config() {
        // 15cm x 10cm -> 590 x 393 (1/100 英寸，向下取整)
        let script = build_print_script("P1", Some(15.0), Some(10.0));
        assert!(script.contains("PaperSize('CustomSize', 590, 393)"));
        // 未覆盖时回退 config.toml 默认值 10.57 x 29.70 -> 416 x 1169
        let script = build_print_script("P1", None, None);
        assert!(script.contains("PaperSize('CustomSize', 416, 1169)"));
    }
    #[test]
    fn print_script_matches_csharp_logic() {
        // 10.57cm x 29.70cm -> 416 x 1169 (1/100 英寸)
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        assert!(script.contains("PaperSize('CustomSize', 416, 1169)"));
        assert!(script.contains("($area.Width - 28)"));
        assert!(script.contains("($area.Height - $h - 780) / 2"));
        assert!(script.contains("StandardPrintController"));
        assert!(script.contains("$pd.PrinterSettings.PrinterName = $printerName"));
    }

    #[test]
    fn print_script_reads_image_from_stdin_without_temp_file() {
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        assert!(script.contains("Add-Type -AssemblyName System.Drawing"));
        assert!(script.contains("[Convert]::FromBase64String([Console]::In.ReadToEnd())"));
        assert!(script.contains("[System.IO.MemoryStream]::new($bytes)"));
        assert!(!script.contains("ReadAllBytes"));
        assert!(!script.contains("Remove-Item"));
    }

    #[test]
    fn print_script_escapes_single_quotes() {
        let script = build_script("Bob's Printer", 416, 1169);
        assert!(script.contains("$printerName = 'Bob''s Printer';"));
    }

    #[test]
    fn empty_printer_falls_back_to_system_default() {
        let script = build_script("", 416, 1169);
        assert!(script.contains("$printerName = '';"));
        assert!(script.contains("Get-CimInstance Win32_Printer | Where-Object { $_.Default }"));
    }

    #[test]
    fn print_script_monitors_job_until_result() {
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        // 唯一任务名 + 队列轮询 + 三类结局：错误状态、超时、出队成功
        assert!(script.contains("$pd.DocumentName = 'print-agent-'"));
        assert!(script.contains("Get-PrintJob -PrinterName $printerName"));
        assert!(script.contains("Error|Offline|PaperOut|NoToner|DoorOpen|Paused|Deleted|Blocked"));
        assert!(script.contains("exit 1"));
        assert!(script.contains("exit 2"));
        assert!(script.contains("Start-Sleep -Milliseconds 500"));
    }

    /// 需要本机有 powershell，仅在 Windows 上运行
    #[cfg(windows)]
    #[test]
    fn print_script_is_valid_powershell() {
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        let script_path = std::env::temp_dir().join(format!("qr-print-{}.ps1", std::process::id()));
        // Windows PowerShell 5.1 对无 BOM 文件按 ANSI 读取，中文会乱码——加 BOM 让它按 UTF-8 解析
        std::fs::write(&script_path, format!("\u{feff}{script}")).unwrap();

        // 仅做语法解析，不执行
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[scriptblock]::Create((Get-Content -LiteralPath $env:QR_PRINT_SCRIPT -Raw)) | Out-Null",
            ])
            .env("QR_PRINT_SCRIPT", &script_path)
            .output()
            .expect("failed to run powershell");
        let _ = std::fs::remove_file(&script_path);

        assert!(
            output.status.success(),
            "powershell parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
