# Arduino Uno

## Pin Aliases

| alias       | pin |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 13  |

## Tổng quan

Arduino Uno là board vi điều khiển dựa trên ATmega328P. Có 14 pin digital I/O (0–13) và 6 đầu vào analog (A0–A5).

## Pin Digital

- **Pins 0–13:** Digital I/O. Có thể là INPUT hoặc OUTPUT.
- **Pin 13:** LED tích hợp (onboard). Kết nối LED với GND hoặc dùng để xuất tín hiệu.
- **Pins 0–1:** Cũng dùng cho Serial (RX/TX). Tránh dùng nếu đang sử dụng Serial.

## GPIO

- `digitalWrite(pin, HIGH)` hoặc `digitalWrite(pin, LOW)` để xuất tín hiệu.
- `digitalRead(pin)` để đọc đầu vào (trả về 0 hoặc 1).
- Số pin trong giao thức ZeroClaw: 0–13.

## Serial

- UART trên pin 0 (RX) và 1 (TX).
- USB qua ATmega16U2 hoặc CH340 (bản clone).
- Baud rate: 115200 cho firmware ZeroClaw.

## ZeroClaw Tools

- `gpio_read`: Đọc giá trị pin (0 hoặc 1).
- `gpio_write`: Đặt pin lên cao (1) hoặc xuống thấp (0).
- `arduino_upload`: Agent tạo code Arduino sketch đầy đủ; ZeroClaw biên dịch và tải lên qua arduino-cli. Dùng cho "make a heart", các pattern tùy chỉnh — agent viết code, không cần chỉnh sửa thủ công. Pin 13 = LED tích hợp.
