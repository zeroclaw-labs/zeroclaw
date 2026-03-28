# Skill: I2C Operations via Aardvark

<!-- Copy to ~/.zeroclaw/hardware/skills/i2c.md -->

## Always scan first

If the I2C address is unknown, run `i2c_scan` before anything else.

## Common device addresses

| Address range | Typical devices                               |
|---------------|-----------------------------------------------|
| 0x08–0x0F     | Reserved / rare                               |
| 0x40–0x4F     | LM75, TMP102, HTU21D (temp/humidity)          |
| 0x48–0x4F     | LM75, DS1621, ADS1115 (ADC)                   |
| 0x50–0x57     | AT24Cxx EEPROM                                |
| 0x68–0x6F     | MPU6050 IMU, DS1307 / DS3231 RTC              |
| 0x76–0x77     | BME280, BMP280 (pressure + humidity)          |
| 0x42          | Common PSoC6 default                          |
| 0x3C, 0x3D    | SSD1306 OLED display                          |

## Reading a register

```text
i2c_read(addr=0x48, register=0x00, len=2)
```

## Writing a register

```text
i2c_write(addr=0x48, bytes=[0x01, 0x60])
```

## Write-then-read (register pointer pattern)

Some devices require you to first write the register address, then read separately:

```text
i2c_write(addr=0x48, bytes=[0x00])
i2c_read(addr=0x48, len=2)
```

The `i2c_read` tool handles this automatically when you specify `register=`.

## Temperature conversion — LM75 / TMP102

Raw bytes from register 0x00 are big-endian, 9-bit or 11-bit:

```
raw = (byte[0] << 1) | (byte[1] >> 7)   # for LM75 (9-bit)
if raw >= 256: raw -= 512                # handle negative (two's complement)
temp_c = raw * 0.5
```

## Decision table — Aardvark vs Pico tools

| Scenario                                       | Use           |
|------------------------------------------------|---------------|
| Talking to an I2C sensor via Aardvark          | `i2c_read`    |
| Configuring a sensor register                  | `i2c_write`   |
| Discovering what's on the bus                  | `i2c_scan`    |
| Running MicroPython on the connected Pico      | `device_exec` |
| Blinking Pico LED                              | `device_exec` |
