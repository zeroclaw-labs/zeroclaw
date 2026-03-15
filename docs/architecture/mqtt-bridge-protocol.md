# MQTT Bridge Protocol

## Overview

This document defines the MQTT topic structure and message schemas for ZeroClaw's MQTT bridge, enabling tethered nodes to register capabilities and execute tools via MQTT transport.

The protocol aligns with ZeroClaw's existing WebSocket gateway protocol (`src/gateway/nodes.rs`) to maintain consistency across transports.

## Topic Structure

All MQTT topics follow the pattern:

```
zeroclaw/nodes/{node_id}/{message_type}
```

### Topic Components

- `zeroclaw/nodes/` — Fixed prefix for all node messages
- `{node_id}` — Unique identifier for the node (alphanumeric, hyphens, underscores)
- `{message_type}` — One of: `register`, `invoke`, `result`, `heartbeat`

### Topic Types

| Topic Pattern | Direction | Purpose |
|---|---|---|
| `zeroclaw/nodes/{node_id}/register` | Node → Gateway | Node registration with capabilities |
| `zeroclaw/nodes/{node_id}/invoke` | Gateway → Node | Tool invocation request |
| `zeroclaw/nodes/{node_id}/result` | Node → Gateway | Tool execution result |
| `zeroclaw/nodes/{node_id}/heartbeat` | Node → Gateway | Node liveness signal |

## Message Schemas

All messages use JSON payloads with UTF-8 encoding.

### 1. Register Message

**Topic:** `zeroclaw/nodes/{node_id}/register`  
**Direction:** Node → Gateway  
**QoS:** 1 (at least once)  
**Retain:** false

Sent by a node to advertise its capabilities to the gateway.

**Schema:**

```json
{
  "type": "register",
  "node_id": "string",
  "capabilities": [
    {
      "name": "string",
      "description": "string",
      "parameters": {
        "type": "object",
        "properties": {}
      }
    }
  ]
}
```

**Example:**

```json
{
  "type": "register",
  "node_id": "rpi-sensor-01",
  "capabilities": [
    {
      "name": "read_temperature",
      "description": "Read temperature from DHT22 sensor",
      "parameters": {
        "type": "object",
        "properties": {
          "unit": {
            "type": "string",
            "enum": ["celsius", "fahrenheit"],
            "default": "celsius"
          }
        }
      }
    },
    {
      "name": "toggle_led",
      "description": "Toggle GPIO LED on/off",
      "parameters": {
        "type": "object",
        "properties": {
          "state": {
            "type": "boolean"
          }
        },
        "required": ["state"]
      }
    }
  ]
}
```

### 2. Invoke Message

**Topic:** `zeroclaw/nodes/{node_id}/invoke`  
**Direction:** Gateway → Node  
**QoS:** 1 (at least once)  
**Retain:** false

Sent by the gateway to request tool execution on a node.

**Schema:**

```json
{
  "type": "invoke",
  "call_id": "string",
  "capability": "string",
  "args": {}
}
```

**Example:**

```json
{
  "type": "invoke",
  "call_id": "call_20260315_063543_abc123",
  "capability": "read_temperature",
  "args": {
    "unit": "celsius"
  }
}
```

### 3. Result Message

**Topic:** `zeroclaw/nodes/{node_id}/result`  
**Direction:** Node → Gateway  
**QoS:** 1 (at least once)  
**Retain:** false

Sent by a node to return tool execution results.

**Schema:**

```json
{
  "type": "result",
  "call_id": "string",
  "success": boolean,
  "output": "string",
  "error": "string | null"
}
```

**Example (Success):**

```json
{
  "type": "result",
  "call_id": "call_20260315_063543_abc123",
  "success": true,
  "output": "Temperature: 22.5°C",
  "error": null
}
```

**Example (Failure):**

```json
{
  "type": "result",
  "call_id": "call_20260315_063543_xyz789",
  "success": false,
  "output": "",
  "error": "Sensor read timeout after 5s"
}
```

### 4. Heartbeat Message

**Topic:** `zeroclaw/nodes/{node_id}/heartbeat`  
**Direction:** Node → Gateway  
**QoS:** 0 (at most once)  
**Retain:** false

Periodic liveness signal from node to gateway.

**Schema:**

```json
{
  "type": "heartbeat",
  "node_id": "string",
  "timestamp": "ISO8601 string",
  "uptime_seconds": number
}
```

**Example:**

```json
{
  "type": "heartbeat",
  "node_id": "rpi-sensor-01",
  "timestamp": "2026-03-15T06:35:43.331Z",
  "uptime_seconds": 86400
}
```

## Protocol Flow

```
Node                           Gateway
  |                               |
  |-- register ------------------>|  (1) Node advertises capabilities
  |                               |
  |<-- (ack via connection) ------|  (2) Gateway accepts registration
  |                               |
  |-- heartbeat ----------------->|  (3) Periodic liveness (optional)
  |                               |
  |<-- invoke --------------------|  (4) Gateway requests tool execution
  |                               |
  |-- result -------------------->|  (5) Node returns execution result
  |                               |
```

## Naming Conventions

### Node IDs

- Alphanumeric characters, hyphens, underscores only
- No spaces or special characters
- Recommended format: `{device_type}-{location}-{number}` (e.g., `rpi-garage-01`)

### Capability Names

- Snake_case format
- Descriptive verb-noun pairs (e.g., `read_temperature`, `toggle_led`)
- No spaces or special characters

### Call IDs

- Unique per invocation
- Recommended format: `call_{timestamp}_{random}` (e.g., `call_20260315_063543_abc123`)

## Error Handling

### Connection Loss

- Nodes should use MQTT Last Will and Testament (LWT) to signal disconnection
- Gateway should mark nodes as offline after missing N consecutive heartbeats

### Message Delivery

- All messages except heartbeat use QoS 1 for guaranteed delivery
- Nodes must handle duplicate invocations (idempotency)

### Timeout Handling

- Gateway should timeout invoke requests after configurable duration (default: 30s)
- Nodes should return error results for operations that exceed internal timeouts

## Security Considerations

- MQTT broker should require authentication (username/password or certificates)
- TLS encryption recommended for production deployments
- Node IDs should be validated against allowlist in gateway configuration
- Capability parameters should be validated against schema before execution

## Alignment with WebSocket Protocol

This MQTT protocol mirrors the WebSocket node protocol defined in `src/gateway/nodes.rs`:

- `NodeMessage::Register` → MQTT register message
- `NodeMessage::Result` → MQTT result message
- `GatewayMessage::Invoke` → MQTT invoke message
- Message field names and types match exactly for seamless transport abstraction
