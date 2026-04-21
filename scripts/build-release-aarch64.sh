#!/usr/bin/env bash
# build-release-aarch64.sh - 构建树莓派 aarch64 安装包
# 使用方法: ./scripts/build-release-aarch64.sh

set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== QuantClaw aarch64 发布包构建 ==="
echo ""

# 检查 Docker
if ! docker info &>/dev/null; then
    echo "[!] Docker 未运行，请启动 Docker Desktop"
    exit 1
fi

# 获取版本号
VERSION=$(grep "^version" Cargo.toml | head -1 | cut -d'"' -f2)
PKG_NAME="quantclaw-${VERSION}-aarch64-linux-gnu"
PKG_DIR="dist/${PKG_NAME}"

echo "版本: $VERSION"
echo "包名: $PKG_NAME"
echo ""

# 确保 web/dist 存在
if [[ ! -d "web/dist" ]]; then
    echo "[*] 构建前端..."
    if [[ -d "web" ]]; then
        (cd web && npm ci && npm run build)
    else
        echo "[!] web 目录不存在"
        exit 1
    fi
fi

# 使用 Docker 构建 aarch64 二进制
echo "[*] 使用 Docker 交叉编译 aarch64 版本..."
echo "    这可能需要几分钟..."
echo ""

docker build -f Dockerfile.build-aarch64 -t quantclaw-builder:aarch64 . --progress=plain

# 提取二进制文件
echo ""
echo "[*] 提取编译结果..."
mkdir -p "$PKG_DIR"

# 从镜像中提取
docker create --name extract-aarch64 quantclaw-builder:aarch64 2>/dev/null || true
docker cp extract-aarch64:/quantclaw "$PKG_DIR/quantclaw" 2>/dev/null || {
    # 如果 scratch 镜像无法创建容器，使用 builder 阶段
    docker build -f Dockerfile.build-aarch64 --target builder -t quantclaw-builder:temp .
    docker create --name extract-temp quantclaw-builder:temp
    docker cp extract-temp:/app/quantclaw "$PKG_DIR/quantclaw"
    docker rm extract-temp
}
docker rm extract-aarch64 2>/dev/null || true
docker rmi quantclaw-builder:aarch64 2>/dev/null || true

# 检查提取是否成功
if [[ ! -f "$PKG_DIR/quantclaw" ]]; then
    echo "[!] 提取二进制文件失败"
    exit 1
fi

echo ""
echo "[*] 验证二进制文件..."
file "$PKG_DIR/quantclaw"
ls -lh "$PKG_DIR/quantclaw"

# 复制额外文件
echo ""
echo "[*] 准备发布包..."
cp -r web/dist "$PKG_DIR/" 2>/dev/null || echo "[!] 跳过 web/dist"
cp scripts/quantclaw.service "$PKG_DIR/" 2>/dev/null || echo "[!] 跳过服务文件"
cp README.md "$PKG_DIR/" 2>/dev/null || echo "[!] 跳过 README"
cp LICENSE-MIT "$PKG_DIR/" 2>/dev/null || echo "[!] 跳过 LICENSE"

# 创建安装脚本
cat > "$PKG_DIR/install.sh" << 'INSTALLEOF'
#!/usr/bin/env bash
# QuantClaw 树莓派安装脚本
set -e

INSTALL_DIR="/usr/local/bin"
SERVICE_DIR="/etc/systemd/system"
QUANTCLAW_USER="${QUANTCLAW_USER:-root}"

echo "=== QuantClaw 树莓派安装 ==="
echo ""

# 检查系统
if [[ $(uname -m) != "aarch64" ]]; then
    echo "[!] 警告: 当前系统不是 aarch64 架构"
    echo "    检测到: $(uname -m)"
    read -p "是否继续安装? (y/N) " -n 1 -r
    echo
    [[ $REPLY =~ ^[Yy]$ ]] || exit 1
fi

# 检查权限
if [[ $EUID -ne 0 ]]; then
   echo "[!] 请使用 sudo 运行安装脚本"
   exit 1
fi

# 安装二进制
echo "[*] 安装 quantclaw..."
cp quantclaw "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/quantclaw"

# 创建配置目录
echo "[*] 创建配置目录..."
mkdir -p "/root/.quantclaw"
mkdir -p "/root/.quantclaw/workspace"

# 创建默认配置（如果不存在）
if [[ ! -f "/root/.quantclaw/config.toml" ]]; then
    echo "[*] 创建默认配置..."
    cat > "/root/.quantclaw/config.toml" << 'CONFIGEOF'
api_key = ""
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7
provider_timeout_secs = 120

[autonomy]
level = "supervised"
workspace_only = true

[gateway]
port = 42617
host = "0.0.0.0"
web_dist_dir = "/usr/local/share/quantclaw/web/dist"

[observability]
backend = "none"
CONFIGEOF
fi

# 安装 web 资源
if [[ -d "web/dist" ]]; then
    echo "[*] 安装 web 资源..."
    mkdir -p "/usr/local/share/quantclaw"
    cp -r web/dist "/usr/local/share/quantclaw/"
fi

# 安装服务
if [[ -f "quantclaw.service" ]]; then
    echo "[*] 安装 systemd 服务..."
    cp quantclaw.service "$SERVICE_DIR/"
    sed -i "s|/usr/local/bin/quantclaw|$INSTALL_DIR/quantclaw|g" "$SERVICE_DIR/quantclaw.service" 2>/dev/null || true
    systemctl daemon-reload
    systemctl enable quantclaw
fi

# 设置权限
chown -R "$QUANTCLAW_USER:$QUANTCLAW_USER" "/root/.quantclaw" 2>/dev/null || true

echo ""
echo "=== 安装完成! ==="
echo ""
echo "使用方法:"
echo "    quantclaw --help              # 查看帮助"
echo "    quantclaw gateway             # 启动网关服务"
echo "    quantclaw daemon              # 启动守护进程"
echo ""
echo "服务管理:"
echo "    sudo systemctl start quantclaw   # 启动服务"
echo "    sudo systemctl stop quantclaw    # 停止服务"
echo "    sudo systemctl status quantclaw  # 查看状态"
echo ""
echo "配置位置: /root/.quantclaw/config.toml"
echo "网关地址: http://$(hostname -I | awk '{print $1}'):42617"
echo ""
INSTALLEOF

chmod +x "$PKG_DIR/install.sh"

# 创建卸载脚本
cat > "$PKG_DIR/uninstall.sh" << 'UNINSTALLEOF'
#!/usr/bin/env bash
set -e

echo "=== QuantClaw 卸载脚本 ==="
echo ""

if [[ $EUID -ne 0 ]]; then
   echo "[!] 请使用 sudo 运行"
   exit 1
fi

echo "[*] 停止服务..."
systemctl stop quantclaw 2>/dev/null || true
systemctl disable quantclaw 2>/dev/null || true

echo "[*] 删除文件..."
rm -f "/usr/local/bin/quantclaw"
rm -f "/etc/systemd/system/quantclaw.service"
rm -rf "/usr/local/share/quantclaw"

echo "[*] 重新加载 systemd..."
systemctl daemon-reload

echo ""
echo "=== 卸载完成 ==="
echo ""
echo "注意: 配置文件保留在 /root/.quantclaw/"
echo "      如需完全删除，请手动执行: rm -rf /root/.quantclaw"
echo ""
UNINSTALLEOF

chmod +x "$PKG_DIR/uninstall.sh"

# 创建 README
cat > "$PKG_DIR/README.txt" << 'READMEEOF'
QuantClaw for Raspberry Pi (aarch64)
=====================================

安装要求:
- Raspberry Pi 4/5 (64位系统)
- Raspberry Pi OS (64-bit) 或 Ubuntu Server 22.04+

安装步骤:
1. 解压文件:
   tar xzf quantclaw-*.tar.gz
   cd quantclaw-*-aarch64-linux-gnu

2. 运行安装脚本:
   sudo ./install.sh

3. 编辑配置文件设置 API 密钥:
   sudo nano /root/.quantclaw/config.toml

4. 启动服务:
   sudo systemctl start quantclaw

访问:
- 网关界面: http://<树莓派IP>:42617
- API 文档: http://<树莓派IP>:42617/api

卸载:
   sudo ./uninstall.sh

更多信息:
   quantclaw --help

READMEEOF

# 打包
echo ""
echo "[*] 创建压缩包..."
cd dist
tar czf "${PKG_NAME}.tar.gz" "$PKG_NAME"

echo ""
echo "=== 构建完成! ==="
echo ""
echo "发布包: dist/${PKG_NAME}.tar.gz"
ls -lh "${PKG_NAME}.tar.gz"
echo ""
echo "使用方法:"
echo "    1. 复制到树莓派: scp dist/${PKG_NAME}.tar.gz pi@raspberrypi.local:~"
echo "    2. SSH 到树莓派: ssh pi@raspberrypi.local"
echo "    3. 解压并安装:"
echo "         tar xzf ${PKG_NAME}.tar.gz"
echo "         cd ${PKG_NAME}"
echo "         sudo ./install.sh"
echo ""
