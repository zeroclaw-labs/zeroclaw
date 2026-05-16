use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Deduplication set capacity — evict half of entries when full.
const DEDUP_CAPACITY: usize = 10_000;

/// Mochat customer service channel.
///
/// Integrates with the Mochat open-source customer service platform API
/// for receiving and sending messages through its HTTP endpoints.
pub struct MochatChannel {
    api_url: String,
    api_token: String,
    /// The alias key under `[channels.mochat.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    poll_interval_secs: u64,
    /// Message deduplication set.
    dedup: Arc<RwLock<HashSet<String>>>,
}

impl MochatChannel {
    pub fn new(
        api_url: String,
        api_token: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            api_token,
            alias: alias.into(),
            peer_resolver,
            poll_interval_secs,
            dedup: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Return the alias under `[channels.mochat.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.mochat")
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    /// Check and insert message ID for deduplication.
    async fn is_duplicate(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }

        let mut dedup = self.dedup.write().await;

        if dedup.contains(msg_id) {
            return true;
        }

        if dedup.len() >= DEDUP_CAPACITY {
            let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }

        dedup.insert(msg_id.to_string());
        false
    }
}

impl ::zeroclaw_api::attribution::Attributable for MochatChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::MoChat,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for MochatChannel {
    fn name(&self) -> &str {
        "mochat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let body = json!({
            "toUserId": message.recipient,
            "msgType": "text",
            "content": {
                "text": message.content,
            }
        });

        let resp = self
            .http_client()
            .post(format!("{}/api/message/send", self.api_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Mochat send message failed ({status}): {err}");
        }

        let result: serde_json::Value = resp.json().await?;
        let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 && code != 200 {
            let msg = result
                .get("msg")
                .or_else(|| result.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("Mochat API error (code={code}): {msg}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "starting message poller");

        let poll_interval = std::time::Duration::from_secs(self.poll_interval_secs);
        let mut last_message_id: Option<String> = None;

        loop {
            let mut url = format!("{}/api/message/receive", self.api_url);
            if let Some(ref id) = last_message_id {
                use std::fmt::Write;
                let _ = write!(url, "?since_id={id}");
            }

            match self
                .http_client()
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.api_token))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let data: serde_json::Value = match resp.json().await {
                        Ok(d) => d,
                        Err(e) => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "failed to parse response");
                            tokio::time::sleep(poll_interval).await;
                            continue;
                        }
                    };

                    let messages = data
                        .get("data")
                        .or_else(|| data.get("messages"))
                        .and_then(|d| d.as_array());

                    if let Some(messages) = messages {
                        for msg in messages {
                            let msg_id = msg
                                .get("messageId")
                                .or_else(|| msg.get("id"))
                                .and_then(|i| i.as_str())
                                .unwrap_or("");

                            if self.is_duplicate(msg_id).await {
                                continue;
                            }

                            let sender = msg
                                .get("fromUserId")
                                .or_else(|| msg.get("sender"))
                                .and_then(|s| s.as_str())
                                .unwrap_or("unknown");

                            if !self.is_user_allowed(sender) {
                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"sender": sender})), "ignoring message from unauthorized user");
                                continue;
                            }

                            let content = msg
                                .get("content")
                                .and_then(|c| {
                                    c.get("text")
                                        .and_then(|t| t.as_str())
                                        .or_else(|| c.as_str())
                                })
                                .unwrap_or("")
                                .trim();

                            if content.is_empty() {
                                continue;
                            }

                            let channel_msg = ChannelMessage {
                                id: Uuid::new_v4().to_string(),
                                sender: sender.to_string(),
                                reply_target: sender.to_string(),
                                content: content.to_string(),
                                channel: "mochat".to_string(),
                                channel_alias: Some(self.alias.clone()),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: None,
                                interruption_scope_id: None,
                                attachments: vec![],
                            };

                            if tx.send(channel_msg).await.is_err() {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "message channel closed");
                                return Ok(());
                            }

                            if !msg_id.is_empty() {
                                last_message_id = Some(msg_id.to_string());
                            }
                        }
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": err.to_string(), "status": status.to_string()})), "poll request failed");
                }
                Err(e) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "poll request error");
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        let resp = self
            .http_client()
            .get(format!("{}/api/health", self.api_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await;

        match resp {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = MochatChannel::new(
            "https://mochat.example.com".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(Vec::new),
            5,
        );
        assert_eq!(ch.name(), "mochat");
    }

    #[test]
    fn test_api_url_trailing_slash_stripped() {
        let ch = MochatChannel::new(
            "https://mochat.example.com/".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(Vec::new),
            5,
        );
        assert_eq!(ch.api_url, "https://mochat.example.com");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = MochatChannel::new(
            "https://m.test".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(|| vec!["*".into()]),
            5,
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = MochatChannel::new(
            "https://m.test".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(|| vec!["user123".into()]),
            5,
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = MochatChannel::new(
            "https://m.test".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(Vec::new),
            5,
        );
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[tokio::test]
    async fn test_dedup() {
        let ch = MochatChannel::new(
            "https://m.test".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(Vec::new),
            5,
        );
        assert!(!ch.is_duplicate("msg1").await);
        assert!(ch.is_duplicate("msg1").await);
        assert!(!ch.is_duplicate("msg2").await);
    }

    #[tokio::test]
    async fn test_dedup_empty_id() {
        let ch = MochatChannel::new(
            "https://m.test".into(),
            "tok".into(),
            "mochat_test_alias",
            Arc::new(Vec::new),
            5,
        );
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[test]
    fn v2_allowed_users_fold_into_peer_groups() {
        // V2 `[channels.mochat].allowed_users` migrates into a synthesized
        // `[peer_groups.mochat_default]` block in V3, while the channel block
        // itself survives under the bridge alias `default`.
        let v2_toml = r#"
schema_version = 2

[channels.mochat]
enabled = true
api_url = "https://mochat.example.com"
api_token = "secret"
allowed_users = ["user1"]
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 mochat config migrates to V3");
        let mochat = cfg
            .channels
            .mochat
            .get("default")
            .expect("V2 mochat folds under alias `default`");
        assert_eq!(mochat.api_url, "https://mochat.example.com");
        assert_eq!(mochat.api_token, "secret");

        let group = cfg
            .peer_groups
            .get("mochat_default")
            .expect("mochat allow-list synthesizes [peer_groups.mochat_default]");
        assert_eq!(group.channel, "mochat");
        let peers: Vec<&str> = group.external_peers.iter().map(|p| p.as_str()).collect();
        assert_eq!(peers, vec!["user1"]);
    }

    #[test]
    fn v2_no_allowed_users_synthesizes_no_peer_group() {
        // V2 mochat without `allowed_users` migrates without synthesizing a
        // peer group; `poll_interval_secs` default survives untouched.
        let v2_toml = r#"
schema_version = 2

[channels.mochat]
enabled = true
api_url = "https://mochat.example.com"
api_token = "secret"
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 mochat config without allowed_users migrates");
        assert!(
            !cfg.peer_groups.contains_key("mochat_default"),
            "no peer group synthesized when allowed_users is absent"
        );
        let mochat = cfg
            .channels
            .mochat
            .get("default")
            .expect("V2 mochat folds under alias `default`");
        assert_eq!(mochat.poll_interval_secs, 5);
    }
}
