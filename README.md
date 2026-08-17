# qr_server 标签打印系统

集中式标签打印方案：**qr_service**（模板设计 + 渲染，部署在 Linux 服务器）+ **print-agent**（打印代理，部署在每台接打印机的 Windows 工位）。

```
业务系统 ──POST /label──> qr_service (Linux) ──POST /print──> print-agent (Windows) ──> 打印机
                              │ 模板设计器: http://<服务器>:9095/designer
                              └── 渲染出 PNG
```

## 一键安装

### 打印工位（Windows，print-agent）

管理员 PowerShell 执行：

```powershell
irm https://raw.githubusercontent.com/zy97/qr_server/main/scripts/install-print-agent.ps1 | iex
```

脚本会：下载最新 Release → 解压到 `C:\print-agent` → 首次写入 `print-agent.toml` → 注册开机自启的 Windows 服务（崩溃自动重启）→ 启动服务 → 防火墙放行端口 → 健康检查。

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

重复执行即升级：会先停服务、替换程序、再启动；已有 `print-agent.toml` 不会被覆盖。

安装后在服务器侧 `config.toml` 把 `[print] agent_url` 指向该工位，如 `http://192.168.1.100:9195`。

### 服务器（Linux，qr_service）

root 执行：

```bash
curl -fsSL https://raw.githubusercontent.com/zy97/qr_server/main/scripts/install-qr-service.sh | sudo bash
```

脚本会：下载最新 Release 的 Linux 二进制到 `/opt/qr_service` → 从同版本源码包补 `static/`、`templates/` 种子文件和 `config.toml` → 注册 systemd 开机自启服务并启动 → 验证服务活性。

指定版本：`sudo ./install-qr-service.sh 0.1.0`。重复执行即升级，已有 `config.toml` 和模板数据库 `templates/templates.db` 不会被覆盖。

注意：默认 `chrome` 渲染特性需要服务器安装 Chrome/Chromium（如 `apt install chromium-browser`），脚本检测到缺失会提示。

安装完成后：

- 设计器入口：`http://<服务器IP>:9095/designer`
- 编辑 `/opt/qr_service/config.toml` 的 `[print] agent_url` 指向各打印工位，`enabled = true` 开启打印
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