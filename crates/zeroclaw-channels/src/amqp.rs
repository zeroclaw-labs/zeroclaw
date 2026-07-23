use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use futures_util::StreamExt;
use lapin::{
    Connection, ConnectionProperties,
    options::{
        BasicAckOptions, BasicConsumeOptions, BasicNackOptions, QueueBindOptions,
        QueueDeclareOptions,
    },
    tcp::{OwnedIdentity, OwnedTLSConfig},
    types::FieldTable,
};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::SopDispatch;
use zeroclaw_runtime::sop::audit::SopAuditLogger;
use zeroclaw_runtime::sop::dispatch::{dispatch_untrusted_fan_in, results_need_redelivery};
use zeroclaw_runtime::sop::engine::SopEngine;
use zeroclaw_runtime::sop::types::SopTriggerSource;

static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct AmqpChannel {
    amqp_url: String,
    exchange: String,
    routing_keys: Vec<String>,
    queue: Option<String>,
    ca_cert: Option<PathBuf>,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    sender_label: String,
    content_template: String,
    thread_id_field: String,
    durable_ack: bool,
    dispatch: SopDispatch,
    engine: Option<Arc<Mutex<SopEngine>>>,
    audit: Option<Arc<SopAuditLogger>>,
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
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub sender_label: String,
    pub content_template: String,
    pub thread_id_field: String,
    pub durable_ack: bool,
    pub dispatch: SopDispatch,
    pub engine: Option<Arc<Mutex<SopEngine>>>,
    pub audit: Option<Arc<SopAuditLogger>>,
    pub alias: String,
    pub peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryOutcome {
    Processed,
    /// A matched SOP was backpressured (exec slot or pending-approval pool full)
    /// in SOP-only dispatch mode. The delivery was NOT fully handled: under
    /// `durable_ack` it must be nack/requeued for redelivery rather than acked,
    /// so the trigger is retried once capacity frees instead of being lost. Never
    /// returned for combined `sop_and_agent_loop` dispatch - there, the agent loop
    /// already consumed the delivery, so a SOP-side overflow is surfaced loudly
    /// and acked rather than risking a broker redelivery that would double-run
    /// the agent side (see `route_delivery`).
    Deferred,
    ReceiverGone,
}

impl AmqpChannel {
    pub fn new(cfg: AmqpChannelConfig) -> anyhow::Result<Self> {
        let routes_sop = matches!(
            cfg.dispatch,
            SopDispatch::Sop | SopDispatch::SopAndAgentLoop
        );
        if routes_sop && (cfg.engine.is_none() || cfg.audit.is_none()) {
            anyhow::bail!(
                "amqp.{}: dispatch = {:?} routes to the SOP engine but no SOP \
                 engine/audit handles are available; refusing to start a \
                 channel that would acknowledge deliveries without dispatching \
                 them",
                cfg.alias,
                cfg.dispatch
            );
        }
        Ok(Self {
            amqp_url: cfg.amqp_url,
            exchange: cfg.exchange,
            routing_keys: cfg.routing_keys,
            queue: cfg.queue,
            ca_cert: cfg.ca_cert,
            client_cert: cfg.client_cert,
            client_key: cfg.client_key,
            sender_label: cfg.sender_label,
            content_template: cfg.content_template,
            thread_id_field: cfg.thread_id_field,
            durable_ack: cfg.durable_ack,
            dispatch: cfg.dispatch,
            engine: cfg.engine,
            audit: cfg.audit,
            alias: cfg.alias,
            peer_resolver: cfg.peer_resolver,
        })
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

    async fn route_delivery(
        &self,
        routing_key: &str,
        data: &[u8],
        // The delivery's AMQP `message_id` (replayed unchanged on a redelivery), used as
        // the per-message SOP idempotency key. `None` when the publisher set none.
        message_id: Option<&str>,
        // The broker's `redelivered` flag: only a confirmed redelivery coalesces, so a
        // FRESH delivery reusing a message-id is never coalesced/ACKed away.
        redelivered: bool,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> DeliveryOutcome {
        let routes_sop = matches!(
            self.dispatch,
            SopDispatch::Sop | SopDispatch::SopAndAgentLoop
        );
        let routes_agent = matches!(
            self.dispatch,
            SopDispatch::AgentLoop | SopDispatch::SopAndAgentLoop
        );

        if routes_agent {
            let (content, thread_ts) = self.map_delivery(data);
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

                ..Default::default()
            };

            if tx.send(channel_msg).await.is_err() {
                return DeliveryOutcome::ReceiverGone;
            }
        }

        if routes_sop && let (Some(engine), Some(audit)) = (&self.engine, &self.audit) {
            // A2 per-message idempotency: key on the delivery's `message_id` - the only
            // cross-redelivery-stable identity the broker exposes - SCOPED to this channel
            // alias (so two aliases reusing an id do not collide). Only a CONFIRMED
            // redelivery (`redelivered`) coalesces (a redelivery of the same message, e.g.
            // one requeued because a sibling SOP deferred); a FRESH delivery always
            // dispatches, and a fresh reuse of a live key marks it ambiguous downstream. A
            // delivery with NO / a BLANK message-id is not deduplicated at all (we never
            // ACK a message away on a guess).
            //
            // Best-effort under the AMQP unique-`message_id` contract: publishers MUST set
            // a unique, stable `message_id` per logical message for exactly-once. Under id
            // REUSE (a contract violation) a redelivery of a reused id can still coalesce a
            // distinct trigger - an accepted, documented at-most-once edge. See the full
            // contract on `SopEngine::dispatch_dedup` (sop/engine.rs).
            let dedup = message_id
                .filter(|id| !id.is_empty())
                .map(|id| (format!("amqp:{}:{id}", self.alias), redelivered));
            let results = dispatch_untrusted_fan_in(
                engine,
                audit,
                SopTriggerSource::Amqp,
                Some(routing_key),
                Some(&String::from_utf8_lossy(data)),
                dedup,
            )
            .await;
            if results_need_redelivery(&results) {
                if routes_agent {
                    // Combined `sop_and_agent_loop`: the agent loop ALREADY accepted
                    // this delivery above (`tx.send` succeeded). Requeuing to retry
                    // the SOP side would redeliver the SAME broker message to the
                    // agent loop and double-run its side effects. Without a durable
                    // intake queue (a separate follow-up) there is nothing to
                    // durably re-home the backpressured trigger into, so in combined
                    // mode a SOP-side overflow is a genuine, surfaced-loudly drop
                    // rather than a broker-level redelivery: the broker-redelivery
                    // safety net is reserved for SOP-only dispatch, where there is no
                    // agent handoff to double-run.
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "routing_key": routing_key,
                                "dispatch": "sop_and_agent_loop",
                            })),
                        "amqp: SOP admission backpressured in combined mode; trigger dropped rather \
                         than requeued (the agent side already consumed this delivery and must not \
                         be redelivered)"
                    );
                } else {
                    // SOP-only mode: no agent handoff happened, so signal the
                    // listener to requeue (not ack) so the trigger is redelivered
                    // when capacity frees.
                    return DeliveryOutcome::Deferred;
                }
            }
        }

        DeliveryOutcome::Processed
    }

    async fn connect(&self) -> anyhow::Result<Connection> {
        let props = amqp_connection_properties();

        let cert_chain = match &self.ca_cert {
            Some(path) => Some(std::fs::read_to_string(path)?),
            None => None,
        };

        let identity = self.build_client_identity()?;

        Connection::connect_with_config(
            &self.amqp_url,
            props,
            OwnedTLSConfig {
                identity,
                cert_chain,
            },
        )
        .await
        .map_err(Into::into)
    }

    fn build_client_identity(&self) -> anyhow::Result<Option<OwnedIdentity>> {
        let (cert_path, key_path) = match (&self.client_cert, &self.client_key) {
            (Some(cert), Some(key)) => (cert, key),
            (None, None) => return Ok(None),
            (Some(_), None) => {
                anyhow::bail!(
                    "amqp channel '{}': client_cert is set but client_key is missing",
                    self.alias
                )
            }
            (None, Some(_)) => {
                anyhow::bail!(
                    "amqp channel '{}': client_key is set but client_cert is missing",
                    self.alias
                )
            }
        };

        let cert_pem = std::fs::read(cert_path)?;
        let key_pem = std::fs::read(key_path)?;
        let der = pem_to_pkcs12_der(&cert_pem, &key_pem, &self.alias)?;

        Ok(Some(OwnedIdentity {
            der,
            password: PKCS12_PASSWORD.to_string(),
        }))
    }

    async fn establish_consumer(&self) -> anyhow::Result<(Connection, lapin::Consumer)> {
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

        let consumer = channel
            .basic_consume(
                &bound_queue,
                &format!("zeroclaw-{}", self.alias),
                BasicConsumeOptions {
                    no_ack: !self.durable_ack,
                    ..BasicConsumeOptions::default()
                },
                FieldTable::default(),
            )
            .await?;

        Ok((conn, consumer))
    }
}

fn amqp_connection_properties() -> ConnectionProperties {
    let props =
        ConnectionProperties::default().with_executor(tokio_executor_trait::Tokio::current());

    #[cfg(unix)]
    {
        props.with_reactor(tokio_reactor_trait::Tokio)
    }

    #[cfg(not(unix))]
    {
        props.with_reactor(async_reactor_trait::AsyncIo)
    }
}

/// Ephemeral password protecting the in-memory PKCSidentity. The bundle is
/// built and consumed within a single connect call and never persisted, so the
/// password only has to round-trip through tcp-stream's PKCSreader.
const PKCS12_PASSWORD: &str = "zeroclaw-amqp";

/// Convert a PEM client certificate chain and private key into a PKCSDER
/// bundle suitable for tcp-stream's rustls client-auth path.
fn pem_to_pkcs12_der(cert_pem: &[u8], key_pem: &[u8], alias: &str) -> anyhow::Result<Vec<u8>> {
    use p12_keystore::{Certificate, KeyStore, KeyStoreEntry, PrivateKeyChain};

    let certs: Vec<Vec<u8>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|c| c.as_ref().to_vec())
        .collect();
    if certs.is_empty() {
        anyhow::bail!("amqp channel '{alias}': client_cert contains no certificates");
    }

    let key = rustls_pemfile::private_key(&mut &key_pem[..])?.ok_or_else(|| {
        anyhow::Error::msg(format!(
            "amqp channel '{alias}': client_key contains no private key"
        ))
    })?;

    let chain: Vec<Certificate> = certs
        .iter()
        .map(|der| Certificate::from_der(der))
        .collect::<Result<_, _>>()
        .map_err(|e| {
            anyhow::Error::msg(format!(
                "amqp channel '{alias}': invalid client certificate: {e}"
            ))
        })?;

    // local_key_id ties the private key to its leaf cert inside the bundle.
    let local_key_id = b"zeroclaw-amqp-client";
    let key_chain = PrivateKeyChain::new(key.secret_der(), local_key_id, chain);

    let mut keystore = KeyStore::new();
    keystore.add_entry(alias, KeyStoreEntry::PrivateKeyChain(key_chain));

    keystore.writer(PKCS12_PASSWORD).write().map_err(|e| {
        anyhow::Error::msg(format!(
            "amqp channel '{alias}': failed to build PKCS#12 identity: {e}"
        ))
    })
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

    fn supports_outbound_send(&self) -> bool {
        // `send` above is a deliberate no-op: AMQP is inbound-only here. A surface
        // that must actually deliver (e.g. the SOP approval route adapter) must not
        // route to it and mistake the no-op `Ok` for a successful send.
        false
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let (_conn, mut consumer) = self.establish_consumer().await?;

        zeroclaw_runtime::health::mark_component_ok("amqp");
        let _peers = (self.peer_resolver)();

        while let Some(delivery) = consumer.next().await {
            let Ok(delivery) = delivery else {
                continue;
            };

            let routing_key = delivery.routing_key.as_str().to_string();
            let message_id = delivery
                .properties
                .message_id()
                .as_ref()
                .map(|id| id.to_string());
            let redelivered = delivery.redelivered;
            match self
                .route_delivery(
                    &routing_key,
                    &delivery.data,
                    message_id.as_deref(),
                    redelivered,
                    &tx,
                )
                .await
            {
                DeliveryOutcome::Processed => {
                    if self.durable_ack {
                        delivery.acker.ack(BasicAckOptions::default()).await?;
                    }
                }
                DeliveryOutcome::Deferred => {
                    // Backpressured: nack with requeue so the broker redelivers the
                    // trigger once an exec slot / pending-approval slot frees, rather
                    // than acking it away. `no_ack` is false whenever `durable_ack`
                    // is set, so a manual nack is valid here. Under sustained
                    // saturation this drives broker-paced redelivery churn (the
                    // intended backpressure signal); operators wanting delayed retry
                    // configure a broker-side dead-letter exchange with a TTL.
                    if self.durable_ack {
                        delivery
                            .acker
                            .nack(BasicNackOptions {
                                requeue: true,
                                ..BasicNackOptions::default()
                            })
                            .await?;
                    }
                }
                DeliveryOutcome::ReceiverGone => return Ok(()),
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        match self.connect().await {
            Ok(conn) => {
                let _ = conn.close(200, "health check").await;
                true
            }
            Err(_) => false,
        }
    }

    fn self_handle(&self) -> Option<String> {
        Some(self.sender_label.clone())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator concept in AMQP.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator concept in AMQP.
        Ok(())
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

    fn try_channel_with(
        content_template: &str,
        thread_id_field: &str,
        dispatch: SopDispatch,
        engine: Option<Arc<Mutex<SopEngine>>>,
        audit: Option<Arc<SopAuditLogger>>,
    ) -> anyhow::Result<AmqpChannel> {
        AmqpChannel::new(AmqpChannelConfig {
            amqp_url: "amqp://localhost:5672".into(),
            exchange: "amq.topic".into(),
            routing_keys: vec!["org.release-monitoring.prod.anitya.project.version.update".into()],
            queue: None,
            ca_cert: None,
            client_cert: None,
            client_key: None,
            sender_label: "anitya".into(),
            content_template: content_template.into(),
            thread_id_field: thread_id_field.into(),
            durable_ack: true,
            dispatch,
            engine,
            audit,
            alias: "stagex".into(),
            peer_resolver: Arc::new(Vec::new),
        })
    }

    fn channel_with(content_template: &str, thread_id_field: &str) -> AmqpChannel {
        try_channel_with(
            content_template,
            thread_id_field,
            SopDispatch::AgentLoop,
            None,
            None,
        )
        .expect("agent-loop dispatch needs no SOP handles")
    }

    #[test]
    fn name_is_amqp() {
        assert_eq!(channel_with("", "").name(), "amqp");
    }

    #[tokio::test]
    async fn health_check_false_when_broker_unreachable() {
        assert!(!channel_with("", "").health_check().await);
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

    const TEST_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDGzCCAgOgAwIBAgIUUtTxJz9ELMaQ6z2J//Bpa5kyYoswDQYJKoZIhvcNAQEL
BQAwHTEbMBkGA1UEAwwSemVyb2NsYXctYW1xcC10ZXN0MB4XDTI2MDYwODEwMTM0
MVoXDTM2MDYwNTEwMTM0MVowHTEbMBkGA1UEAwwSemVyb2NsYXctYW1xcC10ZXN0
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA6iCKzDypyR1K8P1x+h3X
1SPNeeHZTKqXxuO7KDOkfxAz0/LHbAECHN9Xr9avqWgTYar6qkbHT6+zsswd7pWn
taiUfI8w8xzS9SkJTU8G9MTAbPVRUJODEc7JkVCy0hmXeBSYTBNr384jsb5wbCb7
g/PzwwNTzx8DU3uXZJUCr7iQQkzmVF5XAyoHQvJTdprmuOq0s8WXA+a5eYSwwAUB
9GuTPD6dXqZhuAnwSWRwArXEG00IRl1dn2BzuVYtvYJEc3syuSqk3V7bZJs3SLs7
PAoG0nFm6FpxHbMzZRLEMUj1mTmum7vjoECczW8hs1yk1wzec0Q3LraIlZybEwo4
UwIDAQABo1MwUTAdBgNVHQ4EFgQUT9jegFsB4vqPv9txOAOw00XkBNgwHwYDVR0j
BBgwFoAUT9jegFsB4vqPv9txOAOw00XkBNgwDwYDVR0TAQH/BAUwAwEB/zANBgkq
hkiG9w0BAQsFAAOCAQEASFq6LCwc3BE+DfOIsxH5GZCsxbWn8qAIyNaLvGZ4BJue
igjrIkcPrka+vvAzH/WZ//sik2iHTqeCYVNQXBrE9IMd6ISbZGSGnbDKWB59XCr0
L7kDxW9go1Ds1YA0VAYzdHpKVNfAY16Z8q8n0EeCuyLty2oxmPb0WbrC1jLT1clK
fATX2TiHItBKHNt4vHVpKv2ro3sFexuTsw+SG8kqGPyQYcQtduxPwQRT4Cvqy9im
yNV2tOdoySeNDbVazE9t9USV1RhxSELM3uHDA21h+9N5WvNjsl/DusmnYRU6ctt7
TaVnvfaqRPw9ppTeitQf8XnYucS5rb4DDI+bFH1+Fg==
-----END CERTIFICATE-----
"#;

    const TEST_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDqIIrMPKnJHUrw
/XH6HdfVI8154dlMqpfG47soM6R/EDPT8sdsAQIc31ev1q+paBNhqvqqRsdPr7Oy
zB3ulae1qJR8jzDzHNL1KQlNTwb0xMBs9VFQk4MRzsmRULLSGZd4FJhME2vfziOx
vnBsJvuD8/PDA1PPHwNTe5dklQKvuJBCTOZUXlcDKgdC8lN2mua46rSzxZcD5rl5
hLDABQH0a5M8Pp1epmG4CfBJZHACtcQbTQhGXV2fYHO5Vi29gkRzezK5KqTdXttk
mzdIuzs8CgbScWboWnEdszNlEsQxSPWZOa6bu+OgQJzNbyGzXKTXDN5zRDcutoiV
nJsTCjhTAgMBAAECggEAOuz20gGAoBAB0RaQzakeLdRFfmQT62JSMeoWLD+XKq26
xaDohSvZyseBi82GR6ZcnmvIi/ulZU5s9Va/P9GltKhZuuHVKZL7G135K950ez1b
yvCRRyzhQ6WegLblUtDDGSNh01/d+iWpQS6Tn/zNt7+5/b6EJPCCx0unZlbEptHb
JWZI9rkycULdUWj+5L67E3AvqNOP7ZfQo8QHlF6UUunzPI++2NJz32YW98WxknME
SDNlFcN3J+tQ8tn1ibWvSmyepOmEMzrnNZGQpVu98R0BzMVcsyZNJ6Fe9ZFoArZe
PLvHArBIXgKvCmGI7EM8hsfw+a8cwY0T8ZiRHCpCwQKBgQD6KdR/cQrgJGxZp0yz
Vmifhje/+u3kgVvOGibrMM8kAYh7dq/T9VQrYhWt62RqmeSnrSO0YdsW1z/adhop
DM6TKp2fW0yWUwtWCP8YJ+tHXMeczgpA4XWYke33wMSBAH2thizXAhKEqZC1YjNt
vfoBqX6j/TBQx/jGCTU0eEXxEwKBgQDvlu5V0MTca5NKebT4asr8QWp/b0vLC49g
vmUFqPDOabzLS42B/F8fRGDBkuwAmI5UzfU4tsz+GhM44vaa+otPAhwZJMQBEpfV
pxa0nwKDYBI3lF/snfgAQXtkxYlsupnVpjS1yixwcVXwizgs1X8O82jAJMPwsk/y
K3yxBVrDwQKBgQC2Cvia4N0kLP035JnZK3kpFRe+udCh50yyV6+YmMU0E3WJOt5K
pQ1iIJdcH57MQD73kfQYkNlI7syFokn5M1ukFm/rhhnejoICUrunjW0WWjrcLcei
XS8hHpiIIRweMAhE3Q4GTHjDV0154QNByeyDhx8kINwm/M5Y9lxkWV20RwKBgQCG
HckAxMLOWHG1CPgi7zT9jGjfOSAGY0w5bZsDVhSml04Vxw9JqkpdKFu5QFNX6g4S
rtAMlVefDl2gRHyjOIjvC1FLSedmalAQS15McY5omEjaT/Z6b9s52W4HdQR+lt4y
WL283ZWOxALFiklB36kmZ19F387HWCmkeG9ucH7kgQKBgFbHOqeODRxk7vsrl19R
U7GhLxgfFRzi6sAAJJpEz6KFZkgcyZiHF2h3yPgoV31Qw1VJe6pYoWibHBYfoddg
LrCdof4+vxz/kRhSxomk5EvQRy6uYgwu3dn4O4LV0AoHZ3LepltdPiixYBOm9VV0
tr7J6RKtO4OsZS/2KoYL8M+o
-----END PRIVATE KEY-----
"#;

    #[test]
    fn pem_to_pkcs12_der_roundtrips() {
        let der = pem_to_pkcs12_der(TEST_CERT_PEM.as_bytes(), TEST_KEY_PEM.as_bytes(), "stagex")
            .expect("PEM cert+key should convert to a PKCS#12 bundle");
        // The same PKCSreader tcp-stream uses must be able to parse it back
        // and recover a private key chain.
        let store = p12_keystore::KeyStore::from_pkcs12(&der, PKCS12_PASSWORD)
            .expect("generated PKCS#12 should parse");
        assert!(
            store.private_key_chain().is_some(),
            "PKCS#12 bundle must carry a private key chain"
        );
    }

    #[test]
    fn build_client_identity_none_without_cert() {
        let ch = channel_with("", "");
        assert!(ch.build_client_identity().expect("no client tls").is_none());
    }

    fn sop_handles() -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
        use zeroclaw_config::schema::SopConfig;
        use zeroclaw_memory::NoneMemory;
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let audit = Arc::new(SopAuditLogger::new(Arc::new(NoneMemory::new("none"))));
        (engine, audit)
    }

    #[test]
    fn new_rejects_sop_dispatch_without_handles() {
        for dispatch in [SopDispatch::Sop, SopDispatch::SopAndAgentLoop] {
            let result = try_channel_with("", "", dispatch, None, None);
            let Err(err) = result else {
                panic!("SOP dispatch without engine/audit must fail closed");
            };
            assert!(
                err.to_string().contains("SOP engine"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn new_accepts_agent_loop_without_handles() {
        assert!(try_channel_with("", "", SopDispatch::AgentLoop, None, None).is_ok());
    }

    #[tokio::test]
    async fn combined_route_fails_closed_when_receiver_gone() {
        let (engine, audit) = sop_handles();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::SopAndAgentLoop,
            Some(engine),
            Some(audit),
        )
        .expect("sop_and_agent_loop with handles constructs");

        let (tx, rx) = mpsc::channel::<ChannelMessage>(1);
        drop(rx);

        let outcome = ch
            .route_delivery("anitya.update", br#"{"name":"curl"}"#, None, false, &tx)
            .await;

        assert_eq!(
            outcome,
            DeliveryOutcome::ReceiverGone,
            "a closed receiver must short-circuit before SOP dispatch so the \
             delivery is left unacked for broker redelivery, not run as a SOP"
        );
    }

    #[tokio::test]
    async fn combined_route_dispatches_agent_when_receiver_open() {
        let (engine, audit) = sop_handles();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::SopAndAgentLoop,
            Some(engine),
            Some(audit),
        )
        .expect("sop_and_agent_loop with handles constructs");

        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(1);
        let outcome = ch
            .route_delivery("anitya.update", br#"{"name":"curl"}"#, None, false, &tx)
            .await;

        assert_eq!(outcome, DeliveryOutcome::Processed);
        let msg = rx.recv().await.expect("agent-loop message delivered");
        assert_eq!(msg.content, "curl");
    }

    #[tokio::test]
    async fn deferred_sop_dispatch_requeues_instead_of_acking() {
        let (engine, audit) = sop_handles_at_exec_slot_full();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::Sop,
            Some(engine),
            Some(audit),
        )
        .expect("sop-only channel with handles constructs");
        let (tx, _rx) = mpsc::channel::<ChannelMessage>(1);

        // A second matching delivery is backpressured (slot full). route_delivery must
        // report Deferred so the listener nacks/requeues it, rather than acking the
        // trigger away under durable_ack.
        let outcome = ch
            .route_delivery("anitya.update", br#"{"name":"curl"}"#, None, false, &tx)
            .await;
        assert_eq!(
            outcome,
            DeliveryOutcome::Deferred,
            "a backpressured SOP delivery must requeue for redelivery, not ack-and-lose"
        );
    }

    #[tokio::test]
    async fn sop_only_mixed_started_and_deferred_delivery_starts_none_and_requeues() {
        let (engine, audit) = sop_handles_with_mixed_admission();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::Sop,
            Some(Arc::clone(&engine)),
            Some(audit),
        )
        .expect("sop-only channel with handles constructs");
        let (tx, _rx) = mpsc::channel::<ChannelMessage>(1);

        let outcome = ch
            .route_delivery("anitya.update", br#"{"name":"curl"}"#, None, false, &tx)
            .await;

        assert_eq!(
            outcome,
            DeliveryOutcome::Deferred,
            "a mixed ready+deferred SOP-only delivery must requeue as one broker unit"
        );
        let eng = engine.lock().unwrap();
        assert!(
            eng.run_summaries(Some("amqp-ready")).is_empty(),
            "the ready sibling must not start before the deferred sibling can share the delivery"
        );
        assert_eq!(
            eng.run_summaries(Some("amqp-full")).len(),
            1,
            "the pre-existing full SOP run remains the only run for that SOP"
        );
    }

    /// Build sop/audit handles with `amqp-sop` already occupying its single exec
    /// slot (`max_concurrent = 1`), so the next matching AMQP delivery is
    /// backpressured and `dispatch_sop_event` returns a `Deferred` result.
    fn sop_handles_at_exec_slot_full() -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
        use zeroclaw_runtime::sop::SopStepKind;
        use zeroclaw_runtime::sop::types::{
            Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopStep, SopTrigger,
        };
        let (engine, audit) = sop_handles();
        {
            let mut eng = engine.lock().unwrap();
            eng.set_sops_for_test(vec![Sop {
                name: "amqp-sop".into(),
                description: "test".into(),
                version: "1.0.0".into(),
                priority: SopPriority::Normal,
                execution_mode: SopExecutionMode::Auto,
                triggers: vec![SopTrigger::Amqp {
                    routing_key: "anitya.update".into(),
                    condition: None,
                }],
                steps: vec![SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::default(),
                    schema: None,
                    ..SopStep::default()
                }],
                cooldown_secs: 0,
                max_concurrent: 1,
                location: None,
                deterministic: false,
                admission_policy: SopAdmissionPolicy::Parallel,
                max_pending_approvals: 0,
                agent: None,
            }]);
            // Fill the single exec slot with an in-flight run.
            eng.start_run(
                "amqp-sop",
                SopEvent {
                    source: SopTriggerSource::Amqp,
                    topic: Some("anitya.update".into()),
                    payload: None,
                    timestamp: zeroclaw_runtime::sop::engine::now_iso8601(),
                },
            )
            .expect("first run fills the slot");
        }
        (engine, audit)
    }

    fn sop_handles_with_parallel_amqp_sop() -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
        use zeroclaw_runtime::sop::SopStepKind;
        use zeroclaw_runtime::sop::types::{
            Sop, SopAdmissionPolicy, SopExecutionMode, SopPriority, SopStep, SopTrigger,
        };
        let (engine, audit) = sop_handles();
        {
            let mut eng = engine.lock().unwrap();
            eng.set_sops_for_test(vec![Sop {
                name: "amqp-sop".into(),
                description: "test".into(),
                version: "1.0.0".into(),
                priority: SopPriority::Normal,
                execution_mode: SopExecutionMode::Auto,
                triggers: vec![SopTrigger::Amqp {
                    routing_key: "anitya.update".into(),
                    condition: None,
                }],
                steps: vec![SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::default(),
                    schema: None,
                    ..SopStep::default()
                }],
                cooldown_secs: 0,
                max_concurrent: 8,
                location: None,
                deterministic: false,
                admission_policy: SopAdmissionPolicy::Parallel,
                max_pending_approvals: 0,
                agent: None,
            }]);
        }
        (engine, audit)
    }

    fn sop_handles_with_mixed_admission() -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
        use zeroclaw_runtime::sop::SopStepKind;
        use zeroclaw_runtime::sop::types::{
            Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopStep, SopTrigger,
        };

        fn amqp_sop(name: &str) -> Sop {
            Sop {
                name: name.into(),
                description: "test".into(),
                version: "1.0.0".into(),
                priority: SopPriority::Normal,
                execution_mode: SopExecutionMode::Auto,
                triggers: vec![SopTrigger::Amqp {
                    routing_key: "anitya.update".into(),
                    condition: None,
                }],
                steps: vec![SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::default(),
                    schema: None,
                    ..SopStep::default()
                }],
                cooldown_secs: 0,
                max_concurrent: 1,
                location: None,
                deterministic: false,
                admission_policy: SopAdmissionPolicy::Parallel,
                max_pending_approvals: 0,
                agent: None,
            }
        }

        let (engine, audit) = sop_handles();
        {
            let mut eng = engine.lock().unwrap();
            eng.set_sops_for_test(vec![amqp_sop("amqp-ready"), amqp_sop("amqp-full")]);
            eng.start_run(
                "amqp-full",
                SopEvent {
                    source: SopTriggerSource::Amqp,
                    topic: Some("anitya.update".into()),
                    payload: None,
                    timestamp: zeroclaw_runtime::sop::engine::now_iso8601(),
                },
            )
            .expect("first run fills amqp-full slot");
        }
        (engine, audit)
    }

    #[tokio::test]
    async fn route_delivery_coalesces_only_a_redelivery_of_the_same_message_id() {
        // Only a CONFIRMED redelivery (`redelivered = true`) of the same message-id
        // coalesces; a distinct message-id starts its own run. `amqp-sop` is Parallel with
        // ample concurrency, so without the dedup the redelivery would start a second run.
        let (engine, audit) = sop_handles_with_parallel_amqp_sop();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::Sop,
            Some(engine.clone()),
            Some(audit),
        )
        .expect("sop channel constructs");
        let (tx, _rx) = mpsc::channel::<ChannelMessage>(4);
        let body = br#"{"name":"curl"}"#;

        // Fresh m1 starts a run.
        assert_eq!(
            ch.route_delivery("anitya.update", body, Some("m1"), false, &tx)
                .await,
            DeliveryOutcome::Processed
        );
        // A REDELIVERY of m1 coalesces (no second run).
        assert_eq!(
            ch.route_delivery("anitya.update", body, Some("m1"), true, &tx)
                .await,
            DeliveryOutcome::Processed
        );
        assert_eq!(
            engine.lock().unwrap().active_runs().len(),
            1,
            "a redelivery of the same message-id must not start a second run"
        );
        // A distinct message-id starts its own run.
        assert_eq!(
            ch.route_delivery("anitya.update", body, Some("m2"), false, &tx)
                .await,
            DeliveryOutcome::Processed
        );
        assert_eq!(
            engine.lock().unwrap().active_runs().len(),
            2,
            "a distinct message-id must start its own run"
        );
    }

    #[tokio::test]
    async fn route_delivery_fresh_deliveries_reusing_a_message_id_both_start() {
        // The loss case a message-id-only dedup would reintroduce: TWO distinct FRESH
        // deliveries that reuse the same message-id must BOTH start - never coalesce/ACK
        // one away - because only a confirmed redelivery (not a fresh delivery) coalesces.
        let (engine, audit) = sop_handles_with_parallel_amqp_sop();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::Sop,
            Some(engine.clone()),
            Some(audit),
        )
        .expect("sop channel constructs");
        let (tx, _rx) = mpsc::channel::<ChannelMessage>(8);
        let body = br#"{"name":"curl"}"#;

        // Same id, both FRESH (redelivered = false) -> both start.
        for _ in 0..2 {
            assert_eq!(
                ch.route_delivery("anitya.update", body, Some("reused"), false, &tx)
                    .await,
                DeliveryOutcome::Processed
            );
        }
        assert_eq!(
            engine.lock().unwrap().active_runs().len(),
            2,
            "distinct fresh deliveries reusing a message-id must both start, never coalesce"
        );
    }

    #[tokio::test]
    async fn route_delivery_without_or_blank_message_id_does_not_dedup() {
        // No message-id (`None`) and a BLANK message-id (`""`) both mean "no per-message
        // identity": deliveries must NOT be coalesced (never ACK a distinct message away
        // on a guess). Each starts its own run, even when flagged as a redelivery.
        let (engine, audit) = sop_handles_with_parallel_amqp_sop();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::Sop,
            Some(engine.clone()),
            Some(audit),
        )
        .expect("sop channel constructs");
        let (tx, _rx) = mpsc::channel::<ChannelMessage>(8);
        let body = br#"{"name":"curl"}"#;

        for (mid, redelivered) in [
            (None, false),
            (None, true),
            (Some(""), false),
            (Some(""), true),
        ] {
            assert_eq!(
                ch.route_delivery("anitya.update", body, mid, redelivered, &tx)
                    .await,
                DeliveryOutcome::Processed
            );
        }
        assert_eq!(
            engine.lock().unwrap().active_runs().len(),
            4,
            "absent and blank message-ids must never coalesce distinct deliveries"
        );
    }

    #[tokio::test]
    async fn combined_mode_acks_sop_overflow_and_agent_gets_exactly_one_message() {
        // Blocker regression: in combined `sop_and_agent_loop` the agent loop already
        // accepted the delivery, so requeuing to retry a backpressured SOP side would
        // redeliver the SAME message to the agent and double-run its side effects. The
        // channel must ACK (Processed) instead, and the agent must see it exactly once.
        let (engine, audit) = sop_handles_at_exec_slot_full();
        let ch = try_channel_with(
            "{name}",
            "name",
            SopDispatch::SopAndAgentLoop,
            Some(engine),
            Some(audit),
        )
        .expect("combined channel constructs");
        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(4);

        let outcome = ch
            .route_delivery("anitya.update", br#"{"name":"curl"}"#, None, false, &tx)
            .await;
        assert_eq!(
            outcome,
            DeliveryOutcome::Processed,
            "combined mode ACKs a backpressured SOP delivery (agent already consumed); it must not requeue"
        );
        assert!(
            rx.try_recv().is_ok(),
            "the agent loop received the delivery once"
        );
        assert!(
            rx.try_recv().is_err(),
            "the agent loop must NOT be handed a duplicate of the same delivery"
        );
    }
}
