# 硬體周邊裝置設計 — ZeroClaw（繁體中文）

ZeroClaw 讓微控制器（MCU）和單板電腦（SBC）能夠**動態解讀自然語言指令**、產生針對特定硬體的程式碼，並即時執行周邊裝置互動操作。

## 1. 願景

**目標：** ZeroClaw 作為具備硬體感知能力的 AI 代理：
- 透過頻道（WhatsApp、Telegram）接收自然語言觸發指令（例如「移動 X 機械臂」、「打開 LED」）
- 擷取精確的硬體技術文件（資料表、暫存器映射表）
- 使用 LLM（Gemini、本地開源模型）合成 Rust 程式碼 / 邏輯
- 執行邏輯以操控周邊裝置（GPIO、I2C、SPI）
- 將最佳化後的程式碼持久化以供日後重複使用

**思維模型：** ZeroClaw = 理解硬體的大腦。周邊裝置 = 它控制的手腳。

## 2. 兩種運作模式

### 模式 1：邊緣原生（獨立運作）

**目標裝置：** 具備 Wi-Fi 功能的開發板（ESP32、Raspberry Pi）。

ZeroClaw **直接在裝置上執行**。開發板啟動 gRPC/nanoRPC 伺服器，並在本機與周邊裝置通訊。

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  ZeroClaw on ESP32 / Raspberry Pi (Edge-Native)                             │
│                                                                             │
│  ┌─────────────┐    ┌──────────────┐    ┌─────────────────────────────────┐ │
│  │ Channels    │───►│ Agent Loop   │───►│ RAG: datasheets, register maps  │ │
│  │ WhatsApp    │    │ (LLM calls)  │    │ → LLM context                    │ │
│  │ Telegram    │    └──────┬───────┘    └─────────────────────────────────┘ │
│  └─────────────┘           │                                                 │
│                            ▼                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │ Code synthesis → Wasm / dynamic exec → GPIO / I2C / SPI → persist       ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│                                                                             │
│  gRPC/nanoRPC server ◄──► Peripherals (GPIO, I2C, SPI, sensors, actuators)  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**工作流程：**
1. 使用者透過 WhatsApp 發送：*「把 pin 13 的 LED 打開」*
2. ZeroClaw 擷取開發板專屬文件（例如 ESP32 GPIO 腳位映射）
3. LLM 合成 Rust 程式碼
4. 程式碼在沙盒中執行（Wasm 或動態連結）
5. GPIO 被觸發切換；結果回傳給使用者
6. 最佳化後的程式碼被持久化，供未來「打開 LED」請求重複使用

**所有操作皆在裝置上完成。** 不需要主機。

### 模式 2：主機中介（開發 / 除錯）

**目標裝置：** 透過 USB / J-Link / Aardvark 連接到主機（macOS、Linux）的硬體。

ZeroClaw 在**主機**上執行，並維持與目標硬體的感知連結。用於開發、內省和燒錄韌體。

```
┌─────────────────────┐                    ┌──────────────────────────────────┐
│  ZeroClaw on Mac    │   USB / J-Link /   │  STM32 Nucleo-F401RE              │
│                     │   Aardvark         │  (or other MCU)                    │
│  - Channels         │ ◄────────────────► │  - Memory map                     │
│  - LLM              │                    │  - Peripherals (GPIO, ADC, I2C)    │
│  - Hardware probe   │   VID/PID          │  - Flash / RAM                     │
│  - Flash / debug    │   discovery        │                                    │
└─────────────────────┘                    └──────────────────────────────────┘
```

**工作流程：**
1. 使用者透過 Telegram 發送：*「這個 USB 裝置上有哪些可讀取的記憶體位址？」*
2. ZeroClaw 辨識已連接的硬體（VID/PID、架構）
3. 執行記憶體映射；建議可用的位址空間
4. 將結果回傳給使用者

**或者：**
1. 使用者：*「把這個韌體燒錄到 Nucleo 上」*
2. ZeroClaw 透過 OpenOCD 或 probe-rs 進行寫入 / 燒錄
3. 確認燒錄成功

**或者：**
1. ZeroClaw 自動發現：*「在 /dev/ttyACM0 偵測到 STM32 Nucleo，ARM Cortex-M4」*
2. 建議：*「我可以讀寫 GPIO、ADC、快閃記憶體。你想做什麼？」*

---

### 模式比較

| 面向 | 邊緣原生 | 主機中介 |
|------|---------|---------|
| ZeroClaw 執行於 | 裝置（ESP32、RPi） | 主機（Mac、Linux） |
| 硬體連結 | 本機（GPIO、I2C、SPI） | USB、J-Link、Aardvark |
| LLM | 裝置端或雲端（Gemini） | 主機端（雲端或本地） |
| 使用場景 | 生產環境、獨立運作 | 開發、除錯、內省 |
| 頻道 | WhatsApp 等（透過 WiFi） | Telegram、CLI 等 |

## 3. 舊版 / 簡化模式（LLM-on-Edge 之前）

適用於沒有 WiFi 的開發板，或在完整邊緣原生模式就緒之前：

### 模式 A：主機 + 遠端周邊（STM32 透過序列埠）

主機執行 ZeroClaw；周邊裝置執行最小化韌體。透過序列埠傳輸簡單的 JSON。

### 模式 B：RPi 作為主機（原生 GPIO）

ZeroClaw 在 Pi 上執行；GPIO 透過 rppal 或 sysfs 存取。不需要額外韌體。

## 4. 技術需求

| 需求 | 說明 |
|------|------|
| **語言** | 純 Rust。嵌入式目標（STM32、ESP32）適用時使用 `no_std`。 |
| **通訊** | 輕量級 gRPC 或 nanoRPC 堆疊，用於低延遲指令處理。 |
| **動態執行** | 安全地即時執行 LLM 產生的邏輯：以 Wasm runtime 進行隔離，或在支援的平台上使用動態連結。 |
| **技術文件檢索** | RAG（檢索增強生成）管線，將資料表片段、暫存器映射和腳位配置注入 LLM 上下文。 |
| **硬體發現** | USB 裝置的 VID/PID 辨識；架構偵測（ARM Cortex-M、RISC-V 等）。 |

### RAG 管線（資料表檢索）

- **索引：** 資料表、參考手冊、暫存器映射表（PDF → 區塊、嵌入向量）。
- **檢索：** 根據使用者查詢（「打開 LED」），擷取相關片段（例如目標開發板的 GPIO 章節）。
- **注入：** 加入 LLM 系統提示詞或上下文中。
- **結果：** LLM 產生精確的、針對特定開發板的程式碼。

### 動態執行選項

| 選項 | 優點 | 缺點 |
|------|------|------|
| **Wasm** | 沙盒隔離、可攜、無需 FFI | 額外開銷；Wasm 對硬體存取有限制 |
| **動態連結** | 原生速度、完整硬體存取 | 平台相依；安全性疑慮 |
| **直譯式 DSL** | 安全、可稽核 | 較慢；表達能力有限 |
| **預編譯模板** | 快速、安全 | 彈性較低；需要模板庫 |

**建議：** 先從預編譯模板 + 參數化開始；待穩定後再演進到 Wasm 以支援使用者自訂邏輯。

## 5. CLI 與設定

### CLI 旗標

```bash
# 邊緣原生：在裝置上執行（ESP32、RPi）
zeroclaw agent --mode edge

# 主機中介：連接 USB/J-Link 目標
zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
zeroclaw agent --probe jlink

# 硬體內省
zeroclaw hardware discover
zeroclaw hardware introspect /dev/ttyACM0
```

### 設定檔（config.toml）

```toml
[peripherals]
enabled = true
mode = "host"  # "edge" | "host"
datasheet_dir = "docs/datasheets"  # RAG：開發板專屬文件，供 LLM 上下文使用

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

[[peripherals.boards]]
board = "esp32"
transport = "wifi"
# 邊緣原生：ZeroClaw 在 ESP32 上執行
```

## 6. 架構：周邊裝置作為擴充點

### 新 Trait：`Peripheral`

```rust
/// 一個硬體周邊裝置，以工具形式暴露其功能。
#[async_trait]
pub trait Peripheral: Send + Sync {
    fn name(&self) -> &str;
    fn board_type(&self) -> &str;  // 例如 "nucleo-f401re"、"rpi-gpio"
    async fn connect(&mut self) -> anyhow::Result<()>;
    async fn disconnect(&mut self) -> anyhow::Result<()>;
    async fn health_check(&self) -> bool;
    /// 此周邊裝置提供的工具（gpio_read、gpio_write、sensor_read 等）
    fn tools(&self) -> Vec<Box<dyn Tool>>;
}
```

### 流程

1. **啟動：** ZeroClaw 載入設定，偵測到 `peripherals.boards`。
2. **連線：** 針對每個開發板，建立 `Peripheral` 實作，呼叫 `connect()`。
3. **工具：** 從所有已連線的周邊裝置收集工具；與預設工具合併。
4. **代理迴圈：** 代理可呼叫 `gpio_write`、`sensor_read` 等 — 這些操作會委派給對應的周邊裝置。
5. **關閉：** 對每個周邊裝置呼叫 `disconnect()`。

### 開發板支援

| 開發板 | 傳輸方式 | 韌體 / 驅動程式 | 工具 |
|--------|---------|----------------|------|
| nucleo-f401re | serial | Zephyr / Embassy | gpio_read、gpio_write、adc_read |
| rpi-gpio | native | rppal 或 sysfs | gpio_read、gpio_write |
| esp32 | serial/ws | ESP-IDF / Embassy | gpio、wifi、mqtt |

## 7. 通訊協定

### gRPC / nanoRPC（邊緣原生、主機中介）

用於 ZeroClaw 與周邊裝置之間的低延遲型別化 RPC：

- **nanoRPC** 或 **tonic**（gRPC）：以 Protobuf 定義服務。
- 方法：`GpioWrite`、`GpioRead`、`I2cTransfer`、`SpiTransfer`、`MemoryRead`、`FlashWrite` 等。
- 支援串流、雙向呼叫，以及從 `.proto` 檔案產生程式碼。

### 序列埠回退（主機中介、舊版）

適用於不支援 gRPC 的開發板，透過序列埠傳輸簡單 JSON：

**請求（主機 → 周邊）：**
```json
{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}
```

**回應（周邊 → 主機）：**
```json
{"id":"1","ok":true,"result":"done"}
```

## 8. 韌體（獨立 Repo 或 Crate）

- **zeroclaw-firmware** 或 **zeroclaw-peripheral** — 獨立的 crate/workspace。
- 目標平台：`thumbv7em-none-eabihf`（STM32）、`armv7-unknown-linux-gnueabihf`（RPi）等。
- STM32 使用 `embassy` 或 Zephyr。
- 實作上述通訊協定。
- 使用者將韌體燒錄到開發板；ZeroClaw 連線後自動發現功能。

## 9. 實作階段

### 第 1 階段：骨架 ✅（已完成）

- [x] 新增 `Peripheral` trait、設定檔 schema、CLI（`zeroclaw peripheral list/add`）
- [x] 為 agent 新增 `--peripheral` 旗標
- [x] 撰寫 AGENTS.md 文件

### 第 2 階段：主機中介 — 硬體發現 ✅（已完成）

- [x] `zeroclaw hardware discover`：列舉 USB 裝置（VID/PID）
- [x] 開發板註冊表：VID/PID → 架構、名稱的映射（例如 Nucleo-F401RE）
- [x] `zeroclaw hardware introspect <path>`：記憶體映射、周邊裝置清單

### 第 3 階段：主機中介 — 序列埠 / J-Link

- [x] 用於 STM32 USB CDC 的 `SerialPeripheral`
- [ ] probe-rs 或 OpenOCD 整合，用於燒錄 / 除錯
- [x] 工具：`gpio_read`、`gpio_write`（未來加入 memory_read、flash_write）

### 第 4 階段：RAG 管線 ✅（已完成）

- [x] 資料表索引（markdown/text → 區塊）
- [x] 針對硬體相關查詢，檢索並注入 LLM 上下文
- [x] 開發板專屬的提示詞增強

**使用方式：** 在 config.toml 的 `[peripherals]` 中加入 `datasheet_dir = "docs/datasheets"`。將 `.md` 或 `.txt` 檔案以開發板名稱命名放入該目錄（例如 `nucleo-f401re.md`、`rpi-gpio.md`）。`_generic/` 資料夾中或名為 `generic.md` 的檔案適用於所有開發板。區塊透過關鍵字比對進行檢索，並注入使用者訊息上下文中。

### 第 5 階段：邊緣原生 — RPi ✅（已完成）

- [x] ZeroClaw 在 Raspberry Pi 上執行（透過 rppal 原生存取 GPIO）
- [ ] gRPC/nanoRPC 伺服器，用於本機周邊裝置存取
- [ ] 程式碼持久化（儲存合成的程式碼片段）

### 第 6 階段：邊緣原生 — ESP32

- [x] 主機中介 ESP32（序列埠傳輸）— 與 STM32 相同的 JSON 協定
- [x] `zeroclaw-esp32` 韌體 crate（`firmware/zeroclaw-esp32`）— GPIO over UART
- [x] ESP32 加入硬體註冊表（CH340 VID/PID）
- [ ] ZeroClaw *在* ESP32 上執行（WiFi + LLM，邊緣原生）— 未來規劃
- [ ] Wasm 或基於模板的執行方式，用於 LLM 產生的邏輯

**使用方式：** 將 `firmware/zeroclaw-esp32` 燒錄到 ESP32，在設定中加入 `board = "esp32"`、`transport = "serial"`、`path = "/dev/ttyUSB0"`。

### 第 7 階段：動態執行（LLM 產生的程式碼）

- [ ] 模板庫：參數化的 GPIO/I2C/SPI 程式碼片段
- [ ] 選用：Wasm runtime，用於使用者自訂邏輯（沙盒隔離）
- [ ] 持久化並重複使用最佳化的程式碼路徑

## 10. 安全考量

- **序列埠路徑：** 驗證 `path` 是否在允許清單內（例如 `/dev/ttyACM*`、`/dev/ttyUSB*`）；絕不允許任意路徑。
- **GPIO：** 限制暴露的腳位；避免電源 / 重設腳位。
- **周邊裝置上不存放機密資料：** 韌體不應儲存 API 金鑰；由主機處理認證。

## 11. 非目標（目前階段）

- 在裸機 STM32 上執行完整 ZeroClaw（無 WiFi、記憶體有限）— 改用主機中介模式
- 即時性保證 — 周邊裝置操作為盡力而為
- 從 LLM 執行任意原生程式碼 — 建議使用 Wasm 或模板

## 12. 相關文件

- [adding-boards-and-tools.md](./adding-boards-and-tools.md) — 如何新增開發板和資料表
- [network-deployment.md](./network-deployment.md) — RPi 與網路部署

## 13. 參考資料

- [Zephyr RTOS Rust support](https://docs.zephyrproject.org/latest/develop/languages/rust/index.html)
- [Embassy](https://embassy.dev/) — 非同步嵌入式框架
- [rppal](https://github.com/golemparts/rppal) — Rust 版 Raspberry Pi GPIO
- [STM32 Nucleo-F401RE](https://www.st.com/en/evaluation-tools/nucleo-f401re.html)
- [tonic](https://github.com/hyperium/tonic) — Rust 版 gRPC
- [probe-rs](https://probe.rs/) — ARM 除錯探針、燒錄、記憶體存取
- [nusb](https://github.com/nic-hartley/nusb) — USB 裝置列舉（VID/PID）

## 14. 原始提示詞摘要

> *「像 ESP、Raspberry Pi 這類具備 WiFi 的開發板可以連接到 LLM（Gemini 或開源模型）。ZeroClaw 在裝置上執行，建立自己的 gRPC，啟動伺服器，並與周邊裝置通訊。使用者透過 WhatsApp 詢問：『移動 X 機械臂』或『打開 LED』。ZeroClaw 取得精確的技術文件，撰寫程式碼，執行它，以最佳方式儲存，然後運行它，把 LED 打開 — 全部在開發板上完成。*
>
> *對於透過 USB/J-Link/Aardvark 連接到我 Mac 的 STM Nucleo：ZeroClaw 從我的 Mac 存取硬體，在裝置上安裝或寫入所需內容，然後回傳結果。範例：『嘿 ZeroClaw，這個 USB 裝置上有哪些可用 / 可讀的位址？』它可以判斷什麼連接在哪裡並提出建議。」*
