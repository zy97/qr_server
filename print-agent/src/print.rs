use std::io::Write;
use std::process::{Command, Stdio};

use base64::{engine::general_purpose, Engine};
use tracing::info;

use crate::config::CONFIG;

/// 通过 PowerShell System.Drawing 把标签 PNG 发送到打印机，并监听打印任务直到出结果。
/// 打印逻辑移植自原 C# 打印服务的 Print(byte[] images)（经 qr_service 中转至此）：
/// 自定义纸张 + StandardPrintController（跳过打印对话框），按可用宽度缩放、垂直偏移居中。
/// 图片数据以 Base64 经标准输入传给 PowerShell 进程，全程在内存中传递，不落盘。
/// 同步等待任务出结果（通常 1~2 秒；打印机异常时最坏 60 秒超时），成功/失败真实返回
pub fn print_png(png: &[u8], printer_name: &str) -> anyhow::Result<()> {
    let print_config = &CONFIG.print;
    // PaperSize 单位为 1/100 英寸，与原 C# 代码的换算方式一致
    let paper_width = (print_config.paper_width / 2.54 * 100.0) as i32;
    let paper_height = (print_config.paper_height / 2.54 * 100.0) as i32;
    let script = build_print_script(printer_name, paper_width, paper_height);
    let encoded = general_purpose::STANDARD.encode(png);

    run_print_process(&script, &encoded)?;
    info!(printer = printer_name, bytes = png.len(), "label printed");
    Ok(())
}

fn run_print_process(script: &str, encoded: &str) -> anyhow::Result<()> {
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", script])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // 写完即关闭标准输入，PowerShell 读到 EOF 后开始打印
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(encoded.as_bytes())?;
    drop(stdin);

    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        print_failure_message(output.status.code(), &output.stderr)
    );
    Ok(())
}

/// 脚本报错写在 stderr；为空时（如 powershell 自身异常）给兜底描述
fn print_failure_message(exit_code: Option<i32>, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        format!("powershell 打印进程异常退出（退出码 {:?}）", exit_code)
    } else {
        detail
    }
}

/// 生成打印脚本；28 与 780 为原 C# 代码中的固定边距/垂直偏移（1/100 英寸单位），保持原值。
/// 任务级监听说明：$pd.Print() 返回仅表示任务已进打印队列，不代表真的打出来了——
/// 脱机/缺纸/卡纸时任务会带错误状态滞留在队列里。给任务起唯一 DocumentName 后轮询队列：
/// 出队=成功；带错误状态=失败（exit 1）；60s 未出队=超时失败（exit 2）。
/// 小图片可能瞬间打完、首轮轮询前就已出队，2 秒宽限期内观察不到任务视为成功
fn build_print_script(printer_name: &str, paper_width: i32, paper_height: i32) -> String {
    // PowerShell 单引号字符串内的单引号需要双写转义
    let escaped_printer_name = printer_name.replace('\'', "''");
    format!(
        r#"[Console]::OutputEncoding = [Text.Encoding]::UTF8;
Add-Type -AssemblyName System.Drawing;
$bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd());
$script:img = New-Object System.Drawing.Bitmap([System.IO.MemoryStream]::new($bytes));
$pd = New-Object System.Drawing.Printing.PrintDocument;
$pd.PrinterSettings.PrinterName = '{escaped_printer_name}';
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
    $job = @(Get-PrintJob -PrinterName '{escaped_printer_name}' -ErrorAction SilentlyContinue | Where-Object {{ $_.DocumentName -eq $pd.DocumentName }});
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
    fn print_script_matches_csharp_logic() {
        // 10.57cm x 29.70cm -> 416 x 1169 (1/100 英寸)
        let script = build_print_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        assert!(script.contains("PrinterName = 'ZDesigner ZT231-300dpi ZPL'"));
        assert!(script.contains("PaperSize('CustomSize', 416, 1169)"));
        assert!(script.contains("($area.Width - 28)"));
        assert!(script.contains("($area.Height - $h - 780) / 2"));
        assert!(script.contains("StandardPrintController"));
    }

    #[test]
    fn print_script_reads_image_from_stdin_without_temp_file() {
        let script = build_print_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        assert!(script.contains("Add-Type -AssemblyName System.Drawing"));
        assert!(script.contains("[Convert]::FromBase64String([Console]::In.ReadToEnd())"));
        assert!(script.contains("[System.IO.MemoryStream]::new($bytes)"));
        assert!(!script.contains("ReadAllBytes"));
        assert!(!script.contains("Remove-Item"));
    }

    #[test]
    fn print_script_escapes_single_quotes() {
        let script = build_print_script("Bob's Printer", 416, 1169);
        assert!(script.contains("PrinterName = 'Bob''s Printer'"));
        assert!(script.contains("Get-PrintJob -PrinterName 'Bob''s Printer'"));
    }

    #[test]
    fn print_script_monitors_job_until_result() {
        let script = build_print_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        // 唯一任务名 + 队列轮询 + 三类结局：错误状态、超时、出队成功
        assert!(script.contains("$pd.DocumentName = 'print-agent-'"));
        assert!(script.contains("Get-PrintJob -PrinterName"));
        assert!(script.contains("Where-Object { $_.DocumentName -eq $pd.DocumentName }"));
        assert!(script.contains("Error|Offline|PaperOut|NoToner|DoorOpen|Paused|Deleted|Blocked"));
        assert!(script.contains("exit 1"));
        assert!(script.contains("exit 2"));
        assert!(script.contains("Start-Sleep -Milliseconds 500"));
    }

    #[test]
    fn failure_message_prefers_stderr_detail() {
        assert_eq!(
            print_failure_message(Some(1), "打印任务失败: Offline".as_bytes()),
            "打印任务失败: Offline"
        );
        assert!(print_failure_message(Some(1), b"").contains("退出码"));
    }

    /// 以下两个测试需要本机有 powershell，仅在 Windows 上运行
    #[cfg(windows)]
    #[test]
    fn powershell_roundtrips_png_bytes_from_stdin() {
        let png: &[u8] = b"\x89PNG\r\n\x1a\nfake-label-image";
        let encoded = general_purpose::STANDARD.encode(png);

        let mut child = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "$bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd()); [Convert]::ToBase64String($bytes)",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to run powershell");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(encoded.as_bytes())
            .expect("failed to write stdin");

        let output = child.wait_with_output().expect("failed to wait");
        assert!(output.status.success());
        let roundtripped = String::from_utf8(output.stdout).expect("stdout should be utf8");
        assert_eq!(roundtripped.trim(), encoded);
    }

    #[cfg(windows)]
    #[test]
    fn print_script_is_valid_powershell() {
        let script = build_print_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        let script_path = std::env::temp_dir().join(format!("print-agent-{}.ps1", std::process::id()));
        // Windows PowerShell 5.1 对无 BOM 文件按 ANSI 读取，中文会乱码——加 BOM 让它按 UTF-8 解析
        std::fs::write(&script_path, format!("\u{feff}{script}")).unwrap();

        // 仅做语法解析，不执行
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[scriptblock]::Create((Get-Content -LiteralPath $env:PRINT_AGENT_SCRIPT -Raw)) | Out-Null",
            ])
            .env("PRINT_AGENT_SCRIPT", &script_path)
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