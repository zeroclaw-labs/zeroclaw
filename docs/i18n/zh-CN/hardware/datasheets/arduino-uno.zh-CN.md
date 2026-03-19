# Arduino Uno

## 引脚别名

| 别名        | 引脚 |
| ----------- | ---- |
| red_led     | 13   |
| builtin_led | 13   |
| user_led    | 13   |

## 概述

Arduino Uno 是基于 ATmega328P 的微控制器开发板。它有 14 个数字 I/O 引脚（0–13）和 6 个模拟输入（A0–A5）。

## 数字引脚

- **引脚 0–13：** 数字 I/O。可设置为 INPUT 或 OUTPUT。
- **引脚 13：** 板载内置 LED。可将 LED 连接到 GND 或用作输出。
- **引脚 0–1：** 也用于串口（RX/TX）。如果使用串口请避免占用。

## GPIO

- 输出使用 `digitalWrite(pin, HIGH)` 或 `digitalWrite(pin, LOW)`。
- 输入使用 `digitalRead(pin)`（返回 0 或 1）。
- JhedaiClaw 协议中的引脚编号：0–13。

## 串口

- UART 位于引脚 0（RX）和 1（TX）。
- 通过 ATmega16U2 或 CH340（克隆板）实现 USB 连接。
- JhedaiClaw 固件使用的波特率：115200。

## JhedaiClaw 工具

- `gpio_read`：读取引脚值（0 或 1）。
- `gpio_write`：设置引脚为高电平（1）或低电平（0）。
- `arduino_upload`：代理生成完整的 Arduino 草图代码；JhedaiClaw 通过 arduino-cli 编译并上传。用于"制作心形"、自定义图案等场景 —— 代理编写代码，无需手动编辑。引脚 13 = 内置 LED。
