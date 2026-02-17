# ZeroClaw ESP32 UI Firmware

Slint-based graphical UI firmware scaffold for ZeroClaw edge scenarios on ESP32.

## Scope of This Crate

This crate intentionally provides a **minimal, bootable UI scaffold**:

- Initializes ESP-IDF logging/runtime patches
- Compiles and runs a small Slint UI (`MainWindow`)
- Keeps display and touch feature flags available for incremental driver integration

What this crate **does not** do yet:

- No full chat runtime integration
- No production display/touch driver wiring in `src/main.rs`
- No Wi-Fi/BLE transport logic

## Features

- **Slint UI scaffold** suitable for MCU-oriented iteration
- **Display feature flags** for ST7789, ILI9341, SSD1306
- **Touch feature flags** for XPT2046 and FT6X36 integration planning
- **ESP-IDF baseline** for embedded target builds

## Project Structure

```text
firmware/zeroclaw-esp32-ui/
├── Cargo.toml          # Rust package and feature flags
├── build.rs            # Slint compilation hook
├── .cargo/
│   └── config.toml     # Cross-compilation defaults
├── ui/
│   └── main.slint      # Slint UI definition
└── src/
    └── main.rs         # Firmware entry point
```

## Prerequisites

1. **ESP Rust toolchain**
   ```bash
   cargo install espup
   espup install
   source ~/export-esp.sh
   ```

2. **Flashing tools**
   ```bash
   cargo install espflash cargo-espflash
   ```

## Build and Flash

### Default target (ESP32-C3, from `.cargo/config.toml`)

```bash
cd firmware/zeroclaw-esp32-ui
cargo build --release
cargo espflash flash --release --monitor
```

### Build for ESP32-S3 (override target)

```bash
cargo build --release --target xtensa-esp32s3-espidf
```

## Feature Flags

```bash
# Switch display profile
cargo build --release --features display-ili9341

# Enable planned touch profile
cargo build --release --features touch-ft6x36
```

## UI Layout

The current `ui/main.slint` defines:

- `StatusBar`
- `MessageList`
- `InputBar`
- `MainWindow`

These components are placeholders to keep future hardware integration incremental and low-risk.

## Next Integration Steps

1. Wire real display driver initialization in `src/main.rs`
2. Attach touch input events to Slint callbacks
3. Connect UI state with ZeroClaw edge/runtime messaging
4. Add board-specific pin maps with explicit target profiles

## License

MIT - See root `LICENSE`

## References

- [Slint ESP32 Documentation](https://slint.dev/esp32)
- [ESP-IDF Rust Book](https://esp-rs.github.io/book/)
- [ZeroClaw Hardware Design](../../docs/hardware-peripherals-design.md)
