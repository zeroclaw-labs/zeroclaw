# Tham chiếu GPIO ESP32

## Pin Aliases

| alias       | pin |
|-------------|-----|
| builtin_led | 2   |
| red_led     | 2   |

## Các pin thông dụng (ESP32 / ESP32-C3)

- **GPIO 2**: LED tích hợp trên nhiều dev board (output)
- **GPIO 13**: Đầu ra mục đích chung
- **GPIO 21/20**: Thường dùng cho UART0 TX/RX (tránh nếu đang dùng serial)

## Giao thức

ZeroClaw host gửi JSON qua serial (115200 baud):
- `gpio_read`: `{"id":"1","cmd":"gpio_read","args":{"pin":13}}`
- `gpio_write`: `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`

Response: `{"id":"1","ok":true,"result":"0"}` hoặc `{"id":"1","ok":true,"result":"done"}`
