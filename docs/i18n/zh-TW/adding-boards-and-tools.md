# 新增開發板與工具 — ZeroClaw 硬體指南（繁體中文）

本指南說明如何在 ZeroClaw 中新增硬體開發板和自訂工具。

## 快速開始：透過 CLI 新增開發板

```bash
# 新增開發板（更新 ~/.zeroclaw/config.toml）
zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
zeroclaw peripheral add arduino-uno /dev/cu.usbmodem12345
zeroclaw peripheral add rpi-gpio native   # 用於 Raspberry Pi GPIO（Linux）

# 重新啟動 daemon 以套用設定
zeroclaw daemon --host 127.0.0.1 --port 42617
```

## 支援的開發板

| 開發板 | 傳輸方式 | 路徑範例 |
|--------|---------|---------|
| nucleo-f401re | serial | /dev/ttyACM0、/dev/cu.usbmodem* |
| arduino-uno | serial | /dev/ttyACM0、/dev/cu.usbmodem* |
| arduino-uno-q | bridge | （Uno Q IP） |
| rpi-gpio | native | native |
| esp32 | serial | /dev/ttyUSB0 |

## 手動設定

編輯 `~/.zeroclaw/config.toml`：

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets" # 選用：RAG 功能，將「打開紅色 LED」對應到 pin 13

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/cu.usbmodem12345"
baud = 115200
```

## 新增資料表（RAG）

將 `.md` 或 `.txt` 檔案放入 `docs/datasheets/`（或你設定的 `datasheet_dir`）。檔案以開發板名稱命名：`nucleo-f401re.md`、`arduino-uno.md`。

### 腳位別名（建議設定）

新增 `## Pin Aliases` 區段，讓代理能將「red led」對應到 pin 13：

```markdown
# My Board

## Pin Aliases

| alias       | pin |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 5   |
```

或使用鍵值格式：

```markdown
## Pin Aliases
red_led: 13
builtin_led: 13
```

### PDF 資料表

啟用 `rag-pdf` feature 後，ZeroClaw 可以索引 PDF 檔案：

```bash
cargo build --features hardware,rag-pdf
```

將 PDF 放入資料表目錄中，系統會自動擷取內容並分塊以供 RAG 使用。

## 新增開發板類型

1. **建立資料表** — 在 `docs/datasheets/my-board.md` 中包含腳位別名和 GPIO 資訊。
2. **加入設定** — `zeroclaw peripheral add my-board /dev/ttyUSB0`
3. **實作周邊裝置**（選用）— 若需自訂協定，在 `src/peripherals/` 中實作 `Peripheral` trait，並在 `create_peripheral_tools` 中註冊。

完整設計請參閱 `docs/hardware-peripherals-design.md`。

## 新增自訂工具

1. 在 `src/tools/` 中實作 `Tool` trait。
2. 在 `create_peripheral_tools`（硬體工具）或代理工具註冊表中註冊。
3. 在 `src/agent/loop_.rs` 的 `tool_descs` 中加入工具描述。

## CLI 參考

| 指令 | 說明 |
|------|------|
| `zeroclaw peripheral list` | 列出已設定的開發板 |
| `zeroclaw peripheral add <board> <path>` | 新增開發板（寫入設定檔） |
| `zeroclaw peripheral flash` | 燒錄 Arduino 韌體 |
| `zeroclaw peripheral flash-nucleo` | 燒錄 Nucleo 韌體 |
| `zeroclaw hardware discover` | 列出 USB 裝置 |
| `zeroclaw hardware info` | 透過 probe-rs 取得晶片資訊 |

## 疑難排解

- **找不到序列埠** — macOS 上使用 `/dev/cu.usbmodem*`；Linux 上使用 `/dev/ttyACM0` 或 `/dev/ttyUSB0`。
- **啟用硬體功能編譯** — `cargo build --features hardware`
- **Nucleo 使用 probe-rs** — `cargo build --features hardware,probe`
