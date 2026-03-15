# MQTT Bridge Integration Tests

End-to-end test suite for ZeroClaw MQTT bridge with ESP32 QEMU and Berry Script support.

## Services

- **mosquitto**: MQTT broker (port 1883)
- **zeroclaw-gateway**: ZeroClaw gateway (port 42617)
- **zeroclaw-bridge**: MQTT-to-WebSocket bridge
- **esp32-qemu**: ESP32 firmware in QEMU emulator

## Quick Start

```bash
cd tests/integration
docker-compose up -d
./test-e2e-berry.sh
docker-compose down
```

## Test Coverage

1. ESP32 QEMU registration via MQTT
2. Berry script caching (script_cache capability)
3. Berry script execution (script_execute capability)
4. GPIO control via Berry scripts
5. End-to-end message flow: MQTT → Bridge → Gateway → Result

## Requirements

- Docker and Docker Compose
- mosquitto-clients (for testing)

## Environment Variables

Set `API_KEY` for the gateway:

```bash
export API_KEY=sk-your-key
docker-compose up -d
```
