//! MQTT publish tool for agent-initiated message publishing.
//!
//! This tool enables the agent to publish messages to MQTT topics,
//! facilitating leader/follower patterns where a leader ZeroClaw instance
//! can route commands to followers behind NAT/firewalls.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

/// Default MQTT connection timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 10;
/// Default MQTT QoS level (AtLeastOnce).
const DEFAULT_QOS: u8 = 1;
/// Default MQTT keep-alive interval in seconds.
const DEFAULT_KEEP_ALIVE_SECS: u64 = 30;

/// MQTT publish tool for one-shot message publishing.
///
/// Creates a temporary MQTT client connection for each publish operation.
/// This is suitable for low-frequency publishing; for high-throughput scenarios,
/// consider a persistent connection pool (not implemented).
pub struct MqttPublishTool {
    /// Optional default broker URL. If set,工具参数中的 broker_url 可选.
    default_broker_url: Option<String>,
    /// Optional default username for authentication.
    default_username: Option<String>,
    /// Optional default password for authentication.
    default_password: Option<String>,
}

impl MqttPublishTool {
    /// Create a new MQTT publish tool with optional defaults.
    pub fn new(
        default_broker_url: Option<String>,
        default_username: Option<String>,
        default_password: Option<String>,
    ) -> Self {
        Self {
            default_broker_url,
            default_username,
            default_password,
        }
    }

    /// Create a new MQTT publish tool with no defaults (all parameters required).
    pub fn new_no_defaults() -> Self {
        Self {
            default_broker_url: None,
            default_username: None,
            default_password: None,
        }
    }

    /// Extract host from broker URL like "mqtt://host:port" or "mqtts://host:port".
    fn broker_host(url: &str) -> String {
        let without_scheme = url
            .strip_prefix("mqtt://")
            .or_else(|| url.strip_prefix("mqtts://"))
            .unwrap_or(url);
        without_scheme
            .split(':')
            .next()
            .unwrap_or("localhost")
            .to_string()
    }

    /// Extract port from broker URL, defaulting to 1883 for mqtt:// and 8883 for mqtts://.
    fn broker_port(url: &str) -> u16 {
        let is_tls = url.starts_with("mqtts://");
        let without_scheme = url
            .strip_prefix("mqtt://")
            .or_else(|| url.strip_prefix("mqtts://"))
            .unwrap_or(url);
        let default_port: u16 = if is_tls { 8883 } else { 1883 };
        without_scheme
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port)
    }

    /// Generate a unique client ID for this publish operation.
    fn client_id() -> String {
        format!("zeroclaw-publish-{}", uuid::Uuid::new_v4().simple())
    }
}

#[async_trait]
impl Tool for MqttPublishTool {
    fn name(&self) -> &str {
        "mqtt_publish"
    }

    fn description(&self) -> &str {
        "Publish a message to an MQTT topic. Use this tool to send commands to other systems \
        via MQTT message brokers. Common use cases include sending commands to IoT devices, \
        triggering workflows on remote systems, or implementing leader/follower patterns \
        where followers subscribe to specific topics for their commands."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "broker_url": {
                    "type": "string",
                    "description": "MQTT broker URL (e.g., mqtt://localhost:1883 or mqtts://broker.example.com:8883)"
                },
                "topic": {
                    "type": "string",
                    "description": "MQTT topic to publish to (e.g., cmd/machine-a or sensors/temperature)"
                },
                "message": {
                    "type": "string",
                    "description": "Message payload to publish (will be sent as UTF-8 string)"
                },
                "qos": {
                    "type": "integer",
                    "description": "MQTT QoS level: 0=AtMostOnce, 1=AtLeastOnce, 2=ExactlyOnce",
                    "enum": [0, 1, 2],
                    "default": 1
                },
                "retain": {
                    "type": "boolean",
                    "description": "Whether the broker should retain this message for new subscribers",
                    "default": false
                },
                "username": {
                    "type": "string",
                    "description": "Optional username for broker authentication"
                },
                "password": {
                    "type": "string",
                    "description": "Optional password for broker authentication"
                }
            },
            "required": ["broker_url", "topic", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Extract and validate parameters
        let broker_url = args
            .get("broker_url")
            .and_then(|v| v.as_str())
            .or_else(|| self.default_broker_url.as_deref())
            .ok_or_else(|| anyhow::anyhow!("Missing 'broker_url' parameter and no default configured"))?;

        let topic = args
            .get("topic")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'topic' parameter"))?;

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;

        let qos = args
            .get("qos")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_QOS as u64) as u8;

        let retain = args.get("retain").and_then(|v| v.as_bool()).unwrap_or(false);

        let username = args
            .get("username")
            .and_then(|v| v.as_str())
            .or_else(|| self.default_username.as_deref())
            .map(String::from);

        let password = args
            .get("password")
            .and_then(|v| v.as_str())
            .or_else(|| self.default_password.as_deref())
            .map(String::from);

        // Validate broker URL format
        if !broker_url.starts_with("mqtt://") && !broker_url.starts_with("mqtts://") {
            anyhow::bail!("Invalid broker URL: must start with mqtt:// or mqtts://");
        }

        // Validate topic (MQTT topic rules)
        if topic.is_empty() {
            anyhow::bail!("Topic cannot be empty");
        }
        if topic.contains('+') || topic.contains('#') {
            // Wildcards are only valid for subscriptions, not publishing
            anyhow::bail!("Topic cannot contain wildcard characters (+ or #)");
        }
        if topic.len() > 65535 {
            anyhow::bail!("Topic too long (max 65535 characters)");
        }

        // Validate QoS
        if qos > 2 {
            anyhow::bail!("Invalid QoS level: must be 0, 1, or 2");
        }

        // Convert QoS to rumqttc QoS
        let qos: rumqttc::QoS = match qos {
            0 => rumqttc::QoS::AtMostOnce,
            1 => rumqttc::QoS::AtLeastOnce,
            2 => rumqttc::QoS::ExactlyOnce,
            _ => unreachable!(), // qos is validated above to be 0-2
        };

        // Build MQTT options
        let client_id = Self::client_id();
        let host = Self::broker_host(broker_url);
        let port = Self::broker_port(broker_url);

        let mut mqtt_options = rumqttc::MqttOptions::new(&client_id, host, port);
        mqtt_options.set_keep_alive(Duration::from_secs(DEFAULT_KEEP_ALIVE_SECS));

        // Set credentials if provided
        if let (Some(user), Some(pass)) = (&username, &password) {
            mqtt_options.set_credentials(user, pass);
        }

        // Configure TLS for mqtts:// URLs
        if broker_url.starts_with("mqtts://") {
            mqtt_options.set_transport(rumqttc::Transport::tls_with_default_config());
        }

        // Create client and publish
        let (client, mut connection) = rumqttc::AsyncClient::new(mqtt_options, 10);

        // Publish message
        client
            .publish(topic, qos, retain, message.as_bytes())
            .await?;

        // Wait for acknowledgement with timeout
        let timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            match connection.poll().await {
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::PubAck(_) | rumqttc::Packet::PubComp(_))) => {
                    // QoS 1 acknowledgement or QoS 2 completion received
                    return Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Published to topic '{}': {} bytes",
                            topic,
                            message.len()
                        ),
                        error: None,
                    });
                }
                Ok(rumqttc::Event::Incoming(_)) => {
                    // Other events - continue polling
                }
                Ok(rumqttc::Event::Outgoing(_)) => {
                    // Outgoing events - continue polling
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: format!("MQTT error: {}", e),
                        error: Some(format!("MQTT error: {}", e)),
                    });
                }
            }
        }

        // Timeout - message may have been sent but not acknowledged
        // For QoS 0, this is expected (no acknowledgement)
        if qos == rumqttc::QoS::AtMostOnce {
            Ok(ToolResult {
                success: true,
                output: format!(
                    "Published to topic '{}' (QoS 0 - no acknowledgement): {} bytes",
                    topic,
                    message.len()
                ),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: format!(
                    "Timeout waiting for MQTT acknowledgement for topic '{}'",
                    topic
                ),
                error: Some("Timeout waiting for acknowledgement".to_string()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mqtt_publish_tool_name() {
        let tool = MqttPublishTool::new_no_defaults();
        assert_eq!(tool.name(), "mqtt_publish");
    }

    #[test]
    fn mqtt_publish_tool_description() {
        let tool = MqttPublishTool::new_no_defaults();
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("MQTT"));
    }

    #[test]
    fn broker_host_extracts_host() {
        assert_eq!(MqttPublishTool::broker_host("mqtt://myhost:1883"), "myhost");
        assert_eq!(
            MqttPublishTool::broker_host("mqtts://secure.example.com:8883"),
            "secure.example.com"
        );
        assert_eq!(MqttPublishTool::broker_host("mqtt://localhost"), "localhost");
    }

    #[test]
    fn broker_port_extracts_port() {
        assert_eq!(MqttPublishTool::broker_port("mqtt://localhost:1883"), 1883);
        assert_eq!(MqttPublishTool::broker_port("mqtts://host:8883"), 8883);
    }

    #[test]
    fn broker_port_defaults() {
        assert_eq!(MqttPublishTool::broker_port("mqtt://localhost"), 1883);
        assert_eq!(MqttPublishTool::broker_port("mqtts://host"), 8883);
    }

    #[test]
    fn client_id_is_unique() {
        let id1 = MqttPublishTool::client_id();
        let id2 = MqttPublishTool::client_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("zeroclaw-publish-"));
    }

    #[test]
    fn parameters_schema_includes_required_fields() {
        let tool = MqttPublishTool::new_no_defaults();
        let schema = tool.parameters_schema();

        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("broker_url"));
        assert!(props.contains_key("topic"));
        assert!(props.contains_key("message"));

        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 3);
        assert!(required.iter().any(|v| v == "broker_url"));
        assert!(required.iter().any(|v| v == "topic"));
        assert!(required.iter().any(|v| v == "message"));
    }

    #[test]
    fn tool_with_defaults() {
        let tool = MqttPublishTool::new(
            Some("mqtt://localhost:1883".to_string()),
            Some("user".to_string()),
            Some("pass".to_string()),
        );
        assert_eq!(tool.default_broker_url, Some("mqtt://localhost:1883".to_string()));
        assert_eq!(tool.default_username, Some("user".to_string()));
        assert_eq!(tool.default_password, Some("pass".to_string()));
    }
}
