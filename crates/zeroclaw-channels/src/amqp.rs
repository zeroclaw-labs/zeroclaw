use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use futures_util::StreamExt;
use lapin::{
    Connection, ConnectionProperties,
    options::{BasicAckOptions, BasicConsumeOptions, QueueBindOptions, QueueDeclareOptions},
    tcp::{OwnedIdentity, OwnedTLSConfig},
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
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    sender_label: String,
    content_template: String,
    thread_id_field: String,
    durable_ack: bool,
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
            client_cert: cfg.client_cert,
            client_key: cfg.client_key,
            sender_label: cfg.sender_label,
            content_template: cfg.content_template,
            thread_id_field: cfg.thread_id_field,
            durable_ack: cfg.durable_ack,
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
    /// `async-global-executor`. The Tokio reactor adapter is Unix-only, so
    /// non-Unix targets use lapin's cross-platform async-io reactor adapter.
    /// A configured `ca_cert` is supplied as the custom certificate chain for
    /// `amqps://` server verification, and a configured
    /// `client_cert`/`client_key` pair is presented as the client identity for
    /// broker mutual-TLS auth (Fedora Messaging requires this).
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

    /// Build the client-auth identity for mutual TLS from the configured PEM
    /// `client_cert` and `client_key`. tcp-stream's rustls path consumes the
    /// identity as a PKCS#12 DER bundle, so we parse the PEM cert chain and
    /// private key to DER and assemble an in-memory PKCS#12 keystore protected
    /// by an ephemeral password (the bundle never leaves this process).
    ///
    /// Returns `Ok(None)` when no client cert/key is configured (server-auth
    /// only). Both must be supplied together; supplying one without the other
    /// is a configuration error.
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

/// Ephemeral password protecting the in-memory PKCS#12 identity. The bundle is
/// built and consumed within a single connect call and never persisted, so the
/// password only has to round-trip through tcp-stream's PKCS#12 reader.
const PKCS12_PASSWORD: &str = "zeroclaw-amqp";

/// Convert a PEM client certificate chain and private key into a PKCS#12 DER
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

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let (_conn, mut consumer) = self.establish_consumer().await?;

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

            if self.durable_ack {
                delivery.acker.ack(BasicAckOptions::default()).await?;
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

    fn channel_with(content_template: &str, thread_id_field: &str) -> AmqpChannel {
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
            alias: "stagex".into(),
            peer_resolver: Arc::new(Vec::new),
        })
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
        // The same PKCS#12 reader tcp-stream uses must be able to parse it back
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
}
