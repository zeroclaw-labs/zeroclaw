#!/bin/bash
set -e

echo "=== ZeroClaw MQTT Bridge E2E Test (Berry Script) ==="

GATEWAY_URL="http://localhost:42617"
MQTT_TOPIC_BASE="zeroclaw/nodes/esp32-qemu-test"

echo "[1/5] Waiting for services..."
sleep 5

echo "[2/5] Checking QEMU ESP32 registration..."
if mosquitto_sub -h localhost -t "$MQTT_TOPIC_BASE/register" -C 1 -W 5 | grep -q "esp32-qemu-test"; then
  echo "✓ ESP32 QEMU registered"
else
  echo "✗ ESP32 QEMU registration failed"
  exit 1
fi

echo "[3/5] Testing Berry script cache..."
mosquitto_pub -h localhost -t "$MQTT_TOPIC_BASE/invoke" -m '{
  "call_id": "test-cache-1",
  "capability": "script_cache",
  "args": {
    "script_id": "blink",
    "code": "import gpio\ngpio.write(2, 1)"
  }
}'

sleep 2
if mosquitto_sub -h localhost -t "$MQTT_TOPIC_BASE/result" -C 1 -W 5 | grep -q '"success":true'; then
  echo "✓ Berry script cached"
else
  echo "✗ Berry script cache failed"
  exit 1
fi

echo "[4/5] Testing Berry script execution..."
mosquitto_pub -h localhost -t "$MQTT_TOPIC_BASE/invoke" -m '{
  "call_id": "test-exec-1",
  "capability": "script_execute",
  "args": {
    "script_id": "blink"
  }
}'

sleep 2
if mosquitto_sub -h localhost -t "$MQTT_TOPIC_BASE/result" -C 1 -W 5 | grep -q '"success":true'; then
  echo "✓ Berry script executed"
else
  echo "✗ Berry script execution failed"
  exit 1
fi

echo "[5/5] Testing GPIO via Berry..."
mosquitto_pub -h localhost -t "$MQTT_TOPIC_BASE/invoke" -m '{
  "call_id": "test-gpio-1",
  "capability": "script_execute",
  "args": {
    "script_id": "blink"
  }
}'

sleep 2
if mosquitto_sub -h localhost -t "$MQTT_TOPIC_BASE/result" -C 1 -W 5 | grep -q '"output"'; then
  echo "✓ GPIO Berry test passed"
else
  echo "✗ GPIO Berry test failed"
  exit 1
fi

echo ""
echo "=== All E2E Berry tests passed ==="
exit 0
