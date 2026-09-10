use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

pub struct SendblueChannel {
    api_key_id: String,
    api_secret_key: String,
    from_number: String,
    /// The alias key under `[channels.sendblue.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    client: reqwest::Client,
}

const SENDBLUE_API_BASE: &str = "https://api.sendblue.com/api";

impl SendblueChannel {
    pub fn new(
        api_key_id: String,
        api_secret_key: String,
        from_number: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            api_key_id,
            api_secret_key,
            from_number,
            alias: alias.into(),
            peer_resolver,
            client: reqwest::Client::new(),
        }
    }

    /// Return the alias under `[channels.sendblue.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Get the bot's phone number
    pub fn phone_number(&self) -> &str {
        &self.from_number
    }

    /// Sendblue authenticates every API call with a key-id/secret-key header
    /// pair rather than a bearer token.
    fn auth_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder
            .header("sb-api-key-id", &self.api_key_id)
            .header("sb-api-secret-key", &self.api_secret_key)
    }

    /// Check if a sender phone number is allowed (E.164 format: +1234567890)
    fn is_sender_allowed(&self, phone: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, phone, crate::allowlist::Match::Sensitive)
    }

    /// Normalize a Sendblue handle to E.164. Sendblue is inconsistent about
    /// the leading `+` between the REST API and webhook deliveries, and the
    /// peer allowlist is matched literally, so an un-normalized number would
    /// silently fail the allowlist rather than error.
    fn normalize_e164(number: &str) -> String {
        let trimmed = number.trim();
        if trimmed.starts_with('+') {
            trimmed.to_string()
        } else {
            format!("+{trimmed}")
        }
    }

    /// Sendblue delivers a message's attachment as a single `media_url`
    /// rather than a typed parts array.
    fn media_marker(payload: &serde_json::Value) -> Option<String> {
        let url = payload
            .get("media_url")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;

        Some(format!("[IMAGE:{url}]"))
    }

    fn is_outbound(payload: &serde_json::Value) -> bool {
        payload
            .get("is_outbound")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    /// Sendblue posts the message object at the top level, but wraps it in
    /// `data` when the account has the newer envelope format enabled.
    fn message_object(payload: &serde_json::Value) -> &serde_json::Value {
        payload.get("data").unwrap_or(payload)
    }

    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();
        let data = Self::message_object(payload);

        // Sendblue posts delivery receipts for the bot's own sends on the same
        // webhook as inbound traffic. Without this the agent answers itself.
        if Self::is_outbound(data) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping outbound message receipt"
            );
            return messages;
        }

        let Some(from) = data
            .get("from_number")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return messages;
        };

        let normalized_from = Self::normalize_e164(from);

        if !self.is_sender_allowed(&normalized_from) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"normalized_from": normalized_from})),
                "ignoring message from unauthorized sender. Add the number to this channel's peer group."
            );
            return messages;
        }

        let mut content_parts: Vec<String> = Vec::new();

        if let Some(text) = data
            .get("content")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            content_parts.push(text.to_string());
        }

        if let Some(marker) = Self::media_marker(data) {
            content_parts.push(marker);
        }

        if content_parts.is_empty() {
            return messages;
        }

        let content = content_parts.join("\n").trim().to_string();

        if content.is_empty() {
            return messages;
        }

        let timestamp = data
            .get("date_sent")
            .and_then(|value| value.as_str())
            .and_then(|value| {
                chrono::DateTime::parse_from_rfc3339(value)
                    .ok()
                    .map(|dt| dt.timestamp().cast_unsigned())
            })
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });

        // Sendblue conversations are keyed by handle, not by a chat id, so the
        // sender's number is also the reply target.
        let id = data
            .get("message_handle")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| Uuid::new_v4().to_string(), ToString::to_string);

        messages.push(ChannelMessage {
            id,
            reply_target: normalized_from.clone(),
            sender: normalized_from,
            content,
            channel: "sendblue".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        });

        messages
    }
}

impl ::zeroclaw_api::attribution::Attributable for SendblueChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Sendblue,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for SendblueChannel {
    fn name(&self) -> &str {
        "sendblue"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let recipient = Self::normalize_e164(&message.recipient);

        let body = ::serde_json::json!({
            "number": recipient,
            "from_number": self.from_number,
            "content": message.content,
        });

        let resp = self
            .auth_headers(
                self.client
                    .post(format!("{SENDBLUE_API_BASE}/send-message")),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(());
        }

        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(
                    ::serde_json::json!({"status": status.to_string(), "error_body": error_body})
                ),
            "send failed:"
        );
        anyhow::bail!("API error: {status}");
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Sendblue uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /sendblue endpoint.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "channel active (webhook mode). \
            Configure the Sendblue webhook to POST to your gateway's /sendblue endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Sendblue has no dedicated health route; the message list is the
        // cheapest authenticated GET that proves the credential pair works.
        let url = format!("{SENDBLUE_API_BASE}/v2/messages?limit=1");

        self.auth_headers(self.client.get(&url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let body = ::serde_json::json!({
            "number": Self::normalize_e164(recipient),
            "from_number": self.from_number,
        });

        let resp = self
            .auth_headers(
                self.client
                    .post(format!("{SENDBLUE_API_BASE}/send-typing-indicator")),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("start_typing failed: {}", resp.status())
            );
        }

        Ok(())
    }
}

/// Verify an inbound Sendblue webhook against the configured shared secret.
///
/// Sendblue does not sign webhook deliveries — there is no HMAC scheme and no
/// timestamp to bind, so unlike Linq or WhatsApp there is nothing to
/// recompute. The only credential available is a shared secret the operator
/// configures on both ends, presented in a header. Accept the header names
/// Sendblue's dashboard can be configured to send, compare in constant time,
/// and refuse anything else.
///
/// This is weaker than a signature: it does not bind the secret to the request
/// body, so a captured header can be replayed with a forged body. Deploy the
/// endpoint over TLS.
pub fn verify_sendblue_secret(secret: &str, headers: &reqwest::header::HeaderMap) -> bool {
    use zeroclaw_config::pairing::constant_time_eq;

    if secret.is_empty() {
        return false;
    }

    const SECRET_HEADERS: &[&str] = &["x-sendblue-secret", "x-webhook-secret"];

    for name in SECRET_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            let presented = value.trim();
            if !presented.is_empty() && constant_time_eq(presented, secret) {
                return true;
            }
        }
    }

    // `Authorization: Bearer <secret>` is what Sendblue's dashboard emits when
    // the webhook is configured with an auth header rather than a custom one.
    if let Some(value) = headers
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let presented = value.trim().strip_prefix("Bearer ").unwrap_or("").trim();
        if !presented.is_empty() && constant_time_eq(presented, secret) {
            return true;
        }
    }

    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
        "rejecting webhook with missing or invalid shared secret"
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_with_peers(peers: Vec<String>) -> SendblueChannel {
        SendblueChannel::new(
            "key-id".to_string(),
            "secret-key".to_string(),
            "+15550001111".to_string(),
            "main",
            Arc::new(move || peers.clone()),
        )
    }

    fn allowed_channel() -> SendblueChannel {
        channel_with_peers(vec!["+447700900123".to_string()])
    }

    fn inbound(content: &str) -> serde_json::Value {
        serde_json::json!({
            "message_handle": "abc-123",
            "from_number": "+447700900123",
            "to_number": "+15550001111",
            "content": content,
            "is_outbound": false,
            "status": "RECEIVED",
            "date_sent": "2026-09-10T18:00:00.000Z",
        })
    }

    #[test]
    fn parses_a_plain_inbound_message() {
        let msgs = allowed_channel().parse_webhook_payload(&inbound("hello there"));

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello there");
        assert_eq!(msgs[0].sender, "+447700900123");
        assert_eq!(msgs[0].reply_target, "+447700900123");
        assert_eq!(msgs[0].channel, "sendblue");
        assert_eq!(msgs[0].channel_alias.as_deref(), Some("main"));
        assert_eq!(msgs[0].id, "abc-123");
    }

    #[test]
    fn uses_the_date_sent_timestamp() {
        let msgs = allowed_channel().parse_webhook_payload(&inbound("hi"));

        // 2026-09-10T18:00:00Z
        assert_eq!(msgs[0].timestamp, 1_789_063_200);
    }

    #[test]
    fn drops_the_bots_own_outbound_receipts() {
        let mut payload = inbound("delivered");
        payload["is_outbound"] = serde_json::json!(true);

        assert!(
            allowed_channel().parse_webhook_payload(&payload).is_empty(),
            "an outbound receipt must not be dispatched back to the agent"
        );
    }

    #[test]
    fn drops_senders_outside_the_peer_group() {
        let channel = channel_with_peers(vec!["+15555550000".to_string()]);

        assert!(channel.parse_webhook_payload(&inbound("hello")).is_empty());
    }

    #[test]
    fn normalizes_a_sender_missing_its_plus() {
        let mut payload = inbound("hi");
        payload["from_number"] = serde_json::json!("447700900123");

        let msgs = allowed_channel().parse_webhook_payload(&payload);

        assert_eq!(msgs.len(), 1, "an un-prefixed number must still match");
        assert_eq!(msgs[0].sender, "+447700900123");
    }

    #[test]
    fn reads_through_the_data_envelope() {
        let payload = serde_json::json!({ "data": inbound("wrapped") });

        let msgs = allowed_channel().parse_webhook_payload(&payload);

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "wrapped");
    }

    #[test]
    fn appends_a_media_marker() {
        let mut payload = inbound("look at this");
        payload["media_url"] = serde_json::json!("https://cdn.sendblue.co/img.jpg");

        let msgs = allowed_channel().parse_webhook_payload(&payload);

        assert_eq!(
            msgs[0].content,
            "look at this\n[IMAGE:https://cdn.sendblue.co/img.jpg]"
        );
    }

    #[test]
    fn keeps_a_media_only_message() {
        let mut payload = inbound("");
        payload["media_url"] = serde_json::json!("https://cdn.sendblue.co/img.jpg");

        let msgs = allowed_channel().parse_webhook_payload(&payload);

        assert_eq!(msgs.len(), 1, "an image with no caption is still a message");
        assert_eq!(msgs[0].content, "[IMAGE:https://cdn.sendblue.co/img.jpg]");
    }

    #[test]
    fn drops_an_empty_message() {
        assert!(
            allowed_channel()
                .parse_webhook_payload(&inbound("   "))
                .is_empty()
        );
    }

    #[test]
    fn drops_a_message_with_no_sender() {
        let mut payload = inbound("hi");
        payload["from_number"] = serde_json::json!("");

        assert!(allowed_channel().parse_webhook_payload(&payload).is_empty());
    }

    fn headers_with(name: &str, value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        headers
    }

    #[test]
    fn accepts_the_custom_secret_header() {
        assert!(verify_sendblue_secret(
            "s3cret",
            &headers_with("x-sendblue-secret", "s3cret")
        ));
    }

    #[test]
    fn accepts_a_bearer_secret() {
        assert!(verify_sendblue_secret(
            "s3cret",
            &headers_with("authorization", "Bearer s3cret")
        ));
    }

    #[test]
    fn rejects_a_wrong_secret() {
        assert!(!verify_sendblue_secret(
            "s3cret",
            &headers_with("x-sendblue-secret", "nope")
        ));
    }

    #[test]
    fn rejects_a_missing_header() {
        assert!(!verify_sendblue_secret(
            "s3cret",
            &reqwest::header::HeaderMap::new()
        ));
    }

    #[test]
    fn rejects_an_empty_configured_secret() {
        assert!(
            !verify_sendblue_secret("", &headers_with("x-sendblue-secret", "")),
            "an unset secret must never authenticate a request"
        );
    }
}
