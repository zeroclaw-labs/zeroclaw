# Thiết kế Hardware Peripherals — ZeroClaw

ZeroClaw cho phép các vi điều khiển (MCU) và máy tính nhúng (SBC) **phân tích lệnh ngôn ngữ tự nhiên theo thời gian thực**, tổng hợp code phù hợp với từng phần cứng, và thực thi tương tác với ngoại vi trực tiếp.

## 1. Tầm nhìn

**Mục tiêu:** ZeroClaw đóng vai trò là AI agent có hiểu biết về phần cứng, cụ thể:
- Nhận lệnh ngôn ngữ tự nhiên (ví dụ: "Di chuyển cánh tay X", "Bật LED") qua các kênh như WhatsApp, Telegram
- Truy xuất tài liệu phần cứng chính xác (datasheet, register map)
- Tổng hợp code/logic Rust bằng LLM (Gemini, các mô hình mã nguồn mở)
- Thực thi logic để điều khiển ngoại vi (GPIO, I2C, SPI)
- Lưu trữ code tối ưu để tái sử dụng về sau

**Hình dung trực quan:** ZeroClaw = bộ não hiểu phần cứng. Ngoại vi = tay chân mà nó điều khiển.

## 2. Hai chế độ vận hành

### Chế độ 1: Edge-Native (Độc lập trên thiết bị)

**Mục tiêu:** Các board có WiFi (ESP32, Raspberry Pi).

ZeroClaw chạy **trực tiếp trên thiết bị**. Board khởi động server gRPC/nanoRPC và giao tiếp với ngoại vi ngay tại chỗ.

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

**Luồng xử lý:**
1. Người dùng gửi WhatsApp: *"Turn on LED on pin 13"*
2. ZeroClaw truy xuất tài liệu theo board (ví dụ: bản đồ GPIO của ESP32)
3. LLM tổng hợp code Rust
4. Code chạy trong sandbox (Wasm hoặc dynamic linking)
5. GPIO được bật/tắt; kết quả trả về người dùng
6. Code tối ưu được lưu lại để tái sử dụng cho các yêu cầu "Turn on LED" sau này

**Toàn bộ diễn ra trên thiết bị.** Không cần máy chủ trung gian.

### Chế độ 2: Host-Mediated (Phát triển / Gỡ lỗi)

**Mục tiêu:** Phần cứng kết nối qua USB / J-Link / Aardvark với máy chủ (macOS, Linux).

ZeroClaw chạy trên **máy chủ** và duy trì kết nối phần cứng tới thiết bị mục tiêu. Dùng cho phát triển, kiểm tra nội tâm, và nạp firmware.

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

**Luồng xử lý:**
1. Người dùng gửi Telegram: *"What are the readable memory addresses on this USB device?"*
2. ZeroClaw nhận diện phần cứng đang kết nối (VID/PID, kiến trúc)
3. Thực hiện ánh xạ bộ nhớ; gợi ý các vùng địa chỉ khả dụng
4. Trả kết quả về người dùng

**Hoặc:**
1. Người dùng: *"Flash this firmware to the Nucleo"*
2. ZeroClaw ghi/nạp firmware qua OpenOCD hoặc probe-rs
3. Xác nhận thành công

**Hoặc:**
1. ZeroClaw tự phát hiện: *"STM32 Nucleo on /dev/ttyACM0, ARM Cortex-M4"*
2. Gợi ý: *"I can read/write GPIO, ADC, flash. What would you like to do?"*

---

### So sánh hai chế độ

| Khía cạnh | Edge-Native | Host-Mediated |
|-----------|-------------|---------------|
| ZeroClaw chạy trên | Thiết bị (ESP32, RPi) | Máy chủ (Mac, Linux) |
| Kết nối phần cứng | Cục bộ (GPIO, I2C, SPI) | USB, J-Link, Aardvark |
| LLM | Trên thiết bị hoặc cloud (Gemini) | Máy chủ (cloud hoặc local) |
| Trường hợp sử dụng | Sản xuất, độc lập | Phát triển, gỡ lỗi, kiểm tra |
| Kênh liên lạc | WhatsApp, v.v. (qua WiFi) | Telegram, CLI, v.v. |

## 3. Các chế độ cũ / Đơn giản hơn (Trước khi có LLM trên Edge)

Dành cho các board không có WiFi hoặc trước khi Edge-Native hoàn chỉnh:

### Chế độ A: Host + Remote Peripheral (STM32 qua serial)

Máy chủ chạy ZeroClaw; ngoại vi chạy firmware tối giản. JSON đơn giản qua serial.

### Chế độ B: RPi làm Host (Native GPIO)

ZeroClaw trên Pi; GPIO qua rppal hoặc sysfs. Không cần firmware riêng.

## 4. Yêu cầu kỹ thuật

| Yêu cầu | Mô tả |
|---------|-------|
| **Ngôn ngữ** | Thuần Rust. `no_std` khi áp dụng được cho các target nhúng (STM32, ESP32). |
| **Giao tiếp** | Stack gRPC hoặc nanoRPC nhẹ để xử lý lệnh với độ trễ thấp. |
| **Thực thi động** | Chạy an toàn logic do LLM tạo ra theo thời gian thực: Wasm runtime để cô lập, hoặc dynamic linking khi được hỗ trợ. |
| **Truy xuất tài liệu** | Pipeline RAG (Retrieval-Augmented Generation) để đưa đoạn trích datasheet, register map và pinout vào ngữ cảnh LLM. |
| **Nhận diện phần cứng** | Nhận dạng thiết bị USB qua VID/PID; phát hiện kiến trúc (ARM Cortex-M, RISC-V, v.v.). |

### Pipeline RAG (Truy xuất Datasheet)

- **Lập chỉ mục:** Datasheet, hướng dẫn tham chiếu, register map (PDF → các đoạn, embeddings).
- **Truy xuất:** Khi người dùng hỏi ("turn on LED"), lấy các đoạn liên quan (ví dụ: phần GPIO của board mục tiêu).
- **Chèn vào:** Thêm vào system prompt hoặc ngữ cảnh LLM.
- **Kết quả:** LLM tạo code chính xác, đặc thù cho từng board.

### Các lựa chọn thực thi động

| Lựa chọn | Ưu điểm | Nhược điểm |
|----------|---------|-----------|
| **Wasm** | Sandboxed, di động, không cần FFI | Overhead; truy cập phần cứng từ Wasm bị hạn chế |
| **Dynamic linking** | Tốc độ native, truy cập phần cứng đầy đủ | Phụ thuộc nền tảng; lo ngại bảo mật |
| **Interpreted DSL** | An toàn, có thể kiểm tra | Chậm hơn; biểu đạt hạn chế |
| **Pre-compiled templates** | Nhanh, bảo mật | Kém linh hoạt; cần thư viện template |

**Khuyến nghị:** Bắt đầu với pre-compiled templates + parameterization; tiến lên Wasm cho logic do người dùng định nghĩa khi đã ổn định.

## 5. CLI và Config

### CLI Flags

```bash
# Edge-Native: run on device (ESP32, RPi)
zeroclaw agent --mode edge

# Host-Mediated: connect to USB/J-Link target
zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
zeroclaw agent --probe jlink

# Hardware introspection
zeroclaw hardware discover
zeroclaw hardware introspect /dev/ttyACM0
```

### Config (config.toml)

```toml
[peripherals]
enabled = true
mode = "host"  # "edge" | "host"
datasheet_dir = "docs/datasheets"  # RAG: board-specific docs for LLM context

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
# Edge-Native: ZeroClaw runs on ESP32
```

## 6. Kiến trúc: Peripheral là điểm mở rộng

### Trait mới: `Peripheral`

```rust
/// A hardware peripheral that exposes capabilities as tools.
#[async_trait]
pub trait Peripheral: Send + Sync {
    fn name(&self) -> &str;
    fn board_type(&self) -> &str;  // e.g. "nucleo-f401re", "rpi-gpio"
    async fn connect(&mut self) -> anyhow::Result<()>;
    async fn disconnect(&mut self) -> anyhow::Result<()>;
    async fn health_check(&self) -> bool;
    /// Tools this peripheral provides (gpio_read, gpio_write, sensor_read, etc.)
    fn tools(&self) -> Vec<Box<dyn Tool>>;
}
```

### Luồng xử lý

1. **Khởi động:** ZeroClaw nạp config, đọc `peripherals.boards`.
2. **Kết nối:** Với mỗi board, tạo impl `Peripheral`, gọi `connect()`.
3. **Tools:** Thu thập tools từ tất cả peripheral đã kết nối; gộp với tools mặc định.
4. **Vòng lặp agent:** Agent có thể gọi `gpio_write`, `sensor_read`, v.v. — các lệnh này chuyển tiếp tới peripheral.
5. **Tắt máy:** Gọi `disconnect()` trên từng peripheral.

### Hỗ trợ Board

| Board | Transport | Firmware / Driver | Tools |
|-------|-----------|-------------------|-------|
| nucleo-f401re | serial | Zephyr / Embassy | gpio_read, gpio_write, adc_read |
| rpi-gpio | native | rppal or sysfs | gpio_read, gpio_write |
| esp32 | serial/ws | ESP-IDF / Embassy | gpio, wifi, mqtt |

## 7. Giao thức giao tiếp

### gRPC / nanoRPC (Edge-Native, Host-Mediated)

Dành cho RPC có kiểu dữ liệu, độ trễ thấp giữa ZeroClaw và các peripheral:

- **nanoRPC** hoặc **tonic** (gRPC): Dịch vụ định nghĩa bằng Protobuf.
- Phương thức: `GpioWrite`, `GpioRead`, `I2cTransfer`, `SpiTransfer`, `MemoryRead`, `FlashWrite`, v.v.
- Hỗ trợ streaming, gọi hai chiều, và sinh code từ file `.proto`.

### Serial Fallback (Host-Mediated, legacy)

JSON đơn giản qua serial cho các board không hỗ trợ gRPC:

**Request (host → peripheral):**
```json
{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}
```

**Response (peripheral → host):**
```json
{"id":"1","ok":true,"result":"done"}
```

## 8. Firmware (Repo hoặc Crate riêng)

- **zeroclaw-firmware** hoặc **zeroclaw-peripheral** — một crate/workspace riêng biệt.
- Targets: `thumbv7em-none-eabihf` (STM32), `armv7-unknown-linux-gnueabihf` (RPi), v.v.
- Dùng `embassy` hoặc Zephyr cho STM32.
- Triển khai giao thức nêu trên.
- Người dùng nạp lên board; ZeroClaw kết nối và tự phát hiện khả năng.

## 9. Các giai đoạn triển khai

### Phase 1: Skeleton ✅ (Hoàn thành)

- [x] Thêm trait `Peripheral`, config schema, CLI (`zeroclaw peripheral list/add`)
- [x] Thêm flag `--peripheral` cho agent
- [x] Ghi tài liệu vào AGENTS.md

### Phase 2: Host-Mediated — Phát hiện phần cứng ✅ (Hoàn thành)

- [x] `zeroclaw hardware discover`: liệt kê thiết bị USB (VID/PID)
- [x] Board registry: ánh xạ VID/PID → kiến trúc, tên (ví dụ: Nucleo-F401RE)
- [x] `zeroclaw hardware introspect <path>`: memory map, danh sách peripheral

### Phase 3: Host-Mediated — Serial / J-Link

- [x] `SerialPeripheral` cho STM32 qua USB CDC
- [ ] Tích hợp probe-rs hoặc OpenOCD để nạp/gỡ lỗi firmware
- [x] Tools: `gpio_read`, `gpio_write` (memory_read, flash_write trong tương lai)

### Phase 4: Pipeline RAG ✅ (Hoàn thành)

- [x] Lập chỉ mục datasheet (markdown/text → các đoạn)
- [x] Truy xuất và chèn vào ngữ cảnh LLM cho các truy vấn liên quan phần cứng
- [x] Bổ sung prompt đặc thù theo board

**Cách dùng:** Thêm `datasheet_dir = "docs/datasheets"` vào `[peripherals]` trong config.toml. Đặt file `.md` hoặc `.txt` được đặt tên theo board (ví dụ: `nucleo-f401re.md`, `rpi-gpio.md`). Các file trong `_generic/` hoặc tên `generic.md` áp dụng cho mọi board. Các đoạn được truy xuất theo từ khóa và chèn vào ngữ cảnh tin nhắn người dùng.

### Phase 5: Edge-Native — RPi ✅ (Hoàn thành)

- [x] ZeroClaw trên Raspberry Pi (native GPIO qua rppal)
- [ ] Server gRPC/nanoRPC cho truy cập peripheral cục bộ
- [ ] Lưu trữ code (lưu các đoạn code đã tổng hợp)

### Phase 6: Edge-Native — ESP32

- [x] ESP32 qua Host-Mediated (serial transport) — cùng giao thức JSON như STM32
- [x] Crate firmware `zeroclaw-esp32` (`firmware/zeroclaw-esp32`) — GPIO qua UART
- [x] ESP32 trong hardware registry (CH340 VID/PID)
- [ ] ZeroClaw *chạy trực tiếp trên* ESP32 (WiFi + LLM, edge-native) — tương lai
- [ ] Thực thi Wasm hoặc dựa trên template cho logic do LLM tạo ra

**Cách dùng:** Nạp `firmware/zeroclaw-esp32` vào ESP32, thêm `board = "esp32"`, `transport = "serial"`, `path = "/dev/ttyUSB0"` vào config.

### Phase 7: Thực thi động (Code do LLM tạo ra)

- [ ] Thư viện template: các đoạn GPIO/I2C/SPI có tham số
- [ ] Tùy chọn: Wasm runtime cho logic do người dùng định nghĩa (sandboxed)
- [ ] Lưu và tái sử dụng các đường code tối ưu

## 10. Các khía cạnh bảo mật

- **Serial path:** Xác thực `path` nằm trong danh sách cho phép (ví dụ: `/dev/ttyACM*`, `/dev/ttyUSB*`); không bao giờ dùng đường dẫn tùy ý.
- **GPIO:** Giới hạn những pin nào được phép truy cập; tránh các pin nguồn/reset.
- **Không lưu bí mật trên peripheral:** Firmware không nên lưu API key; máy chủ xử lý xác thực.

## 11. Ngoài phạm vi (Hiện tại)

- Chạy ZeroClaw đầy đủ *trực tiếp trên* STM32 bare-metal (không có WiFi, RAM hạn chế) — dùng Host-Mediated thay thế
- Đảm bảo thời gian thực — peripheral hoạt động theo kiểu best-effort
- Thực thi code native tùy ý từ LLM — ưu tiên Wasm hoặc templates

## 12. Tài liệu liên quan

- [adding-boards-and-tools.md](./adding-boards-and-tools.md) — Cách thêm board và datasheet
- [network-deployment.md](network-deployment.md) — Triển khai RPi và mạng

## 13. Tham khảo

- [Zephyr RTOS Rust support](https://docs.zephyrproject.org/latest/develop/languages/rust/index.html)
- [Embassy](https://embassy.dev/) — async embedded framework
- [rppal](https://github.com/golemparts/rppal) — Raspberry Pi GPIO in Rust
- [STM32 Nucleo-F401RE](https://www.st.com/en/evaluation-tools/nucleo-f401re.html)
- [tonic](https://github.com/hyperium/tonic) — gRPC for Rust
- [probe-rs](https://probe.rs/) — ARM debug probe, flash, memory access
- [nusb](https://github.com/nic-hartley/nusb) — USB device enumeration (VID/PID)

## 14. Tóm tắt ý tưởng gốc

> *"Các board như ESP, Raspberry Pi, hoặc các board có WiFi có thể kết nối với LLM (Gemini hoặc mã nguồn mở). ZeroClaw chạy trên thiết bị, tạo gRPC riêng, khởi động nó, và giao tiếp với ngoại vi. Người dùng hỏi qua WhatsApp: 'di chuyển cánh tay X' hoặc 'bật LED'. ZeroClaw lấy tài liệu chính xác, viết code, thực thi, lưu trữ tối ưu, chạy, và bật LED — tất cả trên board phát triển.*
>
> *Với STM Nucleo kết nối qua USB/J-Link/Aardvark vào Mac: ZeroClaw từ Mac truy cập phần cứng, cài đặt hoặc ghi những gì cần thiết lên thiết bị, và trả kết quả. Ví dụ: 'Hey ZeroClaw, những địa chỉ khả dụng/đọc được trên thiết bị USB này là gì?' Nó có thể tự tìm ra thiết bị nào đang kết nối ở đâu và đưa ra gợi ý."*
