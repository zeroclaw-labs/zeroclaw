use anyhow::Context as _;
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use lru::LruCache;
use parking_lot::Mutex as SyncMutex;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::{MarkerKind, MediaAttachment, RenderedMarker};

const GROUP_TARGET_PREFIX: &str = "group:";

const RECENT_TARGETS_CAPACITY: usize = 1024;

/// Signal's own per-attachment limit (`global.attachments.maxBytes`, 100 MiB).
/// Signal measures it after padding and encryption, so signal-cli can still
/// reject a file slightly under this on send.
const SIGNAL_MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

/// Combined file bytes of all attachments one message may hold in memory,
/// inbound or outbound. Fits one maximum-size file. signal-cli exchanges
/// attachments as base64 inside JSON, so the transfer itself is about 4/3 of
/// this; response and file reads are capped against that encoded size.
const SIGNAL_MESSAGE_ATTACHMENT_BUDGET: u64 = SIGNAL_MAX_ATTACHMENT_BYTES;

/// Room for the JSON-RPC envelope around a `getAttachment` payload.
const SIGNAL_RPC_ENVELOPE_BYTES: u64 = 64 * 1024;

/// Workspace subdirectory that inbound attachments are saved into.
const SIGNAL_ATTACHMENT_SAVE_SUBDIR: &str = "signal_files";

/// Outbound marker kinds sent as signal-cli attachments. `LOCATION` is left
/// out on purpose: Signal has no location message, so it stays as text.
const SIGNAL_OUTBOUND_MARKER_KINDS: &[&str] = &[
    "IMAGE", "PHOTO", "DOCUMENT", "FILE", "VIDEO", "AUDIO", "VOICE",
];

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
    pending_approvals: Arc<Mutex<HashMap<String, crate::util::PendingApproval>>>,
    /// Seconds to wait for an operator reply to a `request_approval` prompt
    /// before treating the silence as a deny. Default 300.
    approval_timeout_secs: u64,
    /// Opaque inbound message id → `(targetAuthor, targetTimestamp)` so
    /// outbound reactions can be addressed without embedding the Signal
    /// sender (E.164 phone number or UUID) in `ChannelMessage.id`. Bounded
    /// LRU; once a message ages out, reactions against it fail cleanly.
    recent_targets: Arc<SyncMutex<LruCache<String, ReactionTarget>>>,
    /// Workspace that inbound attachments are saved into and that outbound
    /// media markers must resolve inside. `None` disables both: inbound
    /// attachments arrive as bytes only and outbound markers are refused.
    workspace_dir: Option<PathBuf>,
    /// Per-message attachment budget in file bytes. Always
    /// [`SIGNAL_MESSAGE_ATTACHMENT_BUDGET`] outside tests.
    attachment_budget: u64,
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
    #[serde(rename = "sourceUuid", default)]
    source_uuid: Option<String>,
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

/// One entry of `dataMessage.attachments`, matching signal-cli's
/// `JsonAttachment`. `id` is the handle `getAttachment` takes.
#[derive(Debug, Deserialize)]
struct SignalAttachment {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "contentType", default)]
    content_type: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    size: Option<u64>,
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
            workspace_dir: None,
            attachment_budget: SIGNAL_MESSAGE_ATTACHMENT_BUDGET,
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

    /// Set the workspace used for inbound attachment storage and as the
    /// boundary for outbound media markers.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    #[cfg(test)]
    fn with_attachment_budget(mut self, bytes: u64) -> Self {
        self.attachment_budget = bytes;
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

    /// Effective sender: prefer `sourceNumber` (E.164), then `source`, then `sourceUuid`.
    fn sender(envelope: &Envelope) -> Option<String> {
        envelope
            .source_number
            .as_deref()
            .filter(|sender| !sender.is_empty())
            .or(envelope
                .source
                .as_deref()
                .filter(|sender| !sender.is_empty()))
            .or(envelope
                .source_uuid
                .as_deref()
                .filter(|sender| !sender.is_empty()))
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

    fn canonical_destination(recipient: &str) -> String {
        match Self::parse_recipient_target(recipient) {
            RecipientTarget::Direct(value) => value,
            RecipientTarget::Group(value) => format!("{GROUP_TARGET_PREFIX}{value}"),
        }
    }

    async fn resolve_approval_reply(
        &self,
        message: &ChannelMessage,
    ) -> Option<crate::util::PendingApprovalResolution> {
        let (token, response) = crate::util::parse_approval_reply(&message.content)?;
        let destination = Self::canonical_destination(&message.reply_target);
        Some(
            crate::util::resolve_pending_approval(
                &self.pending_approvals,
                &token,
                response,
                self.is_sender_allowed(&message.sender),
                &destination,
            )
            .await,
        )
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
        self.rpc_request_limited(method, params, None).await
    }

    /// [`Self::rpc_request`], failing once the response body passes
    /// `max_response_bytes` instead of buffering all of it first.
    async fn rpc_request_limited(
        &self,
        method: &str,
        params: serde_json::Value,
        max_response_bytes: Option<u64>,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let url = format!("{}/api/v1/rpc", self.http_url);
        let id = Uuid::new_v4().to_string();

        let mut body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id,
        });
        // Moved in: `json!` would deep-copy params, including attachment data.
        body["params"] = params;

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

        let mut parsed: serde_json::Value = match max_response_bytes {
            Some(limit) => {
                let bytes = read_body_limited(resp, limit).await?;
                if bytes.is_empty() {
                    return Ok(None);
                }
                serde_json::from_slice(&bytes)?
            }
            None => {
                let bytes = resp.bytes().await?;
                if bytes.is_empty() {
                    return Ok(None);
                }
                serde_json::from_slice(&bytes)?
            }
        };
        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            let msg = redact_data_uris(msg);
            anyhow::bail!("Signal RPC error {code}: {msg}");
        }

        Ok(parsed.get_mut("result").map(serde_json::Value::take))
    }

    /// Attachment entries to fetch for `data_msg`: none for poll votes or when
    /// `ignore_attachments` is set.
    fn inbound_attachment_entries<'a>(&self, data_msg: &'a DataMessage) -> &'a [serde_json::Value] {
        let is_poll = data_msg.poll_answer.is_some() || data_msg.poll_vote.is_some();
        if self.ignore_attachments || is_poll {
            return &[];
        }
        data_msg.attachments.as_deref().unwrap_or_default()
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
            return Vec::new();
        };

        let Some(sender) = Self::sender(envelope) else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "dropping Signal envelope without a resolvable sender identity"
            );
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
            .unwrap_or_else(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });

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
            match data_msg.message.as_deref().filter(|t| !t.is_empty()) {
                Some(text) => vec![text.to_string()],
                // Attachment-only message: emit one empty-bodied entry that
                // `attach_inbound_media` fills in after this sender check.
                // With `ignore_attachments` there are no entries, so it drops.
                None if !self.inbound_attachment_entries(data_msg).is_empty() => {
                    vec![String::new()]
                }
                None => Vec::new(),
            }
        };
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
                        author: sender.clone(),
                        timestamp_ms: timestamp,
                    },
                );

                ChannelMessage {
                    id,
                    sender: sender.clone(),
                    reply_target: target.clone(),
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
    fn random_id_suffix() -> String {
        use rand::RngExt;
        const CHARSET: &[u8] = b"0123456789abcdef";
        let mut rng = rand::rng();
        (0..6)
            .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
            .collect()
    }

    /// Download, save, and mark up the attachments on an inbound message that
    /// already passed the sender and group checks in
    /// [`Self::process_envelope`]. An attachment that fails is logged and
    /// skipped; the message text is never lost. Attachments are held in memory
    /// until dispatch, so once the message's attachment budget is spent the
    /// rest are skipped without downloading. Returns `None` only when an
    /// attachment-only message ends up with nothing to deliver.
    async fn attach_inbound_media(
        &self,
        mut msg: ChannelMessage,
        envelope: &Envelope,
    ) -> Option<ChannelMessage> {
        let Some(data_msg) = envelope.data_message.as_ref() else {
            return Some(msg);
        };
        let group_id = data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref());

        let mut remaining = self.attachment_budget;
        for entry in self.inbound_attachment_entries(data_msg) {
            let fetched = if remaining == 0 {
                Err(anyhow::Error::msg("message attachment budget exhausted"))
            } else {
                self.fetch_inbound_attachment(entry, &msg.sender, group_id, remaining)
                    .await
            };
            match fetched {
                Ok(media) => {
                    remaining = remaining.saturating_sub(media.data.len() as u64);
                    if let Some(marker) = &media.marker {
                        if !msg.content.is_empty() {
                            msg.content.push('\n');
                        }
                        let label = marker_label(marker.kind);
                        msg.content
                            .push_str(&format!("[{label}:{}]", marker.target));
                    }
                    msg.attachments.push(media);
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error": zeroclaw_runtime::security::scrub(&format!("{e:#}")),
                            })),
                        "signal: skipping inbound attachment"
                    );
                }
            }
        }

        if msg.content.is_empty() && msg.attachments.is_empty() {
            return None;
        }
        Some(msg)
    }

    /// Fetch one inbound attachment through signal-cli's `getAttachment`,
    /// classify it, and save it into the workspace when one is configured.
    /// `budget` is what remains of the message's attachment budget; the
    /// response is capped at that size in base64 before it is buffered.
    async fn fetch_inbound_attachment(
        &self,
        entry: &serde_json::Value,
        sender: &str,
        group_id: Option<&str>,
        budget: u64,
    ) -> anyhow::Result<MediaAttachment> {
        let att =
            SignalAttachment::deserialize(entry).context("unrecognized attachment metadata")?;
        let id = att
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::Error::msg("attachment has no id"))?;
        let limit = budget.min(SIGNAL_MAX_ATTACHMENT_BYTES);
        if att.size.is_some_and(|size| size > limit) {
            anyhow::bail!("attachment exceeds the {limit}-byte limit");
        }

        let mut params = serde_json::json!({ "account": &self.account, "id": id });
        match group_id {
            Some(group_id) => params["groupId"] = serde_json::json!(group_id),
            None => params["recipient"] = serde_json::json!(sender),
        }
        // The declared size is the sender's claim; the response cap holds
        // even when it is missing or wrong.
        let max_response = base64_len(limit) + SIGNAL_RPC_ENVELOPE_BYTES;
        let result = self
            .rpc_request_limited("getAttachment", params, Some(max_response))
            .await?
            .ok_or_else(|| anyhow::Error::msg("getAttachment returned no result"))?;
        let encoded = result
            .get("data")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::Error::msg("getAttachment result has no data"))?;
        let data = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        if data.len() as u64 > limit {
            anyhow::bail!("attachment exceeds the {limit}-byte limit");
        }

        // Keep only the final path component of the sender-supplied name.
        let file_name = Path::new(att.filename.as_deref().unwrap_or_default())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("attachment")
            .to_string();
        let content_type = att.content_type.as_deref().filter(|ct| !ct.is_empty());
        let kind = inbound_marker_kind(content_type.unwrap_or_default(), &file_name, &data);
        // `file_name` stays as sent for display; the saved copy gets a name
        // its own marker can carry.
        let stored_name = marker_safe_file_name(&file_name);

        let marker = match self.workspace_dir.as_deref() {
            Some(workspace) => save_inbound_attachment(workspace, &stored_name, &data)
                .await
                .inspect_err(|e| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "error": zeroclaw_runtime::security::scrub(&format!("{e:#}")),
                            })),
                        "signal: inbound attachment save failed; passing bytes only"
                    );
                })
                .ok()
                .map(|path| RenderedMarker {
                    target: path.display().to_string(),
                    kind,
                }),
            None => None,
        };

        Ok(MediaAttachment {
            file_name,
            data,
            mime_type: content_type.map(str::to_owned),
            marker,
        })
    }

    /// Split outbound media markers out of `content`. Resolved markers become
    /// signal-cli attachments; unresolved ones are dropped, logged, and
    /// summarized in a count-only note. Content without markers is returned
    /// unchanged.
    async fn prepare_outbound(&self, content: &str) -> (String, Vec<String>) {
        let (cleaned, markers) =
            crate::util::parse_attachment_markers_of_kinds(content, SIGNAL_OUTBOUND_MARKER_KINDS);
        if markers.is_empty() {
            return (content.to_string(), Vec::new());
        }

        let mut text = cleaned;
        let mut attachments = Vec::new();
        let mut failed = 0usize;
        // Every encoded file is held until `send` serializes the request, so
        // the files together must fit the message's attachment budget.
        let mut remaining = self.attachment_budget;
        for (kind, target) in &markers {
            match self.resolve_outbound_marker(target, remaining).await {
                Ok((attachment, file_len)) => {
                    remaining = remaining.saturating_sub(file_len);
                    attachments.push(attachment);
                }
                Err(err) => {
                    failed += 1;
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "kind": kind,
                                "outcome": err.outcome(),
                                "reason": err.reason(),
                            })),
                        "signal: dropping unresolved outbound attachment marker"
                    );
                }
            }
        }

        if let Some(note) = signal_delivery_failure_note(failed) {
            if text.is_empty() {
                text = note;
            } else {
                text.push_str("\n\n");
                text.push_str(&note);
            }
        }
        (text, attachments)
    }

    /// Resolve an outbound marker target into a signal-cli attachment. Only
    /// regular files inside the workspace are accepted, and they are sent as
    /// RFC 2397 data URIs so delivery works even when signal-cli does not
    /// share ZeroClaw's filesystem. A file larger than `budget` (what remains
    /// of the message's attachment budget) is never read. Returns the URI and
    /// the file's size.
    async fn resolve_outbound_marker(
        &self,
        target: &str,
        budget: u64,
    ) -> Result<(String, u64), SignalMarkerError> {
        use tokio::io::AsyncReadExt as _;

        let target = target.trim();
        if target.contains("://") || target.starts_with("data:") || target.starts_with("file:") {
            return Err(SignalMarkerError::Refused("scheme"));
        }
        let workspace = self
            .workspace_dir
            .as_deref()
            .ok_or(SignalMarkerError::Refused("no_workspace"))?;
        let workspace = tokio::fs::canonicalize(workspace)
            .await
            .map_err(|_| SignalMarkerError::Refused("no_workspace"))?;

        // `join` keeps an absolute target as-is and anchors a relative one in
        // the workspace; canonicalizing then resolves `..` and symlinks.
        let path = match tokio::fs::canonicalize(workspace.join(target)).await {
            Ok(path) => path,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(SignalMarkerError::Failed("not_found"));
            }
            Err(_) => return Err(SignalMarkerError::Failed("read_error")),
        };
        if !path.starts_with(&workspace) {
            return Err(SignalMarkerError::Refused("outside_workspace"));
        }

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| SignalMarkerError::Failed("read_error"))?;
        if !metadata.is_file() {
            return Err(SignalMarkerError::Failed("not_a_file"));
        }
        let size_error = |len: u64| {
            if len > SIGNAL_MAX_ATTACHMENT_BYTES {
                SignalMarkerError::Failed("too_large")
            } else {
                SignalMarkerError::Failed("over_budget")
            }
        };
        let limit = budget.min(SIGNAL_MAX_ATTACHMENT_BYTES);
        if metadata.len() > limit {
            return Err(size_error(metadata.len()));
        }
        // Capped read: a file that grows after the size check still cannot
        // push past the limit.
        let mut data = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or_default());
        tokio::fs::File::open(&path)
            .await
            .map_err(|_| SignalMarkerError::Failed("read_error"))?
            .take(limit + 1)
            .read_to_end(&mut data)
            .await
            .map_err(|_| SignalMarkerError::Failed("read_error"))?;
        let file_len = data.len() as u64;
        if file_len > limit {
            return Err(size_error(file_len));
        }

        let mime = mime_guess::from_path(&path)
            .first_raw()
            .or_else(|| zeroclaw_api::media::image_mime_from_magic(&data))
            .unwrap_or("application/octet-stream");
        let mut uri = format!("data:{mime};filename={};base64,", data_uri_file_name(&path));
        base64::engine::general_purpose::STANDARD.encode_string(&data, &mut uri);
        Ok((uri, file_len))
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

/// Why an outbound media marker was not attached. Neither variant carries the
/// target, so logging one cannot leak a local path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalMarkerError {
    /// Trust-boundary refusal: the target was never read.
    Refused(&'static str),
    /// Allowed, but the file could not be delivered.
    Failed(&'static str),
}

impl SignalMarkerError {
    fn outcome(self) -> &'static str {
        match self {
            Self::Refused(_) => "refused",
            Self::Failed(_) => "failed",
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::Refused(reason) | Self::Failed(reason) => reason,
        }
    }
}

/// Pick the marker disposition for an inbound attachment. An `image/*`
/// payload is only an image when the provider loader accepts it; otherwise it
/// renders as a document so the shared pipeline never re-inlines bytes the
/// provider rejects (the same rule as Discord's `marker_kind_for`).
fn inbound_marker_kind(content_type: &str, file_name: &str, data: &[u8]) -> MarkerKind {
    if content_type.starts_with("image/") {
        if zeroclaw_api::media::provider_loadable_image_mime_for(file_name, data).is_some() {
            MarkerKind::Image
        } else {
            MarkerKind::Document
        }
    } else if content_type.starts_with("audio/") {
        MarkerKind::Audio
    } else if content_type.starts_with("video/") {
        MarkerKind::Video
    } else {
        MarkerKind::Document
    }
}

/// The `[KIND:target]` label rendered for a disposition.
fn marker_label(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Image => "IMAGE",
        MarkerKind::Audio => "AUDIO",
        MarkerKind::Video => "VIDEO",
        MarkerKind::Document => "DOCUMENT",
    }
}

/// Write inbound attachment bytes under the workspace. `file_name` must
/// already be a bare file name; the UUID prefix keeps repeated names (and the
/// common nameless `attachment`) from overwriting each other.
async fn save_inbound_attachment(
    workspace: &Path,
    file_name: &str,
    data: &[u8],
) -> anyhow::Result<PathBuf> {
    use tokio::io::AsyncWriteExt as _;

    let dir = workspace.join(SIGNAL_ATTACHMENT_SAVE_SUBDIR);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}_{file_name}", Uuid::new_v4()));
    // Streamed through tokio's bounded write buffer; `tokio::fs::write` would
    // first copy the whole payload.
    let mut file = tokio::fs::File::create(&path).await?;
    file.write_all(data).await?;
    file.flush().await?;
    Ok(path)
}

/// Padded base64 length of `n` bytes.
fn base64_len(n: u64) -> u64 {
    n.div_ceil(3) * 4
}

/// Read a response body, failing as soon as it passes `limit` bytes rather
/// than after buffering all of it.
async fn read_body_limited(mut resp: reqwest::Response, limit: u64) -> anyhow::Result<Vec<u8>> {
    let too_large = || {
        anyhow::Error::msg(format!(
            "signal-cli response exceeds the {limit}-byte limit"
        ))
    };
    if resp.content_length().is_some_and(|len| len > limit) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if (body.len() + chunk.len()) as u64 > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Saved-file form of a sender-supplied file name. The shared marker parser
/// ends a marker at the first `]` and trims its target, so brackets, control
/// characters, and edge whitespace would leave the saved path unreachable
/// through its own marker.
fn marker_safe_file_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "attachment".to_string();
    }
    name.chars()
        .map(|c| {
            if matches!(c, '[' | ']') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// File name for a signal-cli data URI, limited to characters that cannot be
/// confused with data-URI syntax.
fn data_uri_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// signal-cli echoes a rejected attachment argument in its error text. For a
/// data URI that argument is the whole file, so each URI is replaced before
/// the error can reach a log.
fn redact_data_uris(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(512));
    let mut rest = text;
    while let Some(start) = rest.find("data:") {
        let at_token_start = rest[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let (head, tail) = rest.split_at(start);
        out.push_str(head);
        if at_token_start {
            out.push_str("data:<redacted>");
            let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
            rest = &tail[end..];
        } else {
            out.push_str("data:");
            rest = &tail["data:".len()..];
        }
    }
    out.push_str(rest);
    out
}

/// Count-only notice appended when outbound media markers could not be
/// delivered. Targets and reasons stay in the log.
fn signal_delivery_failure_note(failure_count: usize) -> Option<String> {
    if failure_count == 0 {
        return None;
    }
    let count = failure_count.to_string();
    let key = if failure_count == 1 {
        "channel-signal-delivery-failure-note-one"
    } else {
        "channel-signal-delivery-failure-note-many"
    };
    Some(zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        key,
        &[("count", count.as_str())],
    ))
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

    /// A Signal 1:1 DM carries the bare sender (E.164 / UUID) as its
    /// `reply_target`, whereas a group message carries `group:<id>`
    /// (`GROUP_TARGET_PREFIX`). Reusing `parse_recipient_target` — the same
    /// classifier `send`/`reply_target` rely on — a `Direct` target is a DM.
    ///
    /// Without this override Signal fell back to the trait default (`false`),
    /// so every DM was treated as non-direct and ran through the reply-intent
    /// precheck; plain 1:1 messages (e.g. a greeting) could be classified
    /// `NO_REPLY` and silently dropped. Reporting DMs as direct lets the
    /// orchestrator skip the classifier and always answer them, while group
    /// traffic still goes through the precheck.
    fn is_direct_message(&self, msg: &ChannelMessage) -> bool {
        matches!(
            Self::parse_recipient_target(&msg.reply_target),
            RecipientTarget::Direct(_)
        )
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let (text, attachments) = self.prepare_outbound(&message.content).await;
        let mut params = match Self::parse_recipient_target(&message.recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "message": text,
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "message": text,
                "account": &self.account,
            }),
        };
        if !attachments.is_empty() {
            params["attachments"] = serde_json::Value::from(attachments);
        }

        self.rpc_request("send", params).await?;
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
                                            if let Some(resolution) =
                                                self.resolve_approval_reply(&msg).await
                                                && !matches!(
                                                    resolution,
                                                    crate::util::PendingApprovalResolution::NotFound
                                                )
                                            {
                                                consumed_as_approval = true;
                                                continue;
                                            }
                                            let Some(msg) =
                                                self.attach_inbound_media(msg, envelope).await
                                            else {
                                                continue;
                                            };
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
                                if let Some(resolution) = self.resolve_approval_reply(&msg).await
                                    && !matches!(
                                        resolution,
                                        crate::util::PendingApprovalResolution::NotFound
                                    )
                                {
                                    continue;
                                }
                                let Some(msg) = self.attach_inbound_media(msg, envelope).await
                                else {
                                    continue;
                                };
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

    /// Delegates to [`Self::request_approval_attributed`] and drops the
    /// provenance, so the prompt/timeout logic lives in exactly one place.
    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        let token = crate::util::new_approval_token();
        let text = crate::util::build_yesno_approval_prompt(
            &token,
            &request.tool_name,
            &request.arguments_summary,
            request.position_counter(),
        );

        let (tx, rx) = oneshot::channel();
        self.pending_approvals.lock().await.insert(
            token.clone(),
            crate::util::PendingApproval {
                sender: tx,
                destination: Self::canonical_destination(recipient),
                tool_name: request.tool_name.clone(),
            },
        );
        let mut guard = crate::util::PendingApprovalGuard::new(
            Arc::clone(&self.pending_approvals),
            token.clone(),
        );

        if let Err(err) = self.send(&SendMessage::new(text, recipient)).await {
            guard.remove().await;
            return Err(err);
        }

        // Only a real token-echo reply is an operator decision; the
        // dropped-sender and timeout arms are the runtime denying on its own.
        let attributed =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), rx).await {
                Ok(Ok(resp)) => {
                    guard.disarm();
                    zeroclaw_api::channel::AttributedApprovalResponse::operator(resp)
                }
                Ok(Err(_)) => {
                    guard.remove().await;
                    zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                        ChannelApprovalResponse::Deny,
                        zeroclaw_api::channel::ApprovalSource::Unreachable,
                    )
                }
                Err(_) => {
                    guard.remove().await;
                    zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                        ChannelApprovalResponse::Deny,
                        zeroclaw_api::channel::ApprovalSource::TimedOut,
                    )
                }
            };
        Ok(Some(attributed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_envelope(source_number: Option<&str>, message: Option<&str>) -> Envelope {
        Envelope {
            source: source_number.map(String::from),
            source_number: source_number.map(String::from),
            source_uuid: None,
            data_message: message.map(|m| DataMessage {
                message: Some(m.to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
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
    fn is_direct_message_true_for_dm_target_false_for_group() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            false,
            false,
        );
        let e164_dm = ChannelMessage {
            reply_target: "+1111111111".to_string(),
            ..Default::default()
        };
        let uuid_dm = ChannelMessage {
            reply_target: "a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string(),
            ..Default::default()
        };
        let group = ChannelMessage {
            reply_target: "group:group123".to_string(),
            ..Default::default()
        };
        assert!(ch.is_direct_message(&e164_dm));
        assert!(ch.is_direct_message(&uuid_dm));
        assert!(!ch.is_direct_message(&group));
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
    fn canonical_destination_normalizes_bare_and_prefixed_groups() {
        assert_eq!(
            SignalChannel::canonical_destination("abc123"),
            "group:abc123"
        );
        assert_eq!(
            SignalChannel::canonical_destination("group:abc123"),
            "group:abc123"
        );
        assert_eq!(
            SignalChannel::canonical_destination("+1234567890"),
            "+1234567890"
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
            source_uuid: Some("uuid-456".to_string()),
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
            source_uuid: Some("uuid-456".to_string()),
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
            source_uuid: None,
            data_message: Some(DataMessage {
                message: Some("Hello from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
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
            source_uuid: None,
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
    fn sender_falls_back_to_source_uuid() {
        let env: Envelope = serde_json::from_str(
            r#"{
                "source": "",
                "sourceNumber": "",
                "sourceUuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
            }"#,
        )
        .unwrap();

        assert_eq!(
            SignalChannel::sender(&env),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
        );
    }

    #[test]
    fn sender_none_when_all_sources_missing() {
        let env: Envelope = serde_json::from_str(
            r#"{
                "source": "",
                "sourceNumber": null,
                "sourceUuid": "",
                "dataMessage": {
                    "message": "unattributed",
                    "timestamp": 1700000000000
                }
            }"#,
        )
        .unwrap();

        assert_eq!(SignalChannel::sender(&env), None);
        assert!(make_channel().process_envelope(&env).is_empty());
    }

    #[test]
    fn process_envelope_source_uuid_respects_allowlist() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let raw = format!(
            r#"{{
                "source": null,
                "sourceNumber": null,
                "sourceUuid": "{uuid}",
                "timestamp": 1700000000000,
                "dataMessage": {{
                    "message": "Hello from sourceUuid",
                    "timestamp": 1700000000000
                }}
            }}"#
        );
        let env: Envelope = serde_json::from_str(&raw).unwrap();

        let allowed = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(move || vec![uuid.to_string()]),
            false,
            false,
        );
        let denied = make_channel();

        let messages = allowed.process_envelope(&env);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender, uuid);
        assert_eq!(messages[0].content, "Hello from sourceUuid");
        assert!(denied.process_envelope(&env).is_empty());
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
            source_uuid: None,
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![serde_json::json!({"contentType": "image/png"})]),
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
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
            source_uuid: None,
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
            source_uuid: None,
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
        ch.pending_approvals.lock().await.insert(
            "abc123".to_string(),
            crate::util::PendingApproval {
                sender: tx,
                destination: "+1111111111".to_string(),
                tool_name: "tool".to_string(),
            },
        );
        let resolution = crate::util::resolve_pending_approval(
            &ch.pending_approvals,
            "abc123",
            ChannelApprovalResponse::Approve,
            true,
            "+1111111111",
        )
        .await;
        assert_eq!(resolution, crate::util::PendingApprovalResolution::Resolved);
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    #[tokio::test]
    async fn approval_reply_uses_canonical_group_destination_and_rejects_replay() {
        let ch = make_channel();
        let mut group_envelope = make_envelope(Some("+1111111111"), Some("abc123 deny"));
        group_envelope.data_message.as_mut().unwrap().group_info = Some(GroupInfo {
            group_id: Some("group123".to_string()),
        });
        let (approval_tx, mut approval_rx) = oneshot::channel();
        ch.pending_approvals.lock().await.insert(
            "abc123".to_string(),
            crate::util::PendingApproval {
                sender: approval_tx,
                destination: "group:other-group".to_string(),
                tool_name: "tool".to_string(),
            },
        );
        let msg = ch.process_envelope(&group_envelope).pop().unwrap();
        assert_eq!(
            ch.resolve_approval_reply(&msg).await,
            Some(crate::util::PendingApprovalResolution::Rejected)
        );
        assert!(ch.pending_approvals.lock().await.contains_key("abc123"));
        assert!(matches!(
            approval_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        let mut right_envelope = group_envelope;
        right_envelope.data_message.as_mut().unwrap().message = Some("abc123 approve".to_string());
        right_envelope.data_message.as_mut().unwrap().group_info = Some(GroupInfo {
            group_id: Some("other-group".to_string()),
        });
        let right_msg = ch.process_envelope(&right_envelope).pop().unwrap();
        assert_eq!(
            ch.resolve_approval_reply(&right_msg).await,
            Some(crate::util::PendingApprovalResolution::Resolved)
        );
        assert_eq!(approval_rx.await.unwrap(), ChannelApprovalResponse::Approve);
        assert_eq!(
            ch.resolve_approval_reply(&right_msg).await,
            Some(crate::util::PendingApprovalResolution::NotFound)
        );
    }

    #[tokio::test]
    async fn approval_reply_resolves_direct_e164_destination() {
        let ch = make_channel();
        let (tx, rx) = oneshot::channel();
        ch.pending_approvals.lock().await.insert(
            "direct".to_string(),
            crate::util::PendingApproval {
                sender: tx,
                destination: "+1111111111".to_string(),
                tool_name: "tool".to_string(),
            },
        );
        let envelope = make_envelope(Some("+1111111111"), Some("direct yes"));
        let msg = ch.process_envelope(&envelope).pop().unwrap();
        assert_eq!(
            ch.resolve_approval_reply(&msg).await,
            Some(crate::util::PendingApprovalResolution::Resolved)
        );
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
            source_uuid: None,
            data_message: Some(DataMessage {
                message: Some("hi".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
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
            source_uuid: None,
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
            timestamp: Some(1_700_000_000_000),
        }
    }

    fn poll_vote_envelope(sender: Option<&str>, option_indexes: Vec<u32>) -> Envelope {
        Envelope {
            source: sender.map(String::from),
            source_number: sender.map(String::from),
            source_uuid: None,
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

    // ── media attachments ───────────────────────────────────────

    /// PNG signature plus padding: enough for the provider loader to accept.
    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0";

    fn b64(data: &[u8]) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
    }

    fn media_channel(http_url: &str, ignore_attachments: bool) -> SignalChannel {
        SignalChannel::new(
            http_url.to_string(),
            "+1234567890".to_string(),
            Vec::new(),
            false,
            "signal_test_alias",
            Arc::new(|| vec!["*".into()]),
            ignore_attachments,
            false,
        )
    }

    fn attachment_envelope(
        message: Option<&str>,
        group_id: Option<&str>,
        attachments: Vec<serde_json::Value>,
    ) -> Envelope {
        Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            source_uuid: None,
            data_message: Some(DataMessage {
                message: message.map(String::from),
                timestamp: Some(1_700_000_000_000),
                group_info: group_id.map(|id| GroupInfo {
                    group_id: Some(id.to_string()),
                }),
                attachments: Some(attachments),
                poll_answer: None,
                poll_vote: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    fn rpc_method(name: &str) -> wiremock::MockBuilder {
        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_partial_json(serde_json::json!({ "method": name })))
    }

    fn rpc_result(result: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "result": result,
            "id": "1",
        }))
    }

    fn attachment_data(data: &[u8]) -> ResponseTemplate {
        rpc_result(serde_json::json!({ "data": b64(data) }))
    }

    /// A mock signal-cli that accepts `send`.
    async fn send_server() -> MockServer {
        let server = MockServer::start().await;
        rpc_method("send")
            .respond_with(rpc_result(serde_json::json!({ "timestamp": 1 })))
            .mount(&server)
            .await;
        server
    }

    /// A mock signal-cli whose `getAttachment` answers with `response` and
    /// must be called exactly `calls` times.
    async fn attachment_server(response: ResponseTemplate, calls: u64) -> MockServer {
        let server = MockServer::start().await;
        rpc_method("getAttachment")
            .respond_with(response)
            .expect(calls)
            .mount(&server)
            .await;
        server
    }

    /// Params of every recorded call to the given signal-cli method.
    async fn rpc_params(server: &MockServer, name: &str) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .expect("request recording is on")
            .iter()
            .filter_map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).ok())
            .filter(|body| body["method"] == name)
            .map(|body| body["params"].clone())
            .collect()
    }

    async fn send_and_capture(
        ch: &SignalChannel,
        server: &MockServer,
        msg: SendMessage,
    ) -> serde_json::Value {
        ch.send(&msg).await.expect("send succeeds");
        let mut sent = rpc_params(server, "send").await;
        assert_eq!(sent.len(), 1, "exactly one send call");
        sent.remove(0)
    }

    /// The inbound steps `listen` runs for a single-message envelope.
    async fn receive(ch: &SignalChannel, env: &Envelope) -> Option<ChannelMessage> {
        let msg = ch.process_envelope(env).pop().expect("message emitted");
        ch.attach_inbound_media(msg, env).await
    }

    #[tokio::test]
    async fn send_attaches_workspace_marker_as_data_uri() {
        let server = send_server().await;
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("photo.png"), PNG_BYTES).unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());

        let msg = SendMessage::new("Here it is [IMAGE:photo.png]", "+1111111111");
        let params = send_and_capture(&ch, &server, msg).await;

        assert_eq!(params["recipient"], serde_json::json!(["+1111111111"]));
        assert_eq!(params["message"], "Here it is");
        let expected = format!(
            "data:image/png;filename=photo.png;base64,{}",
            b64(PNG_BYTES)
        );
        assert_eq!(params["attachments"], serde_json::json!([expected]));
    }

    #[tokio::test]
    async fn send_attaches_absolute_workspace_path_for_group_recipient() {
        let server = send_server().await;
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("notes.pdf");
        std::fs::write(&file, b"%PDF-1.4").unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());

        let msg = SendMessage::new(format!("[DOCUMENT:{}]", file.display()), "group:grp123");
        let params = send_and_capture(&ch, &server, msg).await;

        assert_eq!(params["groupId"], "grp123");
        assert!(params.get("recipient").is_none());
        assert_eq!(params["message"], "");
        let attachment = params["attachments"][0].as_str().expect("one attachment");
        assert!(
            attachment.starts_with("data:application/pdf;filename=notes.pdf;base64,"),
            "{attachment}"
        );
    }

    #[tokio::test]
    async fn send_drops_unresolved_markers_and_appends_count_note() {
        let server = send_server().await;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let outside_path = outside.path().display().to_string();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());

        let content = format!(
            "Report [DOCUMENT:{outside_path}] [IMAGE:missing.png] [IMAGE:https://example.com/a.png]"
        );
        let params = send_and_capture(&ch, &server, SendMessage::new(content, "+1111111111")).await;

        let message = params["message"].as_str().expect("message text");
        assert!(message.starts_with("Report"), "{message}");
        assert!(
            message.contains('3'),
            "note must carry the failure count: {message}"
        );
        for leaked in [outside_path.as_str(), "missing.png", "example.com"] {
            assert!(
                !message.contains(leaked),
                "target leaked into reply: {message}"
            );
        }
        assert!(params.get("attachments").is_none());
    }

    #[tokio::test]
    async fn send_with_only_failed_markers_sends_the_note_alone() {
        let server = send_server().await;
        let ch = media_channel(&server.uri(), false);

        let msg = SendMessage::new("[IMAGE:photo.png]", "+1111111111");
        let params = send_and_capture(&ch, &server, msg).await;

        assert_eq!(
            params["message"],
            signal_delivery_failure_note(1).expect("note")
        );
        assert!(params.get("attachments").is_none());
    }

    #[tokio::test]
    async fn send_without_media_markers_keeps_text_verbatim() {
        let server = send_server().await;
        let ch = media_channel(&server.uri(), false);

        let text = "  meet here [LOCATION:40.7,-74.0] later  ";
        let params = send_and_capture(&ch, &server, SendMessage::new(text, "+1111111111")).await;

        assert_eq!(params["message"], text);
        assert!(params.get("attachments").is_none());
    }

    #[tokio::test]
    async fn outbound_marker_refuses_urls_and_missing_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel("http://127.0.0.1:1", false);
        assert_eq!(
            ch.resolve_outbound_marker("photo.png", SIGNAL_MESSAGE_ATTACHMENT_BUDGET)
                .await,
            Err(SignalMarkerError::Refused("no_workspace"))
        );

        let ch = ch.with_workspace_dir(workspace.path().into());
        for target in [
            "https://example.com/a.png",
            "http://example.com/a.png",
            "data:image/png;base64,AAAA",
            "file:///etc/hostname",
        ] {
            assert_eq!(
                ch.resolve_outbound_marker(target, SIGNAL_MESSAGE_ATTACHMENT_BUDGET)
                    .await,
                Err(SignalMarkerError::Refused("scheme")),
                "{target}"
            );
        }
    }

    #[tokio::test]
    async fn outbound_marker_refuses_paths_outside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("sub")).unwrap();
        // Both temp entries share the system temp dir, so `sub/../../<name>`
        // climbs out of the workspace and lands on the outside file.
        let outside = tempfile::NamedTempFile::new().unwrap();
        let outside_name = outside.path().file_name().unwrap().to_str().unwrap();
        let ch =
            media_channel("http://127.0.0.1:1", false).with_workspace_dir(workspace.path().into());

        for target in [
            outside.path().display().to_string(),
            format!("sub/../../{outside_name}"),
        ] {
            assert_eq!(
                ch.resolve_outbound_marker(&target, SIGNAL_MESSAGE_ATTACHMENT_BUDGET)
                    .await,
                Err(SignalMarkerError::Refused("outside_workspace")),
                "{target}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn outbound_marker_refuses_symlink_escaping_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("link.txt")).unwrap();
        let ch =
            media_channel("http://127.0.0.1:1", false).with_workspace_dir(workspace.path().into());

        assert_eq!(
            ch.resolve_outbound_marker("link.txt", SIGNAL_MESSAGE_ATTACHMENT_BUDGET)
                .await,
            Err(SignalMarkerError::Refused("outside_workspace"))
        );
    }

    #[tokio::test]
    async fn outbound_marker_reports_missing_directory_and_oversized_targets() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("folder")).unwrap();
        let big = std::fs::File::create(workspace.path().join("big.bin")).unwrap();
        big.set_len(SIGNAL_MAX_ATTACHMENT_BYTES + 1).unwrap();
        let ch =
            media_channel("http://127.0.0.1:1", false).with_workspace_dir(workspace.path().into());

        for (target, reason) in [
            ("missing.png", "not_found"),
            ("folder", "not_a_file"),
            ("big.bin", "too_large"),
        ] {
            assert_eq!(
                ch.resolve_outbound_marker(target, SIGNAL_MESSAGE_ATTACHMENT_BUDGET)
                    .await,
                Err(SignalMarkerError::Failed(reason)),
                "{target}"
            );
        }
    }

    #[test]
    fn delivery_failure_note_is_count_only() {
        assert!(signal_delivery_failure_note(0).is_none());
        for count in [1usize, 2] {
            let note = signal_delivery_failure_note(count).expect("note");
            // Locale-independent: every catalog renders `{$count}` as digits.
            assert!(note.contains(&count.to_string()), "{note}");
            // A missing Fluent key renders as `{key}`.
            assert!(!note.starts_with('{'), "missing Fluent key: {note}");
        }
    }

    #[tokio::test]
    async fn inbound_image_is_saved_and_marked() {
        let server = attachment_server(attachment_data(PNG_BYTES), 1).await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());
        let env = attachment_envelope(
            Some("look"),
            None,
            vec![serde_json::json!({
                "id": "att1",
                "contentType": "image/png",
                "filename": "cat.png",
                "size": PNG_BYTES.len(),
            })],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        assert_eq!(msg.attachments.len(), 1);
        let attachment = &msg.attachments[0];
        assert_eq!(attachment.data, PNG_BYTES);
        let marker = attachment.marker.as_ref().expect("rendered marker");
        assert_eq!(marker.kind, MarkerKind::Image);
        let saved = Path::new(&marker.target);
        assert!(saved.starts_with(workspace.path().join(SIGNAL_ATTACHMENT_SAVE_SUBDIR)));
        assert_eq!(std::fs::read(saved).unwrap(), PNG_BYTES);
        assert_eq!(msg.content, format!("look\n[IMAGE:{}]", marker.target));

        let downloads = rpc_params(&server, "getAttachment").await;
        assert_eq!(downloads[0]["id"], "att1");
        assert_eq!(downloads[0]["recipient"], "+1111111111");
        assert!(downloads[0].get("groupId").is_none());
    }

    #[tokio::test]
    async fn inbound_attachment_only_group_document_is_delivered() {
        let server = attachment_server(attachment_data(b"%PDF-1.4"), 1).await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());
        let env = attachment_envelope(
            None,
            Some("grp123"),
            vec![serde_json::json!({
                "id": "att2",
                "contentType": "application/pdf",
                "filename": "../../etc/report.pdf",
            })],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        let marker = msg.attachments[0].marker.as_ref().expect("rendered marker");
        assert_eq!(marker.kind, MarkerKind::Document);
        assert_eq!(msg.content, format!("[DOCUMENT:{}]", marker.target));
        let saved = Path::new(&marker.target);
        let save_dir = workspace.path().join(SIGNAL_ATTACHMENT_SAVE_SUBDIR);
        assert_eq!(saved.parent(), Some(save_dir.as_path()));
        let saved_name = saved.file_name().unwrap().to_str().unwrap();
        assert!(saved_name.ends_with("_report.pdf"), "{saved_name}");

        let downloads = rpc_params(&server, "getAttachment").await;
        assert_eq!(downloads[0]["groupId"], "grp123");
        assert!(downloads[0].get("recipient").is_none());
    }

    #[tokio::test]
    async fn inbound_attachments_are_not_downloaded_when_ignored() {
        let server = attachment_server(attachment_data(PNG_BYTES), 0).await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), true).with_workspace_dir(workspace.path().into());
        let env = attachment_envelope(
            Some("caption"),
            None,
            vec![serde_json::json!({ "id": "att1", "contentType": "image/png" })],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        assert_eq!(msg.content, "caption");
        assert!(msg.attachments.is_empty());
    }

    #[tokio::test]
    async fn inbound_without_workspace_passes_bytes_without_marker() {
        let server = attachment_server(attachment_data(PNG_BYTES), 1).await;
        let ch = media_channel(&server.uri(), false);
        let env = attachment_envelope(
            Some("hi"),
            None,
            vec![serde_json::json!({ "id": "att1", "contentType": "image/png" })],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        assert_eq!(msg.content, "hi");
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].data, PNG_BYTES);
        assert!(msg.attachments[0].marker.is_none());
    }

    #[tokio::test]
    async fn inbound_download_failure_keeps_text_and_drops_empty_message() {
        let failure = ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "error": { "code": -1, "message": "attachment unavailable" },
            "id": "1",
        }));
        let server = attachment_server(failure, 2).await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());
        let attachment = serde_json::json!({ "id": "att1", "contentType": "image/png" });

        let env = attachment_envelope(Some("caption"), None, vec![attachment.clone()]);
        let msg = receive(&ch, &env).await.expect("text survives");
        assert_eq!(msg.content, "caption");
        assert!(msg.attachments.is_empty());

        let env = attachment_envelope(None, None, vec![attachment]);
        assert!(receive(&ch, &env).await.is_none());
    }

    #[tokio::test]
    async fn inbound_oversized_attachment_is_skipped_without_download() {
        let server = attachment_server(attachment_data(PNG_BYTES), 0).await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());
        let env = attachment_envelope(
            Some("big"),
            None,
            vec![serde_json::json!({
                "id": "att1",
                "contentType": "video/mp4",
                "size": SIGNAL_MAX_ATTACHMENT_BYTES + 1,
            })],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        assert_eq!(msg.content, "big");
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn redact_data_uris_replaces_each_uri_and_keeps_other_text() {
        let text = "Failed to send message: data:image/png;filename=a.png;base64,iVBORw0KGgo: \
                    Invalid attachment (AttachmentInvalidException) data:text/plain;base64,QUJD";
        assert_eq!(
            redact_data_uris(text),
            "Failed to send message: data:<redacted> Invalid attachment \
             (AttachmentInvalidException) data:<redacted>"
        );
        assert_eq!(
            redact_data_uris("bad metadata: field"),
            "bad metadata: field"
        );
        assert_eq!(redact_data_uris("no uri here"), "no uri here");
    }

    #[tokio::test]
    async fn send_error_does_not_echo_attachment_contents() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("photo.png"), PNG_BYTES).unwrap();
        let payload = b64(PNG_BYTES);
        let server = MockServer::start().await;
        rpc_method("send")
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -1,
                    "message": format!(
                        "Failed to send message: data:image/png;filename=photo.png;base64,{payload}: \
                         too large (AttachmentInvalidException)"
                    ),
                },
                "id": "1",
            })))
            .mount(&server)
            .await;
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());

        let err = ch
            .send(&SendMessage::new("[IMAGE:photo.png]", "+1111111111"))
            .await
            .expect_err("signal-cli rejected the send");

        let text = format!("{err:#}");
        assert!(!text.contains(&payload), "attachment data leaked: {text}");
        assert!(text.contains("data:<redacted>"), "{text}");
    }

    #[tokio::test]
    async fn inbound_saved_attachment_can_be_sent_back_with_its_marker() {
        let server = attachment_server(attachment_data(b"%PDF-1.4"), 1).await;
        rpc_method("send")
            .respond_with(rpc_result(serde_json::json!({ "timestamp": 1 })))
            .mount(&server)
            .await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());
        let env = attachment_envelope(
            None,
            None,
            vec![serde_json::json!({
                "id": "att3",
                "contentType": "application/pdf",
                "filename": "report.pdf",
            })],
        );
        let inbound = receive(&ch, &env).await.expect("delivered");
        let marker = inbound.attachments[0]
            .marker
            .as_ref()
            .expect("rendered marker");
        let saved_name = Path::new(&marker.target)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();

        // The agent copies the inbound marker verbatim into its reply.
        let reply = format!("Here it is again {}", inbound.content);
        let params = send_and_capture(&ch, &server, SendMessage::new(reply, "+1111111111")).await;

        assert_eq!(params["message"], "Here it is again");
        let expected = format!(
            "data:application/pdf;filename={saved_name};base64,{}",
            b64(b"%PDF-1.4")
        );
        assert_eq!(params["attachments"], serde_json::json!([expected]));
    }

    #[tokio::test]
    async fn inbound_attachments_past_the_message_budget_are_not_downloaded() {
        let payload = vec![7u8; 600];
        let server = attachment_server(attachment_data(&payload), 1).await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false)
            .with_workspace_dir(workspace.path().into())
            .with_attachment_budget(600);
        let env = attachment_envelope(
            Some("three files"),
            None,
            vec![
                serde_json::json!({ "id": "att1", "contentType": "application/pdf", "size": 600 }),
                // No declared size: only the exhausted budget can stop it.
                serde_json::json!({ "id": "att2", "contentType": "application/pdf" }),
                serde_json::json!({ "id": "att3", "contentType": "application/pdf", "size": 10 }),
            ],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].data, payload);
        let marker = msg.attachments[0].marker.as_ref().expect("rendered marker");
        assert_eq!(
            msg.content,
            format!("three files\n[DOCUMENT:{}]", marker.target)
        );
        let downloads = rpc_params(&server, "getAttachment").await;
        assert_eq!(downloads.len(), 1, "budget spent: no further downloads");
        assert_eq!(downloads[0]["id"], "att1");
    }

    #[tokio::test]
    async fn inbound_declared_size_past_the_remaining_budget_skips_the_download() {
        let server = attachment_server(attachment_data(&[1u8; 600]), 1).await;
        let ch = media_channel(&server.uri(), false).with_attachment_budget(1000);
        let env = attachment_envelope(
            Some("two files"),
            None,
            vec![
                serde_json::json!({ "id": "att1", "contentType": "image/png", "size": 600 }),
                serde_json::json!({ "id": "att2", "contentType": "image/png", "size": 600 }),
            ],
        );

        let msg = receive(&ch, &env).await.expect("delivered");

        assert_eq!(msg.content, "two files");
        assert_eq!(msg.attachments.len(), 1);
        let downloads = rpc_params(&server, "getAttachment").await;
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0]["id"], "att1");
    }

    #[tokio::test]
    async fn inbound_response_past_the_cap_is_refused_without_a_declared_size() {
        // The payload itself fits the budget; only the response is oversized
        // (well past base64(budget) plus the envelope allowance), so the
        // download can only be refused by the response cap.
        let oversized = rpc_result(serde_json::json!({
            "data": b64(b"%PDF-1.4"),
            "padding": "x".repeat(2 * SIGNAL_RPC_ENVELOPE_BYTES as usize),
        }));
        let server = attachment_server(oversized, 2).await;
        let ch = media_channel(&server.uri(), false).with_attachment_budget(1024);

        let err = ch
            .rpc_request_limited(
                "getAttachment",
                serde_json::json!({ "id": "att1" }),
                Some(base64_len(1024) + SIGNAL_RPC_ENVELOPE_BYTES),
            )
            .await
            .expect_err("response is over the cap");
        assert!(format!("{err:#}").contains("byte limit"), "{err:#}");

        let env = attachment_envelope(
            Some("caption"),
            None,
            vec![serde_json::json!({ "id": "att1", "contentType": "video/mp4" })],
        );
        let msg = receive(&ch, &env).await.expect("text survives");
        assert_eq!(msg.content, "caption");
        assert!(msg.attachments.is_empty());
    }

    #[tokio::test]
    async fn send_stops_attaching_files_past_the_message_budget() {
        let server = send_server().await;
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.bin"), [1u8; 600]).unwrap();
        std::fs::write(workspace.path().join("b.bin"), [2u8; 600]).unwrap();
        std::fs::write(workspace.path().join("c.bin"), [3u8; 300]).unwrap();
        let ch = media_channel(&server.uri(), false)
            .with_workspace_dir(workspace.path().into())
            .with_attachment_budget(1000);

        let msg = SendMessage::new(
            "Files [DOCUMENT:a.bin] [DOCUMENT:b.bin] [DOCUMENT:c.bin]",
            "+1111111111",
        );
        let params = send_and_capture(&ch, &server, msg).await;

        let attachments = params["attachments"].as_array().expect("attachments");
        assert_eq!(attachments.len(), 2, "a.bin and c.bin fit; b.bin does not");
        assert!(
            attachments[0]
                .as_str()
                .unwrap()
                .ends_with(&b64(&[1u8; 600]))
        );
        assert!(
            attachments[1]
                .as_str()
                .unwrap()
                .ends_with(&b64(&[3u8; 300]))
        );
        let message = params["message"].as_str().expect("message text");
        assert!(message.starts_with("Files"), "{message}");
        assert!(
            message.contains('1'),
            "note counts the dropped file: {message}"
        );

        assert_eq!(
            ch.resolve_outbound_marker("b.bin", 400).await,
            Err(SignalMarkerError::Failed("over_budget"))
        );
    }

    #[test]
    fn marker_safe_file_name_keeps_saved_paths_parseable() {
        for (name, expected) in [
            ("report]v2.pdf", "report_v2.pdf"),
            ("[draft] notes.txt", "_draft_ notes.txt"),
            ("line\nbreak.txt", "line_break.txt"),
            ("trailing.pdf  ", "trailing.pdf"),
            ("写真.jpg", "写真.jpg"),
            (" \t ", "attachment"),
        ] {
            assert_eq!(marker_safe_file_name(name), expected, "{name:?}");
        }
    }

    #[tokio::test]
    async fn inbound_bracketed_file_name_round_trips_through_its_marker() {
        let server = attachment_server(attachment_data(b"%PDF-1.4"), 1).await;
        rpc_method("send")
            .respond_with(rpc_result(serde_json::json!({ "timestamp": 1 })))
            .mount(&server)
            .await;
        let workspace = tempfile::tempdir().unwrap();
        let ch = media_channel(&server.uri(), false).with_workspace_dir(workspace.path().into());
        let env = attachment_envelope(
            None,
            None,
            vec![serde_json::json!({
                "id": "att4",
                "contentType": "application/pdf",
                "filename": "report]v2.pdf",
            })],
        );

        let inbound = receive(&ch, &env).await.expect("delivered");
        assert_eq!(inbound.attachments[0].file_name, "report]v2.pdf");
        let marker = inbound.attachments[0]
            .marker
            .as_ref()
            .expect("rendered marker");
        assert!(
            marker.target.ends_with("_report_v2.pdf"),
            "{}",
            marker.target
        );

        let reply = format!("Again {}", inbound.content);
        let params = send_and_capture(&ch, &server, SendMessage::new(reply, "+1111111111")).await;

        assert_eq!(params["message"], "Again");
        let attachment = params["attachments"][0].as_str().expect("one attachment");
        assert!(attachment.ends_with(&b64(b"%PDF-1.4")), "{attachment}");
    }
}
