use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::MediaAttachment;
use zeroclaw_runtime::i18n;

// Contract tier: `embed` holds the embed value object that `types`'
// `DiscordOutgoing` envelope carries; both are contract modules (no impl
// imports), so the layer stays acyclic. Consumers import from `embed`
// explicitly; `mod.rs` only names `DiscordEmbed` (in the outbound pipeline).
mod embed;
use embed::DiscordEmbed;

mod types;
// Keep the historical public path (`…::discord::DiscordSlashCommandSpec`) stable.
pub use types::{DiscordSlashCommandResolver, DiscordSlashCommandSpec};
// Contract types/codec/consts used throughout this module and its siblings.
pub(crate) use types::*;

// Contract tier: the typed slash-command option model the command spec carries.
// Consumers (`types`, `slash`, dispatch) import `super::slash_options::…`
// explicitly — no crate-wide re-export needed.
mod slash_options;

// custom_id codec + outbound component builders + the inbound single-use
// pending registry. Accessed via explicit paths (`super::components::…`).
mod components;
mod custom_id;
mod pending;
// Buttoned tool-approval surface (Allow-once / Session / Always / Deny) +
// the server-side decision enum a click resolves the approval `oneshot` with.
mod approval;
// Imported bare so the type-3 arm (where a local `pending` var shadows the
// module) can still name it.
use pending::ComponentIntent;

mod chunk;
pub(crate) use chunk::*;

mod markers;
pub(crate) use markers::*;

mod rest;
pub(crate) use rest::*;

mod interaction;
pub(crate) use interaction::*;

mod slash;
// Keep the historical public path for the orchestrator's resolver wiring.
pub use slash::discord_slash_specs_from_skills;
pub(crate) use slash::*;

/// Discord channel — connects via Gateway WebSocket for real-time messages
pub struct DiscordChannel {
    bot_token: String,
    /// Empty = listen across all guilds the bot is invited to.
    guild_ids: Vec<String>,
    /// Empty = watch every channel; non-empty = restrict the bot to listed
    /// channel IDs (for both interaction and archive).
    channel_ids: Vec<String>,
    /// When set, every non-bot message that passes the channel filter is
    /// archived to a sidecar SQLite memory backend (`discord.db`). The
    /// `discord_search` tool reads from this when registered.
    archive_memory: Option<std::sync::Arc<dyn zeroclaw_memory::Memory>>,
    /// The alias key under `[channels.discord.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    listen_to_bots: bool,
    mention_only: bool,
    /// Raw IDENTIFY mask override (config `intents_mask`). `Some` wins over
    /// everything `gateway_intents()` would derive — including `Some(0)`,
    /// a legal IDENTIFY value. Intents are connection-scoped (sent once in
    /// IDENTIFY), so a construction-time snapshot matches the connection
    /// lifecycle exactly: config reloads rebuild channels, which re-derives
    /// the mask.
    intents_mask_override: Option<u64>,
    /// Which inbound reactions to record (config `reaction_notifications`).
    /// Anything other than `Off` adds the two reaction intents to the
    /// IDENTIFY mask. Same connection-scoped snapshot semantics as the
    /// mask override above.
    reaction_scope: zeroclaw_config::schema::DiscordReactionScope,
    typing_handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Voice transcription config — when set, audio attachments are
    /// downloaded, transcribed, and their text inlined into the message.
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    /// Workspace directory for saving downloaded inbound media attachments.
    workspace_dir: Option<PathBuf>,
    /// Streaming mode: Off, Partial (draft edits), or MultiMessage (paragraph splits).
    stream_mode: zeroclaw_config::schema::StreamMode,
    /// Minimum interval (ms) between draft message edits (Partial mode only).
    draft_update_interval_ms: u64,
    /// Delay (ms) between sending each message chunk (MultiMessage mode only).
    multi_message_delay_ms: u64,
    /// Per-channel rate-limit tracking for draft edits.
    last_draft_edit: Mutex<HashMap<String, std::time::Instant>>,
    /// Tracks how much text has been sent in MultiMessage mode.
    multi_message_sent_len: Mutex<HashMap<String, usize>>,
    /// Thread context captured from `send_draft()` for MultiMessage paragraph delivery.
    multi_message_thread_ts: Mutex<HashMap<String, Option<String>>>,
    /// Stall-watchdog timeout in seconds (0 = disabled).
    stall_timeout_secs: u64,
    pending_approvals: Arc<AsyncMutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
    /// Seconds to wait for an operator reply to a `request_approval` prompt
    /// before treating the silence as a deny. Default 300.
    approval_timeout_secs: u64,
    /// Cached `channel_id -> is_thread` lookups. Populated lazily on first
    /// inbound message from a channel via `GET /channels/{id}`. Thread type
    /// is stable for the channel's lifetime so the cache lives as long as
    /// the channel instance.
    ///
    /// Value is `Some(parent_id)` when the channel is a thread, `None`
    /// when it is a regular (non-thread) channel.
    thread_channels: Arc<AsyncMutex<HashMap<String, Option<String>>>>,
    /// Ephemeral Discord gateway session state for Resume across reconnects.
    gateway_session: Mutex<DiscordGatewaySession>,
    /// When true, register and serve Discord slash commands (e.g. `/ask`)
    /// over the existing Gateway WebSocket. Default false. (Prototype.)
    /// Construction-time wiring of `DiscordConfig.slash_commands` — config
    /// is the source of truth; reloads rebuild the channel.
    slash_commands: bool,
    /// Live interaction credentials, held channel-locally so the bearer
    /// token never enters reply targets, logs, session keys, or memory
    /// rows. Keyed by interaction id; swept on insert; entries expire with
    /// Discord's 15-minute followup window.
    pending_interactions: Arc<Mutex<HashMap<String, PendingInteraction>>>,
    /// Single-use registry binding a live component `custom_id` to the
    /// server-side intent it resolves. A click is trusted only if its id is
    /// present here (forged/replayed/expired ids resolve to nothing). Populated
    /// when the channel emits a component; drained on click.
    pending_components: Arc<Mutex<pending::PendingComponents>>,
    /// Resolves skill-derived commands to register alongside `/ask`.
    /// `None` (or an empty resolution) = `/ask` only.
    slash_command_resolver: Option<DiscordSlashCommandResolver>,
}

#[derive(Clone, Debug, Default)]
struct DiscordGatewaySession {
    session_id: Option<String>,
    resume_gateway_url: Option<String>,
    sequence: Option<i64>,
}

impl DiscordChannel {
    pub fn new(
        bot_token: String,
        guild_ids: Vec<String>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        listen_to_bots: bool,
        mention_only: bool,
    ) -> Self {
        Self {
            bot_token,
            guild_ids,
            channel_ids: vec![],
            archive_memory: None,
            alias: alias.into(),
            peer_resolver,
            listen_to_bots,
            mention_only,
            intents_mask_override: None,
            reaction_scope: zeroclaw_config::schema::DiscordReactionScope::Off,
            typing_handles: Mutex::new(HashMap::new()),
            proxy_url: None,
            transcription: None,
            transcription_manager: None,
            workspace_dir: None,
            stream_mode: zeroclaw_config::schema::StreamMode::Off,
            draft_update_interval_ms: 1000,
            multi_message_delay_ms: 800,
            last_draft_edit: Mutex::new(HashMap::new()),
            multi_message_sent_len: Mutex::new(HashMap::new()),
            multi_message_thread_ts: Mutex::new(HashMap::new()),
            stall_timeout_secs: 0,
            pending_approvals: Arc::new(AsyncMutex::new(HashMap::new())),
            approval_timeout_secs: 300,
            thread_channels: Arc::new(AsyncMutex::new(HashMap::new())),
            gateway_session: Mutex::new(DiscordGatewaySession::default()),
            slash_commands: false,
            pending_interactions: Arc::new(Mutex::new(HashMap::new())),
            pending_components: Arc::new(Mutex::new(pending::PendingComponents::default())),
            slash_command_resolver: None,
        }
    }

    /// Provide the resolver for skill-derived slash commands. Only consulted
    /// when `slash_commands` is enabled.
    pub fn with_slash_command_resolver(mut self, resolver: DiscordSlashCommandResolver) -> Self {
        self.slash_command_resolver = Some(resolver);
        self
    }

    /// Enable Discord slash commands (register + serve over the Gateway).
    pub fn with_slash_commands(mut self, enabled: bool) -> Self {
        self.slash_commands = enabled;
        self
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Send exactly `mask` in IDENTIFY when `Some`, ignoring the derived
    /// mask entirely (including `Some(0)`, a legal IDENTIFY value). Operator
    /// escape hatch (config `intents_mask`).
    pub fn with_intents_mask(mut self, mask: Option<u64>) -> Self {
        self.intents_mask_override = mask;
        self
    }

    /// Record inbound reaction events at the given scope. Anything other
    /// than `Off` adds the (unprivileged) reaction intents to the IDENTIFY
    /// mask.
    pub fn with_reaction_notifications(
        mut self,
        scope: zeroclaw_config::schema::DiscordReactionScope,
    ) -> Self {
        self.reaction_scope = scope;
        self
    }

    /// Gateway intent mask for IDENTIFY: the raw `intents_mask` override
    /// when set, otherwise the fixed baseline plus feature-implied intents.
    fn gateway_intents(&self) -> u64 {
        if let Some(mask) = self.intents_mask_override {
            return mask;
        }
        let mut mask = BASELINE_INTENTS;
        if self.reaction_scope != zeroclaw_config::schema::DiscordReactionScope::Off {
            mask |= INTENT_GUILD_MESSAGE_REACTIONS | INTENT_DIRECT_MESSAGE_REACTIONS;
        }
        mask
    }

    pub fn with_approval_timeout_secs(mut self, secs: u64) -> Self {
        self.approval_timeout_secs = secs;
        self
    }

    /// Configure workspace directory for saving downloaded attachments.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Configure voice transcription for audio attachments.
    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(std::sync::Arc::new(m));
                self.transcription = Some(config);
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "transcription manager init failed, voice transcription disabled"
                );
            }
        }
        self
    }

    /// Configure streaming mode for progressive draft updates or multi-message delivery.
    pub fn with_streaming(
        mut self,
        stream_mode: zeroclaw_config::schema::StreamMode,
        draft_update_interval_ms: u64,
        multi_message_delay_ms: u64,
    ) -> Self {
        self.stream_mode = stream_mode;
        self.draft_update_interval_ms = draft_update_interval_ms;
        self.multi_message_delay_ms = multi_message_delay_ms;
        self
    }

    /// Set the stall-watchdog timeout (0 = disabled).
    pub fn with_stall_timeout(mut self, secs: u64) -> Self {
        self.stall_timeout_secs = secs;
        self
    }

    pub fn with_channel_ids(mut self, ids: Vec<String>) -> Self {
        self.channel_ids = ids;
        self
    }

    fn fatal_listener_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(DiscordListenerFatalError::new(message))
    }

    fn validate_gateway_preflight_response(
        response: reqwest::Response,
    ) -> anyhow::Result<reqwest::Response> {
        Ok(response.error_for_status()?)
    }

    pub fn with_archive_memory(mut self, mem: std::sync::Arc<dyn zeroclaw_memory::Memory>) -> Self {
        self.archive_memory = Some(mem);
        self
    }

    /// Keep archived messages in sync when Discord reports them edited or
    /// deleted. Markers are appended rather than replacing content, so the
    /// original text and the edit history stay searchable. Markers are
    /// plain text in the entry body — the same in-band convention as the
    /// create path's `[attachments: …]` marker. They are advisory context
    /// for the agent, not tamper-proof provenance: message content can
    /// imitate them.
    ///
    /// Only messages that were actually archived are touched — the `get`
    /// on the `discord_{message_id}` key gates everything (a message that
    /// failed any create-time filter was never stored). Edits additionally
    /// re-run the author checks against the UPDATE payload: archive-time
    /// authorization is not durable, and a peer removed from the allowlist
    /// must not keep writing into the archive by editing old messages.
    async fn sync_archive_for_message_event(
        &self,
        event_type: &str,
        d: &serde_json::Value,
        bot_user_id: &str,
    ) {
        let Some(archive_mem) = self.archive_memory.clone() else {
            return;
        };
        match event_type {
            "MESSAGE_UPDATE" => self.apply_archive_edit(&archive_mem, d, bot_user_id).await,
            "MESSAGE_DELETE" => {
                let message_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if !message_id.is_empty() {
                    self.apply_archive_tombstone(&archive_mem, message_id).await;
                }
            }
            "MESSAGE_DELETE_BULK" => {
                // Moderation bulk deletes carry `ids`, not `id` — and they
                // are the highest-signal deletions an archive can record.
                let ids = d
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|i| i.as_str()).collect::<Vec<&str>>())
                    .unwrap_or_default();
                for id in ids {
                    self.apply_archive_tombstone(&archive_mem, id).await;
                }
            }
            _ => {}
        }
    }

    /// Fetch the archived entry for a message id, or `None` (logging
    /// lookup failures — a missed sync beats a silent one).
    async fn archived_entry(
        &self,
        archive_mem: &std::sync::Arc<dyn zeroclaw_memory::Memory>,
        key: &str,
    ) -> Option<zeroclaw_memory::MemoryEntry> {
        match archive_mem.get(key).await {
            Ok(entry) => entry,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error": format!("{e}"),
                            "key": key,
                        })),
                    "discord archive lookup failed for message event"
                );
                None
            }
        }
    }

    /// Re-store an entry preserving its category and session attribution.
    async fn restore_archived_entry(
        &self,
        archive_mem: &std::sync::Arc<dyn zeroclaw_memory::Memory>,
        key: &str,
        content: &str,
        existing: &zeroclaw_memory::MemoryEntry,
    ) {
        if let Err(e) = archive_mem
            .store(
                key,
                content,
                existing.category.clone(),
                existing.session_id.as_deref(),
            )
            .await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error": format!("{e}"),
                        "key": key,
                    })),
                "failed to sync discord archive for message event"
            );
        }
    }

    /// Append a deletion tombstone. Idempotent: gateway redelivery (and a
    /// MESSAGE_DELETE racing a bulk delete) must not double-stamp.
    async fn apply_archive_tombstone(
        &self,
        archive_mem: &std::sync::Arc<dyn zeroclaw_memory::Memory>,
        message_id: &str,
    ) {
        let key = format!("discord_{message_id}");
        let Some(existing) = self.archived_entry(archive_mem, &key).await else {
            return;
        };
        if existing.content.contains("[deleted at ") {
            return;
        }
        // Deletes carry no payload timestamp; receipt time is the best
        // available.
        let ts = chrono::Utc::now().to_rfc3339();
        let updated = format!("{} [deleted at {ts}]", existing.content);
        self.restore_archived_entry(archive_mem, &key, &updated, &existing)
            .await;
    }

    /// Append an edit marker for a genuine content edit.
    ///
    /// Discord sends the full message object on every MESSAGE_UPDATE —
    /// embed unfurls, pins, flag changes — with `content` present and
    /// unchanged. Only real content edits set `edited_timestamp`, so that
    /// field gates the append (and keys the idempotency check: redelivered
    /// or duplicate events for the same edit are no-ops).
    async fn apply_archive_edit(
        &self,
        archive_mem: &std::sync::Arc<dyn zeroclaw_memory::Memory>,
        d: &serde_json::Value,
        bot_user_id: &str,
    ) {
        let message_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
        if message_id.is_empty() {
            return;
        }
        let Some(edited_ts) = d.get("edited_timestamp").and_then(|t| t.as_str()) else {
            return;
        };
        let new_content = d.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if new_content.is_empty() {
            return;
        }
        // Author re-checks: same gates the create path applied, evaluated
        // against *current* policy. A payload without an author cannot be
        // attributed and is not recorded.
        let author_id = d
            .get("author")
            .and_then(|a| a.get("id"))
            .and_then(|i| i.as_str())
            .unwrap_or("");
        if author_id.is_empty() || author_id == bot_user_id {
            return;
        }
        if !self.listen_to_bots
            && d.get("author")
                .and_then(|a| a.get("bot"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        {
            return;
        }
        if !self.is_user_allowed(author_id) {
            return;
        }

        let key = format!("discord_{message_id}");
        let Some(existing) = self.archived_entry(archive_mem, &key).await else {
            return;
        };
        let marker_key = format!(" [edited at {edited_ts}:");
        if existing.content.contains(&marker_key) {
            return;
        }
        let marker = format!(" [edited at {edited_ts}: {new_content}]");
        // Edits are remote-controlled and unlimited; bound entry growth by
        // dropping the middle of the history once it gets large — the
        // original text and the latest edit are what the archive is for.
        let mut base = existing.content.clone();
        if base.len() + marker.len() > MAX_ARCHIVE_ENTRY_BYTES
            && let Some(first_marker) = base.find(" [edited at ")
        {
            base.truncate(first_marker);
            base.push_str(" [edit history truncated]");
        }
        let updated = format!("{base}{marker}");
        self.restore_archived_entry(archive_mem, &key, &updated, &existing)
            .await;
    }

    /// Record (or un-record) an inbound reaction event according to
    /// `reaction_scope`. Reactions land in the archive sidecar under a
    /// `discord_reaction_{message}_{user}_{emoji}` key so `discord_search`
    /// finds them; a MESSAGE_REACTION_REMOVE forgets the same key. The bot's
    /// own reactions (ack/failure emoji) echo back as gateway events and are
    /// skipped, as are reactors outside the peer allowlist and events outside
    /// the guild/channel allowlists. Reactions from *other* bots are recorded
    /// (deliberately not gated by `listen_to_bots` — the peer allowlist
    /// already governs who is recorded at all).
    ///
    /// The key uses the custom emoji `id` when present (stable across guild
    /// renames; unicode emoji have no id and key by the glyph). The
    /// human-readable name only appears in the entry content.
    ///
    /// Scope `Own` keys off `message_author_id`, which Discord includes on
    /// MESSAGE_REACTION_ADD only — REMOVE events skip the author gate and
    /// rely on the key existence check `forget` performs anyway: a reaction
    /// that was never recorded can't be un-recorded.
    async fn handle_reaction_event(
        &self,
        event_type: &str,
        d: &serde_json::Value,
        bot_user_id: &str,
    ) {
        use zeroclaw_config::schema::DiscordReactionScope;

        let user_id = d.get("user_id").and_then(|u| u.as_str()).unwrap_or("");
        let message_id = d.get("message_id").and_then(|m| m.as_str()).unwrap_or("");
        let channel_id = d.get("channel_id").and_then(|c| c.as_str()).unwrap_or("");
        if user_id.is_empty() || message_id.is_empty() {
            return;
        }
        // Our own ack/failure reactions arrive back as events — never record them.
        if user_id == bot_user_id {
            return;
        }
        if !self.is_user_allowed(user_id) {
            return;
        }
        if !self.guild_ids.is_empty()
            && let Some(g) = d.get("guild_id").and_then(serde_json::Value::as_str)
            && !self.guild_ids.iter().any(|allowed| allowed == g)
        {
            return;
        }
        if !self.channel_ids.is_empty() {
            let parent_id =
                if !channel_id.is_empty() && !self.channel_ids.iter().any(|c| c == channel_id) {
                    self.thread_parent(&self.http_client(), channel_id).await
                } else {
                    None
                };
            if !channel_passes_filter(&self.channel_ids, channel_id, parent_id.as_deref()) {
                return;
            }
        }

        // Key identity: custom-emoji `id` first — names are mutable guild
        // state (rename/delete between ADD and REMOVE would orphan the
        // entry, and two same-named emoji would collide). Unicode emoji have
        // no id and key by the glyph itself.
        let emoji_key = d.get("emoji").and_then(|e| {
            e.get("id")
                .and_then(|i| i.as_str())
                .or_else(|| e.get("name").and_then(|n| n.as_str()))
        });
        let Some(emoji_key) = emoji_key else {
            // Neither id nor name — nothing meaningful to record or forget.
            return;
        };
        let emoji_display = d
            .get("emoji")
            .and_then(|e| e.get("name").and_then(|n| n.as_str()))
            .unwrap_or(emoji_key);
        let key = format!("discord_reaction_{message_id}_{user_id}_{emoji_key}");

        let Some(ref archive_mem) = self.archive_memory else {
            // Nowhere to record without the archive sidecar; still useful in
            // the trace for live diagnostics.
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "event_type": event_type,
                        "emoji": emoji_display,
                        "message_id": message_id,
                    })),
                "discord reaction event (archive disabled, not recorded)"
            );
            return;
        };

        if event_type == "MESSAGE_REACTION_REMOVE" {
            // A failed forget leaves the same stale entry a missed event
            // would — log it like the store path does.
            if let Err(e) = archive_mem.forget(&key).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error": format!("{e}"),
                            "key": key,
                        })),
                    "failed to forget archived discord reaction"
                );
            }
            return;
        }

        // Scope `Own`: only reactions to the bot's own messages.
        if self.reaction_scope == DiscordReactionScope::Own {
            let message_author = d
                .get("message_author_id")
                .and_then(|a| a.as_str())
                .unwrap_or("");
            if message_author != bot_user_id {
                return;
            }
        }

        let username = d
            .get("member")
            .and_then(|m| m.get("user"))
            .and_then(|u| u.get("username"))
            .and_then(|n| n.as_str())
            .unwrap_or(user_id);
        let is_dm_event = d.get("guild_id").is_none();
        let channel_display = if is_dm_event { "dm" } else { channel_id };
        let ts = chrono::Utc::now().to_rfc3339();
        let content = format!(
            "@{username} reacted {emoji_display} to message {message_id} in #{channel_display} at {ts}"
        );
        let session = if channel_id.is_empty() {
            None
        } else {
            Some(channel_id)
        };
        if let Err(e) = archive_mem
            .store(
                &key,
                &content,
                zeroclaw_memory::MemoryCategory::Custom("discord".to_string()),
                session,
            )
            .await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error": format!("{e}"),
                        "key": key,
                    })),
                "failed to archive discord reaction"
            );
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.discord",
            self.proxy_url.as_deref(),
        )
    }

    /// Check if a Discord user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    fn bot_user_id_from_token(token: &str) -> Option<String> {
        // Discord bot tokens are base64(bot_user_id).timestamp.hmac
        let part = token.split('.').next()?;
        base64_decode(part)
    }

    /// Resolve whether `channel_id` is a Discord thread (ANNOUNCEMENT,
    /// PUBLIC, or PRIVATE thread) via `GET /channels/{id}`. Returns
    /// `Some(parent_id)` when the channel is a thread, `None` otherwise.
    /// Results are cached for the channel instance's lifetime: thread-ness
    /// is stable for a given channel ID, so one lookup per ID per process.
    /// Failures (network, 429, missing fields) return `None` without
    /// caching so the next message retries.
    async fn thread_parent(&self, client: &reqwest::Client, channel_id: &str) -> Option<String> {
        discord_thread_parent(client, &self.bot_token, &self.thread_channels, channel_id).await
    }

    /// Apply the trust-boundary / delivery-failure emoji reactions to the
    /// bot's just-sent message. Best-effort: reaction failures are debug
    /// logged but never propagated. `message_id` being `None` (e.g. when
    /// every chunk failed to post) skips the reaction step entirely.
    async fn apply_failure_reactions(
        &self,
        channel_id: &str,
        message_id: Option<&str>,
        reactions: &[&'static str],
    ) {
        let Some(message_id) = message_id else {
            return;
        };
        for emoji in reactions {
            if let Err(e) = self.add_reaction(channel_id, message_id, emoji).await {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(
                            ::serde_json::json!({"emoji": emoji, "error": format!("{}", e)})
                        ),
                    "failed to add failure reaction to outgoing message"
                );
            }
        }
    }

    /// The legacy plaintext approval prompt: the operator replies
    /// "`<token> yes|no|always`" in the channel, parsed back by
    /// `parse_approval_reply` on the inbound MESSAGE_CREATE path. Used when the
    /// interaction pipe isn't live (buttons would be dead controls).
    async fn send_plaintext_approval(
        &self,
        channel_id: &str,
        token: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<()> {
        let text = format!(
            "APPROVAL REQUIRED [{}]\nTool: {}\nArgs: {}\n\nReply: \"{} yes\", \"{} no\", or \"{} always\"",
            token, request.tool_name, request.arguments_summary, token, token, token,
        );
        self.send(&SendMessage::new(text, channel_id)).await
    }

    /// The buttoned approval prompt: an Allow-once / Session / Always / Deny
    /// action row. Each button is registered in `pending_components` under its
    /// own `custom_id` carrying the server-side [`approval::ApprovalDecision`]
    /// (NOT read from the wire) and the approval `token` (the key into
    /// `pending_approvals`). A click is dispatched by the type-3 arm, which —
    /// after fail-closed `interaction_gate` — `take`s the entry (single-use)
    /// and resolves the `oneshot` with the bound decision.
    ///
    /// The bindings are registered BEFORE the send so a fast click can never
    /// race an absent entry; the token already lives in `pending_approvals` and
    /// the caller drops it on any error.
    async fn send_buttoned_approval(
        &self,
        channel_id: &str,
        token: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<()> {
        let (row, bindings) = approval::build_approval_row(token);
        // Register every button's intent first. Single-use is enforced by the
        // registry's `take`; the per-click `interaction_gate` is enforced by the
        // type-3 dispatch before any `take`.
        {
            let mut reg = self.pending_components.lock();
            for (cid, decision) in &bindings {
                if let Some(wire) = cid.encode() {
                    reg.register(
                        wire,
                        ComponentIntent::Approval {
                            token: token.to_string(),
                            decision: *decision,
                        },
                    );
                }
            }
        }

        let text = format!(
            "APPROVAL REQUIRED\nTool: {}\nArgs: {}",
            request.tool_name, request.arguments_summary,
        );
        let outgoing = DiscordOutgoing::with_components(text, vec![row]);
        let client = self.http_client();
        send_discord_outgoing(&client, &self.bot_token, channel_id, &outgoing)
            .await
            .map(|_id| ())
    }
}

/// Whether a Discord channel type integer identifies a thread.
/// Discord channel types `10` (ANNOUNCEMENT_THREAD), `11` (PUBLIC_THREAD),
/// and `12` (PRIVATE_THREAD) per the Channel Types documentation.
const fn is_thread_channel_type(channel_type: u64) -> bool {
    matches!(channel_type, 10..=12)
}

/// Hard cap on `GET /channels/{id}` while resolving whether an inbound
/// channel is a thread. Discord normally responds in under 200 ms; this
/// is a safety bound so a hung request cannot stall the listener.
const THREAD_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Lift `[EMBED:{json}]` markers out of agent reply text, vet each spec's URLs
/// against the egress trust boundary, and budget the result to Discord's
/// limits. Returns the embed-free text, the wire embeds, the URL rejections
/// (drive 🚫/⚠️ reactions, not the attachment note), and whether structural
/// budgeting dropped anything (drives a ⚠️). Pure — the testable core of the
/// outbound embed pipeline that `send` wires to the HTTP builders.
fn prepare_outgoing_embeds(
    raw_content: &str,
    workspace_dir: Option<&Path>,
) -> (String, Vec<DiscordEmbed>, Vec<DiscordMarkerFailure>, bool) {
    let (content_without_embeds, embed_specs) = parse_embed_markers(raw_content);
    let mut embeds = Vec::new();
    let mut embed_failures = Vec::new();
    for spec in embed_specs {
        let (embed, failures) = spec_to_embed(spec, workspace_dir);
        embed_failures.extend(failures);
        if let Some(embed) = embed {
            embeds.push(embed);
        }
    }
    let truncated = budget_embeds(&mut embeds);
    (content_without_embeds, embeds, embed_failures, truncated)
}

/// Deliver a (deferred) interaction's answer, splitting it across Discord's
/// 2000-char limit: the first chunk edits the @original deferred message (with
/// any `embeds`) and any remaining chunks are posted as followups. The chunking
/// lives here in the wiring layer so `interaction` need not depend on `chunk`
/// (preserving the no-impl-to-impl module boundary).
async fn deliver_interaction_answer(
    client: &reqwest::Client,
    app_id: &str,
    interaction_token: &str,
    api_base: &str,
    content: &str,
    embeds: &[DiscordEmbed],
) -> anyhow::Result<()> {
    let chunks = split_message_for_discord(content);
    let mut chunks = chunks.iter();
    let first = chunks.next().map(String::as_str).unwrap_or("");
    discord_edit_interaction_response(client, app_id, interaction_token, api_base, first, embeds)
        .await?;
    for chunk in chunks {
        discord_post_interaction_followup(client, app_id, interaction_token, api_base, chunk)
            .await?;
    }
    Ok(())
}

/// Pure channel-filter decision: does `msg_channel` pass the allowlist?
///
/// A channel passes when:
/// 1. `channel_filter` is empty (accept all), OR
/// 2. `msg_channel` is directly in `channel_filter`, OR
/// 3. `thread_parent_id` is `Some(parent)` and `parent` is in `channel_filter`
///    (thread whose parent forum/channel is allowed).
fn channel_passes_filter(
    channel_filter: &[String],
    msg_channel: &str,
    thread_parent_id: Option<&str>,
) -> bool {
    if channel_filter.is_empty() {
        return true;
    }
    if channel_filter.iter().any(|c| c == msg_channel) {
        return true;
    }
    if let Some(parent) = thread_parent_id {
        return channel_filter.iter().any(|c| c == parent);
    }
    false
}

/// Process Discord message attachments in a single pass.
///
/// Returns the text block appended to the agent's prompt and the structured
/// `MediaAttachment` list consumed by the media pipeline. Each attachment is
/// downloaded at most once: text/* is inlined as text, audio is transcribed
/// inline when a transcription manager is configured and returns non-empty
/// text (otherwise it falls through to the media pipeline), and
/// image/video/document attachments are saved to the workspace and emitted as
/// `[KIND:<path>]` markers plus a `MediaAttachment` for vision-capable
/// providers.
async fn process_attachments(
    attachments: &[serde_json::Value],
    client: &reqwest::Client,
    workspace_dir: Option<&Path>,
    transcription_manager: Option<&super::transcription::TranscriptionManager>,
) -> (String, Vec<MediaAttachment>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut media: Vec<MediaAttachment> = Vec::new();

    for att in attachments {
        let ct = att
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = att
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        let Some(url) = att.get("url").and_then(|v| v.as_str()) else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"name": name})),
                "attachment has no url, skipping"
            );
            continue;
        };

        if ct.starts_with("text/") {
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(text) = resp.text().await {
                        text_parts.push(format!("[{name}]\n{text}"));
                    }
                }
                Ok(resp) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"name": name, "status": resp.status().to_string()})), "attachment fetch failed");
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"name": name, "error": format!("{}", e)})
                            ),
                        "attachment fetch error"
                    );
                }
            }
            continue;
        }

        let is_audio = is_discord_audio_attachment(ct, name);

        // Audio with channel-level transcription configured: transcribe
        // inline so the agent receives `[Voice] <transcript>` text rather
        // than opaque bytes through the media pipeline.
        let mut downloaded_audio_bytes = None;
        if is_audio && let Some(manager) = transcription_manager {
            let bytes = match download_attachment_bytes(client, url, name).await {
                Some(b) => b,
                None => continue,
            };
            match manager.transcribe(&bytes, name).await {
                Ok(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!(
                                "transcribed audio attachment {} ({} chars)",
                                name,
                                trimmed.len()
                            )
                        );
                        text_parts.push(format!("[Voice] {trimmed}"));
                        continue;
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"name": name, "error": format!("{}", e)})
                            ),
                        "voice transcription failed"
                    );
                }
            }
            downloaded_audio_bytes = Some(bytes);
        }

        let marker_kind = marker_kind_for(ct, is_audio);

        let bytes = match downloaded_audio_bytes {
            Some(b) => b,
            None => match download_attachment_bytes(client, url, name).await {
                Some(b) => b,
                None => continue,
            },
        };

        let marker_target = match workspace_dir {
            Some(dir) => match save_attachment_bytes_to_workspace(dir, name, &bytes).await {
                Ok(local_path) => local_path.display().to_string(),
                Err(e) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"name": name, "kind": marker_kind, "error": format!("{}", e)})), "attachment save failed, falling back to url");
                    url.to_string()
                }
            },
            None => url.to_string(),
        };
        text_parts.push(format!("[{marker_kind}:{marker_target}]"));

        media.push(MediaAttachment {
            file_name: name.to_string(),
            data: bytes,
            mime_type: if ct.is_empty() {
                None
            } else {
                Some(ct.to_string())
            },
        });
    }

    (text_parts.join("\n---\n"), media)
}

/// Download an attachment URL into memory, with structured warn-logging on
/// each failure mode. Returns `None` when the attachment should be skipped.
async fn download_attachment_bytes(
    client: &reqwest::Client,
    url: &str,
    name: &str,
) -> Option<Vec<u8>> {
    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => Some(b.to_vec()),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"name": name, "error": format!("{}", e)})),
                    "failed to read attachment bytes"
                );
                None
            }
        },
        Ok(resp) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"name": name, "status": resp.status().to_string()})
                    ),
                "attachment download failed"
            );
            None
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"name": name, "error": format!("{}", e)})),
                "attachment fetch error"
            );
            None
        }
    }
}

async fn save_attachment_bytes_to_workspace(
    workspace_dir: &Path,
    filename: &str,
    bytes: &[u8],
) -> anyhow::Result<PathBuf> {
    let save_dir = workspace_dir.join("discord_files");
    tokio::fs::create_dir_all(&save_dir).await?;

    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("attachment");
    let local_name = format!("{}_{}", Uuid::new_v4(), safe_name);
    let local_path = save_dir.join(local_name);

    tokio::fs::write(&local_path, bytes).await?;
    Ok(local_path)
}

/// Audio file extensions accepted for voice transcription.
const DISCORD_AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "mpeg", "mpga", "mp4", "m4a", "ogg", "oga", "opus", "wav", "webm",
];

/// Check if a content type or filename indicates an audio file.
fn is_discord_audio_attachment(content_type: &str, filename: &str) -> bool {
    if content_type.starts_with("audio/") {
        return true;
    }
    if let Some(ext) = filename.rsplit('.').next() {
        return DISCORD_AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str());
    }
    false
}

/// Map a Discord attachment's content type plus audio-detection result to
/// the canonical outbound marker kind. Pulled out of `process_attachments`
/// so the MIME-to-marker dispatch can be unit-tested without a live HTTP
/// download.
fn marker_kind_for(content_type: &str, is_audio: bool) -> &'static str {
    if content_type.starts_with("image/") {
        "IMAGE"
    } else if is_audio {
        "AUDIO"
    } else if content_type.starts_with("video/") {
        "VIDEO"
    } else {
        "DOCUMENT"
    }
}

/// Why a slash-command interaction was refused. Drives the WARN log and the
/// ephemeral rejection text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractionDenial {
    UnauthorizedUser,
    GuildNotAllowed,
    ChannelNotAllowed,
}

/// Slash-command authorization: the same gates MESSAGE_CREATE applies to
/// inbound messages, applied to the interaction's invoker and origin. A
/// globally-registered command is visible to every guild member in every
/// guild the bot was added to — visibility is Discord's, authorization is
/// ours.
fn interaction_gate(
    peers: &[String],
    guild_filter: &[String],
    channel_filter: &[String],
    user_id: &str,
    guild_id: Option<&str>,
    channel_id: &str,
    thread_parent: Option<&str>,
) -> Result<(), InteractionDenial> {
    if !crate::allowlist::is_user_allowed(peers, user_id, crate::allowlist::Match::Sensitive) {
        return Err(InteractionDenial::UnauthorizedUser);
    }
    if !guild_filter.is_empty()
        && let Some(g) = guild_id
        && !guild_filter.iter().any(|allowed| allowed == g)
    {
        return Err(InteractionDenial::GuildNotAllowed);
    }
    if !channel_filter.is_empty()
        && !channel_passes_filter(channel_filter, channel_id, thread_parent)
    {
        return Err(InteractionDenial::ChannelNotAllowed);
    }
    Ok(())
}

const BASE64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Upper bound for an archived message entry once edit markers accrue.
/// Edits are remote-controlled and unlimited; past this size the middle of
/// the edit history is dropped (original text and latest edit retained).
const MAX_ARCHIVE_ENTRY_BYTES: usize = 16 * 1024;
const DISCORD_ACK_REACTIONS: &[&str] = &["⚡️", "🦀", "🙌", "💪", "👌", "👀", "👣"];

fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            #[allow(clippy::cast_possible_truncation)]
            return (value % upper) as usize;
        }
    }
}

fn random_discord_ack_reaction() -> &'static str {
    DISCORD_ACK_REACTIONS[pick_uniform_index(DISCORD_ACK_REACTIONS.len())]
}

fn mention_tags(bot_user_id: &str) -> [String; 2] {
    [format!("<@{bot_user_id}>"), format!("<@!{bot_user_id}>")]
}

fn contains_bot_mention(content: &str, bot_user_id: &str) -> bool {
    let tags = mention_tags(bot_user_id);
    content.contains(&tags[0]) || content.contains(&tags[1])
}

/// Whether a Discord message `type` represents a real user turn the bot should
/// act on, versus a system/auto message it must ignore.
///
/// Only `DEFAULT` (0) and `REPLY` (19) are conversational. Everything else is a
/// system message: notably `THREAD_CREATED` (18) — posted in the parent channel
/// when a thread is created — and `THREAD_STARTER_MESSAGE` (21), plus joins,
/// pins, boosts, etc. Acting on `THREAD_CREATED` is what made the bot "respond
/// to a thread's birth message".
fn is_conversational_message_type(message_type: u64) -> bool {
    matches!(message_type, 0 | 19)
}

/// Decide whether an inbound Discord message passes the listener gate.
/// Returns the cleaned text body when admitted, or `None` to drop the
/// message. Attachment-only messages (empty `content` plus at least one
/// attachment) are admitted as long as the mention requirement is
/// satisfied; otherwise a Discord message that contained only an image,
/// PDF, ZIP, video, or audio with no caption would never reach the
/// media pipeline.
fn admit_discord_message(
    content: &str,
    has_attachments: bool,
    mention_only: bool,
    bot_user_id: &str,
) -> Option<String> {
    if mention_only && !contains_bot_mention(content, bot_user_id) {
        return None;
    }

    let normalized = content.trim().to_string();
    if normalized.is_empty() && !has_attachments {
        return None;
    }

    Some(normalized)
}

/// Minimal base64 decode (no extra dep) — only needs to decode the user ID portion
#[allow(clippy::cast_possible_truncation)]
fn base64_decode(input: &str) -> Option<String> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let mut bytes = Vec::new();
    let chars: Vec<u8> = padded.bytes().collect();

    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let mut v = [0usize; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                v[i] = 0;
            } else {
                v[i] = BASE64_ALPHABET.iter().position(|&a| a == b)?;
            }
        }

        bytes.push(((v[0] << 2) | (v[1] >> 4)) as u8);
        if chunk[2] != b'=' {
            bytes.push((((v[1] & 0xF) << 4) | (v[2] >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push((((v[2] & 0x3) << 6) | v[3]) as u8);
        }
    }

    String::from_utf8(bytes).ok()
}

/// Free-function form of the thread-parent lookup so spawned tasks (which
/// cannot borrow the channel) share the same cache and semantics.
async fn discord_thread_parent(
    client: &reqwest::Client,
    bot_token: &str,
    thread_channels: &Arc<AsyncMutex<HashMap<String, Option<String>>>>,
    channel_id: &str,
) -> Option<String> {
    {
        let cache = thread_channels.lock().await;
        if let Some(value) = cache.get(channel_id) {
            return value.clone();
        }
    }

    // Only a successful API response is cached. A transient network blip
    // or 429 must not poison the cache for the channel's lifetime; the
    // next message should retry the lookup. Failure paths return `None`
    // (the safe default) without writing to the cache. The whole request
    // is wrapped in an explicit timeout so a hung Discord API call can
    // never stall the listener; the shared channel HTTP client may not
    // carry a request-level timeout.
    let url = format!("https://discord.com/api/v10/channels/{channel_id}");
    let lookup = async {
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bot {bot_token}"))
            .send()
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "request failed"
                );
                anyhow::Error::msg(format!("request failed: {e}"))
            })?;
        if !resp.status().is_success() {
            anyhow::bail!("non-success status {}", resp.status());
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "body parse failed"
            );
            anyhow::Error::msg(format!("body parse failed: {e}"))
        })?;
        let is_thread = body
            .get("type")
            .and_then(serde_json::Value::as_u64)
            .map(is_thread_channel_type)
            .unwrap_or(false);
        Ok::<Option<String>, anyhow::Error>(if is_thread {
            body.get("parent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        } else {
            None
        })
    };
    let result = match tokio::time::timeout(THREAD_LOOKUP_TIMEOUT, lookup).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"channel_id": channel_id, "error": format!("{}", e)})
                    ),
                "channel lookup failed"
            );
            return None;
        }
        Err(_) => {
            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel_id": channel_id, "timeout_secs": THREAD_LOOKUP_TIMEOUT.as_secs()})), "channel lookup timed out");
            return None;
        }
    };

    thread_channels
        .lock()
        .await
        .insert(channel_id.to_string(), result.clone());
    result
}

// Discord gateway intent bits (API v10) — the ones zeroclaw consumes or
// exposes as opt-ins. https://discord.com/developers/docs/events/gateway#gateway-intents
const INTENT_GUILDS: u64 = 1 << 0;
const INTENT_GUILD_MEMBERS: u64 = 1 << 1; // privileged
const INTENT_GUILD_PRESENCES: u64 = 1 << 8; // privileged
const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
const INTENT_GUILD_MESSAGE_REACTIONS: u64 = 1 << 10;
const INTENT_DIRECT_MESSAGES: u64 = 1 << 12;
const INTENT_DIRECT_MESSAGE_REACTIONS: u64 = 1 << 13;
const INTENT_MESSAGE_CONTENT: u64 = 1 << 15; // privileged

/// The intents every Discord channel needs: guild topology plus guild/DM
/// messages with content. MESSAGE_CONTENT is privileged but always requested
/// — without it the bot only sees text in DMs and @-mentions, which silently
/// breaks `archive`, `listen_to_bots`, and mention-free channels.
const BASELINE_INTENTS: u64 =
    INTENT_GUILDS | INTENT_GUILD_MESSAGES | INTENT_DIRECT_MESSAGES | INTENT_MESSAGE_CONTENT;

/// Human-readable names for a resolved intent mask, for connect logs and
/// close-code diagnostics. Bits without a known name (possible via the raw
/// `intents_mask` override) are reported as one hex remainder entry rather
/// than silently dropped.
fn intent_names(mask: u64) -> Vec<String> {
    let known: [(u64, &str); 8] = [
        (INTENT_GUILDS, "guilds"),
        (INTENT_GUILD_MEMBERS, "guild_members"),
        (INTENT_GUILD_PRESENCES, "guild_presences"),
        (INTENT_GUILD_MESSAGES, "guild_messages"),
        (INTENT_GUILD_MESSAGE_REACTIONS, "guild_message_reactions"),
        (INTENT_DIRECT_MESSAGES, "direct_messages"),
        (INTENT_DIRECT_MESSAGE_REACTIONS, "direct_message_reactions"),
        (INTENT_MESSAGE_CONTENT, "message_content"),
    ];
    let mut names = Vec::new();
    let mut rest = mask;
    for (bit, name) in known {
        if mask & bit != 0 {
            names.push(name.to_string());
            rest &= !bit;
        }
    }
    if rest != 0 {
        names.push(format!("unknown({rest:#x})"));
    }
    names
}

/// Close 4014 means the Developer Portal doesn't grant a privileged intent
/// we requested. Name the portal toggles so the operator knows exactly what
/// to fix instead of staring at a bare close code.
fn disallowed_intents_hint(mask: u64) -> String {
    let mut requested = Vec::new();
    if mask & INTENT_MESSAGE_CONTENT != 0 {
        requested.push("Message Content".to_string());
    }
    if mask & INTENT_GUILD_MEMBERS != 0 {
        requested.push("Server Members".to_string());
    }
    if mask & INTENT_GUILD_PRESENCES != 0 {
        requested.push("Presence".to_string());
    }
    if requested.is_empty() {
        // Reachable only via an `intents_mask` override with no privileged
        // bits — name the raw mask so the operator has something to act on.
        requested.push(format!("mask {mask:#x}"));
    }
    format!(
        " — a privileged intent is not enabled for this bot in the Discord \
         Developer Portal (Bot → Privileged Gateway Intents). Requested: {}",
        requested.join(", ")
    )
}

fn is_fatal_gateway_close_code(code: u16) -> bool {
    matches!(code, 4004 | 4010 | 4011 | 4012 | 4013 | 4014)
}

fn requires_new_session_close_code(code: u16) -> bool {
    matches!(code, 4007 | 4009)
}

impl ::zeroclaw_api::attribution::Attributable for DiscordChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Discord,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    /// Discord bot tokens encode the bot's user ID in the first
    /// segment (`base64(user_id).timestamp.hmac`); decode on demand
    /// rather than caching since the result is deterministic and the
    /// orchestrator only calls `self_handle` on the inbound path.
    /// Returning the user ID engages the SDK self-loop guard against
    /// gateway events the bot itself produced (typing indicators,
    /// echoed message events from intent overlap, etc.).
    fn self_handle(&self) -> Option<String> {
        Self::bot_user_id_from_token(&self.bot_token)
    }

    /// Discord renders user mentions as `<@SNOWFLAKE>` (or
    /// `<@!SNOWFLAKE>` with the legacy nickname prefix, which the API
    /// normalizes to the bare form on inbound). Returns the bot's
    /// snowflake wrapped in that exact form so the agent matches its
    /// own mention without parsing the angle brackets itself.
    fn self_addressed_mention(&self) -> Option<String> {
        self.self_handle().map(|id| format!("<@{id}>"))
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Slash-command replies: the recipient carries an
        // `interaction:{interaction_id}` sentinel. Resolve the credentials
        // from the channel-local store (never from the reply target — the
        // token is a live credential) and answer by editing the deferred
        // interaction response instead of posting a channel message.
        if let Some(interaction_id) = parse_discord_interaction_target(&message.recipient) {
            let pending = {
                let guard = self.pending_interactions.lock();
                guard.get(interaction_id).cloned()
            };
            let Some(pending) = pending else {
                anyhow::bail!("interaction reply target unknown or expired (id {interaction_id})");
            };
            if pending.created.elapsed() > INTERACTION_TOKEN_TTL {
                anyhow::bail!("interaction followup token expired (id {interaction_id}, >15min)");
            }
            // Render embeds on slash-command replies too: lift `[EMBED:…]` out
            // and attach it to the @original edit, the same as a normal send.
            // (Bad-URL / truncation reactions aren't surfaced on an interaction
            // @original edit — consistent with how it doesn't surface attachment
            // failures either; the embed still renders without the bad field.)
            let raw = crate::util::strip_tool_call_tags(&message.content);
            let (content, embeds, _embed_failures, _embeds_truncated) =
                prepare_outgoing_embeds(&raw, self.workspace_dir.as_deref());
            let client = self.http_client();
            return deliver_interaction_answer(
                &client,
                &pending.app_id,
                &pending.token,
                DISCORD_API_BASE,
                &content,
                &embeds,
            )
            .await;
        }

        let raw_content = crate::util::strip_tool_call_tags(&message.content);

        // Embeds first: their `[EMBED:{json}]` payload can itself contain `[`/`]`,
        // so they must be lifted out before the media-marker scan runs on the rest.
        let (content_without_embeds, embeds, embed_failures, embeds_truncated) =
            prepare_outgoing_embeds(&raw_content, self.workspace_dir.as_deref());

        let (cleaned_content, parsed_attachments) =
            parse_attachment_markers(&content_without_embeds);
        let (mut local_files, remote_urls, failures) =
            classify_outgoing_attachments(&parsed_attachments, self.workspace_dir.as_deref());

        // Discord accepts max 10 files per message.
        if local_files.len() > 10 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"count": local_files.len()})),
                "truncating local attachment upload list to 10 files"
            );
            local_files.truncate(10);
        }

        let body = with_inline_attachment_urls(&cleaned_content, &remote_urls);
        // The delivery-failure note counts dropped *attachments* only. Embed URL
        // rejections and structural truncation surface as reactions, not a note.
        let note = delivery_failure_note(&failures);
        let content = compose_body_with_failure_note(&body, note.as_deref());

        let mut reaction_failures = failures.clone();
        reaction_failures.extend(embed_failures.iter().copied());
        let mut reactions = decide_failure_reactions(&reaction_failures);
        if embeds_truncated && !reactions.contains(&"⚠️") {
            reactions.push("⚠️");
        }

        let client = self.http_client();
        let chunks = chunks_for_send(
            &content,
            self.stream_mode,
            DISCORD_MAX_MESSAGE_LENGTH,
            // Force at least one message even when the text is empty, so an
            // embeds-only (or files-only) reply still has a first message to
            // carry them.
            !local_files.is_empty() || !embeds.is_empty(),
        );
        let inter_chunk_delay_ms =
            if self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage {
                self.multi_message_delay_ms
            } else {
                500
            };

        let mut first_message_id: Option<String> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            // Embeds ride the first message only (same placement as attachments).
            let message_id = if i == 0 {
                let payload = DiscordOutgoing {
                    content: Some(chunk.clone()),
                    embeds: embeds.clone(),
                    ..Default::default()
                };
                if local_files.is_empty() {
                    send_discord_message_payload(
                        &client,
                        &self.bot_token,
                        &message.recipient,
                        &payload,
                    )
                    .await?
                } else {
                    send_discord_message_payload_with_files(
                        &client,
                        &self.bot_token,
                        &message.recipient,
                        &payload,
                        &local_files,
                    )
                    .await?
                }
            } else {
                send_discord_message_json(&client, &self.bot_token, &message.recipient, chunk)
                    .await?
            };
            if first_message_id.is_none() {
                first_message_id = Some(message_id);
            }

            if i < chunks.len() - 1 {
                if message
                    .cancellation_token
                    .as_ref()
                    .is_some_and(|t| t.is_cancelled())
                {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "Discord delivery interrupted after chunk {}/{}",
                            i + 1,
                            chunks.len()
                        )
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(inter_chunk_delay_ms)).await;
            }
        }

        self.apply_failure_reactions(&message.recipient, first_message_id.as_deref(), &reactions)
            .await;

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = Self::bot_user_id_from_token(&self.bot_token).unwrap_or_default();
        let mut had_ready = false;

        // Get Gateway URL
        let gw_resp = self
            .http_client()
            .get("https://discord.com/api/v10/gateway/bot")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?;
        let gw_resp = Self::validate_gateway_preflight_response(gw_resp)?;
        let gw_resp: serde_json::Value = gw_resp.json().await?;

        if let Some(remaining) = gw_resp
            .get("session_start_limit")
            .and_then(|v| v.get("remaining"))
            .and_then(serde_json::Value::as_u64)
            && remaining == 0
        {
            return Err(Self::fatal_listener_error(
                "discord gateway identify blocked: session_start_limit.remaining is 0",
            ));
        }

        let fresh_gateway_url = gw_resp
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| Self::fatal_listener_error("discord gateway preflight missing url"))?
            .to_string();
        let session_snapshot = self.gateway_session.lock().clone();
        let can_resume =
            session_snapshot.session_id.is_some() && session_snapshot.sequence.is_some();
        let gw_url = if can_resume {
            session_snapshot
                .resume_gateway_url
                .clone()
                .unwrap_or_else(|| fresh_gateway_url.clone())
        } else {
            fresh_gateway_url.clone()
        };

        let ws_url = format!("{gw_url}/?v=10&encoding=json");
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"resume": can_resume, "gateway_url": gw_url})),
            "connecting to gateway..."
        );

        let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
            &ws_url,
            "channel.discord",
            self.proxy_url.as_deref(),
        )
        .await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read.next().await.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"phase": "gateway_hello"})),
                "discord: gateway closed before Hello"
            );
            anyhow::Error::msg("No hello")
        })??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        let mut sequence = session_snapshot.sequence.unwrap_or(-1);

        if can_resume {
            let resume = json!({
                "op": 6,
                "d": {
                    "token": self.bot_token,
                    "session_id": session_snapshot.session_id,
                    "seq": session_snapshot.sequence,
                }
            });
            write.send(Message::Text(resume.to_string().into())).await?;
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sequence": sequence})),
                "sent Discord Resume"
            );
        } else {
            let intents = self.gateway_intents();
            if intents & BASELINE_INTENTS != BASELINE_INTENTS {
                // Only reachable via the raw `intents_mask` override — the
                // derived path always starts from the full baseline.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "intents": intents,
                            "missing": intent_names(BASELINE_INTENTS & !intents),
                        })),
                    "intents_mask drops baseline intents — message handling may go quiet"
                );
            }
            let identify = json!({
                "op": 2,
                "d": {
                    "token": self.bot_token,
                    "intents": intents,
                    "properties": {
                        "os": "linux",
                        "browser": "zeroclaw",
                        "device": "zeroclaw"
                    }
                }
            });
            write
                .send(Message::Text(identify.to_string().into()))
                .await?;
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "intents": intents,
                        "intent_names": intent_names(intents),
                    })),
                "sent Discord Identify"
            );
        }

        // Spawn heartbeat timer — sends a tick signal, actual heartbeat
        // is assembled in the select! loop where `sequence` lives.
        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        let hb_interval = heartbeat_interval;
        zeroclaw_spawn::spawn!(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(hb_interval));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        let guild_filter = self.guild_ids.clone();
        let channel_filter = self.channel_ids.clone();
        let archive_memory = self.archive_memory.clone();

        // --- Stall watchdog --------------------------------------------------
        let watchdog = if self.stall_timeout_secs > 0 {
            Some(zeroclaw_infra::stall_watchdog::StallWatchdog::new(
                self.stall_timeout_secs,
            ))
        } else {
            None
        };

        let (stall_tx, mut stall_rx) = tokio::sync::mpsc::channel::<()>(1);
        if let Some(ref wd) = watchdog {
            let stall_signal = stall_tx.clone();
            wd.start(move || {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "stall watchdog fired — no events for configured timeout, triggering reconnect"
                );
                let _ = stall_signal.try_send(());
            })
            .await;
        }
        // Keep stall_tx alive so the receiver doesn't close prematurely when
        // the watchdog is disabled (recv will just pend forever).
        let _stall_tx_guard = stall_tx;

        loop {
            tokio::select! {
                _ = stall_rx.recv() => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "breaking listen loop due to stall watchdog");
                    break;
                }
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Ping(payload))) => {
                            if write.send(Message::Pong(payload)).await.is_err() {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "pong send failed, reconnecting");
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(frame))) => {
                            if let Some(frame) = frame {
                                let code = u16::from(frame.code);
                                let reason = frame.reason.to_string();
                                if requires_new_session_close_code(code) {
                                    let mut session = self.gateway_session.lock();
                                    session.session_id = None;
                                    session.resume_gateway_url = None;
                                    session.sequence = None;
                                }
                                if is_fatal_gateway_close_code(code) {
                                    let mut message = format!(
                                        "discord gateway closed with fatal code {code}: {reason}"
                                    );
                                    if code == 4014 {
                                        message.push_str(&disallowed_intents_hint(
                                            self.gateway_intents(),
                                        ));
                                    }
                                    return Err(Self::fatal_listener_error(message));
                                }
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"code": code, "reason": reason, "had_ready": had_ready, "sequence": sequence})), "discord gateway closed; reconnecting");
                            }
                            break;
                        }
                        None => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"had_ready": had_ready, "sequence": sequence})), "discord gateway stream ended; reconnecting");
                            break;
                        }
                        Some(Err(e)) => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": format!("{}", e), "had_ready": had_ready, "sequence": sequence})), "websocket read error, reconnecting");
                            break;
                        }
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Mark activity for the stall watchdog on every
                    // successfully parsed gateway event.
                    if let Some(ref wd) = watchdog {
                        wd.touch();
                    }

                    // Track sequence number from all dispatch events
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                        self.gateway_session.lock().sequence = Some(s);
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);
                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");

                    match event_type {
                        "READY" => {
                            had_ready = true;
                            let session_id = event
                                .get("d")
                                .and_then(|d| d.get("session_id"))
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string);
                            let resume_gateway_url = event
                                .get("d")
                                .and_then(|d| d.get("resume_gateway_url"))
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string);
                            {
                                let mut session = self.gateway_session.lock();
                                session.session_id = session_id.clone();
                                session.resume_gateway_url = resume_gateway_url;
                                session.sequence = if sequence >= 0 { Some(sequence) } else { None };
                            }
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                                    ::serde_json::json!({"sequence": sequence, "session_id_present": session_id.is_some()})
                                ),
                                "discord READY received"
                            );
                            // Slash commands: register `/ask` once on READY.
                            // The application id is carried in the READY payload
                            // (`d.application.id`), so no extra REST call is needed.
                            // Spawned so registration never blocks the heartbeat.
                            if self.slash_commands {
                                let app_id = event
                                    .get("d")
                                    .and_then(|d| d.get("application"))
                                    .and_then(|a| a.get("id"))
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);
                                if let Some(app_id) = app_id {
                                    // Resolve + reconcile entirely in a
                                    // spawned task: the skills loader does
                                    // blocking file IO (spawn_blocking) and
                                    // the reconcile is several REST calls —
                                    // none of it may run on the listen loop.
                                    let client = self.http_client();
                                    let bot_token = self.bot_token.clone();
                                    let resolver = self.slash_command_resolver.clone();
                                    let workspace_dir = self.workspace_dir.clone();
                                    zeroclaw_spawn::spawn!(async move {
                                        let specs = match resolver {
                                            Some(resolve) => {
                                                match tokio::task::spawn_blocking(move || resolve()).await {
                                                    Ok(specs) => specs,
                                                    Err(e) => {
                                                        // A resolver panic must not be
                                                        // mistaken for "no skills" — that
                                                        // would reconcile every skill
                                                        // command away and commit it as
                                                        // success. Skip; next READY retries.
                                                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "skills resolver panicked; skipping slash command reconcile");
                                                        return;
                                                    }
                                                }
                                            }
                                            None => Vec::new(),
                                        };
                                        let body = slash_command_registration_body(&specs);
                                        let fingerprint = {
                                            use std::hash::{Hash, Hasher};
                                            let mut h = std::collections::hash_map::DefaultHasher::new();
                                            body.to_string().hash(&mut h);
                                            h.finish()
                                        };
                                        use crate::discord_slash_state::SlashReconcileState;
                                        let now = crate::discord_slash_state::now_unix();
                                        let state =
                                            SlashReconcileState::load(workspace_dir.as_deref(), &app_id);
                                        // Honour a persisted rate-limit cooldown across restarts: a
                                        // 429'd reconcile must not re-hammer Discord on the next READY.
                                        if state.rate_limited(now) {
                                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"retry_after_until": state.retry_after_until})), "discord slash command reconcile in rate-limit cooldown; skipping");
                                            return;
                                        }
                                        // Skip only when the set matches the last *successful*
                                        // reconcile. The fingerprint is persisted, so an unchanged
                                        // set is skipped after a restart too (no daily-budget churn).
                                        if state.fingerprint == Some(fingerprint) {
                                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"commands": specs.len() + 1})), "discord slash command set unchanged; skipping re-registration");
                                            return;
                                        }
                                        match reconcile_slash_commands(&client, &bot_token, &app_id, &body, DISCORD_API_BASE).await {
                                            Ok(ReconcileOutcome::Reconciled) => {
                                                SlashReconcileState::record_success(workspace_dir.as_deref(), &app_id, fingerprint, now);
                                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"commands": specs.len() + 1})), "discord slash commands registered");
                                            }
                                            Ok(ReconcileOutcome::RateLimited { until }) => {
                                                // Persist the cooldown (keeping the prior fingerprint)
                                                // so the next READY/restart waits it out.
                                                SlashReconcileState::record_retry_after(workspace_dir.as_deref(), &app_id, &state, until);
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"retry_after_until": until})), "discord slash command reconcile rate-limited; cooldown persisted");
                                            }
                                            Err(e) => {
                                                // Hard failure: leave persisted state untouched. The
                                                // new fingerprint differs from the stored one, so the
                                                // next READY retries without a forced reset.
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord slash command registration failed");
                                            }
                                        }
                                    });
                                } else {
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "slash_commands enabled but READY had no application.id");
                                }
                            }
                            continue;
                        }
                        "RESUMED" => {
                            had_ready = true;
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                                    ::serde_json::json!({"sequence": sequence})
                                ),
                                "discord RESUMED received"
                            );
                            continue;
                        }
                        _ => {}
                    }

                    match op {
                        // Op 1: Server requests an immediate heartbeat
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        // Op 7: Reconnect
                        7 => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"had_ready": had_ready, "sequence": sequence})), "received Reconnect (op 7), closing for restart");
                            break;
                        }
                        // Op 9: Invalid Session
                        9 => {
                            let resumable = event.get("d").and_then(serde_json::Value::as_bool).unwrap_or(false);
                            if !resumable {
                                let mut session = self.gateway_session.lock();
                                session.session_id = None;
                                session.resume_gateway_url = None;
                                session.sequence = None;
                            }
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"resumable": resumable, "had_ready": had_ready, "sequence": sequence})), "received Invalid Session (op 9), closing for restart");
                            break;
                        }
                        _ => {}
                    }

                    // Slash commands arrive as INTERACTION_CREATE over this
                    // same gateway. The entire handling sequence — thread
                    // lookup, authorization gate, ephemeral reject or type-5
                    // defer, then enqueue — runs in one spawned task so no
                    // REST call can starve the heartbeat, and the enqueue
                    // happens only after a successful defer: an agent
                    // completion whose followup PATCH is doomed never starts.
                    if self.slash_commands && event_type == "INTERACTION_CREATE" {
                        if let Some(d) = event.get("d") {
                            let itype = d.get("type").and_then(serde_json::Value::as_u64).unwrap_or(0);
                            // type 2 = APPLICATION_COMMAND
                            if itype == 2 {
                                let interaction_id = d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let interaction_token = d.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let app_id = d.get("application_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let command = d.get("data").and_then(|x| x.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                                // user is under `member.user` (guild) or `user` (DM)
                                let user_id = d
                                    .get("member")
                                    .and_then(|m| m.get("user"))
                                    .or_else(|| d.get("user"))
                                    .and_then(|u| u.get("id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                // `/ask` carries a `prompt` option; skill
                                // commands carry `input`. Extract both —
                                // routing happens in the spawned task.
                                let prompt = interaction_string_option(d, "prompt");
                                let input = interaction_string_option(d, "input");
                                // Extract typed-option values here (owned) so the
                                // spawned 'static task doesn't borrow `event`.
                                let submitted = slash_options::extract_submitted_options(d);
                                let interaction_guild = d
                                    .get("guild_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);
                                let interaction_channel = d
                                    .get("channel_id")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                                    .to_string();

                                // Without id/token/app there is nothing we
                                // can even acknowledge.
                                if !interaction_id.is_empty()
                                    && !interaction_token.is_empty()
                                    && !app_id.is_empty()
                                {
                                    let client = self.http_client();
                                    let bot_token = self.bot_token.clone();
                                    let peers = (self.peer_resolver)();
                                    let guild_filter = guild_filter.clone();
                                    let channel_filter = channel_filter.clone();
                                    let thread_channels = Arc::clone(&self.thread_channels);
                                    let pending = Arc::clone(&self.pending_interactions);
                                    let alias = self.alias.clone();
                                    let tx = tx.clone();
                                    let resolver = self.slash_command_resolver.clone();

                                    zeroclaw_spawn::spawn!(async move {
                                        // /ask with no prompt: answer
                                        // ephemerally instead of leaving
                                        // Discord's "did not respond" timeout.
                                        // (Skill commands are validated after
                                        // the defer — the skill set can't be
                                        // resolved inside the 3s window.)
                                        if command == "ask" && prompt.is_empty() {
                                            let msg = i18n::get_required_cli_string(
                                                "channel-discord-interaction-malformed",
                                            );
                                            if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction reject failed");
                                            }
                                            return;
                                        }

                                        // Authorization: same gates as
                                        // MESSAGE_CREATE. Global commands are
                                        // visible to the whole guild; only
                                        // configured peers in allowed
                                        // guilds/channels may invoke.
                                        // Cheap peer check first: an
                                        // unauthorized invoker must not be
                                        // able to trigger the authenticated
                                        // thread-lookup REST call (parity
                                        // with MESSAGE_CREATE's ordering).
                                        // interaction_gate re-checks below.
                                        if !crate::allowlist::is_user_allowed(
                                            &peers,
                                            &user_id,
                                            crate::allowlist::Match::Sensitive,
                                        ) {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "denial": "UnauthorizedUser"})), "rejecting unauthorized slash command interaction");
                                            let msg = i18n::get_required_cli_string(
                                                "channel-discord-interaction-unauthorized",
                                            );
                                            if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction reject failed");
                                            }
                                            return;
                                        }
                                        let parent_id = if !channel_filter.is_empty()
                                            && !interaction_channel.is_empty()
                                            && !channel_filter.iter().any(|c| c == &interaction_channel)
                                        {
                                            discord_thread_parent(
                                                &client,
                                                &bot_token,
                                                &thread_channels,
                                                &interaction_channel,
                                            )
                                            .await
                                        } else {
                                            None
                                        };
                                        if let Err(denial) = interaction_gate(
                                            &peers,
                                            &guild_filter,
                                            &channel_filter,
                                            &user_id,
                                            interaction_guild.as_deref(),
                                            &interaction_channel,
                                            parent_id.as_deref(),
                                        ) {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "denial": format!("{denial:?}")})), "rejecting unauthorized slash command interaction");
                                            let msg = i18n::get_required_cli_string(
                                                "channel-discord-interaction-unauthorized",
                                            );
                                            if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction reject failed");
                                            }
                                            return;
                                        }

                                        // Stash credentials before the defer so
                                        // a fast reply can never race an absent
                                        // entry; sweep expired entries while
                                        // holding the lock anyway.
                                        {
                                            let mut guard = pending.lock();
                                            guard.retain(|_, p| {
                                                p.created.elapsed() < INTERACTION_TOKEN_TTL
                                            });
                                            guard.insert(
                                                interaction_id.clone(),
                                                PendingInteraction {
                                                    app_id: app_id.clone(),
                                                    token: interaction_token.clone(),
                                                    created: std::time::Instant::now(),
                                                },
                                            );
                                        }
                                        // Ack within the 3s window; only a
                                        // successful defer earns an enqueue.
                                        if let Err(e) = discord_defer_interaction(&client, &interaction_id, &interaction_token).await {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction defer failed");
                                            pending.lock().remove(&interaction_id);
                                            return;
                                        }

                                        // Route to agent-bound content. /ask
                                        // passes its prompt verbatim; a skill
                                        // command resolves the live skill set
                                        // (blocking IO — spawn_blocking) and
                                        // wraps its input in a prompt that
                                        // addresses the skill by name. The
                                        // skill is already in the owning
                                        // agent's system prompt and tool set.
                                        let content = if command == "ask" {
                                            Some(prompt)
                                        } else {
                                            let specs = match resolver {
                                                Some(resolve) => {
                                                    match tokio::task::spawn_blocking(move || resolve()).await {
                                                        Ok(specs) => specs,
                                                        Err(e) => {
                                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "skills resolver panicked; treating command as unavailable");
                                                            Vec::new()
                                                        }
                                                    }
                                                }
                                                None => Vec::new(),
                                            };
                                            match specs.into_iter().find(|spec| spec.slug == command) {
                                                Some(spec) => {
                                                    skill_command_prompt(&spec, &input, &submitted)
                                                }
                                                None => None, // stale or foreign command
                                            }
                                        };
                                        let Some(content) = content else {
                                            // Already deferred — clear the
                                            // "thinking…" state with a visible
                                            // explanation instead of letting
                                            // it hang, then drop the creds.
                                            let msg = i18n::get_required_cli_string(
                                                "channel-discord-interaction-unavailable",
                                            );
                                            if let Err(e) = discord_edit_interaction_response(&client, &app_id, &interaction_token, DISCORD_API_BASE, &msg, &[]).await {
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction unavailable-notice failed");
                                            }
                                            pending.lock().remove(&interaction_id);
                                            return;
                                        };

                                        let channel_msg = ChannelMessage {
                                            id: format!("discord_interaction_{interaction_id}"),
                                            sender: user_id,
                                            reply_target: discord_interaction_reply_target(&interaction_id),
                                            content,
                                            channel: "discord".to_string(),
                                            channel_alias: Some(alias),
                                            timestamp: std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs(),
                                            interruption_scope_id: None,
                                            thread_ts: None,
                                            attachments: Vec::new(),
                                            subject: None,
                                        };
                                        if tx.send(channel_msg).await.is_err() {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "orchestrator channel closed; dropping interaction prompt");
                                        }
                                    });
                                }
                            } else if itype == 3 || itype == 5 {
                                // type 3 = MESSAGE_COMPONENT (button / select click);
                                // type 5 = MODAL_SUBMIT. Both echo back a `zc1`
                                // custom_id and share the whole lifecycle (authz →
                                // single-use take → defer → resolve-into-turn); the
                                // modal additionally carries submitted field values
                                // that are appended to the enqueued prompt.
                                let interaction_id = d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let interaction_token = d.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let app_id = d.get("application_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let custom_id_raw = d
                                    .get("data")
                                    .and_then(|x| x.get("custom_id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                // Modal submits carry their typed-in field values;
                                // a component click carries none.
                                let modal_fields = if itype == 5 {
                                    components::extract_modal_fields(d)
                                } else {
                                    Vec::new()
                                };
                                let user_id = d
                                    .get("member")
                                    .and_then(|m| m.get("user"))
                                    .or_else(|| d.get("user"))
                                    .and_then(|u| u.get("id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let interaction_guild = d
                                    .get("guild_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);
                                let interaction_channel = d
                                    .get("channel_id")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                                    .to_string();

                                // Foreign or malformed component: not our `zc1`
                                // scheme, so another app may own it — drop
                                // silently rather than acking someone else's
                                // button. The pending registry is the real gate
                                // below; this is a cheap pre-filter.
                                if custom_id::CustomId::parse(&custom_id_raw).is_none() {
                                    continue;
                                }
                                if !interaction_id.is_empty()
                                    && !interaction_token.is_empty()
                                    && !app_id.is_empty()
                                {
                                    let client = self.http_client();
                                    let bot_token = self.bot_token.clone();
                                    let peers = (self.peer_resolver)();
                                    let guild_filter = guild_filter.clone();
                                    let channel_filter = channel_filter.clone();
                                    let thread_channels = Arc::clone(&self.thread_channels);
                                    let pending = Arc::clone(&self.pending_interactions);
                                    let pending_components = Arc::clone(&self.pending_components);
                                    let pending_approvals = Arc::clone(&self.pending_approvals);
                                    let alias = self.alias.clone();
                                    let tx = tx.clone();

                                    zeroclaw_spawn::spawn!(async move {
                                        // Cheap peer check first (parity with
                                        // type-2): an unauthorized invoker must
                                        // not be able to drive the authenticated
                                        // thread-lookup REST call.
                                        // interaction_gate re-checks fail-closed.
                                        if !crate::allowlist::is_user_allowed(
                                            &peers,
                                            &user_id,
                                            crate::allowlist::Match::Sensitive,
                                        ) {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "denial": "UnauthorizedUser"})), "rejecting unauthorized component interaction");
                                            let msg = i18n::get_required_cli_string(
                                                "channel-discord-interaction-unauthorized",
                                            );
                                            if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction reject failed");
                                            }
                                            return;
                                        }
                                        let parent_id = if !channel_filter.is_empty()
                                            && !interaction_channel.is_empty()
                                            && !channel_filter.iter().any(|c| c == &interaction_channel)
                                        {
                                            discord_thread_parent(
                                                &client,
                                                &bot_token,
                                                &thread_channels,
                                                &interaction_channel,
                                            )
                                            .await
                                        } else {
                                            None
                                        };
                                        if let Err(denial) = interaction_gate(
                                            &peers,
                                            &guild_filter,
                                            &channel_filter,
                                            &user_id,
                                            interaction_guild.as_deref(),
                                            &interaction_channel,
                                            parent_id.as_deref(),
                                        ) {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "denial": format!("{denial:?}")})), "rejecting unauthorized component interaction");
                                            let msg = i18n::get_required_cli_string(
                                                "channel-discord-interaction-unauthorized",
                                            );
                                            if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord interaction reject failed");
                                            }
                                            return;
                                        }

                                        // Single-use: drain the intent bound to
                                        // this custom_id. The `take` runs ONLY
                                        // after the fail-closed gate above, so an
                                        // unauthorized click never drains an
                                        // entry. Absent/expired/replayed (incl. a
                                        // forged-but-zc1 id we never registered)
                                        // → refuse, don't act.
                                        let intent = pending_components.lock().take(&custom_id_raw);
                                        let prompt = match intent {
                                            // Buttoned approval: resolve the parked
                                            // `oneshot` keyed by the registered
                                            // token with the SERVER-bound decision
                                            // (never derived from the wire
                                            // custom_id). Ack the click; do NOT
                                            // enqueue a turn.
                                            Some(ComponentIntent::Approval { token, decision }) => {
                                                let resolved = {
                                                    let mut guard = pending_approvals.lock().await;
                                                    approval::resolve_parked_approval(
                                                        &mut guard, &token, decision,
                                                    )
                                                };
                                                // Ack the interaction so the
                                                // operator doesn't see "did not
                                                // respond". An already-resolved
                                                // token (raced/timed-out) just
                                                // means the buttons are stale.
                                                let key = if resolved {
                                                    "channel-discord-approval-recorded"
                                                } else {
                                                    "channel-discord-component-expired"
                                                };
                                                let msg = i18n::get_required_cli_string(key);
                                                if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord approval ack failed");
                                                }
                                                return;
                                            }
                                            Some(ComponentIntent::ResolveIntoTurn { prompt }) => prompt,
                                            // Absent / expired / replayed / forged.
                                            None => {
                                                let msg = i18n::get_required_cli_string(
                                                    "channel-discord-component-expired",
                                                );
                                                if let Err(e) = discord_reject_interaction(&client, &interaction_id, &interaction_token, &msg).await {
                                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord component expired-notice failed");
                                                }
                                                return;
                                            }
                                        };

                                        // A modal submit appends its typed-in
                                        // fields ("label: value" lines) to the
                                        // registered prompt; a component click has
                                        // none, so the prompt is used as-is.
                                        let content = if modal_fields.is_empty() {
                                            prompt
                                        } else {
                                            let mut c = prompt;
                                            for (field, value) in &modal_fields {
                                                c.push_str(&format!("\n{field}: {value}"));
                                            }
                                            c
                                        };

                                        // Stash creds before the defer so a fast
                                        // reply can't race an absent entry.
                                        {
                                            let mut guard = pending.lock();
                                            guard.retain(|_, p| {
                                                p.created.elapsed() < INTERACTION_TOKEN_TTL
                                            });
                                            guard.insert(
                                                interaction_id.clone(),
                                                PendingInteraction {
                                                    app_id: app_id.clone(),
                                                    token: interaction_token.clone(),
                                                    created: std::time::Instant::now(),
                                                },
                                            );
                                        }
                                        if let Err(e) = discord_defer_interaction(&client, &interaction_id, &interaction_token).await {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord component defer failed");
                                            pending.lock().remove(&interaction_id);
                                            return;
                                        }

                                        // Resolve-into-turn: the registered
                                        // intent drives an agent turn, answered
                                        // through the interaction followup.
                                        let channel_msg = ChannelMessage {
                                            id: format!("discord_interaction_{interaction_id}"),
                                            sender: user_id,
                                            reply_target: discord_interaction_reply_target(&interaction_id),
                                            content,
                                            channel: "discord".to_string(),
                                            channel_alias: Some(alias),
                                            timestamp: std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs(),
                                            interruption_scope_id: None,
                                            thread_ts: None,
                                            attachments: Vec::new(),
                                            subject: None,
                                        };
                                        if tx.send(channel_msg).await.is_err() {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "orchestrator channel closed; dropping component prompt");
                                        }
                                    });
                                }
                            } else if itype == 4 {
                                // type 4 = APPLICATION_COMMAND_AUTOCOMPLETE.
                                // Fired on EVERY keystroke in a focused option,
                                // so it must be cheap and side-effect-free: it
                                // answers inline with a type-8
                                // (AUTOCOMPLETE_RESULT) choice set and NEVER
                                // defers or posts an ephemeral. Authorization
                                // reuses the same `interaction_gate`
                                // (fail-closed) as the other arms, but evaluated
                                // WITHOUT the reject side-effect — an
                                // unauthorized keystroke gets an empty choice
                                // set (no policy leak, no hang), exactly like a
                                // query with no matches.
                                let interaction_id = d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let interaction_token = d.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let user_id = d
                                    .get("member")
                                    .and_then(|m| m.get("user"))
                                    .or_else(|| d.get("user"))
                                    .and_then(|u| u.get("id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let interaction_guild = d
                                    .get("guild_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);
                                let interaction_channel = d
                                    .get("channel_id")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                if !interaction_id.is_empty() && !interaction_token.is_empty() {
                                    let client = self.http_client();
                                    let peers = (self.peer_resolver)();
                                    let guild_filter = guild_filter.clone();
                                    let channel_filter = channel_filter.clone();

                                    zeroclaw_spawn::spawn!(async move {
                                        // Fail-closed authz, side-effect-free:
                                        // `interaction_gate` is a pure check (the
                                        // reject/defer side-effects in the other
                                        // arms are separate REST calls we simply
                                        // don't make here). On denial OR no
                                        // matches we answer an empty choice set.
                                        //
                                        // No thread-parent REST lookup: it is an
                                        // authenticated round-trip per keystroke
                                        // and would defeat the side-effect-free
                                        // requirement, so a channel-filtered
                                        // thread simply yields no completions
                                        // (fail-closed) rather than probing.
                                        let authorized = interaction_gate(
                                            &peers,
                                            &guild_filter,
                                            &channel_filter,
                                            &user_id,
                                            interaction_guild.as_deref(),
                                            &interaction_channel,
                                            None,
                                        )
                                        .is_ok();

                                        // LIMITATION: choices come from EPIC D's
                                        // typed-option model, which is not wired
                                        // in this branch. Until then an
                                        // authorized keystroke answers an empty
                                        // (valid) choice set — the dispatch +
                                        // fail-closed gate are the deliverable
                                        // here; populated choices land with D.
                                        let choices: Vec<(String, String)> = Vec::new();
                                        let answer: &[(String, String)] =
                                            if authorized { &choices } else { &[] };

                                        if let Err(e) = discord_answer_autocomplete(
                                            &client,
                                            &interaction_id,
                                            &interaction_token,
                                            answer,
                                        )
                                        .await
                                        {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": e.to_string()})), "discord autocomplete answer failed");
                                        }
                                    });
                                }
                            }
                        }
                        continue;
                    }
                    // MESSAGE_UPDATE / MESSAGE_DELETE / MESSAGE_DELETE_BULK
                    // keep the archive in sync. All three already arrive
                    // under the GUILD_MESSAGES / DIRECT_MESSAGES intents;
                    // agent routing stays MESSAGE_CREATE-only.
                    if event_type == "MESSAGE_UPDATE"
                        || event_type == "MESSAGE_DELETE"
                        || event_type == "MESSAGE_DELETE_BULK"
                    {
                        if let Some(d) = event.get("d") {
                            self.sync_archive_for_message_event(event_type, d, &bot_user_id)
                                .await;
                        }
                        continue;
                    }

                    // Inbound reaction events — only delivered at all when
                    // the IDENTIFY mask included the reaction intents. The
                    // scope re-check matters when a raw `intents_mask`
                    // override requested the reaction bits while
                    // `reaction_notifications = off`, or on a resumed
                    // session that negotiated a wider mask.
                    if event_type == "MESSAGE_REACTION_ADD"
                        || event_type == "MESSAGE_REACTION_REMOVE"
                    {
                        if self.reaction_scope
                            != zeroclaw_config::schema::DiscordReactionScope::Off
                            && let Some(d) = event.get("d")
                        {
                            self.handle_reaction_event(event_type, d, &bot_user_id).await;
                        }
                        continue;
                    }

                    // Only handle MESSAGE_CREATE (opcode 0, type "MESSAGE_CREATE")
                    if event_type != "MESSAGE_CREATE" {
                        continue;
                    }

                    let Some(d) = event.get("d") else {
                        continue;
                    };

                    // Skip non-conversational system messages. Discord posts a
                    // MESSAGE_CREATE of type 18 (THREAD_CREATED) in the parent
                    // channel when a thread is born — authored by the human who
                    // created it, with the thread name as content — which would
                    // otherwise pass the admit gate and make the bot "reply" to
                    // the thread's birth. Type 21 (THREAD_STARTER_MESSAGE), pins,
                    // joins, etc. are likewise not user turns. Only DEFAULT (0)
                    // and REPLY (19) are real messages to act on. Absent `type`
                    // defaults to 0 for forward-compatibility.
                    let message_type = d.get("type").and_then(serde_json::Value::as_u64).unwrap_or(0);
                    if !is_conversational_message_type(message_type) {
                        continue;
                    }

                    // Skip messages from the bot itself
                    let author_id = d.get("author").and_then(|a| a.get("id")).and_then(|i| i.as_str()).unwrap_or("");
                    if author_id == bot_user_id {
                        continue;
                    }

                    // Skip bot messages (unless listen_to_bots is enabled)
                    if !self.listen_to_bots && d.get("author").and_then(|a| a.get("bot")).and_then(serde_json::Value::as_bool).unwrap_or(false) {
                        continue;
                    }

                    // Sender validation
                    if !self.is_user_allowed(author_id) {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"author_id": author_id})), "ignoring message from unauthorized user");
                        continue;
                    }

                    // Guild allowlist. Empty list = accept all guilds.
                    // DMs have no guild_id, so they always pass through.
                    if !guild_filter.is_empty() {
                        let msg_guild = d.get("guild_id").and_then(serde_json::Value::as_str);
                        if let Some(g) = msg_guild
                            && !guild_filter.iter().any(|allowed| allowed == g)
                        {
                            continue;
                        }
                    }

                    // Channel allowlist. Empty = watch every channel.
                    // Thread messages carry the thread's own channel_id, not the
                    // parent's. When the direct match fails, look up the thread's
                    // parent_id and accept if *that* is in the allowlist.
                    if !channel_filter.is_empty() {
                        let msg_channel = d
                            .get("channel_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        let parent_id = if !msg_channel.is_empty()
                            && !channel_filter.iter().any(|c| c == msg_channel)
                        {
                            self.thread_parent(&self.http_client(), msg_channel).await
                        } else {
                            None
                        };
                        if !channel_passes_filter(
                            &channel_filter,
                            msg_channel,
                            parent_id.as_deref(),
                        ) {
                            continue;
                        }
                    }

                    // Archive every non-bot message to discord.db when enabled.
                    if let Some(ref archive_mem) = archive_memory {
                        let archive_channel_id =
                            d.get("channel_id").and_then(|c| c.as_str()).unwrap_or("");
                        let is_dm_event = d.get("guild_id").is_none();
                        let username = d
                            .get("author")
                            .and_then(|a| a.get("username"))
                            .and_then(|u| u.as_str())
                            .unwrap_or(author_id);
                        let content_raw =
                            d.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        let archive_msg_id =
                            d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        if !content_raw.is_empty() {
                            let ts = chrono::Utc::now().to_rfc3339();
                            let channel_display =
                                if is_dm_event { "dm" } else { archive_channel_id };
                            let atts = d
                                .get("attachments")
                                .and_then(|a| a.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|a| a.get("url").and_then(|u| u.as_str()))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                                .unwrap_or_default();
                            let mut mem_content = format!(
                                "@{username} in #{channel_display} at {ts}: {content_raw}"
                            );
                            if !atts.is_empty() {
                                mem_content.push_str(&format!(" [attachments: {atts}]"));
                            }
                            let mem_key = if archive_msg_id.is_empty() {
                                format!("discord_{}", Uuid::new_v4())
                            } else {
                                format!("discord_{archive_msg_id}")
                            };
                            let session = if archive_channel_id.is_empty() {
                                None
                            } else {
                                Some(archive_channel_id)
                            };
                            if let Err(e) = archive_mem
                                .store(
                                    &mem_key,
                                    &mem_content,
                                    zeroclaw_memory::MemoryCategory::Custom(
                                        "discord".to_string(),
                                    ),
                                    session,
                                )
                                .await
                            {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "archive store failed");
                            }
                        }
                    }

                    let content = d.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    // DMs carry no guild_id in the Discord gateway payload. They are
                    // inherently private and implicitly addressed to the bot, so bypass
                    // the mention gate — requiring a @mention in a DM is never correct.
                    let is_dm = d.get("guild_id").is_none();
                    let effective_mention_only = self.mention_only && !is_dm;
                    let atts = d
                        .get("attachments")
                        .and_then(|a| a.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let has_attachments = !atts.is_empty();
                    let Some(clean_content) = admit_discord_message(
                        content,
                        has_attachments,
                        effective_mention_only,
                        &bot_user_id,
                    ) else {
                        continue;
                    };

                    let client = self.http_client();
                    let (attachment_text, media_attachments) = process_attachments(
                        &atts,
                        &client,
                        self.workspace_dir.as_deref(),
                        self.transcription_manager.as_deref(),
                    )
                    .await;
                    let final_content = if attachment_text.is_empty() {
                        clean_content
                    } else {
                        format!("{clean_content}\n\n[Attachments]\n{attachment_text}")
                    };

                    // Intercept approval replies before forwarding to the agent.
                    if let Some((token, response)) =
                        crate::util::parse_approval_reply(&final_content)
                    {
                        let mut map = self.pending_approvals.lock().await;
                        if let Some(sender) = map.remove(&token) {
                            let _ = sender.send(response);
                            continue;
                        }
                    }

                    let message_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let channel_id = d
                        .get("channel_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();

                    if !message_id.is_empty() && !channel_id.is_empty() {
                        let reaction_channel = DiscordChannel::new(
                            self.bot_token.clone(),
                            self.guild_ids.clone(),
                            self.alias.clone(),
                            Arc::clone(&self.peer_resolver),
                            self.listen_to_bots,
                            self.mention_only,
                        );
                        let reaction_channel_id = channel_id.clone();
                        let reaction_message_id = message_id.to_string();
                        let reaction_emoji = random_discord_ack_reaction().to_string();
                        zeroclaw_spawn::spawn!(async move {
                            if let Err(err) = reaction_channel
                                .add_reaction(
                                    &reaction_channel_id,
                                    &reaction_message_id,
                                    &reaction_emoji,
                                )
                                .await
                            {
                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"reaction_message_id": reaction_message_id, "err": err.to_string()})), "failed to add ACK reaction for message");
                            }
                        });
                    }

                    // Thread context decides `thread_ts` plus `interruption_scope_id`,
                    // which the orchestrator uses as part of the conversation-history
                    // key and the cancellation scope. When the lookup fails it falls
                    // back to `None` and the failure is not cached, so the next
                    // message in the same Discord thread will retry. The trade-off:
                    // the first message after a transient lookup miss is keyed
                    // without the thread suffix; once the cache warms, subsequent
                    // messages are keyed with it. History for that thread can split
                    // across two scopes until the warm-up completes. Acceptable
                    // because the lookup is bounded by `THREAD_LOOKUP_TIMEOUT` and
                    // the alternative (stalling the listener on a hung Discord call)
                    // is worse.
                    let thread_ts = if channel_id.is_empty() {
                        None
                    } else if self.thread_parent(&client, &channel_id).await.is_some()
                    {
                        Some(channel_id.clone())
                    } else {
                        None
                    };

                    let channel_msg = ChannelMessage {
                        id: if message_id.is_empty() {
                            Uuid::new_v4().to_string()
                        } else {
                            format!("discord_{message_id}")
                        },
                        sender: author_id.to_string(),
                        reply_target: if channel_id.is_empty() {
                            author_id.to_string()
                        } else {
                            channel_id.clone()
                        },
                        content: final_content,
                        channel: "discord".to_string(),
                        channel_alias: Some(self.alias.clone()),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        interruption_scope_id: thread_ts.clone(),
                        thread_ts,
                        attachments: media_attachments,
                        subject: None,
                    };

                    if tx.send(channel_msg).await.is_err() {
                        break;
                    }
                }
            }
        }

        // Clean up the watchdog task before returning so the outer
        // reconnection loop can start fresh.
        if let Some(ref wd) = watchdog {
            wd.stop().await;
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://discord.com/api/v10/users/@me")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        // Interaction sentinels are not channel ids; the deferred "thinking…"
        // state already plays the typing role for slash replies.
        if parse_discord_interaction_target(recipient).is_some() {
            return Ok(());
        }
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let token = self.bot_token.clone();
        let channel_id = recipient.to_string();

        let handle = zeroclaw_spawn::spawn!(async move {
            let url = format!("https://discord.com/api/v10/channels/{channel_id}/typing");
            loop {
                let _ = client
                    .post(&url)
                    .header("Authorization", format!("Bot {token}"))
                    .send()
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            }
        });

        let mut guard = self.typing_handles.lock();
        guard.insert(recipient.to_string(), handle);

        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let mut guard = self.typing_handles.lock();
        if let Some(handle) = guard.remove(recipient) {
            handle.abort();
        }
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        self.stream_mode != zeroclaw_config::schema::StreamMode::Off
    }

    fn supports_multi_message_streaming(&self) -> bool {
        self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.multi_message_delay_ms
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        use zeroclaw_config::schema::StreamMode;
        // Interaction replies have no channel to draft into — the recipient
        // is a sentinel, not a channel id. The final answer arrives via the
        // followup-webhook edit in send().
        if parse_discord_interaction_target(&message.recipient).is_some() {
            return Ok(None);
        }
        match self.stream_mode {
            StreamMode::Off => Ok(None),
            StreamMode::Partial => {
                let initial_text = if message.content.is_empty() {
                    "...".to_string()
                } else {
                    message.content.clone()
                };

                let client = self.http_client();
                let msg_id = send_discord_message_json(
                    &client,
                    &self.bot_token,
                    &message.recipient,
                    &initial_text,
                )
                .await?;

                self.last_draft_edit
                    .lock()
                    .insert(message.recipient.clone(), std::time::Instant::now());

                Ok(Some(msg_id))
            }
            StreamMode::MultiMessage => {
                // No initial draft — paragraphs are sent as new messages.
                // Store thread context for paragraph delivery.
                self.multi_message_sent_len.lock().clear();
                self.multi_message_thread_ts
                    .lock()
                    .insert(message.recipient.clone(), message.thread_ts.clone());
                Ok(Some("multi_message_synthetic".to_string()))
            }
        }
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        use zeroclaw_config::schema::StreamMode;
        // Sentinel recipients have no draft message (see send_draft).
        if parse_discord_interaction_target(recipient).is_some() {
            return Ok(());
        }
        match self.stream_mode {
            StreamMode::Off => Ok(()),
            StreamMode::Partial => {
                // Rate-limit edits per channel.
                {
                    let last_edits = self.last_draft_edit.lock();
                    if let Some(last_time) = last_edits.get(recipient) {
                        let elapsed_ms =
                            u64::try_from(last_time.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if elapsed_ms < self.draft_update_interval_ms {
                            return Ok(());
                        }
                    }
                }

                // UTF-8 safe truncation to Discord limit.
                let display_text = if text.len() > DISCORD_MAX_MESSAGE_LENGTH {
                    let mut end = 0;
                    for (idx, ch) in text.char_indices() {
                        let next = idx + ch.len_utf8();
                        if next > DISCORD_MAX_MESSAGE_LENGTH {
                            break;
                        }
                        end = next;
                    }
                    &text[..end]
                } else {
                    text
                };

                let client = self.http_client();
                match edit_discord_message(
                    &client,
                    &self.bot_token,
                    recipient,
                    message_id,
                    display_text,
                )
                .await
                {
                    Ok(()) => {
                        self.last_draft_edit
                            .lock()
                            .insert(recipient.to_string(), std::time::Instant::now());
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "draft update failed"
                        );
                    }
                }

                Ok(())
            }
            StreamMode::MultiMessage => {
                // Track accumulated text and send new paragraphs at \n\n boundaries.
                // Extract paragraph (if any) under the lock, then drop it before async work.
                let (paragraph, thread_ts) = {
                    let thread_ts = self
                        .multi_message_thread_ts
                        .lock()
                        .get(recipient)
                        .cloned()
                        .flatten();
                    let mut sent_map = self.multi_message_sent_len.lock();
                    let sent_so_far = sent_map.get(recipient).copied().unwrap_or(0);

                    // DraftEvent::Clear resets accumulated text — reset our counter.
                    if text.len() < sent_so_far {
                        sent_map.insert(recipient.to_string(), 0);
                        return Ok(());
                    }
                    if text.len() == sent_so_far {
                        return Ok(());
                    }

                    let new_text = &text[sent_so_far..];
                    let mut scan_pos = 0;
                    let mut in_fence = false;
                    let bytes = new_text.as_bytes();
                    let mut found_paragraph = None;

                    while scan_pos < bytes.len() {
                        let ch = bytes[scan_pos];

                        if ch == b'`'
                            && scan_pos + 2 < bytes.len()
                            && bytes[scan_pos + 1] == b'`'
                            && bytes[scan_pos + 2] == b'`'
                            && (scan_pos == 0 || bytes[scan_pos - 1] == b'\n')
                        {
                            in_fence = !in_fence;
                        }

                        if !in_fence
                            && ch == b'\n'
                            && scan_pos + 1 < bytes.len()
                            && bytes[scan_pos + 1] == b'\n'
                        {
                            let paragraph = new_text[..scan_pos].trim().to_string();
                            let consumed = scan_pos + 2;
                            *sent_map.entry(recipient.to_string()).or_insert(0) += consumed;
                            if !paragraph.is_empty() {
                                found_paragraph = Some(paragraph);
                            }
                            break;
                        }

                        scan_pos += 1;
                    }
                    // Lock is dropped here at end of block.
                    (found_paragraph, thread_ts)
                };

                if let Some(paragraph) = paragraph {
                    let msg = SendMessage::new(&paragraph, recipient).in_thread(thread_ts.clone());
                    if let Err(e) = self.send(&msg).await {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "multi-message paragraph send failed"
                        );
                    }
                    if self.multi_message_delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            self.multi_message_delay_ms,
                        ))
                        .await;
                    }
                    // Recurse to handle remaining text.
                    return self.update_draft(recipient, message_id, text).await;
                }

                Ok(())
            }
        }
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage {
            // Flush remaining buffered text.
            let thread_ts = self
                .multi_message_thread_ts
                .lock()
                .remove(recipient)
                .flatten();
            let sent_so_far = self
                .multi_message_sent_len
                .lock()
                .remove(recipient)
                .unwrap_or(0);
            if text.len() > sent_so_far {
                let remaining = text[sent_so_far..].trim().to_string();
                if !remaining.is_empty() {
                    let msg = SendMessage::new(&remaining, recipient).in_thread(thread_ts);
                    if let Err(e) = self.send(&msg).await {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "multi-message final flush failed"
                        );
                    }
                }
            }
            return Ok(());
        }

        // Belt-and-suspenders: kill any typing handles for this channel.
        let _ = self.stop_typing(recipient).await;
        self.last_draft_edit.lock().remove(recipient);

        let text = &crate::util::strip_tool_call_tags(text);
        // Lift `[EMBED:…]` out before the media-marker scan (its JSON can contain
        // `[`/`]`); embeds attach to the first finalized message below, so a
        // streaming/draft reply renders embeds the same as a normal send.
        let (text_without_embeds, embeds, _embed_failures, _embeds_truncated) =
            prepare_outgoing_embeds(text, self.workspace_dir.as_deref());
        let (cleaned_content, parsed_attachments) = parse_attachment_markers(&text_without_embeds);
        let (mut local_files, remote_urls, failures) =
            classify_outgoing_attachments(&parsed_attachments, self.workspace_dir.as_deref());
        let body = with_inline_attachment_urls(&cleaned_content, &remote_urls);
        let note = delivery_failure_note(&failures);
        let content = compose_body_with_failure_note(&body, note.as_deref());
        let reactions = decide_failure_reactions(&failures);

        let client = self.http_client();

        // Path 1: file attachments — delete draft and POST fresh message with files.
        if !local_files.is_empty() {
            let _ = delete_discord_message(&client, &self.bot_token, recipient, message_id).await;

            if local_files.len() > 10 {
                local_files.truncate(10);
            }
            let chunks = split_message_for_discord(&content);
            let mut first_message_id: Option<String> = None;
            for (i, chunk) in chunks.iter().enumerate() {
                let new_id = if i == 0 {
                    // Embeds + files ride the first message.
                    let payload = DiscordOutgoing {
                        content: Some(chunk.clone()),
                        embeds: embeds.clone(),
                        ..Default::default()
                    };
                    send_discord_message_payload_with_files(
                        &client,
                        &self.bot_token,
                        recipient,
                        &payload,
                        &local_files,
                    )
                    .await?
                } else {
                    send_discord_message_json(&client, &self.bot_token, recipient, chunk).await?
                };
                if first_message_id.is_none() {
                    first_message_id = Some(new_id);
                }
                if i < chunks.len() - 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            self.apply_failure_reactions(recipient, first_message_id.as_deref(), &reactions)
                .await;
            return Ok(());
        }

        // Path 2: text exceeds limit — delete draft and POST as chunked messages.
        if content.chars().count() > DISCORD_MAX_MESSAGE_LENGTH {
            let _ = delete_discord_message(&client, &self.bot_token, recipient, message_id).await;

            let chunks = split_message_for_discord(&content);
            let mut first_message_id: Option<String> = None;
            for (i, chunk) in chunks.iter().enumerate() {
                let new_id = if i == 0 {
                    // Embeds ride the first message.
                    let payload = DiscordOutgoing {
                        content: Some(chunk.clone()),
                        embeds: embeds.clone(),
                        ..Default::default()
                    };
                    send_discord_message_payload(&client, &self.bot_token, recipient, &payload)
                        .await?
                } else {
                    send_discord_message_json(&client, &self.bot_token, recipient, chunk).await?
                };
                if first_message_id.is_none() {
                    first_message_id = Some(new_id);
                }
                if i < chunks.len() - 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            self.apply_failure_reactions(recipient, first_message_id.as_deref(), &reactions)
                .await;
            return Ok(());
        }

        // Path 3: simple case — edit in-place (with any embeds); fall back to
        // delete + POST on failure. The reaction target is the draft message_id
        // when the edit lands; when the fallback fires it's the freshly posted
        // message instead.
        let payload = DiscordOutgoing {
            content: Some(content.clone()),
            embeds: embeds.clone(),
            ..Default::default()
        };
        let reaction_target = match edit_discord_message_payload(
            &client,
            &self.bot_token,
            recipient,
            message_id,
            &payload,
        )
        .await
        {
            Ok(()) => message_id.to_string(),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "Discord finalize_draft edit failed: ; falling back to delete+send"
                );
                let _ =
                    delete_discord_message(&client, &self.bot_token, recipient, message_id).await;
                send_discord_message_payload(&client, &self.bot_token, recipient, &payload).await?
            }
        };
        self.apply_failure_reactions(recipient, Some(&reaction_target), &reactions)
            .await;

        Ok(())
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage {
            self.multi_message_sent_len.lock().remove(recipient);
            self.multi_message_thread_ts.lock().remove(recipient);
            return Ok(());
        }

        let _ = self.stop_typing(recipient).await;
        self.last_draft_edit.lock().remove(recipient);

        let client = self.http_client();
        if let Err(e) =
            delete_discord_message(&client, &self.bot_token, recipient, message_id).await
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "cancel_draft delete failed"
            );
        }

        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        // Interaction sentinels are not channel ids; a reaction REST call
        // against one is guaranteed 404 (with the sentinel in the URL).
        if parse_discord_interaction_target(channel_id).is_some() {
            return Ok(());
        }
        let url = discord_reaction_url(channel_id, message_id, emoji);

        let resp = self
            .http_client()
            .put(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .header("Content-Length", "0")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            anyhow::bail!("Discord add reaction failed ({status}): {err}");
        }

        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        // Interaction sentinels are not channel ids (see add_reaction).
        if parse_discord_interaction_target(channel_id).is_some() {
            return Ok(());
        }
        let url = discord_reaction_url(channel_id, message_id, emoji);

        let resp = self
            .http_client()
            .delete(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            anyhow::bail!("Discord remove reaction failed ({status}): {err}");
        }

        Ok(())
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        // Approval prompts can't be delivered over a deferred interaction
        // reply (the sentinel is not a channel and the single @original
        // edit is reserved for the answer). Fail fast so the agent loop's
        // deny-by-default applies instead of a doomed REST round-trip.
        if parse_discord_interaction_target(recipient).is_some() {
            anyhow::bail!("approval prompts are not supported over interaction replies");
        }
        let token = crate::util::new_approval_token();

        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(token.clone(), tx);

        // Strip thread suffix — approval message goes to the channel root.
        let channel_id = recipient.split(':').next().unwrap_or(recipient);

        // Prefer the buttoned prompt when the interaction pipe is live: a click
        // can only be dispatched (type-3) and thus resolve the `oneshot` when
        // `slash_commands` is enabled (the INTERACTION_CREATE handler is gated
        // on it). Emitting buttons without that pipe would leave the operator
        // with dead controls — so fall back to the plaintext-token prompt the
        // inbound MESSAGE_CREATE path parses (`parse_approval_reply`).
        let emitted = if self.slash_commands {
            self.send_buttoned_approval(channel_id, &token, request)
                .await
        } else {
            self.send_plaintext_approval(channel_id, &token, request)
                .await
        };
        if let Err(err) = emitted {
            self.pending_approvals.lock().await.remove(&token);
            return Err(err);
        }

        // Timeout → Deny, preserving the deny-by-default silence semantics. The
        // pending entry is dropped so a late click can't resolve a stale token.
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
    use std::fmt::Write as _;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_string()).collect()
    }

    #[test]
    fn prepare_outgoing_embeds_lifts_marker_vets_urls_and_strips_text() {
        let raw = "look [EMBED:{\"title\":\"Report\",\"image\":\"https://ex.com/i.png\"}] done";
        let (text, embeds, failures, truncated) = prepare_outgoing_embeds(raw, None);
        assert_eq!(text, "look  done");
        assert_eq!(embeds.len(), 1);
        assert_eq!(embeds[0].title.as_deref(), Some("Report"));
        assert_eq!(
            embeds[0].image.as_ref().unwrap().url,
            "https://ex.com/i.png"
        );
        assert!(failures.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn prepare_outgoing_embeds_drops_bad_url_and_reports_failure() {
        let raw = "[EMBED:{\"title\":\"T\",\"image\":\"file:///etc/passwd\"}]";
        let (text, embeds, failures, _) = prepare_outgoing_embeds(raw, None);
        assert_eq!(text, "");
        assert_eq!(embeds.len(), 1);
        assert!(embeds[0].image.is_none(), "disallowed scheme dropped");
        assert_eq!(failures, vec![DiscordMarkerFailure::Refused]);
    }

    #[test]
    fn prepare_outgoing_embeds_flags_structural_truncation() {
        // 11 embeds → over the 10-per-message cap → truncated, ⚠️ territory.
        let markers: String = (0..11)
            .map(|i| format!("[EMBED:{{\"title\":\"t{i}\"}}]"))
            .collect();
        let (_, embeds, _, truncated) = prepare_outgoing_embeds(&markers, None);
        assert_eq!(embeds.len(), 10);
        assert!(truncated);
    }

    #[test]
    fn prepare_outgoing_embeds_leaves_plain_text_untouched() {
        let (text, embeds, failures, truncated) =
            prepare_outgoing_embeds("just a normal reply", None);
        assert_eq!(text, "just a normal reply");
        assert!(embeds.is_empty());
        assert!(failures.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn finalize_draft_builds_a_first_message_payload_carrying_embeds() {
        // finalize_draft (and the slash-reply path) lift embeds out of the final
        // text and attach them to the first message's DiscordOutgoing — the same
        // transformation send() does. Pin that so neither path regresses to
        // content-only and leaks the raw [EMBED:…] marker.
        let raw = "Result [EMBED:{\"title\":\"Report\"}]";
        let (content, embeds, _failures, _truncated) = prepare_outgoing_embeds(raw, None);
        assert_eq!(content, "Result");
        assert_eq!(embeds.len(), 1);
        let payload = DiscordOutgoing {
            content: Some(content),
            embeds,
            ..Default::default()
        };
        assert_eq!(
            payload.to_rest_json(),
            serde_json::json!({ "content": "Result", "embeds": [{ "title": "Report" }] })
        );
    }

    #[test]
    fn interaction_gate_applies_peer_allowlist() {
        // Wildcard admits anyone; otherwise the invoker must be listed.
        assert_eq!(
            interaction_gate(&s(&["*"]), &[], &[], "u1", None, "c1", None),
            Ok(())
        );
        assert_eq!(
            interaction_gate(&s(&["u1"]), &[], &[], "u1", None, "c1", None),
            Ok(())
        );
        assert_eq!(
            interaction_gate(&s(&["u1"]), &[], &[], "intruder", None, "c1", None),
            Err(InteractionDenial::UnauthorizedUser)
        );
        // Empty peer list = nobody, same as the message path.
        assert_eq!(
            interaction_gate(&[], &[], &[], "u1", None, "c1", None),
            Err(InteractionDenial::UnauthorizedUser)
        );
    }

    #[test]
    fn interaction_gate_applies_guild_and_channel_filters() {
        let peers = s(&["*"]);
        let guilds = s(&["g1"]);
        let channels = s(&["c1"]);

        assert_eq!(
            interaction_gate(&peers, &guilds, &channels, "u1", Some("g1"), "c1", None),
            Ok(())
        );
        assert_eq!(
            interaction_gate(&peers, &guilds, &[], "u1", Some("g2"), "c1", None),
            Err(InteractionDenial::GuildNotAllowed)
        );
        // DM interactions carry no guild_id and pass the guild filter,
        // mirroring MESSAGE_CREATE.
        assert_eq!(
            interaction_gate(&peers, &guilds, &[], "u1", None, "c1", None),
            Ok(())
        );
        assert_eq!(
            interaction_gate(&peers, &[], &channels, "u1", Some("g1"), "c2", None),
            Err(InteractionDenial::ChannelNotAllowed)
        );
        // A thread whose parent is allowlisted passes, like threaded messages.
        assert_eq!(
            interaction_gate(
                &peers,
                &[],
                &channels,
                "u1",
                Some("g1"),
                "thread9",
                Some("c1")
            ),
            Ok(())
        );
    }

    #[tokio::test]
    async fn interaction_answer_over_2000_chars_chunks_into_edit_plus_followup() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // The first chunk edits the deferred @original message.
        Mock::given(method("PATCH"))
            .and(path("/webhooks/app1/tok/messages/@original"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        // The remaining chunk is delivered as a followup POST (not truncated).
        Mock::given(method("POST"))
            .and(path("/webhooks/app1/tok"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        // 3000 contiguous chars (no break point) → a 2000-char chunk + a 1000.
        let content = "a".repeat(3000);
        deliver_interaction_answer(&client, "app1", "tok", &server.uri(), &content, &[])
            .await
            .unwrap();
        // wiremock verifies the expect(1) counts when the server drops.
    }

    #[tokio::test]
    async fn short_interaction_answer_only_edits_original() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/webhooks/app1/tok/messages/@original"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        // A short reply must not trigger any followup POST.
        Mock::given(method("POST"))
            .and(path("/webhooks/app1/tok"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        deliver_interaction_answer(&client, "app1", "tok", &server.uri(), "short answer", &[])
            .await
            .unwrap();
    }

    #[test]
    fn interaction_reply_target_roundtrips() {
        let target = discord_interaction_reply_target("123456789");
        assert_eq!(target, "interaction:123456789");
        assert_eq!(parse_discord_interaction_target(&target), Some("123456789"));
    }

    #[test]
    fn non_interaction_targets_are_ignored() {
        // A normal Discord channel id must NOT be treated as an interaction.
        assert_eq!(parse_discord_interaction_target("123456789012345678"), None);
        // Empty ids are rejected.
        assert_eq!(parse_discord_interaction_target("interaction:"), None);
        // The legacy `app:token` form (which carried a live credential in the
        // reply target) must never round-trip as valid again.
        assert_eq!(
            parse_discord_interaction_target("interaction:app123:tok456"),
            None
        );
    }

    #[tokio::test]
    async fn send_to_unknown_interaction_sentinel_fails_without_rest() {
        // A sentinel whose credentials are not in the pending store (expired,
        // restarted process, forged) must error out before any HTTP happens.
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        );
        let msg = SendMessage {
            content: "hello".into(),
            recipient: "interaction:999".into(),
            subject: None,
            thread_ts: None,
            cancellation_token: None,
            attachments: Vec::new(),
            in_reply_to: None,
        };
        let err = ch.send(&msg).await.unwrap_err();
        assert!(err.to_string().contains("unknown or expired"));
    }

    fn skill(name: &str, description: &str, tags: &[&str]) -> zeroclaw_runtime::skills::Skill {
        zeroclaw_runtime::skills::Skill {
            name: name.to_string(),
            description: description.to_string(),
            version: "1.0.0".to_string(),
            author: None,
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            tools: vec![],
            prompts: vec![],
            slash_options: Vec::new(),
            location: None,
        }
    }

    #[test]
    fn command_slug_fits_discord_charset() {
        assert_eq!(discord_command_slug("Deploy Status"), "deploy-status");
        assert_eq!(discord_command_slug("summarize_pdf"), "summarize_pdf");
        assert_eq!(discord_command_slug("a  b!!c"), "a-b-c");
        assert_eq!(discord_command_slug("--weird--"), "weird");
        assert_eq!(discord_command_slug(""), "");
        // All-non-ASCII names slug to empty (documented limitation).
        assert_eq!(discord_command_slug("日本語スキル"), "");
        // 32-char cap, with a trailing dash at the boundary trimmed.
        assert_eq!(discord_command_slug(&"x".repeat(50)).len(), 32);
        let boundary = format!("{} tail", "y".repeat(31));
        let slug = discord_command_slug(&boundary);
        assert!(slug.len() <= 32 && !slug.ends_with('-'));
    }

    #[test]
    fn specs_require_the_slash_tag_and_unique_slugs() {
        let skills = vec![
            skill("deploy status", "Check deploy state", &["slash"]),
            skill("not exposed", "No tag, no command", &[]),
            skill("Deploy Status", "Colliding slug", &["slash"]),
            skill("ask", "Reserved name", &["slash"]),
            skill("no-desc", "", &["slash"]),
            skill(
                "community",
                "Synced from a remote repo",
                &["slash", "open-skills"],
            ),
        ];
        let specs = discord_slash_specs_from_skills(&skills);
        let slugs: Vec<&str> = specs.iter().map(|s| s.slug.as_str()).collect();
        // Sorted, deduped, reserved + untagged + open-skills excluded.
        assert_eq!(slugs, vec!["deploy-status", "no-desc"]);
        assert_eq!(specs[1].description, "Run the no-desc skill");
    }

    #[test]
    fn specs_are_deterministic_regardless_of_input_order() {
        let a = vec![
            skill("bravo", "b", &["slash"]),
            skill("alpha", "a", &["slash"]),
        ];
        let b = vec![
            skill("alpha", "a", &["slash"]),
            skill("bravo", "b", &["slash"]),
        ];
        assert_eq!(
            discord_slash_specs_from_skills(&a),
            discord_slash_specs_from_skills(&b)
        );
    }

    #[test]
    fn specs_cap_at_the_registration_limit() {
        let many: Vec<_> = (0..95)
            .map(|i| skill(&format!("skill-{i:03}"), "d", &["slash"]))
            .collect();
        let specs = discord_slash_specs_from_skills(&many);
        assert_eq!(specs.len(), MAX_SKILL_SLASH_COMMANDS);
    }

    #[test]
    fn specs_sanitize_names_that_enter_the_synthesized_prompt() {
        let skills = vec![skill("evil'\nname", "d", &["slash"])];
        let specs = discord_slash_specs_from_skills(&skills);
        assert!(!specs[0].skill_name.contains('\''));
        assert!(!specs[0].skill_name.contains('\n'));
    }

    #[test]
    fn registration_body_contains_ask_plus_skill_commands() {
        let specs = vec![DiscordSlashCommandSpec {
            skill_name: "deploy status".to_string(),
            slug: "deploy-status".to_string(),
            description: "Check deploy state".to_string(),
            options: Vec::new(),
        }];
        let body = slash_command_registration_body(&specs);
        let commands = body.as_array().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0]["name"], "ask");
        assert_eq!(commands[1]["name"], "deploy-status");
        assert_eq!(commands[1]["options"][0]["name"], "input");
        assert_eq!(commands[1]["options"][0]["required"], true);
        // Every desired command matches the ownership fingerprint except
        // /ask (whose option is `prompt`) — exactly the reaping contract.
        assert!(!is_skill_command_shape(&commands[0]));
        assert!(is_skill_command_shape(&commands[1]));
    }

    #[test]
    fn foreign_commands_do_not_match_the_skill_shape() {
        // No options at all.
        assert!(!is_skill_command_shape(&serde_json::json!({"name": "x"})));
        // Multiple options.
        assert!(!is_skill_command_shape(&serde_json::json!({
            "name": "x",
            "options": [
                {"name": "input", "type": 3, "required": true},
                {"name": "more", "type": 3, "required": false}
            ]
        })));
        // Right shape, wrong option name.
        assert!(!is_skill_command_shape(&serde_json::json!({
            "name": "x",
            "options": [{"name": "query", "type": 3, "required": true}]
        })));
        // The critical foreign-collision case: a generic `/x <input>`
        // command registered by other tooling. Structure matches, but the
        // ownership marker (our exact option description) does not — it
        // must never be reaped.
        assert!(!is_skill_command_shape(&serde_json::json!({
            "name": "x",
            "options": [{
                "name": "input", "type": 3, "required": true,
                "description": "what to run"
            }]
        })));
        // Our own marker matches.
        assert!(is_skill_command_shape(&serde_json::json!({
            "name": "x",
            "options": [{
                "name": "input", "type": 3, "required": true,
                "description": SKILL_COMMAND_OPTION_DESCRIPTION
            }]
        })));
    }

    fn stale_skill_command(id: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": name, "description": "d", "type": 1,
            "options": [{
                "name": "input", "type": 3, "required": true,
                "description": SKILL_COMMAND_OPTION_DESCRIPTION
            }]
        })
    }

    #[tokio::test]
    async fn reconcile_fails_when_a_stale_delete_fails() {
        // A transiently failing DELETE of an owned stale command must make
        // the whole reconcile report Err — otherwise the caller records the
        // fingerprint as successful and the stale command is never retried
        // while the desired set stays unchanged.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/applications/app1/commands"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                stale_skill_command("c1", "ghost-skill")
            ])))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/applications/app1/commands/c1"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        // Desired set: /ask only (the upsert must still be attempted and
        // succeed even though the delete fails).
        Mock::given(method("POST"))
            .and(path("/applications/app1/commands"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let desired = slash_command_registration_body(&[]);
        let err = reconcile_slash_commands(&client, "tok", "app1", &desired, &server.uri())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stale skill command delete"));
    }

    #[tokio::test]
    async fn reconcile_treats_delete_404_as_already_gone() {
        // 404 means the command is already gone (raced cleanup) — the
        // desired end state holds, so the pass records as successful.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/applications/app1/commands"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                stale_skill_command("c1", "ghost-skill")
            ])))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/applications/app1/commands/c1"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/applications/app1/commands"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let desired = slash_command_registration_body(&[]);
        reconcile_slash_commands(&client, "tok", "app1", &desired, &server.uri())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reconcile_skips_unchanged_and_spares_foreign_commands() {
        // Steady state: existing /ask matches the desired projection (no
        // POST), and a foreign command with a generic input option is left
        // alone. Zero writes.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let existing_ask = serde_json::json!({
            "id": "a1", "name": "ask",
            "description": "Ask the agent a question", "type": 1,
            "options": [{
                "name": "prompt", "description": "What to ask",
                "type": 3, "required": true
            }]
        });
        let foreign = serde_json::json!({
            "id": "f1", "name": "run",
            "description": "external tool", "type": 1,
            "options": [{
                "name": "input", "type": 3, "required": true,
                "description": "what to run"
            }]
        });
        Mock::given(method("GET"))
            .and(path("/applications/app1/commands"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([existing_ask, foreign])),
            )
            .expect(1)
            .mount(&server)
            .await;
        // No DELETE and no POST expectations mounted: any write request
        // would 404 the mock server and fail the reconcile.

        let client = reqwest::Client::new();
        let desired = slash_command_registration_body(&[]);
        reconcile_slash_commands(&client, "tok", "app1", &desired, &server.uri())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reconcile_returns_rate_limited_on_post_429() {
        // A 429 on an upsert must surface as RateLimited (with the body's
        // retry_after deadline) so the caller persists a cooldown instead of
        // re-hammering the daily command budget on the next READY.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/applications/app1/commands"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/applications/app1/commands"))
            .respond_with(
                ResponseTemplate::new(429).set_body_json(serde_json::json!({"retry_after": 5.0})),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let desired = slash_command_registration_body(&[]); // /ask → one POST
        let now = crate::discord_slash_state::now_unix();
        let outcome = reconcile_slash_commands(&client, "tok", "app1", &desired, &server.uri())
            .await
            .unwrap();
        match outcome {
            ReconcileOutcome::RateLimited { until } => assert!(until >= now + 5),
            ReconcileOutcome::Reconciled => panic!("expected RateLimited on a POST 429"),
        }
    }

    #[test]
    fn command_projection_ignores_server_side_decorations() {
        // Discord's GET response decorates commands with id/version/etc.;
        // change detection must compare only what we author.
        let ours = serde_json::json!({
            "name": "deploy-status",
            "description": "Check deploy state",
            "type": 1,
            "options": [{
                "name": "input", "type": 3, "required": true,
                "description": SKILL_COMMAND_OPTION_DESCRIPTION
            }]
        });
        let theirs = serde_json::json!({
            "id": "1234", "version": "5678", "application_id": "42",
            "default_member_permissions": serde_json::Value::Null,
            "name": "deploy-status",
            "description": "Check deploy state",
            "type": 1,
            "options": [{
                "name": "input", "type": 3, "required": true,
                "description": SKILL_COMMAND_OPTION_DESCRIPTION
            }]
        });
        assert_eq!(command_projection(&ours), command_projection(&theirs));

        let changed = serde_json::json!({
            "name": "deploy-status",
            "description": "A different description",
            "type": 1,
            "options": ours["options"].clone()
        });
        assert_ne!(command_projection(&ours), command_projection(&changed));
    }

    #[test]
    fn string_options_extract_by_name() {
        let d = serde_json::json!({
            "data": {
                "options": [
                    {"name": "input", "value": "check prod"},
                    {"name": "other", "value": "x"}
                ]
            }
        });
        assert_eq!(interaction_string_option(&d, "input"), "check prod");
        assert_eq!(interaction_string_option(&d, "missing"), "");
    }

    #[test]
    fn channel_resolves_skill_commands_through_resolver() {
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        )
        .with_slash_command_resolver(Arc::new(|| {
            discord_slash_specs_from_skills(&[skill("deploy status", "Check", &["slash"])])
        }));
        let specs = ch.slash_command_resolver.as_ref().map(|r| r()).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].slug, "deploy-status");
    }

    #[test]
    fn pending_interaction_sweep_drops_expired_entries() {
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        );
        let mut guard = ch.pending_interactions.lock();
        guard.insert(
            "live".into(),
            PendingInteraction {
                app_id: "a".into(),
                token: "t".into(),
                created: std::time::Instant::now(),
            },
        );
        guard.insert(
            "stale".into(),
            PendingInteraction {
                app_id: "a".into(),
                token: "t".into(),
                created: std::time::Instant::now()
                    - INTERACTION_TOKEN_TTL
                    - std::time::Duration::from_secs(1),
            },
        );
        guard.retain(|_, p| p.created.elapsed() < INTERACTION_TOKEN_TTL);
        assert!(guard.contains_key("live"));
        assert!(!guard.contains_key("stale"));
    }

    #[test]
    fn discord_channel_name() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        assert_eq!(ch.name(), "discord");
    }

    /// (channel, archive) pair backed by a throwaway sqlite file, mirroring
    /// the orchestrator's `with_archive_memory` wiring.
    fn archived_test_channel() -> (
        DiscordChannel,
        std::sync::Arc<dyn zeroclaw_memory::Memory>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mem: std::sync::Arc<dyn zeroclaw_memory::Memory> = std::sync::Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", dir.path(), "discord").unwrap(),
        );
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["*".to_string()]),
            false,
            false,
        )
        .with_archive_memory(std::sync::Arc::clone(&mem));
        (ch, mem, dir)
    }

    async fn seed_archived_message(mem: &std::sync::Arc<dyn zeroclaw_memory::Memory>) {
        mem.store(
            "discord_111",
            "@alice in #200 at t0: original text",
            zeroclaw_memory::MemoryCategory::Custom("discord".to_string()),
            Some("200"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn message_update_appends_edit_marker_to_archived_entry() {
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;

        let d = serde_json::json!({
            "id": "111", "channel_id": "200", "content": "revised text",
            "edited_timestamp": "2026-06-11T01:00:00Z",
            "author": {"id": "u-alice", "bot": false}
        });
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
            .await;

        let entry = mem.get("discord_111").await.unwrap().unwrap();
        assert!(
            entry
                .content
                .starts_with("@alice in #200 at t0: original text")
        );
        assert!(
            entry
                .content
                .contains("[edited at 2026-06-11T01:00:00Z: revised text]")
        );
        // Session attribution survives the re-store.
        assert_eq!(entry.session_id.as_deref(), Some("200"));
    }

    #[tokio::test]
    async fn redelivered_update_is_idempotent() {
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;
        let d = serde_json::json!({
            "id": "111", "channel_id": "200", "content": "revised text",
            "edited_timestamp": "2026-06-11T01:00:00Z",
            "author": {"id": "u-alice", "bot": false}
        });
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
            .await;
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
            .await;
        let entry = mem.get("discord_111").await.unwrap().unwrap();
        assert_eq!(entry.content.matches("[edited at ").count(), 1);
    }

    #[tokio::test]
    async fn full_object_update_without_edited_timestamp_is_not_an_edit() {
        // Discord sends the complete message object (content included,
        // unchanged) on embed unfurls, pins, and flag changes — only real
        // edits carry edited_timestamp. No phantom markers.
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;
        let d = serde_json::json!({
            "id": "111", "channel_id": "200", "content": "original text",
            "edited_timestamp": serde_json::Value::Null,
            "author": {"id": "u-alice", "bot": false}
        });
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
            .await;
        let entry = mem.get("discord_111").await.unwrap().unwrap();
        assert_eq!(entry.content, "@alice in #200 at t0: original text");
    }

    #[tokio::test]
    async fn deauthorized_author_cannot_write_via_edits() {
        // Archive-time authorization is not durable: once a peer leaves
        // the allowlist their edits must stop reaching the archive.
        let dir = tempfile::tempdir().unwrap();
        let mem: std::sync::Arc<dyn zeroclaw_memory::Memory> = std::sync::Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", dir.path(), "discord").unwrap(),
        );
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["someone-else".to_string()]),
            false,
            false,
        )
        .with_archive_memory(std::sync::Arc::clone(&mem));
        seed_archived_message(&mem).await;

        let d = serde_json::json!({
            "id": "111", "channel_id": "200", "content": "injected",
            "edited_timestamp": "2026-06-11T01:00:00Z",
            "author": {"id": "u-alice", "bot": false}
        });
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
            .await;
        let entry = mem.get("discord_111").await.unwrap().unwrap();
        assert_eq!(entry.content, "@alice in #200 at t0: original text");
    }

    #[tokio::test]
    async fn message_delete_appends_tombstone_once() {
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;

        let d = serde_json::json!({"id": "111", "channel_id": "200"});
        ch.sync_archive_for_message_event("MESSAGE_DELETE", &d, "botid")
            .await;
        // Redelivery must not double-stamp.
        ch.sync_archive_for_message_event("MESSAGE_DELETE", &d, "botid")
            .await;

        let entry = mem.get("discord_111").await.unwrap().unwrap();
        assert!(
            entry
                .content
                .starts_with("@alice in #200 at t0: original text")
        );
        assert_eq!(entry.content.matches("[deleted at ").count(), 1);
        assert_eq!(entry.session_id.as_deref(), Some("200"));
    }

    #[tokio::test]
    async fn bulk_delete_tombstones_every_archived_id() {
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;
        mem.store(
            "discord_112",
            "@bob in #200 at t1: second message",
            zeroclaw_memory::MemoryCategory::Custom("discord".to_string()),
            Some("200"),
        )
        .await
        .unwrap();

        let d = serde_json::json!({"ids": ["111", "112", "999"], "channel_id": "200"});
        ch.sync_archive_for_message_event("MESSAGE_DELETE_BULK", &d, "botid")
            .await;

        assert!(
            mem.get("discord_111")
                .await
                .unwrap()
                .unwrap()
                .content
                .contains("[deleted at ")
        );
        assert!(
            mem.get("discord_112")
                .await
                .unwrap()
                .unwrap()
                .content
                .contains("[deleted at ")
        );
        // Unarchived ids stay unarchived.
        assert!(mem.get("discord_999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn edit_then_delete_keeps_both_markers() {
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;
        let edit = serde_json::json!({
            "id": "111", "channel_id": "200", "content": "revised",
            "edited_timestamp": "2026-06-11T01:00:00Z",
            "author": {"id": "u-alice", "bot": false}
        });
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &edit, "botid")
            .await;
        let del = serde_json::json!({"id": "111", "channel_id": "200"});
        ch.sync_archive_for_message_event("MESSAGE_DELETE", &del, "botid")
            .await;
        let entry = mem.get("discord_111").await.unwrap().unwrap();
        assert!(entry.content.contains("[edited at "));
        assert!(entry.content.contains("[deleted at "));
    }

    #[tokio::test]
    async fn message_events_for_unarchived_messages_are_ignored() {
        // A message that never passed the inbound filters was never stored;
        // its edit/delete events must not conjure an archive entry.
        let (ch, mem, _dir) = archived_test_channel();

        let d = serde_json::json!({
            "id": "999", "channel_id": "200", "content": "whatever",
            "edited_timestamp": "2026-06-11T01:00:00Z",
            "author": {"id": "u-alice", "bot": false}
        });
        ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
            .await;
        ch.sync_archive_for_message_event("MESSAGE_DELETE", &d, "botid")
            .await;

        assert!(mem.get("discord_999").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn edit_history_growth_is_bounded() {
        let (ch, mem, _dir) = archived_test_channel();
        seed_archived_message(&mem).await;
        let big = "z".repeat(4000);
        for i in 0..10 {
            let d = serde_json::json!({
                "id": "111", "channel_id": "200",
                "content": format!("{big}-{i}"),
                "edited_timestamp": format!("2026-06-11T01:00:{i:02}Z"),
                "author": {"id": "u-alice", "bot": false}
            });
            ch.sync_archive_for_message_event("MESSAGE_UPDATE", &d, "botid")
                .await;
        }
        let entry = mem.get("discord_111").await.unwrap().unwrap();
        // Bounded: cap plus at most one marker's overshoot.
        assert!(entry.content.len() < MAX_ARCHIVE_ENTRY_BYTES + 5000);
        assert!(entry.content.contains("[edit history truncated]"));
        assert!(
            entry
                .content
                .starts_with("@alice in #200 at t0: original text")
        );
        // The latest edit always survives.
        assert!(entry.content.contains("01:00:09Z"));
    }

    #[test]
    fn gateway_intents_default_matches_legacy_mask() {
        // The pre-resolver IDENTIFY hardcoded 37377. A default-config channel
        // must request exactly the same mask — no silent behavior change.
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        );
        assert_eq!(ch.gateway_intents(), 37377);
        assert_eq!(ch.gateway_intents(), BASELINE_INTENTS);
    }

    #[test]
    fn intent_names_decode_the_mask() {
        assert_eq!(
            intent_names(BASELINE_INTENTS),
            vec![
                "guilds",
                "guild_messages",
                "direct_messages",
                "message_content"
            ]
        );
        assert_eq!(
            intent_names(BASELINE_INTENTS | INTENT_GUILD_MEMBERS | INTENT_GUILD_PRESENCES),
            vec![
                "guilds",
                "guild_members",
                "guild_presences",
                "guild_messages",
                "direct_messages",
                "message_content"
            ]
        );
        // Bits with no known name (reachable via the raw override) are
        // reported, not dropped.
        assert_eq!(
            intent_names(INTENT_GUILDS | (1 << 21)),
            vec!["guilds".to_string(), format!("unknown({:#x})", 1u64 << 21)]
        );
    }

    #[test]
    fn disallowed_intents_hint_names_privileged_toggles() {
        let hint = disallowed_intents_hint(BASELINE_INTENTS | INTENT_GUILD_MEMBERS);
        assert!(hint.contains("Server Members"));
        assert!(hint.contains("Message Content"));
        assert!(!hint.contains("Presence,"));

        let base_hint = disallowed_intents_hint(BASELINE_INTENTS);
        assert!(base_hint.contains("Message Content"));
        assert!(!base_hint.contains("Server Members"));

        // An override mask with no privileged bits still produces an
        // actionable message instead of a dangling empty list.
        let bare = disallowed_intents_hint(INTENT_GUILDS);
        assert!(bare.contains("mask 0x1"));
    }

    use zeroclaw_config::schema::DiscordReactionScope;

    fn reaction_test_channel(
        scope: DiscordReactionScope,
    ) -> (
        DiscordChannel,
        std::sync::Arc<dyn zeroclaw_memory::Memory>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mem: std::sync::Arc<dyn zeroclaw_memory::Memory> = std::sync::Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", dir.path(), "discord").unwrap(),
        );
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["*".to_string()]),
            false,
            false,
        )
        .with_archive_memory(std::sync::Arc::clone(&mem))
        .with_reaction_notifications(scope);
        (ch, mem, dir)
    }

    #[test]
    fn reaction_scope_adds_reaction_intents_to_the_mask() {
        let (off, _m1, _d1) = reaction_test_channel(DiscordReactionScope::Off);
        assert_eq!(off.gateway_intents(), 37377);

        let (own, _m2, _d2) = reaction_test_channel(DiscordReactionScope::Own);
        assert_eq!(
            own.gateway_intents(),
            37377 | INTENT_GUILD_MESSAGE_REACTIONS | INTENT_DIRECT_MESSAGE_REACTIONS
        );
        // The reactions-on mask is OpenClaw's static base — parity by arithmetic.
        assert_eq!(own.gateway_intents(), 46593);
    }

    #[tokio::test]
    async fn reaction_add_is_archived_and_remove_forgets_it() {
        let (ch, mem, _dir) = reaction_test_channel(DiscordReactionScope::All);
        let add = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "👍"},
            "member": {"user": {"username": "bob"}}
        });
        ch.handle_reaction_event("MESSAGE_REACTION_ADD", &add, "botid")
            .await;

        let key = "discord_reaction_m1_u1_👍";
        let entry = mem.get(key).await.unwrap().unwrap();
        assert!(
            entry
                .content
                .contains("@bob reacted 👍 to message m1 in #c1")
        );

        let remove = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "👍"}
        });
        ch.handle_reaction_event("MESSAGE_REACTION_REMOVE", &remove, "botid")
            .await;
        assert!(mem.get(key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn own_scope_only_records_reactions_to_bot_messages() {
        let (ch, mem, _dir) = reaction_test_channel(DiscordReactionScope::Own);

        let to_other = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "👍"},
            "message_author_id": "someone_else"
        });
        ch.handle_reaction_event("MESSAGE_REACTION_ADD", &to_other, "botid")
            .await;
        assert!(
            mem.get("discord_reaction_m1_u1_👍")
                .await
                .unwrap()
                .is_none()
        );

        let to_bot = serde_json::json!({
            "user_id": "u1", "message_id": "m2", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "🎉"},
            "message_author_id": "botid"
        });
        ch.handle_reaction_event("MESSAGE_REACTION_ADD", &to_bot, "botid")
            .await;
        assert!(
            mem.get("discord_reaction_m2_u1_🎉")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn bots_own_reactions_are_never_recorded() {
        // Ack/failure emoji the bot adds itself echo back as gateway events.
        let (ch, mem, _dir) = reaction_test_channel(DiscordReactionScope::All);
        let own_ack = serde_json::json!({
            "user_id": "botid", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "⚡️"},
            "message_author_id": "u1"
        });
        ch.handle_reaction_event("MESSAGE_REACTION_ADD", &own_ack, "botid")
            .await;
        assert!(
            mem.get("discord_reaction_m1_botid_⚡️")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn custom_emoji_keys_by_stable_id_so_rename_cannot_orphan_entries() {
        // Discord sends {id, name} for custom emoji; the name is mutable
        // guild state. ADD records by id, and a REMOVE arriving after the
        // emoji was renamed or deleted (name: null) must still forget it.
        let (ch, mem, _dir) = reaction_test_channel(DiscordReactionScope::All);
        let add = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1",
            "emoji": {"id": "424242", "name": "partyclaw"}
        });
        ch.handle_reaction_event("MESSAGE_REACTION_ADD", &add, "botid")
            .await;
        let entry = mem.get("discord_reaction_m1_u1_424242").await.unwrap();
        // Content keeps the human-readable name; the key uses the id.
        assert!(entry.unwrap().content.contains("reacted partyclaw"));

        let remove = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1",
            "emoji": {"id": "424242", "name": serde_json::Value::Null}
        });
        ch.handle_reaction_event("MESSAGE_REACTION_REMOVE", &remove, "botid")
            .await;
        assert!(
            mem.get("discord_reaction_m1_u1_424242")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn identityless_emoji_are_ignored() {
        // No id and no name: nothing meaningful to record (or forget).
        let (ch, mem, _dir) = reaction_test_channel(DiscordReactionScope::All);
        let add = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {}
        });
        ch.handle_reaction_event("MESSAGE_REACTION_ADD", &add, "botid")
            .await;
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn reaction_filters_mirror_the_message_path() {
        // Peer allowlist: a reactor outside the peer set is never recorded.
        let dir = tempfile::tempdir().unwrap();
        let mem: std::sync::Arc<dyn zeroclaw_memory::Memory> = std::sync::Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", dir.path(), "discord").unwrap(),
        );
        let gated = DiscordChannel::new(
            "fake".into(),
            vec!["g1".into()],
            "discord_test_alias",
            Arc::new(|| vec!["friend".to_string()]),
            false,
            false,
        )
        .with_channel_ids(vec!["c1".into()])
        .with_archive_memory(std::sync::Arc::clone(&mem))
        .with_reaction_notifications(DiscordReactionScope::All);

        let stranger = serde_json::json!({
            "user_id": "stranger", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "👍"}
        });
        gated
            .handle_reaction_event("MESSAGE_REACTION_ADD", &stranger, "botid")
            .await;
        assert_eq!(mem.count().await.unwrap(), 0);

        // Guild allowlist: wrong guild is dropped even for an allowed peer.
        let wrong_guild = serde_json::json!({
            "user_id": "friend", "message_id": "m2", "channel_id": "c1",
            "guild_id": "g2", "emoji": {"name": "👍"}
        });
        gated
            .handle_reaction_event("MESSAGE_REACTION_ADD", &wrong_guild, "botid")
            .await;
        assert_eq!(mem.count().await.unwrap(), 0);

        // All gates pass: recorded.
        let ok = serde_json::json!({
            "user_id": "friend", "message_id": "m3", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "👍"}
        });
        gated
            .handle_reaction_event("MESSAGE_REACTION_ADD", &ok, "botid")
            .await;
        assert!(
            mem.get("discord_reaction_m3_friend_👍")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn remove_without_prior_add_is_a_noop() {
        let (ch, mem, _dir) = reaction_test_channel(DiscordReactionScope::All);
        let remove = serde_json::json!({
            "user_id": "u1", "message_id": "m1", "channel_id": "c1",
            "guild_id": "g1", "emoji": {"name": "👍"}
        });
        ch.handle_reaction_event("MESSAGE_REACTION_REMOVE", &remove, "botid")
            .await;
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[test]
    fn intents_mask_override_wins_verbatim() {
        // Operator escape hatch: a Some(_) intents_mask is sent exactly as
        // configured, ignoring the derived baseline — including Some(0),
        // which is a legal IDENTIFY value.
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        )
        .with_intents_mask(Some(46593));
        assert_eq!(ch.gateway_intents(), 46593);

        let zero = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        )
        .with_intents_mask(Some(0));
        assert_eq!(zero.gateway_intents(), 0);

        // None means "derive".
        let derived = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            false,
            false,
        )
        .with_intents_mask(None);
        assert_eq!(derived.gateway_intents(), BASELINE_INTENTS);
    }

    #[test]
    fn base64_decode_bot_id() {
        // "MTIzNDU2" decodes to "123456"
        let decoded = base64_decode("MTIzNDU2");
        assert_eq!(decoded, Some("123456".to_string()));
    }

    #[test]
    fn bot_user_id_extraction() {
        // Token format: base64(user_id).timestamp.hmac
        let token = "MTIzNDU2.fake.hmac";
        let id = DiscordChannel::bot_user_id_from_token(token);
        assert_eq!(id, Some("123456".to_string()));
    }

    #[test]
    fn gateway_preflight_429_remains_retryable_http_error() {
        let response = reqwest::Response::from(
            axum::http::Response::builder()
                .status(reqwest::StatusCode::TOO_MANY_REQUESTS)
                .header(reqwest::header::RETRY_AFTER, "1")
                .body(reqwest::Body::from(""))
                .expect("test response should build"),
        );

        let error = DiscordChannel::validate_gateway_preflight_response(response)
            .expect_err("429 should remain an HTTP error");
        assert!(error.downcast_ref::<reqwest::Error>().is_some());
        assert!(
            error.downcast_ref::<DiscordListenerFatalError>().is_none(),
            "gateway preflight 429 must not be wrapped as fatal"
        );
        assert!(
            !zeroclaw_providers::reliable::is_non_retryable(&error),
            "gateway preflight 429 should stay on the supervisor retry path"
        );
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        assert!(!ch.is_user_allowed("12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["*".into()]),
            listen_to_bots,
            mention_only,
        );
        assert!(ch.is_user_allowed("12345"));
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["111".into(), "222".into()]),
            listen_to_bots,
            mention_only,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("222"));
        assert!(!ch.is_user_allowed("333"));
        assert!(!ch.is_user_allowed("unknown"));
    }

    #[test]
    fn allowlist_is_exact_match_not_substring() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["111".into()]),
            listen_to_bots,
            mention_only,
        );
        assert!(!ch.is_user_allowed("1111"));
        assert!(!ch.is_user_allowed("11"));
        assert!(!ch.is_user_allowed("0111"));
    }

    #[test]
    fn allowlist_empty_string_user_id() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["111".into()]),
            listen_to_bots,
            mention_only,
        );
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_with_wildcard_and_specific() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["111".into(), "*".into()]),
            listen_to_bots,
            mention_only,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("anyone_else"));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(|| vec!["ABC".into()]),
            listen_to_bots,
            mention_only,
        );
        assert!(ch.is_user_allowed("ABC"));
        assert!(!ch.is_user_allowed("abc"));
        assert!(!ch.is_user_allowed("Abc"));
    }

    #[test]
    fn base64_decode_empty_string() {
        let decoded = base64_decode("");
        assert_eq!(decoded, Some(String::new()));
    }

    #[test]
    fn fatal_gateway_close_codes_match_expected_discord_auth_and_intent_errors() {
        for code in [4004_u16, 4010, 4011, 4012, 4013, 4014] {
            assert!(
                is_fatal_gateway_close_code(code),
                "code {code} should be fatal"
            );
        }
        assert!(!is_fatal_gateway_close_code(4007));
        assert!(!is_fatal_gateway_close_code(4009));
    }

    #[test]
    fn new_session_close_codes_match_invalidated_gateway_sessions() {
        assert!(requires_new_session_close_code(4007));
        assert!(requires_new_session_close_code(4009));
        assert!(!requires_new_session_close_code(4004));
    }

    #[test]
    fn base64_decode_invalid_chars() {
        let decoded = base64_decode("!!!!");
        assert!(decoded.is_none());
    }

    #[test]
    fn bot_user_id_from_empty_token() {
        let id = DiscordChannel::bot_user_id_from_token("");
        assert_eq!(id, Some(String::new()));
    }

    #[test]
    fn contains_bot_mention_supports_plain_and_nick_forms() {
        assert!(contains_bot_mention("hi <@12345>", "12345"));
        assert!(contains_bot_mention("hi <@!12345>", "12345"));
        assert!(!contains_bot_mention("hi <@99999>", "12345"));
    }

    #[test]
    fn thread_created_and_system_messages_are_not_conversational() {
        // The bug: THREAD_CREATED (18) was treated as a normal message, so the
        // bot replied to a thread's birth. It and other system types must be
        // rejected; only DEFAULT (0) and REPLY (19) are real user turns.
        assert!(is_conversational_message_type(0)); // DEFAULT
        assert!(is_conversational_message_type(19)); // REPLY
        assert!(!is_conversational_message_type(18)); // THREAD_CREATED
        assert!(!is_conversational_message_type(21)); // THREAD_STARTER_MESSAGE
        assert!(!is_conversational_message_type(6)); // CHANNEL_PINNED_MESSAGE
        assert!(!is_conversational_message_type(7)); // USER_JOIN
    }

    #[test]
    fn admit_discord_message_requires_mention_when_enabled() {
        let cleaned = admit_discord_message("hello there", false, true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn admit_discord_message_preserves_mention_in_body() {
        let cleaned = admit_discord_message("  <@!12345> run status  ", false, true, "12345");
        assert_eq!(cleaned.as_deref(), Some("<@!12345> run status"));
    }

    #[test]
    fn admit_discord_message_admits_caption_that_is_only_the_mention() {
        let cleaned = admit_discord_message("<@12345>", false, true, "12345");
        assert_eq!(cleaned.as_deref(), Some("<@12345>"));
    }

    #[test]
    fn admit_discord_message_attachment_only_in_dm_is_admitted() {
        // DM (effective_mention_only=false), empty text body, at least one
        // attachment. Previously dropped at the empty-text gate; now passes
        // through so process_attachments can run on the media.
        let cleaned = admit_discord_message("", true, false, "12345");
        assert_eq!(cleaned.as_deref(), Some(""));
    }

    #[test]
    fn admit_discord_message_attachment_only_with_mention_in_guild_is_admitted() {
        // Guild channel with mention_only=true. Caption is the @mention tag
        // and the message has a media attachment. Mention gate passes; the
        // body keeps the mention text so downstream code (and the agent it
        // routes to) can see who was addressed.
        let cleaned = admit_discord_message("<@12345>", true, true, "12345");
        assert_eq!(cleaned.as_deref(), Some("<@12345>"));
    }

    #[test]
    fn admit_discord_message_attachment_only_without_mention_in_guild_is_rejected() {
        // Guild channel with mention_only=true, attachment but no mention
        // anywhere in the caption. The mention gate is orthogonal to
        // attachment presence: no mention signal means drop.
        let cleaned = admit_discord_message("", true, true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn admit_discord_message_drops_when_no_text_and_no_attachments() {
        // Completely empty payload with attachments absent is always dropped,
        // regardless of mention_only setting.
        assert!(admit_discord_message("", false, false, "12345").is_none());
        assert!(admit_discord_message("", false, true, "12345").is_none());
    }

    // mention_only DM-bypass tests

    #[test]
    fn mention_only_dm_bypasses_mention_gate() {
        // DMs (no guild_id) must pass through even when mention_only is true
        // and the message contains no @mention. Mirrors the listen call-site logic.
        let mention_only = true;
        let is_dm = true;
        let effective = mention_only && !is_dm;
        let cleaned = admit_discord_message("hello without mention", false, effective, "12345");
        assert_eq!(cleaned.as_deref(), Some("hello without mention"));
    }

    #[test]
    fn mention_only_guild_message_without_mention_is_rejected() {
        // Guild messages (has guild_id, so is_dm = false) must still be rejected
        // when mention_only is true and the message contains no @mention.
        let mention_only = true;
        let is_dm = false;
        let effective = mention_only && !is_dm;
        let cleaned = admit_discord_message("hello without mention", false, effective, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn mention_only_guild_message_with_mention_passes_through() {
        // Guild messages that carry a @mention pass through the gate with
        // the mention text preserved so downstream consumers (and the agent
        // it routes to) can see who was addressed.
        let mention_only = true;
        let is_dm = false;
        let effective = mention_only && !is_dm;
        let cleaned = admit_discord_message("<@12345> run status", false, effective, "12345");
        assert_eq!(cleaned.as_deref(), Some("<@12345> run status"));
    }

    // Message splitting tests

    #[test]
    fn split_empty_message() {
        let chunks = split_message_for_discord("");
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn split_short_message_under_limit() {
        let msg = "Hello, world!";
        let chunks = split_message_for_discord(msg);
        assert_eq!(chunks, vec![msg]);
    }

    #[test]
    fn split_message_exactly_2000_chars() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_message_just_over_limit() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn split_very_long_message() {
        let msg = "word ".repeat(2000); // 10000 characters (5 chars per "word ")
        let chunks = split_message_for_discord(&msg);
        // Should split into 5 chunks of <= 2000 chars
        assert_eq!(chunks.len(), 5);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH)
        );
        // Verify total content is preserved
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_prefer_newline_break() {
        let msg = format!("{}\n{}", "a".repeat(1500), "b".repeat(500));
        let chunks = split_message_for_discord(&msg);
        // Should split at the newline
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
        assert!(chunks[1].starts_with('b'));
    }

    #[test]
    fn split_prefer_space_break() {
        let msg = format!("{} {}", "a".repeat(1500), "b".repeat(600));
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_without_good_break_points_hard_split() {
        // No spaces or newlines - should hard split at 2000
        let msg = "a".repeat(5000);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[2].chars().count(), 1000);
    }

    #[test]
    fn split_multiple_breaks() {
        // Create a message with multiple newlines
        let part1 = "a".repeat(900);
        let part2 = "b".repeat(900);
        let part3 = "c".repeat(900);
        let msg = format!("{part1}\n{part2}\n{part3}");
        let chunks = split_message_for_discord(&msg);
        // Should split into 2 chunks (first two parts + third part)
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        assert!(chunks[1].chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_preserves_content() {
        let original = "Hello world! This is a test message with some content. ".repeat(200);
        let chunks = split_message_for_discord(&original);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, original);
    }

    #[test]
    fn split_unicode_content() {
        // Test with emoji and multi-byte characters
        let msg = "🦀 Rust is awesome! ".repeat(500);
        let chunks = split_message_for_discord(&msg);
        // All chunks should be valid UTF-8
        for chunk in &chunks {
            assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
            assert!(chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
        // Reconstruct and verify
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_newline_too_close_to_end() {
        // If newline is in the first half, don't use it - use space instead or hard split
        let msg = format!("{}\n{}", "a".repeat(1900), "b".repeat(500));
        let chunks = split_message_for_discord(&msg);
        // Should split at newline since it's in the second half of the window
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_multibyte_only_content_without_panics() {
        let msg = "🦀".repeat(2500);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 500);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_chunks_always_within_discord_limit() {
        let msg = "x".repeat(12_345);
        let chunks = split_message_for_discord(&msg);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH)
        );
    }

    #[test]
    fn split_message_with_multiple_newlines() {
        let msg = "Line 1\nLine 2\nLine 3\n".repeat(1000);
        let chunks = split_message_for_discord(&msg);
        assert!(chunks.len() > 1);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn typing_handles_start_empty() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let guard = ch.typing_handles.lock();
        assert!(guard.is_empty());
    }

    #[tokio::test]
    async fn start_typing_sets_handle() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let _ = ch.start_typing("123456").await;
        let guard = ch.typing_handles.lock();
        assert!(guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_clears_handle() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let _ = ch.start_typing("123456").await;
        let _ = ch.stop_typing("123456").await;
        let guard = ch.typing_handles.lock();
        assert!(!guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_is_idempotent() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        assert!(ch.stop_typing("123456").await.is_ok());
        assert!(ch.stop_typing("123456").await.is_ok());
    }

    #[tokio::test]
    async fn concurrent_typing_handles_are_independent() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "fake".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let _ = ch.start_typing("111").await;
        let _ = ch.start_typing("222").await;
        {
            let guard = ch.typing_handles.lock();
            assert_eq!(guard.len(), 2);
            assert!(guard.contains_key("111"));
            assert!(guard.contains_key("222"));
        }
        // Stopping one does not affect the other
        let _ = ch.stop_typing("111").await;
        let guard = ch.typing_handles.lock();
        assert_eq!(guard.len(), 1);
        assert!(guard.contains_key("222"));
    }

    // ── Emoji encoding for reactions ──────────────────────────────

    #[test]
    fn encode_emoji_unicode_percent_encodes() {
        let encoded = encode_emoji_for_discord("\u{1F440}");
        assert_eq!(encoded, "%F0%9F%91%80");
    }

    #[test]
    fn encode_emoji_checkmark() {
        let encoded = encode_emoji_for_discord("\u{2705}");
        assert_eq!(encoded, "%E2%9C%85");
    }

    #[test]
    fn encode_emoji_custom_guild_emoji_passthrough() {
        let encoded = encode_emoji_for_discord("custom_emoji:123456789");
        assert_eq!(encoded, "custom_emoji:123456789");
    }

    #[test]
    fn encode_emoji_simple_ascii_char() {
        let encoded = encode_emoji_for_discord("A");
        assert_eq!(encoded, "%41");
    }

    #[test]
    fn random_discord_ack_reaction_is_from_pool() {
        for _ in 0..128 {
            let emoji = random_discord_ack_reaction();
            assert!(DISCORD_ACK_REACTIONS.contains(&emoji));
        }
    }

    #[test]
    fn discord_reaction_url_encodes_emoji_and_strips_prefix() {
        let url = discord_reaction_url("123", "discord_456", "👀");
        assert_eq!(
            url,
            "https://discord.com/api/v10/channels/123/messages/456/reactions/%F0%9F%91%80/@me"
        );
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn discord_message_id_format_includes_discord_prefix() {
        // Verify that message IDs follow the format: discord_{message_id}
        let message_id = "123456789012345678";
        let expected_id = format!("discord_{message_id}");
        assert_eq!(expected_id, "discord_123456789012345678");
    }

    #[test]
    fn discord_message_id_is_deterministic() {
        // Same message_id = same ID (prevents duplicates after restart)
        let message_id = "123456789012345678";
        let id1 = format!("discord_{message_id}");
        let id2 = format!("discord_{message_id}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn discord_message_id_different_message_different_id() {
        // Different message IDs produce different IDs
        let id1 = "discord_123456789012345678".to_string();
        let id2 = "discord_987654321098765432".to_string();
        assert_ne!(id1, id2);
    }

    #[test]
    fn discord_message_id_uses_snowflake_id() {
        // Discord snowflake IDs are numeric strings
        let message_id = "123456789012345678"; // Typical snowflake format
        let id = format!("discord_{message_id}");
        assert!(id.starts_with("discord_"));
        // Snowflake IDs are numeric
        assert!(message_id.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn discord_message_id_fallback_to_uuid_on_empty() {
        // Edge case: empty message_id falls back to UUID
        let message_id = "";
        let id = if message_id.is_empty() {
            format!("discord_{}", uuid::Uuid::new_v4())
        } else {
            format!("discord_{message_id}")
        };
        assert!(id.starts_with("discord_"));
        // Should have UUID dashes
        assert!(id.contains('-'));
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG6: Channel platform limit edge cases for Discord (2000 char limit)
    // Prevents: Pattern 6 — issues #574, #499
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn split_message_code_block_at_boundary() {
        // Code block that spans the split boundary
        let mut msg = String::new();
        msg.push_str("```rust\n");
        msg.push_str(&"x".repeat(1990));
        msg.push_str("\n```\nMore text after code block");
        let parts = split_message_for_discord(&msg);
        assert!(
            parts.len() >= 2,
            "code block spanning boundary should split"
        );
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "each part must be <= {DISCORD_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
    }

    #[test]
    fn split_message_single_long_word_exceeds_limit() {
        // A single word longer than 2000 chars must be hard-split
        let long_word = "a".repeat(2500);
        let parts = split_message_for_discord(&long_word);
        assert!(parts.len() >= 2, "word exceeding limit must be split");
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "hard-split part must be <= {DISCORD_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
        // Reassembled content should match original
        let reassembled: String = parts.join("");
        assert_eq!(reassembled, long_word);
    }

    #[test]
    fn split_message_exactly_at_limit_no_split() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let parts = split_message_for_discord(&msg);
        assert_eq!(parts.len(), 1, "message exactly at limit should not split");
        assert_eq!(parts[0].len(), DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_message_one_over_limit_splits() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let parts = split_message_for_discord(&msg);
        assert!(parts.len() >= 2, "message 1 char over limit must split");
    }

    #[test]
    fn split_message_many_short_lines() {
        // Many short lines should be batched into chunks under the limit
        let msg: String = (0..500).fold(String::new(), |mut acc, i| {
            let _ = writeln!(acc, "line {i}");
            acc
        });
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "short-line batch must be <= limit"
            );
        }
        // All content should be preserved
        let reassembled: String = parts.join("");
        assert_eq!(reassembled.trim(), msg.trim());
    }

    #[test]
    fn split_message_only_whitespace() {
        let msg = "   \n\n\t  ";
        let parts = split_message_for_discord(msg);
        // Should handle gracefully without panic
        assert!(parts.len() <= 1);
    }

    #[test]
    fn split_message_emoji_at_boundary() {
        // Emoji are multi-byte; ensure we don't split mid-emoji
        let mut msg = "a".repeat(1998);
        msg.push_str("🎉🎊"); // 2 emoji at the boundary (2000 chars total)
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            // The function splits on character count, not byte count
            assert!(
                part.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH,
                "emoji boundary split must respect limit"
            );
        }
    }

    #[test]
    fn split_message_consecutive_newlines_at_boundary() {
        let mut msg = "a".repeat(1995);
        msg.push_str("\n\n\n\n\n");
        msg.push_str(&"b".repeat(100));
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            assert!(part.len() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
    }

    // process_attachments tests

    #[tokio::test]
    async fn process_attachments_empty_list_returns_empty() {
        let client = reqwest::Client::new();
        let (text, media) = process_attachments(&[], &client, None, None).await;
        assert!(text.is_empty());
        assert!(media.is_empty());
    }

    #[tokio::test]
    async fn process_attachments_preserves_audio_when_transcription_fails() {
        use crate::transcription::TranscriptionManager;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let media_server = MockServer::start().await;
        let whisper_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/voice.ogg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-audio"))
            .expect(1)
            .mount(&media_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .respond_with(
                ResponseTemplate::new(503)
                    .set_body_json(serde_json::json!({"error": "stt unavailable"})),
            )
            .mount(&whisper_server)
            .await;

        let audio_url = format!("{}/voice.ogg", media_server.uri());
        let attachments = vec![serde_json::json!({
            "content_type": "audio/ogg",
            "filename": "voice.ogg",
            "url": audio_url,
        })];
        let transcription =
            TranscriptionManager::new(&local_whisper_transcription_config(&whisper_server))
                .expect("transcription manager")
                .with_agent_transcription_provider("local_whisper");

        let client = reqwest::Client::new();
        let (text, media) =
            process_attachments(&attachments, &client, None, Some(&transcription)).await;

        assert_eq!(
            text,
            format!("[AUDIO:{}]", attachments[0]["url"].as_str().unwrap())
        );
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].file_name, "voice.ogg");
        assert_eq!(media[0].mime_type.as_deref(), Some("audio/ogg"));
        assert_eq!(media[0].data, b"fake-audio");
    }

    #[tokio::test]
    async fn process_attachments_preserves_audio_when_transcription_is_empty() {
        use crate::transcription::TranscriptionManager;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let media_server = MockServer::start().await;
        let whisper_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/voice.ogg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-audio"))
            .expect(1)
            .mount(&media_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": ""})))
            .mount(&whisper_server)
            .await;

        let audio_url = format!("{}/voice.ogg", media_server.uri());
        let attachments = vec![serde_json::json!({
            "content_type": "audio/ogg",
            "filename": "voice.ogg",
            "url": audio_url,
        })];
        let transcription =
            TranscriptionManager::new(&local_whisper_transcription_config(&whisper_server))
                .expect("transcription manager")
                .with_agent_transcription_provider("local_whisper");

        let client = reqwest::Client::new();
        let (text, media) =
            process_attachments(&attachments, &client, None, Some(&transcription)).await;

        assert_eq!(
            text,
            format!("[AUDIO:{}]", attachments[0]["url"].as_str().unwrap())
        );
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].file_name, "voice.ogg");
        assert_eq!(media[0].mime_type.as_deref(), Some("audio/ogg"));
        assert_eq!(media[0].data, b"fake-audio");
    }

    fn local_whisper_transcription_config(
        server: &wiremock::MockServer,
    ) -> zeroclaw_config::schema::TranscriptionConfig {
        zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", server.uri()),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn marker_kind_for_classifies_each_mime_family() {
        assert_eq!(marker_kind_for("image/png", false), "IMAGE");
        assert_eq!(marker_kind_for("image/jpeg", false), "IMAGE");
        assert_eq!(marker_kind_for("video/mp4", false), "VIDEO");
        assert_eq!(marker_kind_for("application/pdf", false), "DOCUMENT");
        assert_eq!(marker_kind_for("application/zip", false), "DOCUMENT");
        assert_eq!(marker_kind_for("", false), "DOCUMENT");
    }

    #[test]
    fn marker_kind_for_treats_audio_flag_as_audio_regardless_of_content_type() {
        // Filename-detected audio with no content_type should still classify
        // as AUDIO, matching the unified inbound pipeline.
        assert_eq!(marker_kind_for("", true), "AUDIO");
        assert_eq!(marker_kind_for("application/octet-stream", true), "AUDIO");
    }

    #[test]
    fn marker_kind_for_prefers_image_over_audio_when_content_type_is_image() {
        // Defensive: if a Discord attachment somehow tripped both heuristics,
        // image MIME wins so vision-capable providers still receive image
        // bytes through the MediaAttachment path.
        assert_eq!(marker_kind_for("image/png", true), "IMAGE");
    }

    #[test]
    fn is_thread_channel_type_matches_only_thread_types() {
        // Thread types per Discord docs: 10/11/12.
        assert!(is_thread_channel_type(10));
        assert!(is_thread_channel_type(11));
        assert!(is_thread_channel_type(12));
        // Non-thread channel types must not be classified as threads.
        for non_thread in [0u64, 1, 2, 3, 4, 5, 13, 14, 15, 16] {
            assert!(
                !is_thread_channel_type(non_thread),
                "type {non_thread} must not classify as thread"
            );
        }
    }

    #[test]
    fn channel_filter_empty_accepts_everything() {
        let filter: Vec<String> = vec![];
        assert!(channel_passes_filter(&filter, "12345", None));
        assert!(channel_passes_filter(&filter, "99999", Some("12345")));
        assert!(channel_passes_filter(&filter, "", None));
    }

    #[test]
    fn channel_filter_direct_match() {
        let filter = vec!["111".to_string(), "222".to_string()];
        assert!(channel_passes_filter(&filter, "111", None));
        assert!(channel_passes_filter(&filter, "222", None));
        assert!(!channel_passes_filter(&filter, "333", None));
    }

    #[test]
    fn channel_filter_thread_parent_fallback() {
        let filter = vec!["111".to_string()];
        // Thread whose parent is in the allowlist — accepted.
        assert!(channel_passes_filter(&filter, "999", Some("111")));
        // Thread whose parent is NOT in the allowlist — rejected.
        assert!(!channel_passes_filter(&filter, "999", Some("888")));
        // Non-thread channel not in the allowlist — rejected.
        assert!(!channel_passes_filter(&filter, "999", None));
    }

    #[test]
    fn channel_filter_direct_match_skips_parent_check() {
        let filter = vec!["111".to_string()];
        // Direct match with a parent_id present — parent is irrelevant.
        assert!(channel_passes_filter(&filter, "111", Some("999")));
    }

    #[test]
    fn parse_attachment_markers_extracts_supported_markers() {
        let input = "Report\n[IMAGE:https://example.com/a.png]\n[DOCUMENT:/tmp/a.pdf]";
        let (cleaned, attachments) = parse_attachment_markers(input);

        assert_eq!(cleaned, "Report");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, DiscordAttachmentKind::Image);
        assert_eq!(attachments[0].target, "https://example.com/a.png");
        assert_eq!(attachments[1].kind, DiscordAttachmentKind::Document);
        assert_eq!(attachments[1].target, "/tmp/a.pdf");
    }

    #[test]
    fn parse_attachment_markers_keeps_invalid_marker_text() {
        let input = "Hello [NOT_A_MARKER:foo] world";
        let (cleaned, attachments) = parse_attachment_markers(input);

        assert_eq!(cleaned, input);
        assert!(attachments.is_empty());
    }

    #[test]
    fn classify_outgoing_attachments_keeps_workspace_locals_and_http() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("image.png");
        std::fs::write(&file_path, b"fake").expect("write fixture");

        let attachments = vec![
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: file_path.to_string_lossy().to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: "https://example.com/remote.png".to_string(),
            },
        ];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert_eq!(locals.len(), 1);
        let canonical_file = std::fs::canonicalize(&file_path).expect("canonicalize fixture");
        assert_eq!(locals[0], canonical_file);
        assert_eq!(remotes, vec!["https://example.com/remote.png".to_string()]);
        assert!(failures.is_empty());
    }

    #[test]
    fn classify_outgoing_attachments_drops_missing_absolute_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Video,
            target: temp
                .path()
                .join("does-not-exist.mp4")
                .to_string_lossy()
                .to_string(),
        }];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert!(locals.is_empty());
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0], DiscordMarkerFailure::NotFound);
    }

    #[test]
    fn classify_outgoing_attachments_drops_paths_outside_workspace() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let outside_file = outside.path().join("escape.png");
        std::fs::write(&outside_file, b"fake").expect("write fixture");

        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Image,
            target: outside_file.to_string_lossy().to_string(),
        }];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(workspace.path()));
        assert!(
            locals.is_empty(),
            "absolute paths outside workspace must be refused"
        );
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0], DiscordMarkerFailure::Refused);
    }

    #[test]
    fn classify_outgoing_attachments_drops_relative_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Document,
            target: "relative/report.pdf".to_string(),
        }];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert!(locals.is_empty(), "relative paths must be refused");
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0], DiscordMarkerFailure::Refused);
    }

    #[test]
    fn classify_outgoing_attachments_drops_disallowed_schemes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachments = vec![
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: "file:///etc/hostname".to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Document,
                target: "data:text/plain;base64,aGk=".to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Video,
                target: "ftp://example.com/clip.mp4".to_string(),
            },
        ];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert!(locals.is_empty());
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 3);
        for kind in &failures {
            assert_eq!(*kind, DiscordMarkerFailure::Refused);
        }
    }

    #[test]
    fn classify_outgoing_attachments_refuses_local_without_workspace() {
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Image,
            target: "/some/absolute/path.png".to_string(),
        }];

        let (locals, remotes, failures) = classify_outgoing_attachments(&attachments, None);
        assert!(
            locals.is_empty(),
            "local paths must be refused without workspace_dir"
        );
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0], DiscordMarkerFailure::Refused);
    }

    #[test]
    fn classify_outgoing_attachments_passes_http_without_workspace() {
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Image,
            target: "https://example.com/x.png".to_string(),
        }];

        let (locals, remotes, failures) = classify_outgoing_attachments(&attachments, None);
        assert!(locals.is_empty());
        assert_eq!(remotes, vec!["https://example.com/x.png".to_string()]);
        assert!(failures.is_empty());
    }

    #[test]
    fn with_inline_attachment_urls_appends_remote_urls_only() {
        let content = "Done";
        let remote_urls = vec!["https://example.com/a.png".to_string()];

        let rendered = with_inline_attachment_urls(content, &remote_urls);
        assert_eq!(rendered, "Done\nhttps://example.com/a.png");
    }

    #[test]
    fn with_inline_attachment_urls_keeps_content_when_no_urls() {
        let rendered = with_inline_attachment_urls("Done", &[]);
        assert_eq!(rendered, "Done");
    }

    #[test]
    fn delivery_failure_note_is_none_when_no_failures() {
        assert!(delivery_failure_note(&[]).is_none());
    }

    #[test]
    fn delivery_failure_note_singular_for_one_failure() {
        let note = delivery_failure_note(&[DiscordMarkerFailure::NotFound])
            .expect("one failure should produce a note");
        assert_eq!(note, "(note: I couldn't deliver 1 file.)");
        assert!(
            !note.contains("/workspace/missing.png"),
            "user-facing failure note must not echo local marker targets"
        );
    }

    #[test]
    fn delivery_failure_note_plural_redacts_targets() {
        let note = delivery_failure_note(&[
            DiscordMarkerFailure::Refused,
            DiscordMarkerFailure::NotFound,
            DiscordMarkerFailure::Refused,
        ])
        .expect("multiple failures should produce a note");
        assert_eq!(note, "(note: I couldn't deliver 3 files.)");
        assert!(
            !note.contains("a.png") && !note.contains("b.pdf") && !note.contains("c.mp4"),
            "user-facing failure note must not echo failed marker targets"
        );
    }

    #[test]
    fn composed_delivery_failure_note_redacts_parsed_marker_target() {
        let content = "Done\n[IMAGE: /workspace/missing.png]";
        let (cleaned_content, parsed_attachments) = parse_attachment_markers(content);
        let (_locals, _remotes, failures) =
            classify_outgoing_attachments(&parsed_attachments, None);
        let note = delivery_failure_note(&failures);
        let composed = compose_body_with_failure_note(&cleaned_content, note.as_deref());

        assert_eq!(composed, "Done\n\n(note: I couldn't deliver 1 file.)");
        assert!(
            !composed.contains("/workspace/missing.png"),
            "composed outbound body must not echo failed marker targets"
        );
    }

    #[test]
    fn compose_body_with_failure_note_uses_note_alone_when_content_empty() {
        let composed = compose_body_with_failure_note("", Some("(note: ...)"));
        assert_eq!(composed, "(note: ...)");
    }

    #[test]
    fn compose_body_with_failure_note_appends_note_to_existing_content() {
        let composed = compose_body_with_failure_note("Hello.", Some("(note: ...)"));
        assert_eq!(composed, "Hello.\n\n(note: ...)");
    }

    #[test]
    fn compose_body_with_failure_note_returns_content_when_no_note() {
        let composed = compose_body_with_failure_note("Hello.", None);
        assert_eq!(composed, "Hello.");
    }

    #[test]
    fn compose_body_with_failure_note_returns_empty_when_no_content_and_no_note() {
        let composed = compose_body_with_failure_note("", None);
        assert_eq!(composed, "");
    }

    #[test]
    fn decide_failure_reactions_empty_for_no_failures() {
        assert!(decide_failure_reactions(&[]).is_empty());
    }

    #[test]
    fn decide_failure_reactions_emits_refused_only() {
        let r = decide_failure_reactions(&[
            DiscordMarkerFailure::Refused,
            DiscordMarkerFailure::Refused,
        ]);
        assert_eq!(r, vec!["🚫"]);
    }

    #[test]
    fn decide_failure_reactions_emits_not_found_only() {
        let r = decide_failure_reactions(&[DiscordMarkerFailure::NotFound]);
        assert_eq!(r, vec!["\u{26A0}\u{FE0F}"]);
    }

    #[test]
    fn decide_failure_reactions_emits_both_when_mixed() {
        let r = decide_failure_reactions(&[
            DiscordMarkerFailure::Refused,
            DiscordMarkerFailure::NotFound,
        ]);
        assert_eq!(r, vec!["🚫", "\u{26A0}\u{FE0F}"]);
    }

    // ── Streaming mode tests ──────────────────────────────────────────

    #[test]
    fn supports_draft_updates_respects_stream_mode() {
        use zeroclaw_config::schema::StreamMode;

        let listen_to_bots = false;
        let mention_only = false;
        let off = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        assert!(!off.supports_draft_updates());

        let partial = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        )
        .with_streaming(StreamMode::Partial, 750, 800);
        assert!(partial.supports_draft_updates());
        assert_eq!(partial.draft_update_interval_ms, 750);

        let multi = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        )
        .with_streaming(StreamMode::MultiMessage, 1000, 600);
        assert!(multi.supports_draft_updates());
        assert_eq!(multi.multi_message_delay_ms, 600);
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_not_partial() {
        use zeroclaw_api::channel::SendMessage;
        use zeroclaw_config::schema::StreamMode;

        let listen_to_bots = false;
        let mention_only = false;
        let off = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let msg = SendMessage::new("hello", "123");
        assert!(off.send_draft(&msg).await.unwrap().is_none());

        let multi = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        )
        .with_streaming(StreamMode::MultiMessage, 1000, 800);
        // MultiMessage returns a synthetic ID so the draft_updater task runs.
        assert_eq!(
            multi.send_draft(&msg).await.unwrap().as_deref(),
            Some("multi_message_synthetic")
        );
    }

    #[tokio::test]
    async fn update_draft_rate_limit_short_circuits() {
        use zeroclaw_config::schema::StreamMode;

        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        )
        .with_streaming(StreamMode::Partial, 60_000, 800);

        // Seed a recent edit time.
        ch.last_draft_edit
            .lock()
            .insert("chan".to_string(), std::time::Instant::now());

        // Should return Ok immediately (rate-limited) without making a network call.
        let result = ch.update_draft("chan", "fake_msg_id", "new text").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cancel_draft_cleans_up_tracking() {
        use zeroclaw_config::schema::StreamMode;

        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "t".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        )
        .with_streaming(StreamMode::Partial, 1000, 800);

        ch.last_draft_edit
            .lock()
            .insert("chan".to_string(), std::time::Instant::now());

        // cancel_draft will try to delete a message (will fail with network error)
        // but should still clean up the tracking entry.
        let _ = ch.cancel_draft("chan", "fake_msg_id").await;
        assert!(!ch.last_draft_edit.lock().contains_key("chan"));
    }

    // ── MultiMessage splitter tests ───────────────────────────────────

    #[test]
    fn split_message_for_discord_multi_splits_at_paragraphs() {
        let content = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let chunks = split_message_for_discord_multi(content, 2000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "First paragraph.");
        assert_eq!(chunks[1], "Second paragraph.");
        assert_eq!(chunks[2], "Third paragraph.");
    }

    #[test]
    fn split_message_for_discord_multi_single_paragraph() {
        let content = "Just one paragraph with no breaks.";
        let chunks = split_message_for_discord_multi(content, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], content);
    }

    #[test]
    fn split_message_for_discord_multi_respects_max_len() {
        // Create a single paragraph that exceeds max_len.
        let long_para = "a ".repeat(1100); // ~2200 chars
        let chunks = split_message_for_discord_multi(&long_para, 2000);
        assert!(chunks.len() > 1, "should split oversized paragraph");
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= 2000,
                "chunk exceeds max: {}",
                chunk.chars().count()
            );
        }
    }

    #[test]
    fn split_message_for_discord_multi_preserves_code_fences() {
        let content =
            "Before.\n\n```rust\nfn main() {\n\n    println!(\"hello\");\n}\n```\n\nAfter.";
        let chunks = split_message_for_discord_multi(content, 2000);
        // The code fence contains \n\n but should not be split there.
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "Before.");
        assert!(chunks[1].contains("```rust"));
        assert!(chunks[1].contains("println!"));
        assert!(chunks[1].contains("```"));
        assert_eq!(chunks[2], "After.");
    }

    #[test]
    fn split_message_for_discord_multi_empty_input() {
        let chunks = split_message_for_discord_multi("", 2000);
        assert!(chunks.is_empty());
    }

    // Regression lock for the marker-only paragraph in MultiMessage stream
    // mode. Before the fix this produced an empty chunk vec and the chunk
    // loop in send() iterated zero times, silently skipping the file upload.
    #[test]
    fn chunks_for_send_emits_empty_chunk_when_multi_message_paragraph_collapses_to_only_a_file() {
        use zeroclaw_config::schema::StreamMode;
        let chunks = chunks_for_send("", StreamMode::MultiMessage, 2000, true);
        assert_eq!(chunks, vec![String::new()]);
    }

    #[test]
    fn chunks_for_send_does_not_emit_empty_chunk_when_no_files_to_upload() {
        use zeroclaw_config::schema::StreamMode;
        let chunks = chunks_for_send("", StreamMode::MultiMessage, 2000, false);
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunks_for_send_passes_through_non_empty_content() {
        use zeroclaw_config::schema::StreamMode;
        for mode in [
            StreamMode::MultiMessage,
            StreamMode::Partial,
            StreamMode::Off,
        ] {
            for has_files in [true, false] {
                let chunks = chunks_for_send("hello", mode, 2000, has_files);
                assert_eq!(
                    chunks,
                    vec!["hello".to_string()],
                    "mode={mode:?} has_files={has_files}"
                );
            }
        }
    }

    #[test]
    fn pending_approvals_map_is_initially_empty() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "token".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let map = ch.pending_approvals.try_lock().unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn approval_timeout_defaults_to_300_and_is_overridable() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "token".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        assert_eq!(ch.approval_timeout_secs, 300);
        let ch = ch.with_approval_timeout_secs(60);
        assert_eq!(ch.approval_timeout_secs, 60);
    }

    #[tokio::test]
    async fn pending_approval_oneshot_delivers_response() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "token".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        let (tx, rx) = oneshot::channel();
        ch.pending_approvals
            .lock()
            .await
            .insert("abc123".to_string(), tx);
        let sender = ch.pending_approvals.lock().await.remove("abc123").unwrap();
        sender.send(ChannelApprovalResponse::Deny).unwrap();
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Deny);
    }

    // ── Buttoned approval: dispatch composition (the security invariants) ──
    //
    // The live type-3 arm runs: cheap peer check → `interaction_gate`
    // (fail-closed) → `pending_components.take` (single-use) → resolve the
    // parked `oneshot`. These tests wire the *real* primitives in that exact
    // order so the gate-before-take and single-use invariants are locked, then
    // assert resolution behaviour. (The arm itself lives inside the gateway
    // listen loop and can't be driven without a socket; the resolution logic it
    // calls is `approval::resolve_parked_approval`, unit-tested in `approval`.)

    /// Faithful model of the type-3 dispatch's post-peer-check sequence: gate
    /// first, and ONLY on success take + resolve. Mirrors mod.rs so the test
    /// asserts the real ordering contract.
    fn dispatch_approval_click(
        peers: &[String],
        user_id: &str,
        custom_id: &str,
        pending_components: &parking_lot::Mutex<pending::PendingComponents>,
        pending_approvals: &mut std::collections::HashMap<
            String,
            oneshot::Sender<ChannelApprovalResponse>,
        >,
    ) -> bool {
        // Fail-closed authz BEFORE any take. DM-style (no guild/channel filter)
        // with an empty peer list = nobody, exactly like the message path.
        if interaction_gate(peers, &[], &[], user_id, None, "c1", None).is_err() {
            return false; // unauthorized: must not drain or resolve anything
        }
        let intent = pending_components.lock().take(custom_id);
        match intent {
            Some(ComponentIntent::Approval { token, decision }) => {
                approval::resolve_parked_approval(pending_approvals, &token, decision)
            }
            _ => false,
        }
    }

    #[tokio::test]
    async fn authorized_click_resolves_with_the_bound_decision() {
        let token = "tok123";
        let (cid, decision) =
            approval::approval_button_binding(token, approval::ApprovalDecision::AllowOnce);
        let wire = cid.encode().unwrap();

        let reg = parking_lot::Mutex::new(pending::PendingComponents::default());
        reg.lock().register(
            wire.clone(),
            ComponentIntent::Approval {
                token: token.to_string(),
                decision,
            },
        );
        let mut approvals = std::collections::HashMap::new();
        let (tx, rx) = oneshot::channel();
        approvals.insert(token.to_string(), tx);

        let resolved =
            dispatch_approval_click(&[String::from("*")], "u1", &wire, &reg, &mut approvals);
        assert!(resolved, "authorized click resolves the oneshot");
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    #[tokio::test]
    async fn unauthorized_click_neither_resolves_nor_drains() {
        let token = "tok123";
        let (cid, decision) =
            approval::approval_button_binding(token, approval::ApprovalDecision::AllowOnce);
        let wire = cid.encode().unwrap();

        let reg = parking_lot::Mutex::new(pending::PendingComponents::default());
        reg.lock().register(
            wire.clone(),
            ComponentIntent::Approval {
                token: token.to_string(),
                decision,
            },
        );
        let mut approvals = std::collections::HashMap::new();
        let (tx, mut rx) = oneshot::channel();
        approvals.insert(token.to_string(), tx);

        // "intruder" is not in the (specific, non-wildcard) peer list → gate
        // denies BEFORE the take.
        let resolved = dispatch_approval_click(
            &[String::from("u1")],
            "intruder",
            &wire,
            &reg,
            &mut approvals,
        );
        assert!(!resolved, "unauthorized click resolves nothing");
        // The oneshot is unresolved (rx still pending, sender still parked).
        assert!(rx.try_recv().is_err(), "no decision delivered");
        assert!(
            approvals.contains_key(token),
            "the approval entry is NOT drained by an unauthorized click"
        );
        // And the pending component entry survives: an authorized user could
        // still click it (the intruder didn't burn the single use).
        assert!(
            reg.lock().take(&wire).is_some(),
            "the component entry was not drained by the unauthorized click"
        );
    }

    #[tokio::test]
    async fn replayed_click_is_refused_single_use() {
        let token = "tok123";
        let (cid, decision) =
            approval::approval_button_binding(token, approval::ApprovalDecision::Deny);
        let wire = cid.encode().unwrap();

        let reg = parking_lot::Mutex::new(pending::PendingComponents::default());
        reg.lock().register(
            wire.clone(),
            ComponentIntent::Approval {
                token: token.to_string(),
                decision,
            },
        );
        let mut approvals = std::collections::HashMap::new();
        let (tx, rx) = oneshot::channel();
        approvals.insert(token.to_string(), tx);

        assert!(dispatch_approval_click(
            &[String::from("*")],
            "u1",
            &wire,
            &reg,
            &mut approvals
        ));
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Deny);
        // The component entry is gone (single-use take), so a replay of the same
        // custom_id resolves nothing even from an authorized user.
        assert!(
            !dispatch_approval_click(&[String::from("*")], "u1", &wire, &reg, &mut approvals),
            "replayed click refused"
        );
    }

    #[test]
    fn buttoned_approval_registers_four_resolvable_bindings() {
        let listen_to_bots = false;
        let mention_only = false;
        let ch = DiscordChannel::new(
            "token".into(),
            vec![],
            "discord_test_alias",
            Arc::new(Vec::new),
            listen_to_bots,
            mention_only,
        );
        // Register exactly what send_buttoned_approval registers, then confirm
        // every button id resolves to its bound decision (and only its own).
        let token = "abc123";
        let (_, bindings) = approval::build_approval_row(token);
        {
            let mut reg = ch.pending_components.lock();
            for (cid, decision) in &bindings {
                reg.register(
                    cid.encode().unwrap(),
                    ComponentIntent::Approval {
                        token: token.to_string(),
                        decision: *decision,
                    },
                );
            }
        }
        for (cid, decision) in &bindings {
            let got = ch.pending_components.lock().take(&cid.encode().unwrap());
            assert_eq!(
                got,
                Some(ComponentIntent::Approval {
                    token: token.to_string(),
                    decision: *decision,
                }),
                "each button resolves to its server-bound decision"
            );
        }
    }

    #[test]
    fn autocomplete_authz_is_side_effect_free() {
        // The type-4 arm authorizes a keystroke with `interaction_gate` and then
        // answers a type-8 choice set — it never calls reject/defer. The gate
        // itself is a pure function (no &self, no REST), so it is safe to
        // evaluate per-keystroke. Assert it is callable as a pure predicate and
        // is fail-closed for an unauthorized user (→ empty choices).
        assert!(
            interaction_gate(&[String::from("*")], &[], &[], "u1", None, "c1", None).is_ok(),
            "authorized keystroke gates open"
        );
        assert!(
            interaction_gate(
                &[String::from("u1")],
                &[],
                &[],
                "intruder",
                None,
                "c1",
                None
            )
            .is_err(),
            "unauthorized keystroke fails closed → empty choice set, no side effect"
        );
        // DM (no guild) with an empty peer list = nobody, same as messages.
        assert!(
            interaction_gate(&[], &[], &[], "u1", None, "c1", None).is_err(),
            "empty peer list denies"
        );
    }
}
