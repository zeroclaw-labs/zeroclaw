# ESP32 Node Firmware

Minimal ESP32 firmware skeleton with WiFi and MQTT support, designed for QEMU testing.

## Prerequisites

- PlatformIO Core
- QEMU for ESP32 (optional, for emulation)

## Build

```bash
platformio run -d firmware/esp32-node
```

## QEMU Testing

Install QEMU for ESP32:

```bash
python $IDF_PATH/tools/idf_tools.py install qemu-xtensa
```

Run in QEMU:

```bash
./scripts/test-qemu.sh
```

## Configuration

Edit `src/main.cpp` to set:
- WiFi SSID/password
- MQTT broker address

## Hardware Deployment

Flash to physical ESP32:

```bash
platformio run -t upload -d firmware/esp32-node
```
