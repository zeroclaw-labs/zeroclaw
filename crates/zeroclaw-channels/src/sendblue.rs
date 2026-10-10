use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    /// non-2xx or a timeout, and each poll re-reads an overlap window. The
    /// record is per instance: the gateway and the channel server hold
    /// separate instances, so it does not de-duplicate across the two modes,
    /// and it does not survive a restart.
    seen_handles: Mutex<VecDeque<String>>,
    /// `(succeeded, at)` for the last completed poll exchange. Read by
    /// `listener_health`, which must not perform I/O.
    poll_health: Mutex<Option<(bool, Instant)>>,
    api_base: String,
    client: reqwest::Client,
}

const SENDBLUE_API_BASE: &str = "https://api.sendblue.com/api";

/// Whole-request and connect deadlines for every Sendblue API call, so a
/// blackholed endpoint cannot stall the poller or a send indefinitely.
const API_TIMEOUT_SECS: u64 = 30;
const API_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Read receipts are best effort and run off the dispatch path; this bounds
/// how long one can keep its background task alive.
const READ_RECEIPT_TIMEOUT: Duration = Duration::from_secs(10);

/// One webhook-mode listener's queue, held weakly so the registry never keeps
/// the orchestrator's receiver open after the listener has gone.
struct ForwarderEntry {
    registration: u64,
    sender: tokio::sync::mpsc::WeakSender<ChannelMessage>,
}

/// Per-alias inbound forwarders registered by webhook-mode `listen()`.
///
/// The gateway's `/sendblue` webhook handler and the channel server hold
/// separate `SendblueChannel` instances, so without a bridge, webhook
/// deliveries dispatch through the stateless gateway chat path — no
/// per-sender history, no session persistence, no interrupt-on-new-message.
/// Webhook-mode `listen()` registers its orchestrator queue here so the
/// gateway can hand verified inbound messages to the channel server and get
/// the full conversational treatment while keeping webhook latency.
static INBOUND_FORWARDERS: std::sync::OnceLock<
    Mutex<std::collections::HashMap<String, ForwarderEntry>>,
> = std::sync::OnceLock::new();

static NEXT_FORWARDER_REGISTRATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn inbound_forwarders() -> &'static Mutex<std::collections::HashMap<String, ForwarderEntry>> {
    INBOUND_FORWARDERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Removes its alias's forwarder when the owning `listen()` future ends or is
/// dropped, unless a newer listener has since replaced it.
struct ForwarderRegistration {
    alias: String,
    registration: u64,
}

impl ForwarderRegistration {
    fn register(alias: &str, sender: &tokio::sync::mpsc::Sender<ChannelMessage>) -> Self {
        let registration =
            NEXT_FORWARDER_REGISTRATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        inbound_forwarders().lock().insert(
            alias.to_string(),
            ForwarderEntry {
                registration,
                sender: sender.downgrade(),
            },
        );
        Self {
            alias: alias.to_string(),
            registration,
        }
    }
}

impl Drop for ForwarderRegistration {
    fn drop(&mut self) {
        let mut forwarders = inbound_forwarders().lock();
        if forwarders
            .get(&self.alias)
            .is_some_and(|entry| entry.registration == self.registration)
        {
            forwarders.remove(&self.alias);
        }
    }
}

/// Why [`forward_inbound_to_listener`] did not admit a message. Both variants
/// hand the message back so the caller can choose its own fallback.
#[derive(Debug)]
pub enum ForwardError {
    /// No live webhook-mode listener is registered for the alias.
    NoListener(Box<ChannelMessage>),
    /// The listener's queue is full. The message was not admitted.
    QueueFull(Box<ChannelMessage>),
}

impl ForwardError {
    pub fn into_message(self) -> ChannelMessage {
        match self {
            Self::NoListener(msg) | Self::QueueFull(msg) => *msg,
        }
    }
}

/// Admit one verified inbound message to the channel-server listener for
/// `alias` without waiting. Never blocks: a full queue is reported rather than
/// awaited, so the caller decides before anything can cancel it whether the
/// message was taken.
pub fn forward_inbound_to_listener(alias: &str, msg: ChannelMessage) -> Result<(), ForwardError> {
    let sender = inbound_forwarders()
        .lock()
        .get(alias)
        .and_then(|entry| entry.sender.upgrade());
    let Some(sender) = sender else {
        return Err(ForwardError::NoListener(Box::new(msg)));
    };
    sender.try_send(msg).map_err(|err| match err {
        tokio::sync::mpsc::error::TrySendError::Full(msg) => ForwardError::QueueFull(Box::new(msg)),
        tokio::sync::mpsc::error::TrySendError::Closed(msg) => {
            ForwardError::NoListener(Box::new(msg))
        }
    })
}

/// How long a successful poll stays evidence that the listener works. A
/// blackholed request keeps `listen()` alive with nothing to end it, so a
/// stale success must stop counting.
const POLL_HEALTH_STALE_AFTER: Duration = Duration::from_secs(120);

/// Message handles retained for de-duplication. Each poll re-reads an overlap
/// window, so recently delivered messages come back and are dropped here.
const SEEN_HANDLE_CAP: usize = 2048;

/// The list API's maximum page size.
const POLL_PAGE_LIMIT: usize = 100;

/// Pages drained per poll before yielding to the next interval. Pages are read
/// oldest first and the cursor only covers delivered pages, so a backlog
/// larger than this is finished on later polls rather than skipped.
const POLL_MAX_PAGES: usize = 10;

/// Each poll re-reads this far behind the cursor. `date_sent` is the closest
/// field to the `created_at` the filter uses and Sendblue's clock is not ours;
/// the overlap absorbs both, and `claim_unseen` drops the repeats.
const POLL_OVERLAP: Duration = Duration::from_secs(60);

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
            api_base: SENDBLUE_API_BASE.to_string(),
            client: zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
                "channel.sendblue",
                API_TIMEOUT_SECS,
                API_CONNECT_TIMEOUT_SECS,
            ),
        }
    }

    #[cfg(test)]
    fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
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
    ///
    /// iMessage rich-link previews arrive as `.pluginPayloadAttachment`
    /// blobs — an Apple-internal serialization of the link card, not an
    /// image any model can load. Dropping them keeps the shared URL (which
    /// arrives as message text) as the thing the agent acts on, instead of
    /// a broken attachment it fixates on. Real photos keep their normal
    /// media extensions and pass through untouched.
    fn media_marker(payload: &serde_json::Value) -> Option<String> {
        let url = payload
            .get("media_url")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;

        if url.ends_with(".pluginPayloadAttachment") {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping rich-link preview attachment"
            );
            return None;
        }

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

    /// Drop messages whose `message_handle` this channel instance has already
    /// accepted, and record the rest.
    ///
    /// Sendblue re-delivers a webhook that did not get a 2xx, and each poll
    /// re-reads an overlap window, so without this the agent answers some
    /// messages twice. A caller that claims a message and then fails to admit
    /// it must [`Self::release`] it so the retry is not mistaken for a repeat.
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

    /// Forget claimed handles whose messages were not admitted, so Sendblue's
    /// retry of them is processed rather than dropped as a repeat.
    pub fn release(&self, handles: &[String]) {
        self.seen_handles
            .lock()
            .retain(|seen| !handles.contains(seen));
    }

    /// Mark each distinct sender's conversation read, so they see their
    /// message landed while the agent is still composing. One receipt per
    /// conversation, not per message, because a debounced batch from one
    /// sender is a single conversation.
    ///
    /// Best effort by contract: Sendblue documents no delivery confirmation,
    /// the endpoint is iMessage/RCS only (SMS carries no read state), and it
    /// has to be enabled per account. Each receipt runs in its own bounded
    /// background task, so a slow or failing endpoint cannot hold up dispatch.
    pub fn spawn_read_receipts(&self, messages: &[ChannelMessage]) {
        if !self.read_receipts {
            return;
        }

        let mut marked: Vec<&str> = Vec::new();
        for msg in messages {
            if marked.contains(&msg.reply_target.as_str()) {
                continue;
            }
            marked.push(&msg.reply_target);

            let request = self
                .auth_headers(self.client.post(format!("{}/mark-read", self.api_base)))
                .timeout(READ_RECEIPT_TIMEOUT)
                .json(&::serde_json::json!({
                    "number": Self::normalize_e164(&msg.reply_target),
                    "from_number": self.from_number,
                }));
            zeroclaw_spawn::spawn!(async move {
                let outcome = match request.send().await {
                    Ok(resp) if resp.status().is_success() => return,
                    Ok(resp) => resp.status().to_string(),
                    Err(err) => err.to_string(),
                };
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("mark_read failed: {outcome}")
                );
            });
        }
    }

    /// Fetch one page of inbound messages to this channel's line, recorded at
    /// or after `since`, oldest first.
    ///
    /// Direction and line are filtered by the API so the bot's own sends and
    /// other lines' traffic cannot crowd a page. Returns the raw message
    /// objects so the caller can reuse [`Self::parse_webhook_payload`]: the
    /// polling API and the webhook deliver the same record shape, so both
    /// inbound paths share one parser, one allowlist and one line check.
    async fn fetch_inbound_page(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        offset: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!("{}/v2/messages", self.api_base);

        let resp = self
            .auth_headers(self.client.get(&url))
            .query(&[
                ("is_outbound", "false".to_string()),
                ("sendblue_number", self.from_number.clone()),
                (
                    "created_at_gte",
                    since.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                ),
                ("order_by", "createdAt".to_string()),
                ("order_direction", "asc".to_string()),
                ("limit", POLL_PAGE_LIMIT.to_string()),
                ("offset", offset.to_string()),
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
            .cloned()
            .unwrap_or_default())
    }

    /// One poll: drain the ordered window from `cursor - POLL_OVERLAP`, page
    /// by page, handing each accepted message to `tx`.
    ///
    /// `cursor` advances only past pages whose messages have all been handed
    /// over, so an error or the page cap leaves the rest for the next poll
    /// instead of skipping it. Returns `Ok(false)` when the receiver is gone.
    async fn poll_once(
        &self,
        cursor: &mut chrono::DateTime<chrono::Utc>,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<bool> {
        let overlap = chrono::Duration::from_std(POLL_OVERLAP).unwrap_or_default();
        let since = *cursor - overlap;

        for page_index in 0..POLL_MAX_PAGES {
            let page = self
                .fetch_inbound_page(since, page_index * POLL_PAGE_LIMIT)
                .await?;

            let mut newest = *cursor;
            let mut accepted = Vec::new();
            for payload in &page {
                if let Some(sent_at) = payload
                    .get("date_sent")
                    .and_then(|value| value.as_str())
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                {
                    newest = newest.max(sent_at.with_timezone(&chrono::Utc));
                }
                accepted.extend(self.claim_unseen(self.parse_webhook_payload(payload)));
            }

            self.spawn_read_receipts(&accepted);
            for msg in accepted {
                if tx.send(msg).await.is_err() {
                    return Ok(false);
                }
            }
            *cursor = newest;

            if page.len() < POLL_PAGE_LIMIT {
                break;
            }
        }

        Ok(true)
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

        // Webhooks are registered per account and the list API is account
        // wide, so every line's traffic reaches every alias on the account.
        // Only the alias that owns the destination line may answer it.
        let line = ["sendblue_number", "to_number"].iter().find_map(|field| {
            data.get(*field)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
        let Some(line) = line else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"alias": self.alias})),
                "skipping message with no destination line"
            );
            return messages;
        };
        if Self::normalize_e164(line) != Self::normalize_e164(&self.from_number) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"alias": self.alias})),
                "skipping message addressed to another Sendblue line"
            );
            return messages;
        }

        // `message_handle` is the idempotency key. An event without one would
        // get a fresh identity on every retry and evade de-duplication.
        let Some(id) = data
            .get("message_handle")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"alias": self.alias})),
                "skipping message with no message_handle"
            );
            return messages;
        };

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
            .auth_headers(self.client.post(format!("{}/send-message", self.api_base)))
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

            // Publish this listener's queue so the gateway webhook handler can
            // route verified inbound messages through the channel server
            // (per-sender history, session persistence, interrupts) instead of
            // the stateless gateway chat path. The registration is withdrawn
            // when this future ends or is dropped.
            let _registration = ForwarderRegistration::register(&self.alias, &tx);
            tx.closed().await;
            return Ok(());
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"interval_secs": interval.as_secs()})),
            "channel active (polling mode)"
        );

        // Only messages that arrive from about now on are ours to answer;
        // replaying the account's backlog on every restart would re-answer old
        // texts.
        let mut cursor = chrono::Utc::now();

        loop {
            tokio::time::sleep(interval).await;

            match self.poll_once(&mut cursor, &tx).await {
                Ok(true) => *self.poll_health.lock() = Some((true, Instant::now())),
                // Receiver dropped: the orchestrator is shutting this channel
                // down.
                Ok(false) => return Ok(()),
                Err(err) => {
                    *self.poll_health.lock() = Some((false, Instant::now()));
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": err.to_string()})),
                        "poll failed"
                    );
                }
            }
        }
    }

    fn listener_health(&self) -> Option<ListenerHealth> {
        // Webhook mode does no exchange of its own. `None` keeps the
        // supervisor's default meaning — the listener is alive, which here
        // means its queue is registered for the gateway to forward into —
        // rather than claiming a probe this mode never runs.
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
        let url = format!("{}/v2/messages?limit=1", self.api_base);

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
                    .post(format!("{}/send-typing-indicator", self.api_base)),
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
            "sendblue_number": "+15550001111",
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
    fn drops_a_rich_link_preview_attachment_but_keeps_the_url_text() {
        let mut payload = inbound("https://maps.app.goo.gl/abc123");
        payload["media_url"] = serde_json::json!(
            "https://storage.googleapis.com/inbound-file-store/x_ABC.pluginPayloadAttachment"
        );

        let msgs = allowed_channel().parse_webhook_payload(&payload);

        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].content, "https://maps.app.goo.gl/abc123",
            "the shared URL is the actionable part; the preview blob is not loadable"
        );
    }

    #[test]
    fn drops_a_preview_only_fragment_entirely() {
        let mut payload = inbound("");
        payload["media_url"] = serde_json::json!(
            "https://storage.googleapis.com/inbound-file-store/x_ABC.pluginPayloadAttachment"
        );

        assert!(
            allowed_channel().parse_webhook_payload(&payload).is_empty(),
            "a preview blob with no text carries no user intent"
        );
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
        // to use receipts must not reach for it.
        let _http = HTTP_TEST_LOCK.lock().await;
        let server = wiremock::MockServer::start().await;
        let channel = allowed_channel().with_api_base(server.uri());

        channel.spawn_read_receipts(&channel.parse_webhook_payload(&inbound("hi")));
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "receipts disabled must not send any request"
        );
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

    /// Serializes tests that make HTTP requests, because one of them points
    /// the process-global runtime proxy at a test server.
    static HTTP_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn line_channel(line: &str, alias: &str) -> SendblueChannel {
        SendblueChannel::new(
            "key-id".to_string(),
            "secret-key".to_string(),
            line.to_string(),
            alias,
            Arc::new(|| vec!["+447700900123".to_string()]),
        )
    }

    #[test]
    fn only_the_alias_owning_the_destination_line_accepts_a_message() {
        let line_a = line_channel("+15550001111", "a");
        let line_b = line_channel("+15550002222", "b");
        let payload = inbound("for line a");

        let accepted_a = line_a.parse_webhook_payload(&payload);
        assert_eq!(accepted_a.len(), 1);
        assert_eq!(accepted_a[0].channel_alias.as_deref(), Some("a"));
        assert!(
            line_b.parse_webhook_payload(&payload).is_empty(),
            "an account-wide event for line A must not invoke or reply through alias B"
        );
    }

    #[test]
    fn falls_back_to_to_number_for_the_destination_line() {
        let mut payload = inbound("hi");
        payload.as_object_mut().unwrap().remove("sendblue_number");
        payload["to_number"] = serde_json::json!("15550001111");

        assert_eq!(allowed_channel().parse_webhook_payload(&payload).len(), 1);
    }

    #[test]
    fn drops_a_message_with_no_destination_line() {
        let mut payload = inbound("hi");
        let object = payload.as_object_mut().unwrap();
        object.remove("sendblue_number");
        object.remove("to_number");

        assert!(allowed_channel().parse_webhook_payload(&payload).is_empty());
    }

    #[test]
    fn drops_a_message_with_no_message_handle() {
        let mut payload = inbound("hi");
        payload.as_object_mut().unwrap().remove("message_handle");

        assert!(
            allowed_channel().parse_webhook_payload(&payload).is_empty(),
            "an invented id would evade de-duplication on every retry"
        );
    }

    #[test]
    fn a_released_handle_can_be_claimed_again() {
        let channel = allowed_channel();
        let first = channel.claim_unseen(channel.parse_webhook_payload(&inbound("hello")));
        assert_eq!(first.len(), 1);

        channel.release(&[first[0].id.clone()]);

        assert_eq!(
            channel
                .claim_unseen(channel.parse_webhook_payload(&inbound("hello")))
                .len(),
            1,
            "a message that was never admitted must be accepted on retry"
        );
    }

    fn page_record(index: usize, sent_at: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
        let mut record = inbound(&format!("message {index}"));
        record["message_handle"] = serde_json::json!(format!("handle-{index}"));
        record["date_sent"] =
            serde_json::json!(sent_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        record
    }

    fn page_body(records: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "status": "OK", "data": records })
    }

    fn drain(rx: &mut tokio::sync::mpsc::Receiver<ChannelMessage>) -> Vec<String> {
        let mut contents = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            contents.push(msg.content);
        }
        contents
    }

    #[tokio::test]
    async fn polling_drains_every_page_in_order_without_duplicates() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        let _http = HTTP_TEST_LOCK.lock().await;
        let server = wiremock::MockServer::start().await;
        let start = chrono::SubsecRound::trunc_subsecs(chrono::Utc::now(), 3);
        let records: Vec<_> = (0..150)
            .map(|i| page_record(i, start + chrono::Duration::milliseconds(i as i64)))
            .collect();

        // Direction and line are filtered at the API, so outbound traffic
        // cannot fill a page and hide an inbound record behind it.
        let base = || {
            Mock::given(method("GET"))
                .and(path("/v2/messages"))
                .and(query_param("is_outbound", "false"))
                .and(query_param("sendblue_number", "+15550001111"))
                .and(query_param("order_direction", "asc"))
                .and(query_param("limit", "100"))
        };
        base()
            .and(query_param("offset", "0"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(page_body(records[..100].to_vec())),
            )
            .mount(&server)
            .await;
        base()
            .and(query_param("offset", "100"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(page_body(records[100..].to_vec())),
            )
            .mount(&server)
            .await;

        let channel = allowed_channel().with_api_base(server.uri());
        let (tx, mut rx) = tokio::sync::mpsc::channel(512);
        let mut cursor = start;

        assert!(channel.poll_once(&mut cursor, &tx).await.unwrap());
        let expected: Vec<String> = (0..150).map(|i| format!("message {i}")).collect();
        assert_eq!(drain(&mut rx), expected, "both pages, oldest first");
        assert_eq!(cursor, start + chrono::Duration::milliseconds(149));

        // The next poll re-reads the overlap window and gets the same records
        // back; none of them may be dispatched again.
        assert!(channel.poll_once(&mut cursor, &tx).await.unwrap());
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn a_failed_page_leaves_the_cursor_behind_the_undelivered_records() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        let _http = HTTP_TEST_LOCK.lock().await;
        let server = wiremock::MockServer::start().await;
        let start = chrono::SubsecRound::trunc_subsecs(chrono::Utc::now(), 3);
        let records: Vec<_> = (0..120)
            .map(|i| page_record(i, start + chrono::Duration::milliseconds(i as i64)))
            .collect();

        Mock::given(method("GET"))
            .and(path("/v2/messages"))
            .and(query_param("offset", "0"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(page_body(records[..100].to_vec())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/messages"))
            .and(query_param("offset", "100"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/messages"))
            .and(query_param("offset", "100"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(page_body(records[100..].to_vec())),
            )
            .mount(&server)
            .await;

        let channel = allowed_channel().with_api_base(server.uri());
        let (tx, mut rx) = tokio::sync::mpsc::channel(512);
        let mut cursor = start;

        assert!(channel.poll_once(&mut cursor, &tx).await.is_err());
        assert_eq!(
            drain(&mut rx).len(),
            100,
            "the delivered page stays delivered"
        );
        assert_eq!(
            cursor,
            start + chrono::Duration::milliseconds(99),
            "the cursor covers only the page that was handed over"
        );

        assert!(channel.poll_once(&mut cursor, &tx).await.unwrap());
        let expected: Vec<String> = (100..120).map(|i| format!("message {i}")).collect();
        assert_eq!(
            drain(&mut rx),
            expected,
            "the failed page is recovered, once"
        );
    }

    #[tokio::test]
    async fn a_stalled_read_receipt_does_not_delay_dispatch() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let _http = HTTP_TEST_LOCK.lock().await;
        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mark-read"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(page_body(vec![page_record(0, chrono::Utc::now())])),
            )
            .mount(&server)
            .await;

        let channel = allowed_channel()
            .with_read_receipts(true)
            .with_api_base(server.uri());
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut cursor = chrono::Utc::now();

        tokio::time::timeout(Duration::from_secs(5), channel.poll_once(&mut cursor, &tx))
            .await
            .expect("a blackholed mark-read endpoint must not hold up the poll")
            .unwrap();
        assert_eq!(drain(&mut rx), vec!["message 0".to_string()]);
    }

    #[tokio::test]
    async fn api_calls_go_through_the_runtime_proxy() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        use zeroclaw_config::schema::{ProxyConfig, ProxyScope, set_runtime_proxy_config};

        let _http = HTTP_TEST_LOCK.lock().await;
        let proxy = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page_body(vec![])))
            .expect(1)
            .mount(&proxy)
            .await;

        set_runtime_proxy_config(ProxyConfig {
            enabled: true,
            http_proxy: Some(proxy.uri()),
            scope: ProxyScope::Services,
            services: vec!["channel.sendblue".to_string()],
            ..Default::default()
        });
        // `.invalid` never resolves, so the request can only succeed if the
        // client hands it to the configured proxy.
        let channel = allowed_channel().with_api_base("http://sendblue.invalid/api");
        set_runtime_proxy_config(ProxyConfig::default());

        assert!(
            channel.health_check().await,
            "the probe must reach the proxy"
        );
    }

    fn webhook_channel(alias: &str) -> Arc<SendblueChannel> {
        Arc::new(SendblueChannel::new(
            "key-id".to_string(),
            "secret-key".to_string(),
            "+15550001111".to_string(),
            alias,
            Arc::new(|| vec!["+447700900123".to_string()]),
        ))
    }

    fn forwarded_message(alias: &str) -> ChannelMessage {
        webhook_channel(alias).parse_webhook_payload(&inbound("forwarded"))[0].clone()
    }

    #[tokio::test]
    async fn a_cancelled_webhook_listener_releases_the_dispatch_queue() {
        let alias = "forwarder-cancel";
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let channel = webhook_channel(alias);
        let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });

        tokio::time::timeout(Duration::from_secs(2), async {
            while inbound_forwarders().lock().get(alias).is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("listener registers its forwarder");
        forward_inbound_to_listener(alias, forwarded_message(alias)).unwrap();
        assert!(rx.recv().await.is_some());

        listener.abort();
        let _ = listener.await;

        assert!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("the receiver must close once the listener is gone")
                .is_none(),
            "nothing may keep the orchestrator's receiver open after cancellation"
        );
        assert!(matches!(
            forward_inbound_to_listener(alias, forwarded_message(alias)),
            Err(ForwardError::NoListener(_))
        ));
    }

    #[tokio::test]
    async fn a_replaced_listener_keeps_its_successors_registration() {
        let alias = "forwarder-replace";
        let (old_tx, _old_rx) = tokio::sync::mpsc::channel(8);
        let (new_tx, mut new_rx) = tokio::sync::mpsc::channel(8);

        let old = ForwarderRegistration::register(alias, &old_tx);
        let _new = ForwarderRegistration::register(alias, &new_tx);
        drop(old);

        forward_inbound_to_listener(alias, forwarded_message(alias))
            .expect("the newer listener must still receive forwards");
        assert!(new_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn a_full_listener_queue_is_reported_not_awaited() {
        let alias = "forwarder-full";
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let _registration = ForwarderRegistration::register(alias, &tx);

        forward_inbound_to_listener(alias, forwarded_message(alias)).unwrap();
        let err = forward_inbound_to_listener(alias, forwarded_message(alias))
            .expect_err("the second message does not fit");

        assert!(matches!(err, ForwardError::QueueFull(_)));
        assert_eq!(err.into_message().content, "forwarded");
    }
}
