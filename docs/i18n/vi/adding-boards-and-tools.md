# Thêm Board và Tool — Hướng dẫn phần cứng ZeroClaw

Hướng dẫn này giải thích cách thêm board phần cứng mới và tool tùy chỉnh vào ZeroClaw.

## Bắt đầu nhanh: Thêm board qua CLI

```bash
# Thêm board (cập nhật ~/.zeroclaw/config.toml)
zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
zeroclaw peripheral add arduino-uno /dev/cu.usbmodem12345
zeroclaw peripheral add rpi-gpio native   # cho Raspberry Pi GPIO (Linux)

# Khởi động lại daemon để áp dụng
zeroclaw daemon --host 127.0.0.1 --port 3000
```

## Các board được hỗ trợ

| Board | Transport | Ví dụ đường dẫn |
|-------|-----------|-----------------|
| nucleo-f401re | serial | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno | serial | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno-q | bridge | (IP của Uno Q) |
| rpi-gpio | native | native |
| esp32 | serial | /dev/ttyUSB0 |

## Cấu hình thủ công

Chỉnh sửa `~/.zeroclaw/config.toml`:

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets" # tùy chọn: RAG cho "turn on red led" → pin 13

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

## Thêm Datasheet (RAG)

Đặt file `.md` hoặc `.txt` vào `docs/datasheets/` (hoặc `datasheet_dir` của bạn). Đặt tên file theo board: `nucleo-f401re.md`, `arduino-uno.md`.

### Pin Aliases (Khuyến nghị)

Thêm mục `## Pin Aliases` để agent có thể ánh xạ "red led" → pin 13:

```markdown
# My Board

## Pin Aliases

| alias       | pin |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 5   |
```

Hoặc dùng định dạng key-value:

```markdown
## Pin Aliases
red_led: 13
builtin_led: 13
```

### PDF Datasheets

Với feature `rag-pdf`, ZeroClaw có thể lập chỉ mục file PDF:

```bash
cargo build --features hardware,rag-pdf
```

Đặt file PDF vào thư mục datasheet. Chúng sẽ được trích xuất và chia nhỏ thành các đoạn cho RAG.

## Thêm loại board mới

1. **Tạo datasheet** — `docs/datasheets/my-board.md` với pin aliases và thông tin GPIO.
2. **Thêm vào config** — `zeroclaw peripheral add my-board /dev/ttyUSB0`
3. **Triển khai peripheral** (tùy chọn) — Với giao thức tùy chỉnh, hãy implement trait `Peripheral` trong `src/peripherals/` và đăng ký trong `create_peripheral_tools`.

Xem `docs/hardware-peripherals-design.md` để hiểu toàn bộ thiết kế.

## Thêm Tool tùy chỉnh

1. Implement trait `Tool` trong `src/tools/`.
2. Đăng ký trong `create_peripheral_tools` (với hardware tool) hoặc tool registry của agent.
3. Thêm mô tả tool vào `tool_descs` của agent trong `src/agent/loop_.rs`.

## Tham chiếu CLI

| Lệnh | Mô tả |
|------|-------|
| `zeroclaw peripheral list` | Liệt kê các board đã cấu hình |
| `zeroclaw peripheral add <board> <path>` | Thêm board (ghi vào config) |
| `zeroclaw peripheral flash` | Nạp firmware Arduino |
| `zeroclaw peripheral flash-nucleo` | Nạp firmware Nucleo |
| `zeroclaw hardware discover` | Liệt kê thiết bị USB |
| `zeroclaw hardware info` | Thông tin chip qua probe-rs |

## Xử lý sự cố

- **Không tìm thấy serial port** — Trên macOS dùng `/dev/cu.usbmodem*`; trên Linux dùng `/dev/ttyACM0` hoặc `/dev/ttyUSB0`.
- **Build với hardware** — `cargo build --features hardware`
- **probe-rs cho Nucleo** — `cargo build --features hardware,probe`
