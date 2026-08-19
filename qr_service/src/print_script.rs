//! 打印脚本生成：纸张、边距、任务监听规则都是业务逻辑，集中在 qr_service 维护，
//! 经 /ws/agent 的 render_ok 消息随标签 PNG 一起下发给 print-agent 执行。
//!
//! 脚本模板默认用内置的 BUILTIN_TEMPLATE；也可在 config.toml 的 [print] 段配置
//! script_template = "print_script.ps1" 指向自定义模板文件——纯静态内容，
//! 后期改脚本不用重新编译，改文件重启服务即可（每次打印实时读文件）。
//! 模板占位符（双花括号在 PowerShell 里无含义，不会冲突）：
//!   {{printer_name}}     打印机名（已按单引号转义）
//!   {{paper_width}}      纸张宽（1/100 英寸）
//!   {{paper_height}}     纸张高（1/100 英寸）
//!   {{margin}}           横向边距（1/100 英寸）
//!   {{y_offset}}         垂直偏移（1/100 英寸）
//!   {{queue_timeout}}    队列监听超时（秒）

use crate::config::CONFIG;

/// 原 C# 打印服务的固定参数：横向边距、垂直偏移（1/100 英寸）、队列监听超时（秒）
pub const DEFAULT_MARGIN: i32 = 28;
pub const DEFAULT_Y_OFFSET: i32 = 780;
pub const DEFAULT_QUEUE_TIMEOUT_SECS: i32 = 60;

/// 打印脚本参数覆盖（全部可选；None 时回退 config.toml / 原 C# 默认值）。
/// 由工位级设置提供（见 agent_ws / template_store::AgentSettings）
#[derive(Debug, Clone, Copy, Default)]
pub struct PrintOverrides {
    /// 自定义纸张宽度（cm）
    pub paper_width: Option<f64>,
    /// 自定义纸张高度（cm）
    pub paper_height: Option<f64>,
    /// 横向边距（1/100 英寸）
    pub margin: Option<i32>,
    /// 垂直偏移（1/100 英寸）
    pub y_offset: Option<i32>,
    /// 队列监听超时（秒）
    pub queue_timeout_secs: Option<i32>,
}

/// 内置打印脚本模板。打印逻辑移植自原 C# 打印服务的 Print(byte[] images)：
/// 自定义纸张 + StandardPrintController（跳过打印对话框），按可用宽度缩放、垂直偏移居中；
/// 28 与 780 为原 C# 代码中的固定边距/垂直偏移（1/100 英寸单位），保持原值。
/// 图片数据以 Base64 经标准输入传给 PowerShell 进程，全程在内存中传递，不落盘。
///
/// 任务级监听：$pd.Print() 返回仅表示任务已进打印队列，不代表真的打出来了——
/// 脱机/缺纸/卡纸时任务会带错误状态滞留在队列里。给任务起唯一 DocumentName 后轮询队列：
/// 出队=成功；带错误状态=失败（exit 1）；超时未出队=超时失败（exit 2）。
/// 小图片可能瞬间打完、首轮轮询前就已出队，2 秒宽限期内观察不到任务视为成功
const BUILTIN_TEMPLATE: &str = r#"[Console]::OutputEncoding = [Text.Encoding]::UTF8;
$ProgressPreference = 'SilentlyContinue';
Add-Type -AssemblyName System.Drawing;
$bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd());
$script:img = New-Object System.Drawing.Bitmap([System.IO.MemoryStream]::new($bytes));
[Console]::Error.WriteLine('stage: image-loaded');
$printerName = '{{printer_name}}';
if (-not $printerName) { $printerName = (Get-CimInstance Win32_Printer | Where-Object { $_.Default }).Name }
$pd = New-Object System.Drawing.Printing.PrintDocument;
$pd.PrinterSettings.PrinterName = $printerName;
$pd.DocumentName = 'print-agent-' + [guid]::NewGuid().ToString('N');
$pd.PrintController = New-Object System.Drawing.Printing.StandardPrintController;
$pd.DefaultPageSettings.PaperSize = New-Object System.Drawing.Printing.PaperSize('CustomSize', {{paper_width}}, {{paper_height}});
$pd.add_PrintPage({
    param($s, $e)
    $area = $e.PageBounds;
    $sc = ($area.Width - {{margin}}) / $script:img.Width;
    $w = [int]($script:img.Width * $sc);
    $h = [int]($script:img.Height * $sc);
    $y = [int](($area.Height - $h - {{y_offset}}) / 2);
    $e.Graphics.DrawImage($script:img, 0, $y, $w, $h);
});
$pd.Print();
[Console]::Error.WriteLine('stage: print-submitted');
$script:img.Dispose();
$deadline = (Get-Date).AddSeconds({{queue_timeout}});
$graceUntil = (Get-Date).AddSeconds(2);
$seen = $false;
while ((Get-Date) -lt $deadline) {
    $job = @(Get-PrintJob -PrinterName $printerName -ErrorAction SilentlyContinue | Where-Object { $_.DocumentName -eq $pd.DocumentName });
    if ($job.Count -gt 0) {
        if (-not $seen) { [Console]::Error.WriteLine('stage: job-queued') }
        $seen = $true;
        $status = $job[0].JobStatus.ToString();
        if ($status -match 'Error|Offline|PaperOut|NoToner|DoorOpen|Paused|Deleted|Blocked') {
            [Console]::Error.WriteLine("打印任务失败: $status");
            exit 1;
        }
    } elseif ($seen) {
        exit 0;
    } elseif ((Get-Date) -gt $graceUntil) {
        exit 0;
    }
    Start-Sleep -Milliseconds 500;
}
[Console]::Error.WriteLine('打印任务超时（{{queue_timeout}} 秒）仍未完成，请检查打印机状态');
exit 2;"#;

/// 用参数渲染模板：纯占位符文本替换，不做任何语法解析
fn render_template(
    template: &str,
    printer_name: &str,
    paper_width: i32,
    paper_height: i32,
    margin: i32,
    y_offset: i32,
    queue_timeout_secs: i32,
) -> String {
    // PowerShell 单引号字符串内的单引号需要双写转义
    let escaped_printer_name = printer_name.replace('\'', "''");
    template
        .replace("{{printer_name}}", &escaped_printer_name)
        .replace("{{paper_width}}", &paper_width.to_string())
        .replace("{{paper_height}}", &paper_height.to_string())
        .replace("{{margin}}", &margin.to_string())
        .replace("{{y_offset}}", &y_offset.to_string())
        .replace("{{queue_timeout}}", &queue_timeout_secs.to_string())
}

/// 加载脚本模板：配置了 script_template 文件则每次打印实时读取（改文件即生效），
/// 未配置或读取失败回退内置模板（打印链路不因模板文件问题中断）
fn load_template() -> String {
    let Some(path) = CONFIG.print.script_template.as_deref() else {
        return BUILTIN_TEMPLATE.to_string();
    };
    match std::fs::read_to_string(path) {
        // Windows 编辑器保存常带 BOM，剥掉以免混进脚本内容
        Ok(template) => template.trim_start_matches('\u{feff}').to_string(),
        Err(err) => {
            tracing::warn!(path, error = %err, "打印脚本模板读取失败，回退内置模板");
            BUILTIN_TEMPLATE.to_string()
        }
    }
}

/// 生成完整打印脚本（含任务监听）。printer_name 为空时打印到系统默认打印机。
/// 纸张宽高（cm）可由工位级设置覆盖；None 时回退服务器 config.toml 的 [print] 段
/// （cm → 1/100 英寸换算与原 C# 一致）
pub fn build_print_script(printer_name: &str, overrides: &PrintOverrides) -> String {
    let print_config = &CONFIG.print;
    let paper_width =
        (overrides.paper_width.unwrap_or(print_config.paper_width) / 2.54 * 100.0) as i32;
    let paper_height =
        (overrides.paper_height.unwrap_or(print_config.paper_height) / 2.54 * 100.0) as i32;
    render_template(
        &load_template(),
        printer_name,
        paper_width,
        paper_height,
        overrides.margin.unwrap_or(DEFAULT_MARGIN),
        overrides.y_offset.unwrap_or(DEFAULT_Y_OFFSET),
        overrides
            .queue_timeout_secs
            .unwrap_or(DEFAULT_QUEUE_TIMEOUT_SECS),
    )
}

/// 用内置模板生成脚本（测试与兜底用）
fn build_script(
    printer_name: &str,
    paper_width: i32,
    paper_height: i32,
    margin: i32,
    y_offset: i32,
    queue_timeout_secs: i32,
) -> String {
    render_template(
        BUILTIN_TEMPLATE,
        printer_name,
        paper_width,
        paper_height,
        margin,
        y_offset,
        queue_timeout_secs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_paper_override_wins_over_global_config() {
        // 15cm x 10cm -> 590 x 393 (1/100 英寸，向下取整)
        let script = build_print_script(
            "P1",
            &PrintOverrides {
                paper_width: Some(15.0),
                paper_height: Some(10.0),
                ..Default::default()
            },
        );
        assert!(script.contains("PaperSize('CustomSize', 590, 393)"));
        // 未覆盖时回退 config.toml 默认值 10.57 x 29.70 -> 416 x 1169
        let script = build_print_script("P1", &PrintOverrides::default());
        assert!(script.contains("PaperSize('CustomSize', 416, 1169)"));
    }

    #[test]
    fn script_params_override_wins_over_csharp_defaults() {
        let script = build_print_script(
            "P1",
            &PrintOverrides {
                margin: Some(40),
                y_offset: Some(900),
                queue_timeout_secs: Some(90),
                ..Default::default()
            },
        );
        assert!(script.contains("($area.Width - 40)"));
        assert!(script.contains("($area.Height - $h - 900) / 2"));
        assert!(script.contains("AddSeconds(90)"));
        assert!(script.contains("超时（90 秒）"));
        // 未覆盖时保持原 C# 固定值
        let script = build_print_script("P1", &PrintOverrides::default());
        assert!(script.contains("($area.Width - 28)"));
        assert!(script.contains("($area.Height - $h - 780) / 2"));
        assert!(script.contains("AddSeconds(60)"));
    }

    #[test]
    fn custom_template_substitutes_all_placeholders() {
        let template = "print '{{printer_name}}' on {{paper_width}}x{{paper_height}} \
                        margin={{margin}} y={{y_offset}} timeout={{queue_timeout}} { 保留花括号 }";
        let script = render_template(template, "Bob's Printer", 416, 1169, 28, 780, 60);
        assert_eq!(
            script,
            "print 'Bob''s Printer' on 416x1169 margin=28 y=780 timeout=60 { 保留花括号 }"
        );
    }

    #[test]
    fn print_script_matches_csharp_logic() {
        // 10.57cm x 29.70cm -> 416 x 1169 (1/100 英寸)
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169, 28, 780, 60);
        assert!(script.contains("PaperSize('CustomSize', 416, 1169)"));
        assert!(script.contains("($area.Width - 28)"));
        assert!(script.contains("($area.Height - $h - 780) / 2"));
        assert!(script.contains("StandardPrintController"));
        assert!(script.contains("$pd.PrinterSettings.PrinterName = $printerName"));
    }

    #[test]
    fn print_script_reads_image_from_stdin_without_temp_file() {
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169, 28, 780, 60);
        assert!(script.contains("Add-Type -AssemblyName System.Drawing"));
        assert!(script.contains("[Convert]::FromBase64String([Console]::In.ReadToEnd())"));
        assert!(script.contains("[System.IO.MemoryStream]::new($bytes)"));
        assert!(!script.contains("ReadAllBytes"));
        assert!(!script.contains("Remove-Item"));
    }

    #[test]
    fn print_script_escapes_single_quotes() {
        let script = build_script("Bob's Printer", 416, 1169, 28, 780, 60);
        assert!(script.contains("$printerName = 'Bob''s Printer';"));
    }

    #[test]
    fn empty_printer_falls_back_to_system_default() {
        let script = build_script("", 416, 1169, 28, 780, 60);
        assert!(script.contains("$printerName = '';"));
        assert!(script.contains("Get-CimInstance Win32_Printer | Where-Object { $_.Default }"));
    }

    #[test]
    fn print_script_monitors_job_until_result() {
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169, 28, 780, 60);
        // 唯一任务名 + 队列轮询 + 三类结局：错误状态、超时、出队成功
        assert!(script.contains("$pd.DocumentName = 'print-agent-'"));
        assert!(script.contains("Get-PrintJob -PrinterName $printerName"));
        assert!(script.contains("Error|Offline|PaperOut|NoToner|DoorOpen|Paused|Deleted|Blocked"));
        assert!(script.contains("exit 1"));
        assert!(script.contains("exit 2"));
        assert!(script.contains("exit 0"));
    }

    /// 需要本机有 powershell，仅在 Windows 上运行
    #[cfg(windows)]
    #[test]
    fn print_script_is_valid_powershell() {
        let script = build_script("ZDesigner ZT231-300dpi ZPL", 416, 1169, 28, 780, 60);
        let script_path = std::env::temp_dir().join(format!("qr-print-{}.ps1", std::process::id()));
        // Windows PowerShell 5.1 对无 BOM 文件按 ANSI 读取，中文会乱码——加 BOM 让它按 UTF-8 解析
        std::fs::write(&script_path, format!("\u{feff}{script}")).unwrap();

        // 仅做语法解析，不执行
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "$null = [System.Management.Automation.Language.Parser]::ParseFile('{}', [ref]$null, [ref]$null)",
                    script_path.display()
                ),
            ])
            .output()
            .expect("failed to run powershell");
        std::fs::remove_file(&script_path).ok();
        assert!(
            output.status.success(),
            "powershell parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}