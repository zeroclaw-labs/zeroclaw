# ESP32 GPIO 參考

## 腳位別名

| 別名        | 腳位 |
|-------------|------|
| builtin_led | 2    |
| red_led     | 2    |

## 常用腳位（ESP32 / ESP32-C3）

- **GPIO 2**：許多開發板上的內建 LED（輸出）
- **GPIO 13**：通用輸出
- **GPIO 21/20**：常用於 UART0 TX/RX（使用序列通訊時請避免佔用）

## 協定

ZeroClaw 主機透過序列埠傳送 JSON（鮑率 115200）：
- `gpio_read`：`{"id":"1","cmd":"gpio_read","args":{"pin":13}}`
- `gpio_write`：`{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`

回應：`{"id":"1","ok":true,"result":"0"}` 或 `{"id":"1","ok":true,"result":"done"}`
