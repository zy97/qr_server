#!/usr/bin/env bash
# qr_service 一键安装（标签模板设计与渲染服务，Linux 服务器）
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/zy97/qr_server/master/scripts/install-qr-service.sh | sudo bash
#   或: sudo ./install-qr-service.sh 0.3.0        （也接受 v0.3.0 / qr_service/v0.3.0）
# 重复执行即升级：会先停服务、替换二进制、再启动；已有 config.toml / templates/templates.db 不会被覆盖
#
# 国内服务器访问 GitHub Release 容易超时，可用 GH_PROXY 代理前缀（末尾带 /）：
#   GH_PROXY="https://ghfast.top/" curl -fsSL .../install-qr-service.sh | sudo -E bash
set -euo pipefail

REPO="zy97/qr_server"
INSTALL_DIR="/opt/qr_service"
SERVICE_NAME="qr_service"
PKG_PREFIX="qr_service"              # 本脚本安装qr_service的包：优先匹配 qr_service/vX.Y.Z 形式的 Release
ASSET="qr_service-x86_64-unknown-linux-gnu.tar.xz"
GH_PROXY="${GH_PROXY:-}"             # GitHub 下载代理前缀，如 https://ghfast.top/

if [[ $EUID -ne 0 ]]; then
    echo "请用 root 运行（sudo）" >&2
    exit 1
fi

# 下载统一入口：连接 15s 超时 + 重试，进度条可见；卡死会快速失败而不是干等
dl() {
    curl -fL --connect-timeout 15 --retry 3 --retry-delay 2 "$@"
}

# 列出所有 Release 的 tag（含按包发布的 包名/vX.Y.Z 形式）
list_tags() {
    dl -s "https://api.github.com/repos/$REPO/releases" | grep -oP '"tag_name":\s*"\K[^"]+'
}

# 解析用户指定的版本：接受 0.3.0 / v0.3.0 / qr_service/v0.3.0，验证 Release 真实存在
resolve_user_tag() {
    local arg="$1" bare cand
    [[ "$arg" == */* ]] && { echo "$arg"; return; }
    bare="${arg#v}"
    for cand in "$PKG_PREFIX/v$bare" "v$bare"; do
        if [[ $(curl -s -o /dev/null -w "%{http_code}" "https://api.github.com/repos/$REPO/releases/tags/$cand") == "200" ]]; then
            echo "$cand"
            return
        fi
    done
    echo "找不到版本 $arg 对应的 Release（试过 $PKG_PREFIX/v$bare 和 v$bare）" >&2
    exit 1
}

# 自动选最新版本：优先 qr_service/vX.Y.Z 按包 tag（版本号最大者），回退统一 tag vX.Y.Z
resolve_latest_tag() {
    local tags tag
    tags=$(list_tags)
    tag=$(printf '%s\n' "$tags" | grep -P "^$PKG_PREFIX/v?\d+\.\d+\.\d+" | sort -V | tail -1)
    [[ -z "$tag" ]] && tag=$(printf '%s\n' "$tags" | grep -P "^v?\d+\.\d+\.\d+" | sort -V | tail -1)
    if [[ -z "$tag" ]]; then
        echo "未找到任何 Release（api.github.com 不可达？），可显式指定: sudo ./install-qr-service.sh 0.3.0" >&2
        exit 1
    fi
    echo "$tag"
}

if [[ -n "${1:-}" ]]; then
    TAG=$(resolve_user_tag "$1")
else
    echo "==> 查询最新 Release..."
    TAG=$(resolve_latest_tag)
fi
echo "==> 安装 qr_service（Release: $TAG）到 $INSTALL_DIR"

mkdir -p "$INSTALL_DIR"

# 1. dist 构建的二进制。先下载后停服务：下载可能很慢甚至失败，
# 期间旧版本继续提供服务，停服窗口只剩本地替换的几秒
echo "==> 下载二进制（GitHub Release）..."
dl "${GH_PROXY}https://github.com/$REPO/releases/download/$TAG/$ASSET" -o /tmp/qr_service.tar.xz
systemctl stop "$SERVICE_NAME" 2>/dev/null || true
# dist tar 包内容嵌在一层同名子目录里——解压到暂存目录再无条件替换二进制。
# 不能在 $INSTALL_DIR 里就地解压 + 仅当目标不存在才归位：升级时旧二进制存在，
# 新二进制会永远留在嵌套子目录里，服务跑的仍是旧版本
rm -rf /tmp/qr_service-bin
mkdir -p /tmp/qr_service-bin
tar -xJf /tmp/qr_service.tar.xz -C /tmp/qr_service-bin
rm /tmp/qr_service.tar.xz
found=$(find /tmp/qr_service-bin -name qr_service -type f | head -1)
[[ -z "$found" ]] && { echo "压缩包里没找到 qr_service 二进制" >&2; exit 1; }
cp "$found" "$INSTALL_DIR/qr_service.new"
chmod +x "$INSTALL_DIR/qr_service.new"
mv "$INSTALL_DIR/qr_service.new" "$INSTALL_DIR/qr_service"
rm -rf /tmp/qr_service-bin

# 2. 运行时资源（dist 产物只含二进制；static/templates 从同版本源码包取）
echo "==> 下载运行时资源（static/templates）..."
dl "${GH_PROXY}https://codeload.github.com/$REPO/tar.gz/refs/tags/$TAG" -o /tmp/qr_server-src.tar.gz
rm -rf /tmp/qr_server-src
mkdir -p /tmp/qr_server-src
tar -xzf /tmp/qr_server-src.tar.gz -C /tmp/qr_server-src --strip-components=1
cp -r /tmp/qr_server-src/static "$INSTALL_DIR/"
# templates 只补种子文件，不覆盖已有数据库。
# 逐文件判断目标是否存在：兼容所有 coreutils/busybox（cp -n 行为不可移植，--update=none 需 coreutils 9.2+）
mkdir -p "$INSTALL_DIR/templates"
find /tmp/qr_server-src/templates -type f | while IFS= read -r f; do
    rel="${f#/tmp/qr_server-src/templates/}"
    [[ -e "$INSTALL_DIR/templates/$rel" ]] || cp "$f" "$INSTALL_DIR/templates/$rel"
done
[[ -f "$INSTALL_DIR/config.toml" ]] || cp /tmp/qr_server-src/config.toml "$INSTALL_DIR/config.toml"
rm -rf /tmp/qr_server-src /tmp/qr_server-src.tar.gz

# 3. 渲染浏览器：chrome 特性经 chromiumoxide 探测（CHROME 环境变量 → PATH → 常见安装路径）。
# 优先系统 Chrome/Chromium；没有则回落到 agent-browser 安装的浏览器，
# 并把路径写进 systemd unit 的 CHROME 环境变量
CHROME_PATH=""
for c in chromium chromium-browser google-chrome google-chrome-stable chrome; do
    if command -v "$c" >/dev/null 2>&1; then
        CHROME_PATH=$(command -v "$c")
        break
    fi
done
if [[ -z "$CHROME_PATH" ]]; then
    # agent-browser install 通过 Playwright 下载 Chrome for Testing，浏览器位于 ms-playwright 缓存目录。
    # 调用者与 systemd 服务可能使用不同 HOME，因此同时检查 root 和普通用户缓存。
    CHROME_PATH=$(find "$HOME/.cache/ms-playwright" /root/.cache/ms-playwright /home/*/.cache/ms-playwright \
        -type f \( -path '*/chrome-linux*/chrome' -o -path '*/chrome-headless-shell-linux*/chrome-headless-shell' \) \
        2>/dev/null | sort -V | tail -1)
fi
if [[ -n "$CHROME_PATH" ]]; then
    echo "==> 渲染浏览器: $CHROME_PATH"
else
    echo "提示: 未检测到 Chrome/Chromium 或 agent-browser 浏览器（chrome 渲染特性需要），"
    echo "      如: apt install chromium-browser，或 agent-browser install 后重跑本脚本"
fi

# 4. systemd 开机自启服务
echo "==> 注册 systemd 服务..."
cat > /etc/systemd/system/qr_service.service <<EOF
[Unit]
Description=qr_service 标签模板设计与渲染服务
After=network.target

[Service]
$( [[ -n "$CHROME_PATH" ]] && echo "Environment=CHROME=$CHROME_PATH" )
WorkingDirectory=$INSTALL_DIR
ExecStart=$INSTALL_DIR/qr_service
Restart=always
RestartSec=5
# 显式声明 cgroup 级联终止（也是默认值）：停止/重启/关机时连带杀掉
# chrome、typst watch 等全部子孙进程，等价于 Windows 的 KILL_ON_JOB_CLOSE
KillMode=control-group
TimeoutStopSec=10

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now "$SERVICE_NAME"

# 防火墙放行 9095（qr_service 默认监听 0.0.0.0:9095；按实际存在的防火墙工具处理）
if command -v ufw >/dev/null 2>&1 && ufw status | grep -q "Status: active"; then
    ufw allow 9095/tcp >/dev/null
    echo "==> ufw 已放行 9095/tcp"
elif command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
    firewall-cmd --permanent --add-port=9095/tcp >/dev/null && firewall-cmd --reload >/dev/null
    echo "==> firewalld 已放行 9095/tcp"
fi

sleep 2
if systemctl is-active --quiet "$SERVICE_NAME"; then
    echo "安装完成，服务运行中。启动日志（含版本号）："
    journalctl -u "$SERVICE_NAME" --since "-15s" --no-pager | grep -m1 "qr_service 启动" || true
    echo "设计器入口: http://<服务器IP>:9095/designer"
    echo "别忘了编辑 $INSTALL_DIR/config.toml 的 [print] agent_url 指向各打印工位"
else
    echo "服务未能启动，请查看日志: journalctl -u $SERVICE_NAME -e" >&2
    exit 1
fi