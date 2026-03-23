# Android 安装指南

ZeroClaw 为 Android 设备提供预构建二进制文件。

## 支持的架构

| 目标 | Android 版本 | 设备 |
|--------|-----------------|---------|
| `armv7-linux-androideabi` | Android 4.1+ (API 16+) | 旧款 32 位手机（Galaxy S3 等） |
| `aarch64-linux-android` | Android 5.0+ (API 21+) | 现代 64 位手机 |

## 通过 Termux 安装

在 Android 上运行 ZeroClaw 最简单的方式是通过 [Termux](https://termux.dev/)。

### 1. 安装 Termux

从 [F-Droid](https://f-droid.org/packages/com.termux/)（推荐）或 GitHub 发布页下载。

> ⚠️ **注意：** Play Store 版本已过时且不受支持。

### 2. 下载 ZeroClaw

```bash
# 检查你的架构
uname -m
# aarch64 = 64 位, armv7l/armv8l = 32 位

# 下载对应的二进制文件
# 64 位（aarch64）：
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz

# 32 位（armv7）：
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-linux-androideabi.tar.gz
tar xzf zeroclaw-armv7-linux-androideabi.tar.gz
```

### 3. 安装和运行

```bash
chmod +x zeroclaw
mv zeroclaw $PREFIX/bin/

# 验证安装
zeroclaw --version

# 运行设置
zeroclaw onboard
```

## 通过 ADB 直接安装

适用于希望在 Termux 之外运行 ZeroClaw 的高级用户：

```bash
# 在安装了 ADB（Android 调试桥）的电脑上执行
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

> ⚠️ 在 Termux 之外运行需要 root 权限或特定权限才能获得完整功能。

## Android 上的限制

- **无 systemd：** 守护进程模式使用 Termux 的 `termux-services`
- **存储访问：** 需要 Termux 存储权限（`termux-setup-storage`）
- **网络：** 某些功能可能需要 Android VPN 权限才能进行本地绑定

## 从源码构建

如需自行构建 Android 版本：

```bash
# 安装 Android NDK
# 添加目标
rustup target add armv7-linux-androideabi aarch64-linux-android

# 设置 NDK 路径
export ANDROID_NDK_HOME=/path/to/ndk
export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH

# 构建
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

## 故障排除

### "Permission denied"

```bash
chmod +x zeroclaw
```

### "not found" 或链接器错误

确保你下载了与设备架构匹配的正确版本。

### 旧版 Android（4.x）

使用 API 级别 16+ 支持的 `armv7-linux-androideabi` 构建。
