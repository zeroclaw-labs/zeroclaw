# ESP32 GPIO Reference

## Pin Aliases

| alias       | pin |
|-------------|-----|
| builtin_led | 2   |
| red_led     | 2   |

## Common pins (ESP32 / ESP32-C3)

- **GPIO 2**: Built-in LED on many dev boards (output)
- **GPIO 13**: General-purpose output
- **GPIO 21/20**: Often used for UART0 TX/RX (avoid if using serial)

## Protocol

ZeroClaw host sends JSON over serial (115200 baud):
- `gpio_read`: `{"id":"1","cmd":"gpio_read","args":{"pin":13}}`
- `gpio_write`: `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`

Response: `{"id":"1","ok":true,"result":"0"}` or `{"id":"1","ok":true,"result":"done"}`
