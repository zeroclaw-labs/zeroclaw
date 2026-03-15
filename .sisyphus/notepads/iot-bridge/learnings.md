
## Task 11: Heartbeat Implementation

### What Was Done
- ESP32 sends heartbeat every 30s to `zeroclaw/nodes/{node_id}/heartbeat`
- Bridge subscribes to heartbeat topic and forwards to WebSocket
- Heartbeat logged on reception for monitoring

### Implementation Details
- ESP32: Added `last_heartbeat` timer and `HEARTBEAT_INTERVAL` (30000ms)
- ESP32: `send_heartbeat()` publishes JSON with `millis()` timestamp
- ESP32: Loop checks timer and sends heartbeat when interval elapsed
- Bridge: Added heartbeat subscription on connect and reconnect
- Bridge: Logs heartbeat reception with `info!()` for visibility
- Bridge: Forwards heartbeat to WebSocket like other messages

### Verification
- Use `mosquitto_sub -h localhost -t 'zeroclaw/nodes/+/heartbeat' -v`
- Expect messages every 30 seconds with timestamp payload
- Gateway handles timeout detection (90s no heartbeat = offline)

### Notes
- QoS 0 (fire and forget) for minimal overhead
- Simple JSON payload: `{"timestamp": <millis>}`
- No complex state tracking in bridge (gateway owns lifecycle)
