# 網路部署 — 在 Raspberry Pi 與區域網路上部署 ZeroClaw（繁體中文）

本文件涵蓋在 Raspberry Pi 或區域網路內其他主機上部署 ZeroClaw，搭配 Telegram 及可選的 webhook 頻道。

---

## 1. 概觀

| 模式 | 需要開放入站連接埠？ | 使用情境 |
|------|----------------------|----------|
| **Telegram 輪詢** | 否 | ZeroClaw 主動輪詢 Telegram API；任何地方皆可運作 |
| **Matrix 同步（含 E2EE）** | 否 | ZeroClaw 透過 Matrix client API 同步；不需要入站 webhook |
| **Discord/Slack** | 否 | 同上 — 僅出站連線 |
| **Nostr** | 否 | 透過 WebSocket 連接中繼伺服器；僅出站連線 |
| **Gateway webhook** | 是 | POST /webhook、/whatsapp、/linq、/nextcloud-talk 需要公開 URL |
| **Gateway 配對** | 是 | 透過 gateway 配對用戶端時需要 |
| **Alpine/OpenRC 服務** | 否 | Alpine Linux 上的系統級背景服務 |

**重點：** Telegram、Discord、Slack 和 Nostr 使用**出站連線** — ZeroClaw 主動連接外部伺服器/中繼站。不需要通訊埠轉發或公開 IP。

---

## 2. 在 Raspberry Pi 上執行 ZeroClaw

### 2.1 前置條件

- Raspberry Pi（3/4/5），搭載 Raspberry Pi OS
- USB 周邊裝置（Arduino、Nucleo），若使用序列傳輸
- 可選：`rppal` 用於原生 GPIO（`peripheral-rpi` feature）

### 2.2 安裝

```bash
# 為 RPi 建置（或從主機交叉編譯）
cargo build --release --features hardware

# 或使用你偏好的安裝方式
```

### 2.3 設定

編輯 `~/.zeroclaw/config.toml`：

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

# 或透過 USB 連接 Arduino
[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = []

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false
```

### 2.4 執行 Daemon（僅本機）

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

- Gateway 綁定至 `127.0.0.1` — 其他機器無法存取
- Telegram 頻道正常運作：ZeroClaw 主動輪詢 Telegram API（出站）
- 不需要防火牆規則或通訊埠轉發

---

## 3. 綁定至 0.0.0.0（區域網路）

若要讓區域網路上的其他裝置存取 gateway（例如配對或 webhook）：

### 3.1 選項 A：明確啟用

```toml
[gateway]
host = "0.0.0.0"
port = 42617
allow_public_bind = true
```

```bash
zeroclaw daemon --host 0.0.0.0 --port 42617
```

**安全提醒：** `allow_public_bind = true` 會將 gateway 暴露在區域網路上。僅在受信任的區域網路中使用。

### 3.2 選項 B：使用通道（建議用於 Webhook）

若需要**公開 URL**（例如 WhatsApp webhook、外部用戶端）：

1. 在 localhost 上執行 gateway：
   ```bash
   zeroclaw daemon --host 127.0.0.1 --port 42617
   ```

2. 啟動通道：
   ```toml
   [tunnel]
   provider = "tailscale"   # 或 "ngrok"、"cloudflare"
   ```
   或使用 `zeroclaw tunnel`（參閱通道文件）。

3. 除非 `allow_public_bind = true` 或通道已啟用，ZeroClaw 會拒絕綁定 `0.0.0.0`。

---

## 4. Telegram 輪詢（不需入站連接埠）

Telegram 預設使用**長輪詢（long-polling）**：

- ZeroClaw 呼叫 `https://api.telegram.org/bot{token}/getUpdates`
- 不需要入站連接埠或公開 IP
- 可在 NAT 後方、RPi 上、家用實驗室中運作

**設定：**

```toml
[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = []            # 預設拒絕所有，明確綁定身分
```

執行 `zeroclaw daemon` — Telegram 頻道會自動啟動。

若要在執行期核准一個 Telegram 帳號：

```bash
zeroclaw channel bind-telegram <IDENTITY>
```

`<IDENTITY>` 可以是 Telegram 數字使用者 ID 或使用者名稱（不含 `@`）。

### 4.1 單一輪詢器規則（重要）

Telegram Bot API `getUpdates` 每個 bot token 同時只支援一個輪詢器。

- 同一 token 僅保持一個執行中的實例（建議：`zeroclaw daemon` 服務）。
- 不要同時執行 `cargo run -- channel start` 或其他 bot 行程。

若遇到此錯誤：

`Conflict: terminated by other getUpdates request`

表示存在輪詢衝突。停止多餘的實例，只重新啟動一個 daemon。

---

## 5. Webhook 頻道（WhatsApp、Nextcloud Talk、自訂）

基於 webhook 的頻道需要**公開 URL**，讓 Meta（WhatsApp）或你的用戶端能 POST 事件。

### 5.1 Tailscale Funnel

```toml
[tunnel]
provider = "tailscale"
```

Tailscale Funnel 透過 `*.ts.net` URL 暴露你的 gateway。不需要通訊埠轉發。

### 5.2 ngrok

```toml
[tunnel]
provider = "ngrok"
```

或手動執行 ngrok：
```bash
ngrok http 42617
# 使用 HTTPS URL 作為你的 webhook 位址
```

### 5.3 Cloudflare Tunnel

設定 Cloudflare Tunnel 轉發至 `127.0.0.1:42617`，然後將 webhook URL 指向通道的公開主機名稱。

---

## 6. 檢查清單：RPi 部署

- [ ] 使用 `--features hardware` 建置（若使用原生 GPIO 則加上 `peripheral-rpi`）
- [ ] 設定 `[peripherals]` 和 `[channels_config.telegram]`
- [ ] 執行 `zeroclaw daemon --host 127.0.0.1 --port 42617`（Telegram 不需要 0.0.0.0 即可運作）
- [ ] 區域網路存取：`--host 0.0.0.0` + 在設定中啟用 `allow_public_bind = true`
- [ ] Webhook 需求：使用 Tailscale、ngrok 或 Cloudflare tunnel

---

## 7. OpenRC（Alpine Linux 服務）

ZeroClaw 支援 Alpine Linux 及其他使用 OpenRC 初始化系統的發行版。OpenRC 服務為**系統級**執行，需要 root/sudo 權限。

### 7.1 前置條件

- Alpine Linux（或其他基於 OpenRC 的發行版）
- Root 或 sudo 存取權限
- 專用的 `zeroclaw` 系統使用者（安裝時建立）

### 7.2 安裝服務

```bash
# 安裝服務（Alpine 上會自動偵測 OpenRC）
sudo zeroclaw service install
```

此指令會建立：
- 初始化腳本：`/etc/init.d/zeroclaw`
- 設定目錄：`/etc/zeroclaw/`
- 日誌目錄：`/var/log/zeroclaw/`

### 7.3 設定

通常不需要手動複製設定檔。

`sudo zeroclaw service install` 會自動準備 `/etc/zeroclaw`，並在可用時從你的使用者設定遷移既有的執行期狀態，同時為 `zeroclaw` 服務使用者設定擁有權與權限。

若沒有可遷移的既有執行期狀態，請在啟動服務前建立 `/etc/zeroclaw/config.toml`。

### 7.4 啟用並啟動

```bash
# 加入預設 runlevel
sudo rc-update add zeroclaw default

# 啟動服務
sudo rc-service zeroclaw start

# 確認狀態
sudo rc-service zeroclaw status
```

### 7.5 管理服務

| 指令 | 說明 |
|------|------|
| `sudo rc-service zeroclaw start` | 啟動 daemon |
| `sudo rc-service zeroclaw stop` | 停止 daemon |
| `sudo rc-service zeroclaw status` | 確認服務狀態 |
| `sudo rc-service zeroclaw restart` | 重新啟動 daemon |
| `sudo zeroclaw service status` | ZeroClaw 狀態包裝器（使用 `/etc/zeroclaw` 設定） |

### 7.6 日誌

OpenRC 將日誌導向：

| 日誌 | 路徑 |
|------|------|
| 存取/stdout | `/var/log/zeroclaw/access.log` |
| 錯誤/stderr | `/var/log/zeroclaw/error.log` |

檢視日誌：

```bash
sudo tail -f /var/log/zeroclaw/error.log
```

### 7.7 解除安裝

```bash
# 停止服務並從 runlevel 移除
sudo rc-service zeroclaw stop
sudo rc-update del zeroclaw default

# 移除初始化腳本
sudo zeroclaw service uninstall
```

### 7.8 注意事項

- OpenRC 僅支援**系統級**服務（無使用者級服務）
- 所有服務操作皆需 `sudo` 或 root 權限
- 服務以 `zeroclaw:zeroclaw` 使用者身分執行（最小權限原則）
- 設定檔必須位於 `/etc/zeroclaw/config.toml`（初始化腳本中的明確路徑）
- 若 `zeroclaw` 使用者不存在，安裝會失敗並提示建立步驟

### 7.9 檢查清單：Alpine/OpenRC 部署

- [ ] 安裝：`sudo zeroclaw service install`
- [ ] 啟用：`sudo rc-update add zeroclaw default`
- [ ] 啟動：`sudo rc-service zeroclaw start`
- [ ] 驗證：`sudo rc-service zeroclaw status`
- [ ] 檢查日誌：`/var/log/zeroclaw/error.log`

---

## 8. 參考資料

- [channels-reference.md](./channels-reference.md) — 頻道設定總覽
- [matrix-e2ee-guide.md](./matrix-e2ee-guide.md) — Matrix 設定與加密房間疑難排解
- [hardware-peripherals-design.md](./hardware-peripherals-design.md) — 周邊裝置設計
- [adding-boards-and-tools.md](./adding-boards-and-tools.md) — 硬體設定與新增開發板
