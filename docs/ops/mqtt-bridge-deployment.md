# MQTT Bridge Deployment

This document covers deploying the ZeroClaw MQTT bridge for tethered nodes (ESP32, Raspberry Pi, Arduino) that communicate via MQTT instead of direct WebSocket connections.

## Overview

The MQTT bridge enables resource-constrained devices to register capabilities and execute tools through an MQTT broker, with the bridge translating between MQTT and the ZeroClaw gateway WebSocket protocol.

**Architecture:**

```
[ESP32/RPi Node] --MQTT--> [Mosquitto Broker] --MQTT--> [Bridge] --WebSocket--> [ZeroClaw Gateway]
```

**Key components:**

- MQTT broker (mosquitto recommended)
- zeroclaw-bridge binary
- ZeroClaw gateway (running with node support)
- Tethered nodes (ESP32, RPi, Arduino)

## Prerequisites

- Rust toolchain (for building from source)
- MQTT broker (mosquitto 2.0+)
- ZeroClaw gateway running and accessible
- systemd (for service management)

## Installation

### 1. Build the Bridge

```bash
cd /path/to/zeroclaw
cargo build --release -p zeroclaw-bridge
```

Binary location: `target/release/zeroclaw-bridge`

### 2. Install as User Service

```bash
./scripts/install-bridge.sh
```

This script:
- Copies binary to `~/.cargo/bin/zeroclaw-bridge`
- Copies config template to `~/.zeroclaw/bridge.toml`
- Installs systemd service to `~/.config/systemd/user/`
- Enables service for auto-start

### 3. Install MQTT Broker

**Debian/Ubuntu:**

```bash
sudo apt update
sudo apt install mosquitto mosquitto-clients
sudo systemctl enable mosquitto
sudo systemctl start mosquitto
```

**Alpine:**

```bash
sudo apk add mosquitto mosquitto-clients
sudo rc-update add mosquitto default
sudo rc-service mosquitto start
```

**macOS:**

```bash
brew install mosquitto
brew services start mosquitto
```

## Configuration

### Bridge Configuration

Edit `~/.zeroclaw/bridge.toml`:

```toml
# MQTT broker connection
mqtt_broker_url = "mqtt://localhost:1883"

# ZeroClaw gateway WebSocket endpoint
websocket_url = "ws://localhost:42617"

# Authentication token for gateway
auth_token = "your-bearer-token-here"
```

**Configuration keys:**

- `mqtt_broker_url`: MQTT broker address (format: `mqtt://host:port`)
- `websocket_url`: ZeroClaw gateway WebSocket endpoint
- `auth_token`: Bearer token for gateway authentication (obtain via pairing)

### Gateway Configuration

Ensure the gateway is configured to accept node connections. In `~/.zeroclaw/config.toml`:

```toml
[gateway]
port = 42617
host = "127.0.0.1"
require_pairing = true
```

### Mosquitto Broker Configuration

Minimal `mosquitto.conf` for local testing:

```conf
listener 1883
allow_anonymous true
```

For production, enable authentication:

```conf
listener 1883
allow_anonymous false
password_file /etc/mosquitto/passwd
```

Create password file:

```bash
sudo mosquitto_passwd -c /etc/mosquitto/passwd bridge_user
sudo systemctl restart mosquitto
```

Update bridge config:

```toml
mqtt_broker_url = "mqtt://bridge_user:password@localhost:1883"
```

## Service Management

### Start the Bridge

```bash
systemctl --user start zeroclaw-bridge
```

### Check Status

```bash
systemctl --user status zeroclaw-bridge
```

### View Logs

```bash
journalctl --user -u zeroclaw-bridge -f
```

### Stop the Bridge

```bash
systemctl --user stop zeroclaw-bridge
```

### Restart the Bridge

```bash
systemctl --user restart zeroclaw-bridge
```

### Disable Auto-Start

```bash
systemctl --user disable zeroclaw-bridge
```

## MQTT Topic Structure

The bridge uses the following topic pattern:

```
zeroclaw/nodes/{node_id}/{message_type}
```

**Message types:**

- `register`: Node advertises capabilities (Node → Gateway)
- `invoke`: Gateway requests tool execution (Gateway → Node)
- `result`: Node returns execution result (Node → Gateway)
- `heartbeat`: Node liveness signal (Node → Gateway, optional)

**QoS levels:**

- Register, invoke, result: QoS 1 (at least once delivery)
- Heartbeat: QoS 0 (at most once delivery)

For full protocol specification, see [docs/architecture/mqtt-bridge-protocol.md](../architecture/mqtt-bridge-protocol.md).

## Testing the Bridge

### 1. Verify MQTT Broker

```bash
# Subscribe to all node topics
mosquitto_sub -h localhost -t 'zeroclaw/nodes/#' -v
```

### 2. Test Node Registration

Publish a test registration message:

```bash
mosquitto_pub -h localhost -t 'zeroclaw/nodes/test-node-01/register' -m '{
  "type": "register",
  "node_id": "test-node-01",
  "capabilities": [
    {
      "name": "echo",
      "description": "Echo test capability",
      "parameters": {
        "type": "object",
        "properties": {
          "message": {"type": "string"}
        }
      }
    }
  ]
}'
```

### 3. Check Bridge Logs

```bash
journalctl --user -u zeroclaw-bridge -f
```

Expected output:

```
Bridge connected to MQTT broker
Bridge connected to WebSocket gateway
Forwarding register message from test-node-01
```

### 4. Verify Gateway Connection

Check gateway logs for node registration:

```bash
zeroclaw status
```

## Troubleshooting

### Bridge Won't Start

**Symptom:** Service fails immediately after start

**Check:**

```bash
systemctl --user status zeroclaw-bridge
journalctl --user -u zeroclaw-bridge -n 50
```

**Common causes:**

1. **Missing config file**
   - Verify `~/.zeroclaw/bridge.toml` exists
   - Run `./scripts/install-bridge.sh` to create template

2. **Invalid config syntax**
   - Check TOML syntax with `cat ~/.zeroclaw/bridge.toml`
   - Ensure URLs are quoted strings

3. **Binary not found**
   - Verify `~/.cargo/bin/zeroclaw-bridge` exists
   - Check `$PATH` includes `~/.cargo/bin`

### Cannot Connect to MQTT Broker

**Symptom:** Bridge logs show "Connection refused" or "Connection timeout"

**Check broker status:**

```bash
# systemd
sudo systemctl status mosquitto

# OpenRC
sudo rc-service mosquitto status

# Test connection
mosquitto_pub -h localhost -t test -m "hello"
```

**Solutions:**

1. **Broker not running**
   ```bash
   sudo systemctl start mosquitto
   ```

2. **Wrong broker URL**
   - Verify `mqtt_broker_url` in config
   - Default: `mqtt://localhost:1883`

3. **Firewall blocking port 1883**
   ```bash
   sudo ufw allow 1883/tcp
   ```

4. **Authentication required**
   - Add credentials to URL: `mqtt://user:pass@host:1883`

### Cannot Connect to Gateway

**Symptom:** Bridge logs show "WebSocket connection failed"

**Check gateway status:**

```bash
zeroclaw status
```

**Solutions:**

1. **Gateway not running**
   ```bash
   zeroclaw daemon
   # or
   zeroclaw service start
   ```

2. **Wrong WebSocket URL**
   - Verify `websocket_url` in config
   - Default: `ws://localhost:42617`

3. **Invalid auth token**
   - Obtain token via pairing flow
   - Update `auth_token` in config

4. **Gateway not accepting connections**
   - Check `~/.zeroclaw/config.toml` gateway section
   - Ensure `host` and `port` match bridge config

### Node Registration Not Working

**Symptom:** Node publishes register message but gateway doesn't see it

**Debug steps:**

1. **Verify MQTT message arrives at broker**
   ```bash
   mosquitto_sub -h localhost -t 'zeroclaw/nodes/#' -v
   ```

2. **Check bridge is subscribed**
   - Bridge subscribes to `zeroclaw/nodes/+/register` on startup
   - Check logs for "Subscribed to topic" messages

3. **Verify message format**
   - Must be valid JSON
   - Must include `type`, `node_id`, `capabilities` fields
   - See [mqtt-bridge-protocol.md](../architecture/mqtt-bridge-protocol.md) for schema

4. **Check bridge transformation**
   - Bridge logs should show "Forwarding register message"
   - If not, message format may be invalid

### Tool Invocation Not Reaching Node

**Symptom:** Gateway sends invoke but node never receives it

**Debug steps:**

1. **Subscribe to invoke topic**
   ```bash
   mosquitto_sub -h localhost -t 'zeroclaw/nodes/+/invoke' -v
   ```

2. **Check bridge logs**
   - Should show "Forwarding invoke message"
   - If not, WebSocket receive may be failing

3. **Verify node subscription**
   - Node must subscribe to `zeroclaw/nodes/{node_id}/invoke`
   - Check node logs for subscription confirmation

4. **Check QoS level**
   - Invoke messages use QoS 1
   - Ensure node subscribes with QoS 1

### Bridge Disconnects Frequently

**Symptom:** Bridge reconnects every few minutes

**Check logs:**

```bash
journalctl --user -u zeroclaw-bridge -f | grep -i "reconnect\|disconnect\|error"
```

**Common causes:**

1. **Network instability**
   - Check network connection
   - Increase MQTT keepalive interval

2. **Broker restarting**
   - Check broker logs: `sudo journalctl -u mosquitto -f`

3. **Gateway restarting**
   - Check gateway status: `zeroclaw status`

4. **Resource exhaustion**
   - Check system resources: `top`, `free -h`

**Solutions:**

- Bridge has automatic reconnection with exponential backoff
- Reconnection is normal for transient failures
- If reconnections are frequent (>1/minute), investigate root cause

### High Memory Usage

**Symptom:** Bridge process uses excessive memory

**Check memory:**

```bash
ps aux | grep zeroclaw-bridge
```

**Solutions:**

1. **Restart bridge**
   ```bash
   systemctl --user restart zeroclaw-bridge
   ```

2. **Check for message backlog**
   - Large number of queued messages can increase memory
   - Verify nodes are processing invocations

3. **Update to latest version**
   - Memory leaks may be fixed in newer releases

## Production Deployment

### Security Hardening

1. **Enable MQTT authentication**
   ```conf
   # mosquitto.conf
   allow_anonymous false
   password_file /etc/mosquitto/passwd
   ```

2. **Enable TLS for MQTT**
   ```conf
   listener 8883
   cafile /etc/mosquitto/ca.crt
   certfile /etc/mosquitto/server.crt
   keyfile /etc/mosquitto/server.key
   ```

   Update bridge config:
   ```toml
   mqtt_broker_url = "mqtts://localhost:8883"
   ```

3. **Use secure WebSocket (wss://)**
   - Deploy gateway behind reverse proxy with TLS
   - Update bridge config: `websocket_url = "wss://gateway.example.com"`

4. **Restrict broker access**
   ```conf
   # mosquitto ACL file
   user bridge_user
   topic readwrite zeroclaw/nodes/#
   ```

### High Availability

1. **Run multiple bridge instances**
   - Each bridge can handle different node groups
   - Use different MQTT client IDs

2. **Use clustered MQTT broker**
   - Deploy mosquitto cluster or use managed MQTT service
   - Configure bridge with cluster endpoint

3. **Monitor bridge health**
   - Use systemd service status
   - Set up alerting on service failures

### Monitoring

**Key metrics to monitor:**

- Bridge service status: `systemctl --user is-active zeroclaw-bridge`
- MQTT broker connections: `mosquitto_sub -h localhost -t '$SYS/broker/clients/connected'`
- Gateway node count: `zeroclaw status` (check registered nodes)
- Bridge logs for errors: `journalctl --user -u zeroclaw-bridge | grep -i error`

**Recommended monitoring setup:**

1. **Service health check**
   ```bash
   #!/bin/bash
   if ! systemctl --user is-active --quiet zeroclaw-bridge; then
     echo "Bridge service is down"
     systemctl --user restart zeroclaw-bridge
   fi
   ```

2. **Log monitoring**
   - Use journald or syslog forwarding
   - Alert on error patterns: "Connection refused", "Authentication failed"

## FAQ

### Q: Can I run multiple bridges?

Yes. Each bridge instance can connect to the same or different MQTT brokers and gateways. Use different systemd service names:

```bash
cp ~/.config/systemd/user/zeroclaw-bridge.service ~/.config/systemd/user/zeroclaw-bridge-2.service
# Edit service file to use different config path
systemctl --user daemon-reload
systemctl --user start zeroclaw-bridge-2
```

### Q: What happens if the bridge crashes?

The systemd service is configured with `Restart=on-failure`, so it will automatically restart after 5 seconds.

### Q: Can nodes connect directly to the gateway?

Yes. Nodes can use WebSocket directly if they have sufficient resources. The bridge is for resource-constrained devices that prefer MQTT.

### Q: How do I update the bridge?

```bash
cd /path/to/zeroclaw
git pull
cargo build --release -p zeroclaw-bridge
cp target/release/zeroclaw-bridge ~/.cargo/bin/
systemctl --user restart zeroclaw-bridge
```

### Q: Does the bridge support MQTT v5?

Currently the bridge uses MQTT v3.1.1 (rumqttc default). MQTT v5 support may be added in future releases.

### Q: Can I use a cloud MQTT broker?

Yes. Update `mqtt_broker_url` to point to your cloud broker (AWS IoT Core, HiveMQ Cloud, etc.). Ensure authentication credentials are included.

### Q: How do I get the gateway auth token?

The token is obtained through the gateway pairing flow:

1. Start gateway: `zeroclaw gateway`
2. Note the 6-digit pairing code from logs
3. Pair via API: `curl -X POST http://localhost:42617/pair -H "X-Pairing-Code: 123456"`
4. Copy the returned bearer token to bridge config

## References

- MQTT protocol specification: [docs/architecture/mqtt-bridge-protocol.md](../architecture/mqtt-bridge-protocol.md)
- Integration tests: [tests/integration/README.md](../../tests/integration/README.md)
- Gateway configuration: [docs/reference/api/config-reference.md](../reference/api/config-reference.md)
- Network deployment guide: [docs/ops/network-deployment.md](./network-deployment.md)
