## Aardvark Adapter (aardvark0)

- Protocol: I2C and SPI via Total Phase Aardvark USB
- Bitrate: 100 kHz (standard-mode I2C) by default
- Use `i2c_scan` first to discover connected devices
- Use `i2c_read` / `i2c_write` for register operations
- Use `spi_transfer` for full-duplex SPI
- Use `gpio_aardvark` to control the Aardvark's GPIO expansion pins
- Use `datasheet` tool when user identifies a new device

## Tool Selection — Aardvark

| Goal                           | Tool            |
|--------------------------------|-----------------|
| Find devices on the I2C bus    | `i2c_scan`      |
| Read a register                | `i2c_read`      |
| Write a register               | `i2c_write`     |
| Full-duplex SPI transfer       | `spi_transfer`  |
| Control Aardvark GPIO pins     | `gpio_aardvark` |
| User names a new device        | `datasheet`     |

## I2C Workflow

1. Run `i2c_scan` — find what addresses respond.
2. User identifies the device (or look up the address in the skill file).
3. Read the relevant register with `i2c_read`.
4. If datasheet is not yet cached, use `datasheet(action="search", device_name="...")`.

## Notes

- Aardvark has no firmware — it calls the C library directly.
  Do NOT use `device_exec`, `device_read_code`, or `device_write_code` for Aardvark.
- The Aardvark adapter auto-enables I2C pull-ups (3.3 V) — no external resistors needed
  for most sensors.
