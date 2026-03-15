# MQTT Bridge Deployment

## Installation

### Prerequisites

- Rust 1.70+ and Cargo
- MQTT broker (Mosquitto, EMQX, etc.)
- Network access between ZeroClaw and broker

### Build from Source

```bash
git clone https://github.com/your-org/zeroclaw-micro.git
cd zeroclaw-micro
cargo build --release --features mqtt
```

Binary location: `target/release/zeroclaw`

### Quick Start

```bash
# 1. Configure MQTT channel
cat > config.toml <<EOF
[channels.mqtt]
enabled = true
broker_url = "mqtt://localhost:1883"
client_id = "zeroclaw-agent"
topics = ["zeroclaw/commands"]
qos = 1
EOF

# 2. Start agent
./target/release/zeroclaw agent --config config.toml
```

## Configuration

### Basic Config

```toml
[channels.mqtt]
enabled = true
broker_url = "mqtt://broker.example.com:1883"
client_id = "zeroclaw-001"
topics = ["agent/commands", "agent/tasks"]
qos = 1
```

### TLS/SSL Config

```toml
[channels.mqtt]
enabled = true
broker_url = "mqtts://secure-broker.example.com:8883"
client_id = "zeroclaw-secure"
topics = ["secure/commands"]
qos = 2
tls_ca_cert = "/path/to/ca.crt"
tls_client_cert = "/path/to/client.crt"
tls_client_key = "/path/to/client.key"
```

### Authentication

```toml
[channels.mqtt]
enabled = true
broker_url = "mqtt://broker.example.com:1883"
client_id = "zeroclaw-auth"
username = "agent_user"
password = "${MQTT_PASSWORD}"  # Use env var
topics = ["private/commands"]
qos = 1
```

### Config Options Reference

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | bool | false | Enable MQTT channel |
| `broker_url` | string | required | Broker URL (mqtt:// or mqtts://) |
| `client_id` | string | required | Unique client identifier |
| `topics` | array | required | Topics to subscribe to |
| `qos` | int | 1 | Quality of Service (0, 1, or 2) |
| `username` | string | optional | Authentication username |
| `password` | string | optional | Authentication password |
| `tls_ca_cert` | string | optional | CA certificate path |
| `tls_client_cert` | string | optional | Client certificate path |
| `tls_client_key` | string | optional | Client private key path |
| `keep_alive` | int | 60 | Keep-alive interval (seconds) |
| `clean_session` | bool | true | Clean session on connect |

## Troubleshooting

### 1. Connection Refused

**Symptom**: `Error: Connection refused (os error 111)`

**Causes**:
- Broker not running
- Wrong broker URL/port
- Firewall blocking connection

**Solutions**:
```bash
# Check broker is running
systemctl status mosquitto

# Test connectivity
telnet broker.example.com 1883

# Check firewall
sudo ufw status
sudo ufw allow 1883/tcp
```

### 2. Authentication Failed

**Symptom**: `Error: MQTT connection failed: Not authorized`

**Causes**:
- Invalid username/password
- Missing credentials in config
- Broker ACL restrictions

**Solutions**:
```bash
# Verify credentials with mosquitto_pub
mosquitto_pub -h broker.example.com -u agent_user -P password -t test -m "hello"

# Check broker logs
tail -f /var/log/mosquitto/mosquitto.log

# Update config with correct credentials
export MQTT_PASSWORD="correct_password"
./zeroclaw agent --config config.toml
```

### 3. TLS Certificate Errors

**Symptom**: `Error: SSL certificate verification failed`

**Causes**:
- Invalid/expired certificate
- Wrong CA certificate
- Hostname mismatch

**Solutions**:
```bash
# Verify certificate
openssl s_client -connect broker.example.com:8883 -CAfile ca.crt

# Check certificate expiry
openssl x509 -in client.crt -noout -dates

# Test with mosquitto_sub
mosquitto_sub -h broker.example.com -p 8883 \
  --cafile ca.crt --cert client.crt --key client.key \
  -t test
```

### 4. Messages Not Received

**Symptom**: Agent running but not processing messages

**Causes**:
- Wrong topic subscription
- QoS mismatch
- Message format issues

**Solutions**:
```bash
# Monitor subscribed topics
mosquitto_sub -h broker.example.com -t "zeroclaw/#" -v

# Publish test message
mosquitto_pub -h broker.example.com -t "zeroclaw/commands" \
  -m '{"action":"test","data":"hello"}'

# Check agent logs
./zeroclaw agent --config config.toml --log-level debug

# Verify topic in config matches publisher
grep "topics" config.toml
```

### 5. High Latency / Slow Response

**Symptom**: Delayed message processing, timeouts

**Causes**:
- Network congestion
- Broker overload
- QoS 2 overhead
- Large message payloads

**Solutions**:
```bash
# Check network latency
ping broker.example.com

# Monitor broker load
mosquitto_sub -h broker.example.com -t '$SYS/broker/load/#' -v

# Reduce QoS if reliability not critical
[channels.mqtt]
qos = 0  # Fastest, no guarantees

# Split large payloads
# Instead of one 10MB message, send 10x 1MB messages

# Use local broker if possible
broker_url = "mqtt://localhost:1883"
```

### 6. Reconnection Loops

**Symptom**: Agent repeatedly connecting/disconnecting

**Causes**:
- Client ID conflict
- Broker session limits
- Network instability

**Solutions**:
```bash
# Use unique client ID
[channels.mqtt]
client_id = "zeroclaw-$(hostname)-$(date +%s)"

# Enable clean session
clean_session = true

# Increase keep-alive
keep_alive = 120

# Check broker connection limits
# In mosquitto.conf:
max_connections -1
```

### 7. Memory Leak / Resource Exhaustion

**Symptom**: Agent memory usage grows over time

**Causes**:
- Message queue buildup
- Unprocessed retained messages
- Connection leak

**Solutions**:
```bash
# Monitor memory usage
ps aux | grep zeroclaw

# Clear retained messages
mosquitto_pub -h broker.example.com -t "zeroclaw/commands" -r -n

# Restart agent periodically (systemd)
[Service]
Restart=always
RuntimeMaxSec=86400  # Restart daily

# Check for connection leaks in logs
grep "connection" zeroclaw.log | wc -l
```

## Production Deployment

### Systemd Service

```ini
[Unit]
Description=ZeroClaw MQTT Agent
After=network.target

[Service]
Type=simple
User=zeroclaw
WorkingDirectory=/opt/zeroclaw
ExecStart=/opt/zeroclaw/zeroclaw agent --config /etc/zeroclaw/config.toml
Restart=always
RestartSec=10
Environment="MQTT_PASSWORD=secret"

[Install]
WantedBy=multi-user.target
```

Enable and start:
```bash
sudo systemctl enable zeroclaw-mqtt
sudo systemctl start zeroclaw-mqtt
sudo systemctl status zeroclaw-mqtt
```

### Docker Deployment

```dockerfile
FROM rust:1.70 as builder
WORKDIR /build
COPY . .
RUN cargo build --release --features mqtt

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/zeroclaw /usr/local/bin/
COPY config.toml /etc/zeroclaw/
CMD ["zeroclaw", "agent", "--config", "/etc/zeroclaw/config.toml"]
```

Run:
```bash
docker build -t zeroclaw-mqtt .
docker run -d --name zeroclaw-agent \
  -e MQTT_PASSWORD=secret \
  -v /etc/zeroclaw:/etc/zeroclaw:ro \
  zeroclaw-mqtt
```

## Monitoring

### Health Check

```bash
# Check agent is running
ps aux | grep zeroclaw

# Check MQTT connection
mosquitto_sub -h broker.example.com -t '$SYS/broker/clients/connected' -C 1

# Test end-to-end
mosquitto_pub -h broker.example.com -t "zeroclaw/commands" \
  -m '{"action":"ping"}'
```

### Logs

```bash
# View agent logs
journalctl -u zeroclaw-mqtt -f

# Filter errors
journalctl -u zeroclaw-mqtt -p err

# Export logs
journalctl -u zeroclaw-mqtt --since "1 hour ago" > zeroclaw.log
```

## Security Best Practices

1. **Use TLS** for production deployments
2. **Rotate credentials** regularly
3. **Restrict topics** with broker ACLs
4. **Use unique client IDs** per agent
5. **Store secrets** in environment variables, not config files
6. **Enable authentication** on broker
7. **Monitor connections** for anomalies
8. **Update dependencies** regularly

## References

- [MQTT Protocol Specification](http://docs.oasis-open.org/mqtt/mqtt/v3.1.1/mqtt-v3.1.1.html)
- [Mosquitto Broker Documentation](https://mosquitto.org/documentation/)
- [ZeroClaw Configuration Reference](../reference/configuration.md)
