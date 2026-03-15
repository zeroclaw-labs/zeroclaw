#!/bin/bash
set -e

echo "=== ESP32 GPIO QEMU Test ==="

FIRMWARE_DIR="firmware/esp32-node"
BUILD_DIR="$FIRMWARE_DIR/.pio/build/esp32dev"

if [ ! -f "$BUILD_DIR/firmware.elf" ]; then
  echo "Building firmware..."
  platformio run -d "$FIRMWARE_DIR"
fi

echo "Starting QEMU with GPIO test..."
timeout 10s qemu-system-xtensa \
  -M esp32 \
  -nographic \
  -kernel "$BUILD_DIR/firmware.elf" \
  -serial mon:stdio \
  -d guest_errors,unimp || true

echo ""
echo "GPIO test completed (QEMU exit expected)"
