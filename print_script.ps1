# qr_service 打印脚本模板（样例，与内置模板一致）
# 用法：复制本文件到服务器 /opt/qr_service/print_script.ps1，
#       并在 config.toml 的 [print] 段加 script_template = "print_script.ps1"
# 占位符（运行时替换）：{{printer_name}} {{paper_width}} {{paper_height}} {{margin}} {{y_offset}} {{queue_timeout}}
# 说明：图像数据以 Base64 经标准输入传入；任务级监听打印队列，出队=成功、错误状态=失败、超时=失败
[Console]::OutputEncoding = [Text.Encoding]::UTF8;
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
exit 2;