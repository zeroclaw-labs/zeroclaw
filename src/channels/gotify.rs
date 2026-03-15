use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Result;
use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use serde::Deserialize;

/// Default priority for Gotify messages (0 = lowest).
const DEFAULT_PRIORITY: u8 = 5;

/// Reconnect delay when the WebSocket connection drops.
const WS_RECONNECT_DELAY_SECS: u64 = 5;

/// Monotonic counter for unique message IDs.
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Gotify channel — connects to a self-hosted Gotify server.
///
/// Uses two separate tokens: an app token for sending messages via
/// the REST API, and a client token for receiving messages via WebSocket.
/// Supports priority levels and markdown extras.
pub struct GotifyChannel {
    server_url: String,
    app_token: String,
    client_token: String,
    default_priority: u8,
    use_markdown: bool,
}

/// Configuration for constructing a `GotifyChannel`.
pub struct GotifyChannelConfig {
    pub server_url: String,
    pub app_token: String,
    pub client_token: String,
    pub default_priority: Option<u8>,
    pub use_markdown: bool,
}

impl GotifyChannel {
    pub fn new(cfg: GotifyChannelConfig) -> Self {
        let server_url = cfg.server_url.trim_end_matches('/').to_string();
        Self {
            server_url,
            app_token: cfg.app_token,
            client_token: cfg.client_token,
            default_priority: cfg.default_priority.unwrap_or(DEFAULT_PRIORITY),
            use_markdown: cfg.use_markdown,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.gotify")
    }

    /// Post a message to Gotify via REST API.
    async fn post_message(
        &self,
        title: Option<&str>,
        message: &str,
        priority: u8,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/message?token={}", self.server_url, self.app_token);

        let mut body = serde_json::json!({
            "message": message,
            "priority": priority,
        });

        if let Some(t) = title {
            body["title"] = serde_json::Value::String(t.to_string());
        }

        if self.use_markdown {
            body["extras"] = serde_json::json!({
                "client::display": {
                    "contentType": "text/markdown"
                }
            });
        }

        let resp = self.http_client().post(&url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gotify post_message failed ({status}): {resp_body}");
        }

        let json: serde_json::Value = resp.json().await?;
        Ok(json)
    }

    /// Build the WebSocket URL for the Gotify stream.
    fn ws_url(&self) -> String {
        let base = if self.server_url.starts_with("https://") {
            self.server_url.replacen("https://", "wss://", 1)
        } else if self.server_url.starts_with("http://") {
            self.server_url.replacen("http://", "ws://", 1)
        } else {
            format!("ws://{}", self.server_url)
        };
        format!("{base}/stream?token={}", self.client_token)
    }
}

/// A message received from the Gotify WebSocket stream.
#[derive(Debug, Clone, Deserialize)]
struct GotifyMessage {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    priority: u8,
    #[serde(default)]
    date: String,
    #[serde(default, rename = "appid")]
    app_id: u64,
}

#[async_trait]
impl Channel for GotifyChannel {
    fn name(&self) -> &str {
        "gotify"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let title = message.subject.as_deref();
        self.post_message(title, &message.content, self.default_priority)
            .await?;
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!("Gotify channel connecting to {}...", self.server_url);

        loop {
            let ws_url = self.ws_url();
            tracing::debug!("Gotify WebSocket connecting...");

            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws_stream, _)) => {
                    tracing::info!("Gotify WebSocket connected");
                    use futures_util::StreamExt;
                    let (_, mut read) = ws_stream.split();

                    while let Some(msg_result) = read.next().await {
                        let ws_msg = match msg_result {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::warn!("Gotify WebSocket error: {e:#}");
                                break;
                            }
                        };

                        let text = match ws_msg {
                            tokio_tungstenite::tungstenite::Message::Text(t) => t,
                            tokio_tungstenite::tungstenite::Message::Close(_) => break,
                            // Ping, Pong, Binary, Frame — skip
                            _ => continue,
                        };

                        let gotify_msg: GotifyMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::debug!("Gotify: ignoring unparseable message: {e}");
                                continue;
                            }
                        };

                        if gotify_msg.message.is_empty() {
                            continue;
                        }

                        let content = if gotify_msg.title.is_empty() {
                            gotify_msg.message.clone()
                        } else {
                            format!("{}: {}", gotify_msg.title, gotify_msg.message)
                        };

                        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
                        let channel_msg = ChannelMessage {
                            id: format!("gotify_{seq}_{}", gotify_msg.id),
                            sender: format!("app_{}", gotify_msg.app_id),
                            reply_target: format!("app_{}", gotify_msg.app_id),
                            content,
                            channel: "gotify".to_string(),
                            timestamp: chrono::DateTime::parse_from_rfc3339(&gotify_msg.date)
                                .map(|dt| u64::try_from(dt.timestamp()).unwrap_or(0))
                                .unwrap_or_else(|_| {
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                }),
                            thread_ts: None,
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }

                    tracing::info!("Gotify WebSocket disconnected, reconnecting...");
                }
                Err(e) => {
                    tracing::warn!("Gotify WebSocket connection failed: {e:#}");
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(WS_RECONNECT_DELAY_SECS)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.server_url);
        let resp = self.http_client().get(&url).send().await;
        resp.is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config / constructor ──────────────────────────────────

    #[test]
    fn config_defaults_priority() {
        let ch = GotifyChannel::new(GotifyChannelConfig {
            server_url: "https://gotify.example.com".into(),
            app_token: "app_tok".into(),
            client_token: "client_tok".into(),
            default_priority: None,
            use_markdown: false,
        });
        assert_eq!(ch.default_priority, DEFAULT_PRIORITY);
        assert!(!ch.use_markdown);
    }

    #[test]
    fn config_accepts_explicit_values() {
        let ch = GotifyChannel::new(GotifyChannelConfig {
            server_url: "https://gotify.example.com/".into(),
            app_token: "app_tok".into(),
            client_token: "client_tok".into(),
            default_priority: Some(10),
            use_markdown: true,
        });
        assert_eq!(ch.server_url, "https://gotify.example.com");
        assert_eq!(ch.default_priority, 10);
        assert!(ch.use_markdown);
    }

    #[test]
    fn config_strips_trailing_slash() {
        let ch = GotifyChannel::new(GotifyChannelConfig {
            server_url: "http://localhost:8080/".into(),
            app_token: "a".into(),
            client_token: "c".into(),
            default_priority: None,
            use_markdown: false,
        });
        assert_eq!(ch.server_url, "http://localhost:8080");
    }

    #[test]
    fn name_returns_gotify() {
        let ch = make_channel();
        assert_eq!(ch.name(), "gotify");
    }

    // ── WebSocket URL ─────────────────────────────────────────

    #[test]
    fn ws_url_converts_https_to_wss() {
        let ch = GotifyChannel::new(GotifyChannelConfig {
            server_url: "https://gotify.example.com".into(),
            app_token: "a".into(),
            client_token: "mytoken".into(),
            default_priority: None,
            use_markdown: false,
        });
        assert_eq!(ch.ws_url(), "wss://gotify.example.com/stream?token=mytoken");
    }

    #[test]
    fn ws_url_converts_http_to_ws() {
        let ch = GotifyChannel::new(GotifyChannelConfig {
            server_url: "http://localhost:8080".into(),
            app_token: "a".into(),
            client_token: "tok".into(),
            default_priority: None,
            use_markdown: false,
        });
        assert_eq!(ch.ws_url(), "ws://localhost:8080/stream?token=tok");
    }

    // ── Config serde ──────────────────────────────────────────

    #[test]
    fn gotify_config_serde_roundtrip() {
        use crate::config::schema::GotifyConfig;

        let config = GotifyConfig {
            server_url: "https://gotify.example.com".into(),
            app_token: "app_abc".into(),
            client_token: "client_xyz".into(),
            default_priority: Some(8),
            use_markdown: Some(true),
        };

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: GotifyConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.server_url, "https://gotify.example.com");
        assert_eq!(parsed.app_token, "app_abc");
        assert_eq!(parsed.client_token, "client_xyz");
        assert_eq!(parsed.default_priority, Some(8));
        assert_eq!(parsed.use_markdown, Some(true));
    }

    #[test]
    fn gotify_config_minimal_toml() {
        use crate::config::schema::GotifyConfig;

        let toml_str = r#"
server_url = "http://localhost:8080"
app_token = "a"
client_token = "c"
"#;
        let parsed: GotifyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.server_url, "http://localhost:8080");
        assert_eq!(parsed.app_token, "a");
        assert_eq!(parsed.client_token, "c");
        assert!(parsed.default_priority.is_none());
        assert!(parsed.use_markdown.is_none());
    }

    // ── Helpers ───────────────────────────────────────────────

    fn make_channel() -> GotifyChannel {
        GotifyChannel::new(GotifyChannelConfig {
            server_url: "https://gotify.example.com".into(),
            app_token: "test_app".into(),
            client_token: "test_client".into(),
            default_priority: None,
            use_markdown: false,
        })
    }
}
