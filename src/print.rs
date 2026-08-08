use std::io::Write;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;

use base64::{engine::general_purpose, Engine};
use tracing::{error, info};

use crate::config::CONFIG;
use crate::err::CustomError;

/// 通过 PowerShell System.Drawing 把标签 PNG 发送到打印机（master/typst/chrome 共用）。
/// 打印逻辑一比一移植自原 C# 打印服务的 Print(byte[] images)：
/// 自定义纸张 + StandardPrintController（跳过打印对话框），按可用宽度缩放、垂直偏移居中。
/// 图片数据以 Base64 经标准输入传给 PowerShell 进程，全程在内存中传递，不落盘。
pub fn print_label_png(png: &[u8]) -> Result<(), CustomError> {
    let print_config = &CONFIG.print;
    // PaperSize 单位为 1/100 英寸，与原 C# 代码的换算方式一致
    let paper_width = (print_config.paper_width / 2.54 * 100.0) as i32;
    let paper_height = (print_config.paper_height / 2.54 * 100.0) as i32;
    let script = build_print_script(&print_config.printer_name, paper_width, paper_height);
    let encoded = general_purpose::STANDARD.encode(png);

    // spawn + 写 stdin 放到后台线程：图片 Base64 通常超过匿名管道缓冲区，
    // 子进程要等 PowerShell 启动并执行到 ReadToEnd 才会读取，同步写会阻塞请求路径
    let _ = dispatch_print(script, encoded);

    info!(printer = print_config.printer_name, "label sent to printer");
    Ok(())
}

/// 后台线程启动 PowerShell 打印进程并写入图片数据；返回句柄仅供测试等待结果
fn dispatch_print(script: String, encoded: String) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let result = run_print_process(&script, &encoded);
        if let Err(error) = result {
            error!(error = %error, "failed to dispatch label to printer");
        }
    })
}

fn run_print_process(script: &str, encoded: &str) -> std::io::Result<()> {
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", script])
        .stdin(Stdio::piped())
        .spawn()?;

    // 写完即关闭标准输入，打印在后台进行，不等待打印完成
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(encoded.as_bytes())?;
    drop(stdin);
    Ok(())
}

/// 生成打印脚本；28 与 780 为原 C# 代码中的固定边距/垂直偏移（1/100 英寸单位），保持原值
fn build_print_script(printer_name: &str, paper_width: i32, paper_height: i32) -> String {
    // PowerShell 单引号字符串内的单引号需要双写转义
    let escaped_printer_name = printer_name.replace('\'', "''");
    format!(
        r#"Add-Type -AssemblyName System.Drawing;
$bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd());
$script:img = New-Object System.Drawing.Bitmap([System.IO.MemoryStream]::new($bytes));
$pd = New-Object System.Drawing.Printing.PrintDocument;
$pd.PrinterSettings.PrinterName = '{escaped_printer_name}';
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
$script:img.Dispose();"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

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
    }

    #[test]
    fn powershell_roundtrips_png_bytes_from_stdin() {
        // 模拟打印脚本的数据入口：Base64 经标准输入还原后与原始字节一致
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

    #[test]
    fn dispatch_print_does_not_block_on_full_pipe() {
        // 数据量远超匿名管道缓冲区，且子进程延迟 3 秒才开始读 stdin；
        // dispatch 应立即返回，不阻塞调用方
        let encoded = "A".repeat(256 * 1024);
        let script = "Start-Sleep -Seconds 3; [Console]::In.ReadToEnd() | Out-Null".to_string();

        let started = Instant::now();
        let handle = dispatch_print(script, encoded);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "dispatch_print should return without waiting for the child to drain stdin"
        );

        handle.join().expect("print thread should finish");
    }

    #[test]
    fn print_script_is_valid_powershell() {
        let script = build_print_script("ZDesigner ZT231-300dpi ZPL", 416, 1169);
        let script_path = std::env::temp_dir().join(format!("qr-print-{}.ps1", std::process::id()));
        let mut file = std::fs::File::create(&script_path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        drop(file);

        // 仅做语法解析，不执行
        let output = Command::new("powershell")
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
