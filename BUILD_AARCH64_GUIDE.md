# QuantClaw aarch64 (树莓派) 编译指南

## 方法一：使用 GitHub Actions 自动构建 (推荐)

最简单的办法是将代码推送到 GitHub，使用 GitHub Actions 自动编译 aarch64 版本。

已为你创建了 `.github/workflows/build-aarch64.yml`：

```yaml
name: Build aarch64 Release

on:
  push:
    tags:
      - 'v*'
  workflow_dispatch:

jobs:
  build-aarch64:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Set up QEMU
        uses: docker/setup-qemu-action@v3
        with:
          platforms: arm64
      
      - name: Build aarch64 binary
        uses: docker/build-push-action@v5
        with:
          file: ./Dockerfile.build-aarch64
          tags: quantclaw:aarch64
          load: true
      
      - name: Extract binary
        run: |
          docker create --name extract quantclaw:aarch64
          docker cp extract:/quantclaw ./quantclaw-aarch64
          docker rm extract
      
      - name: Create Release Package
        run: |
          VERSION=${GITHUB_REF#refs/tags/}
          PKG_NAME="quantclaw-${VERSION}-aarch64-linux-gnu"
          mkdir -p "$PKG_NAME"
          cp quantclaw-aarch64 "$PKG_NAME/quantclaw"
          cp -r web/dist "$PKG_NAME/"
          cp scripts/quantclaw.service "$PKG_NAME/"
          cat > "$PKG_NAME/install.sh" << 'EOF'
          #!/bin/bash
          set -e
          sudo cp quantclaw /usr/local/bin/
          sudo chmod +x /usr/local/bin/quantclaw
          mkdir -p ~/.quantclaw
          echo "安装完成!"
          EOF
          chmod +x "$PKG_NAME/install.sh"
          tar czf "${PKG_NAME}.tar.gz" "$PKG_NAME"
      
      - name: Upload Release
        uses: softprops/action-gh-release@v1
        if: startsWith(github.ref, 'refs/tags/')
        with:
          files: quantclaw-*-aarch64-linux-gnu.tar.gz
```

**使用方法：**
1. 推送代码到 GitHub
2. 创建一个标签: `git tag v0.1.0 && git push origin v0.1.0`
3. GitHub Actions 会自动编译并发布

---

## 方法二：本地交叉编译

### macOS 用户

#### 1. 安装交叉编译工具

```bash
# 安装 Homebrew (如果还没有)
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# 安装 zig (用于交叉编译)
brew install zig

# 安装 cargo-zigbuild
cargo install cargo-zigbuild

# 添加 aarch64 目标
rustup target add aarch64-unknown-linux-gnu
```

#### 2. 编译

```bash
cd /Users/shuangdada/Desktop/quantclaw/quantclaw

# 使用 zigbuild 交叉编译
cargo zigbuild \
  --target aarch64-unknown-linux-gnu \
  --features "hardware,peripheral-rpi" \
  --release

# 输出文件: target/aarch64-unknown-linux-gnu/release/quantclaw
```

#### 3. 创建安装包

```bash
VERSION=$(grep "^version" Cargo.toml | head -1 | cut -d'"' -f2)
PKG_NAME="quantclaw-${VERSION}-aarch64-linux-gnu"
mkdir -p "dist/$PKG_NAME"

cp target/aarch64-unknown-linux-gnu/release/quantclaw "dist/$PKG_NAME/"
cp -r web/dist "dist/$PKG_NAME/"
cp scripts/quantclaw.service "dist/$PKG_NAME/"

cd dist
tar czf "${PKG_NAME}.tar.gz" "$PKG_NAME"
```

---

### Linux 用户

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y gcc-aarch64-linux-gnu libc6-dev-arm64-cross

# 添加目标
rustup target add aarch64-unknown-linux-gnu

# 设置环境变量
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc

# 编译
cargo build --target aarch64-unknown-linux-gnu --features "hardware,peripheral-rpi" --release
```

---

## 方法三：直接在树莓派上编译

如果你有树莓派，可以直接在设备上编译：

```bash
# 在树莓派上执行
sudo apt-get update
sudo apt-get install -y rustc cargo pkg-config libssl-dev

git clone https://github.com/quant-speed/quantclaw.git
cd quantclaw

cargo build --release --features "hardware,peripheral-rpi"

# 安装
sudo cp target/release/quantclaw /usr/local/bin/
```

---

## 安装到树莓派

```bash
# 1. 复制安装包到树莓派
scp quantclaw-*-aarch64-linux-gnu.tar.gz pi@raspberrypi.local:~/

# 2. SSH 到树莓派
ssh pi@raspberrypi.local

# 3. 安装
tar xzf quantclaw-*-aarch64-linux-gnu.tar.gz
cd quantclaw-*-aarch64-linux-gnu
sudo ./install.sh

# 4. 配置 API 密钥
sudo nano /root/.quantclaw/config.toml

# 5. 启动服务
sudo systemctl start quantclaw
sudo systemctl enable quantclaw
```

---

## 快速脚本

使用项目提供的脚本：

```bash
# 自动交叉编译并部署到树莓派
RPI_HOST=raspberrypi.local RPI_USER=pi ./scripts/deploy-rpi.sh
```

---

## 常见问题

### 1. 缺少交叉编译器

**错误:** `failed to find tool "aarch64-linux-gnu-gcc"`

**解决:**
- macOS: `brew install zig && cargo install cargo-zigbuild`
- Linux: `sudo apt-get install gcc-aarch64-linux-gnu`

### 2. ring crate 编译失败

**错误:** `error: failed to run custom build command for ring`

**解决:** 使用 zigbuild 替代原生交叉编译

### 3. Docker 构建失败

**解决:** 确保 Docker Desktop 已启动，或改用本地交叉编译

---

## 支持的功能

aarch64 版本支持以下特性：
- `hardware` - 硬件设备支持
- `peripheral-rpi` - 树莓派外设 (GPIO, I2C, SPI)
- 所有标准通道和工具

---

## 系统要求

- Raspberry Pi 4/5 (64位模式)
- Raspberry Pi OS (64-bit) 或 Ubuntu Server 22.04/24.04
- 至少 2GB RAM (建议 4GB)
- 2GB 可用存储空间
