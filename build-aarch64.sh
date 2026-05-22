#!/usr/bin/env bash
# build-aarch64.sh - 为树莓派 aarch64 编译 QuantClaw 安装包
set -euo pipefail

echo "=== QuantClaw aarch64 交叉编译脚本 ==="
echo ""

# 检测平台
ARCH=$(uname -m)
OS=$(uname -s)

echo "当前平台: $OS $ARCH"
echo "目标平台: Linux aarch64 (树莓派 4/5)"
echo ""

# 检查 rustup 目标
if ! rustup target list --installed | grep -q "aarch64-unknown-linux-gnu"; then
    echo "[*] 安装 aarch64 目标..."
    rustup target add aarch64-unknown-linux-gnu
fi

# 安装交叉编译工具链
if [[ "$OS" == "Darwin" ]]; then
    if ! command -v aarch64-linux-gnu-gcc &> /dev/null; then
        echo "[*] 安装 aarch64-linux-gnu 工具链..."
        if command -v brew &> /dev/null; then
            brew install aarch64-elf-gcc 2>/dev/null || {
                echo "[!] 请手动安装交叉编译工具链:"
                echo "    brew install aarch64-elf-gcc"
                echo ""
                echo "或者使用 zig 进行交叉编译:"
                echo "    brew install zig"
                echo "    cargo install cargo-zigbuild"
                exit 1
            }
        else
            echo "[!] 未检测到 Homebrew，请手动安装交叉编译工具链"
            exit 1
        fi
    fi
fi

# 设置环境变量
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++

FEATURES="hardware,peripheral-rpi"
TARGET="aarch64-unknown-linux-gnu"
BINARY="target/${TARGET}/release/quantclaw"

echo ""
echo "[*] 开始交叉编译..."
echo "    特性: $FEATURES"
echo "    目标: $TARGET"
echo ""

cargo build --target "$TARGET" --features "$FEATURES" --release

echo ""
echo "=== 编译成功! ==="
echo ""
echo "二进制文件: $BINARY"
ls -lh "$BINARY"
file "$BINARY"

echo ""
echo "[*] 创建发布包..."
VERSION=$(grep "^version" Cargo.toml | head -1 | cut -d'"' -f2)
PKG_NAME="quantclaw-${VERSION}-aarch64-linux-gnu"
PKG_DIR="dist/${PKG_NAME}"

mkdir -p "$PKG_DIR"
cp "$BINARY" "$PKG_DIR/quantclaw"
cp -r web/dist "$PKG_DIR/web" 2>/dev/null || echo "[!] web/dist 不存在，跳过"
cp scripts/quantclaw.service "$PKG_DIR/" 2>/dev/null || echo "[!] 服务文件不存在，跳过"
cp README.md "$PKG_DIR/" 2>/dev/null || echo "[!] README 不存在，跳过"

# 创建安装脚本
cat > "$PKG_DIR/install.sh" << 'EOF'
#!/usr/bin/env bash
# 树莓派安装脚本
set -e

INSTALL_DIR="/usr/local/bin"
SERVICE_DIR="/etc/systemd/system"

echo "=== QuantClaw 树莓派安装脚本 ==="
echo ""

# 检查权限
if [[ $EUID -ne 0 ]]; then
   echo "[!] 请使用 sudo 运行"
   exit 1
fi

# 安装二进制
echo "[*] 安装 quantclaw 到 $INSTALL_DIR..."
cp quantclaw "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/quantclaw"

# 安装服务
if [[ -f quantclaw.service ]]; then
    echo "[*] 安装 systemd 服务..."
    cp quantclaw.service "$SERVICE_DIR/"
    systemctl daemon-reload
    systemctl enable quantclaw
fi

# 创建配置目录
echo "[*] 创建配置目录..."
mkdir -p /root/.quantclaw

echo ""
echo "=== 安装完成! ==="
echo ""
echo "使用方法:"
echo "    quantclaw --help"
echo "    quantclaw gateway    # 启动网关"
echo "    quantclaw daemon     # 启动守护进程"
echo ""
echo "如需配置，请编辑: /root/.quantclaw/config.toml"
EOF

chmod +x "$PKG_DIR/install.sh"

# 打包
cd dist
tar czf "${PKG_NAME}.tar.gz" "$PKG_NAME"

echo ""
echo "=== 发布包已创建 ==="
echo ""
echo "文件: dist/${PKG_NAME}.tar.gz"
ls -lh "${PKG_NAME}.tar.gz"
echo ""
echo "安装方法:"
echo "    1. 将 ${PKG_NAME}.tar.gz 复制到树莓派"
echo "    2. tar xzf ${PKG_NAME}.tar.gz"
echo "    3. cd ${PKG_NAME}"
echo "    4. sudo ./install.sh"
