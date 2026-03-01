# ZeroClaw 搭配 Arduino Uno Q — 逐步設定指南（繁體中文）

在 Arduino Uno Q 的 Linux 端執行 ZeroClaw。Telegram 透過 WiFi 通訊；GPIO 控制使用 Bridge（需要搭配一個簡易的 App Lab 應用程式）。

---

## 已包含的內容（不需要修改程式碼）

ZeroClaw 已包含 Arduino Uno Q 所需的一切。**複製專案倉庫並依照本指南操作 — 不需要任何修補或自訂程式碼。**

| 元件 | 位置 | 用途 |
|------|------|------|
| Bridge 應用程式 | `firmware/zeroclaw-uno-q-bridge/` | MCU 草稿碼 + Python socket 伺服器（port 9999），用於 GPIO |
| Bridge 工具 | `src/peripherals/uno_q_bridge.rs` | 透過 TCP 與 Bridge 通訊的 `gpio_read` / `gpio_write` 工具 |
| 設定指令 | `src/peripherals/uno_q_setup.rs` | `zeroclaw peripheral setup-uno-q` 透過 scp + arduino-app-cli 部署 Bridge |
| 設定檔 schema | `board = "arduino-uno-q"`、`transport = "bridge"` | 在 `config.toml` 中支援 |

使用 `--features hardware` 編譯以包含 Uno Q 支援。

---

## 前置需求

- 已設定 WiFi 的 Arduino Uno Q
- Mac 上已安裝 Arduino App Lab（用於初始設定和部署）
- LLM 的 API 金鑰（OpenRouter 等）

---

## 階段 1：Uno Q 初始設定（僅需一次）

### 1.1 透過 App Lab 設定 Uno Q

1. 下載 [Arduino App Lab](https://docs.arduino.cc/software/app-lab/)（Linux 上為 AppImage）。
2. 透過 USB 連接 Uno Q，開啟電源。
3. 開啟 App Lab，連接到開發板。
4. 依照設定精靈操作：
   - 設定使用者名稱和密碼（用於 SSH）
   - 設定 WiFi（SSID、密碼）
   - 套用韌體更新（如有）
5. 記下顯示的 IP 位址（例如 `arduino@192.168.1.42`），或稍後透過 App Lab 終端機執行 `ip addr show` 查詢。

### 1.2 驗證 SSH 存取

```bash
ssh arduino@<UNO_Q_IP>
# 輸入你設定的密碼
```

---

## 階段 2：在 Uno Q 上安裝 ZeroClaw

### 選項 A：在裝置上編譯（較簡單，約 20-40 分鐘）

```bash
# SSH 進入 Uno Q
ssh arduino@<UNO_Q_IP>

# 安裝 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 安裝編譯相依套件（Debian）
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev

# 複製 zeroclaw（或用 scp 傳送你的專案）
git clone https://github.com/theonlyhennygod/zeroclaw.git
cd zeroclaw

# 編譯（在 Uno Q 上約需 15-30 分鐘）
cargo build --release --features hardware

# 安裝
sudo cp target/release/zeroclaw /usr/local/bin/
```

### 選項 B：在 Mac 上交叉編譯（較快速）

```bash
# 在你的 Mac 上 — 新增 aarch64 目標
rustup target add aarch64-unknown-linux-gnu

# 安裝交叉編譯器（macOS；連結所需）
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# 編譯
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --features hardware

# 複製到 Uno Q
scp target/aarch64-unknown-linux-gnu/release/zeroclaw arduino@<UNO_Q_IP>:~/
ssh arduino@<UNO_Q_IP> "sudo mv ~/zeroclaw /usr/local/bin/"
```

如果交叉編譯失敗，請改用選項 A 在裝置上編譯。

---

## 階段 3：設定 ZeroClaw

### 3.1 執行 onboard 或手動建立設定

```bash
ssh arduino@<UNO_Q_IP>

# 快速設定
zeroclaw onboard --api-key YOUR_OPENROUTER_KEY --provider openrouter

# 或手動建立設定檔
mkdir -p ~/.zeroclaw/workspace
nano ~/.zeroclaw/config.toml
```

### 3.2 最小設定檔 config.toml

```toml
api_key = "YOUR_OPENROUTER_API_KEY"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"

[peripherals]
enabled = false
# GPIO 透過 Bridge 需要完成階段 4

[channels_config.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false

[agent]
compact_context = true
```

---

## 階段 4：執行 ZeroClaw Daemon

```bash
ssh arduino@<UNO_Q_IP>

# 執行 daemon（Telegram 輪詢透過 WiFi 運作）
zeroclaw daemon --host 127.0.0.1 --port 42617
```

**此時：** Telegram 聊天已可使用。向你的機器人傳送訊息 — ZeroClaw 會回覆。但尚未支援 GPIO。

---

## 階段 5：透過 Bridge 控制 GPIO（ZeroClaw 自動處理）

ZeroClaw 已內建 Bridge 應用程式和設定指令。

### 5.1 部署 Bridge 應用程式

**從你的 Mac**（在 zeroclaw 專案目錄中）：
```bash
zeroclaw peripheral setup-uno-q --host 192.168.0.48
```

**從 Uno Q**（SSH 連線中）：
```bash
zeroclaw peripheral setup-uno-q
```

此指令會將 Bridge 應用程式複製到 `~/ArduinoApps/zeroclaw-uno-q-bridge` 並啟動它。

### 5.2 加入 config.toml

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 執行 ZeroClaw

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

現在當你向 Telegram 機器人傳送 *「打開 LED」* 或 *「Set pin 13 high」* 時，ZeroClaw 會透過 Bridge 使用 `gpio_write`。

---

## 指令摘要：從頭到尾

| 步驟 | 指令 |
|------|------|
| 1 | 在 App Lab 中設定 Uno Q（WiFi、SSH） |
| 2 | `ssh arduino@<IP>` |
| 3 | `curl -sSf https://sh.rustup.rs \| sh -s -- -y && source ~/.cargo/env` |
| 4 | `sudo apt-get install -y pkg-config libssl-dev` |
| 5 | `git clone https://github.com/theonlyhennygod/zeroclaw.git && cd zeroclaw` |
| 6 | `cargo build --release --features hardware` |
| 7 | `zeroclaw onboard --api-key KEY --provider openrouter` |
| 8 | 編輯 `~/.zeroclaw/config.toml`（加入 Telegram bot_token） |
| 9 | `zeroclaw daemon --host 127.0.0.1 --port 42617` |
| 10 | 向你的 Telegram 機器人傳送訊息 — 它會回覆 |

---

## 疑難排解

- **「command not found: zeroclaw」** — 使用完整路徑：`/usr/local/bin/zeroclaw`，或確認 `~/.cargo/bin` 已加入 PATH。
- **Telegram 無回應** — 檢查 bot_token、allowed_users，以及 Uno Q 是否已連上網路（WiFi）。
- **記憶體不足** — 保持最少的 feature 啟用（Uno Q 使用 `--features hardware`）；考慮啟用 `compact_context = true`。
- **GPIO 指令無回應** — 確認 Bridge 應用程式正在執行（`zeroclaw peripheral setup-uno-q` 會部署並啟動它）。設定中必須包含 `board = "arduino-uno-q"` 和 `transport = "bridge"`。
- **LLM 供應商（GLM/Zhipu）** — 使用 `default_provider = "glm"` 或 `"zhipu"`，搭配環境變數或設定中的 `GLM_API_KEY`。ZeroClaw 會使用正確的 v4 端點。
