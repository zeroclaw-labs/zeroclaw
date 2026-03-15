use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Result;
use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use serde::Deserialize;

/// Default ntfy server.
const DEFAULT_SERVER: &str = "https://ntfy.sh";

/// Monotonic counter for unique message IDs.
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// ntfy priority levels (1-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NtfyPriority {
    Min = 1,
    Low = 2,
    Default = 3,
    High = 4,
    Max = 5,
}

impl NtfyPriority {
    fn as_str(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Low => "low",
            Self::Default => "default",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Min,
            2 => Self::Low,
            4 => Self::High,
            5 => Self::Max,
            _ => Self::Default,
        }
    }
}

/// ntfy channel — notification-first channel using the ntfy.sh API.
///
/// Primarily designed for outbound notifications. Supports subscribing
/// to a topic via JSON stream for inbound messages, priority levels,
/// tags, and click actions.
pub struct NtfyChannel {
    server: String,
    topic: String,
    auth_token: Option<String>,
    default_priority: NtfyPriority,
    default_tags: Vec<String>,
    default_click: Option<String>,
}

/// Configuration for constructing an `NtfyChannel`.
pub struct NtfyChannelConfig {
    pub server: Option<String>,
    pub topic: String,
    pub auth_token: Option<String>,
    pub default_priority: Option<u8>,
    pub default_tags: Vec<String>,
    pub default_click: Option<String>,
}

impl NtfyChannel {
    pub fn new(cfg: NtfyChannelConfig) -> Self {
        let server = cfg
            .server
            .unwrap_or_else(|| DEFAULT_SERVER.to_string())
            .trim_end_matches('/')
            .to_string();
        let default_priority = cfg
            .default_priority
            .map(NtfyPriority::from_u8)
            .unwrap_or(NtfyPriority::Default);
        Self {
            server,
            topic: cfg.topic,
            auth_token: cfg.auth_token,
            default_priority,
            default_tags: cfg.default_tags,
            default_click: cfg.default_click,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.ntfy")
    }

    /// Build the full topic URL.
    fn topic_url(&self) -> String {
        format!("{}/{}", self.server, self.topic)
    }

    /// Publish a message to the ntfy topic.
    async fn publish(
        &self,
        message: &str,
        title: Option<&str>,
        priority: NtfyPriority,
        tags: &[String],
        click: Option<&str>,
    ) -> Result<()> {
        let url = self.topic_url();
        let mut req = self.http_client().post(&url);

        if let Some(ref token) = self.auth_token {
            req = req.bearer_auth(token);
        }

        // ntfy uses HTTP headers for metadata
        if let Some(t) = title {
            req = req.header("Title", t);
        }

        if priority != NtfyPriority::Default {
            req = req.header("Priority", priority.as_str());
        }

        if !tags.is_empty() {
            req = req.header("Tags", tags.join(","));
        }

        if let Some(c) = click {
            req = req.header("Click", c);
        }

        let resp = req
            .header("Content-Type", "text/plain")
            .body(message.to_string())
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ntfy publish failed ({status}): {body}");
        }

        Ok(())
    }
}

/// A message received from the ntfy JSON stream.
#[derive(Debug, Clone, Deserialize)]
struct NtfyEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    event: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    time: u64,
}

#[async_trait]
impl Channel for NtfyChannel {
    fn name(&self) -> &str {
        "ntfy"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let title = message.subject.as_deref();
        self.publish(
            &message.content,
            title,
            self.default_priority,
            &self.default_tags,
            self.default_click.as_deref(),
        )
        .await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            "ntfy channel subscribing to {}/{}...",
            self.server,
            self.topic
        );

        let url = format!("{}/json", self.topic_url());

        loop {
            let mut req = self.http_client().get(&url);
            if let Some(ref token) = self.auth_token {
                req = req.bearer_auth(token);
            }

            match req.send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        tracing::warn!("ntfy subscribe got {status}, retrying...");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }

                    let mut stream = resp.bytes_stream();
                    use futures_util::StreamExt;
                    let mut buffer = String::new();

                    while let Some(chunk_result) = stream.next().await {
                        let chunk = match chunk_result {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("ntfy stream error: {e:#}");
                                break;
                            }
                        };

                        buffer.push_str(&String::from_utf8_lossy(&chunk));

                        // Process complete JSON lines
                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim().to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            let event: NtfyEvent = match serde_json::from_str(&line) {
                                Ok(e) => e,
                                Err(_) => continue,
                            };

                            // Only process actual messages, not keepalives or open events
                            if event.event != "message" || event.message.is_empty() {
                                continue;
                            }

                            let content = if event.title.is_empty() {
                                event.message.clone()
                            } else {
                                format!("{}: {}", event.title, event.message)
                            };

                            let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
                            let channel_msg = ChannelMessage {
                                id: format!("ntfy_{seq}_{}", event.id),
                                sender: self.topic.clone(),
                                reply_target: self.topic.clone(),
                                content,
                                channel: "ntfy".to_string(),
                                timestamp: if event.time > 0 {
                                    event.time
                                } else {
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                },
                                thread_ts: None,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                return Ok(());
                            }
                        }
                    }

                    tracing::info!("ntfy stream ended, reconnecting...");
                }
                Err(e) => {
                    tracing::warn!("ntfy connection error: {e:#}");
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // ntfy has a health endpoint at /v1/health
        let url = format!("{}/v1/health", self.server);
        let mut req = self.http_client().get(&url);
        if let Some(ref token) = self.auth_token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await;
        resp.is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config / constructor ──────────────────────────────────

    #[test]
    fn config_defaults_server_to_ntfy_sh() {
        let ch = NtfyChannel::new(NtfyChannelConfig {
            server: None,
            topic: "test-topic".into(),
            auth_token: None,
            default_priority: None,
            default_tags: vec![],
            default_click: None,
        });
        assert_eq!(ch.server, DEFAULT_SERVER);
        assert_eq!(ch.topic, "test-topic");
        assert_eq!(ch.default_priority, NtfyPriority::Default);
    }

    #[test]
    fn config_accepts_custom_server() {
        let ch = NtfyChannel::new(NtfyChannelConfig {
            server: Some("https://ntfy.example.com/".into()),
            topic: "alerts".into(),
            auth_token: Some("tk_secret".into()),
            default_priority: Some(4),
            default_tags: vec!["warning".into()],
            default_click: Some("https://example.com".into()),
        });
        assert_eq!(ch.server, "https://ntfy.example.com");
        assert_eq!(ch.topic, "alerts");
        assert_eq!(ch.auth_token.as_deref(), Some("tk_secret"));
        assert_eq!(ch.default_priority, NtfyPriority::High);
    }

    #[test]
    fn name_returns_ntfy() {
        let ch = make_channel();
        assert_eq!(ch.name(), "ntfy");
    }

    // ── Priority ──────────────────────────────────────────────

    #[test]
    fn priority_from_u8_known_values() {
        assert_eq!(NtfyPriority::from_u8(1), NtfyPriority::Min);
        assert_eq!(NtfyPriority::from_u8(2), NtfyPriority::Low);
        assert_eq!(NtfyPriority::from_u8(3), NtfyPriority::Default);
        assert_eq!(NtfyPriority::from_u8(4), NtfyPriority::High);
        assert_eq!(NtfyPriority::from_u8(5), NtfyPriority::Max);
    }

    #[test]
    fn priority_from_u8_unknown_defaults() {
        assert_eq!(NtfyPriority::from_u8(0), NtfyPriority::Default);
        assert_eq!(NtfyPriority::from_u8(99), NtfyPriority::Default);
    }

    #[test]
    fn priority_as_str_roundtrip() {
        for &(val, expected) in &[
            (NtfyPriority::Min, "min"),
            (NtfyPriority::Low, "low"),
            (NtfyPriority::Default, "default"),
            (NtfyPriority::High, "high"),
            (NtfyPriority::Max, "max"),
        ] {
            assert_eq!(val.as_str(), expected);
        }
    }

    // ── Topic URL ─────────────────────────────────────────────

    #[test]
    fn topic_url_formats_correctly() {
        let ch = NtfyChannel::new(NtfyChannelConfig {
            server: Some("https://ntfy.example.com".into()),
            topic: "my-topic".into(),
            auth_token: None,
            default_priority: None,
            default_tags: vec![],
            default_click: None,
        });
        assert_eq!(ch.topic_url(), "https://ntfy.example.com/my-topic");
    }

    // ── Config serde ──────────────────────────────────────────

    #[test]
    fn ntfy_config_serde_roundtrip() {
        use crate::config::schema::NtfyConfig;

        let config = NtfyConfig {
            server: Some("https://ntfy.example.com".into()),
            topic: "zeroclaw-alerts".into(),
            auth_token: Some("tk_abc".into()),
            default_priority: Some(4),
            default_tags: vec!["robot".into()],
            default_click: Some("https://example.com".into()),
        };

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: NtfyConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.server.as_deref(), Some("https://ntfy.example.com"));
        assert_eq!(parsed.topic, "zeroclaw-alerts");
        assert_eq!(parsed.auth_token.as_deref(), Some("tk_abc"));
        assert_eq!(parsed.default_priority, Some(4));
        assert_eq!(parsed.default_tags, vec!["robot"]);
        assert_eq!(parsed.default_click.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn ntfy_config_minimal_toml() {
        use crate::config::schema::NtfyConfig;

        let toml_str = r#"
topic = "my-topic"
"#;
        let parsed: NtfyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.topic, "my-topic");
        assert!(parsed.server.is_none());
        assert!(parsed.auth_token.is_none());
        assert!(parsed.default_priority.is_none());
        assert!(parsed.default_tags.is_empty());
        assert!(parsed.default_click.is_none());
    }

    // ── Helpers ───────────────────────────────────────────────

    fn make_channel() -> NtfyChannel {
        NtfyChannel::new(NtfyChannelConfig {
            server: None,
            topic: "test".into(),
            auth_token: None,
            default_priority: None,
            default_tags: vec![],
            default_click: None,
        })
    }
}
