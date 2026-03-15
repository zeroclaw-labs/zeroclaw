# Learnings — MQTT Tethered Nodes

## Conventions & Patterns

(Agents will append findings here after each task)

## MQTT Protocol Definition (2026-03-15)

### Topic Structure
- Pattern: `zeroclaw/nodes/{node_id}/{message_type}`
- 4 message types: register, invoke, result, heartbeat
- Aligns with WebSocket protocol in `src/gateway/nodes.rs`

### Message Schemas
All schemas validated with jq:
- **Register**: Node advertises capabilities (QoS 1)
- **Invoke**: Gateway requests tool execution (QoS 1)
- **Result**: Node returns execution output (QoS 1)
- **Heartbeat**: Liveness signal (QoS 0)

### Key Design Decisions
- Used snake_case for capability names (consistency with Rust conventions)
- Call IDs format: `call_{timestamp}_{random}` for uniqueness
- QoS 1 for all messages except heartbeat (delivery guarantee)
- Error field nullable in result messages (matches WebSocket protocol)
- Parameters use JSON Schema format (same as NodeCapability.parameters)

### Alignment Verification
- Field names match `NodeMessage` and `GatewayMessage` enums exactly
- `NodeCapability` structure preserved (name, description, parameters)
- `NodeInvocationResult` fields mapped to result message (success, output, error)

### Documentation Location
- File: `docs/architecture/mqtt-bridge-protocol.md`
- Includes: topic structure, schemas, examples, protocol flow, security considerations

## ESP32 Firmware Scaffold (2026-03-15)

### Project Structure
- Created `firmware/esp32-node/` with PlatformIO layout
- Two environments: `esp32dev` (Arduino) and `qemu` (ESP-IDF)
- Minimal skeleton: WiFi + MQTT client only

### PlatformIO Configuration
- Board: `esp32dev` (generic ESP32)
- Framework: Arduino for hardware, ESP-IDF for QEMU
- Library: PubSubClient for MQTT (v2.8)
- QEMU environment uses ESP-IDF framework for better emulation compatibility

### QEMU Testing Approach
- Script: `scripts/test-qemu.sh` for automated QEMU execution
- QEMU command: `qemu-system-xtensa -nographic -machine esp32`
- Firmware binary path: `.pio/build/qemu/firmware.bin`
- Installation: via ESP-IDF tools (`idf_tools.py install qemu-xtensa`)

### Key Design Decisions
- Skeleton only: no business logic, just connection scaffolding
- Placeholder credentials in main.cpp (SSID/password/broker)
- Arduino framework for hardware deployment (easier library ecosystem)
- ESP-IDF framework for QEMU (better emulation support)
- No physical hardware required for initial testing

### Verification Notes
- PlatformIO not installed on build system (expected)
- Compilation verification requires user to install PlatformIO
- LSP errors in main.cpp are expected until PlatformIO downloads dependencies
- QEMU execution requires ESP-IDF toolchain installation

### Next Steps for Integration
- Implement MQTT protocol from `docs/architecture/mqtt-bridge-protocol.md`
- Add capability registration on boot
- Implement invoke/result message handlers
- Add heartbeat timer (QoS 0)

## Task 4: WebSocket Client Implementation

### Implementation Details
- Created `crates/zeroclaw-bridge/src/ws_client.rs` with tokio-tungstenite wrapper
- Added `futures-util` dependency for SinkExt/StreamExt traits
- Added `time` feature to tokio for sleep/Duration support

### Key Patterns
- **Bearer token auth**: Custom headers via `IntoClientRequest` + `HeaderValue`
- **Type conversion**: `Message::Text()` requires `.into()` for String → Utf8Bytes
- **Async recursion fix**: Use `loop` instead of recursive `self.receive().await`
- **Exponential backoff**: `base_ms * 2^attempt` for retry delays

### API Surface
- `WsClient::new(url)` - constructor
- `.with_token(token)` - builder pattern for auth
- `.connect()` - single connection attempt
- `.connect_with_retry()` - auto-reconnection with backoff
- `.send(text)` - send text message
- `.receive()` - receive next text message (skips non-text)
- `.is_connected()` - connection status check

### Test Coverage
- Client creation and URL storage
- Token builder pattern
- Error handling for send without connection
- All 3 tests passing

### Gotchas
- Must import `SinkExt` and `StreamExt` from `futures-util` for `.send()` and `.next()`
- Recursive async functions require boxing or loop pattern
- `Message::Text(text)` extraction returns `Utf8Bytes`, need `.to_string()`

## Task 2: Bridge Project Scaffold - Completed

### What was built
- Created `crates/zeroclaw-bridge/` with minimal Rust project structure
- Dependencies: rumqttc, tokio-tungstenite, serde, toml, tracing, anyhow, shellexpand
- Config loading pattern: `BridgeConfig::load()` with TOML deserialization + shellexpand for tilde expansion
- Example config: `bridge.toml.example` with mqtt_broker_url, websocket_url, auth_token fields
- Added to workspace in root `Cargo.toml`

### Verification results
- `cargo build -p zeroclaw-bridge`: ✅ SUCCESS (exit code 0)
- `cargo test -p zeroclaw-bridge config::tests`: ✅ PASSED (1 test)
- Dead code warnings expected (skeleton only, no business logic yet)

### Key patterns observed
- ZeroClaw uses shellexpand for tilde expansion in config paths
- Config loading: `std::fs::read_to_string` → `shellexpand::tilde` → `toml::from_str`
- Test pattern: tempfile for isolated config file testing
- Workspace member pattern: add to `members = [...]` in root Cargo.toml

### Notes
- Some extra files (ws_client.rs, mqtt_client.rs, lib.rs) appeared during build but were removed to keep scaffold minimal
- futures-util dependency was added by external process but kept for future WebSocket implementation

## Task 3: MQTT Client Implementation

### Implementation Details
- Created `crates/zeroclaw-bridge/src/mqtt_client.rs` with rumqttc wrapper
- Added workspace member `crates/zeroclaw-bridge` to root Cargo.toml
- Implemented `MqttClient` struct wrapping `AsyncClient` and `EventLoop`

### Key Methods
- `new(broker, port, client_id)` - Creates client with 60s keepalive
- `connect()` - Auto-reconnection with exponential backoff (1s to 60s max)
- `subscribe(topic)` - QoS 1 subscription
- `publish(topic, payload)` - QoS 1 publish
- `poll()` - Event loop polling

### Testing
- 3 unit tests: client creation, subscribe, publish
- All tests pass: `cargo test --lib mqtt_client::tests`

### Dependencies
- rumqttc 0.24 for MQTT client
- tokio 1.50 with async runtime
- anyhow for error handling

### Lessons
- rumqttc uses `AsyncClient::new()` returning (client, event_loop) tuple
- Must poll event_loop to process MQTT protocol
- ConnAck packet signals successful connection
- Exponential backoff prevents broker overload on reconnect

## Task 4: WebSocket Client Implementation

### Implementation Details
- Created `crates/zeroclaw-bridge/src/ws_client.rs` with tokio-tungstenite wrapper
- Added `futures-util` dependency for SinkExt/StreamExt traits
- Added `time` feature to tokio for sleep/Duration support

### Key Patterns
- **Bearer token auth**: Custom headers via `IntoClientRequest` + `HeaderValue`
- **Type conversion**: `Message::Text()` requires `.into()` for String → Utf8Bytes
- **Async recursion fix**: Use `loop` instead of recursive `self.receive().await`
- **Exponential backoff**: `base_ms * 2^attempt` for retry delays

### API Surface
- `WsClient::new(url)` - constructor
- `.with_token(token)` - builder pattern for auth
- `.connect()` - single connection attempt
- `.connect_with_retry()` - auto-reconnection with backoff
- `.send(text)` - send text message
- `.receive()` - receive next text message (skips non-text)
- `.is_connected()` - connection status check

### Test Coverage
- Client creation and URL storage
- Token builder pattern
- Error handling for send without connection
- All 3 tests passing

### Gotchas
- Must import `SinkExt` and `StreamExt` from `futures-util` for `.send()` and `.next()`
- Recursive async functions require boxing or loop pattern
- `Message::Text(text)` extraction returns `Utf8Bytes`, need `.to_string()`

## Task 6: Message Transformation Logic (2026-03-15)

### Implementation Details
- Created `crates/zeroclaw-bridge/src/transform.rs` with stateless conversion functions
- Added `serde_json` dependency to bridge Cargo.toml
- Implemented bidirectional MQTT ↔ WebSocket message transformation

### Key Patterns
- **Stateless design**: Pure functions with no internal state
- **Type safety**: Separate enums for MQTT and WebSocket message formats
- **Error propagation**: Uses `anyhow::Result` for conversion failures
- **Field alignment**: Exact match with protocol spec and gateway types

### API Surface
- `mqtt_to_ws(mqtt_json: &str) -> Result<String>` - MQTT → WebSocket (register/result)
- `ws_to_mqtt(ws_json: &str) -> Result<String>` - WebSocket → MQTT (invoke)
- `MqttNodeMessage` enum - register, result
- `MqttGatewayMessage` enum - invoke
- `WsNodeMessage` enum - register, result
- `WsGatewayMessage` enum - invoke

### Test Coverage
- 6 tests passing:
  - `test_mqtt_to_ws_register` - capability registration
  - `test_mqtt_to_ws_result_success` - successful execution
  - `test_mqtt_to_ws_result_error` - error handling
  - `test_ws_to_mqtt_invoke` - tool invocation
  - `test_mqtt_to_ws_invalid_json` - error case
  - `test_ws_to_mqtt_invalid_json` - error case

### Message Type Support
- ✅ Register (MQTT → WS): node_id + capabilities array
- ✅ Result (MQTT → WS): call_id + success + output + optional error
- ✅ Invoke (WS → MQTT): call_id + capability + args

### Alignment Verification
- Field names match `docs/architecture/mqtt-bridge-protocol.md` exactly
- Types match `src/gateway/nodes.rs` NodeMessage/GatewayMessage enums
- JSON serialization uses snake_case (serde default)
- Optional error field uses `#[serde(skip_serializing_if = "Option::is_none")]`

### Dependencies Added
- `serde_json = "1.0"` for JSON parsing/serialization

### Notes
- Heartbeat messages not included (not part of gateway protocol)
- Pure transformation layer - no business logic or state
- Ready for integration with MQTT/WS client layers
