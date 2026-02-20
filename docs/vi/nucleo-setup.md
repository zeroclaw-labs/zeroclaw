# ZeroClaw trên Nucleo-F401RE — Hướng dẫn từng bước

Chạy ZeroClaw trên Mac hoặc Linux. Kết nối Nucleo-F401RE qua USB. Điều khiển GPIO (LED, các pin) qua Telegram hoặc CLI.

---

## Lấy thông tin board qua Telegram (Không cần nạp firmware)

ZeroClaw có thể đọc thông tin chip từ Nucleo qua USB **mà không cần nạp firmware nào**. Nhắn tin cho Telegram bot của bạn:

- *"What board info do I have?"*
- *"Board info"*
- *"What hardware is connected?"*
- *"Chip info"*

Agent dùng tool `hardware_board_info` để trả về tên chip, kiến trúc và memory map. Với feature `probe`, nó đọc dữ liệu trực tiếp qua USB/SWD; nếu không, nó trả về thông tin tĩnh từ datasheet.

**Cấu hình:** Thêm Nucleo vào `config.toml` trước (để agent biết board nào cần truy vấn):

```toml
[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200
```

**Thay thế bằng CLI:**

```bash
cargo build --features hardware,probe
zeroclaw hardware info
zeroclaw hardware discover
```

---

## Những gì đã có sẵn (Không cần thay đổi code)

ZeroClaw bao gồm mọi thứ cần thiết cho Nucleo-F401RE:

| Thành phần | Vị trí | Mục đích |
|------------|--------|---------|
| Firmware | `firmware/zeroclaw-nucleo/` | Embassy Rust — USART2 (115200), gpio_read, gpio_write |
| Serial peripheral | `src/peripherals/serial.rs` | Giao thức JSON-over-serial (giống Arduino/ESP32) |
| Flash command | `zeroclaw peripheral flash-nucleo` | Build firmware, nạp qua probe-rs |

Giao thức: JSON phân tách bằng dòng mới. Request: `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`. Response: `{"id":"1","ok":true,"result":"done"}`.

---

## Yêu cầu trước khi bắt đầu

- Board Nucleo-F401RE
- Cáp USB (USB-A sang Mini-USB; Nucleo có ST-Link tích hợp sẵn)
- Để nạp firmware: `cargo install probe-rs-tools --locked` (hoặc dùng [install script](https://probe.rs/docs/getting-started/installation/))

---

## Phase 1: Nạp Firmware

### 1.1 Kết nối Nucleo

1. Kết nối Nucleo với Mac/Linux qua USB.
2. Board xuất hiện như thiết bị USB (ST-Link). Không cần driver riêng trên các hệ thống hiện đại.

### 1.2 Nạp qua ZeroClaw

Từ thư mục gốc của repo zeroclaw:

```bash
zeroclaw peripheral flash-nucleo
```

Lệnh này build `firmware/zeroclaw-nucleo` và chạy `probe-rs run --chip STM32F401RETx`. Firmware chạy ngay sau khi nạp xong.

### 1.3 Nạp thủ công (Phương án thay thế)

```bash
cd firmware/zeroclaw-nucleo
cargo build --release --target thumbv7em-none-eabihf
probe-rs run --chip STM32F401RETx target/thumbv7em-none-eabihf/release/zeroclaw-nucleo
```

---

## Phase 2: Tìm Serial Port

- **macOS:** `/dev/cu.usbmodem*` hoặc `/dev/tty.usbmodem*` (ví dụ: `/dev/cu.usbmodem101`)
- **Linux:** `/dev/ttyACM0` (hoặc kiểm tra `dmesg` sau khi cắm vào)

USART2 (PA2/PA3) được bridge sang cổng COM ảo của ST-Link, vì vậy máy chủ thấy một thiết bị serial duy nhất.

---

## Phase 3: Cấu hình ZeroClaw

Thêm vào `~/.zeroclaw/config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/cu.usbmodem101"   # điều chỉnh theo port của bạn
baud = 115200
```

---

## Phase 4: Chạy và Kiểm thử

```bash
zeroclaw daemon --host 127.0.0.1 --port 3000
```

Hoặc dùng agent trực tiếp:

```bash
zeroclaw agent --message "Turn on the LED on pin 13"
```

Pin 13 = PA5 = User LED (LD2) trên Nucleo-F401RE.

---

## Tóm tắt: Các lệnh

| Bước | Lệnh |
|------|------|
| 1 | Kết nối Nucleo qua USB |
| 2 | `cargo install probe-rs-tools --locked` |
| 3 | `zeroclaw peripheral flash-nucleo` |
| 4 | Thêm Nucleo vào config.toml (path = serial port của bạn) |
| 5 | `zeroclaw daemon` hoặc `zeroclaw agent -m "Turn on LED"` |

---

## Xử lý sự cố

- **flash-nucleo không nhận ra** — Build từ repo: `cargo run --features hardware -- peripheral flash-nucleo`. Subcommand này chỉ có trong repo build, không có trong cài đặt từ crates.io.
- **Không tìm thấy probe-rs** — `cargo install probe-rs-tools --locked` (crate `probe-rs` là thư viện; CLI nằm trong `probe-rs-tools`)
- **Không phát hiện được probe** — Đảm bảo Nucleo đã kết nối. Thử cáp/cổng USB khác.
- **Không tìm thấy serial port** — Trên Linux, thêm user vào nhóm `dialout`: `sudo usermod -a -G dialout $USER`, rồi đăng xuất/đăng nhập lại.
- **Lệnh GPIO bị bỏ qua** — Kiểm tra `path` trong config có khớp với serial port của bạn. Chạy `zeroclaw peripheral list` để xác nhận.
