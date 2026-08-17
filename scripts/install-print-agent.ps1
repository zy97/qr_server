# print-agent 一键安装（标签打印代理，Windows）
# 用法（管理员 PowerShell）：
#   irm https://raw.githubusercontent.com/zy97/qr_server/main/scripts/install-print-agent.ps1 | iex
# 或下载后带参数运行：
#   .\install-print-agent.ps1 -Version 0.1.0 -PrinterName "ZDesigner ZT231-300dpi ZPL" -Port 9195
# 重复执行即升级：会先停服务、替换程序、再启动；已有 print-agent.toml 不会被覆盖
[CmdletBinding()]
param(
    [string]$Version = "",                                      # 如 0.1.0；留空取 GitHub 最新 Release
    [string]$PrinterName = "ZDesigner ZT231-300dpi ZPL",        # 首次安装时写入配置的打印机名
    [int]$Port = 9195,
    [string]$InstallDir = "C:\print-agent"
)

$ErrorActionPreference = 'Stop'
$repo = "zy97/qr_server"
$serviceName = "print-agent"

# 注册服务 / 写系统目录 / 防火墙都需要管理员
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "请用管理员权限运行此脚本"
}

# 已安装过：先停服务（否则 exe 被占用无法替换），安装完成后再启动
$existing = Get-Service $serviceName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "检测到已安装的 $serviceName 服务，先停止以便升级..."
    Stop-Service $serviceName -Force -ErrorAction SilentlyContinue
    sc.exe delete $serviceName | Out-Null
}

# tag 可能是按包发布的 print-agent/vX.Y.Z，也可能是统一发布的 vX.Y.Z；
# 直接用完整 tag 拼下载地址，不做任何裁剪
function Resolve-LatestTag {
    $tags = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases") | ForEach-Object { $_.tag_name }
    $byPkg = @($tags | Where-Object { $_ -match "^print-agent/v?\d+\.\d+\.\d+" })
    if ($byPkg.Count) { return ($byPkg | Sort-Object { [version]($_ -replace "^print-agent/v?", "") } | Select-Object -Last 1) }
    $unified = @($tags | Where-Object { $_ -match "^v?\d+\.\d+\.\d+" })
    if ($unified.Count) { return ($unified | Sort-Object { [version]($_ -replace "^v", "") } | Select-Object -Last 1) }
    throw "未找到任何 Release（api.github.com 不可达？），可显式指定 -Version"
}
function Resolve-UserTag([string]$v) {
    if ($v.Contains('/')) { return $v }
    $bare = $v.TrimStart('v')
    foreach ($cand in @("print-agent/v$bare", "v$bare")) {
        try {
            Invoke-RestMethod "https://api.github.com/repos/$repo/releases/tags/$cand" | Out-Null
            return $cand
        } catch {}
    }
    throw "找不到版本 $v 对应的 Release（试过 print-agent/v$bare 和 v$bare）"
}

if ($Version) {
    $tag = Resolve-UserTag $Version
} else {
    Write-Host "查询最新 Release..."
    $tag = Resolve-LatestTag
}
Write-Host "使用 Release: $tag"
$asset = "print-agent-x86_64-pc-windows-msvc.zip"
$url = "https://github.com/$repo/releases/download/$tag/$asset"
Write-Host "下载 $url"
New-Item -ItemType Directory -Force $InstallDir | Out-Null
$zip = Join-Path $env:TEMP $asset
Invoke-WebRequest $url -OutFile $zip
Expand-Archive $zip -DestinationPath $InstallDir -Force
Remove-Item $zip

# dist 压缩包可能带一层子目录，把 exe 归位到安装目录根
$exe = Join-Path $InstallDir "print-agent.exe"
if (-not (Test-Path $exe)) {
    $found = Get-ChildItem $InstallDir -Recurse -Filter "print-agent.exe" | Select-Object -First 1
    if (-not $found) { throw "压缩包里没找到 print-agent.exe" }
    Move-Item $found.FullName $exe -Force
}

# 配置文件：已存在则保留（避免覆盖现场打印机设置）
$configPath = Join-Path $InstallDir "print-agent.toml"
if (-not (Test-Path $configPath)) {
    @"
[server]
port = $Port

[print]
printer_name = "$PrinterName"
paper_width = 10.57
paper_height = 29.70
"@ | Set-Content $configPath -Encoding UTF8
    Write-Host "已写入默认配置 $configPath（打印机: $PrinterName）"
} else {
    Write-Host "保留已有配置 $configPath"
}

# 注册为开机自启服务并启动（崩溃自动重启由 install 内置的 sc failure 配置负责）
& $exe install
if ($LASTEXITCODE -ne 0) { throw "服务注册失败" }
sc.exe start $serviceName | Out-Null

# 防火墙放行端口（已存在则跳过）
if (-not (Get-NetFirewallRule -DisplayName "print-agent $Port" -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -DisplayName "print-agent $Port" -Direction Inbound -Protocol TCP -LocalPort $Port -Action Allow | Out-Null
}

# 健康检查
Start-Sleep -Seconds 2
try {
    $resp = Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 5
    Write-Host "安装完成，健康检查通过: $resp"
    Write-Host "服务器侧 config.toml 的 [print] agent_url 指向 http://$($env:COMPUTERNAME):$Port 即可"
} catch {
    Write-Warning "服务已安装但健康检查未通过，请查看 $InstallDir\logs\ 下的日志"
}