# Arduino Uno

## 腳位別名

| 別名        | 腳位 |
|-------------|------|
| red_led     | 13   |
| builtin_led | 13   |
| user_led    | 13   |

## 概述

Arduino Uno 是一款基於 ATmega328P 的微控制器開發板。具有 14 個數位 I/O 腳位（0-13）和 6 個類比輸入（A0-A5）。

## 數位腳位

- **腳位 0-13：** 數位 I/O，可設定為 INPUT 或 OUTPUT。
- **腳位 13：** 內建 LED（板載），可連接 LED 至 GND 或作為輸出使用。
- **腳位 0-1：** 同時用於序列通訊（RX/TX），使用 Serial 時請避免佔用。

## GPIO

- 使用 `digitalWrite(pin, HIGH)` 或 `digitalWrite(pin, LOW)` 進行輸出。
- 使用 `digitalRead(pin)` 進行輸入（回傳 0 或 1）。
- ZeroClaw 協定中的腳位編號：0-13。

## 序列通訊

- UART 位於腳位 0（RX）和 1（TX）。
- 透過 ATmega16U2 或 CH340（相容版）進行 USB 通訊。
- 鮑率：ZeroClaw 韌體使用 115200。

## ZeroClaw 工具

- `gpio_read`：讀取腳位值（0 或 1）。
- `gpio_write`：設定腳位為高電位（1）或低電位（0）。
- `arduino_upload`：代理程式產生完整的 Arduino 草稿碼；ZeroClaw 透過 arduino-cli 編譯並上傳。適用於「製作愛心圖案」、自訂模式等場景——由代理程式撰寫程式碼，無需手動編輯。腳位 13 = 內建 LED。
