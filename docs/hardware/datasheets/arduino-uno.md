# Arduino Uno

## Pin Aliases

| alias       | pin |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 13  |

## Overview

Arduino Uno is a microcontroller board based on the ATmega328P. It has 14 digital I/O pins (0–13) and 6 analog inputs (A0–A5).

## Digital Pins

- **Pins 0–13:** Digital I/O. Can be INPUT or OUTPUT.
- **Pin 13:** Built-in LED (onboard). Connect LED to GND or use for output.
- **Pins 0–1:** Also used for Serial (RX/TX). Avoid if using Serial.

## GPIO

- `digitalWrite(pin, HIGH)` or `digitalWrite(pin, LOW)` for output.
- `digitalRead(pin)` for input (returns 0 or 1).
- Pin numbers in ZeroClaw protocol: 0–13.

## Serial

- UART on pins 0 (RX) and 1 (TX).
- USB via ATmega16U2 or CH340 (clones).
- Baud rate: 115200 for ZeroClaw firmware.

## ZeroClaw Tools

- `gpio_read`: Read pin value (0 or 1).
- `gpio_write`: Set pin high (1) or low (0).
- `arduino_upload`: Agent generates full Arduino sketch code; ZeroClaw compiles and uploads it via arduino-cli. Use for "make a heart", custom patterns — agent writes the code, no manual editing. Pin 13 = built-in LED.
