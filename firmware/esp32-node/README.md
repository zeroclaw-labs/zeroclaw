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

Install QEMU for ESP32:

```bash
python $IDF_PATH/tools/idf_tools.py install qemu-xtensa
```

Run in QEMU:

```bash
./scripts/test-qemu.sh
```

### QEMU Debugging Guide

1. **Build with debug symbols:**

```bash
platformio run -d firmware/esp32-node -e debug
```

2. **Start QEMU with GDB server:**

```bash
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/esp32dev/firmware.bin,if=mtd,format=raw \
  -s -S
```

3. **Connect GDB in another terminal:**

```bash
xtensa-esp32-elf-gdb .pio/build/esp32dev/firmware.elf
(gdb) target remote :1234
(gdb) break app_main
(gdb) continue
```

4. **Common GDB commands:**

```gdb
(gdb) info registers          # View CPU registers
(gdb) backtrace              # Stack trace
(gdb) print variable_name    # Inspect variables
(gdb) step                   # Step into functions
(gdb) next                   # Step over
```

5. **Monitor network traffic in QEMU:**

```bash
# Enable QEMU network tap
qemu-system-xtensa -netdev tap,id=net0 -device esp32_eth,netdev=net0
```
