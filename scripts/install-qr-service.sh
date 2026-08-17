#!/usr/bin/env bash
# qr_service 一键安装（标签模板设计与渲染服务，Linux 服务器）
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/zy97/qr_server/main/scripts/install-qr-service.sh | sudo bash
#   或: sudo ./install-qr-service.sh 0.1.0
# 重复执行即升级：会先停服务、替换二进制、再启动；已有 config.toml / templates/templates.db 不会被覆盖
set -euo pipefail

VERSION="${1:-}"                       # 如 0.1.0；留空取 GitHub 最新 Release
REPO="zy97/qr_server"
INSTALL_DIR="/opt/qr_service"
SERVICE_NAME="qr_service"

if [[ $EUID -ne 0 ]]; then
    echo "请用 root 运行（sudo）" >&2
    exit 1
fi

if [[ -z "$VERSION" ]]; then
    VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep -oP '"tag_name":\s*"\Kv?[^"]+' | sed 's/^v//')
fi
echo "安装 qr_service v$VERSION 到 $INSTALL_DIR"

systemctl stop "$SERVICE_NAME" 2>/dev/null || true
mkdir -p "$INSTALL_DIR"

# 1. dist 构建的二进制
curl -fsSL "https://github.com/$REPO/releases/download/v${VERSION}/qr_service-x86_64-unknown-linux-gnu.tar.xz" -o /tmp/qr_service.tar.xz
tar -xJf /tmp/qr_service.tar.xz -C "$INSTALL_DIR"
rm /tmp/qr_service.tar.xz
# dist 压缩包可能带一层子目录，把二进制归位
if [[ ! -x "$INSTALL_DIR/qr_service" ]]; then
    found=$(find "$INSTALL_DIR" -name qr_service -type f | head -1)
    [[ -n "$found" ]] && mv "$found" "$INSTALL_DIR/qr_service"
fi
chmod +x "$INSTALL_DIR/qr_service"

# 2. 运行时资源（dist 产物只含二进制；static/templates 从同版本源码包取）
curl -fsSL "https://codeload.github.com/$REPO/tar.gz/refs/tags/v${VERSION}" -o /tmp/qr_server-src.tar.gz
rm -rf /tmp/qr_server-src
mkdir -p /tmp/qr_server-src
tar -xzf /tmp/qr_server-src.tar.gz -C /tmp/qr_server-src --strip-components=1
cp -r /tmp/qr_server-src/static "$INSTALL_DIR/"
# templates 只补种子文件，不覆盖已有数据库
mkdir -p "$INSTALL_DIR/templates"
cp -rn /tmp/qr_server-src/templates/. "$INSTALL_DIR/templates/"
[[ -f "$INSTALL_DIR/config.toml" ]] || cp /tmp/qr_server-src/config.toml "$INSTALL_DIR/config.toml"
rm -rf /tmp/qr_server-src /tmp/qr_server-src.tar.gz

# 3. 渲染浏览器：默认 chrome 特性需要 Chrome/Chromium；只提示不强制
if ! command -v chromium >/dev/null 2>&1 && ! command -v chromium-browser >/dev/null 2>&1 && ! command -v google-chrome >/dev/null 2>&1; then
    echo "提示: 未检测到 Chrome/Chromium（chrome 渲染特性需要），如: apt install chromium-browser"
fi

# 4. systemd 开机自启服务
cat > /etc/systemd/system/qr_service.service <<EOF
[Unit]
Description=qr_service 标签模板设计与渲染服务
After=network.target

[Service]
WorkingDirectory=$INSTALL_DIR
ExecStart=$INSTALL_DIR/qr_service
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now "$SERVICE_NAME"
sleep 2
if systemctl is-active --quiet "$SERVICE_NAME"; then
    echo "安装完成，服务运行中。设计器入口: http://<服务器IP>:9095/designer"
    echo "别忘了编辑 $INSTALL_DIR/config.toml 的 [print] agent_url 指向各打印工位"
else
    echo "服务未能启动，请查看日志: journalctl -u $SERVICE_NAME -e" >&2
    exit 1
fi