# ZeroClaw ESP32 UI Firmware

Slint-based graphical interface for ZeroClaw AI assistant on ESP32.

## Features

- **Modern UI**: Declarative interface built with Slint UI framework
- **Touch Support**: Compatible with resistive (XPT2046) and capacitive (FT6X36) touch panels
- **Display Options**: Support for ST7789, ILI9341, and SSD1306 displays
- **Connectivity**: WiFi and Bluetooth Low Energy support
- **Memory Efficient**: Optimized for ESP32's limited RAM (~520KB)

## Hardware Requirements

### Recommended: ESP32-S3
- **SoC**: ESP32-S3 (Xtensa LX7 dual-core, 240MHz)
- **RAM**: 512KB SRAM + 8MB PSRAM (optional but recommended)
- **Display**: 2.8" 320x240 TFT LCD (ST7789 or ILI9341)
- **Touch**: XPT2046 resistive or FT6X36 capacitive
- **Storage**: 4MB+ Flash

### Alternative: ESP32-C3
- **SoC**: ESP32-C3 (RISC-V single-core, 160MHz)
- **RAM**: 400KB SRAM
- **Display**: 1.14" 135x240 TFT (ST7789)
- **Note**: Limited to simpler UI due to RAM constraints

## Project Structure

```
firmware/zeroclaw-esp32-ui/
├── Cargo.toml          # Rust dependencies
├── build.rs            # Build script for Slint compilation
├── .cargo/
│   └── config.toml     # Cross-compilation settings
├── ui/
│   └── main.slint      # Slint UI definition
└── src/
    └── main.rs         # Application entry point
```

## Prerequisites

1. **Rust toolchain with ESP32 support**:
   ```bash
   cargo install espup
   espup install
   source ~/export-esp.sh
   ```

2. **Additional tools**:
   ```bash
   cargo install espflash cargo-espflash
   ```

3. **Hardware setup**:
   - Connect display to SPI pins (see pin configuration below)
   - Ensure proper power supply (3.3V logic level)

## Pin Configuration

Default pin mapping for ESP32-S3 with ST7789 display and FT6X36 capacitive touch:

### Display (SPI)

| Function | GPIO Pin | Description |
|----------|---------|-------------|
| SPI SCK  | GPIO 6   | SPI Clock |
| SPI MOSI | GPIO 7   | SPI Data Out |
| SPI MISO | GPIO 8   | SPI Data In (optional) |
| SPI CS   | GPIO 10  | Chip Select |
| DC       | GPIO 4   | Data/Command |
| RST      | GPIO 3   | Reset |
| Backlight| GPIO 5   | Display backlight |

### Touch Controller (I2C)

| Function | GPIO Pin | Description |
|----------|---------|-------------|
| I2C SDA | GPIO 1   | I2C Data |
| I2C SCL | GPIO 2   | I2C Clock |
| INT     | GPIO 11  | Touch interrupt |

### Hardware Connections

```
ESP32-S3              ST7789 Display        FT6X36 Touch
-----------           ---------------        -------------
GPIO 6  ──────────►  SCK
GPIO 7  ──────────►  MOSI
GPIO 10 ──────────►  CS
GPIO 4  ──────────►  DC
GPIO 3  ──────────►  RST
GPIO 5  ──────────►  BACKLIGHT (via resistor)

GPIO 1  ──────────►  SDA
GPIO 2  ──────────►  SCL
GPIO 11 ◄──────────  INT
```

**Note**: Use 3.3V for power. ST7789 typically requires 3.3V logic level.

## Building

### Standard build for ESP32-S3:
```bash
cd firmware/zeroclaw-esp32-ui
cargo build --release
```

### Flash to device:
```bash
cargo espflash flash --release --monitor
```

### Build for ESP32-C3 (RISC-V):
```bash
rustup target add riscv32imc-esp-espidf
cargo build --release --target riscv32imc-esp-espidf
```

### Feature flags:
```bash
# Use ILI9341 display instead of ST7789
cargo build --release --features display-ili9341

# Enable WiFi support
cargo build --release --features wifi

# Enable touch support
cargo build --release --features touch-xpt2046
```

## UI Design

The interface is defined in `ui/main.slint` with the following components:

- **StatusBar**: Shows connection status and app title
- **MessageList**: Displays conversation history
- **InputBar**: Text input with send button
- **MainWindow**: Root container with vertical layout

### Customizing the UI

Edit `ui/main.slint` and rebuild:
```bash
cargo build --release
```

The build script automatically compiles Slint files.

## Memory Optimization

For ESP32 (non-S3) with limited RAM:

1. Reduce display buffer size in `main.rs`:
   ```rust
   const DISPLAY_WIDTH: usize = 240;
   const DISPLAY_HEIGHT: usize = 135;
   ```

2. Use smaller font sizes in Slint UI

3. Enable release optimizations (already in Cargo.toml):
   - `opt-level = "s"` (optimize for size)
   - `lto = true` (link-time optimization)

## Troubleshooting

### Display shows garbage
- Check SPI connections and pin mapping
- Verify display orientation in `Builder::with_orientation()`
- Try different baud rates (26MHz is default)

### Out of memory
- Reduce Slint window size
- Disable unused features
- Consider ESP32-S3 with PSRAM

### Touch not working
- Verify touch controller is properly wired
- Check I2C/SPI address configuration
- Ensure interrupt pin is correctly connected

## License

MIT - See root LICENSE file

## References

- [Slint ESP32 Documentation](https://slint.dev/esp32)
- [ESP-IDF Rust Book](https://esp-rs.github.io/book/)
- [ZeroClaw Hardware Design](../docs/hardware-peripherals-design.md)
