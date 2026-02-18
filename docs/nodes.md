# ZeroClaw Node Management

ZeroClaw supports multi-node management, allowing you to control remote machines from a central gateway via WebSocket connections.

## Architecture

```
┌─────────────────┐   WebSocket   ┌─────────────────┐
│   Node Server   │◄──────────────│   Node Client   │
│ (Main Gateway)  │  Reverse Conn │  (Remote Host)  │
└────────┬────────┘               └─────────────────┘
         │
    ┌────┴────┬────────────┐
    ↓         ↓            ↓
┌───────┐ ┌───────┐   ┌──────────┐
│ Node 1│ │ Node 2│   │  Node 3  │
│ (VPS) │ │ (PC)  │   │ (Phone)  │
└───────┘ └───────┘   └──────────┘
```

## Features

- **Reverse WebSocket Connection**: Remote nodes connect to the main gateway, solving NAT issues
- **6-Digit Pairing Code**: Simple, time-limited pairing mechanism
- **Remote Command Execution**: Run commands on any connected node
- **Status Monitoring**: Track node health and connectivity

## Quick Start

### 1. Enable Node Server

Add to your `config.toml`:

```toml
[nodes]
enabled = true
listen_port = 8765
pairing_timeout_secs = 300
```

### 2. Start the Daemon

```bash
zeroclaw daemon
```

### 3. Generate a Pairing Code

```bash
zeroclaw node generate-code
```

Example output:
```
✅ Pairing code generated: 123456
⏰ Expires in 300 seconds

On your remote node, run:
  zeroclaw node connect --server ws://gateway-host:8765 --code 123456
```

### 4. Connect Remote Node

On your remote machine:

```bash
zeroclaw node connect --server ws://gateway-host:8765 --code 123456 --name my-laptop
```

### 5. List Connected Nodes

```bash
zeroclaw node list
```

## Commands

### `zeroclaw node generate-code`

Generate a new 6-digit pairing code. The code expires after `pairing_timeout_secs`.

### `zeroclaw node connect`

Connect this node to a remote gateway.

**Arguments:**
- `--server <url>`: Gateway WebSocket URL (e.g., `ws://gateway-host:8765`)
- `--code <code>`: 6-digit pairing code
- `--name <name>`: Optional node name (defaults to hostname)

### `zeroclaw node list`

List all connected nodes.

## Configuration

```toml
[nodes]
# Enable node server
enabled = false

# WebSocket listen port
listen_port = 8765

# Pairing timeout in seconds (default: 300 = 5 minutes)
pairing_timeout_secs = 300

# Pre-authorized nodes (optional)
[[nodes.authorized]]
id = "node-uuid"
name = "trusted-node"
public_key = "optional-public-key"
```

## Agent Tools

Once nodes are connected, the ZeroClaw agent can use these tools:

### `nodes_list`

List all connected nodes.

**Parameters:** None

### `nodes_run`

Execute a command on a remote node.

**Parameters:**
- `node_id` (required): Target node ID
- `command` (required): Command to execute
- `timeout_secs` (optional): Timeout in seconds (default: 60)

**Example:**
```
Use the nodes_run tool to execute "ls -la" on node "abc-123-def" with default timeout.
```

### `nodes_status`

Get status information for a specific node or overall server status.

**Parameters:**
- `node_id` (optional): Node ID to query. If not provided, returns overall server status.

**With node_id:**
Returns detailed node information including:
- Node metadata (id, name, hostname, platform)
- Status: `online` (active < 60s), `idle` (60-180s), `offline` (> 180s)
- Seconds since last activity

**Without node_id:**
Returns server overview:
- Total nodes count
- Online/idle/offline breakdown
- List of all nodes with status

**Examples:**
```
# Get overall status
Use nodes_status tool

# Get specific node
Use nodes_status tool with node_id "abc-123"
```

## Security

- Pairing codes are 6-digit random numbers
- Codes expire after a configurable timeout
- Connections use WebSocket with optional TLS
- Pre-authorized nodes can be configured

## Examples

### Example 1: Basic Setup

**On Gateway:**
```bash
# Enable nodes in config
cat >> ~/.zeroclaw/config.toml << EOF
[nodes]
enabled = true
listen_port = 8765
EOF

# Start daemon
zeroclaw daemon

# In another terminal, generate code
zeroclaw node generate-code
```

**On Remote Node:**
```bash
zeroclaw node connect --server ws://gateway-host:8765 --code 123456
```

### Example 2: Using Agent to Run Remote Commands

Once connected, you can ask the agent:

> "List all connected nodes"

Agent will call `nodes_list` tool.

> "Run 'df -h' on node abc-123"

Agent will call `nodes_run` tool with:
- `node_id`: "abc-123"
- `command`: "df -h"

### Example 3: Check Node Status

> "Show me the status of node xyz-789"

Agent will call `nodes_status` tool with `node_id`: "xyz-789"

## Troubleshooting

### Connection Refused

Ensure:
1. Node server is enabled in config (`[nodes] enabled = true`)
2. Daemon is running (`zeroclaw daemon`)
3. Firewall allows port 8765
4. Correct server URL format (`ws://host:port`)

### Invalid Pairing Code

Ensure:
1. Code is current (not expired)
2. Code matches exactly (6 digits)
3. Generate a new code if needed

### Node Not in List

Check daemon logs for connection errors.

## Advanced

### Custom Node Names

```bash
zeroclaw node connect --server ws://gateway:8765 --code 123456 --name production-db
```

### Pre-Authorized Nodes

For production environments, pre-authorize nodes in config:

```toml
[[nodes.authorized]]
id = "550e8400-e29b-41d4-a716-446655440000"
name = "production-server"
```

### Multiple Gateways

You can run multiple gateways on different ports:

```toml
[nodes]
enabled = true
listen_port = 8765
```

Then connect nodes to `ws://gateway1:8765`, `ws://gateway2:8766`, etc.

## API Reference

### NodeInfo

```rust
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub hostname: Option<String>,
    pub platform: String,
    pub connected_at: u64,
    pub last_seen: u64,
}
```

### NodeCommand

```rust
pub enum NodeCommand {
    Ping,
    Exec { command: String, timeout_secs: Option<u32> },
    Status,
}
```

### NodeResponse

```rust
pub enum NodeResponse {
    Pong,
    ExecResult {
        success: bool,
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    StatusReport {
        cpu_percent: f32,
        memory_percent: f32,
        uptime_secs: u64,
    },
}
```

## See Also

- [Configuration](../README.md#configuration)
- [Agent Tools](../README.md#tools)
- [Gateway](gateway.md)