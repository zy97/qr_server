use std::process::Command;
use tracing::info;

use crate::config::CONFIG;
use crate::err::CustomError;

/// 标签图片路径通过该环境变量传给 PowerShell 打印脚本
/// （-Command 模式下尾随参数不可靠，故不用 $args）
const IMAGE_PATH_ENV: &str = "QR_LABEL_IMAGE";

/// 通过 PowerShell System.Drawing 把标签 PNG 发送到打印机（master/typst/chrome 共用）。
/// 打印逻辑一比一移植自原 C# 打印服务的 Print(byte[] images)：
/// 自定义纸张 + StandardPrintController（跳过打印对话框），按可用宽度缩放、垂直偏移居中。
/// 先把图片写入临时文件，打印完成后由脚本自行删除。
pub fn print_label_png(png: &[u8]) -> Result<(), CustomError> {
    let print_config = &CONFIG.print;
    // PaperSize 单位为 1/100 英寸，与原 C# 代码的换算方式一致
    let paper_width = (print_config.paper_width / 2.54 * 100.0) as i32;
    let paper_height = (print_config.paper_height / 2.54 * 100.0) as i32;
    let script = build_print_script(&print_config.printer_name, paper_width, paper_height);

    let image_path = std::env::temp_dir().join(format!(
        "qr-label-{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::write(&image_path, png)?;

    // 只启动不等待，打印在后台进行
    Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .env(IMAGE_PATH_ENV, &image_path)
        .spawn()
        .map_err(|_| CustomError::PrinterNoFound)?;

    info!(printer = print_config.printer_name, "label sent to printer");
    Ok(())
}

/// 生成打印脚本；28 与 780 为原 C# 代码中的固定边距/垂直偏移（1/100 英寸单位），保持原值
fn build_print_script(printer_name: &str, paper_width: i32, paper_height: i32) -> String {
    // PowerShell 单引号字符串内的单引号需要双写转义
    let escaped_printer_name = printer_name.replace('\'', "''");
    format!(
        r#"Add-Type -AssemblyName System.Drawing;
$script:img = New-Object System.Drawing.Bitmap([System.IO.MemoryStream]::new([System.IO.File]::ReadAllBytes($env:{IMAGE_PATH_ENV})));
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
$script:img.Dispose();
Remove-Item -LiteralPath $env:QR_LABEL_IMAGE -Force;"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
    fn print_script_escapes_single_quotes() {
        let script = build_print_script("Bob's Printer", 416, 1169);
        assert!(script.contains("PrinterName = 'Bob''s Printer'"));
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
