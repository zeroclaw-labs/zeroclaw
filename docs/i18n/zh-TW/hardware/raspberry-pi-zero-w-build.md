# 在 Raspberry Pi Zero W 上建置 ZeroClaw

在 Raspberry Pi Zero W（512MB RAM、ARMv6）上編譯 ZeroClaw 的完整指南。

最後驗證日期：**2026 年 2 月 28 日**。

## 概述

Raspberry Pi Zero W 是一款資源受限的裝置，僅有 **512MB RAM**。在此裝置上編譯 Rust 需要特別注意以下事項：

| 需求 | 最低要求 | 建議配置 |
|------|----------|----------|
| RAM | 512MB | 512MB + 2GB swap |
| 可用磁碟空間 | 4GB | 6GB+ |
| 作業系統 | Raspberry Pi OS（32 位元） | Raspberry Pi OS Lite（32 位元） |
| 架構 | armv6l | armv6l |

**重要：**本指南假設您是在 Pi Zero W 上**原生建置**，而非從更強大的機器進行交叉編譯。

## 目標 ABI：gnueabihf vs musleabihf

在為 Raspberry Pi Zero W 建置時，您有兩種目標 ABI 可供選擇：

| ABI | 完整目標 | 說明 | 二進位檔大小 | 靜態連結 | 建議使用 |
|-----|----------|------|-------------|----------|----------|
| **musleabihf** | `armv6l-unknown-linux-musleabihf` | 使用 musl libc | 較小 | 是（完全靜態） | **是** |
| gnueabihf | `armv6l-unknown-linux-gnueabihf` | 使用 glibc | 較大 | 部分 | 否 |

**為何推薦 musleabihf：**

1. **較小的二進位檔** — musl 產生更精簡的二進位檔，對嵌入式裝置至關重要
2. **完全靜態連結** — 不依賴系統 libc 版本；二進位檔可在不同版本的 Raspberry Pi OS 上運行
3. **更佳的安全性** — musl 精簡的 libc 實作減少了攻擊面
4. **可攜性** — 靜態二進位檔可在任何 ARMv6 Linux 發行版上運行，無相容性疑慮

**取捨：**
- musleabihf 建置的編譯時間可能稍長
- 某些小眾相依套件可能不支援 musl（ZeroClaw 的相依套件皆與 musl 相容）

## 選項 A：原生編譯

### 步驟 1：準備系統

首先，確保您的系統已更新至最新：

```bash
sudo apt update
sudo apt upgrade -y
```

### 步驟 2：新增 Swap 空間（關鍵步驟）

由於 RAM 有限（512MB），**新增 swap 是成功編譯的必要條件**：

```bash
# 建立 2GB swap 檔案
sudo fallocate -l 2G /swapfile

# 設定正確的權限
sudo chmod 600 /swapfile

# 格式化為 swap
sudo mkswap /swapfile

# 啟用 swap
sudo swapon /swapfile

# 確認 swap 已啟用
free -h
```

若要讓 swap 在重新開機後持續生效：

```bash
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

### 步驟 3：安裝 Rust 工具鏈

透過 rustup 安裝 Rust：

```bash
# 安裝 rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 載入環境變數
source $HOME/.cargo/env

# 驗證安裝
rustc --version
cargo --version
```

### 步驟 4：安裝建置相依套件

安裝所需的系統套件：

```bash
sudo apt install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    git \
    curl
```

### 步驟 5：複製 ZeroClaw 儲存庫

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
```

或者如果您已經有此儲存庫：

```bash
cd /path/to/zeroclaw
git fetch --all
git checkout main
git pull
```

### 步驟 6：配置低記憶體建置設定

ZeroClaw 的 `Cargo.toml` 已針對低記憶體裝置進行配置（release profile 中設定 `codegen-units = 1`）。為了在 Pi Zero W 上獲得額外的安全保障：

```bash
# 設定 CARGO_BUILD_JOBS=1 以防止記憶體耗盡
export CARGO_BUILD_JOBS=1
```

### 步驟 7：選擇目標 ABI 並建置 ZeroClaw

此步驟將耗時 **30-60 分鐘**，取決於您的儲存裝置速度和所選目標。

**原生建置時，預設目標為 gnueabihf（與您的系統相符）：**

```bash
# 使用預設目標（gnueabihf）建置
cargo build --release

# 替代方案：僅建置特定功能（較小的二進位檔）
cargo build --release --no-default-features --features "wasm-tools"
```

**使用 musleabihf（較小的靜態二進位檔 — 需要 musl 工具）：**

```bash
# 安裝 musl 開發工具
sudo apt install -y musl-tools musl-dev

# 新增 musl 目標
rustup target add armv6l-unknown-linux-musleabihf

# 為 musleabihf 建置（較小的靜態二進位檔）
cargo build --release --target armv6l-unknown-linux-musleabihf
```

**注意：**如果建置因「記憶體不足」錯誤而失敗，您可能需要將 swap 大小增加至 4GB：

```bash
sudo swapoff /swapfile
sudo rm /swapfile
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

然後重試建置。

### 步驟 8：安裝 ZeroClaw

```bash
# gnueabihf（預設目標）
sudo cp target/release/zeroclaw /usr/local/bin/

# musleabihf
sudo cp target/armv6l-unknown-linux-musleabihf/release/zeroclaw /usr/local/bin/

# 驗證安裝
zeroclaw --version

# 驗證二進位檔為靜態連結（僅限 musleabihf）
file /usr/local/bin/zeroclaw
# musleabihf 應顯示 "statically linked"
```

## 選項 B：交叉編譯（建議方式）

為了獲得更快的建置速度，可從更強大的機器（Linux、macOS 或 Windows）進行交叉編譯。Pi Zero W 上的原生建置需要 **30-60 分鐘**，而交叉編譯通常只需 **5-10 分鐘**即可完成。

### 為何選擇交叉編譯？

| 因素 | 原生建置（Pi Zero W） | 交叉編譯（x86_64） |
|------|----------------------|-------------------|
| 建置時間 | 30-60 分鐘 | 5-10 分鐘 |
| 所需 RAM | 512MB + 2GB swap | 通常 4GB+ |
| CPU 負載 | 高（單核心） | 相對於主機較低 |
| 迭代速度 | 慢 | 快 |

### 前置需求

在您的建置主機上（以 Linux x86_64 為例）：

```bash
# 安裝 ARM 交叉編譯工具鏈
# 注意：我們使用 gcc-arm-linux-gnueabihf 作為連結器工具，
# 但 Rust 的目標配置會產生靜態 musl 二進位檔
sudo apt install -y musl-tools musl-dev gcc-arm-linux-gnueabihf

# 確認交叉編譯器可用
arm-linux-gnueabihf-gcc --version
```

**為何 musl 建置要使用 gnueabihf？**

標準 Ubuntu/Debian 套件庫中沒有純粹的 `arm-linux-musleabihf-gcc` 交叉編譯器。解決方法如下：
1. 使用 `gcc-arm-linux-gnueabihf` 作為連結器工具（可從套件庫取得）
2. Rust 的目標規格（`armv6l-unknown-linux-musleabihf.json`）設定 `env: "musl"`
3. 靜態連結（`-C link-arg=-static`）消除 glibc 依賴
4. 結果：可在任何 ARMv6 Linux 上運行的可攜式靜態 musl 二進位檔

**macOS：**透過 Homebrew 安裝：
```bash
brew install musl-cross
```

**Windows：**使用 WSL2 或安裝 mingw-w64 交叉編譯器。

### 為 musleabihf 建置（建議方式）

ZeroClaw 儲存庫已包含預先配置的 `.cargo/config.toml` 和 `.cargo/armv6l-unknown-linux-musleabihf.json` 以支援靜態連結。

```bash
# 複製 ZeroClaw 儲存庫
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 將 ARMv6 musl 目標新增至 rustup
rustup target add armv6l-unknown-linux-musleabihf

# 儲存庫的 .cargo/config.toml 已包含：
# [target.armv6l-unknown-linux-musleabihf]
# rustflags = ["-C", "link-arg=-static"]
#
# 而 .cargo/armv6l-unknown-linux-musleabihf.json 提供了
# 適當 ARMv6 支援的目標規格。

# 為目標建置（靜態二進位檔，無執行時期相依）
cargo build --release --target armv6l-unknown-linux-musleabihf
```

### 了解靜態連結的優勢

`rustflags = ["-C", "link-arg=-static"]` 旗標確保**完全靜態連結**：

| 優勢 | 說明 |
|------|------|
| **無 libc 依賴** | 二進位檔可在任何 ARMv6 Linux 發行版上運行 |
| **較小的檔案大小** | musl 產生比 glibc 更精簡的二進位檔 |
| **版本無關** | 可在 Raspberry Pi OS Bullseye、Bookworm 或未來版本上運行 |
| **預設安全** | musl 精簡的 libc 減少攻擊面 |
| **可攜性** | 同一個二進位檔可在不同 ARMv6 的 Pi 型號上運行 |

### 驗證靜態連結

建置完成後，確認二進位檔為靜態連結：

```bash
file target/armv6l-unknown-linux-musleabihf/release/zeroclaw
# 輸出應包含："statically linked"

ldd target/armv6l-unknown-linux-musleabihf/release/zeroclaw
# 輸出應為："not a dynamic executable"
```

### 為 gnueabihf 建置（替代方案）

如果您需要動態連結或有特定的 glibc 相依需求：

```bash
# 新增 ARMv6 glibc 目標
rustup target add armv6l-unknown-linux-gnueabihf

# 為目標建置
cargo build --release --target armv6l-unknown-linux-gnueabihf
```

**注意：**gnueabihf 二進位檔會較大，且依賴目標系統的 glibc 版本。

### 自訂功能建置

透過僅建置所需功能來縮減二進位檔大小：

```bash
# 最小化建置（僅 agent 核心）
cargo build --release --target armv6l-unknown-linux-musleabihf --no-default-features

# 指定功能集
cargo build --release --target armv6l-unknown-linux-musleabihf --features "telegram,discord"

# 使用 dist profile 產生大小最佳化的二進位檔
cargo build --profile dist --target armv6l-unknown-linux-musleabihf
```

### 傳輸至 Pi Zero W

```bash
# 從建置機器（依需要調整目標）
scp target/armv6l-unknown-linux-musleabihf/release/zeroclaw pi@zero-w-ip:/home/pi/

# 在 Pi Zero W 上
sudo mv ~/zeroclaw /usr/local/bin/
sudo chmod +x /usr/local/bin/zeroclaw
zeroclaw --version

# 驗證為靜態連結（目標系統無需額外相依）
ldd /usr/local/bin/zeroclaw
# 應輸出："not a dynamic executable"
```

### 交叉編譯工作流程摘要

```
┌─────────────────┐     Clone/Fork     ┌─────────────────────┐
│  ZeroClaw Repo  │ ──────────────────> │   您的建置主機       │
│  (GitHub)       │                    │  (Linux/macOS/Win)  │
└─────────────────┘                    └─────────────────────┘
                                                │
                                                │ rustup target add
                                                │ cargo build --release
                                                ▼
                                        ┌─────────────────────┐
                                        │  靜態二進位檔         │
                                        │  (armv6l-musl)      │
                                        └─────────────────────┘
                                                │
                                                │ scp / rsync
                                                ▼
                                        ┌─────────────────────┐
                                        │  Raspberry Pi       │
                                        │  Zero W             │
                                        │  /usr/local/bin/    │
                                        └─────────────────────┘
```

## 安裝後配置

### 初始化 ZeroClaw

```bash
# 執行互動式設定
zeroclaw setup

# 或手動配置
mkdir -p ~/.config/zeroclaw
nano ~/.config/zeroclaw/config.toml
```

### 啟用硬體功能（選用）

若要支援 Raspberry Pi GPIO：

```bash
# 使用 peripheral-rpi 功能建置（僅限原生建置）
cargo build --release --features peripheral-rpi
```

### 以系統服務方式執行（選用）

建立 systemd 服務：

```bash
sudo nano /etc/systemd/system/zeroclaw.service
```

新增以下內容：

```ini
[Unit]
Description=ZeroClaw AI Agent
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi
ExecStart=/usr/local/bin/zeroclaw agent
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

啟用並啟動服務：

```bash
sudo systemctl daemon-reload
sudo systemctl enable zeroclaw
sudo systemctl start zeroclaw
```

## 疑難排解

### 建置因「記憶體不足」而失敗

**解決方法：**增加 swap 大小：

```bash
sudo swapoff /swapfile
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

### 連結器錯誤

**解決方法：**確保已安裝正確的工具鏈：

```bash
sudo apt install -y build-essential pkg-config libssl-dev
```

### 執行時期 SSL/TLS 錯誤

**解決方法：**安裝 SSL 憑證：

```bash
sudo apt install -y ca-certificates
```

### 二進位檔過大

**解決方法：**使用最少功能建置：

```bash
cargo build --release --no-default-features --features "wasm-tools"
```

或使用 `.dist` profile：

```bash
cargo build --profile dist
```

## 效能最佳化建議

1. **使用 Lite 作業系統：**Raspberry Pi OS Lite 的系統負擔較低
2. **超頻（選用）：**在 `/boot/config.txt` 中新增 `arm_freq=1000`
3. **停用圖形介面：**`sudo systemctl disable lightdm`（如果使用桌面版）
4. **使用外接儲存裝置：**如果可用，在 USB 3.0 磁碟機上建置

## 相關文件

- [硬體周邊設計](../hardware-peripherals-design.md) - 架構說明
- [一鍵安裝](../one-click-bootstrap.md) - 一般安裝方式
- [維運手冊](../operations/operations-runbook.md) - 正式環境運行

## 參考資料

- [Raspberry Pi Zero W 規格](https://www.raspberrypi.com/products/raspberry-pi-zero-w/)
- [Rust 交叉編譯指南](https://rust-lang.github.io/rustc/platform-support.html)
- [Cargo Profile 配置](https://doc.rust-lang.org/cargo/reference/profiles.html)
