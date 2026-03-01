# ZeroClaw 搭配 Nucleo-F401RE — 逐步設定指南（繁體中文）

在你的 Mac 或 Linux 主機上執行 ZeroClaw。透過 USB 連接 Nucleo-F401RE。使用 Telegram 或 CLI 控制 GPIO（LED、腳位）。

---

## 透過 Telegram 取得開發板資訊（不需要燒錄韌體）

ZeroClaw 可以透過 USB **直接讀取 Nucleo 的晶片資訊，無需事先燒錄任何韌體**。向你的 Telegram 機器人發送以下訊息：

- *「我有什麼開發板資訊？」*
- *「Board info」*
- *「有什麼硬體連接著？」*
- *「Chip info」*

代理使用 `hardware_board_info` 工具回傳晶片名稱、架構和記憶體映射資訊。啟用 `probe` feature 時，會透過 USB/SWD 讀取即時資料；否則回傳靜態的資料表資訊。

**設定：** 先在 `config.toml` 中加入 Nucleo（讓代理知道要查詢哪塊開發板）：

```toml
[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200
```

**CLI 替代方式：**

```bash
cargo build --features hardware,probe
zeroclaw hardware info
zeroclaw hardware discover
```

---

## 已包含的內容（不需要修改程式碼）

ZeroClaw 已包含 Nucleo-F401RE 所需的一切：

| 元件 | 位置 | 用途 |
|------|------|------|
| 韌體 | `firmware/zeroclaw-nucleo/` | Embassy Rust — USART2（115200）、gpio_read、gpio_write |
| 序列埠周邊 | `src/peripherals/serial.rs` | JSON-over-serial 協定（與 Arduino/ESP32 相同） |
| 燒錄指令 | `zeroclaw peripheral flash-nucleo` | 建置韌體，透過 probe-rs 燒錄 |

協定：以換行分隔的 JSON。請求：`{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`。回應：`{"id":"1","ok":true,"result":"done"}`。

---

## 前置需求

- Nucleo-F401RE 開發板
- USB 連接線（USB-A 轉 Mini-USB；Nucleo 內建 ST-Link）
- 燒錄韌體需安裝：`cargo install probe-rs-tools --locked`（或使用[安裝腳本](https://probe.rs/docs/getting-started/installation/)）

---

## 階段 1：燒錄韌體

### 1.1 連接 Nucleo

1. 透過 USB 將 Nucleo 連接到你的 Mac/Linux。
2. 開發板會以 USB 裝置（ST-Link）形式出現。現代系統不需要額外安裝驅動程式。

### 1.2 透過 ZeroClaw 燒錄

在 zeroclaw 專案根目錄執行：

```bash
zeroclaw peripheral flash-nucleo
```

此指令會建置 `firmware/zeroclaw-nucleo` 並執行 `probe-rs run --chip STM32F401RETx`。韌體燒錄後會立即開始執行。

### 1.3 手動燒錄（替代方式）

```bash
cd firmware/zeroclaw-nucleo
cargo build --release --target thumbv7em-none-eabihf
probe-rs run --chip STM32F401RETx target/thumbv7em-none-eabihf/release/zeroclaw-nucleo
```

---

## 階段 2：尋找序列埠

- **macOS：** `/dev/cu.usbmodem*` 或 `/dev/tty.usbmodem*`（例如 `/dev/cu.usbmodem101`）
- **Linux：** `/dev/ttyACM0`（或插入後執行 `dmesg` 查看）

USART2（PA2/PA3）橋接到 ST-Link 的虛擬 COM 埠，因此主機只會看到一個序列裝置。

---

## 階段 3：設定 ZeroClaw

在 `~/.zeroclaw/config.toml` 中加入：

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/cu.usbmodem101"   # 調整為你的連接埠
baud = 115200
```

---

## 階段 4：執行與測試

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

或直接使用代理：

```bash
zeroclaw agent --message "Turn on the LED on pin 13"
```

Pin 13 = PA5 = Nucleo-F401RE 上的使用者 LED（LD2）。

---

## 指令摘要

| 步驟 | 指令 |
|------|------|
| 1 | 透過 USB 連接 Nucleo |
| 2 | `cargo install probe-rs-tools --locked` |
| 3 | `zeroclaw peripheral flash-nucleo` |
| 4 | 在 config.toml 中加入 Nucleo（path = 你的序列埠） |
| 5 | `zeroclaw daemon` 或 `zeroclaw agent -m "Turn on LED"` |

---

## 疑難排解

- **無法辨識 flash-nucleo** — 從專案目錄建置：`cargo run --features hardware -- peripheral flash-nucleo`。此子指令僅在從原始碼建置時可用，crates.io 安裝版本不包含。
- **找不到 probe-rs** — `cargo install probe-rs-tools --locked`（`probe-rs` crate 是函式庫；CLI 工具在 `probe-rs-tools` 中）
- **未偵測到探針** — 確認 Nucleo 已連接。嘗試更換 USB 線或連接埠。
- **找不到序列埠** — Linux 上，將使用者加入 `dialout` 群組：`sudo usermod -a -G dialout $USER`，然後重新登入。
- **GPIO 指令無回應** — 檢查設定中的 `path` 是否與你的序列埠一致。執行 `zeroclaw peripheral list` 進行確認。
