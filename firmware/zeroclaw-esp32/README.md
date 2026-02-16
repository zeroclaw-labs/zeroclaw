# ZeroClaw ESP32 Firmware

Peripheral firmware for ESP32 — speaks the same JSON-over-serial protocol as the STM32 firmware. Flash this to your ESP32, then configure ZeroClaw on the host to connect via serial.

## Protocol

- **Request** (host → ESP32): `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}\n`
- **Response** (ESP32 → host): `{"id":"1","ok":true,"result":"done"}\n`

Commands: `gpio_read`, `gpio_write`.

## Prerequisites

1. **ESP toolchain** (espup):
   ```sh
   cargo install espup espflash
   espup install
   source ~/export-esp.sh   # or ~/export-esp.fish for Fish
   ```

2. **Target**: ESP32-C3 (RISC-V) by default. Edit `.cargo/config.toml` for other targets (e.g. `xtensa-esp32-espidf` for original ESP32).

## Build & Flash

```sh
cd firmware/zeroclaw-esp32
cargo build --release
espflash flash target/riscv32imc-esp-espidf/release/zeroclaw-esp32 --monitor
```

## Host Config

Add to `config.toml`:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "esp32"
transport = "serial"
path = "/dev/ttyUSB0"   # or /dev/ttyACM0, COM3, etc.
baud = 115200
```

## Pin Mapping

Default GPIO 2 and 13 are configured for output. Edit `src/main.rs` to add more pins or change for your board. ESP32-C3 has different pin layout — adjust UART pins (gpio21/gpio20) if needed.

## Edge-Native (Future)

Phase 6 also envisions ZeroClaw running *on* the ESP32 (WiFi + LLM). This firmware is the host-mediated serial peripheral; edge-native will be a separate crate.
