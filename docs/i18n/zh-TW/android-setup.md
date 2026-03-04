# Android 設定指南（繁體中文）

ZeroClaw 提供適用於 Android 裝置的預建置二進位檔。

## 支援架構

| 目標平台 | Android 版本 | 適用裝置 |
|--------|-----------------|---------|
| `armv7-linux-androideabi` | Android 4.1+（API 16+）| 舊型 32 位元手機（Galaxy S3 等）|
| `aarch64-linux-android` | Android 5.0+（API 21+）| 現代 64 位元手機 |

## 透過 Termux 安裝

在 Android 上執行 ZeroClaw 最簡單的方式是透過 [Termux](https://termux.dev/)。

### 1. 安裝 Termux

從 [F-Droid](https://f-droid.org/packages/com.termux/)（建議）或 GitHub releases 下載。

> **注意：** Play Store 上的版本已過時且不受支援。

### 2. 下載 ZeroClaw

```bash
# 確認裝置架構
uname -m
# aarch64 = 64 位元, armv7l/armv8l = 32 位元

# 下載對應的二進位檔
# 64 位元（aarch64）：
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz

# 32 位元（armv7）：
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-linux-androideabi.tar.gz
tar xzf zeroclaw-armv7-linux-androideabi.tar.gz
```

### 3. 安裝與執行

```bash
chmod +x zeroclaw
mv zeroclaw $PREFIX/bin/

# 驗證安裝
zeroclaw --version

# 執行設定
zeroclaw onboard
```

## 透過 ADB 直接安裝

適用於想在 Termux 外執行 ZeroClaw 的進階使用者：

```bash
# 從電腦透過 ADB 操作
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

> 在 Termux 外執行需要已 root 的裝置或特定權限，才能使用完整功能。

## Android 上的限制

- **無 systemd：** 使用 Termux 的 `termux-services` 來啟用常駐模式
- **儲存存取：** 需要 Termux 儲存權限（`termux-setup-storage`）
- **網路：** 部分功能可能需要 Android VPN 權限以進行本機繫結

## 從原始碼建置

ZeroClaw 支援兩種 Android 原始碼建置工作流程。

### A）直接在 Termux 內建置（裝置端）

適用於在手機或平板上原生編譯的情境。

```bash
# Termux 前置套件
pkg update
pkg install -y clang pkg-config

# 新增 Android Rust 目標（大多數裝置只需 aarch64 目標）
rustup target add aarch64-linux-android armv7-linux-androideabi

# 為目前裝置架構進行建置
cargo build --release --target aarch64-linux-android
```

注意事項：
- `.cargo/config.toml` 預設在 Android 目標上使用 `clang`。
- 在 Termux 原生建置時不需要 NDK 前綴的 linker，例如 `aarch64-linux-android21-clang`。
- `wasm-tools` 執行環境目前在 Android 建置中不可用；WASM 工具會回退至 stub 實作。

### B）從 Linux/macOS 以 Android NDK 交叉編譯

適用於從桌機 CI/開發環境建置 Android 二進位檔的情境。

```bash
# 新增目標
rustup target add armv7-linux-androideabi aarch64-linux-android

# 設定 Android NDK 工具鏈
export ANDROID_NDK_HOME=/path/to/ndk
export NDK_TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export PATH="$NDK_TOOLCHAIN:$PATH"

# 以 NDK 包裝 linker 覆蓋 Cargo 預設值
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$NDK_TOOLCHAIN/armv7a-linux-androideabi21-clang"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK_TOOLCHAIN/aarch64-linux-android21-clang"

# 確保 cc-rs 建置腳本使用相同的編譯器
export CC_armv7_linux_androideabi="$CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"

# 建置
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

### 快速環境自我檢測

使用內建檢測工具在長時間建置前驗證 linker/工具鏈設定：

```bash
# 在 repo 根目錄執行
scripts/android/termux_source_build_check.sh --target aarch64-linux-android

# 強制 Termux 原生診斷
scripts/android/termux_source_build_check.sh --target aarch64-linux-android --mode termux-native

# 強制桌機 NDK 交叉編譯診斷
scripts/android/termux_source_build_check.sh --target aarch64-linux-android --mode ndk-cross

# 環境驗證後實際執行 cargo check
scripts/android/termux_source_build_check.sh --target aarch64-linux-android --run-cargo-check
```

當 `--run-cargo-check` 失敗時，腳本會分析常見的 linker/`cc-rs` 錯誤，並針對所選模式印出可直接複製貼上的修復指令。

您也可以直接診斷先前擷取的 cargo 日誌：

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log
```

適用於 CI 自動化，輸出機器可讀的報告：

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log \
  --json-output /tmp/zeroclaw-android-selfcheck.json
```

適用於 pipeline，將 JSON 直接輸出至 stdout：

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log \
  --json-output - \
  --quiet
```

JSON 報告重點：
- `status`：`ok` 或 `error`
- `error_code`：穩定的分類碼（`NONE`、`BAD_ARGUMENT`、`MISSING_DIAGNOSE_LOG`、`CARGO_CHECK_FAILED` 等）
- `detection_codes`：結構化診斷碼（`CC_RS_TOOL_NOT_FOUND`、`LINKER_RESOLUTION_FAILURE`、`MISSING_RUST_TARGET_STDLIB` 等）
- `suggestions`：可複製貼上的修復指令

在 CI 中啟用嚴格閘控：

```bash
scripts/android/termux_source_build_check.sh \
  --target aarch64-linux-android \
  --mode ndk-cross \
  --diagnose-log /path/to/cargo-error.log \
  --json-output /tmp/zeroclaw-android-selfcheck.json \
  --strict
```

## 疑難排解

### "Permission denied"

```bash
chmod +x zeroclaw
```

### "not found" 或 linker 錯誤

請確認下載的架構版本與裝置相符。

針對 Termux 原生建置，請確保 `clang` 存在並移除過期的 NDK 覆蓋設定：

```bash
unset CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER
unset CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER
unset CC_aarch64_linux_android
unset CC_armv7_linux_androideabi
command -v clang
```

針對交叉編譯，請確保 `ANDROID_NDK_HOME` 與 `CARGO_TARGET_*_LINKER` 指向有效的 NDK 二進位檔。
若建置腳本（例如 `ring`/`aws-lc-sys`）仍回報 `failed to find tool "aarch64-linux-android-clang"`，
也請將 `CC_aarch64_linux_android` / `CC_armv7_linux_androideabi` 設定為相同的 NDK clang 包裝器。

### "WASM tools are unavailable on Android"

這是目前的預期行為。Android 建置會以 stub 模式執行 WASM 工具載入器；若需要執行時期的 `wasm-tools`，請在 Linux/macOS/Windows 上建置。

### 舊版 Android（4.x）

使用 `armv7-linux-androideabi` 建置版本，需 API level 16+。
