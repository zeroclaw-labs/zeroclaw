use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// WeCom (WeChat Enterprise) Bot Webhook channel.
///
/// Sends messages via the WeCom Bot Webhook API. Incoming messages are received
/// through a configurable callback URL that WeCom posts to.
pub struct WeComChannel {
    webhook_key: String,
    /// The alias key under `[channels.wecom.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl WeComChannel {
    pub fn new(
        webhook_key: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            webhook_key,
            alias: alias.into(),
            peer_resolver,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.wecom")
    }

    fn webhook_url(&self) -> String {
        format!(
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key={}",
            self.webhook_key
        )
    }

    /// Check whether `user_id` is on the allowlist for this WeCom channel.
    ///
    /// WeCom Bot Webhook is send-only, so this gate is exercised only by
    /// callback flows the gateway routes back through this channel handle.
    /// The `alias` is included in the trace span so multi-WeCom deployments
    /// can distinguish which channel made the decision.
    pub fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        let allowed =
            crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive);
        tracing::trace!(
            channel = "wecom",
            alias = %self.alias,
            user_id = %user_id,
            allowed,
            "wecom allowlist decision"
        );
        allowed
    }
}

#[async_trait]
impl Channel for WeComChannel {
    fn name(&self) -> &str {
        "wecom"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "msgtype": "text",
            "text": {
                "content": message.content,
            }
        });

        let resp = self
            .http_client()
            .post(self.webhook_url())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("WeCom webhook send failed ({status}): {err}");
        }

        // WeCom returns {"errcode":0,"errmsg":"ok"} on success.
        let result: serde_json::Value = resp.json().await?;
        let errcode = result.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
        if errcode != 0 {
            let errmsg = result
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("WeCom API error (errcode={errcode}): {errmsg}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WeCom Bot Webhook is send-only by default. For receiving messages,
        // an enterprise application with a callback URL is needed, which is
        // handled via the gateway webhook subsystem.
        //
        // This listener keeps the channel alive and waits for the sender to close.
        tracing::info!("WeCom: channel ready (send-only via Bot Webhook)");
        tx.closed().await;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Verify we can reach the WeCom API endpoint.
        let resp = self
            .http_client()
            .post(self.webhook_url())
            .json(&serde_json::json!({
                "msgtype": "text",
                "text": {
                    "content": "health_check"
                }
            }))
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
        let ch = WeComChannel::new("test-key".into(), "wecom_test_alias", Arc::new(Vec::new));
        assert_eq!(ch.name(), "wecom");
    }

    #[test]
    fn test_webhook_url() {
        let ch = WeComChannel::new("abc-123".into(), "wecom_test_alias", Arc::new(Vec::new));
        assert_eq!(
            ch.webhook_url(),
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc-123"
        );
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = WeComChannel::new(
            "key".into(),
            "wecom_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = WeComChannel::new(
            "key".into(),
            "wecom_test_alias",
            Arc::new(|| vec!["user123".into()]),
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = WeComChannel::new("key".into(), "wecom_test_alias", Arc::new(Vec::new));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn v2_allowed_users_fold_into_peer_groups() {
        // V2 `[channels.wecom].allowed_users` migrates into a synthesized
        // `[peer_groups.wecom_default]` block in V3. The wildcard sentinel is
        // filtered out during synthesis so only concrete usernames survive as
        // external peers.
        let v2_toml = r#"
schema_version = 2

[channels.wecom]
enabled = true
webhook_key = "key-abc-123"
allowed_users = ["user1", "*"]
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 wecom config migrates to V3");
        let wecom = cfg
            .channels
            .wecom
            .get("default")
            .expect("V2 wecom folds under alias `default`");
        assert_eq!(wecom.webhook_key, "key-abc-123");

        let group = cfg
            .peer_groups
            .get("wecom_default")
            .expect("wecom allow-list synthesizes [peer_groups.wecom_default]");
        assert_eq!(group.channel, "wecom");
        let peers: Vec<&str> = group.external_peers.iter().map(|p| p.as_str()).collect();
        assert_eq!(peers, vec!["user1"]);
    }

    #[test]
    fn v2_no_allowed_users_synthesizes_no_peer_group() {
        // V2 wecom without `allowed_users` must not synthesize a peer group.
        let v2_toml = r#"
schema_version = 2

[channels.wecom]
enabled = true
webhook_key = "key"
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 wecom config without allowed_users migrates");
        assert!(
            !cfg.peer_groups.contains_key("wecom_default"),
            "no peer group synthesized when allowed_users is absent"
        );
    }
}
