# qr_server 标签打印系统

集中式标签打印方案：**qr_service**（模板设计 + 渲染，部署在 Linux 服务器）+ **print-agent**（打印代理，部署在每台接打印机的 Windows 工位）。

```
工位浏览器 ──POST http://127.0.0.1:9195/label──▶ 本机 print-agent
print-agent ──WS 长连接（主动外连，连接即工位身份）──▶ qr_service (Linux)
qr_service ──渲染 PNG，沿同一条 WS 返回──▶ print-agent ──立即响应浏览器；后台异步打印，结果经 WS 上报 qr_service
```

- 服务器**不需要配置任何工位地址**：新增工位 = 装 print-agent 并填服务器地址
- /label 渲染成功即返回标签 PNG，不等待打印；真实结果（脱机/缺纸/卡纸/超时）经 WS 上报 qr_service：代理管理页面 `/agents` 展示最近打印记录（接口 `GET /api/print-results`），同时写入服务器与 agent 本机日志
- 打印脚本（纸张/边距/队列监听规则）由 qr_service 统一生成、随渲染结果经 WS 下发，调整打印行为只需升级服务器，工位 agent 不用动
- 打印代理管理（可为每个工位覆盖打印机与纸张尺寸，未覆盖时用代理本地配置）：`http://<服务器>:9095/agents`；在线工位 API：`http://<服务器>:9095/api/agents`；模板设计器：`http://<服务器>:9095/designer`

## 一键安装

### 打印工位（Windows，print-agent）

管理员 PowerShell 执行，直接请求已部署的 qr_service。安装脚本会根据该请求地址自动配置对应的 WebSocket 服务地址：

```powershell
irm http://<服务器IP>:9095/install-print-agent.ps1 | iex
```

例如：

```powershell
irm http://192.168.1.10:9095/install-print-agent.ps1 | iex
```

脚本会：下载最新 Release → 解压到 `C:\print-agent` → 首次写入 `print-agent.toml` → 注册开机自启的 Windows 服务（崩溃自动重启）→ 启动服务 → 防火墙放行端口 → 健康检查。> 提示：脚本含中文提示信息，推荐直接用上面的 `irm | iex` 一行命令（Windows PowerShell 5.1 和 PowerShell 7 都可以）；
> 如果下载 `install-print-agent.ps1` 到本地再运行，请用 PowerShell 7（pwsh）——5.1 直接运行 .ps1 文件会把中文按 ANSI 误读导致解析失败。

可选参数（本地下载脚本后使用）：

```powershell
.\install-print-agent.ps1 -Version 0.1.0 -PrinterName "ZDesigner ZT231-300dpi ZPL" -Port 9195
```

| 参数 | 默认值 | 说明 |
|---|---|---|
| `-Version` | 最新 Release | 指定版本号（不带 v 前缀） |
| `-PrinterName` | `ZDesigner ZT231-300dpi ZPL` | 首次安装写入配置的打印机名 |
| `-Port` | `9195` | HTTP 监听端口 |
| `-InstallDir` | `C:\print-agent` | 安装目录 |
| `-ServerUrl` | 空 | qr_service WebSocket 地址；通过服务器安装端点执行时自动传入 |

重复执行即升级：会先停服务、替换程序、再启动。通过服务器安装端点执行时，仅自动写入或更新 `print-agent.toml` 的 `[server] url`，保留现场打印机和其他配置。

服务器安装端点会根据请求的 Host 和协议自动生成 `[server] url`。HTTP 对应 `ws://`，HTTPS 对应 `wss://`，无需安装后手工修改。仍可在本地运行脚本时通过 `-ServerUrl` 显式指定。

### 服务器（Linux，qr_service）

root 执行：

```bash
curl -fsSL https://raw.githubusercontent.com/zy97/qr_server/master/scripts/install-qr-service.sh | sudo bash
```

脚本会：下载最新 Release 的 Linux 二进制到 `/opt/qr_service` → 从同版本源码包补 `static/`、`templates/` 种子文件和 `config.toml` → 注册 systemd 开机自启服务并启动 → 验证服务活性。

指定版本：`sudo ./install-qr-service.sh 0.1.0`。重复执行即升级，已有 `config.toml` 和模板数据库 `templates/templates.db` 不会被覆盖。

> 国内服务器访问 GitHub Release（objects.githubusercontent.com）容易超时卡死，可用 `GH_PROXY` 走代理前缀：
>
> ```bash
> GH_PROXY="https://ghfast.top/" curl -fsSL https://raw.githubusercontent.com/zy97/qr_server/master/scripts/install-qr-service.sh | sudo -E bash
> ```
>
> （代理服务地址可能变化，换成你可用的即可；注意 `sudo -E` 保留环境变量）

注意：默认 `chrome` 渲染特性需要服务器安装 Chrome/Chromium（如 `apt install chromium-browser`），脚本检测到缺失会提示。

安装完成后：

- 设计器入口：`http://<服务器IP>:9095/designer`
- qr_service 无需配置工位地址；打印链路完全由 print-agent 主动外连建立
- 改完配置重启：`systemctl restart qr_service`

## 发布（CI/CD）

推送 tag 触发 GitHub Actions 自动编译 Windows/Linux 产物并创建 Release：

| tag 格式 | 构建内容 |
|---|---|
| `v0.1.0` | 两个包都构建（版本号需与各自 Cargo.toml 一致） |
| `qr_service/v0.1.0` | 只构建 qr_service |
| `print-agent/v0.1.0` | 只构建 print-agent |

## 手动部署（不用一键脚本时）

dist 产物只含二进制，运行时还需要这些文件放在程序工作目录旁：

- qr_service：`static/`、`templates/`（种子模板）、`config.toml`；chrome 特性还需 Chrome/Chromium
- print-agent：`print-agent.toml`；`print-agent install` 注册服务 / `print-agent run` 控制台调试