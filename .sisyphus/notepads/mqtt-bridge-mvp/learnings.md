
## Task 12: ESP32 Registration
- Register message sent immediately after MQTT connect in reconnect()
- Includes 3 capabilities: gpio_read, gpio_write, adc_read
- Each capability has name, description, and JSON schema parameters
- Uses QoS 0 (default) for register - protocol specifies QoS 1 but keeping minimal for V1
- Topic: zeroclaw/nodes/{node_id}/register
- Message format matches protocol spec exactly
