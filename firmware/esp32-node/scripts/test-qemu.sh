#!/bin/bash
set -e

cd "$(dirname "$0")/.."

echo "Building firmware for QEMU..."
platformio run -e qemu

echo "Running firmware in QEMU..."
qemu-system-xtensa -nographic -machine esp32 \
  -drive file=.pio/build/qemu/firmware.bin,if=mtd,format=raw \
  -serial mon:stdio
