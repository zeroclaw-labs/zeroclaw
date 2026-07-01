use async_trait::async_trait;
use futures_util::StreamExt;
use lru::LruCache;
use parking_lot::Mutex as SyncMutex;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

const GROUP_TARGET_PREFIX: &str = "group:";

/// How many recent inbound messages we remember for the purpose of
/// addressing outbound reactions back at them. signal-cli's `sendReaction`
/// is keyed on `(targetAuthor, targetTimestamp)`, but we don't want those
/// values to leak into the generic `ChannelMessage.id` (which flows into
/// logs, memory keys, thread roots, and tool args). Instead we mint an
/// opaque id and remember the mapping channel-locally.
const RECENT_TARGETS_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecipientTarget {
    Direct(String),
    Group(String),
}

/// `(targetAuthor, targetTimestamp_ms)` recovered by `add_reaction` /
/// `remove_reaction` from an opaque inbound id. Held in `recent_targets`.
#[derive(Debug, Clone)]
struct ReactionTarget {
    author: String,
    timestamp_ms: u64,
}

/// Signal channel using signal-cli daemon's native JSON-RPC + SSE API.
///
/// Connects to a running `signal-cli daemon --http <host:port>`.
/// Listens via SSE at `/api/v1/events` and sends via JSON-RPC at
/// `/api/v1/rpc`.
#[derive(Clone)]
pub struct SignalChannel {
    http_url: String,
    account: String,
    /// Empty = no group filter (all groups accepted).
    group_ids: Vec<String>,
    /// When true, accept only DMs and reject all group traffic.
    dm_only: bool,
    /// The alias key under `[channels.signal.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ignore_attachments: bool,
    ignore_stories: bool,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
    /// Seconds to wait for an operator reply to a `request_approval` prompt
    /// before treating the silence as a deny. Default 300.
    approval_timeout_secs: u64,
    /// Opaque inbound message id → `(targetAuthor, targetTimestamp)` so
    /// outbound reactions can be addressed without embedding the Signal
    /// sender (E.164 phone number or UUID) in `ChannelMessage.id`. Bounded
    /// LRU; once a message ages out, reactions against it fail cleanly.
    recent_targets: Arc<SyncMutex<LruCache<String, ReactionTarget>>>,
}

// ── signal-cli SSE event JSON shapes ────────────────────────────

#[derive(Debug, Deserialize)]
struct SseEnvelope {
    #[serde(default)]
    envelope: Option<Envelope>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    source: Option<String>,
    #[serde(rename = "sourceNumber", default)]
    source_number: Option<String>,
    #[serde(rename = "dataMessage", default)]
    data_message: Option<DataMessage>,
    #[serde(rename = "storyMessage", default)]
    story_message: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(rename = "groupInfo", default)]
    group_info: Option<GroupInfo>,
    #[serde(default)]
    attachments: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    #[serde(rename = "groupId", default)]
    group_id: Option<String>,
}

impl SignalChannel {
    pub fn new(
        http_url: String,
        account: String,
        group_ids: Vec<String>,
        dm_only: bool,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        ignore_attachments: bool,
        ignore_stories: bool,
    ) -> Self {
        let http_url = http_url.trim_end_matches('/').to_string();
        Self {
            http_url,
            account,
            group_ids,
            dm_only,
            alias: alias.into(),
            peer_resolver,
            ignore_attachments,
            ignore_stories,
            proxy_url: None,
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            approval_timeout_secs: 300,
            recent_targets: Arc::new(SyncMutex::new(LruCache::new(
                NonZeroUsize::new(RECENT_TARGETS_CAPACITY)
                    .expect("RECENT_TARGETS_CAPACITY is a non-zero constant"),
            ))),
        }
    }

    /// Return the alias under `[channels.signal.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    pub fn with_approval_timeout_secs(mut self, secs: u64) -> Self {
        self.approval_timeout_secs = secs;
        self
    }

    fn http_client(&self) -> Client {
        let builder = Client::builder().connect_timeout(Duration::from_secs(10));
        let builder = zeroclaw_config::schema::apply_channel_proxy_to_builder(
            builder,
            "channel.signal",
            self.proxy_url.as_deref(),
        );
        builder.build().expect("Signal HTTP client should build")
    }

    /// Effective sender: prefer `sourceNumber` (E.164), fall back to `source`.
    fn sender(envelope: &Envelope) -> Option<String> {
        envelope
            .source_number
            .as_deref()
            .or(envelope.source.as_deref())
            .map(String::from)
    }

    fn is_sender_allowed(&self, sender: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, sender, crate::allowlist::Match::Sensitive)
    }

    fn is_e164(recipient: &str) -> bool {
        let Some(number) = recipient.strip_prefix('+') else {
            return false;
        };
        (2..=15).contains(&number.len()) && number.chars().all(|c| c.is_ascii_digit())
    }

    /// Check whether a string is a valid UUID (signal-cli uses these for
    /// privacy-enabled users who have opted out of sharing their phone number).
    fn is_uuid(s: &str) -> bool {
        Uuid::parse_str(s).is_ok()
    }

    fn parse_recipient_target(recipient: &str) -> RecipientTarget {
        if let Some(group_id) = recipient.strip_prefix(GROUP_TARGET_PREFIX) {
            return RecipientTarget::Group(group_id.to_string());
        }

        if Self::is_e164(recipient) || Self::is_uuid(recipient) {
            RecipientTarget::Direct(recipient.to_string())
        } else {
            RecipientTarget::Group(recipient.to_string())
        }
    }

    /// Build the JSON-RPC params for signal-cli's `sendReaction` method.
    ///
    /// `targetAuthor` and `targetTimestamp` are recovered from
    /// `recent_targets` rather than parsed out of `message_id`, so the
    /// generic id stays opaque and the Signal sender never leaks into
    /// the surfaces that consume `ChannelMessage.id`.
    ///
    /// Extracted from `add_reaction` / `remove_reaction` so the wire
    /// shape is unit-testable without a live daemon.
    fn build_reaction_params(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
        remove: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let target = self.recent_targets.lock().get(message_id).cloned().ok_or_else(|| {
            anyhow::Error::msg(format!(
                "no recent inbound Signal message matches id {message_id} — may have been evicted from the lookup cache or never received"
            ))
        })?;

        let params = match Self::parse_recipient_target(channel_id) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "emoji": emoji,
                "targetAuthor": target.author,
                "targetTimestamp": target.timestamp_ms,
                "remove": remove,
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "emoji": emoji,
                "targetAuthor": target.author,
                "targetTimestamp": target.timestamp_ms,
                "remove": remove,
                "account": &self.account,
            }),
        };

        Ok(params)
    }

    /// Check whether the message passes the group/DM filter.
    ///
    /// - `dm_only = true`: only DMs accepted; all group messages rejected.
    /// - `dm_only = false`, `group_ids` empty: accept all (DMs and any group).
    /// - `dm_only = false`, `group_ids` non-empty: accept DMs and listed
    ///   groups only.
    fn matches_group(&self, data_msg: &DataMessage) -> bool {
        let incoming_group = data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref());

        if self.dm_only {
            return incoming_group.is_none();
        }

        if self.group_ids.is_empty() {
            return true;
        }

        match incoming_group {
            Some(gid) => self.group_ids.iter().any(|allowed| allowed == gid),
            None => true,
        }
    }

    /// Determine the send target: group id or the sender's number.
    fn reply_target(&self, data_msg: &DataMessage, sender: &str) -> String {
        if let Some(group_id) = data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref())
        {
            format!("{GROUP_TARGET_PREFIX}{group_id}")
        } else {
            sender.to_string()
        }
    }

    /// Send a JSON-RPC request to signal-cli daemon.
    async fn rpc_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let url = format!("{}/api/v1/rpc", self.http_url);
        let id = Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        let resp = self
            .http_client()
            .post(&url)
            .timeout(Duration::from_secs(30))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        // 201 = success with no body (e.g. typing indicators)
        if resp.status().as_u16() == 201 {
            return Ok(None);
        }

        let text = resp.text().await?;
        if text.is_empty() {
            return Ok(None);
        }

        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Signal RPC error {code}: {msg}");
        }

        Ok(parsed.get("result").cloned())
    }

    /// Process a single SSE envelope, returning a ChannelMessage if valid.
    fn process_envelope(&self, envelope: &Envelope) -> Option<ChannelMessage> {
        // Skip story messages when configured
        if self.ignore_stories && envelope.story_message.is_some() {
            return None;
        }

        let data_msg = envelope.data_message.as_ref()?;

        // Skip attachment-only messages when configured
        if self.ignore_attachments {
            let has_attachments = data_msg.attachments.as_ref().is_some_and(|a| !a.is_empty());
            if has_attachments && data_msg.message.is_none() {
                return None;
            }
        }

        let text = data_msg.message.as_deref().filter(|t| !t.is_empty())?;
        let sender = Self::sender(envelope)?;

        if !self.is_sender_allowed(&sender) {
            return None;
        }

        if !self.matches_group(data_msg) {
            return None;
        }

        let target = self.reply_target(data_msg, &sender);

        let timestamp = data_msg
            .timestamp
            .or(envelope.timestamp)
            .unwrap_or_else(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });

        // Opaque id: timestamp is convenient for debugging, the random
        // suffix disambiguates two senders that happen to post at the same
        // millisecond in a group. Crucially, neither component reveals the
        // sender — that lives only in the channel-local `recent_targets`
        // map and the `sender` field on `ChannelMessage`.
        let id = format!("sig_{timestamp}_{}", Self::random_id_suffix());
        self.recent_targets.lock().put(
            id.clone(),
            ReactionTarget {
                author: sender.clone(),
                timestamp_ms: timestamp,
            },
        );

        Some(ChannelMessage {
            id,
            sender: sender.clone(),
            reply_target: target,
            content: text.to_string(),
            channel: "signal".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp: timestamp / 1000, // millis → secs
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
    }

    fn random_id_suffix() -> String {
        use rand::RngExt;
        const CHARSET: &[u8] = b"0123456789abcdef";
        let mut rng = rand::rng();
        (0..6)
            .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
            .collect()
    }
}

impl ::zeroclaw_api::attribution::Attributable for SignalChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Signal)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn name(&self) -> &str {
        "signal"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let params = match Self::parse_recipient_target(&message.recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "message": &message.content,
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "message": &message.content,
                "account": &self.account,
            }),
        };

        self.rpc_request("send", params).await?;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let mut url = reqwest::Url::parse(&format!("{}/api/v1/events", self.http_url))?;
        url.query_pairs_mut().append_pair("account", &self.account);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("channel listening via SSE on {}...", self.http_url)
        );

        let mut retry_delay_secs = 2u64;
        let max_delay_secs = 60u64;

        loop {
            let resp = self
                .http_client()
                .get(url.clone())
                .header("Accept", "text/event-stream")
                .send()
                .await;

            let resp = match resp {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"status": status.to_string(), "body": body})
                            ),
                        "SSE returned"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    retry_delay_secs = (retry_delay_secs * 2).min(max_delay_secs);
                    continue;
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "SSE connect error, retrying..."
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    retry_delay_secs = (retry_delay_secs * 2).min(max_delay_secs);
                    continue;
                }
            };

            retry_delay_secs = 2;

            let mut bytes_stream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut current_data = String::new();

            while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "SSE chunk error, reconnecting"
                        );
                        break;
                    }
                };

                let text = match String::from_utf8(chunk.to_vec()) {
                    Ok(t) => t,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "SSE invalid UTF-8, skipping chunk"
                        );
                        continue;
                    }
                };

                buffer.push_str(&text);

                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    // Skip SSE comments (keepalive)
                    if line.starts_with(':') {
                        continue;
                    }

                    if line.is_empty() {
                        // Empty line = event boundary, dispatch accumulated data
                        if !current_data.is_empty() {
                            match serde_json::from_str::<SseEnvelope>(&current_data) {
                                Ok(sse) => {
                                    if let Some(ref envelope) = sse.envelope
                                        && let Some(msg) = self.process_envelope(envelope)
                                    {
                                        if let Some((token, response)) =
                                            crate::util::parse_approval_reply(&msg.content)
                                        {
                                            let mut map = self.pending_approvals.lock().await;
                                            if let Some(sender) = map.remove(&token) {
                                                let _ = sender.send(response);
                                                current_data.clear();
                                                continue;
                                            }
                                        }
                                        if tx.send(msg).await.is_err() {
                                            return Ok(());
                                        }
                                    }
                                }
                                Err(e) => {
                                    ::zeroclaw_log::record!(
                                        DEBUG,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_attrs(
                                            ::serde_json::json!({"error": format!("{}", e)})
                                        ),
                                        "SSE parse skip"
                                    );
                                }
                            }
                            current_data.clear();
                        }
                    } else if let Some(data) = line.strip_prefix("data:") {
                        if !current_data.is_empty() {
                            current_data.push('\n');
                        }
                        current_data.push_str(data.trim_start());
                    }
                    // Ignore "event:", "id:", "retry:" lines
                }
            }

            if !current_data.is_empty() {
                match serde_json::from_str::<SseEnvelope>(&current_data) {
                    Ok(sse) => {
                        if let Some(ref envelope) = sse.envelope
                            && let Some(msg) = self.process_envelope(envelope)
                        {
                            if let Some((token, response)) =
                                crate::util::parse_approval_reply(&msg.content)
                            {
                                let mut map = self.pending_approvals.lock().await;
                                if let Some(sender) = map.remove(&token) {
                                    let _ = sender.send(response);
                                    continue;
                                }
                            }
                            let _ = tx.send(msg).await;
                        }
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "SSE trailing parse skip"
                        );
                    }
                }
            }

            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "SSE stream ended, reconnecting..."
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/api/v1/check", self.http_url);
        let Ok(resp) = self
            .http_client()
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        else {
            return false;
        };
        resp.status().is_success()
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let params = match Self::parse_recipient_target(recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "account": &self.account,
            }),
        };
        self.rpc_request("sendTyping", params).await?;
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // signal-cli doesn't have a stop-typing RPC; typing indicators
        // auto-expire after ~15s on the client side.
        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let params = self.build_reaction_params(channel_id, message_id, emoji, false)?;
        self.rpc_request("sendReaction", params).await?;
        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let params = self.build_reaction_params(channel_id, message_id, emoji, true)?;
        self.rpc_request("sendReaction", params).await?;
        Ok(())
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let token = crate::util::new_approval_token();
        let text = format!(
            "APPROVAL REQUIRED [{}]\nTool: {}\nArgs: {}\n\nReply: \"{} yes\", \"{} no\", or \"{} always\"",
            token, request.tool_name, request.arguments_summary, token, token, token,
        );

        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(token.clone(), tx);

        if let Err(err) = self.send(&SendMessage::new(text, recipient)).await {
            self.pending_approvals.lock().await.remove(&token);
            return Err(err);
        }

        let response =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), rx).await {
                Ok(Ok(resp)) => resp,
                _ => {
                    self.pending_approvals.lock().await.remove(&token);
                    ChannelApprovalResponse::Deny
                }
            };
        Ok(Some(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_envelope(source_number: Option<&str>, message: Option<&str>) -> Envelope {
        Envelope {
            source: source_number.map(String::from),
            source_number: source_number.map(String::from),
            data_message: message.map(|m| DataMessage {
                message: Some(m.to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn creates_with_correct_fields() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
        assert_eq!(ch.account, "+1234567890");
        assert!(ch.group_ids.is_empty());
        assert!(!ch.dm_only);
        assert!(ch.is_sender_allowed("+1111111111"));
        assert!(!ch.ignore_attachments);
        assert!(!ch.ignore_stories);
    }

    #[test]
    fn strips_trailing_slash() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686/".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(Vec::new),
            ignore_attachments,
            ignore_stories,
        );
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
    }

    #[test]
    fn wildcard_allows_anyone() {
        let dm_only = true;
        let ignore_attachments = true;
        let ignore_stories = true;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        assert!(ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn specific_sender_allowed() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        assert!(ch.is_sender_allowed("+1111111111"));
    }

    #[test]
    fn unknown_sender_denied() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        assert!(!ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(Vec::new),
            ignore_attachments,
            ignore_stories,
        );
        assert!(!ch.is_sender_allowed("+1111111111"));
    }

    #[test]
    fn name_returns_signal() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        assert_eq!(ch.name(), "signal");
    }

    #[test]
    fn matches_group_no_group_id_accepts_all() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(ch.matches_group(&group));
    }

    #[test]
    fn matches_group_filters_group() {
        let dm_only = false;
        let ignore_attachments = true;
        let ignore_stories = true;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            vec!["group123".to_string()],
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(ch.matches_group(&matching));

        let non_matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("other_group".to_string()),
            }),
            attachments: None,
        };
        assert!(!ch.matches_group(&non_matching));
    }

    #[test]
    fn matches_group_dm_keyword() {
        let dm_only = true;
        let ignore_attachments = true;
        let ignore_stories = true;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(!ch.matches_group(&group));
    }

    #[test]
    fn reply_target_dm() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert_eq!(ch.reply_target(&dm, "+1111111111"), "+1111111111");
    }

    #[test]
    fn reply_target_group() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert_eq!(ch.reply_target(&group, "+1111111111"), "group:group123");
    }

    #[test]
    fn parse_recipient_target_e164_is_direct() {
        assert_eq!(
            SignalChannel::parse_recipient_target("+1234567890"),
            RecipientTarget::Direct("+1234567890".to_string())
        );
    }

    #[test]
    fn parse_recipient_target_prefixed_group_is_group() {
        assert_eq!(
            SignalChannel::parse_recipient_target("group:abc123"),
            RecipientTarget::Group("abc123".to_string())
        );
    }

    #[test]
    fn parse_recipient_target_uuid_is_direct() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        assert_eq!(
            SignalChannel::parse_recipient_target(uuid),
            RecipientTarget::Direct(uuid.to_string())
        );
    }

    #[test]
    fn parse_recipient_target_non_e164_plus_is_group() {
        assert_eq!(
            SignalChannel::parse_recipient_target("+abc123"),
            RecipientTarget::Group("+abc123".to_string())
        );
    }

    #[test]
    fn is_uuid_valid() {
        assert!(SignalChannel::is_uuid(
            "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        ));
        assert!(SignalChannel::is_uuid(
            "00000000-0000-0000-0000-000000000000"
        ));
    }

    #[test]
    fn is_uuid_invalid() {
        assert!(!SignalChannel::is_uuid("+1234567890"));
        assert!(!SignalChannel::is_uuid("not-a-uuid"));
        assert!(!SignalChannel::is_uuid("group:abc123"));
        assert!(!SignalChannel::is_uuid(""));
    }

    #[test]
    fn sender_prefers_source_number() {
        let env = Envelope {
            source: Some("uuid-123".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: None,
            story_message: None,
            timestamp: Some(1000),
        };
        assert_eq!(SignalChannel::sender(&env), Some("+1111111111".to_string()));
    }

    #[test]
    fn sender_falls_back_to_source() {
        let env = Envelope {
            source: Some("uuid-123".to_string()),
            source_number: None,
            data_message: None,
            story_message: None,
            timestamp: Some(1000),
        };
        assert_eq!(SignalChannel::sender(&env), Some("uuid-123".to_string()));
    }

    #[test]
    fn process_envelope_uuid_sender_dm() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Hello from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, uuid);
        assert_eq!(msg.reply_target, uuid);
        assert_eq!(msg.content, "Hello from privacy user");
        assert!(
            msg.id.starts_with("sig_1700000000000_"),
            "id should embed timestamp but stay opaque: {}",
            msg.id
        );
        // Privacy regression: the routing identity must not appear in the
        // generic message id, which flows into logs, memory keys, and the
        // LLM-facing tool context.
        assert!(
            !msg.id.contains(uuid),
            "UUID sender must not leak into msg.id: {}",
            msg.id
        );
        assert_eq!(msg.timestamp, 1_700_000_000);
        assert_eq!(msg.channel_alias.as_deref(), Some("signal_test_alias"));

        // Verify reply routing: UUID sender in DM should route as Direct
        let target = SignalChannel::parse_recipient_target(&msg.reply_target);
        assert_eq!(target, RecipientTarget::Direct(uuid.to_string()));
    }

    #[test]
    fn process_envelope_uuid_sender_in_group() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            vec!["testgroup".to_string()],
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Group msg from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: Some(GroupInfo {
                    group_id: Some("testgroup".to_string()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, uuid);
        assert_eq!(msg.reply_target, "group:testgroup");

        // Verify reply routing: group message should still route as Group
        let target = SignalChannel::parse_recipient_target(&msg.reply_target);
        assert_eq!(target, RecipientTarget::Group("testgroup".to_string()));
    }

    #[test]
    fn sender_none_when_both_missing() {
        let env = Envelope {
            source: None,
            source_number: None,
            data_message: None,
            story_message: None,
            timestamp: None,
        };
        assert_eq!(SignalChannel::sender(&env), None);
    }

    #[test]
    fn process_envelope_valid_dm() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = make_envelope(Some("+1111111111"), Some("Hello!"));
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.content, "Hello!");
        assert_eq!(msg.sender, "+1111111111");
        assert_eq!(msg.channel, "signal");
        assert!(
            msg.id.starts_with("sig_1700000000000_"),
            "id should embed timestamp but stay opaque: {}",
            msg.id
        );
        // Privacy regression: the E.164 phone number must not appear in
        // the generic message id, which flows into logs, memory keys, and
        // the LLM-facing tool context.
        assert!(
            !msg.id.contains("+1111111111"),
            "E.164 sender must not leak into msg.id: {}",
            msg.id
        );
        assert_eq!(msg.timestamp, 1_700_000_000);
        assert_eq!(msg.channel_alias.as_deref(), Some("signal_test_alias"));
    }

    #[test]
    fn process_envelope_denied_sender() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = make_envelope(Some("+9999999999"), Some("Hello!"));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_empty_message() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = make_envelope(Some("+1111111111"), Some(""));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_no_data_message() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = make_envelope(Some("+1111111111"), None);
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_skips_stories() {
        let dm_only = true;
        let ignore_attachments = true;
        let ignore_stories = true;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let mut env = make_envelope(Some("+1111111111"), Some("story text"));
        env.story_message = Some(serde_json::json!({}));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_skips_attachment_only() {
        let dm_only = true;
        let ignore_attachments = true;
        let ignore_stories = true;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![serde_json::json!({"contentType": "image/png"})]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_group_happy_path() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            vec!["group_xyz".to_string()],
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("group hello".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: Some(GroupInfo {
                    group_id: Some("group_xyz".to_string()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, "+1111111111");
        assert_eq!(msg.reply_target, "group:group_xyz");
        assert_eq!(msg.content, "group hello");
        assert_eq!(msg.channel, "signal");
        assert!(
            msg.id.starts_with("sig_1700000000000_"),
            "id should embed timestamp but stay opaque: {}",
            msg.id
        );
        // Privacy regression: the in-group sender must not appear in the
        // generic message id, even though the group id itself is in
        // `reply_target` and not sensitive.
        assert!(
            !msg.id.contains("+1111111111"),
            "E.164 sender must not leak into group msg.id: {}",
            msg.id
        );
        assert_eq!(msg.timestamp, 1_700_000_000);
        assert_eq!(msg.channel_alias.as_deref(), Some("signal_test_alias"));
    }

    #[test]
    fn process_envelope_populates_recent_targets() {
        // The opaque `msg.id` is unusable for `sendReaction` on its own —
        // signal-cli needs `(targetAuthor, targetTimestamp)`. Confirm the
        // channel-local lookup is seeded so a later reaction can recover
        // those values without the id leaking the sender.
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            vec!["group_xyz".to_string()],
            false,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            false,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("group hello".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: Some(GroupInfo {
                    group_id: Some("group_xyz".to_string()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        let target = ch
            .recent_targets
            .lock()
            .peek(&msg.id)
            .cloned()
            .expect("recent_targets should contain the just-emitted id");
        assert_eq!(target.author, "+1111111111");
        assert_eq!(target.timestamp_ms, 1_700_000_000_000);
    }

    #[test]
    fn sse_envelope_deserializes() {
        let json = r#"{
            "envelope": {
                "source": "+1111111111",
                "sourceNumber": "+1111111111",
                "timestamp": 1700000000000,
                "dataMessage": {
                    "message": "Hello Signal!",
                    "timestamp": 1700000000000
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        assert_eq!(env.source_number.as_deref(), Some("+1111111111"));
        let dm = env.data_message.unwrap();
        assert_eq!(dm.message.as_deref(), Some("Hello Signal!"));
    }

    #[test]
    fn sse_envelope_deserializes_group() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+2222222222",
                "dataMessage": {
                    "message": "Group msg",
                    "groupInfo": {
                        "groupId": "abc123"
                    }
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        let dm = env.data_message.unwrap();
        assert_eq!(
            dm.group_info.as_ref().unwrap().group_id.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn envelope_defaults() {
        let json = r#"{}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(env.source.is_none());
        assert!(env.source_number.is_none());
        assert!(env.data_message.is_none());
        assert!(env.story_message.is_none());
        assert!(env.timestamp.is_none());
    }

    #[test]
    fn pending_approvals_map_is_initially_empty() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let map = ch.pending_approvals.try_lock().unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn approval_timeout_defaults_to_300_and_is_overridable() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        assert_eq!(ch.approval_timeout_secs, 300);
        let ch = ch.with_approval_timeout_secs(60);
        assert_eq!(ch.approval_timeout_secs, 60);
    }

    #[tokio::test]
    async fn pending_approval_oneshot_delivers_response() {
        let dm_only = false;
        let ignore_attachments = false;
        let ignore_stories = false;
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            dm_only,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            ignore_attachments,
            ignore_stories,
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        ch.pending_approvals
            .lock()
            .await
            .insert("abc123".to_string(), tx);
        // simulate listen() routing
        let sender = ch.pending_approvals.lock().await.remove("abc123").unwrap();
        sender.send(ChannelApprovalResponse::Approve).unwrap();
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    fn make_reaction_channel() -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
            false,
        )
    }

    fn seed_reaction_target(ch: &SignalChannel, id: &str, author: &str, ts_ms: u64) {
        ch.recent_targets.lock().put(
            id.to_string(),
            ReactionTarget {
                author: author.to_string(),
                timestamp_ms: ts_ms,
            },
        );
    }

    #[test]
    fn build_reaction_params_dm_includes_recipient() {
        let ch = make_reaction_channel();
        seed_reaction_target(
            &ch,
            "sig_1700000000000_abcdef",
            "+2222222222",
            1_700_000_000_000,
        );
        let params = ch
            .build_reaction_params(
                "+1111111111",
                "sig_1700000000000_abcdef",
                "\u{1F44D}",
                false,
            )
            .unwrap();
        assert_eq!(
            params["recipient"],
            serde_json::json!(["+1111111111".to_string()])
        );
        assert!(params.get("groupId").is_none());
        assert_eq!(params["emoji"], "\u{1F44D}");
        assert_eq!(params["targetAuthor"], "+2222222222");
        assert_eq!(params["targetTimestamp"], 1_700_000_000_000_u64);
        assert_eq!(params["remove"], false);
        assert_eq!(params["account"], "+1234567890");
    }

    #[test]
    fn build_reaction_params_group_includes_group_id_and_remove() {
        let ch = make_reaction_channel();
        seed_reaction_target(
            &ch,
            "sig_1700000000000_abcdef",
            "+2222222222",
            1_700_000_000_000,
        );
        let params = ch
            .build_reaction_params(
                "group:abc",
                "sig_1700000000000_abcdef",
                "\u{2764}\u{FE0F}",
                true,
            )
            .unwrap();
        assert_eq!(params["groupId"], "abc");
        assert!(params.get("recipient").is_none());
        assert_eq!(params["emoji"], "\u{2764}\u{FE0F}");
        assert_eq!(params["targetAuthor"], "+2222222222");
        assert_eq!(params["targetTimestamp"], 1_700_000_000_000_u64);
        assert_eq!(params["remove"], true);
        assert_eq!(params["account"], "+1234567890");
    }

    #[test]
    fn build_reaction_params_round_trips_uuid_sender_via_lookup() {
        // The opaque id reveals nothing about the sender, so the
        // round-trip property — that `sendReaction` ultimately sends the
        // correct `targetAuthor` — has to come from `process_envelope`
        // seeding the lookup, not from id parsing.
        let ch = make_reaction_channel();
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("hi".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        let params = ch
            .build_reaction_params(&msg.reply_target, &msg.id, "\u{1F44D}", false)
            .unwrap();
        assert_eq!(params["targetAuthor"], uuid);
        assert_eq!(params["targetTimestamp"], 1_700_000_000_000_u64);
    }

    #[test]
    fn build_reaction_params_rejects_unknown_id() {
        let ch = make_reaction_channel();
        let err = ch
            .build_reaction_params("+1111111111", "sig_unknown_id", "\u{1F44D}", false)
            .unwrap_err();
        assert!(
            err.to_string().contains("no recent inbound Signal message"),
            "unexpected error: {err}"
        );
    }
}
