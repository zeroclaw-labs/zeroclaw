# ESP32 GPIO 레퍼런스

## 핀 별칭

| 별칭       | 핀 |
|-------------|-----|
| builtin_led | 2   |
| red_led     | 2   |

## 주요 핀 (ESP32 / ESP32-C3)

- **GPIO 2**: 많은 개발 board에서 내장 LED (출력)
- **GPIO 13**: 범용 출력
- **GPIO 21/20**: UART0 TX/RX로 자주 사용됨 (시리얼 사용 시 피하십시오)

## 프로토콜

ZeroClaw 호스트가 시리얼(115200 baud)을 통해 JSON을 전송합니다:
- `gpio_read`: `{"id":"1","cmd":"gpio_read","args":{"pin":13}}`
- `gpio_write`: `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`

응답: `{"id":"1","ok":true,"result":"0"}` 또는 `{"id":"1","ok":true,"result":"done"}`
