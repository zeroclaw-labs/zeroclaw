# Hardware — Overview

ZeroClaw's hardware subsystem lets the agent control microcontrollers, SBCs, and peripherals directly. Enable with `--features hardware`.

## What's supported

| Target | Protocol | Page |
|---|---|---|
| STM32 Nucleo (F401RE, others) | Serial / OpenOCD | [STM32 Nucleo](./nucleo-setup.md) |
| Arduino Uno Q | Serial / USB | [Arduino Uno Q](./arduino-uno-q-setup.md) |
| Raspberry Pi | GPIO / I2C / SPI (via `/dev/gpiochip*`, `/dev/i2c-*`, `/dev/spidev*`) | Covered by peripherals design |
| Aardvark I2C/SPI host adapter | USB | [Aardvark](./aardvark.md) |
| Android (via Termux) | Serial-over-USB / Bluetooth | [Android](./android-setup.md) |
| Generic boards | `Peripheral` trait | [Adding boards & tools](./adding-boards-and-tools.md) |

See [Peripherals design](./hardware-peripherals-design.md) for the architecture.

## Enabling

At compile time:

```bash
cargo build --release --features hardware
```

Or, if you want only specific boards:

```bash
cargo build --release --features "hardware board-nucleo board-arduino"
```

## Runtime tools

With the feature enabled, the agent gains these tools:

- `gpio_read` / `gpio_write` — digital I/O
- `i2c_read` / `i2c_write` — I2C bus access
- `spi_transfer` — SPI transfers
- `adc_read` — analogue reads (where supported)
- `peripheral_probe` — discover attached boards and sensors
- `peripheral_flash` — flash firmware to a connected microcontroller

All tool invocations go through the same [security policy](../security/overview.md) as any other tool. Restrict device access via:

```toml
[security.sandbox]
allow_devices = ["/dev/gpiochip0", "/dev/i2c-1", "/dev/ttyUSB0"]
```

## Running on a Raspberry Pi

The most common hardware target. A minimal setup:

```bash
# install
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash

# add yourself to hardware groups (re-login after)
sudo usermod -aG gpio,spi,i2c $USER

# install as user service (ensures hardware group membership is inherited)
zeroclaw service install
```

The stock systemd unit sets `SupplementaryGroups=gpio spi i2c`.

## Safety

Hardware tools can brick things. Real, expensive things.

- `peripheral_flash` writes firmware — a bad image can brick the board. The tool requires operator approval at `Supervised` autonomy regardless of autonomy level; there's no way to auto-approve it.
- `i2c_write` / `spi_transfer` to device addresses the agent doesn't know can damage sensors.
- GPIO writes that conflict with external drivers (voltage fights) damage pins.

For production deployments with untrusted channels exposed, disable hardware tools per channel:

```toml
[channels.public-discord]
tools_deny = ["gpio_write", "i2c_write", "spi_transfer", "peripheral_flash"]
```

## Datasheets

Per-board pin maps and electrical characteristics:

- STM32 Nucleo-F401RE: <https://www.st.com/en/evaluation-tools/nucleo-f401re.html>
- Arduino Uno Q: <https://docs.arduino.cc/hardware/uno-q>
- Raspberry Pi GPIO: <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#gpio>
- ESP32: <https://www.espressif.com/sites/default/files/documentation/esp32_datasheet_en.pdf>

## Adding new hardware

See [Adding boards & tools](./adding-boards-and-tools.md) for the step-by-step. TL;DR: implement the `Peripheral` trait from `crates/zeroclaw-hardware/src/`, add a board-specific feature flag, write a probe routine that identifies the board from USB descriptors or serial handshake.

## See also

- [Peripherals design](./hardware-peripherals-design.md) — the architecture
- [Adding boards & tools](./adding-boards-and-tools.md) — implementation guide
- [Aardvark](./aardvark.md) — USB I2C/SPI host adapter setup
