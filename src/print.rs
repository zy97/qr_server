use std::process::Command;
use tracing::info;

use crate::config::CONFIG;
use crate::err::CustomError;

/// 通过 PowerShell System.Drawing 把标签 PNG 发送到打印机（master/typst/chrome 共用）。
/// 先把图片写入临时文件，打印完成后由脚本自行删除。
pub fn print_label_png(png: &[u8]) -> Result<(), CustomError> {
    let printer_name = &CONFIG.print.printer_name;
    let image_path = std::env::temp_dir().join(format!(
        "qr-label-{}-{}.png",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::write(&image_path, png)?;

    // PowerShell 单引号字符串内的单引号需要双写转义
    let escaped_printer_name = printer_name.replace('\'', "''");
    let script = format!(
        r#"Add-Type -AssemblyName System.Drawing;
$script:img = [System.Drawing.Image]::FromFile($args[0]);
$pd = New-Object System.Drawing.Printing.PrintDocument;
$pd.PrinterSettings.PrinterName = '{escaped_printer_name}';
$pd.add_PrintPage({{
    param($s, $e)
    $e.Graphics.DrawImage($script:img, 0, 0);
}});
$pd.Print();
$script:img.Dispose();
Remove-Item -LiteralPath $args[0] -Force;"#
    );

    // 只启动不等待，打印在后台进行
    Command::new("powershell")
        .args(["-Command", "-NoProfile", "-WindowStyle", "Hidden", &script])
        .arg(&image_path)
        .spawn()
        .map_err(|_| CustomError::PrinterNoFound)?;

    info!(printer = printer_name, "label sent to printer");
    Ok(())
}
