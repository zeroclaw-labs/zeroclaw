# Task 9: ESP32 Command Executor - Learnings

## Implementation Approach
- Command whitelist enforced at firmware level (gpio_read, gpio_write, adc_read)
- MQTT topics follow zeroclaw/nodes/{node_id}/invoke and /result pattern
- JSON message format with request_id for async response correlation
- ArduinoJson used for lightweight JSON parsing on ESP32

## Key Patterns
- pinMode() called dynamically based on command (INPUT for read, OUTPUT for write)
- Result publishing uses consistent format: {request_id, success, data?, error?}
- Command validation happens before parameter parsing to fail fast
- QEMU test scripts use timeout to handle expected emulation limitations

## Security
- Whitelist array prevents arbitrary command execution
- Only three commands allowed: gpio_read, gpio_write, adc_read
- Missing parameters return error responses instead of crashing
