use async_trait::async_trait;
use futures_util::StreamExt;
use lru::LruCache;
use parking_lot::Mutex as SyncMutex;
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

const GROUP_TARGET_PREFIX: &str = "group:";

const RECENT_TARGETS_CAPACITY: usize = 1024;

/// Cap on `recent_self_sends`: outbound Note-to-Self bodies we're still
/// watching for so the echo they generate on the SSE stream doesn't get
/// re-ingested as a fresh inbound message.
const RECENT_SELF_SENDS_CAPACITY: usize = 32;

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
    /// `(token, body)` of outbound text messages just sent to
    /// `self.account` (our own Note-to-Self number), so the
    /// `syncMessage.sentMessage` echo signal-cli plays back for our own
    /// sends isn't re-ingested as a new inbound message. Echo matching is
    /// by body (oldest entry first, consumed once); the token lets a
    /// failed `send` roll back exactly its own entry without stealing an
    /// identical-content in-flight send's. Covers plain text sends only:
    /// polls (`sendPollCreate`) echo without a message body and are
    /// already dropped by the empty-content gate, and speculatively
    /// recording a poll question could leave a never-consumed entry that
    /// later swallows a genuine identical note. Bounded ring buffer.
    recent_self_sends: Arc<SyncMutex<VecDeque<(u64, String)>>>,
    /// Monotonic token source for `recent_self_sends` entries.
    self_send_seq: Arc<AtomicU64>,
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
    #[serde(rename = "syncMessage", default)]
    sync_message: Option<SyncMessage>,
}

#[derive(Debug, Deserialize)]
struct SyncMessage {
    #[serde(rename = "sentMessage", default)]
    sent_message: Option<SentMessage>,
}

#[derive(Debug, Deserialize)]
struct SentMessage {
    #[serde(default)]
    destination: Option<String>,
    #[serde(rename = "destinationNumber", default)]
    destination_number: Option<String>,
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
struct DataMessage {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(rename = "groupInfo", default)]
    group_info: Option<GroupInfo>,
    #[serde(default)]
    attachments: Option<Vec<serde_json::Value>>,
    /// Poll-vote payload. Some signal-cli builds surface poll responses
    /// as `pollAnswer` on the inbound dataMessage; without this field
    /// the deserializer silently dropped the data and consumers never
    /// learned the user voted.
    #[serde(rename = "pollAnswer", default)]
    poll_answer: Option<PollAnswer>,
    /// Native signal-cli daemon 0.14.x emits poll responses as `pollVote`.
    #[serde(rename = "pollVote", default)]
    poll_vote: Option<PollAnswer>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    #[serde(rename = "groupId", default)]
    group_id: Option<String>,
}

/// Inbound poll-vote payload.
///
/// Real signal-cli `pollVote` payloads carry selected option indexes.
/// We also accept older/alternate `pollAnswer` title fields when present,
/// but callers should treat the index path as the reliable Signal shape.
#[derive(Debug, Clone, Deserialize)]
pub struct PollAnswer {
    /// Server-assigned poll id this answer is for, when the upstream
    /// payload supplies one. Real `pollVote` payloads usually omit it.
    #[serde(rename = "pollId", default)]
    pub poll_id: Option<u64>,
    /// 0-based indices of the options the user selected. Single-choice
    /// polls (the common case for agent prompts) yield a 1-element
    /// vec; multi-select would yield more.
    #[serde(rename = "selectedIndices", alias = "optionIndexes", default)]
    pub selected_indices: Vec<u32>,
    /// Display titles of the selected options, if an older/alternate
    /// payload supplies them. Real `pollVote` payloads normally omit
    /// titles; consumers should resolve `selected_indices` against the
    /// original poll's option list.
    #[serde(rename = "selectedTitles", default)]
    pub selected_titles: Vec<String>,
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
            recent_self_sends: Arc::new(SyncMutex::new(VecDeque::with_capacity(
                RECENT_SELF_SENDS_CAPACITY,
            ))),
            self_send_seq: Arc::new(AtomicU64::new(0)),
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

    /// Build the JSON-RPC params for signal-cli's native `sendPollCreate`
    /// method.
    ///
    /// Signal poll answers correlate by option index in real `pollVote`
    /// payloads. Callback ids are intentionally not represented in this wire
    /// shape; `Channel::send_choice` documents that callers needing stable
    /// callback ids must maintain that mapping above the channel layer.
    fn build_poll_params(
        &self,
        recipient: &str,
        question: &str,
        options: &[String],
        multiple_choice: bool,
    ) -> serde_json::Value {
        match Self::parse_recipient_target(recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "account": &self.account,
                "question": question,
                "option": options,
                "no-multi": !multiple_choice,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "group-id": group_id,
                "account": &self.account,
                "question": question,
                "option": options,
                "no-multi": !multiple_choice,
            }),
        }
    }

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

    /// Process a single SSE envelope, returning one or more
    /// `ChannelMessage`s. Most envelopes produce 0 or 1 messages; a
    /// multi-select poll vote produces N (one per selected option).
    ///
    /// Inbound shape may be plain text (`dataMessage.message`) OR a
    /// poll-vote (`dataMessage.pollAnswer` or `dataMessage.pollVote`). For
    /// poll-votes we emit a synthetic message per selected option whose `content` is a
    /// documented sentinel: `"[choice-index]N"` for real signal-cli
    /// `pollVote` payloads, or `"[choice]<selected-title>"` when an
    /// alternate payload supplies titles. Consumers
    /// can match this prefix to correlate the vote with their original
    /// option set, or ignore it if they don't handle poll votes.
    ///
    /// Single-select polls (`multiple_choice = false`) emit at most one
    /// message; multi-select polls emit one per selected option, each
    /// resolvable independently. Callers that conflate the two should
    /// treat any vec from this method as "the user's reply set" and
    /// dispatch each entry through their normal inbound pipeline.
    fn process_envelope(&self, envelope: &Envelope) -> Vec<ChannelMessage> {
        // Skip story messages when configured
        if self.ignore_stories && envelope.story_message.is_some() {
            return Vec::new();
        }

        let Some(data_msg) = envelope.data_message.as_ref() else {
            if let Some(sent) = envelope
                .sync_message
                .as_ref()
                .and_then(|s| s.sent_message.as_ref())
            {
                return self.process_sent_sync_message(envelope, sent);
            }
            return Vec::new();
        };

        // Skip attachment-only messages when configured
        if self.ignore_attachments {
            let has_attachments = data_msg.attachments.as_ref().is_some_and(|a| !a.is_empty());
            if has_attachments
                && data_msg.message.is_none()
                && data_msg.poll_answer.is_none()
                && data_msg.poll_vote.is_none()
            {
                return Vec::new();
            }
        }

        let Some(sender) = Self::sender(envelope) else {
            return Vec::new();
        };

        if !self.is_sender_allowed(&sender) {
            return Vec::new();
        }

        if !self.matches_group(data_msg) {
            return Vec::new();
        }

        let target = self.reply_target(data_msg, &sender);

        let timestamp = data_msg
            .timestamp
            .or(envelope.timestamp)
            .unwrap_or_else(Self::now_ms);

        // Build the list of synthetic content strings. For poll votes,
        // emit one entry per selected title (or per selected index when
        // titles are absent). For text messages, emit one entry with
        // the raw body.
        let contents: Vec<String> = if let Some(pa) = data_msg
            .poll_answer
            .as_ref()
            .or(data_msg.poll_vote.as_ref())
        {
            if !pa.selected_titles.is_empty() {
                pa.selected_titles
                    .iter()
                    .map(|t| format!("[choice]{t}"))
                    .collect()
            } else if !pa.selected_indices.is_empty() {
                pa.selected_indices
                    .iter()
                    .map(|i| format!("[choice-index]{}", i + 1))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            data_msg
                .message
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(|t| vec![t.to_string()])
                .unwrap_or_default()
        };

        self.build_messages(&sender, &target, timestamp, contents)
    }

    /// Handle a Signal "Note to Self" sync envelope
    /// (`syncMessage.sentMessage`). signal-cli surfaces messages the
    /// linked account sent to itself this way rather than as a
    /// `dataMessage`, so without this path Note-to-Self traffic is
    /// silently dropped upstream in `process_envelope`.
    ///
    /// Only sync sent-messages addressed to `self.account` are accepted;
    /// sync echoes of messages sent to other contacts and group sync
    /// messages are ignored. Sender and reply target both resolve to the
    /// envelope's source (falling back to `self.account`), mirroring the
    /// direct-DM path so replies land back in the Note-to-Self
    /// conversation.
    fn process_sent_sync_message(
        &self,
        envelope: &Envelope,
        sent: &SentMessage,
    ) -> Vec<ChannelMessage> {
        // Group sync messages are out of scope for Note-to-Self.
        if sent.group_info.is_some() {
            return Vec::new();
        }

        let destination = sent
            .destination_number
            .as_deref()
            .or(sent.destination.as_deref());
        if destination != Some(self.account.as_str()) {
            return Vec::new();
        }

        // Skip attachment-only messages when configured, mirroring the
        // dataMessage path.
        if self.ignore_attachments {
            let has_attachments = sent.attachments.as_ref().is_some_and(|a| !a.is_empty());
            if has_attachments && sent.message.is_none() {
                return Vec::new();
            }
        }

        // Our own outbound Note-to-Self replies echo back through this
        // same sync path; drop the echo instead of re-ingesting it as a
        // new inbound message.
        if self.consume_self_send_echo(sent.message.as_deref()) {
            return Vec::new();
        }

        let sender = Self::sender(envelope).unwrap_or_else(|| self.account.clone());

        if !self.is_sender_allowed(&sender) {
            return Vec::new();
        }

        let timestamp = sent
            .timestamp
            .or(envelope.timestamp)
            .unwrap_or_else(Self::now_ms);

        let Some(content) = sent.message.as_deref().filter(|t| !t.is_empty()) else {
            return Vec::new();
        };

        self.build_messages(&sender, &sender, timestamp, vec![content.to_string()])
    }

    /// Build one `ChannelMessage` per content string, seeding
    /// `recent_targets` for each so outbound reactions can round-trip to
    /// `(author, timestamp)` without embedding the sender in the opaque
    /// id. Shared by the `dataMessage` and `syncMessage.sentMessage`
    /// paths so their emission logic can't drift apart.
    fn build_messages(
        &self,
        sender: &str,
        reply_target: &str,
        timestamp: u64,
        contents: Vec<String>,
    ) -> Vec<ChannelMessage> {
        contents
            .into_iter()
            .enumerate()
            .map(|(idx, content)| {
                // Opaque id: timestamp is convenient for debugging, the random
                // suffix disambiguates senders and multi-select poll entries
                // without revealing the sender. The sender stays only in the
                // channel-local `recent_targets` map and on `ChannelMessage`.
                let id = format!("sig_{timestamp}_{}_{}", idx, Self::random_id_suffix());
                self.recent_targets.lock().put(
                    id.clone(),
                    ReactionTarget {
                        author: sender.to_string(),
                        timestamp_ms: timestamp,
                    },
                );

                ChannelMessage {
                    id,
                    sender: sender.to_string(),
                    reply_target: reply_target.to_string(),
                    content,
                    channel: "signal".to_string(),
                    channel_alias: Some(self.alias.clone()),
                    timestamp: timestamp / 1000, // millis -> secs
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: vec![],
                    subject: None,

                    ..Default::default()
                }
            })
            .collect()
    }

    fn now_ms() -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    /// Record an outbound Note-to-Self text body so its
    /// `syncMessage.sentMessage` echo on the SSE stream can be recognized
    /// and dropped once, instead of being re-ingested as a new inbound
    /// message. Returns the entry's token so a failed send can roll back
    /// exactly its own entry via `remove_self_send`. Text sends only —
    /// see the `recent_self_sends` field docs for why poll sends don't
    /// record. Bounded ring buffer; oldest entry is evicted once
    /// `RECENT_SELF_SENDS_CAPACITY` is reached.
    fn record_self_send(&self, content: String) -> u64 {
        let token = self.self_send_seq.fetch_add(1, Ordering::Relaxed);
        let mut recent = self.recent_self_sends.lock();
        if recent.len() >= RECENT_SELF_SENDS_CAPACITY {
            recent.pop_front();
        }
        recent.push_back((token, content));
        token
    }

    /// Remove the entry `record_self_send` returned `token` for, if it's
    /// still present. No-op when the echo already consumed it — the
    /// token-exact match guarantees a rollback can never remove an
    /// unrelated identical-content entry.
    fn remove_self_send(&self, token: u64) {
        let mut recent = self.recent_self_sends.lock();
        if let Some(pos) = recent.iter().position(|(t, _)| *t == token) {
            recent.remove(pos);
        }
    }

    /// If `content` matches a body recorded by `record_self_send`,
    /// consume the oldest such entry and report the match so the caller
    /// can drop the echo. Returns `false` (no-op) for `None`.
    fn consume_self_send_echo(&self, content: Option<&str>) -> bool {
        let Some(content) = content else {
            return false;
        };
        let mut recent = self.recent_self_sends.lock();
        match recent.iter().position(|(_, c)| c == content) {
            Some(pos) => {
                recent.remove(pos);
                true
            }
            None => false,
        }
    }

    fn random_id_suffix() -> String {
        use rand::RngExt;
        const CHARSET: &[u8] = b"0123456789abcdef";
        let mut rng = rand::rng();
        (0..6)
            .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
            .collect()
    }

    /// Send a multiple-choice poll to `recipient` (E.164 number, UUID,
    /// or `group:<id>`).
    ///
    /// Sent via signal-cli daemon's JSON-RPC `sendPollCreate` method. The
    /// poll renders as native UI in modern Signal clients and emits a
    /// poll-vote event (`pollAnswer` or `pollVote`, depending on signal-cli
    /// version) back through the SSE stream when the user votes — see
    /// `process_envelope` for how that flows back to consumers, normally as
    /// a synthetic `[choice-index]N` `ChannelMessage`.
    ///
    /// `multiple_choice = false` → single-select poll (the common case
    /// for "pick one of N" agent prompts). Pass `true` to allow
    /// multi-select.
    pub async fn send_poll(
        &self,
        recipient: &str,
        question: &str,
        options: &[String],
        multiple_choice: bool,
    ) -> anyhow::Result<()> {
        if options.len() < 2 {
            anyhow::bail!(
                "Signal poll requires at least 2 options (got {}); render as text instead",
                options.len()
            );
        }
        let params = self.build_poll_params(recipient, question, options, multiple_choice);
        self.rpc_request("sendPollCreate", params).await?;
        Ok(())
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
        let target = Self::parse_recipient_target(&message.recipient);
        let params = match &target {
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

        // Record Note-to-Self sends BEFORE the RPC: the SSE listen() loop
        // runs as an independent task, and signal-cli can push the
        // syncMessage.sentMessage echo before the send response resolves.
        // Recording first closes that race; rolling back the entry by
        // token on RPC failure keeps failed sends from poisoning the
        // guard (and is a no-op if the echo already consumed it).
        let self_send_token = match &target {
            RecipientTarget::Direct(number) if *number == self.account => {
                Some(self.record_self_send(message.content.clone()))
            }
            _ => None,
        };

        if let Err(err) = self.rpc_request("send", params).await {
            if let Some(token) = self_send_token {
                self.remove_self_send(token);
            }
            return Err(err);
        }

        Ok(())
    }

    async fn send_choice(
        &self,
        recipient: &str,
        prompt: &str,
        options: &[(String, String)],
    ) -> anyhow::Result<()> {
        // Signal supports native polls via signal-cli JSON-RPC
        // sendPollCreate. Single-select (`no-multi=true`) is the right default
        // for "pick one of N" prompts; consumers needing multi-select
        // should call SignalChannel::send_poll directly.
        //
        // Empty options → no-op (send only the prompt if any) so we
        // don't ship a useless "(reply with name or number)" header
        // with nothing under it. See Channel::send_choice docs.
        let trimmed_prompt = prompt.trim();
        if options.is_empty() {
            if trimmed_prompt.is_empty() {
                return Ok(());
            }
            return self
                .send(&SendMessage::new(trimmed_prompt, recipient))
                .await;
        }

        // Polls require ≥2 options per Signal protocol; for exactly
        // 1 option, fall back to text — a 1-option poll is a UX
        // anti-pattern. The callback ids passed in here are dropped
        // on the wire because real Signal poll votes correlate by
        // option index. Per the trait's docs, callers needing stable
        // callback ids should maintain a side map keyed by poll option
        // index.
        if options.len() >= 2 {
            let labels: Vec<String> = options.iter().map(|(_, l)| l.clone()).collect();
            return self.send_poll(recipient, prompt, &labels, false).await;
        }
        // Single-option text fallback.
        let mut text = String::new();
        if !trimmed_prompt.is_empty() {
            text.push_str(trimmed_prompt);
            text.push_str("\n\n");
        }
        text.push_str("(reply with name or number)\n");
        for (idx, (_id, label)) in options.iter().enumerate() {
            text.push_str(&format!("{}. {}\n", idx + 1, label.trim()));
        }
        let trimmed = text.trim_end().to_string();
        self.send(&SendMessage::new(trimmed, recipient)).await
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
                                    if let Some(ref envelope) = sse.envelope {
                                        let mut consumed_as_approval = false;
                                        let messages = self.process_envelope(envelope);
                                        for msg in messages {
                                            if let Some((token, response)) =
                                                crate::util::parse_approval_reply(&msg.content)
                                            {
                                                let mut map = self.pending_approvals.lock().await;
                                                if let Some(sender) = map.remove(&token) {
                                                    let _ = sender.send(response);
                                                    consumed_as_approval = true;
                                                    continue;
                                                }
                                            }
                                            if tx.send(msg).await.is_err() {
                                                return Ok(());
                                            }
                                        }
                                        if consumed_as_approval {
                                            current_data.clear();
                                            continue;
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
                        if let Some(ref envelope) = sse.envelope {
                            for msg in self.process_envelope(envelope) {
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    fn make_channel() -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(|| vec!["+1111111111".into()]),
            false,
            false,
        )
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
            poll_answer: None,
            poll_vote: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
            poll_answer: None,
            poll_vote: None,
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
            poll_answer: None,
            poll_vote: None,
        };
        assert!(ch.matches_group(&matching));

        let non_matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("other_group".to_string()),
            }),
            attachments: None,
            poll_answer: None,
            poll_vote: None,
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
            poll_answer: None,
            poll_vote: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
            poll_answer: None,
            poll_vote: None,
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
            poll_answer: None,
            poll_vote: None,
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
            poll_answer: None,
            poll_vote: None,
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
            sync_message: None,
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
            sync_message: None,
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
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
            sync_message: None,
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
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
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
        assert!(ch.process_envelope(&env).is_empty());
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
        assert!(ch.process_envelope(&env).is_empty());
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
        assert!(ch.process_envelope(&env).is_empty());
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
        assert!(ch.process_envelope(&env).is_empty());
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        assert!(ch.process_envelope(&env).is_empty());
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
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
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
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

    #[test]
    fn build_poll_params_dm_uses_send_poll_create_shape() {
        let ch = make_reaction_channel();
        let options = vec!["Alpha".to_string(), "Beta".to_string()];
        let params = ch.build_poll_params("+1111111111", "Pick one", &options, false);

        assert_eq!(
            params["recipient"],
            serde_json::json!(["+1111111111".to_string()])
        );
        assert!(params.get("group-id").is_none());
        assert_eq!(params["account"], "+1234567890");
        assert_eq!(params["question"], "Pick one");
        assert_eq!(params["option"], serde_json::json!(["Alpha", "Beta"]));
        assert_eq!(params["no-multi"], true);
        assert!(params.get("options").is_none());
        assert!(params.get("multi").is_none());
    }

    #[test]
    fn build_poll_params_group_preserves_multi_select() {
        let ch = make_reaction_channel();
        let options = vec!["Alpha".to_string(), "Beta".to_string()];
        let params = ch.build_poll_params("group:abc", "Pick any", &options, true);

        assert_eq!(params["group-id"], "abc");
        assert!(params.get("recipient").is_none());
        assert_eq!(params["account"], "+1234567890");
        assert_eq!(params["question"], "Pick any");
        assert_eq!(params["option"], serde_json::json!(["Alpha", "Beta"]));
        assert_eq!(params["no-multi"], false);
        assert!(params.get("groupId").is_none());
        assert!(params.get("options").is_none());
        assert!(params.get("multi").is_none());
    }

    fn poll_envelope(
        sender: Option<&str>,
        selected_titles: Vec<&str>,
        selected_indices: Vec<u32>,
    ) -> Envelope {
        Envelope {
            source: sender.map(String::from),
            source_number: sender.map(String::from),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
                poll_answer: Some(PollAnswer {
                    poll_id: Some(1),
                    selected_indices,
                    selected_titles: selected_titles.iter().map(|s| s.to_string()).collect(),
                }),
                poll_vote: None,
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    fn poll_vote_envelope(sender: Option<&str>, option_indexes: Vec<u32>) -> Envelope {
        Envelope {
            source: sender.map(String::from),
            source_number: sender.map(String::from),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
                poll_answer: None,
                poll_vote: Some(PollAnswer {
                    poll_id: Some(1),
                    selected_indices: option_indexes,
                    selected_titles: Vec::new(),
                }),
            }),
            story_message: None,
            sync_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn process_envelope_poll_answer_emits_choice_sentinel() {
        let ch = make_channel();
        let env = poll_envelope(Some("+1111111111"), vec!["Librarian"], vec![0]);
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[choice]Librarian");
        assert_eq!(msgs[0].sender, "+1111111111");
        assert_eq!(msgs[0].channel, "signal");
    }

    #[test]
    fn process_envelope_poll_answer_falls_back_to_index() {
        let ch = make_channel();
        // No titles provided; only index 2 (0-based) → emits "[choice-index]3".
        let env = poll_envelope(Some("+1111111111"), vec![], vec![2]);
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[choice-index]3");
    }

    #[test]
    fn process_envelope_poll_vote_falls_back_to_index() {
        let ch = make_channel();
        // signal-cli daemon 0.14.x emits native poll votes as
        // dataMessage.pollVote.optionIndexes.
        let env = poll_vote_envelope(Some("+1111111111"), vec![0]);
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[choice-index]1");
    }

    #[test]
    fn poll_vote_option_indexes_deserializes_from_signal_cli_shape() {
        let env: Envelope = serde_json::from_value(serde_json::json!({
            "source": "+1111111111",
            "sourceNumber": "+1111111111",
            "timestamp": 1_700_000_000_000_u64,
            "dataMessage": {
                "timestamp": 1_700_000_000_000_u64,
                "pollVote": {
                    "targetSentTimestamp": 1_700_000_000_000_u64,
                    "optionIndexes": [0],
                    "voteCount": 1
                }
            }
        }))
        .unwrap();

        let vote = env
            .data_message
            .as_ref()
            .and_then(|dm| dm.poll_vote.as_ref())
            .unwrap();
        assert_eq!(vote.selected_indices, vec![0]);
        assert!(vote.selected_titles.is_empty());
    }

    #[test]
    fn process_envelope_poll_answer_multi_select_emits_one_per_title() {
        let ch = make_channel();
        let env = poll_envelope(
            Some("+1111111111"),
            vec!["Librarian", "Critic", "Custodian"],
            vec![0, 1, 2],
        );
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 3, "multi-select must emit one msg per title");
        assert_eq!(msgs[0].content, "[choice]Librarian");
        assert_eq!(msgs[1].content, "[choice]Critic");
        assert_eq!(msgs[2].content, "[choice]Custodian");
        // Ids must differ so downstream dedupe doesn't drop selections.
        assert_ne!(msgs[0].id, msgs[1].id);
        assert_ne!(msgs[1].id, msgs[2].id);
    }

    #[test]
    fn process_envelope_poll_answer_denied_sender_drops() {
        let ch = make_channel();
        let env = poll_envelope(Some("+9999999999"), vec!["Librarian"], vec![0]);
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_empty_poll_answer_emits_nothing() {
        let ch = make_channel();
        // PollAnswer present but both vecs empty (signal-cli weirdness).
        let env = poll_envelope(Some("+1111111111"), vec![], vec![]);
        assert!(ch.process_envelope(&env).is_empty());
    }

    // ── Note-to-Self (syncMessage.sentMessage) ─────────────────────

    /// Note-to-Self channels need the account itself in the allowlist,
    /// since the "sender" of a Note-to-Self message is the account.
    fn make_self_allowed_channel(ignore_attachments: bool) -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            ignore_attachments,
            false,
        )
    }

    fn make_sent_sync_envelope(
        source: Option<&str>,
        destination: Option<&str>,
        message: Option<&str>,
        group_id: Option<&str>,
        attachments: Option<Vec<serde_json::Value>>,
    ) -> Envelope {
        Envelope {
            source: source.map(String::from),
            source_number: source.map(String::from),
            data_message: None,
            story_message: None,
            timestamp: Some(1_700_000_000_000),
            sync_message: Some(SyncMessage {
                sent_message: Some(SentMessage {
                    destination: destination.map(String::from),
                    destination_number: destination.map(String::from),
                    message: message.map(String::from),
                    timestamp: Some(1_700_000_000_000),
                    group_info: group_id.map(|gid| GroupInfo {
                        group_id: Some(gid.to_string()),
                    }),
                    attachments,
                }),
            }),
        }
    }

    /// Wrap a hand-built `SentMessage` in a sync envelope, for tests
    /// needing destination/timestamp shapes `make_sent_sync_envelope`
    /// doesn't cover.
    fn wrap_sent_sync(
        source: Option<&str>,
        envelope_timestamp: Option<u64>,
        sent: SentMessage,
    ) -> Envelope {
        Envelope {
            source: source.map(String::from),
            source_number: source.map(String::from),
            data_message: None,
            story_message: None,
            timestamp: envelope_timestamp,
            sync_message: Some(SyncMessage {
                sent_message: Some(sent),
            }),
        }
    }

    #[test]
    fn process_envelope_note_to_self_happy_path() {
        let ch = make_self_allowed_channel(false);
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1234567890"),
            Some("note to self text"),
            None,
            None,
        );
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
        assert_eq!(msg.sender, "+1234567890");
        assert_eq!(msg.reply_target, "+1234567890");
        assert_eq!(msg.content, "note to self text");
        assert_eq!(msg.channel, "signal");
        assert_eq!(msg.timestamp, 1_700_000_000);
        assert!(
            msg.id.starts_with("sig_1700000000000_"),
            "id should embed timestamp: {}",
            msg.id
        );
        let target = ch
            .recent_targets
            .lock()
            .peek(&msg.id)
            .cloned()
            .expect("recent_targets should contain the just-emitted id");
        assert_eq!(target.author, "+1234567890");
        assert_eq!(target.timestamp_ms, 1_700_000_000_000);

        // Verify reply routing: the Note-to-Self reply target must route
        // as a Direct send back to the account's own number.
        assert_eq!(
            SignalChannel::parse_recipient_target(&msg.reply_target),
            RecipientTarget::Direct("+1234567890".to_string())
        );
    }

    #[test]
    fn process_envelope_sent_sync_to_other_contact_ignored() {
        let ch = make_self_allowed_channel(false);
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1999999999"),
            Some("hey there"),
            None,
            None,
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_sent_sync_destination_number_takes_precedence() {
        let ch = make_self_allowed_channel(false);
        // destinationNumber matches the account: accepted even though
        // `destination` names someone else.
        let env = wrap_sent_sync(
            Some("+1234567890"),
            Some(1_700_000_000_000),
            SentMessage {
                destination: Some("+1999999999".to_string()),
                destination_number: Some("+1234567890".to_string()),
                message: Some("number wins".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            },
        );
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "number wins");

        // destinationNumber names someone else: skipped even though
        // `destination` matches the account.
        let env = wrap_sent_sync(
            Some("+1234567890"),
            Some(1_700_000_000_000),
            SentMessage {
                destination: Some("+1234567890".to_string()),
                destination_number: Some("+1999999999".to_string()),
                message: Some("number wins".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            },
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_sent_sync_no_destination_ignored() {
        let ch = make_self_allowed_channel(false);
        let env = wrap_sent_sync(
            Some("+1234567890"),
            Some(1_700_000_000_000),
            SentMessage {
                destination: None,
                destination_number: None,
                message: Some("nowhere to go".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            },
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_sent_sync_timestamp_falls_back_to_envelope() {
        let ch = make_self_allowed_channel(false);
        let env = wrap_sent_sync(
            Some("+1234567890"),
            Some(1_720_000_000_000),
            SentMessage {
                destination: Some("+1234567890".to_string()),
                destination_number: Some("+1234567890".to_string()),
                message: Some("envelope time".to_string()),
                timestamp: None,
                group_info: None,
                attachments: None,
            },
        );
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].timestamp, 1_720_000_000);
    }

    #[test]
    fn process_envelope_sent_sync_timestamp_falls_back_to_now() {
        let ch = make_self_allowed_channel(false);
        let env = wrap_sent_sync(
            Some("+1234567890"),
            None,
            SentMessage {
                destination: Some("+1234567890".to_string()),
                destination_number: Some("+1234567890".to_string()),
                message: Some("no timestamps at all".to_string()),
                timestamp: None,
                group_info: None,
                attachments: None,
            },
        );
        let now_secs_before = SignalChannel::now_ms() / 1000;
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].timestamp >= now_secs_before,
            "timestamp {} should be at least {}",
            msgs[0].timestamp,
            now_secs_before
        );
    }

    #[test]
    fn process_envelope_sent_sync_empty_string_message_ignored() {
        let ch = make_self_allowed_channel(false);
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1234567890"),
            Some(""),
            None,
            None,
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_note_to_self_echo_guard_consumes_once() {
        let ch = make_self_allowed_channel(false);
        ch.record_self_send("already sent this".to_string());
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1234567890"),
            Some("already sent this"),
            None,
            None,
        );
        // First sighting: matches the recorded outbound send, consumed as
        // an echo rather than ingested.
        assert!(ch.process_envelope(&env).is_empty());
        // Second sighting of the identical sync envelope: the guard entry
        // was already consumed, so this is treated as genuine inbound.
        let msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "already sent this");
    }

    #[tokio::test]
    async fn failed_self_send_rolls_back_echo_guard() {
        // Nothing listens on port 9 (discard), so the RPC fails; the
        // pre-recorded guard entry must be removed again so a send that
        // never happened can't swallow a later genuine Note-to-Self.
        let ch = SignalChannel::new(
            "http://127.0.0.1:9".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            false,
            false,
        );
        let result = ch
            .send(&SendMessage::new("doomed note", "+1234567890"))
            .await;
        assert!(result.is_err());
        assert!(ch.recent_self_sends.lock().is_empty());
    }

    #[test]
    fn consume_self_send_echo_removes_oldest_matching_entry() {
        let ch = make_self_allowed_channel(false);
        let first = ch.record_self_send("dup".to_string());
        let second = ch.record_self_send("dup".to_string());
        let _ = ch.record_self_send("other".to_string());
        assert!(ch.consume_self_send_echo(Some("dup")));
        // Only the oldest of the two duplicates is consumed per match.
        {
            let recent = ch.recent_self_sends.lock();
            assert_eq!(recent.len(), 2);
            assert!(!recent.iter().any(|(t, _)| *t == first));
            assert!(recent.iter().any(|(t, _)| *t == second));
        }
        assert!(ch.consume_self_send_echo(Some("dup")));
        assert!(!ch.consume_self_send_echo(Some("dup")));
        assert!(!ch.consume_self_send_echo(None));
        assert!(ch.consume_self_send_echo(Some("other")));
        assert!(ch.recent_self_sends.lock().is_empty());
    }

    #[test]
    fn remove_self_send_is_token_exact() {
        // A rollback whose entry was already consumed by the real echo
        // must NOT steal an unrelated identical-content entry.
        let ch = make_self_allowed_channel(false);
        let first = ch.record_self_send("same text".to_string());
        let second = ch.record_self_send("same text".to_string());
        // The echo consumes the oldest entry (the first send's).
        assert!(ch.consume_self_send_echo(Some("same text")));
        // Rolling back the first send is now a no-op; the second send's
        // entry survives for its own echo.
        ch.remove_self_send(first);
        assert_eq!(ch.recent_self_sends.lock().len(), 1);
        assert!(ch.consume_self_send_echo(Some("same text")));
        // Rolling back the second send after consumption is also a no-op.
        ch.remove_self_send(second);
        assert!(ch.recent_self_sends.lock().is_empty());
    }

    #[test]
    fn record_self_send_evicts_oldest_at_capacity() {
        let ch = make_self_allowed_channel(false);
        for i in 0..RECENT_SELF_SENDS_CAPACITY + 1 {
            ch.record_self_send(format!("note {i}"));
        }
        assert_eq!(
            ch.recent_self_sends.lock().len(),
            RECENT_SELF_SENDS_CAPACITY
        );
        // The very first entry was evicted; the newest survives.
        assert!(!ch.consume_self_send_echo(Some("note 0")));
        assert!(ch.consume_self_send_echo(Some(&format!("note {RECENT_SELF_SENDS_CAPACITY}"))));
    }

    #[test]
    fn sent_sync_envelope_serde_fixture() {
        let json = r#"{
            "envelope": {
                "source": "+1234567890",
                "sourceNumber": "+1234567890",
                "sourceUuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
                "timestamp": 1720000000000,
                "syncMessage": {
                    "sentMessage": {
                        "destination": "+1234567890",
                        "destinationNumber": "+1234567890",
                        "destinationUuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
                        "timestamp": 1720000000000,
                        "message": "note text",
                        "expiresInSeconds": 0,
                        "viewOnce": false
                    }
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        let sent = env
            .sync_message
            .as_ref()
            .and_then(|s| s.sent_message.as_ref())
            .expect("sentMessage should deserialize");
        assert_eq!(sent.destination_number.as_deref(), Some("+1234567890"));
        assert_eq!(sent.message.as_deref(), Some("note text"));
        assert_eq!(sent.timestamp, Some(1_720_000_000_000));
    }

    #[test]
    fn process_envelope_group_sent_sync_ignored() {
        let ch = make_self_allowed_channel(false);
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1234567890"),
            Some("group note"),
            Some("group_xyz"),
            None,
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_sent_sync_empty_message_ignored() {
        let ch = make_self_allowed_channel(false);
        let env =
            make_sent_sync_envelope(Some("+1234567890"), Some("+1234567890"), None, None, None);
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_sent_sync_denied_self_sender() {
        // Default make_channel()'s allowlist doesn't include the
        // account itself, so a Note-to-Self message is dropped exactly
        // like an unknown-sender DM would be.
        let ch = make_channel();
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1234567890"),
            Some("hi self"),
            None,
            None,
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_data_message_precedence_over_sync() {
        let ch = make_self_allowed_channel(false);
        let mut env = make_envelope(Some("+1234567890"), Some("dataMessage wins"));
        env.sync_message = Some(SyncMessage {
            sent_message: Some(SentMessage {
                destination: Some("+1234567890".to_string()),
                destination_number: Some("+1234567890".to_string()),
                message: Some("syncMessage should be ignored".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
        });
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
        assert_eq!(msg.content, "dataMessage wins");
        assert_eq!(msg.sender, "+1234567890");
        assert_eq!(msg.reply_target, "+1234567890");
    }

    #[test]
    fn process_envelope_sent_sync_attachment_only_skipped() {
        let ch = make_self_allowed_channel(true);
        let env = make_sent_sync_envelope(
            Some("+1234567890"),
            Some("+1234567890"),
            None,
            None,
            Some(vec![serde_json::json!({"contentType": "image/png"})]),
        );
        assert!(ch.process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_note_to_self_sender_falls_back_to_account() {
        // If the envelope carries no `source`/`sourceNumber` at all,
        // sender and reply_target should both fall back to the
        // configured account rather than dropping the message.
        let ch = make_self_allowed_channel(false);
        let env = make_sent_sync_envelope(
            None,
            Some("+1234567890"),
            Some("no source here"),
            None,
            None,
        );
        let mut msgs = ch.process_envelope(&env);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.remove(0);
        assert_eq!(msg.sender, "+1234567890");
        assert_eq!(msg.reply_target, "+1234567890");
    }
}
