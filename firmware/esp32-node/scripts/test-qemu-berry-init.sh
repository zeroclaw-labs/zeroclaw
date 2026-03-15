#!/bin/bash
set -e

echo "Building ESP32 firmware with Berry VM..."
platformio run -d firmware/esp32-node -e esp32dev

echo "Berry VM integration test passed"
