use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// WeCom (WeChat Enterprise) Bot Webhook channel.
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
    /// API host, overridable for tests (mirrors `LineChannel`).
    api_base_url: String,
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
            api_base_url: "https://qyapi.weixin.qq.com".into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_api_base_url(mut self, url: &str) -> Self {
        self.api_base_url = url.trim_end_matches('/').to_string();
        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.wecom")
    }

    fn webhook_url(&self) -> String {
        format!(
            "{}/cgi-bin/webhook/send?key={}",
            self.api_base_url, self.webhook_key
        )
    }

    pub fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        let allowed =
            crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive);
        ::zeroclaw_log::record!(TRACE, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel": "wecom", "alias": self.alias, "user_id": user_id, "allowed": allowed})), "wecom allowlist decision");
        allowed
    }
}

impl ::zeroclaw_api::attribution::Attributable for WeComChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::WeCom)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for WeComChannel {
    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator endpoint in the WeCom API.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator endpoint in the WeCom API.
        Ok(())
    }

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "channel ready (send-only via Bot Webhook)"
        );
        tx.closed().await;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        // WeCom's Bot Webhook API has no side-effect-free validation surface:
        // every documented webhook operation posts a visible message to the
        // group. Routine health and doctor checks must not create user-visible
        // traffic, so this validates configuration only. Delivery — including
        // WeCom's application-level `errcode` — is validated on real sends.
        let configured = !self.webhook_key.trim().is_empty();
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"alias": self.alias, "configured": configured})),
            "wecom health: configuration-only validation (the webhook API has no message-free probe)"
        );
        configured
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

    #[tokio::test]
    async fn health_check_is_side_effect_free_and_reflects_configuration() {
        use wiremock::MockServer;

        // No mock is mounted: any request would fail the test via the
        // received-requests assertion below.
        let server = MockServer::start().await;

        let ch = WeComChannel::new("key-abc".into(), "wecom_test_alias", Arc::new(Vec::new))
            .with_api_base_url(&server.uri());
        assert!(ch.health_check().await, "configured key reads healthy");

        let empty = WeComChannel::new(String::new(), "wecom_test_alias", Arc::new(Vec::new))
            .with_api_base_url(&server.uri());
        assert!(!empty.health_check().await, "missing key reads unhealthy");

        let received = server.received_requests().await.unwrap_or_default();
        assert!(
            received.is_empty(),
            "health_check must not call the WeCom API (would post a visible message); saw {} request(s)",
            received.len()
        );
    }

    #[tokio::test]
    async fn send_reports_api_level_failure_despite_http_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cgi-bin/webhook/send"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errcode": 93000,
                "errmsg": "invalid webhook url"
            })))
            .mount(&server)
            .await;

        let ch = WeComChannel::new("bad-key".into(), "wecom_test_alias", Arc::new(Vec::new))
            .with_api_base_url(&server.uri());

        let error = ch
            .send(&SendMessage::new("hello", "group"))
            .await
            .expect_err("HTTP 200 with nonzero errcode must not read as delivered");
        let message = error.to_string();
        assert!(
            message.contains("errcode=93000"),
            "error should surface the WeCom errcode: {message}"
        );
    }

    #[tokio::test]
    async fn send_succeeds_on_errcode_zero() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cgi-bin/webhook/send"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errcode": 0,
                "errmsg": "ok"
            })))
            .mount(&server)
            .await;

        let ch = WeComChannel::new("good-key".into(), "wecom_test_alias", Arc::new(Vec::new))
            .with_api_base_url(&server.uri());

        ch.send(&SendMessage::new("hello", "group"))
            .await
            .expect("errcode 0 delivers");
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
