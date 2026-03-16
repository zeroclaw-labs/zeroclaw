# ESP32 GPIO 参考

## 引脚别名

| 别名       | 引脚 |
|-------------|-----|
| builtin_led | 2   |
| red_led     | 2   |

## 常用引脚（ESP32 / ESP32-C3）

- **GPIO 2**：许多开发板上的内置 LED（输出）
- **GPIO 13**：通用输出
- **GPIO 21/20**：常用于 UART0 TX/RX（如果使用串口请避免占用）

## 协议

ZeroClaw 主机通过串口发送 JSON（波特率 115200）：
- `gpio_read`：`{"id":"1","cmd":"gpio_read","args":{"pin":13}}`
- `gpio_write`：`{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`

响应：`{"id":"1","ok":true,"result":"0"}` 或 `{"id":"1","ok":true,"result":"done"}`
