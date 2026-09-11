use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, ListenerHealth, SendMessage};

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
    /// How often `listen` pulls new messages. `None` disables polling, leaving
    /// the gateway's `/sendblue` webhook route as the only inbound path.
    poll_interval: Option<Duration>,
    /// Whether to mark a conversation read once an inbound message is accepted.
    read_receipts: bool,
    /// Recently accepted `message_handle`s, oldest first.
    ///
    /// Sendblue asks endpoints to be idempotent because it re-delivers on a
    /// non-2xx or a timeout, and the polling watermark is inclusive so the
    /// boundary message comes back on the next sweep. One shared record keeps
    /// both inbound paths from answering the same message twice.
    seen_handles: Mutex<VecDeque<String>>,
    /// `(succeeded, at)` for the last completed poll exchange. Read by
    /// `listener_health`, which must not perform I/O.
    poll_health: Mutex<Option<(bool, Instant)>>,
    client: reqwest::Client,
}

const SENDBLUE_API_BASE: &str = "https://api.sendblue.com/api";

/// How long a successful poll stays evidence that the listener works. A
/// blackholed request keeps `listen()` alive with nothing to end it, so a
/// stale success must stop counting.
const POLL_HEALTH_STALE_AFTER: Duration = Duration::from_secs(120);

/// Message handles retained for de-duplication. `created_at_gte` is inclusive
/// and Sendblue's clock is not ours, so the same message can be returned by
/// two consecutive polls.
const SEEN_HANDLE_CAP: usize = 512;

impl SendblueChannel {
    pub fn new(
        api_key_id: String,
        api_secret_key: String,
        from_number: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self::with_poll_interval(
            api_key_id,
            api_secret_key,
            from_number,
            alias,
            peer_resolver,
            None,
        )
    }

    pub fn with_poll_interval(
        api_key_id: String,
        api_secret_key: String,
        from_number: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        poll_interval: Option<Duration>,
    ) -> Self {
        Self {
            api_key_id,
            api_secret_key,
            from_number,
            alias: alias.into(),
            peer_resolver,
            poll_interval,
            read_receipts: false,
            seen_handles: Mutex::new(VecDeque::new()),
            poll_health: Mutex::new(None),
            client: reqwest::Client::new(),
        }
    }

    /// Mark conversations read as inbound messages are accepted.
    ///
    /// Off by default: Sendblue gates the endpoint per account (their
    /// engineering team has to turn it on), so defaulting it on would make
    /// every accepted message attempt a call most accounts cannot serve.
    #[must_use]
    pub fn with_read_receipts(mut self, read_receipts: bool) -> Self {
        self.read_receipts = read_receipts;
        self
    }

    /// Resolve a configured interval into a listener setting: `0` disables
    /// polling (leaving the webhook route as the only inbound path), and
    /// anything below the floor is clamped so a mistyped value cannot hammer
    /// the API.
    pub fn poll_interval_from_secs(secs: u64) -> Option<Duration> {
        const MIN_POLL_SECS: u64 = 5;
        (secs > 0).then(|| Duration::from_secs(secs.max(MIN_POLL_SECS)))
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

    /// Drop messages whose `message_handle` this channel has already accepted,
    /// and record the rest.
    ///
    /// Sendblue re-delivers a webhook that did not get a 2xx and returns the
    /// boundary message again on the next poll, so without this the agent
    /// answers some messages twice. Both inbound paths share one record.
    pub fn claim_unseen(&self, messages: Vec<ChannelMessage>) -> Vec<ChannelMessage> {
        let mut seen = self.seen_handles.lock();
        let mut fresh = Vec::with_capacity(messages.len());

        for msg in messages {
            if seen.contains(&msg.id) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"message_handle": msg.id})),
                    "skipping already-accepted message"
                );
                continue;
            }
            seen.push_back(msg.id.clone());
            while seen.len() > SEEN_HANDLE_CAP {
                seen.pop_front();
            }
            fresh.push(msg);
        }

        fresh
    }

    /// Mark the conversation with `recipient` as read, so the sender sees their
    /// message has landed while the agent is still composing.
    ///
    /// Best effort by contract: Sendblue documents no delivery confirmation,
    /// the endpoint is iMessage/RCS only (SMS carries no read state), and it
    /// has to be enabled per account. A failure here must never hold up or fail
    /// the inbound dispatch, so callers ignore the result and this logs at
    /// debug.
    pub async fn mark_read(&self, recipient: &str) -> anyhow::Result<()> {
        if !self.read_receipts {
            return Ok(());
        }

        let body = ::serde_json::json!({
            "number": Self::normalize_e164(recipient),
            "from_number": self.from_number,
        });

        let resp = self
            .auth_headers(self.client.post(format!("{SENDBLUE_API_BASE}/mark-read")))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("mark_read failed: {}", resp.status())
            );
        }

        Ok(())
    }

    /// Send read receipts for every distinct sender in `messages`, ignoring
    /// failures. One receipt per conversation, not per message, because a
    /// debounced batch from one sender is a single conversation.
    pub async fn mark_read_for(&self, messages: &[ChannelMessage]) {
        if !self.read_receipts {
            return;
        }

        let mut marked: Vec<&str> = Vec::new();
        for msg in messages {
            if marked.contains(&msg.reply_target.as_str()) {
                continue;
            }
            marked.push(&msg.reply_target);
            let _ = self.mark_read(&msg.reply_target).await;
        }
    }

    /// Fetch inbound messages Sendblue recorded at or after `since`.
    ///
    /// Returns the raw message objects so the caller can reuse
    /// [`Self::parse_webhook_payload`]: the polling API and the webhook deliver
    /// the same record shape, so both inbound paths share one parser and one
    /// allowlist decision.
    async fn fetch_inbound_since(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!("{SENDBLUE_API_BASE}/v2/messages");

        let resp = self
            .auth_headers(self.client.get(&url))
            .query(&[
                ("limit", "50".to_string()),
                ("created_at_gte", since.to_rfc3339()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error: {status}: {error_body}");
        }

        let body = resp.json::<serde_json::Value>().await?;

        Ok(body
            .get("data")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|item| !Self::is_outbound(item))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
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

        // The same endpoint carries delivery-status updates for earlier
        // messages. Only a genuine inbound is ours to answer.
        if let Some(status) = data
            .get("status")
            .and_then(|value| value.as_str())
            .filter(|status| !status.eq_ignore_ascii_case("RECEIVED"))
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"status": status})),
                "skipping non-received message event"
            );
            return messages;
        }

        // Group chats carry `group_id`, and a reply has to go to the group, not
        // to whoever spoke. Answering `from_number` would quietly turn a group
        // question into a private message. Group sends are not implemented, so
        // skip rather than misroute.
        if data
            .get("group_id")
            .and_then(|value| value.as_str())
            .is_some_and(|group_id| !group_id.trim().is_empty())
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping group message: group replies are not supported"
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

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let Some(interval) = self.poll_interval else {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "channel active (webhook mode). \
                Configure the Sendblue webhook to POST to your gateway's /sendblue endpoint."
            );

            // Keep the task alive — it will be cancelled when the channel shuts down
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"interval_secs": interval.as_secs()})),
            "channel active (polling mode)"
        );

        // Only messages that arrive from now on are ours to answer; replaying
        // the account's backlog on every restart would re-answer old texts.
        let mut watermark = chrono::Utc::now();

        loop {
            tokio::time::sleep(interval).await;

            let fetched = self.fetch_inbound_since(watermark).await;
            let payloads = match fetched {
                Ok(payloads) => {
                    *self.poll_health.lock() = Some((true, Instant::now()));
                    payloads
                }
                Err(err) => {
                    *self.poll_health.lock() = Some((false, Instant::now()));
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": err.to_string()})),
                        "poll failed"
                    );
                    continue;
                }
            };

            let mut newest = watermark;

            for payload in &payloads {
                if let Some(sent_at) = payload
                    .get("date_sent")
                    .and_then(|value| value.as_str())
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                {
                    let sent_at = sent_at.with_timezone(&chrono::Utc);
                    if sent_at > newest {
                        newest = sent_at;
                    }
                }

                // `created_at_gte` is inclusive, so the boundary message comes
                // back on the next poll. `claim_unseen` drops it on the handle.
                let parsed = self.claim_unseen(self.parse_webhook_payload(payload));
                if parsed.is_empty() {
                    continue;
                }

                // Ahead of dispatch: the point of the receipt is that the
                // sender sees the message landed while the agent is still
                // working on a reply.
                self.mark_read_for(&parsed).await;

                for msg in parsed {
                    if tx.send(msg).await.is_err() {
                        // Receiver dropped: the orchestrator is shutting this
                        // channel down.
                        return Ok(());
                    }
                }
            }

            watermark = newest;
        }
    }

    fn listener_health(&self) -> Option<ListenerHealth> {
        // Webhook mode does no exchange of its own, so it has no signal to
        // report and must not claim health it cannot observe.
        self.poll_interval?;
        Some(match *self.poll_health.lock() {
            None => ListenerHealth::Pending,
            Some((false, _)) => ListenerHealth::Unhealthy,
            Some((true, at)) if at.elapsed() < POLL_HEALTH_STALE_AFTER => ListenerHealth::Healthy,
            Some((true, _)) => ListenerHealth::Unhealthy,
        })
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

/// Header carrying Sendblue's HMAC signature, when the account sends one.
const SIGNATURE_HEADER: &str = "x-sendblue-signature";

/// Reject signatures older than this. Sendblue's own guidance is five minutes.
const SIGNATURE_MAX_AGE_SECS: i64 = 300;

/// Verify an inbound Sendblue webhook.
///
/// Sendblue has two mechanisms and which one arrives depends on the webhook
/// type, so both are accepted:
///
/// 1. **Signature** — `X-Sendblue-Signature: t=<unix>,v1=<hex>`, where the
///    signature is `HMAC_SHA256(secret, "<t>.<raw body>")`. This binds the
///    secret to the body and carries a timestamp, so it is replay-resistant and
///    is the strictly better check. Documented for Verify webhooks.
/// 2. **Secret echo** — `sb-signing-secret` carrying the configured secret
///    verbatim. This is what the message-webhook docs describe, and it is
///    weaker: nothing binds it to the body, so a captured header can be
///    replayed with forged content. Sendblue enforces HTTPS on webhook URLs,
///    which is what keeps the header off the wire; do not terminate the route
///    on plain HTTP.
///
/// When a signature is present it is authoritative: a bad one is refused rather
/// than falling through to the weaker check, so an attacker cannot downgrade to
/// the echo by sending a deliberately broken signature.
///
/// `x-webhook-secret` is accepted as an echo fallback for proxies that rename
/// the header on the way in.
pub fn verify_sendblue_secret(
    secret: &str,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
) -> bool {
    use zeroclaw_config::pairing::constant_time_eq;

    if secret.is_empty() {
        return false;
    }

    if let Some(header) = headers.get(SIGNATURE_HEADER).and_then(|v| v.to_str().ok()) {
        // Present means it is the contract for this delivery. Do not fall back.
        return verify_sendblue_signature(secret, header, body);
    }

    // `sb-signing-secret` must stay first and must stay the name in
    // `SENDBLUE_WEBHOOK.signature_header`: that spec field is what the ingress
    // layer names when it logs missing-vs-invalid. A request authenticated by
    // the `x-webhook-secret` proxy fallback alone is accepted here, but a
    // *failed* one is reported against the primary header. Do not "correct"
    // this back to a guessed name like `X-Sendblue-Secret`; Sendblue sends
    // `sb-signing-secret` (docs.sendblue.com/security).
    const SECRET_HEADERS: &[&str] = &["sb-signing-secret", "x-webhook-secret"];

    for name in SECRET_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            let presented = value.trim();
            if !presented.is_empty() && constant_time_eq(presented, secret) {
                return true;
            }
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

/// Verify an `X-Sendblue-Signature: t=<unix>,v1=<hex>` header against the raw
/// body. Exposed for callers that already know they hold a signed delivery.
pub fn verify_sendblue_signature(secret: &str, header: &str, body: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut timestamp: Option<&str> = None;
    let mut signature: Option<&str> = None;
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = Some(value.trim()),
            Some(("v1", value)) => signature = Some(value.trim()),
            _ => {}
        }
    }

    let (Some(timestamp), Some(signature)) = (timestamp, signature) else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "webhook signature header is missing its t= or v1= field"
        );
        return false;
    };

    let Ok(sent_at) = timestamp.parse::<i64>() else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "webhook signature timestamp is not a unix time"
        );
        return false;
    };

    let now = chrono::Utc::now().timestamp();
    if (now - sent_at).unsigned_abs() > SIGNATURE_MAX_AGE_SECS.unsigned_abs() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "rejecting stale webhook signature timestamp"
        );
        return false;
    }

    // Signed over the raw bytes, so this must run before any JSON round trip.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);

    let Ok(provided) = hex::decode(signature) else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "webhook signature is not hex"
        );
        return false;
    };

    // Constant-time comparison via HMAC verify.
    mac.verify_slice(&provided).is_ok()
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
    fn accepts_sendblues_own_signing_secret_header() {
        // `sb-signing-secret` is the header Sendblue actually sends; getting
        // this name wrong rejects every genuine delivery.
        assert!(verify_sendblue_secret(
            "s3cret",
            &headers_with("sb-signing-secret", "s3cret"),
            b"{}"
        ));
    }

    #[test]
    fn accepts_a_renamed_header_from_a_proxy() {
        assert!(verify_sendblue_secret(
            "s3cret",
            &headers_with("x-webhook-secret", "s3cret"),
            b"{}"
        ));
    }

    #[test]
    fn rejects_a_wrong_secret() {
        assert!(!verify_sendblue_secret(
            "s3cret",
            &headers_with("sb-signing-secret", "nope"),
            b"{}"
        ));
    }

    #[test]
    fn rejects_a_bearer_token_that_is_not_the_secret_header() {
        // Sendblue does not authenticate with `Authorization`; accepting it
        // would widen the surface for no reason.
        assert!(!verify_sendblue_secret(
            "s3cret",
            &headers_with("authorization", "Bearer s3cret"),
            b"{}"
        ));
    }

    #[test]
    fn rejects_a_missing_header() {
        assert!(!verify_sendblue_secret(
            "s3cret",
            &reqwest::header::HeaderMap::new(),
            b"{}"
        ));
    }

    #[test]
    fn rejects_an_empty_configured_secret() {
        assert!(
            !verify_sendblue_secret("", &headers_with("sb-signing-secret", ""), b"{}"),
            "an unset secret must never authenticate a request"
        );
    }

    fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        format!(
            "t={timestamp},v1={}",
            hex::encode(mac.finalize().into_bytes())
        )
    }

    #[test]
    fn accepts_a_valid_hmac_signature() {
        let body = br#"{"message_handle":"m1"}"#;
        let now = chrono::Utc::now().timestamp();

        assert!(verify_sendblue_secret(
            "s3cret",
            &headers_with("x-sendblue-signature", &sign("s3cret", now, body)),
            body
        ));
    }

    #[test]
    fn rejects_a_signature_over_a_different_body() {
        let now = chrono::Utc::now().timestamp();
        let header = sign("s3cret", now, br#"{"message_handle":"m1"}"#);

        assert!(
            !verify_sendblue_secret(
                "s3cret",
                &headers_with("x-sendblue-signature", &header),
                br#"{"message_handle":"tampered"}"#
            ),
            "the signature binds the body; a swapped body must not verify"
        );
    }

    #[test]
    fn rejects_a_stale_signature() {
        let body = br#"{"message_handle":"m1"}"#;
        let stale = chrono::Utc::now().timestamp() - (SIGNATURE_MAX_AGE_SECS + 60);

        assert!(
            !verify_sendblue_secret(
                "s3cret",
                &headers_with("x-sendblue-signature", &sign("s3cret", stale, body)),
                body
            ),
            "an old signature is replayable and must be refused"
        );
    }

    #[test]
    fn a_bad_signature_does_not_fall_back_to_the_secret_echo() {
        // Otherwise an attacker could downgrade to the weaker check by sending
        // a deliberately broken signature alongside a stolen secret.
        let body = br#"{"message_handle":"m1"}"#;
        let mut headers = headers_with("x-sendblue-signature", "t=1,v1=deadbeef");
        headers.insert("sb-signing-secret", "s3cret".parse().unwrap());

        assert!(!verify_sendblue_secret("s3cret", &headers, body));
    }

    #[test]
    fn rejects_a_malformed_signature_header() {
        let body = br#"{"message_handle":"m1"}"#;
        for header in ["", "garbage", "t=1", "v1=abc", "t=notatime,v1=abc"] {
            assert!(
                !verify_sendblue_secret(
                    "s3cret",
                    &headers_with("x-sendblue-signature", header),
                    body
                ),
                "malformed signature {header:?} must be refused"
            );
        }
    }

    #[test]
    fn claims_each_handle_once_across_both_paths() {
        let channel = allowed_channel();

        let first = channel.claim_unseen(channel.parse_webhook_payload(&inbound("hello")));
        assert_eq!(first.len(), 1);

        // Same handle again: a webhook retry, or the inclusive poll boundary.
        let second = channel.claim_unseen(channel.parse_webhook_payload(&inbound("hello")));
        assert!(
            second.is_empty(),
            "a re-delivered message must not dispatch twice"
        );
    }

    #[test]
    fn a_different_handle_is_still_accepted() {
        let channel = allowed_channel();
        channel.claim_unseen(channel.parse_webhook_payload(&inbound("first")));

        let mut payload = inbound("second");
        payload["message_handle"] = serde_json::json!("abc-456");

        assert_eq!(
            channel
                .claim_unseen(channel.parse_webhook_payload(&payload))
                .len(),
            1
        );
    }

    #[test]
    fn drops_a_delivery_status_update() {
        let mut payload = inbound("hello");
        payload["status"] = serde_json::json!("DELIVERED");

        assert!(
            allowed_channel().parse_webhook_payload(&payload).is_empty(),
            "a status update for an earlier message is not a new inbound"
        );
    }

    #[test]
    fn drops_a_group_message_rather_than_replying_privately() {
        let mut payload = inbound("hello everyone");
        payload["group_id"] = serde_json::json!("group-123");

        assert!(
            allowed_channel().parse_webhook_payload(&payload).is_empty(),
            "group replies are unsupported; answering from_number would turn a \
             group question into a private message"
        );
    }

    #[test]
    fn an_empty_group_id_is_not_a_group() {
        let mut payload = inbound("hello");
        payload["group_id"] = serde_json::json!("");

        assert_eq!(allowed_channel().parse_webhook_payload(&payload).len(), 1);
    }

    #[test]
    fn a_zero_interval_disables_polling() {
        assert_eq!(SendblueChannel::poll_interval_from_secs(0), None);
    }

    #[test]
    fn a_too_small_interval_is_clamped_to_the_floor() {
        assert_eq!(
            SendblueChannel::poll_interval_from_secs(1),
            Some(Duration::from_secs(5)),
            "a mistyped interval must not be allowed to hammer the API"
        );
    }

    #[test]
    fn a_reasonable_interval_is_used_as_given() {
        assert_eq!(
            SendblueChannel::poll_interval_from_secs(30),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn webhook_mode_reports_no_listener_health() {
        // Webhook mode completes no exchange of its own, so it has nothing to
        // vouch for and must not claim health it cannot observe.
        assert_eq!(allowed_channel().listener_health(), None);
    }

    #[tokio::test]
    async fn read_receipts_are_off_unless_enabled() {
        // The endpoint is gated per account, so a channel that was never told
        // to use receipts must not reach for it. With no HTTP mock in front of
        // this, a call that did go out would fail the DNS/connect and surface
        // as an error rather than `Ok`.
        let channel = allowed_channel();

        assert!(
            channel.mark_read("+447700900123").await.is_ok(),
            "receipts disabled must short-circuit before any request"
        );
        channel.mark_read_for(&[]).await;
    }

    #[test]
    fn enabling_read_receipts_is_opt_in() {
        assert!(!allowed_channel().read_receipts);
        assert!(allowed_channel().with_read_receipts(true).read_receipts);
    }

    #[test]
    fn polling_mode_starts_pending_not_healthy() {
        let channel = SendblueChannel::with_poll_interval(
            "key-id".to_string(),
            "secret-key".to_string(),
            "+15550001111".to_string(),
            "main",
            Arc::new(Vec::new),
            Some(Duration::from_secs(15)),
        );

        assert_eq!(
            channel.listener_health(),
            Some(ListenerHealth::Pending),
            "a listener that has not yet polled is not evidence of health"
        );
    }
}
