//! MQTT Channel implementation for ZeroClaw.
//!
//! This module provides a full-duplex MQTT channel that implements the `Channel` trait.
//! It connects to an MQTT broker, subscribes to configured topics, and publishes
//! responses back to dynamically computed reply topics.
//!
//! # Architecture
//!
//! - Uses `rumqttc` for async MQTT 3.1.1/5.0 protocol support
//! - Supports TLS via `mqtts://` URLs with rustls
//! - Implements topic-based routing with wildcard support (+, #)
//! - Per-sender response topic templating via `{{sender}}` placeholder
//! - Sender allowlist for access control
//!
//! # Example Configuration
//!
//! ```toml
//! [channels_config.mqtt]
//! broker_url = "mqtt://broker.example.com:1883"
//! client_id = "zeroclaw_agent"
//! topics = ["commands/zeroclaw/#", "sensors/+/alert"]
//! qos = 1
//! response_topic = "responses/{{sender}}"
//! allowed_senders = ["*"]
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::MqttChannelConfig;

/// Monotonic counter for unique message IDs.
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// MQTT channel implementing the `Channel` trait.
///
/// Provides bidirectional messaging over MQTT pub/sub protocol.
pub struct MqttChannel {
    config: MqttChannelConfig,
    /// Shared MQTT client for publishing responses.
    client: Arc<Mutex<Option<AsyncClient>>>,
}

impl std::fmt::Debug for MqttChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MqttChannel")
            .field("broker_url", &self.config.broker_url)
            .field("client_id", &self.config.client_id)
            .field("topics", &self.config.topics)
            .finish_non_exhaustive()
    }
}

impl MqttChannel {
    /// Create a new MQTT channel from configuration.
    pub fn new(config: MqttChannelConfig) -> Self {
        Self {
            config,
            client: Arc::new(Mutex::new(None)),
        }
    }

    /// Check if a sender is allowed based on the allowlist.
    fn is_sender_allowed(&self, sender: &str) -> bool {
        if self.config.allowed_senders.is_empty() {
            return false; // Deny by default when empty
        }
        if self.config.allowed_senders.iter().any(|s| s == "*") {
            return true; // Wildcard allows all
        }
        self.config
            .allowed_senders
            .iter()
            .any(|s| s.eq_ignore_ascii_case(sender))
    }

    /// Compute the response topic for a given sender.
    ///
    /// Uses the configured `response_topic` template with `{{sender}}` placeholder,
    /// or falls back to `responses/<sender>`.
    fn response_topic_for(&self, sender: &str) -> String {
        if let Some(ref template) = self.config.response_topic {
            template.replace("{{sender}}", sender)
        } else {
            format!("responses/{sender}")
        }
    }

    /// Build MQTT options from configuration.
    fn build_mqtt_options(&self) -> anyhow::Result<MqttOptions> {
        let host = broker_host(&self.config.broker_url);
        let port = broker_port(&self.config.broker_url);

        let mut options = MqttOptions::new(&self.config.client_id, host, port);
        options.set_keep_alive(std::time::Duration::from_secs(self.config.keep_alive_secs));

        if let (Some(ref user), Some(ref pass)) = (&self.config.username, &self.config.password) {
            options.set_credentials(user, pass);
        }

        if self.config.use_tls {
            options.set_transport(Transport::tls_with_default_config());
        }

        Ok(options)
    }
}

#[async_trait]
impl Channel for MqttChannel {
    fn name(&self) -> &str {
        "mqtt"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let guard = self.client.lock().await;
        let client = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MQTT client not connected"))?;

        let qos = qos_from_u8(self.config.qos);
        client
            .publish(&message.recipient, qos, false, message.content.as_bytes())
            .await?;

        debug!(
            "MQTT: published to {} ({} bytes)",
            message.recipient,
            message.content.len()
        );
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let options = self.build_mqtt_options()?;
        let (client, mut eventloop) = AsyncClient::new(options, 64);

        // Store client for send()
        {
            let mut guard = self.client.lock().await;
            *guard = Some(client.clone());
        }

        let qos = qos_from_u8(self.config.qos);

        // Subscribe to all configured topics
        for topic in &self.config.topics {
            client.subscribe(topic, qos).await?;
            info!("MQTT channel: subscribed to '{topic}'");
        }

        crate::health::mark_component_ok("mqtt_channel");

        info!(
            "MQTT channel connected to {} as {}",
            self.config.broker_url, self.config.client_id
        );

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(msg))) => {
                    let topic = msg.topic.clone();
                    let payload = String::from_utf8_lossy(&msg.payload).to_string();

                    // Extract sender from topic (last segment) or use topic as sender
                    let sender = extract_sender_from_topic(&topic);

                    // Check allowlist
                    if !self.is_sender_allowed(&sender) {
                        debug!("MQTT: ignoring message from unauthorized sender: {sender}");
                        continue;
                    }

                    // Compute response topic
                    let reply_target = self.response_topic_for(&sender);

                    let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
                    let channel_msg = ChannelMessage {
                        id: format!("mqtt_{}_{seq}", chrono::Utc::now().timestamp_millis()),
                        sender: sender.clone(),
                        reply_target,
                        content: payload,
                        channel: "mqtt".to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                    };

                    if tx.send(channel_msg).await.is_err() {
                        info!("MQTT channel: receiver dropped, stopping listener");
                        return Ok(());
                    }
                }
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    crate::health::mark_component_ok("mqtt_channel");
                    info!("MQTT channel: connected to broker");
                }
                Ok(_) => {
                    // Other events (PingResp, SubAck, etc.) — ignore
                }
                Err(e) => {
                    crate::health::mark_component_error("mqtt_channel", e.to_string());
                    warn!("MQTT channel: connection error: {e}");
                    // rumqttc handles auto-reconnect; loop continues
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        // Lightweight check: can we build options and is client present?
        if self.build_mqtt_options().is_err() {
            return false;
        }
        let guard = self.client.lock().await;
        guard.is_some()
    }
}

/// Extract sender from topic path.
///
/// Uses the last non-wildcard segment as the sender identifier.
/// Examples:
/// - `commands/zeroclaw/alice` -> `alice`
/// - `sensors/device123/alert` -> `alert` (fallback to last segment)
/// - `chat/bob` -> `bob`
fn extract_sender_from_topic(topic: &str) -> String {
    topic
        .rsplit('/')
        .find(|s| !s.is_empty() && *s != "+" && *s != "#")
        .unwrap_or("unknown")
        .to_string()
}

/// Convert QoS u8 to rumqttc QoS enum.
fn qos_from_u8(qos: u8) -> QoS {
    match qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

/// Extract host from broker URL.
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

/// Extract port from broker URL, with defaults for mqtt:// and mqtts://.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> MqttChannelConfig {
        MqttChannelConfig {
            broker_url: "mqtt://localhost:1883".into(),
            client_id: "test_client".into(),
            topics: vec!["test/#".into()],
            qos: 1,
            username: None,
            password: None,
            use_tls: false,
            keep_alive_secs: 30,
            response_topic: Some("replies/{{sender}}".into()),
            allowed_senders: vec!["*".into()],
        }
    }

    // ── Config validation tests ─────────────────────────────

    #[test]
    fn mqtt_config_validation_accepts_valid() {
        let config = test_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn mqtt_config_validation_rejects_bad_qos() {
        let mut config = test_config();
        config.qos = 3;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("qos must be 0, 1, or 2"));
    }

    #[test]
    fn mqtt_config_validation_rejects_bad_url() {
        let mut config = test_config();
        config.broker_url = "http://localhost:1883".into();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtt://"));
    }

    #[test]
    fn mqtt_config_validation_rejects_empty_topics() {
        let mut config = test_config();
        config.topics = vec![];
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("at least one topic"));
    }

    #[test]
    fn mqtt_config_validation_rejects_empty_client_id() {
        let mut config = test_config();
        config.client_id = String::new();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("client_id must not be empty"));
    }

    #[test]
    fn mqtt_tls_consistency_mqtts_requires_use_tls() {
        let mut config = test_config();
        config.broker_url = "mqtts://localhost:8883".into();
        config.use_tls = false;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("mqtts://"));
    }

    #[test]
    fn mqtt_tls_consistency_mqtt_rejects_use_tls() {
        let mut config = test_config();
        config.use_tls = true;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("use_tls is true"));
    }

    // ── Sender allowlist tests ──────────────────────────────

    #[test]
    fn sender_allowlist_wildcard_allows_all() {
        let config = MqttChannelConfig {
            allowed_senders: vec!["*".into()],
            ..test_config()
        };
        let channel = MqttChannel::new(config);
        assert!(channel.is_sender_allowed("anyone"));
        assert!(channel.is_sender_allowed("alice"));
    }

    #[test]
    fn sender_allowlist_specific_users() {
        let config = MqttChannelConfig {
            allowed_senders: vec!["alice".into(), "bob".into()],
            ..test_config()
        };
        let channel = MqttChannel::new(config);
        assert!(channel.is_sender_allowed("alice"));
        assert!(channel.is_sender_allowed("bob"));
        assert!(!channel.is_sender_allowed("eve"));
    }

    #[test]
    fn sender_allowlist_empty_denies_all() {
        let config = MqttChannelConfig {
            allowed_senders: vec![],
            ..test_config()
        };
        let channel = MqttChannel::new(config);
        assert!(!channel.is_sender_allowed("anyone"));
    }

    #[test]
    fn sender_allowlist_case_insensitive() {
        let config = MqttChannelConfig {
            allowed_senders: vec!["Alice".into()],
            ..test_config()
        };
        let channel = MqttChannel::new(config);
        assert!(channel.is_sender_allowed("alice"));
        assert!(channel.is_sender_allowed("ALICE"));
    }

    // ── Response topic tests ────────────────────────────────

    #[test]
    fn response_topic_template() {
        let config = MqttChannelConfig {
            response_topic: Some("responses/{{sender}}/reply".into()),
            ..test_config()
        };
        let channel = MqttChannel::new(config);
        assert_eq!(channel.response_topic_for("alice"), "responses/alice/reply");
    }

    #[test]
    fn response_topic_default() {
        let config = MqttChannelConfig {
            response_topic: None,
            ..test_config()
        };
        let channel = MqttChannel::new(config);
        assert_eq!(channel.response_topic_for("bob"), "responses/bob");
    }

    // ── Topic parsing tests ─────────────────────────────────

    #[test]
    fn extract_sender_from_topic_simple() {
        assert_eq!(extract_sender_from_topic("chat/alice"), "alice");
    }

    #[test]
    fn extract_sender_from_topic_nested() {
        assert_eq!(extract_sender_from_topic("commands/zeroclaw/bob"), "bob");
    }

    #[test]
    fn extract_sender_from_topic_wildcard() {
        // Wildcards should be filtered out
        assert_eq!(extract_sender_from_topic("sensors/+/alert"), "alert");
    }

    #[test]
    fn extract_sender_from_topic_single() {
        assert_eq!(extract_sender_from_topic("topic"), "topic");
    }

    // ── URL parsing tests ───────────────────────────────────

    #[test]
    fn broker_host_extraction() {
        assert_eq!(broker_host("mqtt://localhost:1883"), "localhost");
        assert_eq!(
            broker_host("mqtts://broker.example.com:8883"),
            "broker.example.com"
        );
    }

    #[test]
    fn broker_port_extraction() {
        assert_eq!(broker_port("mqtt://localhost:1883"), 1883);
        assert_eq!(broker_port("mqtts://broker:8883"), 8883);
    }

    #[test]
    fn broker_port_defaults() {
        assert_eq!(broker_port("mqtt://localhost"), 1883);
        assert_eq!(broker_port("mqtts://broker"), 8883);
    }

    // ── QoS conversion tests ────────────────────────────────

    #[test]
    fn qos_conversion() {
        assert!(matches!(qos_from_u8(0), QoS::AtMostOnce));
        assert!(matches!(qos_from_u8(1), QoS::AtLeastOnce));
        assert!(matches!(qos_from_u8(2), QoS::ExactlyOnce));
        // >2 maps to ExactlyOnce
        assert!(matches!(qos_from_u8(3), QoS::ExactlyOnce));
    }

    // ── Channel trait tests ─────────────────────────────────

    #[test]
    fn channel_name() {
        let channel = MqttChannel::new(test_config());
        assert_eq!(channel.name(), "mqtt");
    }
}
