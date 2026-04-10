# Arduino Uno

## 핀 별칭

| 별칭       | 핀 |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 13  |

## 개요

Arduino Uno는 ATmega328P 기반의 마이크로컨트롤러 board입니다. 14개의 디지털 I/O 핀(0~13)과 6개의 아날로그 입력(A0~A5)을 갖추고 있습니다.

## 디지털 핀

- **핀 0~13:** 디지털 I/O. INPUT 또는 OUTPUT으로 설정할 수 있습니다.
- **핀 13:** 내장 LED (board 위). GND에 LED를 연결하거나 출력으로 사용합니다.
- **핀 0~1:** Serial (RX/TX)로도 사용됩니다. Serial을 사용하는 경우 피하십시오.

## GPIO

- 출력에는 `digitalWrite(pin, HIGH)` 또는 `digitalWrite(pin, LOW)`를 사용합니다.
- 입력에는 `digitalRead(pin)`을 사용합니다 (0 또는 1을 반환).
- ZeroClaw 프로토콜에서의 핀 번호: 0~13.

## Serial

- 핀 0 (RX)과 1 (TX)의 UART.
- ATmega16U2 또는 CH340 (클론)을 통한 USB.
- ZeroClaw firmware의 Baud rate: 115200.

## ZeroClaw 도구

- `gpio_read`: 핀 값을 읽습니다 (0 또는 1).
- `gpio_write`: 핀을 high (1) 또는 low (0)로 설정합니다.
- `arduino_upload`: 에이전트가 전체 Arduino 스케치 코드를 생성하고, ZeroClaw가 arduino-cli를 통해 컴파일 및 업로드합니다. "하트 만들기", 커스텀 패턴 등에 사용됩니다 — 에이전트가 코드를 작성하므로 수동 편집이 필요하지 않습니다. 핀 13 = 내장 LED.
