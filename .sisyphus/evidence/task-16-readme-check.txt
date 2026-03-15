- USB cable for flashing
### Flashing to Physical ESP32
3. Flash the firmware:
### Troubleshooting Flash Issues
If flashing fails, hold the BOOT button during upload, or erase flash first:
Berry is a lightweight scripting language embedded in the firmware for runtime customization without reflashing.
- SPIFFS filesystem may behave differently than physical flash
For full hardware validation, flash to physical ESP32 after QEMU testing.
## Configuration
Minimal ESP32 firmware skeleton with WiFi and MQTT support, designed for QEMU testing and physical hardware deployment.
- QEMU for ESP32 (optional, for emulation)
## QEMU Testing
QEMU allows testing firmware without physical hardware. Two environments are available:
- `qemu` (ESP-IDF framework) — for QEMU emulation
### Install QEMU
python $IDF_PATH/tools/idf_tools.py install qemu-xtensa
./scripts/test-qemu.sh
### Quick QEMU Test Commands
Build and run firmware in QEMU:
# Build for QEMU environment
platformio run -d firmware/esp32-node -e qemu
# Run in QEMU (basic)
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
### QEMU Debugging Guide
QEMU supports GDB remote debugging for step-through execution and inspection.
platformio run -d firmware/esp32-node -e qemu --build-flags "-DDEBUG"
**2. Start QEMU with GDB server:**
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
xtensa-esp32-elf-gdb .pio/build/qemu/firmware.elf
QEMU supports user-mode networking (no root required):
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
### QEMU Test Workflow
# 2. Build for QEMU
platformio run -d firmware/esp32-node -e qemu
./scripts/test-qemu.sh
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
xtensa-esp32-elf-gdb .pio/build/qemu/firmware.elf
### QEMU Limitations
For full hardware validation, flash to physical ESP32 after QEMU testing.
### QEMU Debugging Guide
QEMU supports GDB remote debugging for step-through execution and inspection.
**1. Build with debug symbols:**
platformio run -d firmware/esp32-node -e qemu --build-flags "-DDEBUG"
**5. Debug MQTT protocol flow:**
# 4. Or run with GDB for debugging
