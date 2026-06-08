use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use futures_util::StreamExt;
use lapin::{
    Connection, ConnectionProperties,
    options::{BasicConsumeOptions, QueueBindOptions, QueueDeclareOptions},
    tcp::OwnedTLSConfig,
    types::FieldTable,
};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Generic AMQP 0-9-1 topic consumer as a chat-loop `Channel`.
///
/// Binds a queue to an exchange, consumes deliveries, and lifts each JSON
/// body into a `ChannelMessage` driving the agent loop. The body-to-message
/// mapping is config-driven so a new publisher is onboarded by configuration.
pub struct AmqpChannel {
    amqp_url: String,
    exchange: String,
    routing_keys: Vec<String>,
    queue: Option<String>,
    ca_cert: Option<PathBuf>,
    sender_label: String,
    content_template: String,
    thread_id_field: String,
    alias: String,
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

/// Construction parameters for [`AmqpChannel`].
pub struct AmqpChannelConfig {
    pub amqp_url: String,
    pub exchange: String,
    pub routing_keys: Vec<String>,
    pub queue: Option<String>,
    pub ca_cert: Option<PathBuf>,
    pub sender_label: String,
    pub content_template: String,
    pub thread_id_field: String,
    pub alias: String,
    pub peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl AmqpChannel {
    pub fn new(cfg: AmqpChannelConfig) -> Self {
        Self {
            amqp_url: cfg.amqp_url,
            exchange: cfg.exchange,
            routing_keys: cfg.routing_keys,
            queue: cfg.queue,
            ca_cert: cfg.ca_cert,
            sender_label: cfg.sender_label,
            content_template: cfg.content_template,
            thread_id_field: cfg.thread_id_field,
            alias: cfg.alias,
            peer_resolver: cfg.peer_resolver,
        }
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Lift a raw delivery body into the `(content, thread_ts)` pair the chat
    /// loop consumes, applying the config-driven mapping.
    fn map_delivery(&self, body: &[u8]) -> (String, Option<String>) {
        let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();

        let content = match &parsed {
            Some(json) if !self.content_template.is_empty() => {
                interpolate(&self.content_template, json)
            }
            _ => String::from_utf8_lossy(body).to_string(),
        };

        let thread_ts = match &parsed {
            Some(json) if !self.thread_id_field.is_empty() => {
                dotted_get(json, &self.thread_id_field).map(str_of_value)
            }
            _ => None,
        };

        (content, thread_ts)
    }

    /// Establish a lapin connection on the existing tokio runtime, declaring
    /// the executor and reactor adapters so lapin does not spin its own
    /// `async-global-executor`. A configured `ca_cert` is supplied as the
    /// custom certificate chain for `amqps://` server verification.
    async fn connect(&self) -> anyhow::Result<Connection> {
        let props = ConnectionProperties::default()
            .with_executor(tokio_executor_trait::Tokio::current())
            .with_reactor(tokio_reactor_trait::Tokio);

        let cert_chain = match &self.ca_cert {
            Some(path) => Some(std::fs::read_to_string(path)?),
            None => None,
        };

        Connection::connect_with_config(
            &self.amqp_url,
            props,
            OwnedTLSConfig {
                identity: None,
                cert_chain,
            },
        )
        .await
        .map_err(Into::into)
    }
}

impl ::zeroclaw_api::attribution::Attributable for AmqpChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Amqp)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for AmqpChannel {
    fn name(&self) -> &str {
        "amqp"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        // AMQP is consumed as an inbound trigger source; replies flow back
        // through whatever outbound channel the agent's procedure selects, not
        // by re-publishing to the broker.
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let conn = self.connect().await?;
        let channel = conn.create_channel().await?;

        let queue_name = self.queue.clone().unwrap_or_default();
        let queue = channel
            .queue_declare(
                &queue_name,
                QueueDeclareOptions {
                    exclusive: self.queue.is_none(),
                    auto_delete: self.queue.is_none(),
                    ..QueueDeclareOptions::default()
                },
                FieldTable::default(),
            )
            .await?;
        let bound_queue = queue.name().as_str().to_string();

        for routing_key in &self.routing_keys {
            channel
                .queue_bind(
                    &bound_queue,
                    &self.exchange,
                    routing_key,
                    QueueBindOptions::default(),
                    FieldTable::default(),
                )
                .await?;
        }

        let mut consumer = channel
            .basic_consume(
                &bound_queue,
                &format!("zeroclaw-{}", self.alias),
                BasicConsumeOptions {
                    no_ack: true,
                    ..BasicConsumeOptions::default()
                },
                FieldTable::default(),
            )
            .await?;

        zeroclaw_runtime::health::mark_component_ok("amqp");
        let _peers = (self.peer_resolver)();

        while let Some(delivery) = consumer.next().await {
            let Ok(delivery) = delivery else {
                continue;
            };

            let (content, thread_ts) = self.map_delivery(&delivery.data);
            let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);

            let channel_msg = ChannelMessage {
                id: format!("amqp_{}_{seq}", chrono::Utc::now().timestamp_millis()),
                sender: self.sender_label.clone(),
                reply_target: self.sender_label.clone(),
                content,
                channel: "amqp".to_string(),
                channel_alias: Some(self.alias.clone()),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                thread_ts,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            };

            if tx.send(channel_msg).await.is_err() {
                return Ok(());
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }

    fn self_handle(&self) -> Option<String> {
        Some(self.sender_label.clone())
    }
}

/// Interpolate `{field}` placeholders against a JSON body. Placeholders accept
/// dotted paths (e.g. `{project.name}`) resolved through nested objects.
/// Unmatched placeholders are left intact.
fn interpolate(template: &str, json: &serde_json::Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if let Some(close) = after.find('}') {
            let key = &after[..close];
            match dotted_get(json, key) {
                Some(value) => out.push_str(&str_of_value(value)),
                None => {
                    out.push('{');
                    out.push_str(key);
                    out.push('}');
                }
            }
            rest = &after[close + 1..];
        } else {
            out.push_str(&rest[open..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Resolve a dotted path (e.g. `message.project.name`) into a JSON value.
fn dotted_get<'a>(json: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cursor = json;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    Some(cursor)
}

/// Render a JSON scalar without the quoting `to_string` would add to strings.
fn str_of_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_with(content_template: &str, thread_id_field: &str) -> AmqpChannel {
        AmqpChannel::new(AmqpChannelConfig {
            amqp_url: "amqp://localhost:5672".into(),
            exchange: "amq.topic".into(),
            routing_keys: vec!["org.release-monitoring.prod.anitya.project.version.update".into()],
            queue: None,
            ca_cert: None,
            sender_label: "anitya".into(),
            content_template: content_template.into(),
            thread_id_field: thread_id_field.into(),
            alias: "stagex".into(),
            peer_resolver: Arc::new(Vec::new),
        })
    }

    #[test]
    fn name_is_amqp() {
        assert_eq!(channel_with("", "").name(), "amqp");
    }

    #[test]
    fn self_handle_returns_sender_label() {
        assert_eq!(
            channel_with("", "").self_handle(),
            Some("anitya".to_string())
        );
    }

    #[test]
    fn map_delivery_interpolates_template() {
        let ch = channel_with("New release: {name} {version} (package: {package})", "name");
        let body = br#"{"name":"curl","version":"8.9.1","package":"stagex/curl"}"#;
        let (content, thread_ts) = ch.map_delivery(body);
        assert_eq!(content, "New release: curl 8.9.1 (package: stagex/curl)");
        assert_eq!(thread_ts, Some("curl".to_string()));
    }

    #[test]
    fn map_delivery_extracts_dotted_thread_id() {
        let ch = channel_with("{version}", "project.name");
        let body = br#"{"version":"8.9.1","project":{"name":"curl"}}"#;
        let (content, thread_ts) = ch.map_delivery(body);
        assert_eq!(content, "8.9.1");
        assert_eq!(thread_ts, Some("curl".to_string()));
    }

    #[test]
    fn map_delivery_falls_back_to_raw_body_without_template() {
        let ch = channel_with("", "");
        let (content, thread_ts) = ch.map_delivery(b"plain text payload");
        assert_eq!(content, "plain text payload");
        assert_eq!(thread_ts, None);
    }

    #[test]
    fn interpolate_leaves_unknown_placeholders_intact() {
        let json = serde_json::json!({"a": "x"});
        assert_eq!(interpolate("{a} {missing}", &json), "x {missing}");
    }

    #[test]
    fn interpolate_resolves_dotted_paths() {
        let json = serde_json::json!({
            "project": {"name": "curl", "version": "8.9.1"},
            "old_version": "8.8.0"
        });
        assert_eq!(
            interpolate(
                "New release: {project.name} {project.version} (was {old_version})",
                &json
            ),
            "New release: curl 8.9.1 (was 8.8.0)"
        );
    }

    #[test]
    fn dotted_get_returns_none_for_missing_path() {
        let json = serde_json::json!({"a": {"b": 1}});
        assert!(dotted_get(&json, "a.c").is_none());
        assert_eq!(dotted_get(&json, "a.b").map(str_of_value), Some("1".into()));
    }
}
