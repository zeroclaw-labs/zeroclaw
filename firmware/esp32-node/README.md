# ESP32 Node Firmware

Minimal ESP32 firmware skeleton with WiFi and MQTT support, designed for QEMU testing and physical hardware deployment.

## Prerequisites

- PlatformIO Core
- QEMU for ESP32 (optional, for emulation)
- ESP32 development board (for physical deployment)
- USB cable for flashing

## Build

```bash
platformio run -d firmware/esp32-node
```

## Configuration

### WiFi Settings

Edit `src/main.cpp` and update:

```cpp
const char* ssid = "YOUR_WIFI_SSID";
const char* password = "YOUR_WIFI_PASSWORD";
```

### MQTT Broker

Configure MQTT broker address in `src/main.cpp`:

```cpp
const char* mqtt_server = "192.168.1.100";  // Your broker IP
const int mqtt_port = 1883;
```

For cloud MQTT brokers (HiveMQ, Mosquitto, etc.), use the public endpoint:

```cpp
const char* mqtt_server = "broker.hivemq.com";
const int mqtt_port = 1883;
```

## Hardware Deployment

### Flashing to Physical ESP32

1. Connect ESP32 via USB
2. Identify the port:
   - Linux: `/dev/ttyUSB0` or `/dev/ttyACM0`
   - macOS: `/dev/cu.usbserial-*`
   - Windows: `COM3`, `COM4`, etc.

3. Flash the firmware:

```bash
platformio run -t upload -d firmware/esp32-node
```

4. Monitor serial output:

```bash
platformio device monitor -d firmware/esp32-node
```

### Troubleshooting Flash Issues

If flashing fails, hold the BOOT button during upload, or erase flash first:

```bash
platformio run -t erase -d firmware/esp32-node
platformio run -t upload -d firmware/esp32-node
```

## Berry Script Usage

Berry is a lightweight scripting language embedded in the firmware for runtime customization without reflashing.

### Example 1: Toggle GPIO Pin

```berry
# Toggle LED on GPIO 2
import gpio
gpio.pin_mode(2, gpio.OUTPUT)
gpio.digital_write(2, 1)  # Turn on
```

### Example 2: Read Sensor Data

```berry
# Read temperature from DHT22 on GPIO 4
import dht
var sensor = dht.DHT22(4)
sensor.read()
print("Temperature:", sensor.temperature(), "°C")
print("Humidity:", sensor.humidity(), "%")
```

### Example 3: MQTT Publish

```berry
# Publish sensor data to MQTT topic
import mqtt
var client = mqtt.Client()
client.connect("192.168.1.100", 1883)
client.publish("sensors/temp", "23.5")
client.disconnect()
```

### Example 4: Timer Callback

```berry
# Execute function every 5 seconds
import time
def periodic_task()
  print("Task executed at:", time.millis())
end
time.set_timer(5000, periodic_task)
```

### Loading Berry Scripts

Upload scripts via serial or MQTT, then execute:

```bash
# Via serial monitor
> berry.load("script.be")

# Via MQTT command topic
mosquitto_pub -t "esp32/cmd" -m "berry.load('script.be')"
```

## QEMU Testing

QEMU allows testing firmware without physical hardware. Two environments are available:

- `esp32dev` (Arduino framework) — for physical hardware
- `qemu` (ESP-IDF framework) — for QEMU emulation

### Install QEMU

Install via ESP-IDF tools:

```bash
# Requires ESP-IDF installed and $IDF_PATH set
python $IDF_PATH/tools/idf_tools.py install qemu-xtensa
```

Or use the automated test script:

```bash
./scripts/test-qemu.sh
```

### Quick QEMU Test Commands

Build and run firmware in QEMU:

```bash
# Build for QEMU environment
platformio run -d firmware/esp32-node -e qemu

# Run in QEMU (basic)
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw

# Run with serial monitor
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
  -serial mon:stdio

# Run with network (tap device)
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
  -netdev user,id=net0 -device esp32_eth,netdev=net0
```

### QEMU Debugging Guide

QEMU supports GDB remote debugging for step-through execution and inspection.

**1. Build with debug symbols:**

```bash
platformio run -d firmware/esp32-node -e qemu --build-flags "-DDEBUG"
```

**2. Start QEMU with GDB server:**

The `-s` flag opens GDB server on port 1234, `-S` pauses execution at startup:

```bash
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
  -s -S
```

**3. Connect GDB in another terminal:**

```bash
xtensa-esp32-elf-gdb .pio/build/qemu/firmware.elf
(gdb) target remote :1234
(gdb) break app_main
(gdb) continue
```

**4. Common GDB commands:**

```gdb
(gdb) info registers          # View CPU registers
(gdb) backtrace               # Stack trace
(gdb) print variable_name     # Inspect variables
(gdb) step                    # Step into functions
(gdb) next                    # Step over
(gdb) finish                  # Run until function returns
(gdb) watch variable_name     # Break on variable change
(gdb) info breakpoints        # List breakpoints
```

**5. Debug MQTT protocol flow:**

Set breakpoints on key functions:

```gdb
(gdb) break send_register
(gdb) break callback
(gdb) break publish_result
(gdb) continue
```

**6. Monitor network traffic:**

QEMU supports user-mode networking (no root required):

```bash
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
  -netdev user,id=net0,hostfwd=tcp::1883-:1883 \
  -device esp32_eth,netdev=net0 \
  -s -S
```

### QEMU Test Workflow

Typical development cycle:

```bash
# 1. Edit code in src/main.cpp
# 2. Build for QEMU
platformio run -d firmware/esp32-node -e qemu

# 3. Run automated test
./scripts/test-qemu.sh

# 4. Or run with GDB for debugging
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
  -s -S &
xtensa-esp32-elf-gdb .pio/build/qemu/firmware.elf
```

### QEMU Limitations

- No WiFi emulation (network via tap/user-mode only)
- No Bluetooth support
- GPIO/ADC reads return simulated values
- SPIFFS filesystem may behave differently than physical flash
- Timing may differ from real hardware

For full hardware validation, flash to physical ESP32 after QEMU testing.
