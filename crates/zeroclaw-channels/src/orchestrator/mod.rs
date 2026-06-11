//! Channel subsystem for messaging platform integrations.
//!
//! This module provides the multi-channel messaging infrastructure that connects
//! ZeroClaw to external platforms. Each channel implements the [`Channel`] trait
//! defined in the `traits` submodule, which provides a uniform interface for
//! sending messages, listening for incoming messages, health checking, and typing
//! indicators.
//!
//! Channels are instantiated by [`start_channels`] based on the runtime configuration.
//! The subsystem manages per-sender conversation history, concurrent message processing
//! with configurable parallelism, and exponential-backoff reconnection for resilience.
//!
//! # Extension
//!
//! To add a new channel, implement [`Channel`] in a new top-level module in
//! `zeroclaw-channels/src/`, declare it in `lib.rs` behind the appropriate feature
//! gate, and wire it into [`start_channels`] here. See `AGENTS.md` §7.2 for the
//! full change playbook.

#[cfg(feature = "channel-acp-server")]
pub mod acp_server;
pub mod media_pipeline;
#[cfg(feature = "channel-mqtt")]
pub mod mqtt;

// Channel types imported directly from source crates (no shim files)
#[cfg(feature = "channel-amqp")]
pub use crate::amqp::AmqpChannel;
#[cfg(feature = "channel-bluesky")]
pub use crate::bluesky::BlueskyChannel;
#[cfg(feature = "channel-clawdtalk")]
pub use crate::clawdtalk::ClawdTalkChannel;
#[cfg(feature = "channel-dingtalk")]
pub use crate::dingtalk::DingTalkChannel;
#[cfg(feature = "channel-discord")]
pub use crate::discord::DiscordChannel;
#[cfg(feature = "channel-email")]
pub use crate::email_channel::EmailChannel;
#[cfg(feature = "channel-email")]
pub use crate::gmail_push::GmailPushChannel;
#[cfg(feature = "channel-imessage")]
pub use crate::imessage::IMessageChannel;
#[cfg(feature = "channel-irc")]
pub use crate::irc::IrcChannel;
#[cfg(feature = "channel-lark")]
pub use crate::lark::LarkChannel;
#[cfg(feature = "channel-line")]
pub use crate::line::LineChannel;
#[cfg(feature = "channel-linq")]
pub use crate::linq::LinqChannel;
#[cfg(feature = "channel-mattermost")]
pub use crate::mattermost::MattermostChannel;
#[cfg(feature = "channel-mochat")]
pub use crate::mochat::MochatChannel;
#[cfg(feature = "channel-nextcloud")]
pub use crate::nextcloud_talk::NextcloudTalkChannel;
#[cfg(feature = "channel-nostr")]
pub use crate::nostr::NostrChannel;
#[cfg(feature = "channel-notion")]
pub use crate::notion::NotionChannel;
#[cfg(feature = "channel-qq")]
pub use crate::qq::QQChannel;
#[cfg(feature = "channel-reddit")]
pub use crate::reddit::RedditChannel;
#[cfg(feature = "channel-signal")]
pub use crate::signal::SignalChannel;
#[cfg(feature = "channel-slack")]
pub use crate::slack::SlackChannel;
pub use crate::transcription;
pub use crate::tts::{TtsManager, TtsProvider};
#[cfg(feature = "channel-twitch")]
pub use crate::twitch::TwitchChannel;
#[cfg(feature = "channel-twitter")]
pub use crate::twitter::TwitterChannel;
#[cfg(feature = "channel-voice-call")]
pub use crate::voice_call::VoiceCallChannel;
#[cfg(feature = "voice-wake")]
pub use crate::voice_wake::VoiceWakeChannel;
#[cfg(feature = "channel-wati")]
pub use crate::wati::WatiChannel;
#[cfg(feature = "channel-webhook")]
pub use crate::webhook::WebhookChannel;
#[cfg(feature = "channel-wechat")]
pub use crate::wechat::WeChatChannel;
#[cfg(feature = "channel-wecom")]
pub use crate::wecom::WeComChannel;
#[cfg(feature = "channel-wecom-ws")]
pub use crate::wecom_ws::WeComWsChannel;
#[cfg(feature = "channel-whatsapp-cloud")]
pub use crate::whatsapp::WhatsAppChannel;
pub use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
// Local channel types (in misc, not zeroclaw-channels)
pub use crate::cli::CliChannel;
pub use crate::link_enricher;
#[cfg(feature = "channel-matrix")]
pub use crate::matrix::MatrixChannel;
#[cfg(feature = "channel-telegram")]
pub use crate::telegram::TelegramChannel;
#[cfg(feature = "whatsapp-web")]
pub use crate::whatsapp_web::WhatsAppWebChannel;
pub use zeroclaw_infra::debounce::MessageDebouncer;
pub use zeroclaw_infra::session_backend::SessionBackend;
pub use zeroclaw_infra::session_sqlite::SqliteSessionBackend;
pub use zeroclaw_infra::stall_watchdog::StallWatchdog;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use portable_atomic::{AtomicU64, Ordering};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

use zeroclaw_api::memory_traits::MemoryStrategy;
use zeroclaw_api::session_keys::sanitize_session_key;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{self, MEMORY_CONTEXT_CLOSE, MEMORY_CONTEXT_OPEN, Memory};
use zeroclaw_providers::reliable::{scope_provider_fallback, take_last_provider_fallback};
use zeroclaw_providers::{self, ChatMessage, ModelProvider};
use zeroclaw_runtime::agent::loop_::{
    apply_policy_tool_filter, apply_text_tool_prompt_policy, build_tool_instructions_for_names,
    clear_model_switch_request, get_model_switch_state, is_model_switch_requested,
    run_tool_call_loop, scope_session_key, scope_thread_id, scrub_credentials,
};
use zeroclaw_runtime::approval::ApprovalManager;
use zeroclaw_runtime::observability::traits::{ObserverEvent, ObserverMetric};
use zeroclaw_runtime::observability::{self, Observer};
use zeroclaw_runtime::platform;
use zeroclaw_runtime::security::{AutonomyLevel, SecurityPolicy};
use zeroclaw_runtime::tools::{self, Tool};
use zeroclaw_runtime::util::truncate_with_ellipsis;

type CronChannelRegistry = Arc<HashMap<String, Arc<dyn Channel>>>;

/// Live channel registry consulted by `deliver_announcement` so cron sends reuse the
/// authenticated channel instance (Matrix E2EE can't tolerate per-send session restore).
/// Replaced wholesale by each `start_channels` call.
static CRON_CHANNEL_REGISTRY: std::sync::RwLock<Option<CronChannelRegistry>> =
    std::sync::RwLock::new(None);

/// Observer wrapper that forwards tool-call events to a channel sender
/// for real-time threaded notifications.
struct ChannelNotifyObserver {
    inner: Arc<dyn Observer>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    tools_used: AtomicBool,
}

impl Observer for ChannelNotifyObserver {
    fn record_event(&self, event: &ObserverEvent) {
        if let ObserverEvent::ToolCallStart {
            tool, arguments, ..
        } = event
        {
            self.tools_used.store(true, Ordering::Relaxed);
            let detail = match arguments {
                Some(args) if !args.is_empty() => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
                        if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
                            format!(": `{}`", truncate_with_ellipsis(cmd, 200))
                        } else if let Some(q) = v.get("query").and_then(|c| c.as_str()) {
                            format!(": {}", truncate_with_ellipsis(q, 200))
                        } else if let Some(p) = v.get("path").and_then(|c| c.as_str()) {
                            format!(": {p}")
                        } else if let Some(u) = v.get("url").and_then(|c| c.as_str()) {
                            format!(": {u}")
                        } else {
                            let s = args.to_string();
                            format!(": {}", truncate_with_ellipsis(&s, 120))
                        }
                    } else {
                        let s = args.to_string();
                        format!(": {}", truncate_with_ellipsis(&s, 120))
                    }
                }
                _ => String::new(),
            };
            let _ = self.tx.send(format!("\u{1F527} `{tool}`{detail}"));
        }
        self.inner.record_event(event);
    }
    fn record_metric(&self, metric: &ObserverMetric) {
        self.inner.record_metric(metric);
    }
    fn flush(&self) {
        self.inner.flush();
    }
    fn name(&self) -> &str {
        "channel-notify"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Per-sender conversation history for channel messages.
/// Bounded by `MAX_CONVERSATION_SENDERS` — oldest-accessed senders are evicted.
type ConversationHistoryMap = Arc<Mutex<lru::LruCache<String, Vec<ChatMessage>>>>;
/// Senders that requested `/new` and must force a fresh prompt on their next message.
type PendingNewSessionSet = Arc<Mutex<HashSet<String>>>;
/// Maximum conversation senders kept in memory (LRU eviction beyond this).
const MAX_CONVERSATION_SENDERS: usize = 1000;
/// Maximum history messages to keep per sender.
const MAX_CHANNEL_HISTORY: usize = 50;
/// Minimum user-message length (in chars) for auto-save to memory.
/// Messages shorter than this (e.g. "ok", "thanks") are not stored,
/// reducing noise in memory recall.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;
const CURRENT_DATE_HEADING: &str = "## Current Date\n\n";
const LEGACY_CURRENT_DATE_TIME_HEADING: &str = "## Current Date & Time\n\n";

// System prompt functions live in `zeroclaw_runtime::agent::system_prompt`.
#[allow(unused_imports)]
pub use zeroclaw_runtime::agent::system_prompt::{
    BOOTSTRAP_MAX_CHARS, build_system_prompt, build_system_prompt_with_mode,
    build_system_prompt_with_mode_and_autonomy,
};

const DEFAULT_CHANNEL_INITIAL_BACKOFF_SECS: u64 = 2;
const DEFAULT_CHANNEL_MAX_BACKOFF_SECS: u64 = 60;
const MIN_CHANNEL_MESSAGE_TIMEOUT_SECS: u64 = 30;
/// Default timeout for processing a single channel message (LLM + tools).
/// Used as fallback when not configured in channels_config.message_timeout_secs.
#[cfg(test)]
const CHANNEL_MESSAGE_TIMEOUT_SECS: u64 = 300;
/// Cap timeout scaling so large max_tool_iterations values do not create unbounded waits.
const CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP: u64 = 4;
const CHANNEL_MIN_IN_FLIGHT_MESSAGES: usize = 8;
const CHANNEL_MAX_IN_FLIGHT_MESSAGES: usize = 64;
const CHANNEL_TYPING_REFRESH_INTERVAL_SECS: u64 = 4;
const CHANNEL_HEALTH_HEARTBEAT_SECS: u64 = 30;
const MODEL_CACHE_FILE: &str = "models_cache.json";
const MODEL_CACHE_PREVIEW_LIMIT: usize = 10;
const MEMORY_CONTEXT_MAX_ENTRIES: usize = 4;
const MEMORY_CONTEXT_ENTRY_MAX_CHARS: usize = 800;
const MEMORY_CONTEXT_MAX_CHARS: usize = 4_000;
const CHANNEL_HISTORY_COMPACT_KEEP_MESSAGES: usize = 12;
const CHANNEL_HISTORY_COMPACT_CONTENT_CHARS: usize = 600;
/// Proactive context-window budget in estimated characters (~4 chars/token).
/// When the total character count of conversation history exceeds this limit,
/// older turns are dropped before the request is sent to the model_provider,
/// preventing context-window-exceeded errors.  Set conservatively below
/// common context windows (128 k tokens ≈ 512 k chars) to leave room for
/// system prompt, memory context, and model output.
const PROACTIVE_CONTEXT_BUDGET_CHARS: usize = 400_000;
/// Guardrail for hook-modified outbound channel content.
const CHANNEL_HOOK_MAX_OUTBOUND_CHARS: usize = 20_000;

type ProviderCacheMap = Arc<Mutex<HashMap<String, Arc<dyn ModelProvider>>>>;
type RouteSelectionMap = Arc<Mutex<HashMap<String, ChannelRouteSelection>>>;

fn effective_channel_message_timeout_secs(configured: u64) -> u64 {
    configured.max(MIN_CHANNEL_MESSAGE_TIMEOUT_SECS)
}

#[cfg(test)]
fn channel_message_timeout_budget_secs(
    message_timeout_secs: u64,
    max_tool_iterations: usize,
) -> u64 {
    channel_message_timeout_budget_secs_with_cap(
        message_timeout_secs,
        max_tool_iterations,
        CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP,
    )
}

fn channel_message_timeout_budget_secs_with_cap(
    message_timeout_secs: u64,
    max_tool_iterations: usize,
    scale_cap: u64,
) -> u64 {
    let iterations = max_tool_iterations.max(1) as u64;
    let scale = iterations.min(scale_cap);
    message_timeout_secs.saturating_mul(scale)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelRouteSelection {
    model_provider: String,
    model: String,
    /// Route-specific API key override. When set, this credential is passed
    /// directly to the requested provider instead of the alias entry's key.
    api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelRuntimeCommand {
    ShowProviders,
    SetProvider(String),
    ShowModel,
    SetModel(String),
    ShowConfig,
    NewSession,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelCacheState {
    entries: Vec<ModelCacheEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelCacheEntry {
    model_provider: String,
    models: Vec<String>,
}

#[derive(Debug, Clone)]
struct ChannelRuntimeDefaults {
    default_model_provider: String,
    model: String,
    temperature: Option<f64>,
    api_key: Option<String>,
    api_url: Option<String>,
    reliability: zeroclaw_config::schema::ReliabilityConfig,
}

#[derive(Debug, Clone)]
struct ChannelRuntimeDefaultsSnapshot {
    config: Arc<Config>,
    defaults: ChannelRuntimeDefaults,
    hot: bool,
    generation: u64,
}

#[derive(Debug, Clone)]
struct ChannelRuntimeOverride {
    config: Arc<Config>,
    defaults: ChannelRuntimeDefaults,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigFileStamp {
    modified: SystemTime,
    len: u64,
}

const SYSTEMD_STATUS_ARGS: [&str; 3] = ["--user", "is-active", "zeroclaw.service"];
const SYSTEMD_RESTART_ARGS: [&str; 3] = ["--user", "restart", "zeroclaw.service"];
const OPENRC_STATUS_ARGS: [&str; 2] = ["zeroclaw", "status"];
const OPENRC_RESTART_ARGS: [&str; 2] = ["zeroclaw", "restart"];

#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct InterruptOnNewMessageConfig {
    telegram: bool,
    slack: bool,
    discord: bool,
    mattermost: bool,
    matrix: bool,
    whatsapp: bool,
}

impl InterruptOnNewMessageConfig {
    fn enabled_for_channel(self, channel: &str) -> bool {
        match channel {
            "telegram" => self.telegram,
            "slack" => self.slack,
            "discord" => self.discord,
            "mattermost" => self.mattermost,
            "matrix" => self.matrix,
            "whatsapp" => self.whatsapp,
            _ => false,
        }
    }
}

fn interrupt_on_new_message_config(
    channels: &zeroclaw_config::schema::ChannelsConfig,
) -> InterruptOnNewMessageConfig {
    InterruptOnNewMessageConfig {
        telegram: channels
            .telegram
            .get("default")
            .is_some_and(|tg| tg.interrupt_on_new_message),
        slack: channels
            .slack
            .get("default")
            .is_some_and(|sl| sl.interrupt_on_new_message),
        discord: channels
            .discord
            .get("default")
            .is_some_and(|dc| dc.interrupt_on_new_message),
        mattermost: channels
            .mattermost
            .get("default")
            .is_some_and(|mm| mm.interrupt_on_new_message),
        matrix: channels
            .matrix
            .get("default")
            .is_some_and(|mx| mx.interrupt_on_new_message),
        whatsapp: channels
            .whatsapp
            .get("default")
            .is_some_and(|wa| wa.interrupt_on_new_message),
    }
}

#[derive(Clone)]
struct ChannelCostTrackingState {
    tracker: Arc<zeroclaw_runtime::cost::CostTracker>,
    model_provider_pricing: Arc<zeroclaw_runtime::agent::cost::ModelProviderPricing>,
    agent_alias: Arc<String>,
}

#[derive(Clone)]
struct ChannelRuntimeContext {
    channels_by_name: Arc<HashMap<String, Arc<dyn Channel>>>,
    model_provider: Arc<dyn ModelProvider>,
    model_provider_ref: Arc<String>,
    /// Alias of the agent that owns this runtime context. Stamped onto
    /// every per-message tracing span so descendant events inherit the
    /// attribution without each call site re-passing it.
    agent_alias: Arc<String>,
    /// Resolved aliased-agent config for the agent owning this
    /// runtime context. Per-channel agent dispatch (one agent per
    /// channel.`<type>`.`<alias>`) is a follow-up.
    agent_cfg: Arc<zeroclaw_config::schema::AliasedAgentConfig>,
    prompt_config: Arc<zeroclaw_config::schema::Config>,
    memory: Arc<dyn Memory>,
    memory_strategy: Arc<dyn MemoryStrategy>,
    tools_registry: Arc<Vec<Box<dyn Tool>>>,
    observer: Arc<dyn Observer>,
    system_prompt: Arc<String>,
    model: Arc<String>,
    temperature: Option<f64>,
    auto_save_memory: bool,
    max_tool_iterations: usize,
    min_relevance_score: f64,
    conversation_histories: ConversationHistoryMap,
    pending_new_sessions: PendingNewSessionSet,
    provider_cache: ProviderCacheMap,
    route_overrides: RouteSelectionMap,
    reliability: Arc<zeroclaw_config::schema::ReliabilityConfig>,
    provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
    workspace_dir: Arc<PathBuf>,
    message_timeout_secs: u64,
    interrupt_on_new_message: InterruptOnNewMessageConfig,
    multimodal: zeroclaw_config::schema::MultimodalConfig,
    media_pipeline: zeroclaw_config::schema::MediaPipelineConfig,
    transcription_config: zeroclaw_config::schema::TranscriptionConfig,
    /// Resolved per-agent transcription provider alias (`<type>.<alias>`)
    /// for the runtime-active agent that owns this channel context.
    /// Empty when the agent has no transcription_provider set; downstream
    /// `TranscriptionManager.transcribe` calls then fail loud.
    agent_transcription_provider: String,
    hooks: Option<Arc<zeroclaw_runtime::hooks::HookRunner>>,
    non_cli_excluded_tools: Arc<Vec<String>>,
    autonomy_level: AutonomyLevel,
    tool_call_dedup_exempt: Arc<Vec<String>>,
    model_routes: Arc<Vec<zeroclaw_config::schema::ModelRouteConfig>>,
    query_classification: zeroclaw_config::schema::QueryClassificationConfig,
    ack_reactions: bool,
    show_tool_calls: bool,
    session_store: Option<Arc<dyn zeroclaw_infra::session_backend::SessionBackend>>,
    /// Non-interactive approval manager for channel-driven runs.
    /// Enforces `auto_approve` / `always_ask` / supervised policy from
    /// `[autonomy]` config; auto-denies tools that would need interactive
    /// approval since no operator is present on channel runs.
    approval_manager: Arc<ApprovalManager>,
    activated_tools:
        Option<std::sync::Arc<std::sync::Mutex<zeroclaw_runtime::tools::ActivatedToolSet>>>,
    cost_tracking: Option<ChannelCostTrackingState>,
    pacing: zeroclaw_config::schema::PacingConfig,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    debouncer: Arc<zeroclaw_infra::debounce::MessageDebouncer>,
    /// HMAC receipt generator. `Some` when `[agent.resolved.tool_receipts] enabled = true`.
    /// Threaded into `run_tool_call_loop` so `tool_execution::execute_one_tool`
    /// can sign each result.
    receipt_generator: Option<zeroclaw_runtime::agent::tool_receipts::ReceiptGenerator>,
    /// Mirror of `[agent.resolved.tool_receipts] show_in_response`. When true,
    /// `process_channel_message` renders the per-turn collector as a trailing
    /// `Tool receipts:` block sent after the main reply.
    show_receipts_in_response: bool,
    last_applied_config_stamp: Arc<Mutex<Option<ConfigFileStamp>>>,
    runtime_defaults_override: Arc<Mutex<Option<Arc<ChannelRuntimeOverride>>>>,
}

#[derive(Clone)]
struct InFlightSenderTaskState {
    task_id: u64,
    cancellation: CancellationToken,
    completion: Arc<InFlightTaskCompletion>,
}

struct InFlightTaskCompletion {
    done: AtomicBool,
    notify: tokio::sync::Notify,
}

impl InFlightTaskCompletion {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    fn mark_done(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        if self.done.load(Ordering::Acquire) {
            return;
        }
        self.notify.notified().await;
    }
}

fn conversation_memory_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    // Include thread_ts for per-topic memory isolation in forum groups
    let raw = match &msg.thread_ts {
        Some(tid) => format!("{}_{}_{}_{}", msg.channel, tid, msg.sender, msg.id),
        None => format!("{}_{}_{}", msg.channel, msg.sender, msg.id),
    };
    sanitize_session_key(&raw)
}

pub fn conversation_history_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    // Channel prefix includes the zeroclaw alias when present so two bots
    // on the same platform (e.g. `discord.clamps` + `discord.glados`)
    // compute distinct session_keys and don't share conversation history.
    let channel_scope = match &msg.channel_alias {
        Some(alias) => format!("{}.{}", msg.channel, alias),
        None => msg.channel.clone(),
    };
    if msg.channel == "wecom_ws" {
        return sanitize_session_key(&format!("{channel_scope}_{}", msg.reply_target));
    }
    // reply_target gives per-channel isolation (distinct Discord/Slack
    // channels) and thread_ts gives per-topic isolation in forum groups.
    // Sanitize so the runtime HashMap key matches `SessionStore::list_sessions`
    // after a restart; otherwise hydration loads sessions under the on-disk
    // (sanitized) name while lookup keeps producing the un-sanitized form.
    let thread_scope = match msg.thread_ts.as_deref() {
        // Matrix root events can be self-anchored when `reply_in_thread`
        // is enabled so outbound replies open a thread. That anchor is a
        // delivery detail, not a conversation-history boundary; otherwise
        // every top-level Matrix message becomes a fresh session.
        Some(tid) if is_matrix_channel_name(&msg.channel) && tid == msg.id => None,
        other => other,
    };
    let raw = match thread_scope {
        Some(tid) => format!("{channel_scope}_{}_{tid}_{}", msg.reply_target, msg.sender),
        None => format!("{channel_scope}_{}_{}", msg.reply_target, msg.sender),
    };
    sanitize_session_key(&raw)
}

fn followup_thread_id(msg: &zeroclaw_api::channel::ChannelMessage) -> Option<String> {
    if is_matrix_channel_name(&msg.channel) {
        msg.thread_ts.clone()
    } else {
        msg.thread_ts.clone().or_else(|| Some(msg.id.clone()))
    }
}

fn interruption_scope_key(msg: &zeroclaw_api::channel::ChannelMessage) -> String {
    if msg.channel == "wecom_ws" && msg.reply_target.starts_with("group--") {
        let channel_scope = match &msg.channel_alias {
            Some(alias) => format!("{}.{}", msg.channel, alias),
            None => msg.channel.clone(),
        };
        return sanitize_session_key(&format!("{channel_scope}_{}", msg.reply_target));
    }

    match &msg.interruption_scope_id {
        Some(scope) => format!(
            "{}_{}_{}_{}",
            msg.channel, msg.reply_target, msg.sender, scope
        ),
        None => format!("{}_{}_{}", msg.channel, msg.reply_target, msg.sender),
    }
}

/// Returns `true` when `content` is a `/stop` command (with optional `@botname` suffix).
/// Not gated on channel type — all non-CLI channels support `/stop`.
fn is_stop_command(content: &str) -> bool {
    let trimmed = content.trim();
    if !trimmed.starts_with('/') {
        return false;
    }
    let cmd = trimmed.split_whitespace().next().unwrap_or("");
    let base = cmd.split('@').next().unwrap_or(cmd);
    base.eq_ignore_ascii_case("/stop")
}

/// Strip tool-call XML tags from outgoing messages.
///
/// LLM responses may contain `<function_calls>`, `<function_call>`,
/// `<tool_call>`, `<toolcall>`, `<tool-call>`, `<tool>`, or `<invoke>`
/// blocks that are internal protocol and must not be forwarded to end
/// users on any channel.
pub(crate) fn strip_tool_call_tags(message: &str) -> String {
    const TOOL_CALL_OPEN_TAGS: [&str; 7] = [
        "<function_calls>",
        "<function_call>",
        "<tool_call>",
        "<toolcall>",
        "<tool-call>",
        "<tool>",
        "<invoke>",
    ];

    fn find_first_tag<'a>(haystack: &str, tags: &'a [&'a str]) -> Option<(usize, &'a str)> {
        tags.iter()
            .filter_map(|tag| haystack.find(tag).map(|idx| (idx, *tag)))
            .min_by_key(|(idx, _)| *idx)
    }

    fn matching_close_tag(open_tag: &str) -> Option<&'static str> {
        match open_tag {
            "<function_calls>" => Some("</function_calls>"),
            "<function_call>" => Some("</function_call>"),
            "<tool_call>" => Some("</tool_call>"),
            "<toolcall>" => Some("</toolcall>"),
            "<tool-call>" => Some("</tool-call>"),
            "<tool>" => Some("</tool>"),
            "<invoke>" => Some("</invoke>"),
            _ => None,
        }
    }

    fn extract_first_json_end(input: &str) -> Option<usize> {
        let trimmed = input.trim_start();
        let trim_offset = input.len().saturating_sub(trimmed.len());

        for (byte_idx, ch) in trimmed.char_indices() {
            if ch != '{' && ch != '[' {
                continue;
            }

            let slice = &trimmed[byte_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            if let Some(Ok(_value)) = stream.next() {
                let consumed = stream.byte_offset();
                if consumed > 0 {
                    return Some(trim_offset + byte_idx + consumed);
                }
            }
        }

        None
    }

    fn strip_leading_close_tags(mut input: &str) -> &str {
        loop {
            let trimmed = input.trim_start();
            if !trimmed.starts_with("</") {
                return trimmed;
            }

            let Some(close_end) = trimmed.find('>') else {
                return "";
            };
            input = &trimmed[close_end + 1..];
        }
    }

    // Does the tag structure run to the end of the message? A *real* truncated
    // tool call is the model getting cut off, so the unterminated structure is
    // the last thing in the message. If natural-language prose resumes after the
    // tags, this is an inline *example* (the model is discussing tool calls), not
    // a truncation — so we should keep it. Bias toward keeping: a little leaked
    // XML beats eating the user's text.
    fn tool_structure_runs_to_end(inner: &str) -> bool {
        let mut rest = inner.trim_start();
        while rest.starts_with('<') {
            match rest.find('>') {
                Some(gt) => rest = rest[gt + 1..].trim_start(),
                None => return true,
            }
        }
        let tail = rest.trim();
        if tail.is_empty() {
            return true;
        }
        !looks_like_prose(tail)
    }

    // Heuristic: does `text` read like resumed natural-language prose (as opposed
    // to a cut-off parameter value)? True on an internal sentence boundary
    // (". " / "! " / "? " + a letter) or a multi-word string that ends like a
    // sentence. Deliberately lenient so ambiguous tails are kept, not dropped.
    fn looks_like_prose(text: &str) -> bool {
        let bytes = text.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            if matches!(bytes[i], b'.' | b'!' | b'?')
                && matches!(bytes[i + 1], b' ' | b'\n' | b'\t')
                && text[i + 1..]
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic())
            {
                return true;
            }
        }
        let trimmed = text.trim_end();
        let ends_like_sentence = trimmed
            .chars()
            .last()
            .is_some_and(|c| matches!(c, '.' | '!' | '?'))
            && trimmed
                .chars()
                .rev()
                .nth(1)
                .is_some_and(|c| c.is_alphabetic());
        ends_like_sentence && text.trim().contains(' ')
    }

    let mut kept_segments = Vec::new();
    let mut remaining = message;

    while let Some((start, open_tag)) = find_first_tag(remaining, &TOOL_CALL_OPEN_TAGS) {
        let before = &remaining[..start];
        if !before.is_empty() {
            kept_segments.push(before.to_string());
        }

        let Some(close_tag) = matching_close_tag(open_tag) else {
            break;
        };
        let after_open = &remaining[start + open_tag.len()..];

        if let Some(close_idx) = after_open.find(close_tag) {
            remaining = &after_open[close_idx + close_tag.len()..];
            continue;
        }

        if let Some(consumed_end) = extract_first_json_end(after_open) {
            remaining = strip_leading_close_tags(&after_open[consumed_end..]);
            continue;
        }

        // Unterminated open tag with no parseable JSON body. Drop the broken
        // tail ONLY when it looks like tool-call structure AND that structure
        // runs to the end of the message — a real truncation where the model was
        // cut off mid-call. If prose resumes after the structure, the model is
        // showing an *example*, not making a call, so keep it verbatim (a little
        // leaked XML beats eating the reply). Text merely mentioning a tag is
        // likewise kept.
        let inner = after_open.trim_start();
        let inner_lower = inner.to_ascii_lowercase();
        let looks_like_tool_structure = inner_lower.starts_with("<invoke")
            || inner_lower.starts_with("<parameter")
            || inner_lower.starts_with("<tool")
            || inner_lower.starts_with("<function")
            || inner.starts_with('{')
            || inner.starts_with('[');
        if looks_like_tool_structure && tool_structure_runs_to_end(inner) {
            remaining = "";
            break;
        }

        kept_segments.push(remaining[start..].to_string());
        remaining = "";
        break;
    }

    if !remaining.is_empty() {
        kept_segments.push(remaining.to_string());
    }

    let mut result = kept_segments.concat();

    // Clean up any resulting blank lines (but preserve paragraphs)
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

fn channel_delivery_instructions(channel_name: &str) -> Option<&'static str> {
    match channel_name {
        "matrix" => Some(
            "When responding on Matrix:\n\
             - Use Markdown formatting (bold, italic, code blocks)\n\
             - Be concise and direct\n\
             - For media attachments use markers: [IMAGE:<absolute-path>], [DOCUMENT:<absolute-path>], [VIDEO:<absolute-path>], [AUDIO:<absolute-path>], or [VOICE:<absolute-path>]\n\
             - Paths inside markers MUST be absolute (starting with /). Never use relative paths.\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n\
             - When you receive a [Voice message], the user spoke to you. Respond naturally as in conversation.\n\
             - Your text reply will automatically be converted to audio and sent back as a voice message.\n",
        ),
        "discord" => Some(
            "When responding on Discord:\n\
             - Use Markdown formatting (bold, italic, code blocks)\n\
             - Be concise and direct\n\
             - For media attachments use markers: [IMAGE:<absolute-path>], [DOCUMENT:<absolute-path>], [VIDEO:<absolute-path>], [AUDIO:<absolute-path>], or [VOICE:<absolute-path>]\n\
             - Paths inside markers MUST be absolute (starting with /) and live inside the configured workspace directory. Never use relative paths.\n\
             - Remote media is also accepted via http:// or https:// URLs in the same marker form.\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n",
        ),
        "telegram" => Some(
            "When responding on Telegram:\n\
             - Include media markers for files or URLs that should be sent as attachments\n\
             - Use **bold** for key terms, section titles, and important info (renders as <b>)\n\
             - Use *italic* for emphasis (renders as <i>)\n\
             - Use `backticks` for inline code, commands, or technical terms\n\
             - Use triple backticks for code blocks\n\
             - Use emoji naturally to add personality — but don't overdo it\n\
             - Be concise and direct. Skip filler phrases like 'Great question!' or 'Certainly!'\n\
             - Structure longer answers with bold headers, not raw markdown ## headers\n\
             - For media attachments use markers: [IMAGE:<path-or-url>], [DOCUMENT:<path-or-url>], [VIDEO:<path-or-url>], [AUDIO:<path-or-url>], or [VOICE:<path-or-url>]\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n\
             - Use tool results silently: answer the latest user message directly, and do not narrate delayed/internal tool execution bookkeeping.",
        ),
        "qq" => Some(
            "When responding on QQ:\n\
             - Use Markdown formatting\n\
             - Be concise and direct\n\
             - For media attachments use markers: [IMAGE:<path-or-url>], [DOCUMENT:<path-or-url>], \
               [VIDEO:<path-or-url>], [VOICE:<path-or-url>]\n\
             - Voice supports .wav, .mp3, .silk formats only. Other audio formats use [DOCUMENT:]\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n",
        ),
        "wechat" => Some(
            "When responding on WeChat:\n\
             - Be concise and direct\n\
             - For media attachments use markers: [IMAGE:<path-or-url>], [DOCUMENT:<path-or-url>], \
               [VIDEO:<path-or-url>], [AUDIO:<path-or-url>], or [VOICE:<path-or-url>]\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n\
             - Use absolute local paths when sending generated files whenever possible.\n",
        ),
        "wecom_ws" => Some(
            "When responding on WeCom AI Bot WebSocket:\n\
             - Be concise and direct\n\
             - Use Markdown text; the channel sends progressive draft updates when enabled\n\
             - Do not use local attachment markers; outbound image payloads are not supported yet.\n",
        ),
        _ => None,
    }
}

fn build_channel_system_prompt_for_message(
    base_prompt: &str,
    msg: &zeroclaw_api::channel::ChannelMessage,
    target_channel: Option<&Arc<dyn Channel>>,
) -> String {
    let bot_mention = target_channel.and_then(|c| c.self_addressed_mention());
    build_channel_system_prompt(
        base_prompt,
        &msg.channel,
        &msg.reply_target,
        &msg.sender,
        &msg.id,
        bot_mention.as_deref(),
    )
}

fn build_channel_system_prompt(
    base_prompt: &str,
    channel_name: &str,
    reply_target: &str,
    sender: &str,
    message_id: &str,
    bot_mention: Option<&str>,
) -> String {
    let mut prompt = base_prompt.to_string();

    refresh_channel_prompt_date_section(&mut prompt);

    if let Some(instructions) = channel_delivery_instructions(channel_name) {
        if prompt.is_empty() {
            prompt = instructions.to_string();
        } else {
            prompt = format!("{prompt}\n\n{instructions}");
        }
    }

    if let Some(mention) = bot_mention {
        let block = format!(
            "\n\nYour addressable handle on this channel: {mention}. \
             When you see this exact string anywhere in an inbound message, \
             it refers to YOU, not another agent or user. This same format \
             is also what you should emit when you need to tag yourself or \
             address peers in outbound replies on this channel."
        );
        prompt.push_str(&block);
    }

    if !reply_target.is_empty() {
        // For most channels, `reply_target` is the address to send to (channel/room
        // ID for Slack/Discord/Matrix, peer ID for Telegram/Signal). The webhook
        // channel is the exception: its outbound JSON has both `recipient` and
        // `thread_id`, and downstream services routing through it expect the
        // *sender* as the recipient and the *thread/conversation* identifier in
        // `thread_id`. Reusing `reply_target` as `to` for webhook would strip the
        // thread context and the receiver would discard the callback.
        let delivery_hint = if channel_name.eq_ignore_ascii_case("webhook") {
            format!(
                "delivery={{\"mode\":\"announce\",\"channel\":\"{channel_name}\",\
                 \"to\":\"{sender}\",\"thread_id\":\"{reply_target}\"}}"
            )
        } else {
            format!(
                "delivery={{\"mode\":\"announce\",\"channel\":\"{channel_name}\",\
                 \"to\":\"{reply_target}\"}}"
            )
        };
        let context = format!(
            "\n\nChannel context: You are currently responding on channel={channel_name}, \
             reply_target={reply_target}, sender={sender}, message_id={message_id}. \
             The sender field is the platform-specific user ID of the person who sent \
             this message. Use it to distinguish between different users. \
             The message_id field identifies this incoming message; pass it as the \
             `message_id` argument when calling the `reaction` tool. \
             When scheduling delayed messages or reminders \
             via cron_add for this conversation, use {delivery_hint} so the message \
             reaches the user.\n\nCalibration note: agents in this system currently err \
             on the side of silence when a response would be appropriate, which users \
             find frustrating. Skew toward replying. Memory is supplementary context \
             that informs how you respond, not a gate on whether you respond."
        );
        prompt.push_str(&context);
    }

    prompt
}

fn current_date_section() -> String {
    let now = chrono::Local::now();
    format!(
        "{CURRENT_DATE_HEADING}{} ({})",
        now.format("%Y-%m-%d"),
        now.format("%:z")
    )
}

fn refresh_channel_prompt_date_section(prompt: &mut String) {
    let runtime_start = prompt
        .find("\n## Runtime")
        .map(|i| i + 1)
        .unwrap_or(prompt.len());

    if let Some((start, heading_len)) = find_latest_date_heading_before(prompt, runtime_start) {
        let content_start = start + heading_len;
        let section_end = prompt[content_start..]
            .find("\n## ")
            .map(|i| content_start + i)
            .unwrap_or(prompt.len());
        prompt.replace_range(start..section_end, &current_date_section());
    }
}

fn find_latest_date_heading_before(prompt: &str, before: usize) -> Option<(usize, usize)> {
    let prefix = &prompt[..before];
    [CURRENT_DATE_HEADING, LEGACY_CURRENT_DATE_TIME_HEADING]
        .iter()
        .filter_map(|heading| prefix.rfind(heading).map(|start| (start, heading.len())))
        .max_by_key(|(start, _)| *start)
}

fn timestamp_channel_user_content(content: &str) -> String {
    let now = chrono::Local::now();
    format!("[{}] {}", now.format("%Y-%m-%d %H:%M:%S %Z"), content)
}

fn channel_history_content_for_user_turn(content: &str) -> String {
    let (cleaned, image_refs) = zeroclaw_providers::multimodal::parse_image_markers(content);
    if image_refs.is_empty() {
        return content.to_string();
    }

    let mut cleaned = cleaned.trim().to_string();
    while cleaned.contains("\n\n\n") {
        cleaned = cleaned.replace("\n\n\n", "\n\n");
    }

    if cleaned.is_empty() {
        "[Image attachment processed by vision model]".to_string()
    } else {
        cleaned
    }
}

fn restore_current_user_turn_media_payload(
    turns: &mut [ChatMessage],
    history_content: &str,
    full_content: &str,
) {
    if history_content == full_content {
        return;
    }

    if let Some(turn) = turns
        .iter_mut()
        .rev()
        .find(|turn| turn.role == "user" && turn.content == history_content)
    {
        turn.content = full_content.to_string();
    }
}

fn strip_historical_image_payloads(turns: &mut Vec<ChatMessage>) {
    if turns.len() <= 1 {
        return;
    }

    let last_idx = turns.len() - 1;
    for turn in &mut turns[..last_idx] {
        if turn.content.contains("[IMAGE:") {
            turn.content = channel_history_content_for_user_turn(&turn.content);
        }
    }

    let current = turns.pop();
    turns.retain(|turn| !turn.content.trim().is_empty());
    if let Some(current) = current {
        turns.push(current);
    }
}

fn normalize_cached_channel_turns(turns: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut normalized = Vec::with_capacity(turns.len());
    let mut expecting_user = true;

    for turn in turns {
        match (expecting_user, turn.role.as_str()) {
            // Pass through tool-role messages preserved by
            // keep_tool_context_turns.  After a tool result the
            // next expected message is an assistant response, same as
            // after a user message.
            (_, "tool") | (true, "user") => {
                normalized.push(turn);
                expecting_user = false;
            }
            (false, "assistant") => {
                normalized.push(turn);
                expecting_user = true;
            }
            // Interrupted channel turns can produce consecutive user messages
            // (no assistant persisted yet). Merge instead of dropping.
            (false, "user") | (true, "assistant") => {
                if let Some(last_turn) = normalized.last_mut()
                    && !turn.content.is_empty()
                {
                    if !last_turn.content.is_empty() {
                        last_turn.content.push_str("\n\n");
                    }
                    last_turn.content.push_str(&turn.content);
                }
            }
            _ => {}
        }
    }

    normalized
}

/// Remove `<tool_result …>…</tool_result>` blocks (and a leading `[Tool results]`
/// header, if present) from a conversation-history entry so that stale tool
/// output is never presented to the LLM without the corresponding `<tool_call>`.
fn strip_tool_result_content(text: &str) -> String {
    static TOOL_RESULT_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)<tool_result[^>]*>.*?</tool_result>").unwrap()
    });

    let cleaned = TOOL_RESULT_RE.replace_all(text, "");
    let cleaned = cleaned.trim();

    // If the only remaining content is the header, drop it entirely.
    if cleaned == "[Tool results]" || cleaned.is_empty() {
        return String::new();
    }

    cleaned.to_string()
}

/// Remove a leading `[Used tools: ...]` line from a cached assistant turn.
///
/// The tool-context summary is prepended to history entries so the LLM retains
/// awareness of prior tool usage. However, when these entries are loaded back
/// into the LLM context, the bracket-format leaks into generated output and
/// gets forwarded to end users as-is (bug #4400). Stripping the prefix on
/// reload prevents the model from learning and reproducing this internal format.
fn strip_tool_summary_prefix(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("[Used tools:") {
        // Find the closing bracket, then skip it and any leading newline(s).
        if let Some(bracket_end) = rest.find(']') {
            let after_bracket = &rest[bracket_end + 1..];
            let trimmed = after_bracket.trim_start_matches('\n');
            if trimmed.is_empty() {
                return String::new();
            }
            return trimmed.to_string();
        }
    }
    text.to_string()
}

fn supports_runtime_model_switch(channel_name: &str) -> bool {
    matches!(
        channel_name,
        "telegram" | "discord" | "matrix" | "slack" | "wecom_ws"
    )
}

fn is_explicitly_addressed_channel_message(channel_name: &str, content: &str) -> bool {
    channel_name == "wecom_ws"
        && content.contains("[WeCom group message addressed to this bot via @")
}

fn is_matrix_channel_name(channel_name: &str) -> bool {
    channel_name == "matrix" || channel_name.starts_with("matrix:")
}

fn parse_runtime_command(channel_name: &str, content: &str) -> Option<ChannelRuntimeCommand> {
    let trimmed = content.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let command_token = parts.next()?;
    let base_command = command_token
        .split('@')
        .next()
        .unwrap_or(command_token)
        .to_ascii_lowercase();

    match base_command.as_str() {
        // `/new` is available on every channel — no model-switch gate.
        "/new" => Some(ChannelRuntimeCommand::NewSession),
        // Model/model_provider switching is channel-gated.
        "/models" if supports_runtime_model_switch(channel_name) => {
            if let Some(model_provider) = parts.next() {
                Some(ChannelRuntimeCommand::SetProvider(
                    model_provider.trim().to_string(),
                ))
            } else {
                Some(ChannelRuntimeCommand::ShowProviders)
            }
        }
        "/model" if supports_runtime_model_switch(channel_name) => {
            let model = parts.collect::<Vec<_>>().join(" ").trim().to_string();
            if model.is_empty() {
                Some(ChannelRuntimeCommand::ShowModel)
            } else {
                Some(ChannelRuntimeCommand::SetModel(model))
            }
        }
        "/config" if supports_runtime_model_switch(channel_name) => {
            Some(ChannelRuntimeCommand::ShowConfig)
        }
        _ => None,
    }
}

/// Verify `name` matches a canonical model provider family known to the
/// runtime registry. Returns the canonical (case-corrected) name, or `None`
/// when the input doesn't name a known family. Used by the channel
/// `/models` slash command, which accepts only the bare family name; dotted
/// aliases (`<family>.<alias>`) are resolved elsewhere through
/// `create_resilient_model_provider_from_ref`.
fn canonical_model_provider_name(name: &str) -> Option<String> {
    let candidate = name.trim();
    if candidate.is_empty() {
        return None;
    }

    zeroclaw_providers::list_model_providers()
        .into_iter()
        .find(|model_provider| model_provider.name.eq_ignore_ascii_case(candidate))
        .map(|model_provider| model_provider.name.to_string())
}

/// Outcome of resolving a `/models <arg>` request to a configured,
/// alias-backed provider ref. The bare family path must never construct a
/// provider that ignores the configured `[providers.models.<family>.<alias>]`
/// key/URI — every accepted route resolves to a real alias entry.
#[cfg_attr(test, derive(Debug))]
enum ModelsCommandResolution {
    /// A dotted `<family>.<alias>` ref backed by a configured entry.
    Resolved(String),
    /// The family is valid but has more than one configured alias; the user
    /// must qualify which one. Carries the canonical family and its aliases.
    Ambiguous {
        family: String,
        aliases: Vec<String>,
    },
    /// The family is valid but has no configured alias entry, so there is no
    /// credentialed provider to switch to.
    NoAlias(String),
    /// The argument names no known provider family.
    Unknown,
}

/// Resolve a `/models <arg>` argument to a configured, alias-backed provider
/// ref. Accepts either a dotted `<family>.<alias>` that resolves to a real
/// `[providers.models.<family>.<alias>]` entry, or a bare family name that has
/// exactly one configured alias. A bare family with several aliases is
/// ambiguous; one with none has no credentialed provider. This keeps `/models`
/// inside the v0.8 alias model rather than constructing a bare provider that
/// silently ignores the configured key/URI.
fn resolve_models_command(
    config: &zeroclaw_config::schema::Config,
    raw: &str,
) -> ModelsCommandResolution {
    let candidate = raw.trim();
    if let Some((family, alias)) = candidate.split_once('.') {
        return match config.providers.models.find(family, alias) {
            Some(_) => ModelsCommandResolution::Resolved(format!("{family}.{alias}")),
            None => ModelsCommandResolution::NoAlias(candidate.to_string()),
        };
    }

    let Some(family) = canonical_model_provider_name(candidate) else {
        return ModelsCommandResolution::Unknown;
    };

    let mut aliases: Vec<String> = config
        .providers
        .models
        .aliases_of(&family)
        .map(ToString::to_string)
        .collect();
    aliases.sort();
    match aliases.len() {
        0 => ModelsCommandResolution::NoAlias(family),
        1 => ModelsCommandResolution::Resolved(format!("{family}.{}", aliases[0])),
        _ => ModelsCommandResolution::Ambiguous { family, aliases },
    }
}

fn resolve_provider_ref_for_runtime_switch(config: &Config, raw: &str) -> anyhow::Result<String> {
    match resolve_models_command(config, raw) {
        ModelsCommandResolution::Resolved(provider_ref) => Ok(provider_ref),
        ModelsCommandResolution::Ambiguous { family, aliases } => {
            let list = aliases
                .iter()
                .map(|alias| format!("{family}.{alias}"))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "model_provider `{family}` has multiple configured aliases; use one of: {list}"
            )
        }
        ModelsCommandResolution::NoAlias(ref_or_family) => {
            anyhow::bail!(
                "model_provider `{ref_or_family}` does not resolve to a configured provider"
            )
        }
        ModelsCommandResolution::Unknown => {
            anyhow::bail!("unknown model_provider `{raw}`")
        }
    }
}

fn resolved_runtime_model_provider_ref(
    config: &Config,
    agent_alias: &str,
) -> anyhow::Result<String> {
    let agent = config
        .agents
        .get(agent_alias)
        .with_context(|| format!("agents.{agent_alias} is not configured"))?;
    let configured = agent.model_provider.trim();
    if configured.is_empty() {
        anyhow::bail!(
            "agents.{agent_alias}.model_provider is empty; runtime reload requires a dotted `<type>.<alias>` provider reference"
        );
    }
    let (model_provider, _) = model_provider_entry_for_ref(config, configured)?;
    Ok(model_provider)
}

fn model_provider_entry_for_ref<'a>(
    config: &'a Config,
    model_provider: &str,
) -> anyhow::Result<(String, &'a zeroclaw_config::schema::ModelProviderConfig)> {
    let trimmed = model_provider.trim();
    if trimmed.is_empty() {
        anyhow::bail!("model_provider reference must not be empty");
    }

    let Some((provider_type, provider_alias)) = trimmed.split_once('.') else {
        anyhow::bail!("model_provider `{trimmed}` must use `<type>.<alias>` form");
    };
    let Some(entry) = config.providers.models.find(provider_type, provider_alias) else {
        anyhow::bail!("model_provider `{trimmed}` does not resolve to a configured provider");
    };
    Ok((trimmed.to_string(), entry))
}

/// Resolve runtime defaults from `config` against a specific dotted
/// `model_provider` reference (`"<type>.<alias>"`) — the per-agent
/// resolution path.
fn runtime_defaults_from_config(
    config: &Config,
    model_provider: &str,
) -> anyhow::Result<ChannelRuntimeDefaults> {
    let (default_model_provider, entry) = model_provider_entry_for_ref(config, model_provider)?;
    let model = entry
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model_provider": model_provider,
                        "reason": "no_model_configured",
                    })),
                "orchestrator: model_provider has no resolvable model"
            );
            anyhow::Error::msg(format!(
                "no model configured: model_provider '{model_provider}' does not resolve to a \
                 ModelProviderConfig with a `model` field, and providers.models has no \
                 fallback entry."
            ))
        })?;
    Ok(ChannelRuntimeDefaults {
        default_model_provider,
        model,
        temperature: entry.temperature,
        api_key: entry.api_key.clone(),
        api_url: entry.uri.clone(),
        reliability: config.reliability.clone(),
    })
}

fn runtime_config_path(ctx: &ChannelRuntimeContext) -> Option<PathBuf> {
    ctx.provider_runtime_options
        .zeroclaw_dir
        .as_ref()
        .map(|dir| dir.join("config.toml"))
}

fn runtime_defaults_snapshot(ctx: &ChannelRuntimeContext) -> ChannelRuntimeDefaultsSnapshot {
    if let Some(runtime_override) = ctx
        .runtime_defaults_override
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return ChannelRuntimeDefaultsSnapshot {
            config: Arc::clone(&runtime_override.config),
            defaults: runtime_override.defaults.clone(),
            hot: true,
            generation: runtime_override.generation,
        };
    }

    ChannelRuntimeDefaultsSnapshot {
        config: Arc::clone(&ctx.prompt_config),
        defaults: ChannelRuntimeDefaults {
            default_model_provider: ctx.model_provider_ref.as_str().to_string(),
            model: ctx.model.as_str().to_string(),
            temperature: ctx.temperature,
            api_key: None,
            api_url: None,
            reliability: (*ctx.reliability).clone(),
        },
        hot: false,
        generation: 0,
    }
}

async fn config_file_stamp(path: &Path) -> Option<ConfigFileStamp> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let modified = metadata.modified().ok()?;
    Some(ConfigFileStamp {
        modified,
        len: metadata.len(),
    })
}

async fn load_runtime_config_and_defaults(
    path: &Path,
    agent_alias: &str,
) -> Result<(Config, ChannelRuntimeDefaults)> {
    let contents = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut parsed: Config = zeroclaw_config::migration::migrate_to_current(&contents)
        .with_context(|| format!("Failed to migrate {}", path.display()))?;
    parsed.config_path = path.to_path_buf();

    if let Some(zeroclaw_dir) = path.parent() {
        let store =
            zeroclaw_runtime::security::SecretStore::new(zeroclaw_dir, parsed.secrets.encrypt);
        parsed.decrypt_secrets(&store)?;
    }
    let applied = zeroclaw_config::env_overrides::apply_env_overrides(&mut parsed)?;
    parsed.env_overridden_paths = applied.paths;
    parsed.pre_override_snapshots = applied.snapshots;

    let model_provider = resolved_runtime_model_provider_ref(&parsed, agent_alias)?;
    let defaults = runtime_defaults_from_config(&parsed, &model_provider)?;
    Ok((parsed, defaults))
}

async fn maybe_apply_runtime_config_update(ctx: &ChannelRuntimeContext) -> Result<()> {
    let Some(config_path) = runtime_config_path(ctx) else {
        return Ok(());
    };

    let Some(stamp) = config_file_stamp(&config_path).await else {
        return Ok(());
    };

    {
        let last = ctx
            .last_applied_config_stamp
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *last == Some(stamp) {
            return Ok(());
        }
    }

    let (next_config, next_defaults) =
        load_runtime_config_and_defaults(&config_path, ctx.agent_alias.as_str()).await?;
    let next_config = Arc::new(next_config);
    let next_options = zeroclaw_providers::options_for_provider_ref(
        next_config.as_ref(),
        &next_defaults.default_model_provider,
        &ctx.provider_runtime_options,
    );
    let model_provider_instance = zeroclaw_providers::create_resilient_model_provider_from_ref(
        next_config.as_ref(),
        &next_defaults.default_model_provider,
        next_defaults.api_key.as_deref(),
        next_defaults.api_url.as_deref(),
        &next_defaults.reliability,
        &next_options,
    )?;
    let model_provider_instance: Arc<dyn ModelProvider> = Arc::from(model_provider_instance);

    if let Err(err) = model_provider_instance.warmup().await {
        if zeroclaw_providers::reliable::is_non_retryable(&err) {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": next_defaults.default_model_provider, "model": next_defaults.model, "err": err.to_string()})), "Rejecting config reload: model not available (non-retryable)");
            return Ok(());
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(
                    ::serde_json::json!({"model_provider": next_defaults.default_model_provider, "err": err.to_string()})
                ),
            "ModelProvider warmup failed after config reload (retryable, applying anyway)"
        );
    }

    {
        let mut override_guard = ctx
            .runtime_defaults_override
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let next_generation = override_guard.as_ref().map_or(1, |runtime_override| {
            runtime_override.generation.saturating_add(1)
        });
        let next_override = Arc::new(ChannelRuntimeOverride {
            config: Arc::clone(&next_config),
            defaults: next_defaults.clone(),
            generation: next_generation,
        });
        let cache_key =
            provider_cache_key(&next_defaults.default_model_provider, None, next_generation);

        let mut cache = ctx.provider_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.clear();
        cache.insert(cache_key, Arc::clone(&model_provider_instance));
        *override_guard = Some(next_override);
    }

    *ctx.last_applied_config_stamp
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(stamp);

    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"path": config_path.display().to_string(), "model_provider": next_defaults.default_model_provider, "model": next_defaults.model, "temperature": next_defaults.temperature, "agent_model_provider": next_defaults.default_model_provider})), "Applied updated channel runtime config from disk");

    Ok(())
}

fn default_route_selection_from_snapshot(
    defaults_snapshot: &ChannelRuntimeDefaultsSnapshot,
) -> ChannelRouteSelection {
    let defaults = defaults_snapshot.defaults.clone();
    ChannelRouteSelection {
        model_provider: defaults.default_model_provider,
        model: defaults.model,
        api_key: None,
    }
}

fn get_route_selection(
    ctx: &ChannelRuntimeContext,
    sender_key: &str,
    defaults_snapshot: &ChannelRuntimeDefaultsSnapshot,
) -> ChannelRouteSelection {
    ctx.route_overrides
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(sender_key)
        .cloned()
        .unwrap_or_else(|| default_route_selection_from_snapshot(defaults_snapshot))
}

fn set_route_selection(
    ctx: &ChannelRuntimeContext,
    sender_key: &str,
    next: ChannelRouteSelection,
    defaults_snapshot: &ChannelRuntimeDefaultsSnapshot,
) {
    let default_route = default_route_selection_from_snapshot(defaults_snapshot);
    let mut routes = ctx
        .route_overrides
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if next == default_route {
        routes.remove(sender_key);
    } else {
        routes.insert(sender_key.to_string(), next);
    }
}

fn clear_sender_history(ctx: &ChannelRuntimeContext, sender_key: &str) {
    ctx.conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop(sender_key);
}

fn mark_sender_for_new_session(ctx: &ChannelRuntimeContext, sender_key: &str) {
    ctx.pending_new_sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(sender_key.to_string());
}

fn take_pending_new_session(ctx: &ChannelRuntimeContext, sender_key: &str) -> bool {
    ctx.pending_new_sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(sender_key)
}

fn replace_available_skills_section(base_prompt: &str, refreshed_skills: &str) -> String {
    const SKILLS_HEADER: &str = "## Available Skills\n\n";
    const SKILLS_END: &str = "</available_skills>";
    const WORKSPACE_HEADER: &str = "## Workspace\n\n";

    if let Some(start) = base_prompt.find(SKILLS_HEADER)
        && let Some(rel_end) = base_prompt[start..].find(SKILLS_END)
    {
        let end = start + rel_end + SKILLS_END.len();
        let tail = base_prompt[end..]
            .strip_prefix("\n\n")
            .unwrap_or(&base_prompt[end..]);

        let mut refreshed = String::with_capacity(
            base_prompt.len().saturating_sub(end.saturating_sub(start))
                + refreshed_skills.len()
                + 2,
        );
        refreshed.push_str(&base_prompt[..start]);
        if !refreshed_skills.is_empty() {
            refreshed.push_str(refreshed_skills);
            refreshed.push_str("\n\n");
        }
        refreshed.push_str(tail);
        return refreshed;
    }

    if refreshed_skills.is_empty() {
        return base_prompt.to_string();
    }

    if let Some(workspace_start) = base_prompt.find(WORKSPACE_HEADER) {
        let mut refreshed = String::with_capacity(base_prompt.len() + refreshed_skills.len() + 2);
        refreshed.push_str(&base_prompt[..workspace_start]);
        refreshed.push_str(refreshed_skills);
        refreshed.push_str("\n\n");
        refreshed.push_str(&base_prompt[workspace_start..]);
        return refreshed;
    }

    format!("{base_prompt}\n\n{refreshed_skills}")
}

fn refreshed_new_session_system_prompt(ctx: &ChannelRuntimeContext) -> String {
    let refreshed_skills = zeroclaw_runtime::skills::skills_to_prompt_with_mode(
        &zeroclaw_runtime::skills::load_skills_with_config(
            ctx.workspace_dir.as_ref(),
            ctx.prompt_config.as_ref(),
        ),
        ctx.workspace_dir.as_ref(),
        ctx.prompt_config.skills.prompt_injection_mode,
    );
    replace_available_skills_section(ctx.system_prompt.as_str(), &refreshed_skills)
}

fn compact_sender_history(ctx: &ChannelRuntimeContext, sender_key: &str) -> bool {
    let mut histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let Some(turns) = histories.get_mut(sender_key) else {
        return false;
    };

    if turns.is_empty() {
        return false;
    }

    let keep_from = turns
        .len()
        .saturating_sub(CHANNEL_HISTORY_COMPACT_KEEP_MESSAGES);
    let mut compacted = normalize_cached_channel_turns(turns[keep_from..].to_vec());

    for turn in &mut compacted {
        if turn.content.chars().count() > CHANNEL_HISTORY_COMPACT_CONTENT_CHARS {
            turn.content =
                truncate_with_ellipsis(&turn.content, CHANNEL_HISTORY_COMPACT_CONTENT_CHARS);
        }
    }

    if compacted.is_empty() {
        turns.clear();
        return false;
    }

    *turns = compacted;
    true
}

/// Proactively trim conversation turns so that the total estimated character
/// count stays within [`PROACTIVE_CONTEXT_BUDGET_CHARS`].  Drops the oldest
/// turns first, but always preserves the most recent turn (the current user
/// message).  Returns the number of turns dropped.
fn proactive_trim_turns(turns: &mut Vec<ChatMessage>, budget: usize) -> usize {
    let total_chars: usize = turns.iter().map(|t| t.content.chars().count()).sum();
    if total_chars <= budget || turns.len() <= 1 {
        return 0;
    }

    let mut excess = total_chars.saturating_sub(budget);
    let mut drop_count = 0;

    // Walk from the oldest turn forward, but never drop the very last turn.
    while excess > 0 && drop_count < turns.len().saturating_sub(1) {
        excess = excess.saturating_sub(turns[drop_count].content.chars().count());
        drop_count += 1;
    }

    if drop_count > 0 {
        turns.drain(..drop_count);
    }
    drop_count
}

fn append_sender_turn(ctx: &ChannelRuntimeContext, sender_key: &str, turn: ChatMessage) {
    // Persist to JSONL before adding to in-memory history.
    if let Some(ref store) = ctx.session_store
        && let Err(e) = store.append(sender_key, &turn)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "Failed to persist session turn"
        );
    }

    // Use the user-configured max_history_messages (fall back to
    // MAX_CHANNEL_HISTORY when the config value is 0 or absent).
    let max_history = {
        let configured = ctx.agent_cfg.resolved.max_history_messages;
        if configured > 0 {
            configured
        } else {
            MAX_CHANNEL_HISTORY
        }
    };

    let mut histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories.get_or_insert_mut(sender_key.to_string(), Vec::new);
    turns.push(turn);
    while turns.len() > max_history {
        turns.remove(0);
    }
}

/// Extract tool-call (assistant with tool_call content) and tool-result
/// messages from the current turn in the LLM history, excluding the final
/// assistant text response.  "Current turn" = everything after the last
/// user-role message.
fn extract_current_turn_tool_messages(history: &[ChatMessage]) -> Vec<ChatMessage> {
    // Find the index of the last user message — tool messages for the
    // current turn come after it.
    let last_user_idx = history.iter().rposition(|m| m.role == "user").unwrap_or(0);

    let tail = &history[last_user_idx + 1..];
    if tail.is_empty() {
        return Vec::new();
    }

    // Everything except the very last assistant message (which is the
    // final text response that gets stored separately).
    let end = if tail.last().is_some_and(|m| m.role == "assistant") {
        tail.len() - 1
    } else {
        tail.len()
    };

    tail[..end]
        .iter()
        .filter(|m| m.role == "assistant" || m.role == "tool")
        .cloned()
        .collect()
}

/// Remove tool-role and intermediate assistant tool-call messages from
/// conversation turns older than the most recent `keep_turns` user→assistant
/// exchanges.  This prevents unbounded history growth while preserving
/// tool context for the N most recent turns.
fn strip_old_tool_context(ctx: &ChannelRuntimeContext, sender_key: &str, keep_turns: usize) {
    let mut histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let Some(turns) = histories.get_mut(sender_key) else {
        return;
    };

    // Walk backwards to find the oldest user message that still belongs to the
    // most recent `keep_turns` exchanges. Everything before that boundary is
    // old enough to strip. If the session has fewer than `keep_turns` user
    // turns, preserve every message.
    let mut user_count = 0;
    let mut strip_before = 0;
    for (i, turn) in turns.iter().enumerate().rev() {
        if turn.role == "user" {
            user_count += 1;
            if user_count == keep_turns {
                strip_before = i;
                break;
            }
        }
    }

    if user_count < keep_turns {
        return;
    }

    // Remove tool and intermediate assistant messages before the boundary.
    // An "intermediate assistant" is one whose content looks like a tool
    // call, either in legacy XML / JSON forms or in native `tool_calls` JSON.
    let mut i = 0;
    while i < strip_before && i < turns.len() {
        let dominated = turns[i].role == "tool"
            || (turns[i].role == "assistant" && is_tool_call_content(&turns[i].content));
        if dominated {
            turns.remove(i);
            // Adjust boundary since we removed an element.
            strip_before = strip_before.saturating_sub(1);
        } else {
            i += 1;
        }
    }
}

/// Heuristic: does this assistant message content represent a tool call
/// rather than a final text response?
fn is_tool_call_content(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.contains("<tool_call>")
        || trimmed.starts_with("{\"tool_call\"")
        || is_named_tool_call_json(trimmed)
        || is_native_tool_call_json(trimmed)
}

fn is_named_tool_call_json(content: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };

    value
        .get("name")
        .and_then(|name| name.as_str())
        .is_some_and(|name| {
            !name.is_empty()
                && (value.get("args").is_some()
                    || value.get("arguments").is_some()
                    || value.get("parameters").is_some())
        })
}

fn is_native_tool_call_json(content: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };

    let Some(tool_calls) = value.get("tool_calls").and_then(|calls| calls.as_array()) else {
        return false;
    };

    !tool_calls.is_empty()
        && tool_calls.iter().all(|call| {
            call.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|name| name.as_str())
                .or_else(|| call.get("name").and_then(|name| name.as_str()))
                .is_some()
        })
}

fn rollback_orphan_user_turn(
    ctx: &ChannelRuntimeContext,
    sender_key: &str,
    expected_content: &str,
) -> bool {
    let mut histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(turns) = histories.get_mut(sender_key) else {
        return false;
    };

    let should_pop = turns
        .last()
        .is_some_and(|turn| turn.role == "user" && turn.content == expected_content);
    if !should_pop {
        return false;
    }

    turns.pop();
    if turns.is_empty() {
        histories.pop(sender_key);
    }

    // Also remove the orphan turn from the persisted JSONL session store so
    // it doesn't resurface after a daemon restart (fixes #3674).
    if let Some(ref store) = ctx.session_store
        && let Err(e) = store.remove_last(sender_key)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "Failed to rollback session store entry"
        );
    }

    true
}

fn should_rollback_failed_user_turn(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<zeroclaw_providers::ProviderCapabilityError>()
        .is_some_and(|capability| capability.capability.eq_ignore_ascii_case("vision"))
    {
        return true;
    }

    zeroclaw_providers::reliable::is_non_retryable(error)
}

fn should_skip_memory_context_entry(key: &str, content: &str) -> bool {
    if zeroclaw_memory::is_assistant_autosave_key(key) {
        return true;
    }

    // Skip raw per-turn user messages: re-injecting them causes each
    // recalled entry to embed all prior generations, growing exponentially.
    // Consolidated knowledge is already promoted to Core/Daily entries.
    if zeroclaw_memory::is_user_autosave_key(key) {
        return true;
    }

    if zeroclaw_memory::should_skip_autosave_content(content) {
        return true;
    }

    if key.trim().to_ascii_lowercase().ends_with("_history") {
        return true;
    }

    // Skip entries containing image markers to prevent duplication.
    // When auto_save stores a photo message to memory, a subsequent
    // memory recall on the same turn would surface the marker again,
    // causing two identical image blocks in the model_provider request.
    if content.contains("[IMAGE:") {
        return true;
    }

    // Skip entries containing tool_result blocks. After a daemon restart
    // these can be recalled from SQLite and injected as memory context,
    // presenting the LLM with a `<tool_result>` without a preceding
    // `<tool_call>` and triggering hallucinated output.
    if content.contains("<tool_result") {
        return true;
    }

    content.chars().count() > MEMORY_CONTEXT_MAX_CHARS
}

fn is_context_window_overflow_error(err: &anyhow::Error) -> bool {
    let lower = err.to_string().to_lowercase();
    [
        "exceeds the context window",
        "context window of this model",
        "maximum context length",
        "context length exceeded",
        "too many tokens",
        "token limit exceeded",
        "prompt is too long",
        "input is too long",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
}

fn load_cached_model_preview(workspace_dir: &Path, provider_name: &str) -> Vec<String> {
    let cache_path = workspace_dir.join("state").join(MODEL_CACHE_FILE);
    let Ok(raw) = std::fs::read_to_string(cache_path) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<ModelCacheState>(&raw) else {
        return Vec::new();
    };

    state
        .entries
        .into_iter()
        .find(|entry| entry.model_provider == provider_name)
        .map(|entry| {
            entry
                .models
                .into_iter()
                .take(MODEL_CACHE_PREVIEW_LIMIT)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Build a cache key that includes the runtime-defaults generation, the
/// model_provider name, and, when a route-specific API key is supplied, a hash
/// of that key. Generation `0` is the immutable startup config, so its key shape
/// stays unchanged; hot-reload generations get isolated cache entries.
fn provider_cache_key(provider_name: &str, route_api_key: Option<&str>, generation: u64) -> String {
    let base = match route_api_key {
        Some(key) => {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut hasher);
            format!("{provider_name}@{:x}", hasher.finish())
        }
        None => provider_name.to_string(),
    };
    if generation == 0 {
        base
    } else {
        format!("g{generation}:{base}")
    }
}

/// Resolve a provider ref's own credentials strictly from its
/// `[providers.models.<type>.<alias>]` entry. No default/global fallback: a
/// provider only ever uses its own `api_key` / `uri`. Returns `(None, None)`
/// for a ref that does not parse or does not resolve, so the provider factory
/// surfaces the misconfiguration instead of silently borrowing another
/// provider's key.
fn provider_credentials_for_ref(
    config: &zeroclaw_config::schema::Config,
    provider_ref: &str,
) -> (Option<String>, Option<String>) {
    let Some((type_key, alias_key)) = provider_ref.trim().split_once('.') else {
        return (None, None);
    };
    config
        .providers
        .models
        .find(type_key, alias_key)
        .map_or((None, None), |entry| {
            (entry.api_key.clone(), entry.uri.clone())
        })
}

async fn get_or_create_provider(
    ctx: &ChannelRuntimeContext,
    provider_name: &str,
    route_api_key: Option<&str>,
    defaults_snapshot: &ChannelRuntimeDefaultsSnapshot,
) -> anyhow::Result<Arc<dyn ModelProvider>> {
    let cache_key = provider_cache_key(provider_name, route_api_key, defaults_snapshot.generation);

    if let Some(existing) = ctx
        .provider_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .cloned()
    {
        return Ok(existing);
    }

    let config = Arc::clone(&defaults_snapshot.config);
    let defaults = defaults_snapshot.defaults.clone();

    // Only return the pre-built startup default model_provider while the
    // current runtime defaults still match startup and there is no
    // route-specific credential override. Once config reload changes defaults,
    // the cache/store path above owns the live default provider.
    if route_api_key.is_none()
        && provider_name == defaults.default_model_provider.as_str()
        && provider_name == ctx.model_provider_ref.as_str()
        && !defaults_snapshot.hot
    {
        return Ok(Arc::clone(&ctx.model_provider));
    }
    // Resolve credentials and URL strictly from the requested provider's own
    // `[providers.models.<type>.<alias>]` entry. There is no global/default
    // fallback: a provider never inherits another provider's api_key or
    // api_url. An unresolved ref yields no credentials so the factory
    // surfaces the misconfiguration.
    let (entry_api_key, entry_api_url) =
        provider_credentials_for_ref(config.as_ref(), provider_name);
    let effective_api_key = route_api_key.map(ToString::to_string).or(entry_api_key);

    let model_provider = create_resilient_model_provider_nonblocking(
        config,
        provider_name,
        effective_api_key,
        entry_api_url,
        defaults.reliability,
        ctx.provider_runtime_options.clone(),
    )
    .await?;
    let model_provider: Arc<dyn ModelProvider> = Arc::from(model_provider);

    if let Err(err) = model_provider.warmup().await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(
                    ::serde_json::json!({"model_provider": provider_name, "err": err.to_string()})
                ),
            "ModelProvider warmup failed"
        );
    }

    let mut cache = ctx.provider_cache.lock().unwrap_or_else(|e| e.into_inner());
    let cached = cache
        .entry(cache_key)
        .or_insert_with(|| Arc::clone(&model_provider));
    Ok(Arc::clone(cached))
}

async fn create_resilient_model_provider_nonblocking(
    config: Arc<zeroclaw_config::schema::Config>,
    provider_name: &str,
    api_key: Option<String>,
    api_url: Option<String>,
    reliability: zeroclaw_config::schema::ReliabilityConfig,
    provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    let provider_name = provider_name.to_string();
    tokio::task::spawn_blocking(move || {
        let options = zeroclaw_providers::options_for_provider_ref(
            &config,
            &provider_name,
            &provider_runtime_options,
        );
        zeroclaw_providers::create_resilient_model_provider_from_ref(
            &config,
            &provider_name,
            api_key.as_deref(),
            api_url.as_deref(),
            &reliability,
            &options,
        )
    })
    .await
    .context("failed to join model_provider initialization task")?
}

fn build_models_help_response(
    current: &ChannelRouteSelection,
    workspace_dir: &Path,
    model_routes: &[zeroclaw_config::schema::ModelRouteConfig],
) -> String {
    let mut response = String::new();
    let _ = writeln!(
        response,
        "Current model_provider: `{}`\nCurrent model: `{}`",
        current.model_provider, current.model
    );
    response.push_str("\nSwitch model with `/model <model-id>` or `/model <hint>`.\n");

    if !model_routes.is_empty() {
        response.push_str("\nConfigured model routes:\n");
        for route in model_routes {
            let _ = writeln!(
                response,
                "  `{}` → {} ({})",
                route.hint, route.model, route.model_provider
            );
        }
    }

    let cached_models = load_cached_model_preview(workspace_dir, &current.model_provider);
    if cached_models.is_empty() {
        let _ = writeln!(
            response,
            "\nNo cached model list found for `{}`. Ask the operator to run `zeroclaw models refresh --model-provider {}`.",
            current.model_provider, current.model_provider
        );
    } else {
        let _ = writeln!(
            response,
            "\nCached model IDs (top {}):",
            cached_models.len()
        );
        for model in cached_models {
            let _ = writeln!(response, "- `{model}`");
        }
    }

    response
}

fn build_providers_help_response(current: &ChannelRouteSelection) -> String {
    let mut response = String::new();
    let _ = writeln!(
        response,
        "Current model_provider: `{}`\nCurrent model: `{}`",
        current.model_provider, current.model
    );
    response.push_str("\nSwitch model_provider with `/models <model_provider>`.\n");
    response.push_str("Switch model with `/model <model-id>`.\n\n");
    response.push_str("Available model model_providers:\n");
    for model_provider in zeroclaw_providers::list_model_providers() {
        let _ = writeln!(response, "- {}", model_provider.name);
    }
    response
}

/// Build a plain-text `/config` response for non-Slack channels.
fn build_config_text_response(
    current: &ChannelRouteSelection,
    _workspace_dir: &Path,
    model_routes: &[zeroclaw_config::schema::ModelRouteConfig],
) -> String {
    let mut resp = String::new();
    let _ = writeln!(
        resp,
        "Current model_provider: `{}`\nCurrent model: `{}`",
        current.model_provider, current.model
    );
    resp.push_str("\nAvailable model_providers:\n");
    for p in zeroclaw_providers::list_model_providers() {
        let _ = writeln!(resp, "- `{}`", p.name);
    }
    if !model_routes.is_empty() {
        resp.push_str("\nConfigured model routes:\n");
        for route in model_routes {
            let _ = writeln!(
                resp,
                "  `{}` -> {} ({})",
                route.hint, route.model, route.model_provider
            );
        }
    }
    resp.push_str(
        "\nUse `/models <model_provider>` to switch model_provider.\nUse `/model <model-id>` to switch model.",
    );
    resp
}

/// Build a Slack Block Kit JSON payload for the `/config` interactive UI.
fn build_config_block_kit(
    current: &ChannelRouteSelection,
    workspace_dir: &Path,
    model_routes: &[zeroclaw_config::schema::ModelRouteConfig],
) -> String {
    let provider_options: Vec<serde_json::Value> = zeroclaw_providers::list_model_providers()
        .iter()
        .map(|p| {
            serde_json::json!({
                "text": { "type": "plain_text", "text": p.display_name },
                "value": p.name
            })
        })
        .collect();

    // Build model options from model_routes + cached models.
    let mut model_options: Vec<serde_json::Value> = model_routes
        .iter()
        .map(|r| {
            let label = if r.hint.is_empty() {
                r.model.clone()
            } else {
                format!("{} ({})", r.model, r.hint)
            };
            serde_json::json!({
                "text": { "type": "plain_text", "text": label },
                "value": r.model
            })
        })
        .collect();

    let cached = load_cached_model_preview(workspace_dir, &current.model_provider);
    for model_id in cached {
        if !model_options.iter().any(|o| {
            o.get("value")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == model_id)
        }) {
            model_options.push(serde_json::json!({
                "text": { "type": "plain_text", "text": model_id },
                "value": model_id
            }));
        }
    }

    // If the current model is not in the list, prepend it.
    if !model_options.iter().any(|o| {
        o.get("value")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == current.model)
    }) {
        model_options.insert(
            0,
            serde_json::json!({
                "text": { "type": "plain_text", "text": &current.model },
                "value": &current.model
            }),
        );
    }

    // Find initial options matching current selection.
    let initial_provider = provider_options
        .iter()
        .find(|o| {
            o.get("value")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == current.model_provider)
        })
        .cloned();

    let initial_model = model_options
        .iter()
        .find(|o| {
            o.get("value")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == current.model)
        })
        .cloned();

    let mut provider_select = serde_json::json!({
        "type": "static_select",
        "action_id": "zeroclaw_config_provider",
        "placeholder": { "type": "plain_text", "text": "Select model_provider" },
        "options": provider_options
    });
    if let Some(init) = initial_provider {
        provider_select["initial_option"] = init;
    }

    let mut model_select = serde_json::json!({
        "type": "static_select",
        "action_id": "zeroclaw_config_model",
        "placeholder": { "type": "plain_text", "text": "Select model" },
        "options": model_options
    });
    if let Some(init) = initial_model {
        model_select["initial_option"] = init;
    }

    let blocks = serde_json::json!([
        {
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": format!(
                    "*Model Configuration*\nCurrent: `{}` / `{}`",
                    current.model_provider, current.model
                )
            }
        },
        {
            "type": "section",
            "block_id": "config_provider_block",
            "text": { "type": "mrkdwn", "text": "*ModelProvider*" },
            "accessory": provider_select
        },
        {
            "type": "section",
            "block_id": "config_model_block",
            "text": { "type": "mrkdwn", "text": "*Model*" },
            "accessory": model_select
        }
    ]);

    blocks.to_string()
}

async fn handle_runtime_command_if_needed(
    ctx: &ChannelRuntimeContext,
    msg: &zeroclaw_api::channel::ChannelMessage,
    target_channel: Option<&Arc<dyn Channel>>,
) -> bool {
    let Some(command) = parse_runtime_command(&msg.channel, &msg.content) else {
        return false;
    };

    let Some(channel) = target_channel else {
        return true;
    };

    let sender_key = conversation_history_key(msg);
    let defaults_snapshot = runtime_defaults_snapshot(ctx);
    let mut current = get_route_selection(ctx, &sender_key, &defaults_snapshot);

    let response = match command {
        ChannelRuntimeCommand::ShowProviders => build_providers_help_response(&current),
        ChannelRuntimeCommand::SetProvider(raw_model_provider) => {
            match resolve_models_command(defaults_snapshot.config.as_ref(), &raw_model_provider) {
                ModelsCommandResolution::Resolved(provider_ref) => {
                    match get_or_create_provider(ctx, &provider_ref, None, &defaults_snapshot).await
                    {
                        Ok(_) => {
                            if provider_ref != current.model_provider {
                                current.model_provider = provider_ref.clone();
                                set_route_selection(
                                    ctx,
                                    &sender_key,
                                    current.clone(),
                                    &defaults_snapshot,
                                );
                            }

                            format!(
                                "ModelProvider switched to `{provider_ref}` for this sender session. Current model is `{}`.\nUse `/model <model-id>` to set a provider-compatible model.",
                                current.model
                            )
                        }
                        Err(err) => {
                            let safe_err = zeroclaw_providers::sanitize_api_error(&err.to_string());
                            format!(
                                "Failed to initialize model_provider `{provider_ref}`. Route unchanged.\nDetails: {safe_err}"
                            )
                        }
                    }
                }
                ModelsCommandResolution::Ambiguous { family, aliases } => {
                    let list = aliases
                        .iter()
                        .map(|a| format!("`{family}.{a}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "ModelProvider `{family}` has multiple configured aliases. Qualify which one with `/models {family}.<alias>`: {list}"
                    )
                }
                ModelsCommandResolution::NoAlias(ref_or_family) => format!(
                    "No configured provider entry for `{ref_or_family}`. Add `[providers.models.{ref_or_family}]` (with its api_key/uri) or select a configured provider — `/models` lists valid ones."
                ),
                ModelsCommandResolution::Unknown => format!(
                    "Unknown model_provider `{raw_model_provider}`. Use `/models` to list valid model_providers."
                ),
            }
        }
        ChannelRuntimeCommand::ShowModel => {
            build_models_help_response(&current, ctx.workspace_dir.as_path(), &ctx.model_routes)
        }
        ChannelRuntimeCommand::SetModel(raw_model) => {
            let model = raw_model.trim().trim_matches('`').to_string();
            if model.is_empty() {
                "Model ID cannot be empty. Use `/model <model-id>`.".to_string()
            } else {
                // Resolve model_provider+model from model_routes (match by model name or hint)
                if let Some(route) = ctx.model_routes.iter().find(|r| {
                    r.model.eq_ignore_ascii_case(&model) || r.hint.eq_ignore_ascii_case(&model)
                }) {
                    current.model_provider = route.model_provider.clone();
                    current.model = route.model.clone();
                    current.api_key = route.api_key.clone();
                } else {
                    current.model = model.clone();
                }
                set_route_selection(ctx, &sender_key, current.clone(), &defaults_snapshot);

                format!(
                    "Model switched to `{}` (model_provider: `{}`). Context preserved.",
                    current.model, current.model_provider
                )
            }
        }
        ChannelRuntimeCommand::ShowConfig => {
            if msg.channel == "slack" {
                let blocks_json = build_config_block_kit(
                    &current,
                    ctx.workspace_dir.as_path(),
                    &ctx.model_routes,
                );
                // Use a magic prefix so SlackChannel::send() can detect Block Kit JSON.
                format!("__ZEROCLAW_BLOCK_KIT__{blocks_json}")
            } else {
                build_config_text_response(&current, ctx.workspace_dir.as_path(), &ctx.model_routes)
            }
        }
        ChannelRuntimeCommand::NewSession => {
            clear_sender_history(ctx, &sender_key);
            if let Some(ref store) = ctx.session_store
                && let Err(e) = store.delete_session(&sender_key)
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "sender_key": sender_key})
                        ),
                    "Failed to delete persisted session for"
                );
            }
            mark_sender_for_new_session(ctx, &sender_key);
            "Conversation history cleared. Starting fresh.".to_string()
        }
    };

    if let Err(err) = channel
        .send(&{
            let mut sm = SendMessage::new(response, &msg.reply_target)
                .in_thread(msg.thread_ts.clone())
                .in_reply_to(Some(msg.id.clone()));
            if let Some(ref subj) = msg.subject {
                let reply_subject = if subj.to_lowercase().starts_with("re:") {
                    subj.clone()
                } else {
                    format!("Re: {}", subj)
                };
                sm = sm.subject(reply_subject);
            }
            sm
        })
        .await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "Failed to send runtime command response on {}: {err}",
                channel.name()
            )
        );
    }

    true
}

async fn build_memory_context(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
) -> String {
    build_memory_context_for_sessions(mem, user_msg, min_relevance_score, &[session_id]).await
}

async fn build_memory_context_for_sessions(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_ids: &[Option<&str>],
) -> String {
    let mut entries = Vec::new();
    let mut seen_keys = HashSet::new();

    match session_ids {
        [] => {}
        [session_id] => {
            let recalled = mem.recall(user_msg, 5, *session_id, None, None).await;
            append_recalled_memory_entries(&mut entries, &mut seen_keys, recalled);
        }
        [first_session_id, second_session_id] => {
            let (first_entries, second_entries) = tokio::join!(
                mem.recall(user_msg, 5, *first_session_id, None, None),
                mem.recall(user_msg, 5, *second_session_id, None, None)
            );
            append_recalled_memory_entries(&mut entries, &mut seen_keys, first_entries);
            append_recalled_memory_entries(&mut entries, &mut seen_keys, second_entries);
        }
        _ => {
            for session_id in session_ids {
                let recalled = mem.recall(user_msg, 5, *session_id, None, None).await;
                append_recalled_memory_entries(&mut entries, &mut seen_keys, recalled);
            }
        }
    }

    format_memory_context(&entries, min_relevance_score)
}

fn append_recalled_memory_entries(
    entries: &mut Vec<zeroclaw_memory::MemoryEntry>,
    seen_keys: &mut HashSet<String>,
    recalled: Result<Vec<zeroclaw_memory::MemoryEntry>>,
) {
    if let Ok(recalled) = recalled {
        for entry in recalled {
            if seen_keys.insert(entry.key.clone()) {
                entries.push(entry);
            }
        }
    }
}

fn format_memory_context(
    entries: &[zeroclaw_memory::MemoryEntry],
    min_relevance_score: f64,
) -> String {
    let mut context = String::new();

    let mut included = 0usize;
    let mut used_chars = 0usize;

    for entry in entries.iter().filter(|e| match e.score {
        Some(score) => score >= min_relevance_score,
        None => true, // keep entries without a score (e.g. non-vector backends)
    }) {
        if included >= MEMORY_CONTEXT_MAX_ENTRIES {
            break;
        }

        if should_skip_memory_context_entry(&entry.key, &entry.content) {
            continue;
        }

        let content = if entry.content.chars().count() > MEMORY_CONTEXT_ENTRY_MAX_CHARS {
            truncate_with_ellipsis(&entry.content, MEMORY_CONTEXT_ENTRY_MAX_CHARS)
        } else {
            entry.content.clone()
        };

        let line = format!("- {}: {}\n", entry.key, content);
        let line_chars = line.chars().count();
        if used_chars + line_chars > MEMORY_CONTEXT_MAX_CHARS {
            break;
        }

        if included == 0 {
            context.push_str(MEMORY_CONTEXT_OPEN);
            context.push('\n');
        }

        context.push_str(&line);
        used_chars += line_chars;
        included += 1;
    }

    if included > 0 {
        context.push_str(MEMORY_CONTEXT_CLOSE);
        context.push_str("\n\n");
    }

    context
}

fn is_group_reply_target(reply_target: &str) -> bool {
    reply_target.contains("@g.us") || reply_target.starts_with("group:")
}

fn sender_memory_session_ids(
    msg: &zeroclaw_api::channel::ChannelMessage,
    history_key: &str,
) -> Vec<String> {
    // Match the sanitized form persisted by memory backend migrations.
    let sanitized_sender = sanitize_session_key(&msg.sender);
    if is_group_reply_target(&msg.reply_target) {
        vec![sanitized_sender]
    } else {
        vec![history_key.to_string(), sanitized_sender]
    }
}

/// Extract a compact summary of tool interactions from history messages added
/// during `run_tool_call_loop`. Scans assistant messages for `<tool_call>` tags
/// or native tool-call JSON to collect tool names used.
/// Returns an empty string when no tools were invoked.
#[cfg(test)]
fn extract_tool_context_summary(history: &[ChatMessage], start_index: usize) -> String {
    fn push_unique_tool_name(tool_names: &mut Vec<String>, name: &str) {
        let candidate = name.trim();
        if candidate.is_empty() {
            return;
        }
        if !tool_names.iter().any(|existing| existing == candidate) {
            tool_names.push(candidate.to_string());
        }
    }

    fn collect_tool_names_from_tool_call_tags(content: &str, tool_names: &mut Vec<String>) {
        const TAG_PAIRS: [(&str, &str); 4] = [
            ("<tool_call>", "</tool_call>"),
            ("<toolcall>", "</toolcall>"),
            ("<tool-call>", "</tool-call>"),
            ("<invoke>", "</invoke>"),
        ];

        for (open_tag, close_tag) in TAG_PAIRS {
            for segment in content.split(open_tag) {
                if let Some(json_end) = segment.find(close_tag) {
                    let json_str = segment[..json_end].trim();
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str)
                        && let Some(name) = val.get("name").and_then(|n| n.as_str())
                    {
                        push_unique_tool_name(tool_names, name);
                    }
                }
            }
        }
    }

    fn collect_tool_names_from_native_json(content: &str, tool_names: &mut Vec<String>) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(content)
            && let Some(calls) = val.get("tool_calls").and_then(|c| c.as_array())
        {
            for call in calls {
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .or_else(|| call.get("name").and_then(|n| n.as_str()));
                if let Some(name) = name {
                    push_unique_tool_name(tool_names, name);
                }
            }
        }
    }

    fn collect_tool_names_from_tool_results(content: &str, tool_names: &mut Vec<String>) {
        let marker = "<tool_result name=\"";
        let mut remaining = content;
        while let Some(start) = remaining.find(marker) {
            let name_start = start + marker.len();
            let after_name_start = &remaining[name_start..];
            if let Some(name_end) = after_name_start.find('"') {
                let name = &after_name_start[..name_end];
                push_unique_tool_name(tool_names, name);
                remaining = &after_name_start[name_end + 1..];
            } else {
                break;
            }
        }
    }

    let mut tool_names: Vec<String> = Vec::new();

    for msg in history.iter().skip(start_index) {
        match msg.role.as_str() {
            "assistant" => {
                collect_tool_names_from_tool_call_tags(&msg.content, &mut tool_names);
                collect_tool_names_from_native_json(&msg.content, &mut tool_names);
            }
            "user" => {
                // Prompt-mode tool calls are always followed by [Tool results] entries
                // containing `<tool_result name="...">` tags with canonical tool names.
                collect_tool_names_from_tool_results(&msg.content, &mut tool_names);
            }
            _ => {}
        }
    }

    if tool_names.is_empty() {
        return String::new();
    }

    format!("[Used tools: {}]", tool_names.join(", "))
}

/// Why the assistant chose not to reply. Drives the chat-surface reaction
/// (👍/🚫/⚠️) on the user's inbound message via `Channel::add_reaction` so a
/// no-reply outcome isn't silent. The LLM classifier emits the kind via a
/// `NO_REPLY[KIND]:` prefix; `Informational` is the default when absent.
/// Channels that don't implement `add_reaction` are silently skipped (the
/// trait default is a no-op `Ok(())`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoReplyKind {
    /// "Got it, no action needed" — informational, social, or
    /// non-addressed messages. Reaction: 👍.
    Informational,
    /// "I will not do this" — safety / policy refusals (prompt injection,
    /// blocked tool, disallowed request). Reaction: 🚫.
    Refused,
    /// "I tried but couldn't fulfil" — external failures, missing
    /// resources, timeouts where the assistant gave up. Reaction: ⚠️.
    Failed,
}

impl NoReplyKind {
    fn emoji(self) -> &'static str {
        match self {
            NoReplyKind::Informational => "👍",
            NoReplyKind::Refused => "🚫",
            NoReplyKind::Failed => "⚠️",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AssistantChannelOutcome {
    Reply(String),
    NoReply {
        kind: NoReplyKind,
        reason: Option<String>,
    },
}

impl AssistantChannelOutcome {
    fn history_marker(&self) -> String {
        match self {
            Self::Reply(text) => text.clone(),
            Self::NoReply {
                reason: Some(reason),
                ..
            } if !reason.trim().is_empty() => {
                format!("[No reply sent: {}]", reason.trim())
            }
            Self::NoReply { .. } => "[No reply sent]".to_string(),
        }
    }
}

async fn classify_channel_reply_intent(
    model_provider: &dyn ModelProvider,
    system_prompt: &str,
    history: &[ChatMessage],
    model: &str,
    temperature: Option<f64>,
) -> anyhow::Result<AssistantChannelOutcome> {
    let mut convo = String::from(
        "Decide whether the assistant should send any visible reply to the latest inbound \
         channel message, and if not, which kind of non-reply it is.\n\nReturn exactly one of:\n\
         - `REPLY`\n\
         - `NO_REPLY[INFO]: <short reason>`   (informational/social, no action needed)\n\
         - `NO_REPLY[REFUSE]: <short reason>` (refused for safety, policy, or prompt injection)\n\
         - `NO_REPLY[FAIL]: <short reason>`   (tried but couldn't fulfil — bad URL, missing file, timeout)\n\
         - `NO_REPLY: <short reason>`         (legacy form; treated as INFO)\n\n\
         Rules:\n\
         - Any call to action from the user MUST be actioned — return `REPLY`. A call to action \
         is a question, request, command, or ask: a message that requires the assistant to do \
         or say something. Being merely named, addressed, or referenced is NOT a call to action \
         on its own (e.g. \"stand by\", \"hold on\", \"thanks bot\" — those are not asks). \
         There is no exception when a real ask is present: memory or prior history showing a \
         similar earlier exchange is NOT grounds to skip the response — the user asked now and \
         is owed a reply now.\n\
         - For everything that is not a call to action, default to `REPLY`. Only emit \
         `NO_REPLY[*]` when one of the categories below clearly applies; when in doubt, `REPLY`.\n\
         - `NO_REPLY[INFO]` is reserved for messages plainly not for the assistant: chatter \
         between other humans in a group channel, system broadcasts, or content the embedded \
         system prompt explicitly tells the assistant to ignore.\n\
         - Output exactly one of the tokens above; emit no other text. The `<short reason>` \
         describes the inbound message — it MUST NOT restate or paraphrase these classifier \
         instructions.\n\nConversation:\n",
    );

    for msg in history.iter().filter(|m| m.role != "system") {
        let role = match msg.role.as_str() {
            "assistant" => "assistant",
            _ => "user",
        };
        // Strip media markers — auxiliary classifier does not need image
        // content, and forwarding `[IMAGE:/local/path]` would reach the
        // provider as a malformed `image_url.url` and trigger 400 errors.
        let safe_content = zeroclaw_providers::multimodal::strip_media_markers(&msg.content);
        let _ = writeln!(convo, "[{role}] {safe_content}");
    }

    let response = model_provider
        .chat_with_system(Some(system_prompt), &convo, model, temperature)
        .await?;
    Ok(parse_reply_intent(&response))
}

/// Parse the classifier's raw output into an `AssistantChannelOutcome`. Pure
/// helper extracted so the LLM-call wrapper has no parsing logic and the
/// kinded `NO_REPLY[...]` forms can be unit-tested without a model_provider.
fn parse_reply_intent(response: &str) -> AssistantChannelOutcome {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: None,
        };
    }
    if trimmed.eq_ignore_ascii_case("REPLY") {
        return AssistantChannelOutcome::Reply(String::new());
    }

    for (tag, kind) in &[
        ("NO_REPLY[INFO]:", NoReplyKind::Informational),
        ("NO_REPLY[REFUSE]:", NoReplyKind::Refused),
        ("NO_REPLY[FAIL]:", NoReplyKind::Failed),
    ] {
        if let Some(reason) = trimmed.strip_prefix(tag) {
            return outcome_for_no_reply(reason.trim(), *kind);
        }
    }

    if let Some(reason) = trimmed.strip_prefix("NO_REPLY:") {
        return outcome_for_no_reply(reason.trim(), NoReplyKind::Informational);
    }
    if trimmed.eq_ignore_ascii_case("NO_REPLY") {
        return AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: None,
        };
    }

    AssistantChannelOutcome::Reply(String::new())
}

/// Resolve a per-agent `classifier_provider` ref to a (provider, model, temperature)
/// triple for `classify_channel_reply_intent`. Returns `None` when the
/// ref is empty or unresolvable; the caller MUST then fall back to the
/// main agent's `active_model_provider` plus the active route/defaults snapshot.
///
/// Per AGENTS.md SINGLE SOURCE OF TRUTH: this function reads the
/// referenced `[providers.models.<type>.<alias>]` entry on every call
/// (no field cache on `ChannelRuntimeContext`). The provider instance
/// itself is deduped through the existing `provider_cache` LRU.
async fn resolve_classifier_route(
    ctx: &ChannelRuntimeContext,
    provider_ref: &zeroclaw_config::providers::ModelProviderRef,
    defaults_snapshot: &ChannelRuntimeDefaultsSnapshot,
) -> Option<(Arc<dyn ModelProvider>, String, Option<f64>)> {
    let provider_str = provider_ref.as_str().trim();
    if provider_str.is_empty() {
        return None;
    }

    let (type_key, alias_key) = match provider_str.split_once('.') {
        Some(parts) => parts,
        None => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"provider": provider_str})),
                "classifier_provider must be dotted `<type>.<alias>`; falling back to main agent"
            );
            return None;
        }
    };

    let model_cfg = match defaults_snapshot
        .config
        .providers
        .models
        .find(type_key, alias_key)
    {
        Some(cfg) => cfg,
        None => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"provider": provider_str})),
                "classifier_provider references an unknown [providers.models.<type>.<alias>] entry; falling back to main agent"
            );
            return None;
        }
    };

    let model = model_cfg.model.clone().unwrap_or_default();
    let temperature = model_cfg.temperature;
    if model.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"provider": provider_str})),
            "classifier_provider points to a [providers.models] entry without a `model` field; falling back to main agent"
        );
        return None;
    }

    let provider = match get_or_create_provider(
        ctx,
        provider_str,
        model_cfg.api_key.as_deref(),
        defaults_snapshot,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let safe_err = zeroclaw_providers::sanitize_api_error(&e.to_string());
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"provider": provider_str, "error": safe_err})),
                "Failed to initialize classifier_provider; falling back to main agent provider"
            );
            return None;
        }
    };

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"provider": provider_str, "model": model.as_str()})),
        "classifier_provider override active"
    );

    Some((provider, model, temperature))
}

/// Build the `NoReply` outcome, with a narrow rubric-echo failsafe scoped to
/// the `Informational` kind only. When the classifier emits `NO_REPLY[INFO]`
/// with a reason that restates its own rubric (the only failure mode observed
/// in production after PR #6112), it has failed to actually classify the
/// inbound message — falling through to `Reply` is the safe asymmetry there,
/// since the alternative is silently swallowing a legitimate user message.
///
/// `Refused` and `Failed` are explicit safety routing decisions (e.g. the
/// classifier flagged a prompt-injection attempt or a hard failure), so we
/// respect them verbatim even when the reason text happens to quote
/// rubric-like phrases — converting those to `Reply` would re-enter the
/// tool-capable agent path and skip the refusal/failure recording surface.
fn outcome_for_no_reply(reason: &str, kind: NoReplyKind) -> AssistantChannelOutcome {
    if matches!(kind, NoReplyKind::Informational) && looks_like_meta_instruction_echo(reason) {
        return AssistantChannelOutcome::Reply(String::new());
    }
    AssistantChannelOutcome::NoReply {
        kind,
        reason: (!reason.is_empty()).then(|| reason.to_string()),
    }
}

/// True when the no-reply reason restates the classifier's own instructions
/// rather than describing the inbound message. Observed failure mode after
/// the classifier prompt rewrite in PR #6112: outputs like `NO_REPLY[INFO]:
/// classification task only — must not answer the user.` where the "reason"
/// is verbatim rubric text. Substring match is intentionally narrow — these
/// phrases almost never appear in genuine descriptions of an inbound
/// message, while the false-negative cost (suppressing a real user reply)
/// is high.
fn looks_like_meta_instruction_echo(reason: &str) -> bool {
    if reason.is_empty() {
        return false;
    }
    let lower = reason.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "classification task",
        "only classify",
        "must not answer",
        "not answering the user",
        "do not answer the user",
        "do not reply to the user",
        "classifier instruction",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Strip `<think>...</think>` blocks from streaming draft text so reasoning
/// tokens are never shown to the user in partial updates.
fn strip_think_tags_inline(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        if let Some(start) = rest.find("<think>") {
            result.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find("</think>") {
                rest = &rest[start + end + "</think>".len()..];
            } else {
                // Unclosed tag: drop the tail to avoid leaking partial reasoning.
                break;
            }
        } else {
            result.push_str(rest);
            break;
        }
    }
    result.trim().to_string()
}

fn starts_with_visible_tool_call_tag_example(response: &str) -> bool {
    let lower = response.trim_start().to_ascii_lowercase();
    let starts_with_tool_tag = lower.starts_with("<tool_call")
        || lower.starts_with("<toolcall")
        || lower.starts_with("<tool-call")
        || lower.starts_with("<invoke");

    starts_with_tool_tag && zeroclaw_tool_call_parser::looks_like_tool_protocol_example(response)
}

fn should_suppress_top_level_tool_protocol_response(
    response: &str,
    known_tool_names: &HashSet<String>,
) -> bool {
    if zeroclaw_tool_call_parser::looks_like_tool_protocol_example(response) {
        return false;
    }

    if zeroclaw_tool_call_parser::looks_like_malformed_tool_protocol_envelope_for_known_tools(
        response,
        known_tool_names,
    ) {
        return true;
    }

    if let Some(kind) = zeroclaw_tool_call_parser::classify_tool_protocol_envelope(response) {
        return matches!(
            kind,
            zeroclaw_tool_call_parser::ToolProtocolEnvelopeKind::TaggedToolCall
        ) || (!known_tool_names.is_empty()
            && (matches!(
                kind,
                zeroclaw_tool_call_parser::ToolProtocolEnvelopeKind::ToolResult
            ) || zeroclaw_tool_call_parser::tool_protocol_envelope_mentions_known_tool(
                response,
                known_tool_names,
            )));
    }

    // If the broad envelope detector still matches after classification failed,
    // this is malformed internal protocol JSON rather than ordinary content.
    zeroclaw_tool_call_parser::looks_like_tool_protocol_envelope(response)
}

fn sanitize_channel_response(response: &str, tools: &[Box<dyn Tool>]) -> String {
    let known_tool_names: HashSet<String> = tools
        .iter()
        .map(|tool| tool.name().to_ascii_lowercase())
        .collect();
    // Strip any [Used tools: ...] prefix that the LLM may have echoed from
    // history context. Trim first to handle leading/trailing whitespace.
    let trimmed_response = response.trim();
    let trimmed_response = strip_think_tags_inline(trimmed_response).trim().to_string();
    let trimmed_response = trimmed_response.as_str();
    // Final channel guardrail: reuse the parser classifier so channel cleanup
    // cannot drift from runtime tool-protocol detection.
    if should_suppress_top_level_tool_protocol_response(trimmed_response, &known_tool_names) {
        return String::new();
    }
    let stripped_summary = strip_tool_summary_prefix(trimmed_response);
    let stripped_xml = if starts_with_visible_tool_call_tag_example(&stripped_summary) {
        stripped_summary
    } else {
        strip_tool_call_tags(&stripped_summary)
    };
    let stripped_results = strip_tool_result_content(&stripped_xml);
    let stripped_fenced_json =
        strip_fenced_tool_protocol_artifacts(&stripped_results, &known_tool_names);
    let stripped_json =
        strip_isolated_tool_json_artifacts(&stripped_fenced_json, &known_tool_names);
    // Strip leading narration lines that announce tool usage
    let sanitized = strip_tool_narration(&stripped_json);

    // Scan for credential leaks before returning to caller
    match zeroclaw_runtime::security::LeakDetector::new().scan(&sanitized) {
        zeroclaw_runtime::security::LeakResult::Clean => sanitized,
        zeroclaw_runtime::security::LeakResult::Detected { patterns, redacted } => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"patterns": patterns})),
                "output guardrail: credential leak detected in outbound channel response"
            );
            redacted
        }
    }
}

/// Shown when the agent turn completes but no visible text remains after sanitization.
const EMPTY_CHANNEL_REPLY_FALLBACK: &str =
    "I couldn't produce a visible reply for that message. Please try again.";

/// Ensure channel outbound text is never empty so users don't see typing with no message.
fn ensure_nonempty_channel_reply(
    delivered_response: String,
    outbound_response: &str,
    channel: &str,
    reply_target: &str,
) -> String {
    if !delivered_response.trim().is_empty() {
        return delivered_response;
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "channel": channel,
                "reply_target": reply_target,
                "outbound_len": outbound_response.len(),
            })),
        "channel_reply_empty; substituting fallback"
    );
    EMPTY_CHANNEL_REPLY_FALLBACK.to_string()
}

/// Remove leading lines that narrate tool usage (e.g. "Let me check the weather for you.").
///
/// Only strips lines from the very beginning of the message that match common
/// narration patterns, so genuine content is preserved.
fn strip_tool_narration(message: &str) -> String {
    let narration_prefixes: &[&str] = &[
        "let me ",
        "i'll ",
        "i will ",
        "i am going to ",
        "i'm going to ",
        "searching ",
        "looking up ",
        "fetching ",
        "checking ",
        "using the ",
        "using my ",
        "one moment",
        "hold on",
        "just a moment",
        "give me a moment",
        "allow me to ",
    ];

    let mut result_lines: Vec<&str> = Vec::new();
    let mut past_narration = false;

    for line in message.lines() {
        if past_narration {
            result_lines.push(line);
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if narration_prefixes.iter().any(|p| lower.starts_with(p)) {
            // Skip this narration line
            continue;
        }
        // First non-narration, non-empty line — keep everything from here
        past_narration = true;
        result_lines.push(line);
    }

    let joined = result_lines.join("\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() && !message.trim().is_empty() {
        // If stripping removed everything, return original to avoid empty reply
        message.to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_tool_call_payload(value: &serde_json::Value, known_tool_names: &HashSet<String>) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };

    let (name, has_args) =
        if let Some(function) = object.get("function").and_then(|f| f.as_object()) {
            (
                function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| object.get("name").and_then(|v| v.as_str())),
                function.contains_key("arguments")
                    || function.contains_key("parameters")
                    || object.contains_key("arguments")
                    || object.contains_key("parameters"),
            )
        } else {
            (
                object.get("name").and_then(|v| v.as_str()),
                object.contains_key("arguments") || object.contains_key("parameters"),
            )
        };

    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        return false;
    };

    has_args && known_tool_names.contains(&name.to_ascii_lowercase())
}

fn is_tool_result_payload(
    object: &serde_json::Map<String, serde_json::Value>,
    saw_tool_call_payload: bool,
) -> bool {
    if !saw_tool_call_payload || !object.contains_key("result") {
        return false;
    }

    object.keys().all(|key| {
        matches!(
            key.as_str(),
            "result" | "id" | "tool_call_id" | "name" | "tool"
        )
    })
}

fn sanitize_tool_json_value(
    value: &serde_json::Value,
    known_tool_names: &HashSet<String>,
    saw_tool_call_payload: bool,
) -> Option<(String, bool)> {
    if let Some(kind) =
        zeroclaw_tool_call_parser::classify_tool_protocol_envelope(&value.to_string())
    {
        if known_tool_names.is_empty() {
            return None;
        }

        if matches!(
            kind,
            zeroclaw_tool_call_parser::ToolProtocolEnvelopeKind::ToolResult
        ) {
            return Some((String::new(), true));
        }

        if !zeroclaw_tool_call_parser::tool_protocol_envelope_mentions_known_tool(
            &value.to_string(),
            known_tool_names,
        ) {
            return None;
        }

        let content = safe_protocol_envelope_content(value);
        return Some((content, true));
    }

    if is_tool_call_payload(value, known_tool_names) {
        return Some((String::new(), true));
    }

    if let Some(array) = value.as_array() {
        if !array.is_empty()
            && array
                .iter()
                .all(|item| is_tool_call_payload(item, known_tool_names))
        {
            return Some((String::new(), true));
        }
        return None;
    }

    let object = value.as_object()?;

    if let Some(tool_calls) = object.get("tool_calls").and_then(|value| value.as_array())
        && !tool_calls.is_empty()
        && tool_calls
            .iter()
            .all(|call| is_tool_call_payload(call, known_tool_names))
    {
        let content = object
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        return Some((content, true));
    }

    if is_tool_result_payload(object, saw_tool_call_payload) {
        return Some((String::new(), false));
    }

    None
}

fn safe_protocol_envelope_content(value: &serde_json::Value) -> String {
    let content = value
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();

    if content.is_empty()
        || zeroclaw_tool_call_parser::looks_like_tool_protocol_envelope(content)
        || zeroclaw_tool_call_parser::looks_like_malformed_tool_protocol_envelope(content)
    {
        return String::new();
    }

    content.to_string()
}

fn is_line_isolated_json_segment(message: &str, start: usize, end: usize) -> bool {
    let line_start = message[..start].rfind('\n').map_or(0, |idx| idx + 1);
    let line_end = message[end..]
        .find('\n')
        .map_or(message.len(), |idx| end + idx);

    message[line_start..start].trim().is_empty() && message[end..line_end].trim().is_empty()
}

fn is_inside_markdown_code_fence(message: &str, index: usize) -> bool {
    // This intentionally uses a lightweight fence parity check. The sanitizer only
    // needs to avoid re-processing JSON in ordinary triple-backtick fences that
    // `strip_fenced_tool_protocol_artifacts` already handles; it is not a full
    // Markdown parser for inline code spans or longer fence runs.
    let mut in_fence = false;
    let mut cursor = 0usize;
    while let Some(rel_pos) = message[cursor..index].find("```") {
        in_fence = !in_fence;
        cursor += rel_pos + 3;
    }
    in_fence
}

fn isolated_malformed_tool_protocol_segment_end(
    message: &str,
    start: usize,
    known_tool_names: &HashSet<String>,
) -> Option<usize> {
    let line_start = message[..start].rfind('\n').map_or(0, |idx| idx + 1);
    if !message[line_start..start].trim().is_empty() {
        return None;
    }

    let mut end = start;
    // Malformed JSON has no serde byte offset. Scan forward from an isolated
    // JSON candidate start, but stop before ordinary prose resumes.
    for line in message[start..].split_inclusive('\n') {
        let trimmed = line.trim();
        if end > start
            && !trimmed.is_empty()
            && !trimmed.starts_with(['{', '[', ']', '}'])
            && !trimmed.starts_with('"')
        {
            break;
        }
        end += line.len();
        let candidate = &message[start..end];
        if zeroclaw_tool_call_parser::looks_like_malformed_tool_protocol_envelope_for_known_tools(
            candidate,
            known_tool_names,
        ) {
            return Some(end);
        }
    }

    None
}

fn is_tool_protocol_fence_language(language: &str) -> bool {
    let lower = language.trim().to_ascii_lowercase();
    lower == "tool_call"
        || lower == "toolcall"
        || lower == "tool-call"
        || lower == "invoke"
        || lower
            .strip_prefix("tool")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace) && !rest.trim().is_empty())
}

fn strip_fenced_tool_protocol_artifacts(
    message: &str,
    known_tool_names: &HashSet<String>,
) -> String {
    if zeroclaw_tool_call_parser::looks_like_tool_protocol_example(message) {
        return message.to_string();
    }

    let mut cleaned = String::with_capacity(message.len());
    let mut cursor = 0usize;

    while let Some(rel_open) = message[cursor..].find("```") {
        let open_start = cursor + rel_open;
        let language_start = open_start + 3;
        let Some(line_end_rel) = message[language_start..].find('\n') else {
            break;
        };
        let line_end = language_start + line_end_rel;
        let language = message[language_start..line_end]
            .trim()
            .trim_end_matches('\r');
        let body_start = line_end + 1;
        let Some(close_rel) = message[body_start..].find("```") else {
            break;
        };
        let close_start = body_start + close_rel;
        let close_end = close_start + 3;

        let fence_block = &message[open_start..close_end];
        let should_strip = if language.eq_ignore_ascii_case("json") {
            should_suppress_top_level_tool_protocol_response(
                message[body_start..close_start].trim(),
                known_tool_names,
            )
        } else {
            is_tool_protocol_fence_language(language)
                && zeroclaw_tool_call_parser::contains_tool_protocol_tag_call(fence_block)
        };

        if should_strip {
            cleaned.push_str(&message[cursor..open_start]);
            cursor = close_end;
            continue;
        }

        cleaned.push_str(&message[cursor..close_end]);
        cursor = close_end;
    }

    cleaned.push_str(&message[cursor..]);
    cleaned
}

fn strip_isolated_tool_json_artifacts(message: &str, known_tool_names: &HashSet<String>) -> String {
    let mut cleaned = String::with_capacity(message.len());
    let mut cursor = 0usize;
    let mut saw_tool_call_payload = false;

    while cursor < message.len() {
        let Some(rel_start) = message[cursor..].find(['{', '[']) else {
            cleaned.push_str(&message[cursor..]);
            break;
        };

        let start = cursor + rel_start;
        cleaned.push_str(&message[cursor..start]);
        if is_inside_markdown_code_fence(message, start) {
            let Some(ch) = message[start..].chars().next() else {
                break;
            };
            cleaned.push(ch);
            cursor = start + ch.len_utf8();
            continue;
        }

        let candidate = &message[start..];
        let mut stream =
            serde_json::Deserializer::from_str(candidate).into_iter::<serde_json::Value>();

        if let Some(Ok(value)) = stream.next() {
            let consumed = stream.byte_offset();
            if consumed > 0 {
                let end = start + consumed;
                if is_line_isolated_json_segment(message, start, end)
                    && let Some((replacement, marks_tool_call)) =
                        sanitize_tool_json_value(&value, known_tool_names, saw_tool_call_payload)
                {
                    if marks_tool_call {
                        saw_tool_call_payload = true;
                    }
                    if !replacement.trim().is_empty() {
                        cleaned.push_str(replacement.trim());
                    }
                    cursor = end;
                    continue;
                }
            }
        }

        if let Some(end) =
            isolated_malformed_tool_protocol_segment_end(message, start, known_tool_names)
        {
            cursor = end;
            continue;
        }

        let Some(ch) = message[start..].chars().next() else {
            break;
        };
        cleaned.push(ch);
        cursor = start + ch.len_utf8();
    }

    let mut result = cleaned.replace("\r\n", "\n");
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}

fn spawn_supervised_listener(
    ch: Arc<dyn Channel>,
    alias: Option<String>,
    tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_supervised_listener_with_health_interval(
        ch,
        alias,
        tx,
        initial_backoff_secs,
        max_backoff_secs,
        Duration::from_secs(CHANNEL_HEALTH_HEARTBEAT_SECS),
        cancel,
    )
}

fn spawn_supervised_listener_with_health_interval(
    ch: Arc<dyn Channel>,
    alias: Option<String>,
    tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    health_interval: Duration,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let health_interval = if health_interval.is_zero() {
        Duration::from_secs(1)
    } else {
        health_interval
    };

    let composite = match alias.as_deref() {
        Some(a) if !a.is_empty() => format!("{}.{}", ch.name(), a),
        _ => ch.name().to_string(),
    };
    let span = zeroclaw_log::attribution_span!(&*ch);
    zeroclaw_spawn::spawn!(
        async move {
            let component = format!("channel:{composite}");
            let mut backoff = initial_backoff_secs.max(1);
            let max_backoff = max_backoff_secs.max(backoff);

            loop {
                zeroclaw_runtime::health::mark_component_ok(&component);
                let mut health = tokio::time::interval(health_interval);
                health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let result = {
                    let listen_future = ch.listen(tx.clone());
                    tokio::pin!(listen_future);

                    loop {
                        tokio::select! {
                            () = cancel.cancelled() => return,
                            _ = health.tick() => {
                                zeroclaw_runtime::health::mark_component_ok(&component);
                            }
                            result = &mut listen_future => break result,
                        }
                    }
                };

                match result {
                    Ok(()) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!("Channel {} exited unexpectedly; restarting", ch.name())
                        );
                        zeroclaw_runtime::health::mark_component_error(
                            &component,
                            "listener exited unexpectedly",
                        );
                        backoff = initial_backoff_secs.max(1);
                    }
                    Err(e) => {
                        if is_non_retryable_channel_listener_error(ch.name(), &e) {
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Reject
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "channel listener hit non-retryable error; waiting for config change or shutdown"
                            );
                            zeroclaw_runtime::health::mark_component_error(&component, e.to_string());
                            tokio::select! {
                                () = cancel.cancelled() => return,
                                () = std::future::pending::<()>() => unreachable!(),
                            }
                        }
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "channel listener error; restarting"
                        );
                        zeroclaw_runtime::health::mark_component_error(&component, e.to_string());
                    }
                }

                zeroclaw_runtime::health::bump_component_restart(&component);
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                }
                backoff = backoff.saturating_mul(2).min(max_backoff);
            }
        }
        .instrument(span)
    )
}

fn is_non_retryable_channel_listener_error(channel_name: &str, error: &anyhow::Error) -> bool {
    match channel_name {
        name if name == "discord" || name.starts_with("discord-") => {
            #[cfg(feature = "channel-discord")]
            if error
                .downcast_ref::<crate::discord::DiscordListenerFatalError>()
                .is_some()
            {
                return true;
            }
            zeroclaw_providers::reliable::is_non_retryable(error)
        }
        _ => false,
    }
}

fn compute_max_in_flight_messages(
    channel_count: usize,
    max_concurrent_per_channel: usize,
) -> usize {
    channel_count
        .saturating_mul(max_concurrent_per_channel)
        .clamp(
            CHANNEL_MIN_IN_FLIGHT_MESSAGES,
            CHANNEL_MAX_IN_FLIGHT_MESSAGES,
        )
}

fn max_in_flight_messages_for_config(
    channel_count: usize,
    config: &zeroclaw_config::schema::ChannelsConfig,
) -> usize {
    compute_max_in_flight_messages(channel_count, config.max_concurrent_per_channel)
}

fn log_worker_join_result(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"error": format!("{}", error)})),
            "Channel message worker crashed"
        );
    }
}

fn spawn_scoped_typing_task(
    channel: Arc<dyn Channel>,
    recipient: String,
    cancellation_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let stop_signal = cancellation_token;
    let refresh_interval = Duration::from_secs(CHANNEL_TYPING_REFRESH_INTERVAL_SECS);
    zeroclaw_spawn::spawn!(async move {
        let mut interval = tokio::time::interval(refresh_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                () = stop_signal.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(e) = channel.start_typing(&recipient).await {
                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "failed to start typing");
                    }
                }
            }
        }

        if let Err(e) = channel.stop_typing(&recipient).await {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to stop typing"
            );
        }
    })
}

async fn process_channel_message(
    ctx: Arc<ChannelRuntimeContext>,
    msg: zeroclaw_api::channel::ChannelMessage,
    cancellation_token: CancellationToken,
) {
    if cancellation_token.is_cancelled() {
        return;
    }

    let channel_composite = match &msg.channel_alias {
        Some(alias) => format!("{}.{}", msg.channel, alias),
        None => msg.channel.clone(),
    };
    let agent_alias = Arc::clone(&ctx.agent_alias);
    let sender = msg.sender.clone();
    let message_id = msg.id.clone();
    let composite_for_body = channel_composite.clone();
    zeroclaw_log::scope!(
        category: "channel",
        agent_alias: agent_alias.as_str(),
        channel: channel_composite.as_str(),
        sender: sender.as_str(),
        message_id: message_id.as_str(),
        => async move {
            process_channel_message_body(ctx, msg, cancellation_token, composite_for_body).await;
        }
    )
    .await;
}

async fn process_channel_message_body(
    ctx: Arc<ChannelRuntimeContext>,
    msg: zeroclaw_api::channel::ChannelMessage,
    cancellation_token: CancellationToken,
    channel_composite: String,
) {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Inbound).with_attrs(
            ::serde_json::json!({
                "sender": msg.sender,
                "message_id": msg.id,
                "reply_target": msg.reply_target,
                "thread_ts": msg.thread_ts,
                "content": msg.content,
                "attachments_count": msg.attachments.len(),
            })
        ),
        "channel inbound message"
    );

    // ── Hook: on_message_received (modifying) ────────────
    let mut msg = if let Some(hooks) = &ctx.hooks {
        match hooks.run_on_message_received(msg).await {
            zeroclaw_runtime::hooks::HookResult::Cancel(reason) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"reason": reason.to_string()})),
                    "incoming message dropped by hook"
                );
                return;
            }
            zeroclaw_runtime::hooks::HookResult::Continue(modified) => modified,
        }
    } else {
        msg
    };

    // ── Media pipeline: enrich inbound message with media annotations ──
    if ctx.media_pipeline.enabled && !msg.attachments.is_empty() {
        let vision = ctx.model_provider.supports_vision();
        let transcription_manager =
            crate::transcription::TranscriptionManager::new(&ctx.transcription_config)
                .ok()
                .map(|m| {
                    m.with_agent_transcription_provider(ctx.agent_transcription_provider.clone())
                });
        let pipeline = media_pipeline::MediaPipeline::new(
            &ctx.media_pipeline,
            transcription_manager.as_ref(),
            vision,
        );
        msg.content = Box::pin(pipeline.process(&msg.content, &msg.attachments)).await;
    }

    // ── Link enricher: prepend URL summaries before agent sees the message ──
    let le_config = &ctx.prompt_config.link_enricher;
    if le_config.enabled {
        let enricher_cfg = link_enricher::LinkEnricherConfig {
            enabled: le_config.enabled,
            max_links: le_config.max_links,
            timeout_secs: le_config.timeout_secs,
        };
        let enriched = link_enricher::enrich_message(&msg.content, &enricher_cfg).await;
        if enriched != msg.content {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "Link enricher: prepended URL summaries to message"
            );
            msg.content = enriched;
        }
    }

    let target_channel = find_channel_for_message(&ctx.channels_by_name, &msg).cloned();

    // Self-loop guard, two-layer.
    //
    // Layer 1 — SDK side: channels that expose `Channel::self_handle()`
    // get caught here.
    //
    // Layer 2 — agent-loop fallback: even when the channel returned a
    // handle and Layer 1 ran, re-check via the shared
    // `peers::should_drop_self_loop` helper using the same handle. The
    // fallback exists so a channel impl that gains its
    // self-identity later in its lifecycle (after Layer 1's check
    // fired with `None`) still has a guard available; both layers use
    // identical normalization so they agree on what "self" means.
    if let Some(channel) = target_channel.as_ref() {
        if channel.drop_self_messages(&msg) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "dropping self-authored inbound message (self-loop guard, sdk layer)"
            );
            return;
        }
        if zeroclaw_runtime::peers::should_drop_self_loop(
            &msg.sender,
            channel.self_handle().as_deref(),
        ) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "dropping self-authored inbound message (self-loop guard, agent-loop fallback)"
            );
            return;
        }
    }

    if let Err(err) = maybe_apply_runtime_config_update(ctx.as_ref()).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
            "Failed to apply runtime config update"
        );
    }
    if handle_runtime_command_if_needed(ctx.as_ref(), &msg, target_channel.as_ref()).await {
        return;
    }

    let history_key = conversation_history_key(&msg);
    if let Some(ref store) = ctx.session_store {
        let channel_id = msg
            .channel_alias
            .as_deref()
            .map(|alias| format!("{}.{alias}", msg.channel));
        let room_id = msg
            .thread_ts
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let target = msg.reply_target.trim();
                if target.is_empty() {
                    None
                } else {
                    Some(target)
                }
            });
        let context = zeroclaw_infra::session_backend::SessionContext {
            channel_id: channel_id.as_deref(),
            room_id,
            sender_id: Some(msg.sender.as_str()).filter(|s| !s.is_empty()),
        };
        if let Err(e) = store.set_session_context(&history_key, context) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"history_key": history_key, "e": e.to_string()})
                    ),
                "Failed to stamp session routing context"
            );
        }
    }
    let runtime_defaults = runtime_defaults_snapshot(ctx.as_ref());
    let mut route = get_route_selection(ctx.as_ref(), &history_key, &runtime_defaults);

    // ── Query classification: override route when a rule matches ──
    if let Some(hint) =
        zeroclaw_runtime::agent::classifier::classify(&ctx.query_classification, &msg.content)
        && let Some(matched_route) = ctx
            .model_routes
            .iter()
            .find(|r| r.hint.eq_ignore_ascii_case(&hint))
    {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hint": hint.as_str(), "model_provider": matched_route.model_provider.as_str(), "model": matched_route.model.as_str()})), "Channel message classified — overriding route");
        route = ChannelRouteSelection {
            model_provider: matched_route.model_provider.clone(),
            model: matched_route.model.clone(),
            api_key: matched_route.api_key.clone(),
        };
    }

    let mut active_model_provider = match get_or_create_provider(
        ctx.as_ref(),
        &route.model_provider,
        route.api_key.as_deref(),
        &runtime_defaults,
    )
    .await
    {
        Ok(model_provider) => model_provider,
        Err(err) => {
            let safe_err = zeroclaw_providers::sanitize_api_error(&err.to_string());
            let message = format!(
                "⚠️ Failed to initialize model_provider `{}`. Please run `/models` to choose another model_provider.\nDetails: {safe_err}",
                route.model_provider
            );
            if let Some(channel) = target_channel.as_ref() {
                let _ = channel.send(&SendMessage::reply_to(&msg, message)).await;
            }
            return;
        }
    };
    let history_user_content = channel_history_content_for_user_turn(&msg.content);
    if ctx.auto_save_memory
        && history_user_content.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
        && !zeroclaw_memory::should_skip_autosave_content(&history_user_content)
    {
        let autosave_key = conversation_memory_key(&msg);
        let _ = ctx
            .memory
            .store(
                &autosave_key,
                &history_user_content,
                zeroclaw_memory::MemoryCategory::Conversation,
                Some(&history_key),
            )
            .await;
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"message_id": msg.id})),
        "processing inbound message"
    );
    let started_at = Instant::now();

    let force_fresh_session = take_pending_new_session(ctx.as_ref(), &history_key);
    if force_fresh_session {
        // `/new` should make the next user turn completely fresh even if
        // older cached turns reappear before this message starts.
        clear_sender_history(ctx.as_ref(), &history_key);
    }

    let had_prior_history = if force_fresh_session {
        false
    } else {
        ctx.conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(&history_key)
            .is_some_and(|turns| !turns.is_empty())
    };

    // Preserve the dated user turn before the LLM call so interrupted requests
    // keep the same temporal context as CLI turns. Keep history compact so
    // inline image payloads are not reloaded into later requests.
    let timestamped_content = timestamp_channel_user_content(&msg.content);
    let timestamped_history_content = timestamp_channel_user_content(&history_user_content);
    append_sender_turn(
        ctx.as_ref(),
        &history_key,
        ChatMessage::user(&timestamped_history_content),
    );

    // Build history from per-sender conversation cache.
    let mut prior_turns_raw = if force_fresh_session {
        vec![ChatMessage::user(&timestamped_content)]
    } else {
        ctx.conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&history_key)
            .cloned()
            .unwrap_or_default()
    };
    if !force_fresh_session {
        restore_current_user_turn_media_payload(
            &mut prior_turns_raw,
            &timestamped_history_content,
            &timestamped_content,
        );
    }
    let mut prior_turns = normalize_cached_channel_turns(prior_turns_raw);

    // Strip stale tool_result blocks from cached turns so the LLM never
    // sees a `<tool_result>` without a preceding `<tool_call>`, which
    // causes hallucinated output on subsequent heartbeat ticks or sessions.
    for turn in &mut prior_turns {
        if turn.content.contains("<tool_result") {
            turn.content = strip_tool_result_content(&turn.content);
        }
    }

    // Strip [Used tools: ...] prefixes from cached assistant turns so the
    // LLM never sees (and reproduces) this internal summary format.
    for turn in &mut prior_turns {
        if turn.role == "assistant" && turn.content.starts_with("[Used tools:") {
            turn.content = strip_tool_summary_prefix(&turn.content);
        }
    }

    // Strip [IMAGE:] markers from older cached turns before context
    // compression. The current message keeps its full payload so vision still
    // works for the active turn; historical turns keep compact annotations.
    strip_historical_image_payloads(&mut prior_turns);

    // Proactively trim conversation history before sending to the model_provider
    // to prevent context-window-exceeded errors (bug #3460).
    let dropped = proactive_trim_turns(&mut prior_turns, PROACTIVE_CONTEXT_BUDGET_CHARS);
    if dropped > 0 {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"sender": msg.sender, "dropped_turns": dropped, "remaining_turns": prior_turns.len()})), "Proactively trimmed conversation history to fit context budget");
    }

    // ── Dual-scope memory recall ──────────────────────────────────
    // Always recall before each LLM call (not just first turn).
    // For group chats: merge sender-scope + group-scope memories.
    // For DMs: recall from the current conversation scope plus sender scope.
    let is_group_chat = is_group_reply_target(&msg.reply_target);

    let mem_recall_start = Instant::now();
    let sender_session_ids = sender_memory_session_ids(&msg, &history_key);
    let sender_session_id_refs: Vec<Option<&str>> = sender_session_ids
        .iter()
        .map(|s| Some(s.as_str()))
        .collect();
    let sender_memory_fut = build_memory_context_for_sessions(
        ctx.memory.as_ref(),
        &msg.content,
        ctx.min_relevance_score,
        sender_session_id_refs.as_slice(),
    );

    let (sender_memory, group_memory) = if is_group_chat {
        let group_memory_fut = build_memory_context(
            ctx.memory.as_ref(),
            &msg.content,
            ctx.min_relevance_score,
            Some(&history_key),
        );
        tokio::join!(sender_memory_fut, group_memory_fut)
    } else {
        (sender_memory_fut.await, String::new())
    };
    #[allow(clippy::cast_possible_truncation)]
    let mem_recall_ms = mem_recall_start.elapsed().as_millis() as u64;
    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"mem_recall_ms": mem_recall_ms, "sender_empty": sender_memory.is_empty(), "group_empty": group_memory.is_empty()})), "memory recall completed");

    // Merge sender and group memory context blocks.
    let memory_context = if group_memory.is_empty() {
        sender_memory
    } else if sender_memory.is_empty() {
        group_memory
    } else {
        format!("{sender_memory}\n{group_memory}")
    };

    // Use refreshed system prompt for new sessions (master's /new support),
    // and inject memory into system prompt (not user message) so it
    // doesn't pollute session history and is re-fetched each turn.
    let base_system_prompt = if had_prior_history {
        ctx.system_prompt.as_str().to_string()
    } else {
        refreshed_new_session_system_prompt(ctx.as_ref())
    };
    let mut system_prompt =
        build_channel_system_prompt_for_message(&base_system_prompt, &msg, target_channel.as_ref());
    if !memory_context.is_empty() {
        let _ = write!(system_prompt, "\n\n{memory_context}");
    }
    let mut history = vec![ChatMessage::system(system_prompt)];
    history.extend(prior_turns);

    // ── Proactive context compression ────────────────────────────
    // Use the existing ContextCompressor to summarize older history
    // before the LLM call, preventing context-window-exceeded errors
    // and preserving key decisions through LLM-driven summarization.
    {
        let cc_config = ctx.agent_cfg.resolved.context_compression.clone();
        let compressor = zeroclaw_runtime::agent::context_compressor::ContextCompressor::new(
            cc_config,
            ctx.context_token_budget,
        )
        .with_memory(Arc::clone(&ctx.memory));
        match compressor
            .compress_if_needed(
                &mut history,
                active_model_provider.as_ref(),
                route.model.as_str(),
                runtime_defaults.defaults.temperature,
            )
            .await
        {
            Ok(result) if result.compressed => {
                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"sender": msg.sender, "tokens_before": result.tokens_before, "tokens_after": result.tokens_after, "passes": result.passes_used})), "Proactive context compression applied before LLM call");
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Context compression failed, proceeding without"
                );
            }
            _ => {}
        }
    }

    // ── Reply-intent precheck ────────────────────────────────────────
    let explicit_channel_address =
        is_explicitly_addressed_channel_message(&msg.channel, &msg.content);
    let classifier_intent = if explicit_channel_address {
        AssistantChannelOutcome::Reply(String::new())
    } else {
        let (classifier_provider_arc, classifier_model_owned, classifier_temperature): (
            Arc<dyn ModelProvider>,
            String,
            Option<f64>,
        ) = resolve_classifier_route(
            ctx.as_ref(),
            &ctx.agent_cfg.classifier_provider,
            &runtime_defaults,
        )
        .await
        .unwrap_or_else(|| {
            (
                Arc::clone(&active_model_provider),
                route.model.clone(),
                None,
            )
        });

        classify_channel_reply_intent(
            classifier_provider_arc.as_ref(),
            history[0].content.as_str(),
            &history,
            classifier_model_owned.as_str(),
            classifier_temperature.or(runtime_defaults.defaults.temperature),
        )
        .await
        .unwrap_or(AssistantChannelOutcome::Reply(String::new()))
    };

    // ACP sessions are direct user requests — there is no broadcast,
    // no peer context, no spam concern. The no-reply classifier is a
    // multi-agent / chatroom heuristic; on ACP, every inbound is a
    // call to action and must produce a reply. Override the verdict
    // before the no-reply gate so the agent loop generates a response.
    let is_acp_channel = target_channel
        .as_ref()
        .map(|c| {
            matches!(
                ::zeroclaw_api::attribution::Attributable::role(c.as_ref()),
                ::zeroclaw_api::attribution::Role::Channel(
                    ::zeroclaw_api::attribution::ChannelKind::AcpChannel
                )
            )
        })
        .unwrap_or(false);
    let reply_intent = if is_acp_channel
        && let AssistantChannelOutcome::NoReply {
            ref kind,
            ref reason,
        } = classifier_intent
    {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "kind": format!("{kind:?}"),
                    "reason": reason.as_deref().unwrap_or(""),
                })
            ),
            "ACP channel: classifier voted no_reply, overriding to reply (ACP must always respond)"
        );
        AssistantChannelOutcome::Reply(String::new())
    } else {
        classifier_intent
    };

    if let AssistantChannelOutcome::NoReply { kind, reason } = reply_intent {
        let history_response = AssistantChannelOutcome::NoReply {
            kind,
            reason: reason.clone(),
        }
        .history_marker();
        append_sender_turn(
            ctx.as_ref(),
            &history_key,
            ChatMessage::assistant(&history_response),
        );
        // Surface the no-reply decision in chat with an emoji on the user's
        // message so the chatter isn't left wondering whether the bot saw
        // the message. Same `ack_reactions` gate as the 👀 → ✅/⚠️ ack/done
        // pattern so operators with reactions disabled don't suddenly see
        // them. Best-effort: log on failure, never propagate. Channels that
        // don't implement add_reaction get the trait's no-op default.
        if ctx.ack_reactions
            && let Some(channel) = target_channel.as_ref()
        {
            let emoji = kind.emoji();
            if let Err(e) = channel
                .add_reaction(&msg.reply_target, &msg.id, emoji)
                .await
            {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Failed to add {emoji} no-reply reaction on {}: {e}",
                        channel.name()
                    )
                );
            }
        }
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                .with_duration(u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),)
                .with_attrs(::serde_json::json!({
                    "model_provider": route.model_provider,
                    "model": route.model,
                    "sender": msg.sender,
                    "phase": "precheck",
                    "kind": format!("{kind:?}"),
                    "reason": reason.as_deref().unwrap_or("no reason provided"),
                })),
            "channel_message_no_reply"
        );
        return;
    }

    let use_draft_streaming = target_channel
        .as_ref()
        .is_some_and(|ch| ch.supports_draft_updates());

    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"has_target_channel": target_channel.is_some(), "use_draft_streaming": use_draft_streaming})), "Streaming decision");

    // Partial mode: delta channel for draft updates (progress + text).
    let (delta_tx, delta_rx) = if use_draft_streaming {
        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_runtime::agent::loop_::DraftEvent>(64);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Partial mode: send an initial draft message for progressive editing.
    let draft_message_id = if use_draft_streaming {
        if let Some(channel) = target_channel.as_ref() {
            match channel
                .send_draft(
                    &SendMessage::new("...", &msg.reply_target).in_thread(msg.thread_ts.clone()),
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        &format!("Failed to send draft on {}", channel.name())
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Spawn the appropriate handler for the delta channel.
    let draft_updater = if use_draft_streaming {
        // Partial: accumulate text and edit a single draft message.
        if let (Some(mut rx), Some(draft_id_ref), Some(channel_ref)) = (
            delta_rx,
            draft_message_id.as_deref(),
            target_channel.as_ref(),
        ) {
            let channel = Arc::clone(channel_ref);
            let reply_target = msg.reply_target.clone();
            let draft_id = draft_id_ref.to_string();
            Some(zeroclaw_spawn::spawn!(async move {
                use zeroclaw_runtime::agent::loop_::StreamDelta;
                let mut accumulated = String::new();
                while let Some(event) = rx.recv().await {
                    match event {
                        StreamDelta::Status(text) => {
                            let visible = strip_think_tags_inline(&text);
                            if let Err(e) = channel
                                .update_draft_progress(&reply_target, &draft_id, &visible)
                                .await
                            {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "Draft progress update failed"
                                );
                            }
                        }
                        StreamDelta::Text(text) => {
                            accumulated.push_str(&text);
                            let visible = strip_think_tags_inline(&accumulated);
                            if let Err(e) = channel
                                .update_draft(&reply_target, &draft_id, &visible)
                                .await
                            {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "Draft update failed"
                                );
                            }
                        }
                    }
                }
            }))
        } else {
            None
        }
    } else {
        None
    };

    // React with 👀 to acknowledge the incoming message
    if ctx.ack_reactions
        && let Some(channel) = target_channel.as_ref()
        && let Err(e) = channel
            .add_reaction(&msg.reply_target, &msg.id, "\u{1F440}")
            .await
    {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "Failed to add reaction"
        );
    }

    // Skip typing only for Partial mode — the draft message itself provides
    // visual feedback. MultiMessage and Off both keep typing active.
    let is_partial_draft = target_channel
        .as_ref()
        .is_some_and(|ch| ch.supports_draft_updates() && !ch.supports_multi_message_streaming());
    let typing_cancellation = if is_partial_draft {
        None
    } else {
        target_channel.as_ref().map(|_| CancellationToken::new())
    };
    let typing_task = match (target_channel.as_ref(), typing_cancellation.as_ref()) {
        (Some(channel), Some(token)) => Some(spawn_scoped_typing_task(
            Arc::clone(channel),
            msg.reply_target.clone(),
            token.clone(),
        )),
        _ => None,
    };

    // Wrap observer to forward tool events as live thread messages
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let notify_observer: Arc<ChannelNotifyObserver> = Arc::new(ChannelNotifyObserver {
        inner: Arc::clone(&ctx.observer),
        tx: notify_tx,
        tools_used: AtomicBool::new(false),
    });
    let notify_observer_flag = Arc::clone(&notify_observer);
    let notify_channel = target_channel.clone();
    let notify_reply_target = msg.reply_target.clone();
    let notify_thread_root = followup_thread_id(&msg);
    let notify_task = if msg.channel == "cli" || !ctx.show_tool_calls {
        Some(zeroclaw_spawn::spawn!(async move {
            while notify_rx.recv().await.is_some() {}
        }))
    } else {
        Some(zeroclaw_spawn::spawn!(async move {
            let thread_ts = notify_thread_root;
            while let Some(text) = notify_rx.recv().await {
                if let Some(ref ch) = notify_channel {
                    let _ = ch
                        .send(
                            &SendMessage::new(&text, &notify_reply_target)
                                .in_thread(thread_ts.clone()),
                        )
                        .await;
                }
            }
        }))
    };

    enum LlmExecutionResult {
        Completed(Result<Result<String, anyhow::Error>, tokio::time::error::Elapsed>),
        Cancelled,
    }

    let model_switch_callback = get_model_switch_state();
    let scale_cap = ctx
        .pacing
        .message_timeout_scale_max
        .unwrap_or(CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP);
    let timeout_budget_secs = channel_message_timeout_budget_secs_with_cap(
        ctx.message_timeout_secs,
        ctx.max_tool_iterations,
        scale_cap,
    );
    let cost_tracking_context = ctx.cost_tracking.clone().map(|state| {
        zeroclaw_runtime::agent::loop_::ToolLoopCostTrackingContext::new(
            state.tracker,
            state.model_provider_pricing,
        )
        .with_agent_alias(state.agent_alias.as_str())
    });
    let llm_call_start = Instant::now();
    #[allow(clippy::cast_possible_truncation)]
    let elapsed_before_llm_ms = started_at.elapsed().as_millis() as u64;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"elapsed_before_llm_ms": elapsed_before_llm_ms})),
        "starting LLM call"
    );
    // Per-turn collector. `tool_execution::execute_one_tool` pushes
    // `<tool_name>: <receipt>` here whenever a receipt is generated, so the
    // orchestrator can render the trailing `Tool receipts:` block after the
    // loop returns. Wrapped in `Arc` so the same handle can be shared into
    // `TOOL_LOOP_RECEIPT_CONTEXT` for subagent forwarding. Inert when
    // `receipt_generator` is `None`.
    let tool_receipts_collector: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let receipt_scope = ctx.receipt_generator.as_ref().map(|generator| {
        zeroclaw_runtime::agent::tool_receipts::ReceiptScope {
            generator: generator.clone(),
            collector: std::sync::Arc::clone(&tool_receipts_collector),
        }
    });
    let (llm_result, fallback_info) = scope_provider_fallback(async {
        let llm_result = loop {
            let loop_result = tokio::select! {
                () = cancellation_token.cancelled() => LlmExecutionResult::Cancelled,
                result = tokio::time::timeout(
                    Duration::from_secs(timeout_budget_secs),
                    scope_thread_id(
                        msg.interruption_scope_id.clone()
                            .or_else(|| msg.thread_ts.clone())
                            .or_else(|| Some(msg.id.clone())),
                    scope_session_key(
                        Some(history_key.clone()),
                        zeroclaw_runtime::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                            cost_tracking_context.clone(),
                        zeroclaw_runtime::agent::tool_receipts::TOOL_LOOP_RECEIPT_CONTEXT.scope(
                            receipt_scope.clone(),
                        run_tool_call_loop(
                        active_model_provider.as_ref(),
                        &mut history,
                        ctx.tools_registry.as_ref(),
                        notify_observer.as_ref() as &dyn Observer,
                        route.model_provider.as_str(),
                        route.model.as_str(),
                        runtime_defaults.defaults.temperature,
                        true,
                        Some(&*ctx.approval_manager),
                        msg.channel.as_str(),
                        Some(msg.reply_target.as_str()),
                        &ctx.multimodal,
                        ctx.max_tool_iterations,
                        Some(cancellation_token.clone()),
                        delta_tx.clone(),
                        ctx.hooks.as_deref(),
                        if msg.channel == "cli"
                            || ctx.autonomy_level == AutonomyLevel::Full
                        {
                            &[]
                        } else {
                            ctx.non_cli_excluded_tools.as_ref()
                        },
                        ctx.tool_call_dedup_exempt.as_ref(),
                        ctx.activated_tools.as_ref(),
                        Some(model_switch_callback.clone()),
                        &ctx.pacing,
                        ctx.prompt_config
                            .agent(ctx.agent_alias.as_str())
                            .is_some_and(|agent| agent.resolved.strict_tool_parsing),
                        ctx.prompt_config
                            .agent(ctx.agent_alias.as_str())
                            .is_some_and(|agent| agent.resolved.parallel_tools),
                        ctx.max_tool_result_chars,
                        ctx.context_token_budget,
                        None, // shared_budget
                        target_channel.as_deref(),
                        ctx.receipt_generator.as_ref(),
                        // Collector is meaningful only when the generator is
                        // active. Pass None when receipts are disabled so the
                        // call site reflects that coupling explicitly.
                        ctx.receipt_generator
                            .as_ref()
                            .map(|_| tool_receipts_collector.as_ref()),
                    ),
                    ),
                    ),
                    ),
                    ),
                ) => LlmExecutionResult::Completed(result),
            };

            // Handle model switch: re-create the model_provider and retry
            if let LlmExecutionResult::Completed(Ok(Err(ref e))) = loop_result
                && let Some((new_model_provider, new_model)) = is_model_switch_requested(e)
            {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Model switch requested, switching from {} {} to {} {}",
                        route.model_provider, route.model, new_model_provider, new_model
                    )
                );

                let resolved_model_provider = match resolve_provider_ref_for_runtime_switch(
                    runtime_defaults.config.as_ref(),
                    &new_model_provider,
                ) {
                    Ok(provider_ref) => provider_ref,
                    Err(err) => {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"err": err.to_string()})),
                            "Failed to resolve model_provider after model switch"
                        );
                        clear_model_switch_request();
                        break loop_result;
                    }
                };

                match get_or_create_provider(
                    ctx.as_ref(),
                    &resolved_model_provider,
                    None,
                    &runtime_defaults,
                )
                .await
                {
                    Ok(new_prov) => {
                        active_model_provider = new_prov;
                        route.model_provider = resolved_model_provider;
                        route.model = new_model;
                        clear_model_switch_request();

                        ctx.observer.record_event(&ObserverEvent::AgentStart {
                            model_provider: route.model_provider.clone(),
                            model: route.model.clone(),
                            channel: None,
                            agent_alias: None,
                            turn_id: None,
                        });

                        continue;
                    }
                    Err(err) => {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"err": err.to_string()})),
                            "Failed to create model_provider after model switch"
                        );
                        clear_model_switch_request();
                        // Fall through with the original error
                    }
                }
            }

            break loop_result;
        };
        let fb = take_last_provider_fallback();
        (llm_result, fb)
    })
    .await;

    // Drop all senders so updater tasks can exit (rx.recv() returns None).
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Post-loop: dropping delta_tx and awaiting draft updater"
    );
    drop(delta_tx);
    if let Some(handle) = draft_updater {
        let _ = handle.await;
    }
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Post-loop: draft updater completed"
    );

    // Thread the final reply only if tools were used (multi-message response)
    if notify_observer_flag.tools_used.load(Ordering::Relaxed) && msg.channel != "cli" {
        msg.thread_ts = followup_thread_id(&msg);
    }
    // Drop the notify sender so the forwarder task finishes
    drop(notify_observer);
    drop(notify_observer_flag);
    if let Some(handle) = notify_task {
        let _ = handle.await;
    }

    #[allow(clippy::cast_possible_truncation)]
    let llm_call_ms = llm_call_start.elapsed().as_millis() as u64;
    #[allow(clippy::cast_possible_truncation)]
    let total_ms = started_at.elapsed().as_millis() as u64;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"llm_call_ms": llm_call_ms, "total_ms": total_ms})),
        "LLM call completed"
    );

    if let Some(token) = typing_cancellation.as_ref() {
        token.cancel();
    }
    if let Some(handle) = typing_task {
        log_worker_join_result(handle.await);
    }

    let reaction_done_emoji = match &llm_result {
        LlmExecutionResult::Completed(Ok(Ok(_))) => "\u{2705}", // ✅
        _ => "\u{26A0}\u{FE0F}",                                // ⚠️
    };

    match llm_result {
        LlmExecutionResult::Cancelled => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "Cancelled in-flight channel request due to newer message"
            );
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_duration(
                        u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                    .with_attrs(::serde_json::json!({
                        "model_provider": route.model_provider,
                        "model": route.model,
                        "sender": msg.sender,
                        "reason": "cancelled due to newer inbound message",
                    })),
                "channel_message_cancelled"
            );
            if let (Some(channel), Some(draft_id)) =
                (target_channel.as_ref(), draft_message_id.as_deref())
                && let Err(err) = channel.cancel_draft(&msg.reply_target, draft_id).await
            {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    &format!("Failed to cancel draft on {}", channel.name())
                );
            }
        }
        LlmExecutionResult::Completed(Ok(Ok(response))) => {
            // ── Hook: on_message_sending (modifying) ─────────
            let mut outbound_response = response;
            if let Some(hooks) = &ctx.hooks {
                match hooks
                    .run_on_message_sending(
                        msg.channel.clone(),
                        msg.reply_target.clone(),
                        outbound_response.clone(),
                    )
                    .await
                {
                    zeroclaw_runtime::hooks::HookResult::Cancel(reason) => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"reason": reason.to_string()})),
                            "outgoing message suppressed by hook"
                        );
                        if let (Some(channel), Some(draft_id)) =
                            (target_channel.as_ref(), draft_message_id.as_deref())
                        {
                            let _ = channel.cancel_draft(&msg.reply_target, draft_id).await;
                        }
                        return;
                    }
                    zeroclaw_runtime::hooks::HookResult::Continue((
                        hook_channel,
                        hook_recipient,
                        mut modified_content,
                    )) => {
                        if hook_channel != msg.channel || hook_recipient != msg.reply_target {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"from_channel": channel_composite, "from_recipient": msg.reply_target, "to_channel": hook_channel, "to_recipient": hook_recipient})), "on_message_sending attempted to rewrite channel routing; only content mutation is applied");
                        }

                        let modified_len = modified_content.chars().count();
                        if modified_len > CHANNEL_HOOK_MAX_OUTBOUND_CHARS {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"limit": CHANNEL_HOOK_MAX_OUTBOUND_CHARS, "attempted": modified_len})), "hook-modified outbound content exceeded limit; truncating");
                            modified_content = truncate_with_ellipsis(
                                &modified_content,
                                CHANNEL_HOOK_MAX_OUTBOUND_CHARS,
                            );
                        }

                        if modified_content != outbound_response {
                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"sender": msg.sender, "before_len": outbound_response.chars().count(), "after_len": modified_content.chars().count()})), "outgoing message content modified by hook");
                        }

                        outbound_response = modified_content;
                    }
                }
            }

            let sanitized_response =
                sanitize_channel_response(&outbound_response, ctx.tools_registry.as_ref());
            let mut delivered_response = if sanitized_response.is_empty()
                && !outbound_response.trim().is_empty()
            {
                "I encountered malformed tool-call output and could not produce a safe reply. Please try again.".to_string()
            } else {
                sanitized_response
            };
            delivered_response = ensure_nonempty_channel_reply(
                delivered_response,
                &outbound_response,
                &msg.channel,
                &msg.reply_target,
            );

            // Append a footer when the response was served by a different model_provider family.
            // Intra-family fallbacks (e.g. minimax → minimax-cn) are suppressed.
            if let Some(fb) = fallback_info.as_ref() {
                let req_base = fb.requested_provider.split(':').next().unwrap_or("");
                let act_base = fb.actual_provider.split(':').next().unwrap_or("");
                let same_family = req_base == act_base
                    || req_base.starts_with(act_base)
                    || act_base.starts_with(req_base);
                if !same_family {
                    use std::fmt::Write as _;
                    write!(
                        delivered_response,
                        "\n\n---\n\u{26A1} `{}` unavailable \u{2014} response from **{}** (`{}`)\nSwitch model: /models",
                        fb.requested_provider, fb.actual_provider, fb.actual_model,
                    )
                    .ok();
                }
            }

            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Outbound)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_duration(
                        u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                    .with_attrs(::serde_json::json!({
                        "model_provider": route.model_provider,
                        "model": route.model,
                        "sender": msg.sender,
                        "response": scrub_credentials(&delivered_response),
                    })),
                "channel_message_outbound"
            );

            // Persist intermediate tool-call/result messages from this turn
            // so the model retains concrete "I used tools" examples in
            // context, preventing drift toward tool-less responses.
            let keep_tool_turns = ctx.agent_cfg.resolved.keep_tool_context_turns;
            if keep_tool_turns > 0 {
                // Find tool messages for the current turn: everything after
                // the last user message up to (but not including) the final
                // assistant response that matches our delivered text.
                let tool_messages: Vec<ChatMessage> = extract_current_turn_tool_messages(&history);
                for tool_msg in tool_messages {
                    append_sender_turn(ctx.as_ref(), &history_key, tool_msg);
                }
            }

            let history_response = delivered_response.clone();
            append_sender_turn(
                ctx.as_ref(),
                &history_key,
                ChatMessage::assistant(&history_response),
            );

            // Strip tool-call messages from turns older than
            // keep_tool_context_turns to prevent unbounded growth.
            if keep_tool_turns > 0 {
                strip_old_tool_context(ctx.as_ref(), &history_key, keep_tool_turns);
            }

            // Fire-and-forget LLM-driven memory consolidation. Passes the
            // agent's resolved temperature through unchanged — `None`
            // means the provider sends no `temperature` field (necessary
            // for models that reject it, e.g. claude-opus-4-7).
            if ctx.auto_save_memory && msg.content.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
                let memory_strategy = Arc::clone(&ctx.memory_strategy);
                let model_provider = Arc::clone(&ctx.model_provider);
                let model = ctx.model.to_string();
                let temperature = ctx.temperature;
                let user_msg = msg.content.clone();
                let assistant_resp = delivered_response.clone();
                zeroclaw_spawn::spawn!(async move {
                    if let Err(e) = memory_strategy
                        .consolidate_turn(
                            &user_msg,
                            &assistant_resp,
                            model_provider.as_ref(),
                            &model,
                            temperature,
                        )
                        .await
                    {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Memory consolidation skipped"
                        );
                    }
                });
            }

            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Outbound)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_duration(
                        u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                    .with_attrs(::serde_json::json!({
                        "sender": msg.sender,
                        "message_id": msg.id,
                        "reply_target": msg.reply_target,
                        "thread_ts": msg.thread_ts,
                        "content": delivered_response,
                    })),
                "reply delivered"
            );
            // Build the trailing `Tool receipts:` block from the per-turn
            // collector. Empty when receipts are disabled or no tool ran.
            // Includes receipts from delegate sub-agents because the same
            // `Arc<Mutex<Vec<String>>>` is forwarded via
            // `TOOL_LOOP_RECEIPT_CONTEXT` into sub-loops.
            let receipts_block = if ctx.show_receipts_in_response {
                let receipts = tool_receipts_collector
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if receipts.is_empty() {
                    None
                } else {
                    use std::fmt::Write as _;
                    let mut block = String::from("---\nTool receipts:");
                    for r in receipts.iter() {
                        write!(block, "\n  {r}").ok();
                    }
                    Some(block)
                }
            } else {
                None
            };

            if let Some(channel) = target_channel.as_ref() {
                if let Some(ref draft_id) = draft_message_id {
                    if let Err(e) = channel
                        .finalize_draft(&msg.reply_target, draft_id, &delivered_response)
                        .await
                    {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Failed to finalize draft; sending as new message"
                        );
                        let _ = channel
                            .send(&SendMessage::reply_to(&msg, &delivered_response))
                            .await;
                    }
                } else if let Err(e) = channel
                    .send(
                        &SendMessage::reply_to(&msg, &delivered_response)
                            .with_cancellation(cancellation_token.clone()),
                    )
                    .await
                {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "failed to reply"
                    );
                }
                // Send tool receipts as a separate message in the same thread.
                // The block is the operator-facing audit surface for the feature,
                // so a dropped send must leave a log signal rather than silently
                // disappear.
                if let Some(ref block) = receipts_block
                    && let Err(e) = channel
                        .send(
                            &SendMessage::new(block, &msg.reply_target)
                                .in_thread(msg.thread_ts.clone()),
                        )
                        .await
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "failed to send tool receipts block"
                    );
                }
            }
        }
        LlmExecutionResult::Completed(Ok(Err(e))) => {
            if zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(&e)
                || cancellation_token.is_cancelled()
            {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"sender": msg.sender})),
                    "Cancelled in-flight channel request due to newer message"
                );
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_duration(
                            u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                        )
                        .with_attrs(::serde_json::json!({
                            "model_provider": route.model_provider,
                            "model": route.model,
                            "sender": msg.sender,
                            "reason": "cancelled during tool-call loop",
                        })),
                    "channel_message_cancelled"
                );
                if let (Some(channel), Some(draft_id)) =
                    (target_channel.as_ref(), draft_message_id.as_deref())
                    && let Err(err) = channel.cancel_draft(&msg.reply_target, draft_id).await
                {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                        &format!("Failed to cancel draft on {}", channel.name())
                    );
                }
            } else if is_context_window_overflow_error(&e) {
                let compacted = compact_sender_history(ctx.as_ref(), &history_key);
                let error_text = if compacted {
                    "⚠️ Context window exceeded for this conversation. I compacted recent history and kept the latest context. Please resend your last message."
                } else {
                    "⚠️ Context window exceeded for this conversation. Please resend your last message."
                };
                eprintln!(
                    "  ⚠️ Context window exceeded after {}ms; sender history compacted={}",
                    started_at.elapsed().as_millis(),
                    compacted
                );
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_duration(
                            u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                        )
                        .with_attrs(::serde_json::json!({
                            "model_provider": route.model_provider,
                            "model": route.model,
                            "sender": msg.sender,
                            "reason": "context window exceeded",
                            "history_compacted": compacted,
                        })),
                    "channel_message_error"
                );
                if let Some(channel) = target_channel.as_ref() {
                    if let Some(ref draft_id) = draft_message_id {
                        let _ = channel
                            .finalize_draft(&msg.reply_target, draft_id, error_text)
                            .await;
                    } else {
                        let _ = channel
                            .send(
                                &SendMessage::new(error_text, &msg.reply_target)
                                    .in_thread(msg.thread_ts.clone()),
                            )
                            .await;
                    }
                }
            } else {
                eprintln!(
                    "  ❌ LLM error after {}ms: {e}",
                    started_at.elapsed().as_millis()
                );

                // Evict cached model_provider on auth errors so the next request
                // re-creates it with fresh OAuth credentials.
                if zeroclaw_providers::reliable::is_auth_error(&e) {
                    let cache_key = provider_cache_key(
                        &route.model_provider,
                        route.api_key.as_deref(),
                        runtime_defaults.generation,
                    );
                    let mut cache = ctx.provider_cache.lock().unwrap_or_else(|p| p.into_inner());
                    if cache.remove(&cache_key).is_some() {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(
                                ::serde_json::json!({"model_provider": route.model_provider})
                            ),
                            "Evicted cached model_provider after auth error; next request will re-create with fresh credentials"
                        );
                    }
                }
                let safe_error = zeroclaw_providers::sanitize_api_error(&e.to_string());
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_duration(
                            u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                        )
                        .with_attrs(::serde_json::json!({
                            "model_provider": route.model_provider,
                            "model": route.model,
                            "sender": msg.sender,
                            "error": safe_error,
                        })),
                    "channel_message_error"
                );
                let should_rollback_user_turn = should_rollback_failed_user_turn(&e);
                let rolled_back = should_rollback_user_turn
                    && rollback_orphan_user_turn(
                        ctx.as_ref(),
                        &history_key,
                        &timestamped_history_content,
                    );

                if !rolled_back {
                    // Close the orphan user turn so subsequent messages don't
                    // inherit this failed request as unfinished context.
                    append_sender_turn(
                        ctx.as_ref(),
                        &history_key,
                        ChatMessage::assistant("[Task failed — not continuing this request]"),
                    );
                }
                if let Some(channel) = target_channel.as_ref() {
                    if let Some(ref draft_id) = draft_message_id {
                        let _ = channel
                            .finalize_draft(&msg.reply_target, draft_id, &format!("⚠️ Error: {e}"))
                            .await;
                    } else {
                        let _ = channel
                            .send(
                                &SendMessage::new(format!("⚠️ Error: {e}"), &msg.reply_target)
                                    .in_thread(msg.thread_ts.clone()),
                            )
                            .await;
                    }
                }
            }
        }
        LlmExecutionResult::Completed(Err(_)) => {
            let timeout_msg = format!(
                "LLM response timed out after {}s (base={}s, max_tool_iterations={})",
                timeout_budget_secs, ctx.message_timeout_secs, ctx.max_tool_iterations
            );
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_duration(
                        u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                    .with_attrs(::serde_json::json!({
                        "model_provider": route.model_provider,
                        "model": route.model,
                        "sender": msg.sender,
                        "reason": timeout_msg,
                    })),
                "channel_message_timeout"
            );
            eprintln!(
                "  ❌ {} (elapsed: {}ms)",
                timeout_msg,
                started_at.elapsed().as_millis()
            );
            // Close the orphan user turn so subsequent messages don't
            // inherit this timed-out request as unfinished context.
            append_sender_turn(
                ctx.as_ref(),
                &history_key,
                ChatMessage::assistant("[Task timed out — not continuing this request]"),
            );
            if let Some(channel) = target_channel.as_ref() {
                let error_text =
                    "⚠️ Request timed out while waiting for the model. Please try again.";
                if let Some(ref draft_id) = draft_message_id {
                    let _ = channel
                        .finalize_draft(&msg.reply_target, draft_id, error_text)
                        .await;
                } else {
                    let _ = channel
                        .send(
                            &SendMessage::new(error_text, &msg.reply_target)
                                .in_thread(msg.thread_ts.clone()),
                        )
                        .await;
                }
            }
        }
    }

    // Swap 👀 → ✅ (or ⚠️ on error) to signal processing is complete
    if ctx.ack_reactions
        && let Some(channel) = target_channel.as_ref()
    {
        let _ = channel
            .remove_reaction(&msg.reply_target, &msg.id, "\u{1F440}")
            .await;
        let _ = channel
            .add_reaction(&msg.reply_target, &msg.id, reaction_done_emoji)
            .await;
    }
}

/// Shared worker body extracted so both the normal path and the debounce path
/// can reuse the same in-flight tracking / cancellation / process logic.
async fn dispatch_worker(
    ctx: Arc<ChannelRuntimeContext>,
    msg: zeroclaw_api::channel::ChannelMessage,
    in_flight: Arc<tokio::sync::Mutex<HashMap<String, InFlightSenderTaskState>>>,
    task_sequence: Arc<AtomicU64>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let _permit = permit;
    let interrupt_enabled = ctx
        .interrupt_on_new_message
        .enabled_for_channel(msg.channel.as_str());
    let sender_scope_key = interruption_scope_key(&msg);
    let cancellation_token = CancellationToken::new();
    let completion = Arc::new(InFlightTaskCompletion::new());
    let task_id = task_sequence.fetch_add(1, Ordering::Relaxed);

    let register_in_flight = msg.channel != "cli";

    if register_in_flight {
        let previous = {
            let mut active = in_flight.lock().await;
            active.insert(
                sender_scope_key.clone(),
                InFlightSenderTaskState {
                    task_id,
                    cancellation: cancellation_token.clone(),
                    completion: Arc::clone(&completion),
                },
            )
        };

        if interrupt_enabled && let Some(previous) = previous {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "interrupting previous in-flight request for sender"
            );
            previous.cancellation.cancel();
            previous.completion.wait().await;
        }
    }

    process_channel_message(ctx, msg, cancellation_token).await;

    if register_in_flight {
        let mut active = in_flight.lock().await;
        if active
            .get(&sender_scope_key)
            .is_some_and(|state| state.task_id == task_id)
        {
            active.remove(&sender_scope_key);
        }
    }

    completion.mark_done();
}

/// Maps each inbound `ChannelMessage` to the owning agent's `ChannelRuntimeContext`.
///
/// Lookup mirrors `find_channel_for_message`: composite `<type>.<alias>` first,
/// bare `<type>` second. Returns `None` when no agent owns the channel — the
/// dispatch loop drops the message rather than picking a default.
#[derive(Clone)]
struct AgentRouter {
    by_agent: Arc<HashMap<String, Arc<ChannelRuntimeContext>>>,
    owner_by_channel_key: Arc<HashMap<String, String>>,
    single_ctx: Option<Arc<ChannelRuntimeContext>>,
}

impl AgentRouter {
    #[cfg(test)]
    fn single(ctx: Arc<ChannelRuntimeContext>) -> Self {
        Self {
            by_agent: Arc::new(HashMap::new()),
            owner_by_channel_key: Arc::new(HashMap::new()),
            single_ctx: Some(ctx),
        }
    }

    fn multi(
        by_agent: HashMap<String, Arc<ChannelRuntimeContext>>,
        owner_by_channel_key: HashMap<String, String>,
    ) -> Self {
        Self {
            by_agent: Arc::new(by_agent),
            owner_by_channel_key: Arc::new(owner_by_channel_key),
            single_ctx: None,
        }
    }

    fn resolve(
        &self,
        msg: &zeroclaw_api::channel::ChannelMessage,
    ) -> Option<Arc<ChannelRuntimeContext>> {
        if let Some(ctx) = &self.single_ctx {
            return Some(Arc::clone(ctx));
        }
        if let Some(alias) = msg.channel_alias.as_deref().filter(|s| !s.is_empty()) {
            let composite = format!("{}.{alias}", msg.channel);
            if let Some(agent) = self.owner_by_channel_key.get(&composite)
                && let Some(ctx) = self.by_agent.get(agent)
            {
                return Some(Arc::clone(ctx));
            }
        }
        if let Some(agent) = self.owner_by_channel_key.get(&msg.channel)
            && let Some(ctx) = self.by_agent.get(agent)
        {
            return Some(Arc::clone(ctx));
        }
        None
    }
}

async fn run_message_dispatch_loop(
    mut rx: tokio::sync::mpsc::Receiver<zeroclaw_api::channel::ChannelMessage>,
    router: AgentRouter,
    max_in_flight_messages: usize,
) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_in_flight_messages));
    let mut workers = tokio::task::JoinSet::new();
    let in_flight_by_sender = Arc::new(tokio::sync::Mutex::new(HashMap::<
        String,
        InFlightSenderTaskState,
    >::new()));
    let task_sequence = Arc::new(AtomicU64::new(1));

    while let Some(msg) = rx.recv().await {
        let Some(ctx) = router.resolve(&msg) else {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"channel_alias": msg.channel_alias, "sender": msg.sender})), "dropping inbound message: no agent owns this channel");
            continue;
        };
        // Fast path: /stop cancels the in-flight task for this sender scope without
        // spawning a worker or registering a new task. Handled here — before semaphore
        // acquisition — so the target task is still in the store and is never replaced.
        if msg.channel != "cli" && is_stop_command(&msg.content) {
            let scope_key = interruption_scope_key(&msg);
            let previous = {
                let mut active = in_flight_by_sender.lock().await;
                active.remove(&scope_key)
            };
            let reply = if let Some(state) = previous {
                state.cancellation.cancel();
                "Stop signal sent.".to_string()
            } else {
                "No in-flight task for this sender scope.".to_string()
            };
            let channel = find_channel_for_message(&ctx.channels_by_name, &msg).cloned();
            if let Some(channel) = channel {
                let reply_target = msg.reply_target.clone();
                let thread_ts = msg.thread_ts.clone();
                zeroclaw_spawn::spawn!(async move {
                    let _ = channel
                        .send(&SendMessage::new(reply, &reply_target).in_thread(thread_ts))
                        .await;
                });
            } else {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "stop command: no registered channel found for reply"
                );
            }
            continue;
        }

        // ── Debounce: accumulate rapid messages per sender ──────────
        // CLI messages bypass debouncing so the interactive loop stays responsive.
        let msg = if msg.channel != "cli" && ctx.debouncer.enabled() {
            let debounce_key = conversation_history_key(&msg);
            match ctx.debouncer.debounce(&debounce_key, &msg.content).await {
                zeroclaw_infra::debounce::DebounceResult::Pending(rx) => {
                    // Spawn a lightweight task that waits for the debounce window
                    // to expire, then feeds the combined message through the normal
                    // worker path below.
                    let debounce_ctx = Arc::clone(&ctx);
                    let debounce_in_flight = Arc::clone(&in_flight_by_sender);
                    let debounce_semaphore = Arc::clone(&semaphore);
                    let debounce_task_seq = Arc::clone(&task_sequence);
                    let mut debounce_msg = msg;
                    workers.spawn(async move {
                        let combined = match rx.await {
                            Ok(combined) => combined,
                            Err(_) => {
                                // Receiver dropped — a newer message superseded this one.
                                return;
                            }
                        };
                        debounce_msg.content = combined;
                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"channel": debounce_msg.channel, "sender": debounce_msg.sender})), "Debounced message ready — dispatching combined message");

                        let permit = match debounce_semaphore.acquire_owned().await {
                            Ok(permit) => permit,
                            Err(_) => return,
                        };

                        dispatch_worker(
                            debounce_ctx,
                            debounce_msg,
                            debounce_in_flight,
                            debounce_task_seq,
                            permit,
                        )
                        .await;
                    });
                    continue;
                }
                zeroclaw_infra::debounce::DebounceResult::Passthrough(content) => {
                    let mut m = msg;
                    m.content = content;
                    m
                }
            }
        } else {
            msg
        };

        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };

        let worker_ctx = Arc::clone(&ctx);
        let in_flight = Arc::clone(&in_flight_by_sender);
        let task_sequence = Arc::clone(&task_sequence);
        workers.spawn(async move {
            dispatch_worker(worker_ctx, msg, in_flight, task_sequence, permit).await;
        });

        while let Some(result) = workers.try_join_next() {
            log_worker_join_result(result);
        }
    }

    while let Some(result) = workers.join_next().await {
        log_worker_join_result(result);
    }
}

fn normalize_telegram_identity(value: &str) -> String {
    value.trim().trim_start_matches('@').to_string()
}

pub async fn bind_telegram_identity(config: &Config, identity: &str) -> Result<()> {
    use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};

    let normalized = normalize_telegram_identity(identity);
    if normalized.is_empty() {
        anyhow::bail!("Telegram identity cannot be empty");
    }

    let mut updated = config.clone();
    if !updated.channels.telegram.contains_key("default") {
        anyhow::bail!(
            "Telegram channel is not configured. Run \
             `zeroclaw config set channels.telegram.<alias>.bot-token=<token>` \
             (see docs/book/src/channels/overview.md for the full field list)."
        );
    }

    // Locate (or create) the peer group bound to telegram.default. The
    // V3 surface puts inbound peer authorization in `peer_groups`,
    // not on the channel block. Convention: the synthesized group
    // name is `<type>_<alias>` (matching what the V2→V3 fold uses)
    // so a hand-bound identity lands in the same group an operator
    // would inspect after an upgrade. The `channel` field is the
    // dotted alias ref so authorization stays scoped to the bound
    // alias; a bare type would broaden the peer across every
    // telegram alias on the install.
    let group_name = "telegram_default".to_string();
    let group = updated
        .peer_groups
        .entry(group_name.clone())
        .or_insert_with(|| PeerGroupConfig {
            channel: "telegram.default".to_string(),
            ..PeerGroupConfig::default()
        });

    if group
        .external_peers
        .iter()
        .any(|p| normalize_telegram_identity(p.as_str()) == normalized)
    {
        println!("✅ Telegram identity already bound: {normalized}");
        return Ok(());
    }

    group
        .external_peers
        .push(PeerUsername::new(normalized.clone()));
    updated.save().await?;
    println!("✅ Bound Telegram identity: {normalized}");
    println!("   Saved to {}", updated.config_path.display());
    match maybe_restart_managed_daemon_service() {
        Ok(true) => {
            println!("🔄 Detected running managed daemon service; reloaded automatically.");
        }
        Ok(false) => {
            println!(
                "ℹ️ No managed daemon service detected. If `zeroclaw daemon`/`channel start` is already running, restart it to load the updated allowlist."
            );
        }
        Err(e) => {
            eprintln!(
                "⚠️ Allowlist saved, but failed to reload daemon service automatically: {e}\n\
                 Restart service manually with `zeroclaw service stop && zeroclaw service start`."
            );
        }
    }
    Ok(())
}

fn maybe_restart_managed_daemon_service() -> Result<bool> {
    if cfg!(target_os = "macos") {
        let home = directories::UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .context("Could not find home directory")?;
        let plist = home
            .join("Library")
            .join("LaunchAgents")
            .join("com.zeroclaw.daemon.plist");
        if !plist.exists() {
            return Ok(false);
        }

        let list_output = Command::new("launchctl")
            .arg("list")
            .output()
            .context("Failed to query launchctl list")?;
        let listed = String::from_utf8_lossy(&list_output.stdout);
        if !listed.contains("com.zeroclaw.daemon") {
            return Ok(false);
        }

        let _ = Command::new("launchctl")
            .args(["stop", "com.zeroclaw.daemon"])
            .output();
        let start_output = Command::new("launchctl")
            .args(["start", "com.zeroclaw.daemon"])
            .output()
            .context("Failed to start launchd daemon service")?;
        if !start_output.status.success() {
            let stderr = String::from_utf8_lossy(&start_output.stderr);
            anyhow::bail!("launchctl start failed: {}", stderr.trim());
        }

        return Ok(true);
    }

    if cfg!(target_os = "linux") {
        // OpenRC (system-wide) takes precedence over systemd (user-level)
        let openrc_init_script = PathBuf::from("/etc/init.d/zeroclaw");
        if openrc_init_script.exists()
            && let Ok(status_output) = Command::new("rc-service").args(OPENRC_STATUS_ARGS).output()
        {
            // rc-service exits 0 if running, non-zero otherwise
            if status_output.status.success() {
                let restart_output = Command::new("rc-service")
                    .args(OPENRC_RESTART_ARGS)
                    .output()
                    .context("Failed to restart OpenRC daemon service")?;
                if !restart_output.status.success() {
                    let stderr = String::from_utf8_lossy(&restart_output.stderr);
                    anyhow::bail!("rc-service restart failed: {}", stderr.trim());
                }
                return Ok(true);
            }
        }

        // Systemd (user-level)
        let home = directories::UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .context("Could not find home directory")?;
        let unit_path: PathBuf = home
            .join(".config")
            .join("systemd")
            .join("user")
            .join("zeroclaw.service");
        if !unit_path.exists() {
            return Ok(false);
        }

        let active_output = Command::new("systemctl")
            .args(SYSTEMD_STATUS_ARGS)
            .output()
            .context("Failed to query systemd service state")?;
        let state = String::from_utf8_lossy(&active_output.stdout);
        if !state.trim().eq_ignore_ascii_case("active") {
            return Ok(false);
        }

        let restart_output = Command::new("systemctl")
            .args(SYSTEMD_RESTART_ARGS)
            .output()
            .context("Failed to restart systemd daemon service")?;
        if !restart_output.status.success() {
            let stderr = String::from_utf8_lossy(&restart_output.stderr);
            anyhow::bail!("systemctl restart failed: {}", stderr.trim());
        }

        return Ok(true);
    }

    Ok(false)
}

/// Build a single channel instance by config section name (e.g. "telegram").
fn build_channel_by_id(
    config_arc: &Arc<RwLock<Config>>,
    channel_id: &str,
) -> Result<Arc<dyn Channel>> {
    #[allow(unused_variables)]
    let config = config_arc.read();
    match channel_id {
        #[cfg(feature = "channel-telegram")]
        "telegram" => {
            let tg = config
                .channels
                .telegram
                .get("default")
                .context("Telegram channel is not configured")?;
            let ack = tg.ack_reactions.unwrap_or(config.channels.ack_reactions);
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("telegram", &alias))
            };
            Ok(Arc::new(
                TelegramChannel::new(
                    tg.bot_token.clone(),
                    alias.clone(),
                    peer_resolver,
                    tg.mention_only,
                )
                .with_persistence(config_arc.clone())
                .with_ack_reactions(ack)
                .with_streaming(tg.stream_mode, tg.draft_update_interval_ms)
                .with_transcription(config.transcription.clone())
                .with_tts(&config)
                .with_voice_peer_prefs(&config, "telegram", alias)
                .with_workspace_dir(config.data_dir.clone())
                .with_approval_timeout_secs(tg.approval_timeout_secs),
            ))
        }
        #[cfg(not(feature = "channel-telegram"))]
        "telegram" => {
            anyhow::bail!("Telegram channel requires the `channel-telegram` feature");
        }
        #[cfg(feature = "channel-discord")]
        "discord" => {
            let dc = config
                .channels
                .discord
                .get("default")
                .context("Discord channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("discord", &alias))
            };
            Ok(Arc::new(
                DiscordChannel::new(
                    dc.bot_token.clone(),
                    dc.guild_ids.clone(),
                    alias,
                    peer_resolver,
                    dc.listen_to_bots,
                    dc.mention_only,
                )
                .with_channel_ids(dc.channel_ids.clone())
                .with_workspace_dir(config.data_dir.clone())
                .with_streaming(
                    dc.stream_mode,
                    dc.draft_update_interval_ms,
                    dc.multi_message_delay_ms,
                )
                .with_transcription(config.transcription.clone())
                .with_stall_timeout(dc.stall_timeout_secs)
                .with_approval_timeout_secs(dc.approval_timeout_secs),
            ))
        }
        #[cfg(not(feature = "channel-discord"))]
        "discord" => {
            anyhow::bail!("Discord channel requires the `channel-discord` feature");
        }
        #[cfg(feature = "channel-slack")]
        "slack" => {
            let sl = config
                .channels
                .slack
                .get("default")
                .context("Slack channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("slack", &alias))
            };
            Ok(Arc::new(
                SlackChannel::new(
                    sl.bot_token.clone(),
                    sl.app_token.clone(),
                    sl.channel_ids.clone(),
                    alias,
                    peer_resolver,
                )
                .with_workspace_dir(config.data_dir.clone())
                .with_markdown_blocks(sl.use_markdown_blocks)
                .with_transcription(config.transcription.clone())
                .with_streaming(sl.stream_drafts, sl.draft_update_interval_ms)
                .with_cancel_reaction(sl.cancel_reaction.clone())
                .with_approval_timeout_secs(sl.approval_timeout_secs),
            ))
        }
        #[cfg(not(feature = "channel-slack"))]
        "slack" => {
            anyhow::bail!("Slack channel requires the `channel-slack` feature");
        }
        #[cfg(feature = "channel-mattermost")]
        "mattermost" => {
            let mm = config
                .channels
                .mattermost
                .get("default")
                .context("Mattermost channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("mattermost", &alias))
            };
            Ok(Arc::new(
                MattermostChannel::new(
                    mm.url.clone(),
                    mm.bot_token.clone(),
                    mm.login_id.clone(),
                    mm.password.clone(),
                    mm.channel_ids.clone(),
                    alias,
                    peer_resolver,
                    mm.thread_replies.unwrap_or(true),
                    mm.mention_only.unwrap_or(false),
                )
                .with_team_ids(mm.team_ids.clone())
                .with_discover_dms(mm.discover_dms.unwrap_or(true)),
            ))
        }
        #[cfg(not(feature = "channel-mattermost"))]
        "mattermost" => {
            anyhow::bail!("Mattermost channel requires the `channel-mattermost` feature");
        }
        #[cfg(feature = "channel-signal")]
        "signal" => {
            let sg = config
                .channels
                .signal
                .get("default")
                .context("Signal channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("signal", &alias))
            };
            Ok(Arc::new(
                SignalChannel::new(
                    sg.http_url.clone(),
                    sg.account.clone(),
                    sg.group_ids.clone(),
                    sg.dm_only,
                    alias,
                    peer_resolver,
                    sg.ignore_attachments,
                    sg.ignore_stories,
                )
                .with_approval_timeout_secs(sg.approval_timeout_secs),
            ))
        }
        #[cfg(not(feature = "channel-signal"))]
        "signal" => {
            anyhow::bail!("Signal channel requires the `channel-signal` feature");
        }
        "matrix" => {
            #[cfg(feature = "channel-matrix")]
            {
                let mx = config
                    .channels
                    .matrix
                    .get("default")
                    .context("Matrix channel is not configured")?;
                let alias = "default".to_string();
                let state_dir = matrix_state_dir(&config.config_path, &alias);
                let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                    let cfg_arc = config_arc.clone();
                    let alias = alias.clone();
                    Arc::new(move || cfg_arc.read().channel_external_peers("matrix", &alias))
                };
                let ack = mx.ack_reactions.unwrap_or(config.channels.ack_reactions);
                Ok(Arc::new(
                    MatrixChannel::new(mx.clone(), alias, peer_resolver, state_dir)?
                        .with_transcription(config.transcription.clone())
                        .with_workspace_dir(config.data_dir.clone())
                        .with_ack_reactions(ack),
                ))
            }
            #[cfg(not(feature = "channel-matrix"))]
            {
                anyhow::bail!("Matrix channel requires the `channel-matrix` feature");
            }
        }
        "whatsapp" | "whatsapp-web" | "whatsapp_web" => {
            #[cfg(feature = "whatsapp-web")]
            {
                let wa = config
                    .channels
                    .whatsapp
                    .get("default")
                    .context("WhatsApp channel is not configured")?;
                if !wa.is_web_config() {
                    anyhow::bail!(
                        "WhatsApp channel send requires Web mode (session_path must be set)"
                    );
                }
                let alias = "default".to_string();
                let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                    let cfg_arc = config_arc.clone();
                    let alias = alias.clone();
                    Arc::new(move || cfg_arc.read().channel_external_peers("whatsapp", &alias))
                };
                Ok(Arc::new(WhatsAppWebChannel::new(wa, alias, peer_resolver)))
            }
            #[cfg(not(feature = "whatsapp-web"))]
            {
                anyhow::bail!("WhatsApp channel requires the `whatsapp-web` feature");
            }
        }
        #[cfg(feature = "channel-qq")]
        "qq" => {
            let qq = config
                .channels
                .qq
                .get("default")
                .context("QQ channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("qq", &alias))
            };
            Ok(Arc::new(QQChannel::new(
                qq.app_id.clone(),
                qq.app_secret.clone(),
                alias,
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-qq"))]
        "qq" => {
            anyhow::bail!("QQ channel requires the `channel-qq` feature");
        }
        "lark" => {
            #[cfg(feature = "channel-lark")]
            {
                let lk = config
                    .channels
                    .lark
                    .get("default")
                    .context("Lark channel is not configured")?;
                let alias = "default".to_string();
                let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                    let cfg_arc = config_arc.clone();
                    let alias = alias.clone();
                    Arc::new(move || cfg_arc.read().channel_external_peers("lark", &alias))
                };
                Ok(Arc::new(
                    LarkChannel::from_config(lk, alias, peer_resolver)
                        .with_approval_timeout_secs(lk.approval_timeout_secs)
                        .with_per_user_session(lk.per_user_session)
                        .with_streaming(lk.stream_mode, lk.draft_update_interval_ms),
                ))
            }
            #[cfg(not(feature = "channel-lark"))]
            {
                anyhow::bail!("Lark channel requires the `channel-lark` feature");
            }
        }
        #[cfg(feature = "channel-dingtalk")]
        "dingtalk" => {
            let dt = config
                .channels
                .dingtalk
                .get("default")
                .context("DingTalk channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("dingtalk", &alias))
            };
            Ok(Arc::new(
                DingTalkChannel::new(
                    dt.client_id.clone(),
                    dt.client_secret.clone(),
                    alias,
                    peer_resolver,
                )
                .with_proxy_url(dt.proxy_url.clone()),
            ))
        }
        #[cfg(not(feature = "channel-dingtalk"))]
        "dingtalk" => {
            anyhow::bail!("DingTalk channel requires the `channel-dingtalk` feature");
        }
        #[cfg(feature = "channel-wecom")]
        "wecom" => {
            let wc = config
                .channels
                .wecom
                .get("default")
                .context("WeCom channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("wecom", &alias))
            };
            Ok(Arc::new(WeComChannel::new(
                wc.webhook_key.clone(),
                alias,
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-wecom"))]
        "wecom" => {
            anyhow::bail!("WeCom channel requires the `channel-wecom` feature");
        }
        #[cfg(feature = "channel-wecom-ws")]
        channel_id
            if channel_id == "wecom_ws"
                || channel_id == "wecom-ws"
                || channel_id.starts_with("wecom_ws.")
                || channel_id.starts_with("wecom-ws.") =>
        {
            let alias = channel_id
                .split_once('.')
                .map(|(_, alias)| alias)
                .unwrap_or("default")
                .to_string();
            let wc =
                config.channels.wecom_ws.get(&alias).with_context(|| {
                    format!("WeCom WebSocket channel '{alias}' is not configured")
                })?;
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                let configured_allowed_users = wc.allowed_users.clone();
                Arc::new(move || {
                    let config = cfg_arc.read();
                    let mut peers = configured_allowed_users.clone();
                    for peer in config.channel_external_peers("wecom-ws", &alias) {
                        if !peers.contains(&peer) {
                            peers.push(peer);
                        }
                    }
                    for peer in config.channel_external_peers("wecom_ws", &alias) {
                        if !peers.contains(&peer) {
                            peers.push(peer);
                        }
                    }
                    peers
                })
            };
            Ok(Arc::new(WeComWsChannel::new_with_alias(
                wc,
                alias.clone(),
                peer_resolver,
                &config.channel_workspace_dir(&format!("wecom_ws.{alias}")),
            )?))
        }
        #[cfg(not(feature = "channel-wecom-ws"))]
        channel_id
            if channel_id == "wecom_ws"
                || channel_id == "wecom-ws"
                || channel_id.starts_with("wecom_ws.")
                || channel_id.starts_with("wecom-ws.") =>
        {
            anyhow::bail!("WeCom WebSocket channel requires the `channel-wecom-ws` feature");
        }
        #[cfg(feature = "channel-wechat")]
        "wechat" => {
            let wc = config
                .channels
                .wechat
                .get("default")
                .context("WeChat channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("wechat", &alias))
            };
            Ok(Arc::new(
                WeChatChannel::new(
                    alias,
                    peer_resolver,
                    wc.api_base_url.clone(),
                    wc.cdn_base_url.clone(),
                    wc.state_dir.as_ref().map(|s| expand_tilde_in_path(s)),
                )?
                .with_persistence(config_arc.clone())
                .with_workspace_dir(config.data_dir.clone()),
            ))
        }
        #[cfg(not(feature = "channel-wechat"))]
        "wechat" => {
            anyhow::bail!("WeChat channel requires the `channel-wechat` feature");
        }
        #[cfg(feature = "channel-nextcloud")]
        "nextcloud_talk" | "nextcloud-talk" => {
            let nc = config
                .channels
                .nextcloud_talk
                .get("default")
                .context("Nextcloud Talk channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || {
                    cfg_arc
                        .read()
                        .channel_external_peers("nextcloud_talk", &alias)
                })
            };
            Ok(Arc::new(
                NextcloudTalkChannel::new_with_proxy(
                    nc.base_url.clone(),
                    nc.app_token.clone(),
                    nc.bot_name.clone().unwrap_or_default(),
                    alias,
                    peer_resolver,
                    nc.proxy_url.clone(),
                )
                .with_streaming(nc.stream_mode, nc.draft_update_interval_ms),
            ))
        }
        #[cfg(not(feature = "channel-nextcloud"))]
        "nextcloud_talk" | "nextcloud-talk" => {
            anyhow::bail!("Nextcloud Talk channel requires the `channel-nextcloud` feature");
        }
        #[cfg(feature = "channel-wati")]
        "wati" => {
            let wati_cfg = config
                .channels
                .wati
                .get("default")
                .context("WATI channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("wati", &alias))
            };
            Ok(Arc::new(WatiChannel::new_with_proxy(
                wati_cfg.api_token.clone(),
                wati_cfg.api_url.clone(),
                wati_cfg.tenant_id.clone(),
                alias,
                peer_resolver,
                wati_cfg.proxy_url.clone(),
            )))
        }
        #[cfg(not(feature = "channel-wati"))]
        "wati" => {
            anyhow::bail!("WATI channel requires the `channel-wati` feature");
        }
        #[cfg(feature = "channel-linq")]
        "linq" => {
            let lq = config
                .channels
                .linq
                .get("default")
                .context("Linq channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("linq", &alias))
            };
            Ok(Arc::new(LinqChannel::new(
                lq.api_token.clone(),
                lq.from_phone.clone(),
                alias,
                peer_resolver,
            )))
        }
        #[cfg(feature = "channel-linq")]
        x if x.starts_with("linq.") => {
            let alias = x.strip_prefix("linq.").context("invalid linq channel id")?;
            let lq = config
                .channels
                .linq
                .get(alias)
                .with_context(|| format!("Linq alias '{alias}' not configured"))?;
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.to_string();
                Arc::new(move || cfg_arc.read().channel_external_peers("linq", &alias))
            };
            Ok(Arc::new(LinqChannel::new(
                lq.api_token.clone(),
                lq.from_phone.clone(),
                alias.to_string(),
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-linq"))]
        x if x.starts_with("linq") => {
            anyhow::bail!("Linq channel requires the `channel-linq` feature");
        }
        #[cfg(feature = "channel-email")]
        "email" => {
            let em = config
                .channels
                .email
                .get("default")
                .context("Email channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("email", &alias))
            };
            Ok(Arc::new(EmailChannel::new(
                em.clone(),
                alias,
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-email"))]
        "email" => {
            anyhow::bail!("Email channel requires the `channel-email` feature");
        }
        #[cfg(feature = "channel-email")]
        "gmail_push" | "gmail-push" => {
            let gp = config
                .channels
                .gmail_push
                .get("default")
                .context("Gmail Push channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("gmail_push", &alias))
            };
            Ok(Arc::new(GmailPushChannel::new(
                gp.clone(),
                alias,
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-email"))]
        "gmail_push" | "gmail-push" => {
            anyhow::bail!("Gmail Push channel requires the `channel-email` feature");
        }
        #[cfg(feature = "channel-irc")]
        "irc" => {
            let irc_cfg = config
                .channels
                .irc
                .get("default")
                .context("IRC channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("irc", &alias))
            };
            Ok(Arc::new(IrcChannel::new(crate::irc::IrcChannelConfig {
                server: irc_cfg.server.clone(),
                port: irc_cfg.port,
                nickname: irc_cfg.nickname.clone(),
                username: irc_cfg.username.clone(),
                channels: irc_cfg.channels.clone(),
                alias,
                peer_resolver,
                server_password: irc_cfg.server_password.clone(),
                nickserv_password: irc_cfg.nickserv_password.clone(),
                sasl_password: irc_cfg.sasl_password.clone(),
                verify_tls: irc_cfg.verify_tls.unwrap_or(true),
                mention_only: irc_cfg.mention_only,
            })))
        }
        #[cfg(not(feature = "channel-irc"))]
        "irc" => {
            anyhow::bail!("IRC channel requires the `channel-irc` feature");
        }
        #[cfg(feature = "channel-twitch")]
        "twitch" => {
            let tw_cfg = config
                .channels
                .twitch
                .get("default")
                .context("Twitch channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("twitch", &alias))
            };
            Ok(Arc::new(TwitchChannel::new(
                tw_cfg.bot_username.clone(),
                tw_cfg.oauth_token.clone(),
                tw_cfg.channels.clone(),
                tw_cfg.mention_only,
                alias,
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-twitch"))]
        "twitch" => {
            anyhow::bail!("Twitch channel requires the `channel-twitch` feature");
        }
        #[cfg(feature = "channel-twitter")]
        "twitter" => {
            let tw = config
                .channels
                .twitter
                .get("default")
                .context("X/Twitter channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("twitter", &alias))
            };
            Ok(Arc::new(TwitterChannel::new(
                tw.bearer_token.clone(),
                alias,
                peer_resolver,
            )))
        }
        #[cfg(not(feature = "channel-twitter"))]
        "twitter" => {
            anyhow::bail!("X/Twitter channel requires the `channel-twitter` feature");
        }
        #[cfg(feature = "channel-mochat")]
        "mochat" => {
            let mc = config
                .channels
                .mochat
                .get("default")
                .context("Mochat channel is not configured")?;
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("mochat", &alias))
            };
            Ok(Arc::new(MochatChannel::new(
                mc.api_url.clone(),
                mc.api_token.clone(),
                alias,
                peer_resolver,
                mc.poll_interval_secs,
            )))
        }
        #[cfg(not(feature = "channel-mochat"))]
        "mochat" => {
            anyhow::bail!("Mochat channel requires the `channel-mochat` feature");
        }
        #[cfg(feature = "channel-imessage")]
        "imessage" => {
            if !config.channels.imessage.contains_key("default") {
                anyhow::bail!("iMessage channel is not configured");
            }
            let alias = "default".to_string();
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("imessage", &alias))
            };
            Ok(Arc::new(IMessageChannel::new(alias, peer_resolver)))
        }
        #[cfg(not(feature = "channel-imessage"))]
        "imessage" => {
            anyhow::bail!("iMessage channel requires the `channel-imessage` feature");
        }
        "line" => {
            #[cfg(feature = "channel-line")]
            {
                let ln = config
                    .channels
                    .line
                    .get("default")
                    .context("LINE channel is not configured")?;
                let alias = "default".to_string();
                let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                    let cfg_arc = config_arc.clone();
                    let alias = alias.clone();
                    Arc::new(move || cfg_arc.read().channel_external_peers("line", &alias))
                };
                Ok(Arc::new(
                    LineChannel::from_config(ln, alias, peer_resolver)
                        .with_persistence(config_arc.clone()),
                ))
            }
            #[cfg(not(feature = "channel-line"))]
            {
                anyhow::bail!("LINE channel requires the `channel-line` feature");
            }
        }
        "voice-call" => {
            #[cfg(feature = "channel-voice-call")]
            {
                let (alias, vc) = config
                    .channels
                    .voice_call
                    .iter()
                    .next()
                    .context("Voice Call channel is not configured")?;
                Ok(Arc::new(VoiceCallChannel::new(alias.clone(), vc.clone())))
            }
            #[cfg(not(feature = "channel-voice-call"))]
            {
                anyhow::bail!("Voice Call channel requires the `channel-voice-call` feature");
            }
        }
        other => anyhow::bail!(
            "Unknown channel '{other}'. Supported: telegram, discord, slack, mattermost, signal, \
            matrix, whatsapp, qq, lark, feishu, dingtalk, wecom, wecom_ws, nextcloud_talk, wati, linq, \
            email, gmail_push, irc, twitter, mochat, imessage, line, voice-call"
        ),
    }
}

/// Send a one-off message to a configured channel.
pub async fn send_channel_message(
    config: &Config,
    channel_id: &str,
    recipient: &str,
    message: &str,
) -> Result<()> {
    // Wrap into the canonical shared handle for the builder; this is a
    // one-shot path so the snapshot is dropped immediately after send.
    let config_arc = Arc::new(RwLock::new(config.clone()));
    let channel = build_channel_by_id(&config_arc, channel_id)?;
    let msg = SendMessage::new(message, recipient);
    channel
        .send(&msg)
        .await
        .with_context(|| format!("Failed to send message via {channel_id}"))?;
    println!("Message sent via {channel_id}.");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelHealthState {
    Healthy,
    Unhealthy,
    Timeout,
}

fn classify_health_result(
    result: &std::result::Result<bool, tokio::time::error::Elapsed>,
) -> ChannelHealthState {
    match result {
        Ok(true) => ChannelHealthState::Healthy,
        Ok(false) => ChannelHealthState::Unhealthy,
        Err(_) => ChannelHealthState::Timeout,
    }
}

struct ConfiguredChannel {
    display_name: &'static str,
    /// ZeroClaw channel alias (the `<alias>` half of `[channels.<type>.<alias>]`).
    /// `Some` for every aliased channel built in `collect_configured_channels`;
    /// `None` for singleton channels with no alias concept (e.g. Notion).
    /// Used by `composite_channel_key` to give each `(type, alias)` pair a
    /// distinct slot in the runtime `channels_by_name` registry so two bots
    /// on the same platform (e.g. `discord.clamps` + `discord.glados`) don't
    /// collide and silently overwrite each other.
    alias: Option<String>,
    channel: Arc<dyn Channel>,
}

/// Compose the registry key for a channel given its `name()` and configured alias.
/// Aliased channels live at `<name>.<alias>`; un-aliased singletons keep the bare name.
pub(crate) fn composite_channel_key(name: &str, alias: Option<&str>) -> String {
    match alias.filter(|s| !s.is_empty()) {
        Some(alias) => format!("{name}.{alias}"),
        None => name.to_string(),
    }
}

/// Look up the live channel handle that should send a reply to `msg`.
///
/// Resolution order:
/// 1. Composite key `<channel>.<channel_alias>` — fires for multi-alias platforms
///    (Discord/Telegram/Slack/etc. with multiple `[channels.<type>.<alias>]` blocks).
/// 2. Bare `msg.channel` — singleton channels and legacy callers that didn't
///    supply an alias.
/// 3. `<base>:<qualifier>` split (e.g. Matrix `matrix:!roomId`) falls back to
///    the base channel name.
fn find_channel_for_message<'a>(
    channels: &'a HashMap<String, Arc<dyn Channel>>,
    msg: &zeroclaw_api::channel::ChannelMessage,
) -> Option<&'a Arc<dyn Channel>> {
    if let Some(alias) = msg.channel_alias.as_deref().filter(|s| !s.is_empty()) {
        let composite = format!("{}.{alias}", msg.channel);
        if let Some(ch) = channels.get(&composite) {
            return Some(ch);
        }
    }
    if let Some(ch) = channels.get(&msg.channel) {
        return Some(ch);
    }
    msg.channel
        .split_once(':')
        .and_then(|(base, _)| channels.get(base))
}

/// Active `<type>.<alias>` channel references from enabled agents.
///
/// An empty set means no enabled agent declared channel bindings, so
/// collection falls back to legacy behavior and accepts all enabled channels.
struct ActiveChannelAliases {
    /// Set of `<type>.<alias>` channel references from enabled agents.
    aliases: HashSet<String>,
}

impl ActiveChannelAliases {
    /// Returns true when `channel_ref` is explicitly bound, or when there are
    /// no explicit bindings and legacy "accept all enabled channels" mode applies.
    fn contains(&self, channel_ref: &str) -> bool {
        self.aliases.is_empty() || self.aliases.contains(channel_ref)
    }
}

/// Build `channel_key → Arc<dyn Channel>` map from config.
///
/// Constructs channel instances without starting listen loops.
/// Called by CLI and other callers that need a channel map
/// for late-bound tool handle population.
pub fn build_channel_map(
    config: &Config,
) -> HashMap<String, Arc<dyn zeroclaw_api::channel::Channel>> {
    let config_arc = Arc::new(RwLock::new(config.clone()));
    collect_configured_channels(&config_arc, "", &[])
        .into_iter()
        .map(|ch| {
            let key = composite_channel_key(ch.channel.name(), ch.alias.as_deref());
            (key, ch.channel)
        })
        .collect()
}

/// Build configured channels and register them into late-bound tool handles.
///
/// Constructs channel instances from config (without starting listen loops)
/// and inserts each into the provided handles under their composite key
/// (`<channel>.<alias>` or bare `<channel>` for singletons).
///
/// Returns the list of registered channel names for logging.
pub fn register_channels_for_tools(
    config: &Config,
    ask_user_handle: &Option<tools::PerToolChannelHandle>,
    reaction_handle: &Option<tools::PerToolChannelHandle>,
    poll_handle: &Option<tools::PerToolChannelHandle>,
    escalate_handle: &Option<tools::PerToolChannelHandle>,
) -> Vec<String> {
    let config_arc = Arc::new(RwLock::new(config.clone()));
    let configured = collect_configured_channels(&config_arc, "", &[]);

    let handles = [
        ask_user_handle.as_ref(),
        reaction_handle.as_ref(),
        poll_handle.as_ref(),
        escalate_handle.as_ref(),
    ];

    let mut names = Vec::new();
    for ch in &configured {
        let key = composite_channel_key(ch.channel.name(), ch.alias.as_deref());
        for handle in handles.iter().flatten() {
            handle.write().insert(key.clone(), Arc::clone(&ch.channel));
        }
        names.push(key);
    }
    names
}

/// Per-alias Matrix state directory. Each `[channels.matrix.<alias>]` block
/// must own its own session/crypto store so two bots under one daemon don't
/// restore each other's `session.json` and run as the wrong account. The
/// alias component is what keeps them distinct.
#[cfg(feature = "channel-matrix")]
fn matrix_state_dir(config_path: &std::path::Path, alias: &str) -> std::path::PathBuf {
    config_path
        .parent()
        .map(|p| p.join("state").join("matrix").join(alias))
        .unwrap_or_else(|| std::path::PathBuf::from(".zeroclaw/state/matrix").join(alias))
}

fn collect_configured_channels(
    config_arc: &Arc<RwLock<Config>>,
    matrix_skip_context: &str,
    tool_specs: &[(String, String)],
) -> Vec<ConfiguredChannel> {
    let _ = matrix_skip_context;
    let _ = tool_specs;
    #[allow(unused_mut)]
    let mut channels = Vec::new();

    // Shadow `config` with a read guard so the existing body keeps
    // working via `Deref<Target = Config>`. Resolver closures that
    // outlive the function capture `config_arc.clone()`.
    let config = config_arc.read();

    let active_channel_aliases = ActiveChannelAliases {
        aliases: config
            .agents
            .values()
            .filter(|a| a.enabled)
            .flat_map(|a| a.channels.iter().map(|c| c.as_str().to_string()))
            .collect(),
    };

    #[cfg(feature = "channel-telegram")]
    for (alias, tg) in &config.channels.telegram {
        if !active_channel_aliases.contains(&format!("telegram.{alias}")) {
            continue;
        }
        if !tg.enabled {
            continue;
        }
        let ack = tg.ack_reactions.unwrap_or(config.channels.ack_reactions);
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("telegram", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Telegram",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(
                Arc::new(
                    TelegramChannel::new(
                        tg.bot_token.clone(),
                        alias.clone(),
                        peer_resolver,
                        tg.mention_only,
                    )
                    .with_persistence(config_arc.clone())
                    .with_ack_reactions(ack)
                    .with_streaming(tg.stream_mode, tg.draft_update_interval_ms)
                    .with_transcription(config.transcription.clone())
                    .with_tts(&config)
                    .with_voice_peer_prefs(&config, "telegram", alias)
                    .with_workspace_dir(config.channel_workspace_dir(&format!("telegram.{alias}")))
                    .with_proxy_url(tg.proxy_url.clone())
                    .with_tool_command_specs(tool_specs.to_vec())
                    .with_approval_timeout_secs(tg.approval_timeout_secs),
                ),
                tg,
            ),
        });
    }

    #[cfg(not(feature = "channel-telegram"))]
    if !config.channels.telegram.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Telegram channel is configured but this build was compiled without \
             `channel-telegram`; skipping Telegram."
        );
    }

    #[cfg(feature = "channel-discord")]
    for (alias, dc) in &config.channels.discord {
        if !active_channel_aliases.contains(&format!("discord.{alias}")) {
            continue;
        }
        if !dc.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("discord", &alias))
        };
        let mut discord_ch = DiscordChannel::new(
            dc.bot_token.clone(),
            dc.guild_ids.clone(),
            alias.clone(),
            peer_resolver,
            dc.listen_to_bots,
            dc.mention_only,
        )
        .with_channel_ids(dc.channel_ids.clone())
        .with_workspace_dir(config.channel_workspace_dir(&format!("discord.{alias}")))
        .with_streaming(
            dc.stream_mode,
            dc.draft_update_interval_ms,
            dc.multi_message_delay_ms,
        )
        .with_proxy_url(dc.proxy_url.clone())
        .with_transcription(config.transcription.clone())
        .with_stall_timeout(dc.stall_timeout_secs)
        .with_approval_timeout_secs(dc.approval_timeout_secs);
        if dc.archive {
            match zeroclaw_memory::SqliteMemory::new_named("sqlite", &config.data_dir, "discord") {
                Ok(mem) => {
                    discord_ch = discord_ch.with_archive_memory(std::sync::Arc::new(mem));
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "discord: archive enabled but failed to open discord.db"
                    );
                }
            }
        }
        channels.push(ConfiguredChannel {
            display_name: "Discord",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(Arc::new(discord_ch), dc),
        });
    }

    #[cfg(not(feature = "channel-discord"))]
    if !config.channels.discord.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Discord channel is configured but this build was compiled without \
             `channel-discord`; skipping Discord."
        );
    }

    #[cfg(feature = "channel-slack")]
    for (alias, sl) in &config.channels.slack {
        if !active_channel_aliases.contains(&format!("slack.{alias}")) {
            continue;
        }
        if !sl.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("slack", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Slack",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(
                Arc::new(
                    SlackChannel::new(
                        sl.bot_token.clone(),
                        sl.app_token.clone(),
                        sl.channel_ids.clone(),
                        alias.clone(),
                        peer_resolver,
                    )
                    .with_thread_replies(sl.thread_replies.unwrap_or(true))
                    .with_group_reply_policy(sl.mention_only, Vec::new())
                    .with_strict_mention_in_thread(sl.strict_mention_in_thread)
                    .with_workspace_dir(config.channel_workspace_dir(&format!("slack.{alias}")))
                    .with_markdown_blocks(sl.use_markdown_blocks)
                    .with_proxy_url(sl.proxy_url.clone())
                    .with_transcription(config.transcription.clone())
                    .with_streaming(sl.stream_drafts, sl.draft_update_interval_ms)
                    .with_cancel_reaction(sl.cancel_reaction.clone())
                    .with_approval_timeout_secs(sl.approval_timeout_secs),
                ),
                sl,
            ),
        });
    }

    #[cfg(not(feature = "channel-slack"))]
    if !config.channels.slack.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Slack channel is configured but this build was compiled without \
             `channel-slack`; skipping Slack."
        );
    }

    #[cfg(feature = "channel-mattermost")]
    for (alias, mm) in &config.channels.mattermost {
        if !active_channel_aliases.contains(&format!("mattermost.{alias}")) {
            continue;
        }
        if !mm.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("mattermost", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Mattermost",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(
                Arc::new(
                    MattermostChannel::new(
                        mm.url.clone(),
                        mm.bot_token.clone(),
                        mm.login_id.clone(),
                        mm.password.clone(),
                        mm.channel_ids.clone(),
                        alias.clone(),
                        peer_resolver,
                        mm.thread_replies.unwrap_or(true),
                        mm.mention_only.unwrap_or(false),
                    )
                    .with_team_ids(mm.team_ids.clone())
                    .with_discover_dms(mm.discover_dms.unwrap_or(true))
                    .with_proxy_url(mm.proxy_url.clone())
                    .with_transcription(config.transcription.clone()),
                ),
                mm,
            ),
        });
    }

    #[cfg(not(feature = "channel-mattermost"))]
    if !config.channels.mattermost.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Mattermost channel is configured but this build was compiled without \
             `channel-mattermost`; skipping Mattermost."
        );
    }

    #[cfg(feature = "channel-imessage")]
    for (alias, im) in &config.channels.imessage {
        if !active_channel_aliases.contains(&format!("imessage.{alias}")) {
            continue;
        }
        if !im.enabled {
            continue;
        }
        let _ = im;
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("imessage", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "iMessage",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(
                Arc::new(IMessageChannel::new(alias.clone(), peer_resolver)),
                im,
            ),
        });
    }

    #[cfg(not(feature = "channel-imessage"))]
    if !config.channels.imessage.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "iMessage channel is configured but this build was compiled without \
             `channel-imessage`; skipping iMessage."
        );
    }

    #[cfg(feature = "channel-matrix")]
    for (alias, mx) in &config.channels.matrix {
        if !active_channel_aliases.contains(&format!("matrix.{alias}")) {
            continue;
        }
        if !mx.enabled {
            continue;
        }
        let state_dir = matrix_state_dir(&config.config_path, alias);
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("matrix", &alias))
        };
        let ack = mx.ack_reactions.unwrap_or(config.channels.ack_reactions);
        match MatrixChannel::new(mx.clone(), alias.clone(), peer_resolver, state_dir) {
            Ok(channel) => {
                let channel = channel
                    .with_transcription(config.transcription.clone())
                    .with_workspace_dir(config.channel_workspace_dir(&format!("matrix.{alias}")))
                    .with_ack_reactions(ack);
                channels.push(ConfiguredChannel {
                    display_name: "Matrix",
                    alias: Some(alias.clone()),
                    channel: crate::paced_channel::PacedChannel::wrap(Arc::new(channel), mx),
                });
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Matrix channel construction failed"
                );
            }
        }
    }

    #[cfg(not(feature = "channel-matrix"))]
    if !config.channels.matrix.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "Matrix channel is configured but this build was compiled without `channel-matrix`; skipping Matrix {}.",
                matrix_skip_context
            )
        );
    }

    #[cfg(feature = "channel-signal")]
    for (alias, sig) in &config.channels.signal {
        if !active_channel_aliases.contains(&format!("signal.{alias}")) {
            continue;
        }
        if !sig.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("signal", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Signal",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(
                Arc::new(
                    SignalChannel::new(
                        sig.http_url.clone(),
                        sig.account.clone(),
                        sig.group_ids.clone(),
                        sig.dm_only,
                        alias.clone(),
                        peer_resolver,
                        sig.ignore_attachments,
                        sig.ignore_stories,
                    )
                    .with_proxy_url(sig.proxy_url.clone())
                    .with_approval_timeout_secs(sig.approval_timeout_secs),
                ),
                sig,
            ),
        });
    }

    #[cfg(not(feature = "channel-signal"))]
    if !config.channels.signal.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Signal channel is configured but this build was compiled without \
             `channel-signal`; skipping Signal."
        );
    }

    #[cfg(any(feature = "channel-whatsapp-cloud", feature = "whatsapp-web"))]
    for (alias, wa) in &config.channels.whatsapp {
        if !active_channel_aliases.contains(&format!("whatsapp.{alias}")) {
            continue;
        }
        if !wa.enabled {
            continue;
        }
        if wa.is_ambiguous_config() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "WhatsApp config has both phone_number_id and session_path set; preferring Cloud API mode. Remove one selector to avoid ambiguity."
            );
        }
        // Runtime negotiation: detect backend type from config
        match wa.backend_type() {
            #[cfg(feature = "channel-whatsapp-cloud")]
            "cloud" => {
                // Cloud API mode: requires phone_number_id, access_token, verify_token
                if wa.is_cloud_config() {
                    let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                        let cfg_arc = config_arc.clone();
                        let alias = alias.clone();
                        Arc::new(move || cfg_arc.read().channel_external_peers("whatsapp", &alias))
                    };
                    channels.push(ConfiguredChannel {
                        display_name: "WhatsApp",
                        alias: Some(alias.clone()),
                        channel: crate::paced_channel::PacedChannel::wrap(
                            Arc::new(
                                WhatsAppChannel::new(
                                    wa.access_token.clone().unwrap_or_default(),
                                    wa.phone_number_id.clone().unwrap_or_default(),
                                    wa.verify_token.clone().unwrap_or_default(),
                                    alias.clone(),
                                    peer_resolver,
                                )
                                .with_proxy_url(wa.proxy_url.clone())
                                .with_dm_mention_patterns(wa.dm_mention_patterns.clone())
                                .with_group_mention_patterns(wa.group_mention_patterns.clone())
                                .with_approval_timeout_secs(wa.approval_timeout_secs),
                            ),
                            wa,
                        ),
                    });
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "WhatsApp Cloud API configured but missing required fields (phone_number_id, access_token, verify_token)"
                    );
                }
                #[cfg(not(feature = "channel-whatsapp-cloud"))]
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "WhatsApp Cloud API backend requires 'channel-whatsapp-cloud' feature. Build/run with --features channel-whatsapp-cloud"
                    );
                }
            }
            #[cfg(not(feature = "channel-whatsapp-cloud"))]
            "cloud" => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "WhatsApp Cloud API is configured but this build was compiled without `channel-whatsapp-cloud`; skipping WhatsApp Cloud."
                );
            }
            "web" => {
                // Web mode: requires session_path
                #[cfg(feature = "whatsapp-web")]
                if wa.is_web_config() {
                    let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                        let cfg_arc = config_arc.clone();
                        let alias = alias.clone();
                        Arc::new(move || cfg_arc.read().channel_external_peers("whatsapp", &alias))
                    };
                    channels.push(ConfiguredChannel {
                        display_name: "WhatsApp",
                        alias: Some(alias.clone()),
                        channel: crate::paced_channel::PacedChannel::wrap(
                            Arc::new(
                                WhatsAppWebChannel::new(wa, alias.clone(), peer_resolver)
                                    .with_transcription(config.transcription.clone())
                                    .with_tts(&config)
                                    .with_dm_mention_patterns(wa.dm_mention_patterns.clone())
                                    .with_group_mention_patterns(wa.group_mention_patterns.clone()),
                            ),
                            wa,
                        ),
                    });
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "WhatsApp Web configured but session_path not set"
                    );
                }
                #[cfg(not(feature = "whatsapp-web"))]
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "WhatsApp Web backend requires 'whatsapp-web' feature. Enable with: cargo build --features whatsapp-web"
                    );
                    eprintln!(
                        "  ⚠ WhatsApp Web is configured but the 'whatsapp-web' feature is not compiled in."
                    );
                    eprintln!("    Rebuild with: cargo build --features whatsapp-web");
                }
            }
            _ => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "WhatsApp config invalid: neither phone_number_id (Cloud API) nor session_path (Web) is set"
                );
            }
        }
    }

    #[cfg(feature = "channel-linq")]
    for (alias, lq) in &config.channels.linq {
        if !active_channel_aliases.contains(&format!("linq.{alias}")) {
            continue;
        }
        if !lq.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("linq", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Linq",
            alias: Some(alias.clone()),
            channel: Arc::new(LinqChannel::new(
                lq.api_token.clone(),
                lq.from_phone.clone(),
                alias.clone(),
                peer_resolver,
            )),
        });
    }

    #[cfg(not(feature = "channel-linq"))]
    if !config.channels.linq.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Linq channel is configured but this build was compiled without \
             `channel-linq`; skipping Linq."
        );
    }

    #[cfg(feature = "channel-wati")]
    for (alias, wati_cfg) in &config.channels.wati {
        if !active_channel_aliases.contains(&format!("wati.{alias}")) {
            continue;
        }
        if !wati_cfg.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("wati", &alias))
        };
        let wati_channel = WatiChannel::new_with_proxy(
            wati_cfg.api_token.clone(),
            wati_cfg.api_url.clone(),
            wati_cfg.tenant_id.clone(),
            alias.clone(),
            peer_resolver,
            wati_cfg.proxy_url.clone(),
        )
        .with_transcription(config.transcription.clone());
        channels.push(ConfiguredChannel {
            display_name: "WATI",
            alias: Some(alias.clone()),
            channel: Arc::new(wati_channel),
        });
    }

    #[cfg(not(feature = "channel-wati"))]
    if !config.channels.wati.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "WATI channel is configured but this build was compiled without \
             `channel-wati`; skipping WATI."
        );
    }

    #[cfg(feature = "channel-nextcloud")]
    for (alias, nc) in &config.channels.nextcloud_talk {
        if !active_channel_aliases.contains(&format!("nextcloud_talk.{alias}")) {
            continue;
        }
        if !nc.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || {
                cfg_arc
                    .read()
                    .channel_external_peers("nextcloud_talk", &alias)
            })
        };
        channels.push(ConfiguredChannel {
            display_name: "Nextcloud Talk",
            alias: Some(alias.clone()),
            channel: Arc::new(NextcloudTalkChannel::new_with_proxy(
                nc.base_url.clone(),
                nc.app_token.clone(),
                nc.bot_name.clone().unwrap_or_default(),
                alias.clone(),
                peer_resolver,
                nc.proxy_url.clone(),
            )),
        });
    }

    #[cfg(not(feature = "channel-nextcloud"))]
    if !config.channels.nextcloud_talk.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Nextcloud Talk channel is configured but this build was compiled without \
             `channel-nextcloud`; skipping Nextcloud Talk."
        );
    }

    #[cfg(feature = "channel-email")]
    for (alias, email_cfg) in &config.channels.email {
        if !active_channel_aliases.contains(&format!("email.{alias}")) {
            continue;
        }
        if !email_cfg.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("email", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Email",
            alias: Some(alias.clone()),
            channel: Arc::new(EmailChannel::new(
                email_cfg.clone(),
                alias.clone(),
                peer_resolver,
            )),
        });
    }

    #[cfg(feature = "channel-email")]
    for (alias, gp_cfg) in &config.channels.gmail_push {
        if !active_channel_aliases.contains(&format!("gmail_push.{alias}")) {
            continue;
        }
        if !gp_cfg.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("gmail_push", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Gmail Push",
            alias: Some(alias.clone()),
            channel: Arc::new(GmailPushChannel::new(
                gp_cfg.clone(),
                alias.clone(),
                peer_resolver,
            )),
        });
    }

    #[cfg(not(feature = "channel-email"))]
    if !config.channels.email.is_empty() || !config.channels.gmail_push.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Email/Gmail Push channel is configured but this build was compiled without \
             `channel-email`; skipping Email and Gmail Push."
        );
    }

    #[cfg(feature = "channel-irc")]
    for (alias, irc) in &config.channels.irc {
        if !active_channel_aliases.contains(&format!("irc.{alias}")) {
            continue;
        }
        if !irc.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("irc", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "IRC",
            alias: Some(alias.clone()),
            channel: Arc::new(IrcChannel::new(crate::irc::IrcChannelConfig {
                server: irc.server.clone(),
                port: irc.port,
                nickname: irc.nickname.clone(),
                username: irc.username.clone(),
                channels: irc.channels.clone(),
                alias: alias.clone(),
                peer_resolver,
                server_password: irc.server_password.clone(),
                nickserv_password: irc.nickserv_password.clone(),
                sasl_password: irc.sasl_password.clone(),
                verify_tls: irc.verify_tls.unwrap_or(true),
                mention_only: irc.mention_only,
            })),
        });
    }

    #[cfg(not(feature = "channel-irc"))]
    if !config.channels.irc.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "IRC channel is configured but this build was compiled without \
             `channel-irc`; skipping IRC."
        );
    }

    #[cfg(feature = "channel-amqp")]
    for (alias, amqp) in &config.channels.amqp {
        if !active_channel_aliases.contains(&format!("amqp.{alias}")) {
            continue;
        }
        if !amqp.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("amqp", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "AMQP",
            alias: Some(alias.clone()),
            channel: Arc::new(AmqpChannel::new(crate::amqp::AmqpChannelConfig {
                amqp_url: amqp.amqp_url.clone(),
                exchange: amqp.exchange.clone(),
                routing_keys: amqp.routing_keys.clone(),
                queue: amqp.queue.clone(),
                ca_cert: amqp.ca_cert.clone(),
                client_cert: amqp.client_cert.clone(),
                client_key: amqp.client_key.clone(),
                sender_label: amqp.sender_label.clone(),
                content_template: amqp.content_template.clone(),
                thread_id_field: amqp.thread_id_field.clone(),
                durable_ack: amqp.durable_ack,
                alias: alias.clone(),
                peer_resolver,
            })),
        });
    }

    #[cfg(not(feature = "channel-amqp"))]
    if !config.channels.amqp.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "AMQP channel is configured but this build was compiled without \
             `channel-amqp`; skipping AMQP."
        );
    }

    #[cfg(feature = "channel-twitch")]
    for (alias, tw) in &config.channels.twitch {
        if !active_channel_aliases.contains(&format!("twitch.{alias}")) {
            continue;
        }
        if !tw.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("twitch", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Twitch",
            alias: Some(alias.clone()),
            channel: Arc::new(TwitchChannel::new(
                tw.bot_username.clone(),
                tw.oauth_token.clone(),
                tw.channels.clone(),
                tw.mention_only,
                alias.clone(),
                peer_resolver,
            )),
        });
    }

    #[cfg(not(feature = "channel-twitch"))]
    if !config.channels.twitch.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Twitch channel is configured but this build was compiled without \
             `channel-twitch`; skipping Twitch."
        );
    }

    #[cfg(feature = "channel-lark")]
    for (alias, lk) in &config.channels.lark {
        if !active_channel_aliases.contains(&format!("lark.{alias}")) {
            continue;
        }
        if !lk.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("lark", &alias))
        };
        let display_name = if lk.use_feishu { "Feishu" } else { "Lark" };
        channels.push(ConfiguredChannel {
            display_name,
            alias: Some(alias.clone()),
            channel: Arc::new(
                LarkChannel::from_config(lk, alias.clone(), peer_resolver)
                    .with_approval_timeout_secs(lk.approval_timeout_secs)
                    .with_per_user_session(lk.per_user_session)
                    .with_streaming(lk.stream_mode, lk.draft_update_interval_ms)
                    .with_transcription(config.transcription.clone()),
            ),
        });
    }

    #[cfg(not(feature = "channel-lark"))]
    if !config.channels.lark.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Lark/Feishu channel is configured but this build was compiled without `channel-lark`; skipping Lark/Feishu health check."
        );
    }

    #[cfg(feature = "channel-line")]
    for (alias, ln) in &config.channels.line {
        if !active_channel_aliases.contains(&format!("line.{alias}")) {
            continue;
        }
        if !ln.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("line", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "LINE",
            alias: Some(alias.clone()),
            channel: Arc::new(
                LineChannel::from_config(ln, alias.clone(), peer_resolver)
                    .with_persistence(config_arc.clone())
                    .with_transcription(config.transcription.clone()),
            ),
        });
    }

    #[cfg(not(feature = "channel-line"))]
    if !config.channels.line.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "LINE channel is configured but this build was compiled without `channel-line`; skipping LINE health check."
        );
    }

    #[cfg(feature = "channel-dingtalk")]
    for (alias, dt) in &config.channels.dingtalk {
        if !active_channel_aliases.contains(&format!("dingtalk.{alias}")) {
            continue;
        }
        if !dt.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("dingtalk", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "DingTalk",
            alias: Some(alias.clone()),
            channel: Arc::new(
                DingTalkChannel::new(
                    dt.client_id.clone(),
                    dt.client_secret.clone(),
                    alias.clone(),
                    peer_resolver,
                )
                .with_proxy_url(dt.proxy_url.clone()),
            ),
        });
    }

    #[cfg(not(feature = "channel-dingtalk"))]
    if !config.channels.dingtalk.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "DingTalk channel is configured but this build was compiled without \
             `channel-dingtalk`; skipping DingTalk."
        );
    }

    #[cfg(feature = "channel-qq")]
    for (alias, qq) in &config.channels.qq {
        if !active_channel_aliases.contains(&format!("qq.{alias}")) {
            continue;
        }
        if !qq.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("qq", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "QQ",
            alias: Some(alias.clone()),
            channel: Arc::new(
                QQChannel::new(
                    qq.app_id.clone(),
                    qq.app_secret.clone(),
                    alias.clone(),
                    peer_resolver,
                )
                .with_workspace_dir(config.channel_workspace_dir(&format!("qq.{alias}")))
                .with_proxy_url(qq.proxy_url.clone())
                .with_transcription(config.transcription.clone()),
            ),
        });
    }

    #[cfg(not(feature = "channel-qq"))]
    if !config.channels.qq.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "QQ channel is configured but this build was compiled without \
             `channel-qq`; skipping QQ."
        );
    }

    #[cfg(feature = "channel-twitter")]
    for (alias, tw) in &config.channels.twitter {
        if !active_channel_aliases.contains(&format!("twitter.{alias}")) {
            continue;
        }
        if !tw.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("twitter", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "X/Twitter",
            alias: Some(alias.clone()),
            channel: Arc::new(TwitterChannel::new(
                tw.bearer_token.clone(),
                alias.clone(),
                peer_resolver,
            )),
        });
    }

    #[cfg(not(feature = "channel-twitter"))]
    if !config.channels.twitter.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "X/Twitter channel is configured but this build was compiled without \
             `channel-twitter`; skipping X/Twitter."
        );
    }

    #[cfg(feature = "channel-mochat")]
    for (alias, mc) in &config.channels.mochat {
        if !active_channel_aliases.contains(&format!("mochat.{alias}")) {
            continue;
        }
        if !mc.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("mochat", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "Mochat",
            alias: Some(alias.clone()),
            channel: Arc::new(MochatChannel::new(
                mc.api_url.clone(),
                mc.api_token.clone(),
                alias.clone(),
                peer_resolver,
                mc.poll_interval_secs,
            )),
        });
    }

    #[cfg(not(feature = "channel-mochat"))]
    if !config.channels.mochat.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Mochat channel is configured but this build was compiled without \
             `channel-mochat`; skipping Mochat."
        );
    }

    #[cfg(feature = "channel-wecom")]
    for (alias, wc) in &config.channels.wecom {
        if !active_channel_aliases.contains(&format!("wecom.{alias}")) {
            continue;
        }
        if !wc.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("wecom", &alias))
        };
        channels.push(ConfiguredChannel {
            display_name: "WeCom",
            alias: Some(alias.clone()),
            channel: Arc::new(WeComChannel::new(
                wc.webhook_key.clone(),
                alias.clone(),
                peer_resolver,
            )),
        });
    }

    #[cfg(not(feature = "channel-wecom"))]
    if !config.channels.wecom.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "WeCom channel is configured but this build was compiled without \
             `channel-wecom`; skipping WeCom."
        );
    }

    #[cfg(feature = "channel-wecom-ws")]
    for (alias, wc_ws) in &config.channels.wecom_ws {
        if !active_channel_aliases.contains(&format!("wecom_ws.{alias}"))
            && !active_channel_aliases.contains(&format!("wecom-ws.{alias}"))
        {
            continue;
        }
        if !wc_ws.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            let configured_allowed_users = wc_ws.allowed_users.clone();
            Arc::new(move || {
                let config = cfg_arc.read();
                let mut peers = configured_allowed_users.clone();
                for peer in config.channel_external_peers("wecom-ws", &alias) {
                    if !peers.contains(&peer) {
                        peers.push(peer);
                    }
                }
                for peer in config.channel_external_peers("wecom_ws", &alias) {
                    if !peers.contains(&peer) {
                        peers.push(peer);
                    }
                }
                peers
            })
        };
        match WeComWsChannel::new_with_alias(
            wc_ws,
            alias.clone(),
            peer_resolver,
            &config.channel_workspace_dir(&format!("wecom_ws.{alias}")),
        ) {
            Ok(channel) => channels.push(ConfiguredChannel {
                display_name: "WeCom WebSocket",
                alias: Some(alias.clone()),
                channel: Arc::new(channel),
            }),
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{err:#}")})),
                    format!(
                        "WeCom WebSocket channel configuration is invalid; skipping WeCom WebSocket {matrix_skip_context}"
                    ),
                );
            }
        }
    }

    #[cfg(not(feature = "channel-wecom-ws"))]
    if !config.channels.wecom_ws.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            format!(
                "WeCom WebSocket channel is configured but this build was compiled without `channel-wecom-ws`; skipping WeCom WebSocket {matrix_skip_context}."
            ),
        );
    }

    #[cfg(feature = "channel-wechat")]
    for (alias, wechat) in &config.channels.wechat {
        if !active_channel_aliases.contains(&format!("wechat.{alias}")) {
            continue;
        }
        if !wechat.enabled {
            continue;
        }
        let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
            let cfg_arc = config_arc.clone();
            let alias = alias.clone();
            Arc::new(move || cfg_arc.read().channel_external_peers("wechat", &alias))
        };
        match WeChatChannel::new(
            alias.clone(),
            peer_resolver,
            wechat.api_base_url.clone(),
            wechat.cdn_base_url.clone(),
            wechat.state_dir.as_ref().map(|s| expand_tilde_in_path(s)),
        ) {
            Ok(channel) => {
                channels.push(ConfiguredChannel {
                    display_name: "WeChat",
                    alias: Some(alias.clone()),
                    channel: Arc::new(
                        channel
                            .with_persistence(config_arc.clone())
                            .with_workspace_dir(
                                config.channel_workspace_dir(&format!("wechat.{alias}")),
                            ),
                    ),
                });
            }
            Err(err) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"matrix_skip_context": matrix_skip_context, "err": err.to_string()})), "WeChat channel configuration is invalid; skipping WeChat");
            }
        }
    }

    #[cfg(not(feature = "channel-wechat"))]
    for alias in config.channels.wechat.keys() {
        if active_channel_aliases.contains(&format!("wechat.{alias}")) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"matrix_skip_context": matrix_skip_context})),
                "WeChat channel is configured but this build was compiled without `channel-wechat`; skipping WeChat ."
            );
        }
    }

    #[cfg(feature = "channel-clawdtalk")]
    for (alias, ct) in &config.channels.clawdtalk {
        if !active_channel_aliases.contains(&format!("clawdtalk.{alias}")) {
            continue;
        }
        if !ct.enabled {
            continue;
        }
        channels.push(ConfiguredChannel {
            display_name: "ClawdTalk",
            alias: Some(alias.clone()),
            channel: Arc::new(ClawdTalkChannel::new(alias.clone(), ct.clone())),
        });
    }

    #[cfg(not(feature = "channel-clawdtalk"))]
    if !config.channels.clawdtalk.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "ClawdTalk channel is configured but this build was compiled without \
             `channel-clawdtalk`; skipping ClawdTalk."
        );
    }

    // Notion database poller channel
    #[cfg(feature = "channel-notion")]
    if config.notion.enabled && !config.notion.database_id.trim().is_empty() {
        let notion_api_key = config.notion.api_key.trim().to_string();
        if notion_api_key.is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Notion channel enabled but `notion.api_key` is unset. Set it via the schema-mirror grammar: \
                 `ZEROCLAW_notion__api_key=...`."
            );
        } else {
            channels.push(ConfiguredChannel {
                display_name: "Notion",
                alias: None,
                channel: Arc::new(NotionChannel::new(
                    "notion",
                    notion_api_key,
                    config.notion.database_id.clone(),
                    config.notion.poll_interval_secs,
                    config.notion.status_property.clone(),
                    config.notion.input_property.clone(),
                    config.notion.result_property.clone(),
                    config.notion.max_concurrent,
                    config.notion.recover_stale,
                )),
            });
        }
    }

    #[cfg(not(feature = "channel-notion"))]
    if config.notion.enabled {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Notion channel is enabled but this build was compiled without \
             `channel-notion`; skipping Notion."
        );
    }

    #[cfg(feature = "channel-reddit")]
    for (alias, rd) in &config.channels.reddit {
        if !active_channel_aliases.contains(&format!("reddit.{alias}")) {
            continue;
        }
        if !rd.enabled {
            continue;
        }
        channels.push(ConfiguredChannel {
            display_name: "Reddit",
            alias: Some(alias.clone()),
            channel: Arc::new(RedditChannel::new(
                alias.clone(),
                rd.client_id.clone(),
                rd.client_secret.clone(),
                rd.refresh_token.clone(),
                rd.username.clone(),
                rd.subreddits.clone(),
            )),
        });
    }

    #[cfg(not(feature = "channel-reddit"))]
    if !config.channels.reddit.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Reddit channel is configured but this build was compiled without \
             `channel-reddit`; skipping Reddit."
        );
    }

    #[cfg(feature = "channel-bluesky")]
    for (alias, bs) in &config.channels.bluesky {
        if !active_channel_aliases.contains(&format!("bluesky.{alias}")) {
            continue;
        }
        if !bs.enabled {
            continue;
        }
        channels.push(ConfiguredChannel {
            display_name: "Bluesky",
            alias: Some(alias.clone()),
            channel: Arc::new(BlueskyChannel::new(
                alias.clone(),
                bs.handle.clone(),
                bs.app_password.clone(),
            )),
        });
    }

    #[cfg(not(feature = "channel-bluesky"))]
    if !config.channels.bluesky.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Bluesky channel is configured but this build was compiled without \
             `channel-bluesky`; skipping Bluesky."
        );
    }

    #[cfg(feature = "voice-wake")]
    for (alias, vw) in &config.channels.voice_wake {
        if !active_channel_aliases.contains(&format!("voice_wake.{alias}")) {
            continue;
        }
        if !vw.enabled {
            continue;
        }
        channels.push(ConfiguredChannel {
            display_name: "VoiceWake",
            alias: Some(alias.clone()),
            channel: Arc::new(VoiceWakeChannel::new(
                alias.clone(),
                vw.clone(),
                config.transcription.clone(),
            )),
        });
    }

    #[cfg(not(feature = "voice-wake"))]
    if !config.channels.voice_wake.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "VoiceWake channel is configured but this build was compiled without \
             `voice-wake`; skipping VoiceWake."
        );
    }

    #[cfg(feature = "channel-voice-call")]
    for (alias, vc) in &config.channels.voice_call {
        if !active_channel_aliases.contains(&format!("voice_call.{alias}")) {
            continue;
        }
        if !vc.enabled {
            continue;
        }
        channels.push(ConfiguredChannel {
            display_name: "Voice Call",
            alias: Some(alias.clone()),
            channel: Arc::new(VoiceCallChannel::new(alias.clone(), vc.clone())),
        });
    }

    #[cfg(not(feature = "channel-voice-call"))]
    if !config.channels.voice_call.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Voice Call channel is configured but this build was compiled without \
             `channel-voice-call`; skipping Voice Call."
        );
    }

    #[cfg(feature = "channel-webhook")]
    for (alias, wh) in &config.channels.webhook {
        if !active_channel_aliases.contains(&format!("webhook.{alias}")) {
            continue;
        }
        if !wh.enabled {
            continue;
        }
        channels.push(ConfiguredChannel {
            display_name: "Webhook",
            alias: Some(alias.clone()),
            channel: crate::paced_channel::PacedChannel::wrap(
                Arc::new(WebhookChannel::new(
                    alias.clone(),
                    wh.port,
                    wh.listen_path.clone(),
                    wh.send_url.clone(),
                    wh.send_method.clone(),
                    wh.auth_header.clone(),
                    wh.secret.clone(),
                    wh.max_retries,
                    wh.retry_base_delay_ms,
                    wh.retry_max_delay_ms,
                )),
                wh,
            ),
        });
    }

    #[cfg(not(feature = "channel-webhook"))]
    if !config.channels.webhook.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Webhook channel is configured but this build was compiled without \
             `channel-webhook`; skipping Webhook."
        );
    }

    channels
}

fn no_real_time_channels_message() -> &'static str {
    "No real-time channels configured. Run `zeroclaw quickstart` to set one up."
}

/// Run health checks for configured channels.
pub async fn doctor_channels(config: Config) -> Result<()> {
    let config_arc = Arc::new(RwLock::new(config));
    #[allow(unused_mut)]
    let mut channels = collect_configured_channels(&config_arc, "health check", &[]);

    #[cfg(feature = "channel-nostr")]
    {
        // Materialize the work list into owned values BEFORE any `.await`
        // so the RwLockReadGuard is dropped before the async constructor
        // runs (parking_lot guards are not Send).
        let nostr_jobs: Vec<(String, String, Vec<String>)> = {
            let config = config_arc.read();
            let active_nostr: std::collections::HashSet<String> = config
                .agents
                .values()
                .filter(|a| a.enabled)
                .flat_map(|a| a.channels.iter().map(|c| c.as_str().to_string()))
                .collect();
            config
                .channels
                .nostr
                .iter()
                .filter(|(alias, _)| active_nostr.contains(&format!("nostr.{alias}")))
                .map(|(alias, ns)| (alias.clone(), ns.private_key.clone(), ns.relays.clone()))
                .collect()
        };
        for (alias, private_key, relays) in nostr_jobs {
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                let cfg_arc = config_arc.clone();
                let alias = alias.clone();
                Arc::new(move || cfg_arc.read().channel_external_peers("nostr", &alias))
            };
            channels.push(ConfiguredChannel {
                display_name: "Nostr",
                alias: Some(alias.clone()),
                channel: Arc::new(
                    NostrChannel::new(&private_key, relays, alias, peer_resolver).await?,
                ),
            });
        }
    }

    #[cfg(not(feature = "channel-nostr"))]
    {
        let config = config_arc.read();
        if !config.channels.nostr.is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Nostr channel is configured but this build was compiled without \
                 `channel-nostr`; skipping Nostr health check."
            );
        }
    }

    if channels.is_empty() {
        println!("{}", no_real_time_channels_message());
        return Ok(());
    }

    println!("🩺 ZeroClaw Channel Doctor");
    println!();

    let mut healthy = 0_u32;
    let mut unhealthy = 0_u32;
    let mut timeout = 0_u32;

    for configured in channels {
        let result =
            tokio::time::timeout(Duration::from_secs(10), configured.channel.health_check()).await;
        let state = classify_health_result(&result);

        match state {
            ChannelHealthState::Healthy => {
                healthy += 1;
                println!("  ✅ {:<9} healthy", configured.display_name);
            }
            ChannelHealthState::Unhealthy => {
                unhealthy += 1;
                println!(
                    "  ❌ {:<9} unhealthy (auth/config/network)",
                    configured.display_name
                );
            }
            ChannelHealthState::Timeout => {
                timeout += 1;
                println!("  ⏱️  {:<9} timed out (>10s)", configured.display_name);
            }
        }
    }

    if !config_arc.read().channels.webhook.is_empty() {
        println!("  ℹ️  Webhook   check via `zeroclaw gateway` then GET /health");
    }

    println!();
    println!("Summary: {healthy} healthy, {unhealthy} unhealthy, {timeout} timed out");
    Ok(())
}

fn build_owner_by_channel_key(
    config: &Config,
    enabled_agents: &[String],
    collected_channel_keys: &[String],
) -> HashMap<String, String> {
    // Owner map: `<channel_type>.<alias>` (and bare `<channel_type>` for
    // backward-compat with cron callers / singleton channels) → agent_alias.
    // Built from each enabled agent's `agents.<alias>.channels` list — the
    // schema treats this as the source of truth for channel ownership.
    let mut owner_by_channel_key: HashMap<String, String> = HashMap::new();
    for alias_str in enabled_agents {
        let Some(agent_cfg) = config.agents.get(alias_str) else {
            debug_assert!(
                false,
                "enabled agent alias missing from config.agents: {}",
                alias_str
            );
            continue;
        };
        for ch in &agent_cfg.channels {
            let ch_str: &str = ch.as_ref();
            owner_by_channel_key.insert(ch_str.to_string(), alias_str.clone());
            if let Some((bare, _)) = ch_str.split_once('.') {
                owner_by_channel_key
                    .entry(bare.to_string())
                    .or_insert_with(|| alias_str.clone());
            }
        }
    }

    // Legacy fallback mode: when no enabled agent declares channel bindings,
    // channel collection accepts all enabled channels. Those channels must
    // also be routable, so bind collected channel keys to the runtime-active
    // agent selection (explicit `"default"` alias when present, else
    // lexicographically-smallest enabled alias).
    // `owner_by_channel_key.is_empty()` means every enabled agent had an
    // empty `agents.<alias>.channels` list; this is the same "legacy mode"
    // signal used by `collect_configured_channels` to accept all enabled
    // channel blocks.
    if owner_by_channel_key.is_empty() && !collected_channel_keys.is_empty() {
        let fallback_owner = config
            .resolved_runtime_agent_alias()
            .filter(|alias| enabled_agents.iter().any(|enabled| enabled == *alias))
            .map(ToString::to_string)
            .or_else(|| enabled_agents.first().cloned());

        if let Some(owner_alias) = fallback_owner {
            for channel_key in collected_channel_keys {
                owner_by_channel_key.insert(channel_key.clone(), owner_alias.clone());
                if let Some((bare, _)) = channel_key.split_once('.') {
                    owner_by_channel_key
                        .entry(bare.to_string())
                        .or_insert_with(|| owner_alias.clone());
                }
            }
        }
    }

    owner_by_channel_key
}

/// Start all configured channels and route messages to the agent
#[allow(clippy::too_many_lines)]
pub async fn start_channels(
    config: Config,
    canvas_store: Option<zeroclaw_runtime::tools::CanvasStore>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    // Wrap into the canonical shared handle so channels and persistence
    // paths share one source of truth. The local `config` shadowing
    // keeps this function's body (which threads `config` through dozens
    // of sync reads and awaits) compatible with the old `Config` shape
    // via a one-time clone; channels themselves consult `config_arc`.
    let config_arc = Arc::new(RwLock::new(config));
    let config: Config = config_arc.read().clone();
    // No agent's model provider resolves yet — the user has channels
    // configured but hasn't finished onboarding their model_provider.
    // Returning Ok() here lets the daemon supervisor mark the channels
    // component "done" instead of restart-looping. The user completes
    // onboarding at /onboard and reloads via /admin/reload to bring channels
    // up. Resolution is strict: an enabled agent counts only if its mandatory
    // `<type>.<alias>` ref resolves to a configured entry with a `model`.
    let any_agent_provider_resolves = config
        .agents
        .iter()
        .filter(|(_, a)| a.enabled)
        .any(|(_, a)| runtime_defaults_from_config(&config, a.model_provider.as_str()).is_ok());
    if !any_agent_provider_resolves {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Channels supervisor: no model configured. Waiting for reload \
             (complete onboarding at /onboard or set \
             [providers.models.<type>.<alias>] model = \"...\" and reload)."
        );
        cancel.cancelled().await;
        return Ok(());
    }

    // Every `[channels.<type>.<alias>]` block is owned by exactly one agent
    // (declared via `agents.<alias>.channels = [...]`). One
    // `ChannelRuntimeContext` per enabled agent; `AgentRouter::multi` resolves
    // each inbound message to the owning agent. Discord/Telegram/Slack/etc.
    // sockets stay shared at the channel layer.
    let enabled_agents: Vec<String> = {
        let mut v: Vec<String> = config
            .agents
            .iter()
            .filter(|(_, a)| a.enabled)
            .map(|(alias, _)| alias.clone())
            .collect();
        if v.is_empty() {
            anyhow::bail!("start_channels requires at least one enabled [agents.<alias>] entry");
        }
        v.sort();
        v
    };

    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let runtime: Arc<dyn platform::RuntimeAdapter> =
        Arc::from(platform::create_runtime(&config.runtime)?);

    // i18n is process-global; initialize once before the per-agent loop
    // touches tool descriptions.
    let i18n_locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(zeroclaw_runtime::i18n::detect_locale);
    zeroclaw_runtime::i18n::init(&i18n_locale);

    // Single session backend shared across agents — they're scoped by
    // `session_key` (which already encodes `<channel_type>.<alias>`), so
    // multiple agent ctxs reading the same backend never overlap.
    let shared_session_store: Option<Arc<dyn zeroclaw_infra::session_backend::SessionBackend>> =
        if config.channels.session_persistence {
            match zeroclaw_infra::make_session_backend(
                &config.data_dir,
                &config.channels.session_backend,
            ) {
                Ok(backend) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "📂 Session persistence enabled (backend: {})",
                            config.channels.session_backend
                        )
                    );
                    Some(backend)
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Session persistence disabled"
                    );
                    None
                }
            }
        } else {
            None
        };

    // Channel infrastructure (listeners, `channels_by_name`, the mpsc bus)
    // is built once inside the loop on the first iteration — the primary
    // agent's `tool_specs` are used to wire Telegram slash commands.
    // Subsequent iterations reuse `channels_by_name_shared` to populate
    // their tool handles and to seed their `ChannelRuntimeContext`.
    let mut channels_by_name_shared: Option<Arc<HashMap<String, Arc<dyn Channel>>>> = None;
    let mut collected_channel_keys: Vec<String> = Vec::new();
    let mut max_in_flight_messages: Option<usize> = None;
    let mut listener_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut rx_holder: Option<tokio::sync::mpsc::Receiver<zeroclaw_api::channel::ChannelMessage>> =
        None;

    let mut agent_ctxs: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();

    for agent_alias in &enabled_agents {
        let agent = config
            .resolved_agent_config(agent_alias)
            .with_context(|| format!("agents.{agent_alias} is not configured"))?;
        let risk_profile = config
            .risk_profile_for_agent(agent_alias)
            .with_context(|| {
                format!(
                    "agents.{agent_alias}.risk_profile does not name a configured risk_profiles entry"
                )
            })?
            .clone();

        // Resolve the agent's model provider strictly from its mandatory
        // `<type>.<alias>` reference. No fallback to a first/default provider:
        // an agent whose ref does not resolve to a configured entry with a
        // `model` is rejected here.
        let runtime_defaults = runtime_defaults_from_config(&config, agent.model_provider.as_str())
            .with_context(|| format!("agents.{agent_alias}.model_provider"))?;
        let provider_name = runtime_defaults.default_model_provider.clone();
        let model = runtime_defaults.model.clone();
        let temperature = runtime_defaults.temperature;
        let provider_api_key = runtime_defaults.api_key.clone();
        let provider_api_url = runtime_defaults.api_url.clone();
        let provider_reliability = runtime_defaults.reliability.clone();
        let provider_runtime_options =
            zeroclaw_providers::provider_runtime_options_for_agent(&config, agent_alias);
        let model_provider: Arc<dyn ModelProvider> = Arc::from(
            create_resilient_model_provider_nonblocking(
                Arc::new(config.clone()),
                &provider_name,
                provider_api_key.clone(),
                provider_api_url.clone(),
                provider_reliability.clone(),
                provider_runtime_options.clone(),
            )
            .await?,
        );

        if let Err(e) = model_provider.warmup().await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"error": format!("{}", e), "agent": agent_alias})
                    ),
                "ModelProvider warmup failed (non-fatal)"
            );
        }

        let security = Arc::new(SecurityPolicy::for_agent(&config, agent_alias)?);
        let mem: Arc<dyn Memory> = zeroclaw_memory::create_memory_for_agent(
            &config,
            agent_alias,
            provider_api_key.as_deref(),
        )
        .await?;
        let (composio_key, composio_entity_id) = if config.composio.enabled {
            (
                config.composio.api_key.as_deref(),
                Some(config.composio.entity_id.as_str()),
            )
        } else {
            (None, None)
        };

        // Per-agent workspace: `<install>/agents/<alias>/workspace/`. Holds
        // this agent's IDENTITY.md / SOUL.md / USER.md / TOOLS.md /
        // AGENTS.md / MEMORY.md — the personality files the gateway UI
        // edits via /config/agents/<alias>?tab=personality. The system
        // prompt builder below reads these to render the agent's voice;
        // file_read / file_write tools scope path access to this root.
        let workspace = config.agent_workspace_dir(agent_alias);
        // Per-agent skills: install-wide workspace + open_skills set,
        // unioned with this agent's declared `skill_bundles`.
        let skills =
            zeroclaw_runtime::skills::load_skills_for_agent(&workspace, &config, agent_alias);

        let all_tools_result_ch = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            &risk_profile,
            agent_alias,
            Arc::clone(&runtime),
            Arc::clone(&mem),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &workspace,
            &config.agents,
            provider_api_key.as_deref(),
            &config,
            canvas_store.clone(),
            false,
            None,
        );
        let mut built_tools = all_tools_result_ch.tools;
        let delegate_handle_ch = all_tools_result_ch.delegate_handle;

        // Wire peripheral tools (gpio_read/gpio_write etc.) so channel-driven
        // sessions (Telegram, Discord, etc.) can actuate hardware when
        // [peripherals] is configured. Mirrors the agent loop wiring.
        // The helper is safe to call unconditionally (returns empty when
        // no peripherals are wired).
        let peripheral_tools =
            zeroclaw_runtime::agent::loop_::load_peripheral_tools(config.peripherals.clone()).await;
        if !peripheral_tools.is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"count": peripheral_tools.len()})),
                "Peripheral tools added (channels orchestrator)"
            );
            built_tools.extend(peripheral_tools);
        }
        let reaction_handle_ch = all_tools_result_ch.reaction_handle;
        let ask_user_handle_ch = all_tools_result_ch.ask_user_handle;
        let poll_handle_ch = all_tools_result_ch.poll_handle;
        let escalate_handle_ch = all_tools_result_ch.escalate_handle;

        // ── Built-in SecurityPolicy tool gate (parity with agent::run /
        // process_message / from_config) ────────────────────────────────────
        // Apply the agent's allowlist (`allowed_tools`) AND denylist
        // (`excluded_tools`) to the eager built-in registry, BEFORE MCP and
        // skill tools are added. `start_channels` previously enforced only the
        // risk-profile denylist on the prompt catalog here — never the
        // per-agent allowlist on the registry sent to the model — so an agent
        // allowlisted to e.g. `file_read` still received raw `shell` /
        // `file_write` in its native tool specs. Filtering before skill
        // registration is also what lets a scoped elevation wrapper survive:
        // the raw target is removed while the distinct prefixed
        // `{skill}__{tool}` wrapper is appended later. MCP tools are injected
        // after this gate and are intentionally exempt (a restrictive allowlist
        // must not silently drop a server's tools); the risk-profile denylist
        // still applies to them.
        let before_policy_filter_ch = built_tools.len();
        apply_policy_tool_filter(&mut built_tools, Some(security.as_ref()), None);
        if built_tools.len() != before_policy_filter_ch {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "agent": agent_alias,
                        "before": before_policy_filter_ch,
                        "retained": built_tools.len(),
                        "policy_allowed": security.allowed_tools.as_ref().map(|v| v.len()),
                        "policy_excluded": security.excluded_tools.as_ref().map(|v| v.len()),
                    })),
                "Applied SecurityPolicy built-in tool filter (channel path)"
            );
        }

        // Wire MCP tools into the per-agent registry before freezing —
        // non-fatal. When `mcp.deferred_loading` is enabled, MCP tools are
        // exposed via a `tool_search` built-in rather than added eagerly.
        let mut deferred_section = String::new();
        let mut ch_activated_handle: Option<
            std::sync::Arc<std::sync::Mutex<zeroclaw_runtime::tools::ActivatedToolSet>>,
        > = None;
        // Resolution-only MCP wrappers for skill MCP elevation (kind = "mcp").
        let mut ch_mcp_elevation_arcs: Vec<std::sync::Arc<dyn zeroclaw_runtime::tools::Tool>> =
            Vec::new();
        if config.mcp.enabled && !config.mcp.servers.is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"agent": agent_alias})),
                &format!(
                    "Initializing MCP client — {} server(s) configured",
                    config.mcp.servers.len()
                )
            );
            match zeroclaw_runtime::tools::McpRegistry::connect_all(&config.mcp.servers).await {
                Ok(registry) => {
                    let registry = std::sync::Arc::new(registry);
                    ch_mcp_elevation_arcs =
                        zeroclaw_runtime::tools::collect_mcp_elevation_arcs(&registry).await;
                    if config.mcp.deferred_loading {
                        let deferred_set =
                            zeroclaw_runtime::tools::DeferredMcpToolSet::from_registry(
                                std::sync::Arc::clone(&registry),
                            )
                            .await;
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"agent": agent_alias})),
                            &format!(
                                "MCP deferred: {} tool stub(s) from {} server(s)",
                                deferred_set.len(),
                                registry.server_count()
                            )
                        );
                        deferred_section =
                            zeroclaw_runtime::tools::build_deferred_tools_section(&deferred_set);
                        let activated = std::sync::Arc::new(std::sync::Mutex::new(
                            zeroclaw_runtime::tools::ActivatedToolSet::new(),
                        ));
                        ch_activated_handle = Some(std::sync::Arc::clone(&activated));
                        built_tools.push(Box::new(zeroclaw_runtime::tools::ToolSearchTool::new(
                            deferred_set,
                            activated,
                        )));
                    } else {
                        let names = registry.tool_names();
                        let mut registered = 0usize;
                        for name in names {
                            if let Some(def) = registry.get_tool_def(&name).await {
                                let wrapper: std::sync::Arc<dyn Tool> = std::sync::Arc::new(
                                    zeroclaw_runtime::tools::McpToolWrapper::new(
                                        name,
                                        def,
                                        std::sync::Arc::clone(&registry),
                                    ),
                                );
                                if let Some(ref handle) = delegate_handle_ch {
                                    handle.write().push(std::sync::Arc::clone(&wrapper));
                                }
                                built_tools
                                    .push(Box::new(zeroclaw_runtime::tools::ArcToolRef(wrapper)));
                                registered += 1;
                            }
                        }
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"agent": agent_alias})),
                            &format!(
                                "MCP: {} tool(s) registered from {} server(s)",
                                registered,
                                registry.server_count()
                            )
                        );
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"agent_alias": agent_alias, "error": format!("{}", e)})), "MCP registry failed to initialize");
                }
            }
        }

        // Skill tools share the workspace-loaded `skills` Vec but each
        // agent gets its own `ToolBox` so per-agent security policies
        // gate execution.
        // Resolution registry = built-in arcs + resolution-only MCP wrappers.
        let skill_resolution_registry: Vec<std::sync::Arc<dyn zeroclaw_runtime::tools::Tool>> =
            all_tools_result_ch
                .unfiltered_tool_arcs
                .iter()
                .cloned()
                .chain(ch_mcp_elevation_arcs.iter().cloned())
                .collect();
        zeroclaw_runtime::tools::register_skill_tools_with_context(
            &mut built_tools,
            &skills,
            security.clone(),
            &skill_resolution_registry,
        );

        let tool_specs: Vec<(String, String)> = built_tools
            .iter()
            .map(|t| (t.name().to_string(), t.description().to_string()))
            .collect();

        let tools_registry = Arc::new(built_tools);

        let mut tool_descs: Vec<(&str, &str)> = vec![
            (
                "shell",
                "Execute terminal commands. Use when: running local checks, build/test commands, diagnostics. Don't use when: a safer dedicated tool exists, or command is destructive without approval.",
            ),
            (
                "file_read",
                "Read file contents. Use when: inspecting project files, configs, logs. Don't use when: a targeted search is enough.",
            ),
            (
                "file_write",
                "Write file contents. Use when: applying focused edits, scaffolding files, updating docs/code. Don't use when: side effects are unclear or file ownership is uncertain.",
            ),
            (
                "memory_store",
                "Save to memory. Use when: preserving durable preferences, decisions, key context. Don't use when: information is transient/noisy/sensitive without need.",
            ),
            (
                "memory_recall",
                "Search memory. Use when: retrieving prior decisions, user preferences, historical context. Don't use when: answer is already in current context.",
            ),
            (
                "memory_forget",
                "Delete a memory entry. Use when: memory is incorrect/stale or explicitly requested for removal. Don't use when: impact is uncertain.",
            ),
        ];

        if matches!(
            config.skills.prompt_injection_mode,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Compact
        ) {
            tool_descs.push((
                "read_skill",
                "Load the full source for an available skill by name. Use when: compact mode only shows a summary and you need the complete skill instructions.",
            ));
        }
        if config.browser.enabled {
            tool_descs.push((
                "browser_open",
                "Open approved HTTPS URLs in system browser (allowlist-only, no scraping)",
            ));
        }
        if config.composio.enabled {
            tool_descs.push((
                "composio",
                "Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). Use action='list' to discover actions, 'list_accounts' to retrieve connected account IDs, 'execute' to run (optionally with connected_account_id), and 'connect' for OAuth.",
            ));
        }
        tool_descs.push((
            "schedule",
            "Manage scheduled tasks (create/list/get/cancel/pause/resume). Supports recurring cron and one-shot delays.",
        ));
        tool_descs.push((
            "pushover",
            "Send a Pushover notification to your device. Requires PUSHOVER_TOKEN and PUSHOVER_USER_KEY in .env file.",
        ));
        if !config.agents.is_empty() {
            tool_descs.push((
                "delegate",
                "Delegate a subtask to a specialized agent. Use when: a task benefits from a different model (e.g. fast summarization, deep reasoning, code generation). The sub-agent runs a single prompt and returns its response.",
            ));
        }

        // Filter out tools excluded for non-CLI channels so this agent's
        // system prompt does not advertise them for channel-driven runs.
        {
            let active_profile = &risk_profile;
            let excluded = &active_profile.excluded_tools;
            if !excluded.is_empty() && active_profile.level != AutonomyLevel::Full {
                tool_descs.retain(|(name, _)| !excluded.iter().any(|ex| ex == name));
            }
        }
        let effective_tool_names: HashSet<&str> =
            tools_registry.iter().map(|tool| tool.name()).collect();
        tool_descs.retain(|(name, _)| effective_tool_names.contains(name));

        let bootstrap_max_chars = if agent.resolved.compact_context {
            Some(6000)
        } else {
            None
        };
        let native_tools = model_provider.supports_native_tools();
        let expose_text_tool_protocol = apply_text_tool_prompt_policy(
            native_tools,
            agent.resolved.strict_tool_parsing,
            &mut tool_descs,
            &mut deferred_section,
        );
        let mut system_prompt = build_system_prompt_with_mode_and_autonomy(
            &workspace,
            &model,
            &tool_descs,
            &skills,
            Some(&agent.identity),
            bootstrap_max_chars,
            Some(&risk_profile),
            native_tools,
            config.skills.prompt_injection_mode,
            agent.resolved.compact_context,
            agent.resolved.max_system_prompt_chars,
            true,
        );
        if expose_text_tool_protocol {
            system_prompt.push_str(&build_tool_instructions_for_names(
                tools_registry.as_ref(),
                &effective_tool_names,
            ));
        }
        if !deferred_section.is_empty() {
            system_prompt.push('\n');
            system_prompt.push_str(&deferred_section);
        }
        if agent.resolved.tool_receipts.enabled && agent.resolved.tool_receipts.inject_system_prompt
        {
            system_prompt.push_str(
                "\n## Tool Execution Receipts\n\n\
                 Every tool result includes a `[receipt: ...]` field. This is a cryptographic \
                 signature proving the tool actually executed. You must include the receipt \
                 verbatim when referencing tool results. Do not modify, omit, or fabricate receipts. \
                 A missing or invalid receipt indicates a fabricated tool call.\n\n",
            );
        }

        // === First iteration only: set up shared channel infrastructure ===
        //
        // We collect channels here (using *this* agent's `tool_specs`, since
        // the loop puts the primary agent first) and stash the
        // `channels_by_name` registry so subsequent iterations can populate
        // their tool handles without re-building Discord/Telegram/etc.
        // sockets. The first agent's `tool_specs` wire Telegram-style slash
        // commands; multi-agent installs that want per-bot command sets
        // require a future per-channel `tool_specs` lookup (tracked
        // alongside the per-channel ChannelRuntimeContext follow-up).
        if channels_by_name_shared.is_none() {
            if !skills.is_empty() {
                println!(
                    "  🧩 Skills:   {}",
                    skills
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            #[allow(unused_mut)]
            let mut configured_channels: Vec<ConfiguredChannel> =
                collect_configured_channels(&config_arc, "runtime startup", &tool_specs);

            #[cfg(feature = "channel-nostr")]
            if let Some((alias, ns)) = config.channels.nostr.iter().next() {
                let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> = {
                    let cfg_arc = config_arc.clone();
                    let alias = alias.clone();
                    Arc::new(move || cfg_arc.read().channel_external_peers("nostr", &alias))
                };
                configured_channels.push(ConfiguredChannel {
                    display_name: "Nostr",
                    alias: Some(alias.clone()),
                    channel: Arc::new(
                        NostrChannel::new(
                            &ns.private_key,
                            ns.relays.clone(),
                            alias.clone(),
                            peer_resolver,
                        )
                        .await?,
                    ),
                });
            }
            #[cfg(not(feature = "channel-nostr"))]
            if !config.channels.nostr.is_empty() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "Nostr channel is configured but this build was compiled without \
                     `channel-nostr`; skipping Nostr."
                );
            }
            let channels: Vec<Arc<dyn Channel>> = configured_channels
                .iter()
                .map(|cc| Arc::clone(&cc.channel))
                .collect();
            if channels.is_empty() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "No active channels to supervise (none configured or all disabled). \
                     Waiting for reload signal."
                );
                cancel.cancelled().await;
                return Ok(());
            }

            println!("🦀 ZeroClaw Channel Server");
            println!("  🤖 Model:    {model} (agent: {agent_alias})");
            let effective_backend = config.resolve_active_storage().kind();
            println!(
                "  🧠 Memory:   {} (auto-save: {})",
                effective_backend,
                if config.memory.auto_save { "on" } else { "off" }
            );
            let channel_labels: Vec<String> = configured_channels
                .iter()
                .map(|cc| composite_channel_key(cc.channel.name(), cc.alias.as_deref()))
                .collect();
            collected_channel_keys = channel_labels.clone();
            println!("  📡 Channels: {}", channel_labels.join(", "));
            println!("  🤖 Agents:   {}", enabled_agents.join(", "));
            println!();
            println!("  Listening for messages... (Ctrl+C to stop)");
            println!();

            zeroclaw_runtime::health::mark_component_ok("channels");

            let initial_backoff_secs = config
                .reliability
                .channel_initial_backoff_secs
                .max(DEFAULT_CHANNEL_INITIAL_BACKOFF_SECS);
            let max_backoff_secs = config
                .reliability
                .channel_max_backoff_secs
                .max(DEFAULT_CHANNEL_MAX_BACKOFF_SECS);

            let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(100);

            for cc in &configured_channels {
                listener_handles.push(spawn_supervised_listener(
                    cc.channel.clone(),
                    cc.alias.clone(),
                    tx.clone(),
                    initial_backoff_secs,
                    max_backoff_secs,
                    cancel.clone(),
                ));
            }
            drop(tx);

            // Composite-key registry (see `composite_channel_key`).
            let cbn = Arc::new({
                let mut map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
                let mut name_counts: HashMap<&str, usize> = HashMap::new();
                for cc in &configured_channels {
                    *name_counts.entry(cc.channel.name()).or_insert(0) += 1;
                }
                for cc in &configured_channels {
                    let name = cc.channel.name();
                    let composite = composite_channel_key(name, cc.alias.as_deref());
                    map.insert(composite, Arc::clone(&cc.channel));
                    if name_counts.get(name).copied().unwrap_or(0) == 1 {
                        map.entry(name.to_string())
                            .or_insert_with(|| Arc::clone(&cc.channel));
                    }
                }
                map
            });
            *CRON_CHANNEL_REGISTRY
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&cbn));

            let in_flight = max_in_flight_messages_for_config(channels.len(), &config.channels);
            println!("  🚦 In-flight message limit: {in_flight}");

            max_in_flight_messages = Some(in_flight);
            channels_by_name_shared = Some(cbn);
            rx_holder = Some(rx);
        }

        let channels_by_name = Arc::clone(
            channels_by_name_shared
                .as_ref()
                .expect("channels_by_name initialized on first iteration"),
        );

        // Wire this agent's reaction / ask_user / escalate tool handles
        // into the shared `channels_by_name` map.
        {
            let mut map = reaction_handle_ch.write();
            for (name, ch) in channels_by_name.as_ref() {
                map.insert(name.clone(), Arc::clone(ch));
            }
        }
        if let Some(ref handle) = ask_user_handle_ch {
            let mut map = handle.write();
            for (name, ch) in channels_by_name.as_ref() {
                map.insert(name.clone(), Arc::clone(ch));
            }
        }
        if let Some(ref handle) = poll_handle_ch {
            let mut map = handle.write();
            for (name, ch) in channels_by_name.as_ref() {
                map.insert(name.clone(), Arc::clone(ch));
            }
        }
        if let Some(ref handle) = escalate_handle_ch {
            let mut map = handle.write();
            for (name, ch) in channels_by_name.as_ref() {
                map.insert(name.clone(), Arc::clone(ch));
            }
        }

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(provider_name.clone(), Arc::clone(&model_provider));
        let message_timeout_secs =
            effective_channel_message_timeout_secs(config.channels.message_timeout_secs);
        let interrupt_on_new_message = interrupt_on_new_message_config(&config.channels);

        let memory_strategy: Arc<dyn MemoryStrategy> = Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::clone(&mem),
                config.memory.clone(),
                config.data_dir.clone(),
            ),
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::clone(&channels_by_name),
            model_provider: Arc::clone(&model_provider),
            model_provider_ref: Arc::new(provider_name.clone()),
            agent_alias: Arc::new(agent_alias.clone()),
            agent_cfg: Arc::new(agent.clone()),
            prompt_config: Arc::new(config.clone()),
            memory: Arc::clone(&mem),
            memory_strategy,
            tools_registry: Arc::clone(&tools_registry),
            observer: Arc::clone(&observer),
            system_prompt: Arc::new(system_prompt),
            model: Arc::new(model.clone()),
            temperature,
            auto_save_memory: config.memory.auto_save,
            max_tool_iterations: config.effective_max_tool_iterations(agent_alias.as_str()),
            min_relevance_score: config.memory.min_relevance_score,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(config.reliability.clone()),
            provider_runtime_options,
            // Use this agent's workspace (not the install-wide data dir): the
            // channel runtime context drives per-message skill reloads, prompt
            // refresh, and file-access scoping, all of which must resolve to the
            // same agent workspace that boot-time registration loads from.
            // Pointing at `config.data_dir` silently breaks per-message skill
            // activation (candidates load from `<data_dir>/skills`, which is
            // empty) and mis-scopes file tools.
            workspace_dir: Arc::new(workspace.clone()),
            message_timeout_secs,
            interrupt_on_new_message,
            multimodal: config.multimodal.clone(),
            media_pipeline: config.media_pipeline.clone(),
            transcription_config: config.transcription.clone(),
            agent_transcription_provider: agent.transcription_provider.as_str().to_string(),
            hooks: if config.hooks.enabled {
                let mut runner = zeroclaw_runtime::hooks::HookRunner::new();
                if config.hooks.builtin.command_logger {
                    runner.register(Box::new(
                        zeroclaw_runtime::hooks::builtin::CommandLoggerHook::new(),
                    ));
                }
                if config.hooks.builtin.webhook_audit.enabled {
                    runner.register(Box::new(
                        zeroclaw_runtime::hooks::builtin::WebhookAuditHook::new(
                            config.hooks.builtin.webhook_audit.clone(),
                        ),
                    ));
                }
                Some(Arc::new(runner))
            } else {
                None
            },
            non_cli_excluded_tools: Arc::new(risk_profile.excluded_tools.clone()),
            autonomy_level: risk_profile.level,
            tool_call_dedup_exempt: Arc::new(agent.resolved.tool_call_dedup_exempt.clone()),
            model_routes: Arc::new(config.model_routes.clone()),
            query_classification: config.query_classification.clone(),
            ack_reactions: config.channels.ack_reactions,
            show_tool_calls: config.channels.show_tool_calls,
            session_store: shared_session_store.clone(),
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(&risk_profile)),
            activated_tools: ch_activated_handle,
            cost_tracking: zeroclaw_runtime::cost::CostTracker::get_or_init_global(
                config.cost.clone(),
                &config.data_dir,
            )
            .map(|tracker| {
                // The cost tracker's lookup site (`record_tool_loop_cost_usage`
                // in zeroclaw-runtime) receives the bare provider type — the
                // composite alias isn't threaded through the agent loop. Build
                // the pricing map keyed by `<type>` and merge each alias's
                // `pricing` table into the type-level slot. Rates are per
                // (provider type, model); they don't differ between an
                // operator's `anthropic.work` and `anthropic.personal` keys.
                let mut by_type: std::collections::HashMap<
                    String,
                    std::collections::HashMap<String, f64>,
                > = std::collections::HashMap::new();
                for (type_k, _alias_k, profile) in config.providers.models.iter_entries() {
                    if profile.pricing.is_empty() {
                        continue;
                    }
                    let slot = by_type.entry(type_k.to_string()).or_default();
                    for (key, value) in &profile.pricing {
                        slot.insert(key.clone(), *value);
                    }
                }
                // Merge the `[cost.rates.providers.models.<type>.<model>]`
                // section. Keys land as `"<model>.input"` / `"<model>.output"`
                // / `"<model>.cached_input"` so the existing lookup
                // (`resolve_rates`) finds them with no further changes. The
                // rate sheet wins on conflict — it's the forward-looking
                // surface, the legacy per-alias `pricing` table is the
                // fallback for installs that haven't migrated.
                for (provider_type, model_id, rates) in
                    config.cost.rates.providers.models.iter_entries()
                {
                    let slot = by_type.entry(provider_type.to_string()).or_default();
                    if let Some(input) = rates.input_per_mtok {
                        slot.insert(format!("{model_id}.input"), input);
                    }
                    if let Some(output) = rates.output_per_mtok {
                        slot.insert(format!("{model_id}.output"), output);
                    }
                    if let Some(cached) = rates.cached_input_per_mtok {
                        slot.insert(format!("{model_id}.cached_input"), cached);
                    }
                }
                ChannelCostTrackingState {
                    tracker,
                    model_provider_pricing: Arc::new(by_type),
                    agent_alias: Arc::new(agent_alias.clone()),
                }
            }),
            pacing: config.pacing.clone(),
            max_tool_result_chars: agent.resolved.max_tool_result_chars,
            context_token_budget: agent.resolved.max_context_tokens,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::from_millis(config.channels.debounce_ms),
            )),
            receipt_generator: if agent.resolved.tool_receipts.enabled {
                Some(zeroclaw_runtime::agent::tool_receipts::ReceiptGenerator::new())
            } else {
                None
            },
            show_receipts_in_response: agent.resolved.tool_receipts.show_in_response,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        agent_ctxs.insert(agent_alias.clone(), runtime_ctx);
    }

    let owner_by_channel_key =
        build_owner_by_channel_key(&config, &enabled_agents, &collected_channel_keys);

    // Hydrate persisted session histories into the owning agent's
    // `conversation_histories` LRU. Sessions whose channel has no enabled
    // owner are skipped so their history doesn't end up loaded into the
    // fallback agent (which wouldn't reply on that channel anyway).
    if let Some(ref store) = shared_session_store {
        let mut metadata = store.list_sessions_with_metadata();
        metadata.sort_by_key(|m| std::cmp::Reverse(m.last_activity));
        // Budget proportional to the number of agents — each gets up to
        // `MAX_CONVERSATION_SENDERS` slots, so a multi-agent install
        // hydrates strictly more total sessions than a single-agent one.
        let cap = MAX_CONVERSATION_SENDERS.saturating_mul(enabled_agents.len().max(1));
        if metadata.len() > cap {
            metadata.truncate(cap);
        }

        let mut hydrated = 0usize;
        let mut orphans_closed = 0usize;
        for m in metadata {
            let owner_agent = m
                .channel_id
                .as_deref()
                .and_then(|cid| owner_by_channel_key.get(cid).cloned())
                .or_else(|| {
                    m.channel_id
                        .as_deref()
                        .and_then(|cid| cid.split_once('.').map(|(b, _)| b.to_string()))
                        .and_then(|b| owner_by_channel_key.get(&b).cloned())
                });
            let target_ctx = match owner_agent.as_ref().and_then(|a| agent_ctxs.get(a)) {
                Some(ctx) => ctx,
                None => continue,
            };
            let mut msgs = store.load(&m.key);
            if msgs.is_empty() {
                continue;
            }
            if msgs.len() > MAX_CHANNEL_HISTORY {
                msgs.drain(..msgs.len() - MAX_CHANNEL_HISTORY);
            }
            if msgs.last().is_some_and(|msg| msg.role == "user") {
                let closure =
                    ChatMessage::assistant("[Session interrupted — not continuing this request]");
                if let Err(e) = store.append(&m.key, &closure) {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        &format!("Failed to persist orphan closure for {}", m.key)
                    );
                }
                msgs.push(closure);
                orphans_closed += 1;
            }
            let pruned =
                zeroclaw_runtime::agent::history_pruner::remove_orphaned_tool_messages(&mut msgs);
            if !pruned.is_empty() {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"category": "agent", "agent_alias": owner_agent.as_deref().unwrap_or(""), "channel": m.channel_id.as_deref().unwrap_or(""), "session_key": m.key, "removed": pruned.removed, "orphan_tool_call_ids": pruned.orphan_tool_call_ids})), "removed orphaned tool messages from restored history (tool_use/tool_result pairing inconsistency auto-healed)");
            }

            let mut histories = target_ctx
                .conversation_histories
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            histories.push(m.key.clone(), msgs);
            drop(histories);
            hydrated += 1;
        }
        if hydrated > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"hydrated": hydrated})),
                "restored sessions from disk"
            );
        }
        if orphans_closed > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"orphans_closed": orphans_closed})),
                "closed orphaned session turns from previous crash"
            );
        }
    }

    let router = AgentRouter::multi(agent_ctxs, owner_by_channel_key);

    let rx = rx_holder.expect("rx initialized by first agent's channel setup");
    let max_in_flight =
        max_in_flight_messages.expect("max_in_flight initialized by first agent's channel setup");
    run_message_dispatch_loop(rx, router, max_in_flight).await;

    for h in listener_handles {
        let _ = h.await;
    }

    Ok(())
}

/// Deliver a cron job announcement to a configured channel.
/// Scans for credential leaks before delivery.
///
/// `thread_id` is forwarded to channels whose outbound `thread_id` is distinct
/// from the recipient (notably the webhook channel, which serialises both into
/// the JSON callback). For channels that do not honour `thread_ts` it is a
/// harmless no-op.
pub async fn deliver_announcement(
    config: &zeroclaw_config::schema::Config,
    channel: &str,
    target: &str,
    thread_id: Option<String>,
    output: &str,
) -> anyhow::Result<()> {
    use zeroclaw_api::channel::SendMessage;
    let _ = config;

    // Scan for credential leaks before delivering
    let leak_detector = zeroclaw_runtime::security::LeakDetector::new();
    let safe_output = match leak_detector.scan(output) {
        zeroclaw_runtime::security::LeakResult::Detected { redacted, .. } => redacted,
        zeroclaw_runtime::security::LeakResult::Clean => output.to_string(),
    };

    let make_msg = |s: &str| SendMessage::new(s, target).in_thread(thread_id.clone());

    // Snapshot out of the sync RwLock before awaiting. Use the live
    // channel instance when available — critical for Matrix E2EE which
    // must reuse the authenticated client rather than re-running session
    // restore per delivery.
    let registry_snapshot = CRON_CHANNEL_REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(registry) = registry_snapshot
        && let Some(ch) = registry.get(channel.to_ascii_lowercase().as_str())
    {
        return ch.send(&make_msg(&safe_output)).await;
    }

    let (raw_type, alias) = channel.split_once('.').ok_or_else(|| {
        anyhow::Error::msg(format!(
            "delivery channel {channel:?} must be a dotted <type>.<alias> ref (e.g. telegram.work)"
        ))
    })?;
    let channel_type = raw_type.to_ascii_lowercase();
    #[allow(unused_variables)]
    let not_configured = || {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            &format!("[channels.{channel_type}.{alias}] not configured")
        );
        anyhow::Error::msg(format!("[channels.{channel_type}.{alias}] not configured"))
    };
    match channel_type.as_str() {
        #[cfg(feature = "channel-telegram")]
        "telegram" => {
            let tg = config
                .channels
                .telegram
                .get(alias)
                .ok_or_else(not_configured)?;
            let peers = config.channel_external_peers("telegram", alias);
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
                Arc::new(move || peers.clone());
            let ch =
                TelegramChannel::new(tg.bot_token.clone(), alias, peer_resolver, tg.mention_only);
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-telegram"))]
        "telegram" => {
            anyhow::bail!("Telegram channel requires the `channel-telegram` feature");
        }
        #[cfg(feature = "channel-discord")]
        "discord" => {
            let dc = config
                .channels
                .discord
                .get(alias)
                .ok_or_else(not_configured)?;
            let peers = config.channel_external_peers("discord", alias);
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
                Arc::new(move || peers.clone());
            let ch = DiscordChannel::new(
                dc.bot_token.clone(),
                dc.guild_ids.clone(),
                alias,
                peer_resolver,
                dc.listen_to_bots,
                dc.mention_only,
            )
            .with_channel_ids(dc.channel_ids.clone())
            .with_workspace_dir(config.channel_workspace_dir(channel));
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-discord"))]
        "discord" => {
            anyhow::bail!("Discord channel requires the `channel-discord` feature");
        }
        #[cfg(feature = "channel-slack")]
        "slack" => {
            let sl = config
                .channels
                .slack
                .get(alias)
                .ok_or_else(not_configured)?;
            let peers = config.channel_external_peers("slack", alias);
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
                Arc::new(move || peers.clone());
            let ch = SlackChannel::new(
                sl.bot_token.clone(),
                sl.app_token.clone(),
                sl.channel_ids.clone(),
                alias,
                peer_resolver,
            )
            .with_workspace_dir(config.channel_workspace_dir(channel));
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-slack"))]
        "slack" => {
            anyhow::bail!("Slack channel requires the `channel-slack` feature");
        }
        #[cfg(feature = "channel-signal")]
        "signal" => {
            let sg = config
                .channels
                .signal
                .get(alias)
                .ok_or_else(not_configured)?;
            let peers = config.channel_external_peers("signal", alias);
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
                Arc::new(move || peers.clone());
            let ch = SignalChannel::new(
                sg.http_url.clone(),
                sg.account.clone(),
                sg.group_ids.clone(),
                sg.dm_only,
                alias,
                peer_resolver,
                sg.ignore_attachments,
                sg.ignore_stories,
            );
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-signal"))]
        "signal" => {
            anyhow::bail!("Signal channel requires the `channel-signal` feature");
        }
        #[cfg(feature = "channel-wechat")]
        "wechat" => {
            let wc = config
                .channels
                .wechat
                .get(alias)
                .ok_or_else(not_configured)?;
            let peers = config.channel_external_peers("wechat", alias);
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
                Arc::new(move || peers.clone());
            let ch = WeChatChannel::new(
                alias,
                peer_resolver,
                wc.api_base_url.clone(),
                wc.cdn_base_url.clone(),
                wc.state_dir.as_ref().map(std::path::PathBuf::from),
            )?
            .with_workspace_dir(config.channel_workspace_dir(channel));
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-wechat"))]
        "wechat" => {
            anyhow::bail!("WeChat channel requires the `channel-wechat` feature");
        }
        #[cfg(feature = "channel-lark")]
        "lark" | "feishu" => {
            // [channels.lark.<alias>] is the single source of truth for both
            // names (AGENTS.md). from_config selects the endpoint via
            // use_feishu. Error text names the real config table, not the
            // cron alias the user wrote.
            let lk = config.channels.lark.get(alias).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    &format!(
                        "[channels.lark.{alias}] not configured (cron channel \"{channel_type}.{alias}\")"
                    )
                );
                anyhow::Error::msg(format!(
                    "[channels.lark.{alias}] not configured (cron channel \"{channel_type}.{alias}\")"
                ))
            })?;
            // Asymmetric by design: "feishu"+use_feishu=false is a typo
            // (hard fail). "lark"+use_feishu=true is a soft compat path
            // (warn but still deliver via fallback construction).
            if channel_type == "feishu" && !lk.use_feishu {
                anyhow::bail!(
                    "[channels.lark.{alias}] has use_feishu=false but cron channel=\"feishu.{alias}\"; \
                     use channel=\"lark.{alias}\" or set use_feishu=true"
                );
            }
            if channel_type == "lark" && lk.use_feishu {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "cron channel=\"lark.{alias}\" with [channels.lark.{alias}] use_feishu=true \
                         falls back to one-shot channel construction; prefer channel=\"feishu.{alias}\" \
                         to reuse the live Feishu handle from start_channels"
                    )
                );
            }
            let peers = config.channel_external_peers("lark", alias);
            let peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync> =
                Arc::new(move || peers.clone());
            let ch = LarkChannel::from_config(lk, alias, peer_resolver)
                .with_approval_timeout_secs(lk.approval_timeout_secs)
                .with_per_user_session(lk.per_user_session)
                .with_streaming(lk.stream_mode, lk.draft_update_interval_ms);
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-lark"))]
        "lark" | "feishu" => {
            anyhow::bail!("Lark channel requires the `channel-lark` feature");
        }
        #[cfg(feature = "channel-webhook")]
        "webhook" => {
            let wh = config
                .channels
                .webhook
                .get(alias)
                .ok_or_else(not_configured)?;
            let ch = WebhookChannel::new(
                alias.to_string(),
                wh.port,
                wh.listen_path.clone(),
                wh.send_url.clone(),
                wh.send_method.clone(),
                wh.auth_header.clone(),
                wh.secret.clone(),
                wh.max_retries,
                wh.retry_base_delay_ms,
                wh.retry_max_delay_ms,
            );
            zeroclaw_api::channel::Channel::send(&ch, &make_msg(&safe_output)).await?;
        }
        #[cfg(not(feature = "channel-webhook"))]
        "webhook" => {
            anyhow::bail!("Webhook channel requires the `channel-webhook` feature");
        }
        "wecom_ws" | "wecom-ws" => {
            let _ = config
                .channels
                .wecom_ws
                .get(alias)
                .ok_or_else(not_configured)?;
            anyhow::bail!("wecom_ws channel is not connected");
        }
        other => anyhow::bail!("unsupported delivery channel: {other}"),
    }
    #[allow(unreachable_code)]
    Ok(())
}

#[cfg(feature = "channel-wechat")]
fn expand_tilde_in_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tempfile::TempDir;
    use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};
    use zeroclaw_providers::{ChatMessage, ModelProvider};
    use zeroclaw_runtime::agent::loop_::build_tool_instructions;

    #[test]
    fn no_real_time_channels_message_points_at_quickstart_not_onboard() {
        // The "no channels configured" message must point operators at the
        // current command (zeroclaw quickstart), not the deleted `zeroclaw onboard`.
        // Source of truth: the string at orchestrator/mod.rs:~7376.
        let msg = super::no_real_time_channels_message();
        assert!(
            !msg.contains("zeroclaw onboard"),
            "stale `zeroclaw onboard` reference in message: {msg}"
        );
        assert!(
            msg.contains("zeroclaw quickstart"),
            "expected `zeroclaw quickstart` reference, got: {msg}"
        );
    }

    #[tokio::test]
    async fn channel_runtime_reload_applies_env_overrides_after_migration() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
default_provider = "openrouter"

[model_providers.openrouter]
name = "openrouter"

[agents.demo]
provider = "openrouter"
model = "meta-llama/llama-3.1-8b-instruct"
temperature = 0.3
"#,
        )
        .unwrap();

        let env_name = "ZEROCLAW_providers__models__openrouter__agent_demo__api_key";
        // SAFETY: this test owns this specific env-var key and restores it
        // before returning. The value is synthetic and not a real credential.
        unsafe { std::env::set_var(env_name, "sk-or-v1-test-channel-reload") };

        let result = load_runtime_config_and_defaults(&config_path, "demo").await;

        // SAFETY: undo the test-only process env mutation above.
        unsafe { std::env::remove_var(env_name) };

        let (config, defaults) = result.unwrap();
        assert_eq!(
            defaults.api_key.as_deref(),
            Some("sk-or-v1-test-channel-reload")
        );
        assert!(
            config
                .env_overridden_paths
                .contains("providers.models.openrouter.agent_demo.api_key")
        );
    }

    use zeroclaw_runtime::observability::NoopObserver;
    use zeroclaw_runtime::tools::{Tool, ToolResult};

    fn make_workspace() -> TempDir {
        let tmp = TempDir::new().unwrap();
        // Create minimal workspace files
        std::fs::write(tmp.path().join("SOUL.md"), "# Soul\nBe helpful.").unwrap();
        std::fs::write(tmp.path().join("IDENTITY.md"), "# Identity\nName: ZeroClaw").unwrap();
        std::fs::write(tmp.path().join("USER.md"), "# User\nName: Test User").unwrap();
        std::fs::write(
            tmp.path().join("AGENTS.md"),
            "# Agents\nFollow instructions.",
        )
        .unwrap();
        std::fs::write(tmp.path().join("TOOLS.md"), "# Tools\nUse shell carefully.").unwrap();
        std::fs::write(
            tmp.path().join("HEARTBEAT.md"),
            "# Heartbeat\nCheck status.",
        )
        .unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "# Memory\nUser likes Rust.").unwrap();
        tmp
    }

    /// Minimal mock Channel returning a configurable `name()` so the
    /// channel-registry routing tests can simulate two aliases of the
    /// same channel type without pulling in real platform SDKs.
    /// Identity is checked via `Arc::ptr_eq`, not by inspecting fields.
    struct NamedMockChannel {
        name: &'static str,
    }

    impl ::zeroclaw_api::attribution::Attributable for NamedMockChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for NamedMockChannel {
        fn name(&self) -> &str {
            self.name
        }
        async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn mock_channel(name: &'static str) -> Arc<dyn Channel> {
        Arc::new(NamedMockChannel { name })
    }

    struct MentionMockChannel {
        name: &'static str,
        mention: &'static str,
    }

    impl ::zeroclaw_api::attribution::Attributable for MentionMockChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Discord,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for MentionMockChannel {
        fn name(&self) -> &str {
            self.name
        }
        fn self_addressed_mention(&self) -> Option<String> {
            Some(self.mention.to_string())
        }
        async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn mention_mock(name: &'static str, mention: &'static str) -> Arc<dyn Channel> {
        Arc::new(MentionMockChannel { name, mention })
    }

    fn channel_message(
        channel: &str,
        alias: Option<&str>,
    ) -> zeroclaw_api::channel::ChannelMessage {
        zeroclaw_api::channel::ChannelMessage {
            id: "m1".into(),
            sender: "u1".into(),
            reply_target: "r1".into(),
            content: "hi".into(),
            channel: channel.into(),
            channel_alias: alias.map(|s| s.to_string()),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        }
    }

    #[test]
    fn composite_channel_key_aliased_uses_dotted_form() {
        assert_eq!(
            composite_channel_key("discord", Some("clamps")),
            "discord.clamps"
        );
        assert_eq!(
            composite_channel_key("telegram", Some("default")),
            "telegram.default"
        );
    }

    #[test]
    fn composite_channel_key_unaliased_uses_bare_name() {
        assert_eq!(composite_channel_key("notion", None), "notion");
        // Empty-string alias collapses to bare name so we never produce a
        // `discord.` key that no message would ever match.
        assert_eq!(composite_channel_key("discord", Some("")), "discord");
    }

    #[test]
    fn find_channel_for_message_resolves_by_composite_key_for_multi_alias() {
        // Two Discord bots in the registry: only the composite key
        // distinguishes them. Without this, the second insertion silently
        // overwrites the first via `name()` collision — the bug that left
        // one Discord agent unresponsive on multi-bot configs.
        let clamps = mock_channel("discord");
        let glados = mock_channel("discord");
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("discord.clamps".to_string(), Arc::clone(&clamps));
        channels.insert("discord.glados".to_string(), Arc::clone(&glados));

        let msg_clamps = channel_message("discord", Some("clamps"));
        let msg_glados = channel_message("discord", Some("glados"));

        let resolved_clamps = find_channel_for_message(&channels, &msg_clamps).expect("clamps");
        let resolved_glados = find_channel_for_message(&channels, &msg_glados).expect("glados");

        assert!(Arc::ptr_eq(resolved_clamps, &clamps), "clamps lookup");
        assert!(Arc::ptr_eq(resolved_glados, &glados), "glados lookup");
        // Sanity: the two pointers are actually different.
        assert!(!Arc::ptr_eq(&clamps, &glados));
    }

    #[test]
    fn aliased_inbound_emits_per_alias_mention_in_prompt() {
        let clamps = mention_mock("discord", "<@111>");
        let glados = mention_mock("discord", "<@222>");
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("discord.clamps".into(), Arc::clone(&clamps));
        channels.insert("discord.glados".into(), Arc::clone(&glados));

        let msg_glados = channel_message("discord", Some("glados"));
        let target_glados = find_channel_for_message(&channels, &msg_glados).cloned();
        let prompt_glados =
            build_channel_system_prompt_for_message("Base.", &msg_glados, target_glados.as_ref());
        assert!(
            prompt_glados.contains("<@222>"),
            "glados prompt missing its own mention: {prompt_glados}"
        );
        assert!(
            !prompt_glados.contains("<@111>"),
            "glados prompt leaked the peer's mention: {prompt_glados}"
        );

        let msg_clamps = channel_message("discord", Some("clamps"));
        let target_clamps = find_channel_for_message(&channels, &msg_clamps).cloned();
        let prompt_clamps =
            build_channel_system_prompt_for_message("Base.", &msg_clamps, target_clamps.as_ref());
        assert!(
            prompt_clamps.contains("<@111>"),
            "clamps prompt missing its own mention: {prompt_clamps}"
        );
        assert!(
            !prompt_clamps.contains("<@222>"),
            "clamps prompt leaked the peer's mention: {prompt_clamps}"
        );
    }

    #[test]
    fn unaliased_inbound_with_no_self_handle_omits_mention_block() {
        let webhook = mock_channel("webhook");
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("webhook".into(), Arc::clone(&webhook));

        let msg = channel_message("webhook", None);
        let target = find_channel_for_message(&channels, &msg).cloned();
        let prompt = build_channel_system_prompt_for_message("Base.", &msg, target.as_ref());

        assert!(
            target.is_some(),
            "registry must resolve the webhook channel"
        );
        assert!(
            !prompt.contains("addressable handle on this channel"),
            "channels without self_addressed_mention must not emit the block: {prompt}"
        );
    }

    #[test]
    fn unresolved_channel_omits_mention_block() {
        let channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        let msg = channel_message("discord", Some("ghost"));
        let target = find_channel_for_message(&channels, &msg).cloned();
        let prompt = build_channel_system_prompt_for_message("Base.", &msg, target.as_ref());

        assert!(target.is_none());
        assert!(!prompt.contains("addressable handle on this channel"));
    }

    #[test]
    fn find_channel_for_message_falls_back_to_bare_name_when_no_alias_supplied() {
        // Legacy inbound (or singleton channel) with `channel_alias = None`
        // still resolves via the bare-name slot — the registry builder
        // populates it for single-alias platforms so cron callers and
        // outbound-only channels keep working.
        let webhook = mock_channel("webhook");
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("webhook".to_string(), Arc::clone(&webhook));

        let msg = channel_message("webhook", None);
        let resolved = find_channel_for_message(&channels, &msg).expect("webhook");
        assert!(Arc::ptr_eq(resolved, &webhook));
    }

    #[test]
    fn find_channel_for_message_falls_back_to_base_for_room_qualifier() {
        // Multi-room channels (Matrix) deliver inbound messages with
        // `channel = "matrix:!roomId"`. The registry key is bare `matrix`;
        // the helper splits on `:` and resolves the base channel.
        let matrix = mock_channel("matrix");
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("matrix".to_string(), Arc::clone(&matrix));

        let msg = channel_message("matrix:!room1:example.org", None);
        let resolved = find_channel_for_message(&channels, &msg).expect("matrix");
        assert!(Arc::ptr_eq(resolved, &matrix));
    }

    /// Build a minimal `ChannelRuntimeContext` suitable only for identity
    /// checks (`Arc::ptr_eq`). Every dependency is a no-op default — these
    /// ctxs aren't usable for actually running the dispatch loop.
    fn router_test_ctx() -> Arc<ChannelRuntimeContext> {
        Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new(String::new()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 0,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        })
    }

    #[tokio::test]
    async fn resolve_classifier_route_returns_none_for_empty_ref() {
        let ctx = router_test_ctx();
        let empty = zeroclaw_config::providers::ModelProviderRef::default();
        let result = resolve_classifier_route(
            ctx.as_ref(),
            &empty,
            &runtime_defaults_snapshot(ctx.as_ref()),
        )
        .await;
        assert!(result.is_none(), "empty ref must fall back to main agent");
    }

    #[tokio::test]
    async fn resolve_classifier_route_returns_none_for_unresolvable_ref() {
        let ctx = router_test_ctx();
        let bogus = zeroclaw_config::providers::ModelProviderRef::from("custom.does-not-exist");
        let result = resolve_classifier_route(
            ctx.as_ref(),
            &bogus,
            &runtime_defaults_snapshot(ctx.as_ref()),
        )
        .await;
        assert!(result.is_none(), "unresolvable ref must soft-fail to None");
    }

    #[tokio::test]
    async fn resolve_classifier_route_returns_alias_temperature() {
        // Build a config where `openai.my-classifier` has `temperature = 0.0`.
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.providers.models.openai.insert(
            "my-classifier".to_string(),
            zeroclaw_config::schema::OpenAIModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("gpt-4o-mini".to_string()),
                    temperature: Some(0.0),
                    ..Default::default()
                },
            },
        );

        let base_ctx = (*router_test_ctx()).clone();
        let ctx = Arc::new(ChannelRuntimeContext {
            prompt_config: Arc::new(cfg),
            ..base_ctx
        });

        let alias_ref = zeroclaw_config::providers::ModelProviderRef::from("openai.my-classifier");
        let result = resolve_classifier_route(
            ctx.as_ref(),
            &alias_ref,
            &runtime_defaults_snapshot(ctx.as_ref()),
        )
        .await;

        let (_, _, temp) = result.expect("must resolve to alias");
        assert_eq!(
            temp,
            Some(0.0),
            "alias temperature must be returned, not runtime_defaults.temperature"
        );
    }

    fn seed_sender_history(ctx: &ChannelRuntimeContext, sender: &str, turns: Vec<ChatMessage>) {
        let mut histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        histories.push(sender.to_string(), turns);
    }

    fn cloned_sender_history(ctx: &ChannelRuntimeContext, sender: &str) -> Vec<ChatMessage> {
        let histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        histories.peek(sender).cloned().unwrap_or_default()
    }

    fn history_signature(turns: &[ChatMessage]) -> Vec<(String, String)> {
        turns
            .iter()
            .map(|turn| (turn.role.clone(), turn.content.clone()))
            .collect()
    }

    #[test]
    fn agent_router_multi_routes_each_alias_to_its_owning_agent() {
        // Two enabled agents, each owning one Discord bot. A message tagged
        // with `channel_alias = "clamps"` must resolve to clamps' ctx; the
        // same channel name with `"glados"` must resolve to glados' ctx.
        // This is the exact behavior that was broken before per-agent ctxs:
        // both bots' inbound messages used to land in one shared agent's
        // pipeline and reply with that agent's identity/model.
        let clamps_ctx = router_test_ctx();
        let glados_ctx = router_test_ctx();
        let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
        by_agent.insert("clamps".to_string(), Arc::clone(&clamps_ctx));
        by_agent.insert("glados".to_string(), Arc::clone(&glados_ctx));
        let mut owners: HashMap<String, String> = HashMap::new();
        owners.insert("discord.clamps".to_string(), "clamps".to_string());
        owners.insert("discord.glados".to_string(), "glados".to_string());
        let router = AgentRouter::multi(by_agent, owners);

        let msg_clamps = channel_message("discord", Some("clamps"));
        let msg_glados = channel_message("discord", Some("glados"));

        let resolved_clamps = router.resolve(&msg_clamps).expect("clamps resolves");
        let resolved_glados = router.resolve(&msg_glados).expect("glados resolves");

        assert!(Arc::ptr_eq(&resolved_clamps, &clamps_ctx), "clamps routing");
        assert!(Arc::ptr_eq(&resolved_glados, &glados_ctx), "glados routing");
        assert!(
            !Arc::ptr_eq(&resolved_clamps, &resolved_glados),
            "ctxs distinct"
        );
    }

    #[test]
    fn agent_router_multi_returns_none_for_unowned_channels() {
        let agent_a_ctx = router_test_ctx();
        let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
        by_agent.insert("agent_a".to_string(), Arc::clone(&agent_a_ctx));
        let mut owners: HashMap<String, String> = HashMap::new();
        owners.insert("discord.bot_a".to_string(), "agent_a".to_string());
        let router = AgentRouter::multi(by_agent, owners);

        let cli_msg = channel_message("cli", None);
        assert!(router.resolve(&cli_msg).is_none(), "cli has no owner");
    }

    #[test]
    fn agent_router_multi_resolves_bare_channel_for_singleton_owners() {
        let notion_agent_ctx = router_test_ctx();
        let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
        by_agent.insert("ops".to_string(), Arc::clone(&notion_agent_ctx));
        let mut owners: HashMap<String, String> = HashMap::new();
        owners.insert("notion".to_string(), "ops".to_string());
        let router = AgentRouter::multi(by_agent, owners);

        let msg = channel_message("notion", None);
        let resolved = router.resolve(&msg).expect("notion resolves");
        assert!(Arc::ptr_eq(&resolved, &notion_agent_ctx));
    }

    #[test]
    fn agent_router_multi_resolves_fallback_loaded_channel_to_legacy_agent() {
        let mut config = Config::default();
        config.agents.clear();
        config.agents.insert(
            "legacy".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: true,
                channels: vec![],
                ..Default::default()
            },
        );
        let enabled_agents = vec!["legacy".to_string()];
        let collected_channel_keys = vec!["mattermost.default".to_string()];
        let owners = build_owner_by_channel_key(&config, &enabled_agents, &collected_channel_keys);

        let legacy_ctx = router_test_ctx();
        let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
        by_agent.insert("legacy".to_string(), Arc::clone(&legacy_ctx));
        let router = AgentRouter::multi(by_agent, owners);

        let msg = channel_message("mattermost", Some("default"));
        let resolved = router.resolve(&msg).expect("fallback owner resolves");
        assert!(Arc::ptr_eq(&resolved, &legacy_ctx));
    }

    #[test]
    fn build_owner_by_channel_key_legacy_fallback_is_deterministic_without_default() {
        let mut config = Config::default();
        config.agents.clear();
        config.agents.insert(
            "zeta".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: true,
                channels: vec![],
                ..Default::default()
            },
        );
        config.agents.insert(
            "alpha".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: true,
                channels: vec![],
                ..Default::default()
            },
        );

        let enabled_agents = vec!["alpha".to_string(), "zeta".to_string()];
        let collected_channel_keys = vec!["mattermost.default".to_string()];
        let owners = build_owner_by_channel_key(&config, &enabled_agents, &collected_channel_keys);

        assert_eq!(
            owners.get("mattermost.default").map(String::as_str),
            Some("alpha")
        );
        assert_eq!(owners.get("mattermost").map(String::as_str), Some("alpha"));
    }

    #[test]
    fn find_channel_for_message_returns_none_when_alias_unknown() {
        // A message tagged with an alias that isn't registered must not
        // accidentally fall through to a different bot's handle — silent
        // misrouting is exactly what the original collision bug caused.
        let clamps = mock_channel("discord");
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert("discord.clamps".to_string(), Arc::clone(&clamps));

        // No bare `discord` key and no `discord.ghost` key — lookup must fail.
        let msg = channel_message("discord", Some("ghost"));
        assert!(find_channel_for_message(&channels, &msg).is_none());
    }

    #[test]
    fn effective_channel_message_timeout_secs_clamps_to_minimum() {
        assert_eq!(
            effective_channel_message_timeout_secs(0),
            MIN_CHANNEL_MESSAGE_TIMEOUT_SECS
        );
        assert_eq!(
            effective_channel_message_timeout_secs(15),
            MIN_CHANNEL_MESSAGE_TIMEOUT_SECS
        );
        assert_eq!(effective_channel_message_timeout_secs(300), 300);
    }

    #[test]
    fn compute_max_in_flight_messages_uses_configured_per_channel_budget() {
        assert_eq!(compute_max_in_flight_messages(3, 4), 12);
        assert_eq!(compute_max_in_flight_messages(3, 8), 24);
    }

    #[test]
    fn max_in_flight_messages_for_config_uses_channel_budget() {
        let config = zeroclaw_config::schema::ChannelsConfig {
            max_concurrent_per_channel: 8,
            ..Default::default()
        };

        assert_eq!(max_in_flight_messages_for_config(3, &config), 24);
    }

    #[test]
    fn compute_max_in_flight_messages_preserves_global_bounds() {
        assert_eq!(
            compute_max_in_flight_messages(1, 1),
            CHANNEL_MIN_IN_FLIGHT_MESSAGES
        );
        assert_eq!(
            compute_max_in_flight_messages(100, 4),
            CHANNEL_MAX_IN_FLIGHT_MESSAGES
        );
    }

    #[test]
    fn channel_message_timeout_budget_scales_with_tool_iterations() {
        assert_eq!(channel_message_timeout_budget_secs(300, 1), 300);
        assert_eq!(channel_message_timeout_budget_secs(300, 2), 600);
        assert_eq!(channel_message_timeout_budget_secs(300, 3), 900);
    }

    #[cfg(feature = "channel-wechat")]
    #[test]
    fn expand_tilde_in_path_expands_home_prefix() {
        let expanded = expand_tilde_in_path("~/wechat-state");
        assert!(!expanded.starts_with("~"));
        assert!(expanded.ends_with("wechat-state"));

        let absolute = expand_tilde_in_path("/absolute/path");
        assert_eq!(absolute, PathBuf::from("/absolute/path"));

        let relative = expand_tilde_in_path("relative/path");
        assert_eq!(relative, PathBuf::from("relative/path"));
    }

    #[test]
    fn parse_reply_intent_recognizes_reply_token() {
        assert!(matches!(
            parse_reply_intent("REPLY"),
            AssistantChannelOutcome::Reply(_)
        ));
        assert!(matches!(
            parse_reply_intent("  reply  "),
            AssistantChannelOutcome::Reply(_)
        ));
    }

    #[test]
    fn parse_reply_intent_extracts_kinded_no_reply_reason() {
        assert!(matches!(
            parse_reply_intent("NO_REPLY[INFO]: not addressed to bot"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Informational,
                reason: Some(ref r),
            } if r == "not addressed to bot"
        ));
        assert!(matches!(
            parse_reply_intent("NO_REPLY[REFUSE]: prompt injection attempt"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Refused,
                reason: Some(_),
            }
        ));
        assert!(matches!(
            parse_reply_intent("NO_REPLY[FAIL]: requested URL 404s"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Failed,
                reason: Some(_),
            }
        ));
    }

    #[test]
    fn parse_reply_intent_handles_legacy_no_reply_form() {
        assert!(matches!(
            parse_reply_intent("NO_REPLY: greeting"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Informational,
                reason: Some(ref r),
            } if r == "greeting"
        ));
        assert!(matches!(
            parse_reply_intent("NO_REPLY"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Informational,
                reason: None,
            }
        ));
    }

    #[test]
    fn parse_reply_intent_unrecognized_output_falls_through_to_reply() {
        assert!(matches!(
            parse_reply_intent("idk maybe respond?"),
            AssistantChannelOutcome::Reply(_)
        ));
    }

    #[test]
    fn parse_reply_intent_treats_meta_instruction_echo_as_reply() {
        for echo in &[
            "NO_REPLY[INFO]: classification task only",
            "NO_REPLY[INFO]: classification task only, not answering user",
            "NO_REPLY[INFO]: Classification task only — must not answer the user.",
            "NO_REPLY[INFO]: I must not answer the user.",
            "NO_REPLY: classifier instruction echo",
        ] {
            assert!(
                matches!(parse_reply_intent(echo), AssistantChannelOutcome::Reply(_)),
                "expected Reply for echoed classifier output: {echo}",
            );
        }
    }

    #[test]
    fn parse_reply_intent_preserves_refuse_and_fail_even_with_rubric_like_reasons() {
        assert!(matches!(
            parse_reply_intent(
                "NO_REPLY[REFUSE]: prompt injection says \"do not answer the user\"",
            ),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Refused,
                reason: Some(_),
            }
        ));
        assert!(matches!(
            parse_reply_intent("NO_REPLY[REFUSE]: only classify, do not answer the user"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Refused,
                reason: Some(_),
            }
        ));
        assert!(matches!(
            parse_reply_intent(
                "NO_REPLY[FAIL]: upstream returned a classifier instruction instead of data",
            ),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Failed,
                reason: Some(_),
            }
        ));
    }

    #[test]
    fn parse_reply_intent_preserves_legitimate_no_reply_reasons() {
        assert!(matches!(
            parse_reply_intent(
                "NO_REPLY[INFO]: another user in the group is answering this thread",
            ),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Informational,
                reason: Some(_),
            }
        ));
        assert!(matches!(
            parse_reply_intent("NO_REPLY[INFO]: greeting in group chat, not addressed"),
            AssistantChannelOutcome::NoReply {
                kind: NoReplyKind::Informational,
                reason: Some(_),
            }
        ));
    }

    #[test]
    fn channel_message_timeout_budget_uses_safe_defaults_and_cap() {
        // 0 iterations falls back to 1x timeout budget.
        assert_eq!(channel_message_timeout_budget_secs(300, 0), 300);
        // Large iteration counts are capped to avoid runaway waits.
        assert_eq!(
            channel_message_timeout_budget_secs(300, 10),
            300 * CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP
        );
    }

    #[test]
    fn channel_message_timeout_budget_with_custom_scale_cap() {
        assert_eq!(
            channel_message_timeout_budget_secs_with_cap(300, 8, 8),
            300 * 8
        );
        assert_eq!(
            channel_message_timeout_budget_secs_with_cap(300, 20, 8),
            300 * 8
        );
        assert_eq!(
            channel_message_timeout_budget_secs_with_cap(300, 10, 1),
            300
        );
    }

    #[test]
    fn pacing_config_defaults_preserve_existing_behavior() {
        let pacing = zeroclaw_config::schema::PacingConfig::default();
        assert!(pacing.step_timeout_secs.is_none());
        assert!(pacing.loop_detection_min_elapsed_secs.is_none());
        assert!(pacing.loop_ignore_tools.is_empty());
        assert!(pacing.message_timeout_scale_max.is_none());
    }

    #[test]
    fn pacing_message_timeout_scale_max_overrides_default_cap() {
        // Custom cap of 8 scales budget proportionally
        assert_eq!(
            channel_message_timeout_budget_secs_with_cap(300, 10, 8),
            300 * 8
        );
        // Default cap produces the standard behavior
        assert_eq!(
            channel_message_timeout_budget_secs_with_cap(
                300,
                10,
                CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP
            ),
            300 * CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP
        );
    }

    #[test]
    fn context_window_overflow_error_detector_matches_known_messages() {
        let overflow_err = anyhow::Error::msg(
            "OpenAI Codex stream error: Your input exceeds the context window of this model.",
        );
        assert!(is_context_window_overflow_error(&overflow_err));

        let other_err =
            anyhow::Error::msg("OpenAI Codex API error (502 Bad Gateway): error code: 502");
        assert!(!is_context_window_overflow_error(&other_err));
    }

    #[test]
    fn memory_context_skip_rules_exclude_history_blobs() {
        assert!(should_skip_memory_context_entry(
            "telegram_123_history",
            r#"[{"role":"user"}]"#
        ));
        assert!(should_skip_memory_context_entry(
            "assistant_resp_legacy",
            "fabricated memory"
        ));
        assert!(!should_skip_memory_context_entry("telegram_123_45", "hi"));

        // Entries containing image markers must be skipped to prevent
        // auto-saved photo messages from duplicating image blocks.
        assert!(should_skip_memory_context_entry(
            "telegram_user_msg_99",
            "[IMAGE:/tmp/workspace/photo_1_2.jpg]"
        ));
        assert!(should_skip_memory_context_entry(
            "telegram_user_msg_100",
            "[IMAGE:/tmp/workspace/photo_1_2.jpg]\n\nCheck this screenshot"
        ));
        // Plain text without image markers should not be skipped.
        assert!(!should_skip_memory_context_entry(
            "telegram_user_msg_101",
            "Please describe the image"
        ));

        // Entries containing tool_result blocks must be skipped.
        assert!(should_skip_memory_context_entry(
            "telegram_user_msg_200",
            r#"[Tool results]
<tool_result name="shell">Mon Feb 20</tool_result>"#
        ));
        assert!(!should_skip_memory_context_entry(
            "telegram_user_msg_201",
            "plain text without tool results"
        ));

        // Per-turn user auto-save keys must be skipped to prevent exponential
        // context bloat from re-injected conversation history.
        assert!(should_skip_memory_context_entry(
            "user_msg",
            "original user message text"
        ));
        assert!(should_skip_memory_context_entry(
            "user_msg_a1b2c3d4e5f6",
            "follow-up message embedding prior context"
        ));
        // Channel-scoped keys (e.g. telegram_*) must NOT be affected.
        assert!(!should_skip_memory_context_entry(
            "telegram_user_msg_101",
            "Please describe the image"
        ));
    }

    fn channel_runtime_context_for_defaults_test(
        zeroclaw_dir: &std::path::Path,
        agent_alias: &str,
        default_model_provider: &str,
        model: &str,
    ) -> ChannelRuntimeContext {
        ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new(default_model_provider.to_string()),
            agent_alias: Arc::new(agent_alias.to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: default_model_provider.into(),
                ..Default::default()
            }),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    zeroclaw_dir.to_path_buf(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new(model.to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions {
                zeroclaw_dir: Some(zeroclaw_dir.to_path_buf()),
                ..Default::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn runtime_defaults_are_scoped_by_runtime_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agent_a = channel_runtime_context_for_defaults_test(
            tmp.path(),
            "agent_a",
            "openrouter.default",
            "startup-a",
        );
        let agent_b = channel_runtime_context_for_defaults_test(
            tmp.path(),
            "agent_b",
            "anthropic.default",
            "startup-b",
        );
        assert!(!runtime_defaults_snapshot(&agent_a).hot);
        assert!(!runtime_defaults_snapshot(&agent_b).hot);

        let hot_override = ChannelRuntimeOverride {
            config: Arc::new(zeroclaw_config::schema::Config::default()),
            defaults: ChannelRuntimeDefaults {
                default_model_provider: "openrouter.reloaded".to_string(),
                model: "hot-model".to_string(),
                temperature: Some(0.7),
                api_key: Some("hot-key".to_string()),
                api_url: Some("https://example.test/v1".to_string()),
                reliability: zeroclaw_config::schema::ReliabilityConfig::default(),
            },
            generation: 1,
        };
        *agent_a
            .runtime_defaults_override
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(hot_override));

        let route_a = default_route_selection_from_snapshot(&runtime_defaults_snapshot(&agent_a));
        assert_eq!(route_a.model_provider, "openrouter.reloaded");
        assert_eq!(route_a.model, "hot-model");
        let snapshot_a = runtime_defaults_snapshot(&agent_a);
        assert!(snapshot_a.hot);
        assert_eq!(snapshot_a.generation, 1);

        let route_b = default_route_selection_from_snapshot(&runtime_defaults_snapshot(&agent_b));
        assert_eq!(route_b.model_provider, "anthropic.default");
        assert_eq!(route_b.model, "startup-b");
        assert!(!runtime_defaults_snapshot(&agent_b).hot);
    }

    #[tokio::test]
    async fn load_runtime_config_uses_resolved_agent_provider() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        tokio::fs::write(
            &config_path,
            r#"
schema_version = 3

[agents.agent_a]
model_provider = "openrouter.hot"

[agents.agent_b]
model_provider = "anthropic.default"

[providers.models.openrouter.hot]
model = "hot-model"
api_key = "hot-key"
uri = "https://hot.example.test/v1"
temperature = 0.2

[providers.models.anthropic.default]
model = "cold-model"
api_key = "cold-key"
"#,
        )
        .await
        .unwrap();

        let (_config, defaults) = load_runtime_config_and_defaults(&config_path, "agent_a")
            .await
            .unwrap();

        assert_eq!(defaults.default_model_provider, "openrouter.hot");
        assert_eq!(defaults.model, "hot-model");
        assert_eq!(defaults.api_key.as_deref(), Some("hot-key"));
        assert_eq!(
            defaults.api_url.as_deref(),
            Some("https://hot.example.test/v1")
        );
        assert_eq!(defaults.temperature, Some(0.2));
    }

    #[tokio::test]
    async fn load_runtime_config_rejects_unresolved_agent_provider() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        tokio::fs::write(
            &config_path,
            r#"
[agents.agent_a]
model_provider = "openrouter.missing"

[providers.models.anthropic.default]
model = "cold-model"
api_key = "cold-key"
"#,
        )
        .await
        .unwrap();

        let err = load_runtime_config_and_defaults(&config_path, "agent_a")
            .await
            .expect_err("unresolved agent provider should reject reload");

        assert!(
            err.to_string()
                .contains("model_provider `openrouter.missing` does not resolve")
        );
    }

    #[tokio::test]
    async fn load_runtime_config_rejects_missing_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        tokio::fs::write(
            &config_path,
            r#"
[agents.agent_b]
model_provider = "anthropic.default"

[providers.models.anthropic.default]
model = "cold-model"
api_key = "cold-key"
"#,
        )
        .await
        .unwrap();

        let err = load_runtime_config_and_defaults(&config_path, "agent_a")
            .await
            .expect_err("runtime reload should reject a config missing the active agent");

        assert!(err.to_string().contains("agents.agent_a is not configured"));
    }

    #[tokio::test]
    async fn load_runtime_config_rejects_empty_agent_provider() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        tokio::fs::write(
            &config_path,
            r#"
[agents.agent_a]
model_provider = ""

[providers.models.anthropic.default]
model = "first-model"
api_key = "first-key"

[providers.models.openrouter.default]
model = "second-model"
api_key = "second-key"
"#,
        )
        .await
        .unwrap();

        let err = load_runtime_config_and_defaults(&config_path, "agent_a")
            .await
            .expect_err("empty agent provider should reject reload");

        assert!(err.to_string().contains("model_provider is empty"));
    }

    #[test]
    fn provider_credentials_use_target_alias_key_after_reload() {
        let config: Config = toml::from_str(
            r#"
[providers.models.openrouter.default]
model = "openrouter-model"
api_key = "openrouter-key"
uri = "https://openrouter.example.test/v1"

[providers.models.anthropic.default]
model = "anthropic-model"
api_key = "anthropic-key"
uri = "https://anthropic.example.test/v1"
"#,
        )
        .unwrap();
        let (api_key, api_url) = provider_credentials_for_ref(&config, "anthropic.default");

        assert_eq!(api_key.as_deref(), Some("anthropic-key"));
        assert_eq!(
            api_url.as_deref(),
            Some("https://anthropic.example.test/v1")
        );
    }

    #[test]
    fn provider_credentials_do_not_fall_back_to_default_alias() {
        let config: Config = toml::from_str(
            r#"
[providers.models.openrouter.default]
model = "openrouter-model"
api_key = "openrouter-key"

[providers.models.anthropic.default]
model = "anthropic-model"
api_key = "anthropic-key"
"#,
        )
        .unwrap();

        let (api_key, api_url) = provider_credentials_for_ref(&config, "anthropic");

        assert_eq!(api_key, None);
        assert_eq!(api_url, None);
    }

    #[test]
    fn provider_cache_key_isolates_hot_generations() {
        let startup = provider_cache_key("openrouter.default", None, 0);
        let hot_1 = provider_cache_key("openrouter.default", None, 1);
        let hot_2 = provider_cache_key("openrouter.default", None, 2);

        assert_eq!(startup, "openrouter.default");
        assert_ne!(hot_1, startup);
        assert_ne!(hot_1, hot_2);
    }

    #[test]
    fn strip_tool_result_content_removes_blocks_and_header() {
        let input = r#"[Tool results]
<tool_result name="shell">Mon Feb 20</tool_result>
<tool_result name="http_request">{"status":200}</tool_result>"#;
        assert_eq!(strip_tool_result_content(input), "");

        let mixed = "Some context\n<tool_result name=\"shell\">ok</tool_result>\nMore text";
        let cleaned = strip_tool_result_content(mixed);
        assert!(cleaned.contains("Some context"));
        assert!(cleaned.contains("More text"));
        assert!(!cleaned.contains("tool_result"));

        assert_eq!(
            strip_tool_result_content("no tool results here"),
            "no tool results here"
        );
        assert_eq!(strip_tool_result_content(""), "");
    }

    #[test]
    fn strip_tool_summary_prefix_removes_prefix_and_preserves_content() {
        let input = "[Used tools: browser_open, shell]\nI opened the page successfully.";
        assert_eq!(
            strip_tool_summary_prefix(input),
            "I opened the page successfully."
        );
    }

    #[test]
    fn strip_tool_summary_prefix_returns_empty_when_only_prefix() {
        let input = "[Used tools: browser_open]";
        assert_eq!(strip_tool_summary_prefix(input), "");
    }

    #[test]
    fn strip_tool_summary_prefix_preserves_text_without_prefix() {
        let input = "Here is the result of the search.";
        assert_eq!(strip_tool_summary_prefix(input), input);
    }

    #[test]
    fn strip_tool_summary_prefix_handles_multiple_newlines() {
        let input = "[Used tools: shell]\n\nThe command output is 42.";
        assert_eq!(
            strip_tool_summary_prefix(input),
            "The command output is 42."
        );
    }

    #[test]
    fn ensure_nonempty_channel_reply_substitutes_fallback_when_empty() {
        let result = ensure_nonempty_channel_reply(
            String::new(),
            "   ",
            "whatsapp",
            "15551234567@s.whatsapp.net",
        );
        assert_eq!(result, EMPTY_CHANNEL_REPLY_FALLBACK);
    }

    #[test]
    fn ensure_nonempty_channel_reply_preserves_nonempty_text() {
        let result = ensure_nonempty_channel_reply(
            "Hello".to_string(),
            "Hello",
            "whatsapp",
            "15551234567@s.whatsapp.net",
        );
        assert_eq!(result, "Hello");
    }

    #[test]
    fn sanitize_channel_response_strips_used_tools_with_leading_whitespace() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        //: response with leading whitespace before [Used tools: ...]
        let input = "  [Used tools: web_search_tool]\nHere is the search result.";

        let result = sanitize_channel_response(input, &tools);

        assert!(!result.contains("[Used tools:"));
        assert!(result.contains("Here is the search result."));
    }

    #[test]
    fn normalize_cached_channel_turns_merges_consecutive_user_turns() {
        let turns = vec![
            ChatMessage::user("forwarded content"),
            ChatMessage::user("summarize this"),
        ];

        let normalized = normalize_cached_channel_turns(turns);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].role, "user");
        assert!(normalized[0].content.contains("forwarded content"));
        assert!(normalized[0].content.contains("summarize this"));
    }

    #[test]
    fn normalize_cached_channel_turns_merges_consecutive_assistant_turns() {
        let turns = vec![
            ChatMessage::user("first user"),
            ChatMessage::assistant("assistant part 1"),
            ChatMessage::assistant("assistant part 2"),
            ChatMessage::user("next user"),
        ];

        let normalized = normalize_cached_channel_turns(turns);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].role, "user");
        assert_eq!(normalized[1].role, "assistant");
        assert_eq!(normalized[2].role, "user");
        assert!(normalized[1].content.contains("assistant part 1"));
        assert!(normalized[1].content.contains("assistant part 2"));
    }

    /// Verify that an orphan user turn followed by a failure-marker assistant
    /// turn normalizes correctly, so the LLM sees the failed request as closed
    /// and does not re-execute it on the next user message.
    #[test]
    fn normalize_preserves_failure_marker_after_orphan_user_turn() {
        let turns = vec![
            ChatMessage::user("download something from GitHub"),
            ChatMessage::assistant("[Task failed — not continuing this request]"),
            ChatMessage::user("what is WAL?"),
        ];

        let normalized = normalize_cached_channel_turns(turns);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].role, "user");
        assert_eq!(normalized[1].role, "assistant");
        assert!(normalized[1].content.contains("Task failed"));
        assert_eq!(normalized[2].role, "user");
        assert_eq!(normalized[2].content, "what is WAL?");
    }

    /// Same as above but for the timeout variant.
    #[test]
    fn normalize_preserves_timeout_marker_after_orphan_user_turn() {
        let turns = vec![
            ChatMessage::user("run a long task"),
            ChatMessage::assistant("[Task timed out — not continuing this request]"),
            ChatMessage::user("next question"),
        ];

        let normalized = normalize_cached_channel_turns(turns);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[1].role, "assistant");
        assert!(normalized[1].content.contains("Task timed out"));
        assert_eq!(normalized[2].content, "next question");
    }

    #[test]
    fn compact_sender_history_keeps_recent_truncated_messages() {
        let mut histories =
            lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
        let sender = "telegram_u1".to_string();
        histories.push(
            sender.clone(),
            (0..20)
                .map(|idx| {
                    let content = format!("msg-{idx}-{}", "x".repeat(700));
                    if idx % 2 == 0 {
                        ChatMessage::user(content)
                    } else {
                        ChatMessage::assistant(content)
                    }
                })
                .collect::<Vec<_>>(),
        );

        let ctx = ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        };

        assert!(compact_sender_history(&ctx, &sender));

        let locked_histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let kept = locked_histories
            .peek(&sender)
            .expect("sender history should remain");
        assert_eq!(kept.len(), CHANNEL_HISTORY_COMPACT_KEEP_MESSAGES);
        assert!(kept.iter().all(|turn| {
            let len = turn.content.chars().count();
            len <= CHANNEL_HISTORY_COMPACT_CONTENT_CHARS
                || (len <= CHANNEL_HISTORY_COMPACT_CONTENT_CHARS + 3
                    && turn.content.ends_with("..."))
        }));
    }

    #[test]
    fn proactive_trim_drops_oldest_turns_when_over_budget() {
        // Each message is 100 chars; 10 messages = 1000 chars total.
        let mut turns: Vec<ChatMessage> = (0..10)
            .map(|i| {
                let content = format!("m{i}-{}", "a".repeat(96));
                if i % 2 == 0 {
                    ChatMessage::user(content)
                } else {
                    ChatMessage::assistant(content)
                }
            })
            .collect();

        // Budget of 500 should drop roughly half (oldest turns).
        let dropped = proactive_trim_turns(&mut turns, 500);
        assert!(dropped > 0, "should have dropped some turns");
        assert!(turns.len() < 10, "should have fewer turns after trimming");
        // Last turn should always be preserved.
        assert!(
            turns.last().unwrap().content.starts_with("m9-"),
            "most recent turn must be preserved"
        );
        // Total chars should now be within budget.
        let total: usize = turns.iter().map(|t| t.content.chars().count()).sum();
        assert!(total <= 500, "total chars {total} should be within budget");
    }

    #[test]
    fn proactive_trim_noop_when_within_budget() {
        let mut turns = vec![
            ChatMessage::user("hello".to_string()),
            ChatMessage::assistant("hi there".to_string()),
        ];
        let dropped = proactive_trim_turns(&mut turns, 10_000);
        assert_eq!(dropped, 0);
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn proactive_trim_preserves_last_turn_even_when_over_budget() {
        let mut turns = vec![ChatMessage::user("x".repeat(2000))];
        let dropped = proactive_trim_turns(&mut turns, 100);
        assert_eq!(dropped, 0, "single turn must never be dropped");
        assert_eq!(turns.len(), 1);
    }

    #[test]
    fn append_sender_turn_stores_single_turn_per_call() {
        let sender = "telegram_u2".to_string();
        let ctx = ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        };

        append_sender_turn(&ctx, &sender, ChatMessage::user("hello"));

        let histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .peek(&sender)
            .expect("sender history should exist");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "hello");
    }

    #[test]
    fn timestamp_channel_user_content_adds_wall_clock_prefix() {
        let stamped = timestamp_channel_user_content("hello");

        assert!(
            stamped.starts_with('['),
            "timestamped content should start with a bracketed timestamp: {stamped}"
        );
        assert!(
            stamped.contains("] hello"),
            "timestamped content should preserve the user message after the timestamp: {stamped}"
        );
    }

    #[test]
    fn rollback_orphan_user_turn_removes_only_latest_matching_user_turn() {
        let sender = "telegram_u3".to_string();
        let mut histories =
            lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
        histories.push(
            sender.clone(),
            vec![
                ChatMessage::user("first"),
                ChatMessage::assistant("ok"),
                ChatMessage::user("pending"),
            ],
        );
        let ctx = ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        };

        assert!(rollback_orphan_user_turn(&ctx, &sender, "pending"));

        let locked_histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = locked_histories
            .peek(&sender)
            .expect("sender history should remain");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].content, "first");
        assert_eq!(turns[1].content, "ok");
    }

    #[test]
    fn rollback_orphan_user_turn_also_removes_from_session_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store: Arc<dyn zeroclaw_infra::session_backend::SessionBackend> =
            Arc::new(zeroclaw_infra::session_store::SessionStore::new(tmp.path()).unwrap());

        let sender = "telegram_u4".to_string();

        // Pre-populate the session store with the same turns.
        store.append(&sender, &ChatMessage::user("first")).unwrap();
        store
            .append(&sender, &ChatMessage::assistant("ok"))
            .unwrap();
        store
            .append(
                &sender,
                &ChatMessage::user("[IMAGE:/tmp/photo.jpg]\n\nDescribe this"),
            )
            .unwrap();

        let mut histories =
            lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
        histories.push(
            sender.clone(),
            vec![
                ChatMessage::user("first"),
                ChatMessage::assistant("ok"),
                ChatMessage::user("[IMAGE:/tmp/photo.jpg]\n\nDescribe this"),
            ],
        );

        let ctx = ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: Some(Arc::clone(&store)),
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        };

        assert!(rollback_orphan_user_turn(
            &ctx,
            &sender,
            "[IMAGE:/tmp/photo.jpg]\n\nDescribe this"
        ));

        // In-memory history should have 2 turns remaining.
        let locked = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = locked.peek(&sender).expect("history should remain");
        assert_eq!(turns.len(), 2);

        // Session store should also have only 2 entries.
        let persisted = store.load(&sender);
        assert_eq!(
            persisted.len(),
            2,
            "session store should also lose the rolled-back turn"
        );
        assert_eq!(persisted[0].content, "first");
        assert_eq!(persisted[1].content, "ok");
    }

    struct DummyModelProvider;

    #[async_trait::async_trait]
    impl ModelProvider for DummyModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for DummyModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "DummyModelProvider"
        }
    }

    struct FormatErrorModelProvider;

    #[async_trait::async_trait]
    impl ModelProvider for FormatErrorModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            if messages
                .iter()
                .any(|msg| msg.content.contains("trigger format error"))
            {
                anyhow::bail!(
                    "All model_providers/models failed. Attempts:\nprovider=custom:https://example.invalid/v1 model=test-model attempt 1/3: non_retryable; error=Custom API error (400 Bad Request): {{\"error\":{{\"message\":\"Format Error\",\"type\":\"invalid_request_error\",\"param\":null,\"code\":\"400\"}},\"request_id\":\"test-request-id\"}}"
                );
            }

            Ok("ok".to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for FormatErrorModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "FormatErrorModelProvider"
        }
    }

    #[derive(Default)]
    struct RecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
        start_typing_calls: AtomicUsize,
        stop_typing_calls: AtomicUsize,
        reactions_added: tokio::sync::Mutex<Vec<(String, String, String)>>,
        reactions_removed: tokio::sync::Mutex<Vec<(String, String, String)>>,
    }

    #[derive(Default)]
    struct TelegramRecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
    }

    #[derive(Default)]
    struct SlackRecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
    }

    #[derive(Default)]
    struct WhatsAppRecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
    }

    impl ::zeroclaw_api::attribution::Attributable for TelegramRecordingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for TelegramRecordingChannel {
        fn name(&self) -> &str {
            "telegram"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent_messages
                .lock()
                .await
                .push(format!("{}:{}", message.recipient, message.content));
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for SlackRecordingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for SlackRecordingChannel {
        fn name(&self) -> &str {
            "slack"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent_messages
                .lock()
                .await
                .push(format!("{}:{}", message.recipient, message.content));
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for WhatsAppRecordingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for WhatsAppRecordingChannel {
        fn name(&self) -> &str {
            "whatsapp"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent_messages
                .lock()
                .await
                .push(format!("{}:{}", message.recipient, message.content));
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for RecordingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &str {
            "test-channel"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent_messages
                .lock()
                .await
                .push(format!("{}:{}", message.recipient, message.content));
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            self.start_typing_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
            self.stop_typing_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn add_reaction(
            &self,
            channel_id: &str,
            message_id: &str,
            emoji: &str,
        ) -> anyhow::Result<()> {
            self.reactions_added.lock().await.push((
                channel_id.to_string(),
                message_id.to_string(),
                emoji.to_string(),
            ));
            Ok(())
        }

        async fn remove_reaction(
            &self,
            channel_id: &str,
            message_id: &str,
            emoji: &str,
        ) -> anyhow::Result<()> {
            self.reactions_removed.lock().await.push((
                channel_id.to_string(),
                message_id.to_string(),
                emoji.to_string(),
            ));
            Ok(())
        }
    }

    fn test_runtime_ctx_with_config_agent_and_provider_ref(
        channel: Arc<dyn Channel>,
        model_provider: Arc<dyn ModelProvider>,
        prompt_config: zeroclaw_config::schema::Config,
        agent_cfg: zeroclaw_config::schema::AliasedAgentConfig,
        model_provider_ref: &str,
    ) -> Arc<ChannelRuntimeContext> {
        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider,
            model_provider_ref: Arc::new(model_provider_ref.to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(agent_cfg),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("You are a helpful assistant.".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(prompt_config),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        })
    }

    struct SlowModelProvider {
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl ModelProvider for SlowModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            tokio::time::sleep(self.delay).await;
            Ok(format!("echo: {message}"))
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for SlowModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "SlowModelProvider"
        }
    }

    struct ToolCallingModelProvider;

    fn tool_call_payload() -> String {
        r#"<tool_call>
{"name":"mock_price","arguments":{"symbol":"BTC"}}
</tool_call>"#
            .to_string()
    }

    fn tool_call_payload_with_alias_tag() -> String {
        r#"<toolcall>
{"name":"mock_price","arguments":{"symbol":"BTC"}}
</toolcall>"#
            .to_string()
    }

    #[async_trait::async_trait]
    impl ModelProvider for ToolCallingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(tool_call_payload())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let has_tool_results = messages
                .iter()
                .any(|msg| msg.role == "user" && msg.content.contains("[Tool results]"));
            if has_tool_results {
                Ok("BTC is currently around $65,000 based on latest tool output.".to_string())
            } else {
                Ok(tool_call_payload())
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ToolCallingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ToolCallingModelProvider"
        }
    }

    struct SessionsCurrentModelProvider;

    #[async_trait::async_trait]
    impl ModelProvider for SessionsCurrentModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(r#"<tool_call>
{"name":"sessions_current","arguments":{}}
</tool_call>"#
                .to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            if let Some(tool_results) = messages
                .iter()
                .find(|msg| msg.role == "user" && msg.content.contains("[Tool results]"))
            {
                if tool_results
                    .content
                    .contains("Current session: test-channel_chat-42_alice")
                    && tool_results.content.contains("Messages: 1")
                {
                    return Ok(
                        "Current session: test-channel_chat-42_alice\nMessages: 1".to_string()
                    );
                }

                Ok("session result unavailable".to_string())
            } else {
                self.chat_with_system(None, "", "", None).await
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for SessionsCurrentModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "SessionsCurrentModelProvider"
        }
    }

    struct ToolCallingAliasModelProvider;

    #[async_trait::async_trait]
    impl ModelProvider for ToolCallingAliasModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(tool_call_payload_with_alias_tag())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let has_tool_results = messages
                .iter()
                .any(|msg| msg.role == "user" && msg.content.contains("[Tool results]"));
            if has_tool_results {
                Ok("BTC alias-tag flow resolved to final text output.".to_string())
            } else {
                Ok(tool_call_payload_with_alias_tag())
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ToolCallingAliasModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ToolCallingAliasModelProvider"
        }
    }

    struct RawToolArtifactModelProvider;

    #[async_trait::async_trait]
    impl ModelProvider for RawToolArtifactModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(r#"{"name":"mock_price","parameters":{"symbol":"BTC"}}
{"result":{"symbol":"BTC","price_usd":65000}}
BTC is currently around $65,000 based on latest tool output."#
                .to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for RawToolArtifactModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "RawToolArtifactModelProvider"
        }
    }

    struct IterativeToolModelProvider {
        required_tool_iterations: usize,
    }

    impl IterativeToolModelProvider {
        fn completed_tool_iterations(messages: &[ChatMessage]) -> usize {
            messages
                .iter()
                .filter(|msg| msg.role == "user" && msg.content.contains("[Tool results]"))
                .count()
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for IterativeToolModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(tool_call_payload())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let completed_iterations = Self::completed_tool_iterations(messages);
            if completed_iterations >= self.required_tool_iterations {
                Ok(format!(
                    "Completed after {completed_iterations} tool iterations."
                ))
            } else {
                Ok(tool_call_payload())
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for IterativeToolModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "IterativeToolModelProvider"
        }
    }

    #[derive(Default)]
    struct HistoryCaptureModelProvider {
        calls: std::sync::Mutex<Vec<Vec<(String, String)>>>,
        vision: bool,
    }

    #[async_trait::async_trait]
    impl ModelProvider for HistoryCaptureModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let snapshot = messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect::<Vec<_>>();
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            calls.push(snapshot);
            Ok(format!("response-{}", calls.len()))
        }

        fn supports_vision(&self) -> bool {
            self.vision
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for HistoryCaptureModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "HistoryCaptureModelProvider"
        }
    }

    struct DelayedHistoryCaptureModelProvider {
        delay: Duration,
        calls: std::sync::Mutex<Vec<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for DelayedHistoryCaptureModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let snapshot = messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect::<Vec<_>>();
            let call_index = {
                let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
                calls.push(snapshot);
                calls.len()
            };
            tokio::time::sleep(self.delay).await;
            Ok(format!("response-{call_index}"))
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for DelayedHistoryCaptureModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "DelayedHistoryCaptureModelProvider"
        }
    }

    struct MockPriceTool;

    impl ::zeroclaw_api::attribution::Attributable for MockPriceTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            <Self as ::zeroclaw_api::tool::Tool>::name(self)
        }
    }

    #[derive(Default)]
    struct ModelCaptureModelProvider {
        call_count: AtomicUsize,
        models: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for ModelCaptureModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(model.to_string());
            Ok("ok".to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ModelCaptureModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ModelCaptureModelProvider"
        }
    }

    #[derive(Default)]
    struct PrecheckProbeModelProvider {
        precheck_calls: AtomicUsize,
        main_calls: AtomicUsize,
        models: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for PrecheckProbeModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(model.to_string());

            if message.starts_with("Decide whether the assistant should send any visible reply") {
                self.precheck_calls.fetch_add(1, Ordering::SeqCst);
                return Ok("NO_REPLY[INFO]: background chatter".to_string());
            }

            self.main_calls.fetch_add(1, Ordering::SeqCst);
            Ok("visible reply".to_string())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for PrecheckProbeModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "PrecheckProbeModelProvider"
        }
    }

    #[async_trait::async_trait]
    impl Tool for MockPriceTool {
        fn name(&self) -> &str {
            "mock_price"
        }

        fn description(&self) -> &str {
            "Return a mocked BTC price"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" }
                },
                "required": ["symbol"]
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let symbol = args.get("symbol").and_then(serde_json::Value::as_str);
            if symbol != Some("BTC") {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("unexpected symbol".to_string()),
                });
            }

            Ok(ToolResult {
                success: true,
                output: r#"{"symbol":"BTC","price_usd":65000}"#.to_string(),
                error: None,
            })
        }
    }

    /// Minimal fixed-name tool for allowlist-filter coverage.
    struct NamedMockTool(&'static str);

    impl ::zeroclaw_api::attribution::Attributable for NamedMockTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            self.0
        }
    }

    #[async_trait::async_trait]
    impl Tool for NamedMockTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "named mock"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    /// `start_channels` must apply the agent's `allowed_tools` allowlist to the
    /// eager built-in registry before MCP/skill registration, at parity with
    /// `agent::run` / `process_message` / the `from_config` path. Before this
    /// gate the channel path skipped `apply_policy_tool_filter`, so an agent
    /// allowlisted to `file_read` still emitted raw `shell` / `file_write` in
    /// its native tool specs to the model.
    #[test]
    fn channel_path_allowlist_drops_non_allowlisted_builtins() {
        let mut built_tools: Vec<Box<dyn Tool>> = vec![
            Box::new(NamedMockTool("shell")),
            Box::new(NamedMockTool("file_write")),
            Box::new(NamedMockTool("file_read")),
        ];
        let policy = SecurityPolicy {
            allowed_tools: Some(vec!["file_read".to_string()]),
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        };
        apply_policy_tool_filter(&mut built_tools, Some(&policy), None);
        let names: Vec<&str> = built_tools.iter().map(|t| t.name()).collect();
        assert!(
            !names.contains(&"shell") && !names.contains(&"file_write"),
            "raw built-ins outside the allowlist must be dropped on the channel path; got {names:?}"
        );
        assert!(
            names.contains(&"file_read"),
            "allowlisted tool must survive the filter; got {names:?}"
        );
    }

    #[tokio::test]
    async fn process_channel_message_executes_tool_calls_instead_of_sending_raw_json() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ToolCallingModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-42".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(!sent_messages.is_empty());
        let reply = sent_messages.last().unwrap();
        assert!(reply.starts_with("chat-42:"));
        assert!(reply.contains("BTC is currently around"));
        assert!(!reply.contains("\"tool_calls\""));
        assert!(!reply.contains("mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_scopes_sender_session_key_for_sessions_current_tool() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let tmp = TempDir::new().unwrap();
        let session_store: Arc<dyn zeroclaw_infra::session_backend::SessionBackend> =
            Arc::new(zeroclaw_infra::session_store::SessionStore::new(tmp.path()).unwrap());

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(SessionsCurrentModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(
                zeroclaw_runtime::tools::SessionsCurrentTool::new(Arc::clone(&session_store)),
            )]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            hooks: None,
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: Some(Arc::clone(&session_store)),
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(&{
                let mut profile = zeroclaw_config::schema::RiskProfileConfig::default();
                profile.auto_approve.push("sessions_current".to_string());
                profile
            })),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            agent_transcription_provider: String::new(),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-42".to_string(),
                content: "Which session is this?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(!sent_messages.is_empty());
        let reply = sent_messages.last().unwrap();
        assert!(reply.contains("Current session: test-channel_chat-42_alice"));
        assert!(reply.contains("Messages: 1"));
    }

    #[tokio::test]
    async fn process_channel_message_renders_trailing_tool_receipts_block_when_enabled() {
        // Activated path: a real ReceiptGenerator + show_receipts_in_response=true
        // must produce a second send carrying the "Tool receipts:" block with a
        // valid zc-receipt-* token. Pre-#6214 this was dead code from the test
        // suite because every ChannelRuntimeContext literal pinned the feature
        // off; this test guards the integration so a regression in the block
        // render or send call surfaces in CI rather than in production.
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ToolCallingModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            non_cli_excluded_tools: Arc::new(Vec::new()),
            // Full autonomy + auto-approve mock_price so the loop actually
            // reaches execute_one_tool. The other tests in this file pass
            // under Supervised because ToolCallingProvider returns the BTC
            // reply regardless of whether the tool ran (the LLM only needs
            // to see a `[Tool results]` user message — even a "denied"
            // payload triggers the deterministic response). Receipts only
            // generate on the actual execute path, so we need the gate
            // open here.
            autonomy_level: AutonomyLevel::Full,
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            hooks: None,
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig {
                    level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                    auto_approve: vec!["mock_price".to_string()],
                    ..Default::default()
                },
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: Some(
                zeroclaw_runtime::agent::tool_receipts::ReceiptGenerator::new(),
            ),
            show_receipts_in_response: true,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            agent_transcription_provider: String::new(),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-42".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        // Two sends: the model's reply and the trailing receipts block.
        assert!(
            sent_messages.len() >= 2,
            "expected at least 2 sends (reply + receipts block), got {}: {:?}",
            sent_messages.len(),
            sent_messages
        );

        let receipts_message = sent_messages
            .iter()
            .find(|m| m.contains("Tool receipts:"))
            .unwrap_or_else(|| {
                panic!(
                    "no `Tool receipts:` send found; got {:?}",
                    sent_messages.as_slice()
                )
            });
        assert!(
            receipts_message.starts_with("chat-42:"),
            "receipts block must be sent to the same reply target as the agent reply, got {receipts_message}"
        );
        assert!(
            receipts_message.contains("---\nTool receipts:"),
            "receipts block must be prefixed with the documented `---\\nTool receipts:` separator, got {receipts_message}"
        );
        assert!(
            receipts_message.contains("zc-receipt-"),
            "receipts block must carry at least one zc-receipt-* HMAC token (proves the generator actually ran), got {receipts_message}"
        );
        assert!(
            receipts_message.contains("mock_price"),
            "receipts block should name the tool that produced the receipt, got {receipts_message}"
        );
    }

    #[tokio::test]
    async fn process_channel_message_omits_receipts_block_when_disabled() {
        // Backward-compat: with show_receipts_in_response=false (default), no
        // trailing receipts message is sent — even when a generator is active
        // and the loop ran tools. This is the path every other test relies on
        // implicitly; assert it once explicitly.
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ToolCallingModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            non_cli_excluded_tools: Arc::new(Vec::new()),
            // Match the enabled-test setup so the tool actually runs; the
            // assertion below proves the receipt-block send is gated on
            // `show_receipts_in_response` and not on whether the loop saw
            // any receipts.
            autonomy_level: AutonomyLevel::Full,
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            hooks: None,
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig {
                    level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                    auto_approve: vec!["mock_price".to_string()],
                    ..Default::default()
                },
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: Some(
                zeroclaw_runtime::agent::tool_receipts::ReceiptGenerator::new(),
            ),
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            agent_transcription_provider: String::new(),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-42".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(
            !sent_messages.iter().any(|m| m.contains("Tool receipts:")),
            "no receipts block must be sent when show_receipts_in_response=false; got {:?}",
            sent_messages.as_slice()
        );
    }

    #[tokio::test]
    async fn process_channel_message_disabled_receipt_generator_emits_no_receipts_anywhere() {
        // Strict #6182 acceptance criterion: enabled=false must emit no
        // receipt anywhere — not in any sent message, not in the model's
        // view of conversation history. `receipt_generator: None` is the
        // wire-level reflection of `[agent.resolved.tool_receipts] enabled = false`.
        // Distinct from the show_in_response=false test above (which keeps
        // the generator on but suppresses the trailing block); this one
        // proves nothing is signed in the first place.
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ToolCallingModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::Full,
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            hooks: None,
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig {
                    level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                    auto_approve: vec!["mock_price".to_string()],
                    ..Default::default()
                },
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            agent_transcription_provider: String::new(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-42".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(
            !sent_messages.is_empty(),
            "agent must still respond when receipts are disabled"
        );
        assert!(
            !sent_messages.iter().any(|m| m.contains("zc-receipt-")),
            "no zc-receipt- token must appear in any sent message when receipts are disabled, got {:?}",
            sent_messages.as_slice()
        );
        assert!(
            !sent_messages.iter().any(|m| m.contains("Tool receipts:")),
            "no `Tool receipts:` block must be sent when receipts are disabled, got {:?}",
            sent_messages.as_slice()
        );

        // Strict surface check: the model's view of conversation history must
        // not carry a `[receipt: ` trailer either, otherwise an LLM trained
        // on echoing receipts could leak signed-looking output even though
        // nothing was actually signed.
        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (_key, turns) in histories.iter() {
            for msg in turns.iter() {
                assert!(
                    !msg.content.contains("[receipt: "),
                    "no `[receipt: ` trailer must appear in conversation history when receipts are disabled, got: {}",
                    msg.content
                );
            }
        }
    }

    #[tokio::test]
    async fn process_channel_message_telegram_does_not_persist_tool_summary_prefix() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ToolCallingModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-telegram-tool-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-telegram".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(!sent_messages.is_empty());
        let reply = sent_messages.last().unwrap();
        assert!(reply.contains("BTC is currently around"));

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .peek("telegram_chat-telegram_alice")
            .expect("telegram history should be stored");
        let assistant_turn = turns
            .iter()
            .rev()
            .find(|turn| turn.role == "assistant")
            .expect("assistant turn should be present");
        assert!(
            !assistant_turn.content.contains("[Used tools:"),
            "telegram history should not persist tool-summary prefix"
        );
    }

    #[tokio::test]
    async fn process_channel_message_strips_unexecuted_tool_json_artifacts_from_reply() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(RawToolArtifactModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-raw-json".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-raw".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 3,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("chat-raw:"));
        assert!(sent_messages[0].contains("BTC is currently around"));
        assert!(!sent_messages[0].contains("\"name\":\"mock_price\""));
        assert!(!sent_messages[0].contains("\"result\""));
    }

    #[tokio::test]
    async fn process_channel_message_executes_tool_calls_with_alias_tags() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ToolCallingAliasModelProvider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-2".to_string(),
                sender: "bob".to_string(),
                reply_target: "chat-84".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(!sent_messages.is_empty());
        let reply = sent_messages.last().unwrap();
        assert!(reply.starts_with("chat-84:"));
        assert!(reply.contains("alias-tag flow resolved"));
        assert!(!reply.contains("<toolcall>"));
        assert!(!reply.contains("mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_handles_models_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
        let alt_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let alt_model_provider: Arc<dyn ModelProvider> = alt_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(
            "test-provider".to_string(),
            Arc::clone(&agent_model_provider),
        );
        provider_cache_seed.insert("openrouter.default".to_string(), alt_model_provider);

        let mut prompt_config = zeroclaw_config::schema::Config::default();
        prompt_config
            .providers
            .models
            .ensure("openrouter", "default")
            .expect("openrouter slot must exist");

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&agent_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(prompt_config),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-cmd-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/models openrouter".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("ModelProvider switched to `openrouter.default`"));

        let route_key = "telegram_chat-1_alice";
        let route = runtime_ctx
            .route_overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(route_key)
            .cloned()
            .expect("route should be stored for sender");
        assert_eq!(route.model_provider, "openrouter.default");
        assert_eq!(route.model, "default-model");

        assert_eq!(
            agent_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
        assert_eq!(alt_model_provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_uses_route_override_provider_and_model() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
        let routed_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let routed_model_provider: Arc<dyn ModelProvider> = routed_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(
            "test-provider".to_string(),
            Arc::clone(&agent_model_provider),
        );
        provider_cache_seed.insert("openrouter".to_string(), routed_model_provider);

        let route_key = "telegram_chat-1_alice".to_string();
        let mut route_overrides = HashMap::new();
        route_overrides.insert(
            route_key,
            ChannelRouteSelection {
                model_provider: "openrouter".into(),
                model: "route-model".to_string(),
                api_key: None,
            },
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&agent_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(route_overrides)),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-routed-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello routed model_provider".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        assert_eq!(
            agent_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            routed_model_provider_impl.call_count.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            routed_model_provider_impl
                .models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            &["route-model".to_string()]
        );
    }

    #[tokio::test]
    async fn process_channel_message_uses_classifier_provider_for_precheck_model_selection() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();
        let main_provider_impl = Arc::new(PrecheckProbeModelProvider::default());
        let main_provider: Arc<dyn ModelProvider> = main_provider_impl.clone();
        let classifier_provider_impl = Arc::new(PrecheckProbeModelProvider::default());
        let classifier_provider: Arc<dyn ModelProvider> = classifier_provider_impl.clone();
        let mut prompt_config = zeroclaw_config::schema::Config::default();
        prompt_config.providers.models.openai.insert(
            "my-classifier".to_string(),
            zeroclaw_config::schema::OpenAIModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("fast-intent".to_string()),
                    temperature: Some(0.0),
                    ..Default::default()
                },
            },
        );
        let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
            classifier_provider: zeroclaw_config::providers::ModelProviderRef::from(
                "openai.my-classifier",
            ),
            ..Default::default()
        };
        let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
            channel,
            main_provider,
            prompt_config,
            agent_cfg,
            "test-provider",
        );
        runtime_ctx
            .provider_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("openai.my-classifier".to_string(), classifier_provider);

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-classifier-provider".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-precheck".to_string(),
                content: "background chatter".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        assert_eq!(
            classifier_provider_impl
                .precheck_calls
                .load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            classifier_provider_impl.main_calls.load(Ordering::SeqCst),
            0
        );
        assert_eq!(main_provider_impl.precheck_calls.load(Ordering::SeqCst), 0);
        assert_eq!(main_provider_impl.main_calls.load(Ordering::SeqCst), 0);
        let models = classifier_provider_impl
            .models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(models.as_slice(), ["fast-intent"]);
        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(
            sent_messages.is_empty(),
            "provider returns NO_REPLY from precheck, so no visible reply should be sent"
        );
    }

    #[tokio::test]
    async fn process_channel_message_prefers_cached_default_provider_instance() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let startup_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let startup_model_provider: Arc<dyn ModelProvider> = startup_model_provider_impl.clone();
        let reloaded_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let reloaded_model_provider: Arc<dyn ModelProvider> = reloaded_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), reloaded_model_provider);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&startup_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-default-provider-cache".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello cached default model_provider".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 3,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;
    }

    #[tokio::test]
    async fn process_channel_message_respects_configured_max_tool_iterations_above_default() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(IterativeToolModelProvider {
                required_tool_iterations: 11,
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 12,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig {
                loop_detection_enabled: false,
                ..zeroclaw_config::schema::PacingConfig::default()
            },
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-iter-success".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-iter-success".to_string(),
                content: "Loop until done".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(!sent_messages.is_empty());
        let reply = sent_messages.last().unwrap();
        assert!(reply.starts_with("chat-iter-success:"));
        assert!(reply.contains("Completed after 11 tool iterations."));
        assert!(!reply.contains("⚠️ Error:"));
    }

    #[tokio::test]
    async fn process_channel_message_reports_configured_max_tool_iterations_limit() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(IterativeToolModelProvider {
                required_tool_iterations: 20,
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 3,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig {
                loop_detection_enabled: false,
                ..zeroclaw_config::schema::PacingConfig::default()
            },
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-iter-fail".to_string(),
                sender: "bob".to_string(),
                reply_target: "chat-iter-fail".to_string(),
                content: "Loop forever".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(!sent_messages.is_empty());
        let reply = sent_messages.last().unwrap();
        assert!(reply.starts_with("chat-iter-fail:"));
        // After Phase 9, the agent attempts a graceful summary instead of erroring.
        // The mock model_provider returns a tool call payload as text, which the agent
        // returns as its "summary". The key invariant: the loop terminates and
        // produces a response (not hanging forever).
        assert!(
            reply.contains("⚠️ Error: Agent exceeded maximum tool iterations (3)")
                || reply.len() > "chat-iter-fail:".len(),
            "Expected either an error message or a graceful summary response"
        );
    }

    struct NoopMemory;

    #[async_trait::async_trait]
    impl Memory for NoopMemory {
        fn name(&self) -> &str {
            "noop"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&zeroclaw_memory::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for NoopMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "NoopMemory"
        }
    }

    struct RecallMemory;

    #[async_trait::async_trait]
    impl Memory for RecallMemory {
        fn name(&self) -> &str {
            "recall-memory"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(vec![zeroclaw_memory::MemoryEntry {
                id: "entry-1".to_string(),
                key: "memory_key_1".to_string(),
                content: "Age is 45".to_string(),
                category: zeroclaw_memory::MemoryCategory::Conversation,
                timestamp: "2026-02-20T00:00:00Z".to_string(),
                session_id: None,
                score: Some(0.9),
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
                agent_alias: None,
                agent_id: None,
            }])
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&zeroclaw_memory::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            self.recall(query, limit, session_id, since, until).await
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for RecallMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "RecallMemory"
        }
    }

    /// Model provider used by `message_dispatch_processes_messages_in_parallel`
    /// to observe concurrent in-flight calls directly instead of inferring
    /// parallelism from wall-clock elapsed time.
    ///
    /// Each `chat_with_system` invocation increments `in_flight` on entry,
    /// records the running peak into `peak_in_flight`, then decrements on
    /// exit. After the dispatch loop returns, the test asserts
    /// `peak_in_flight >= 2`, which directly proves two messages were being
    /// processed at the same time. This replaces the original
    /// `elapsed < 700ms` assertion (issue #6813), which flaked on slow
    /// runners because it depended on machine speed rather than on
    /// observable concurrency.
    struct ConcurrencyTrackingProvider {
        delay: Duration,
        in_flight: Arc<AtomicUsize>,
        peak_in_flight: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for ConcurrencyTrackingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak_in_flight.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(format!("echo: {message}"))
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ConcurrencyTrackingProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ConcurrencyTrackingProvider"
        }
    }

    #[tokio::test]
    async fn message_dispatch_processes_messages_in_parallel() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak_in_flight = Arc::new(AtomicUsize::new(0));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(ConcurrencyTrackingProvider {
                delay: Duration::from_millis(250),
                in_flight: in_flight.clone(),
                peak_in_flight: peak_in_flight.clone(),
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(4);
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "1".to_string(),
            sender: "alice".to_string(),
            reply_target: "alice".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".to_string(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        })
        .await
        .unwrap();
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "2".to_string(),
            sender: "bob".to_string(),
            reply_target: "bob".to_string(),
            content: "world".to_string(),
            channel: "test-channel".to_string(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        })
        .await
        .unwrap();
        drop(tx);

        run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 2).await;

        // Deterministic concurrency check: the dispatcher should have processed
        // both messages in parallel, so the peak number of simultaneously
        // in-flight model calls must reach at least 2. This observes parallelism
        // directly rather than inferring it from wall-clock elapsed time, which
        // flaked on slow runners (issue #6813).
        let peak = peak_in_flight.load(Ordering::SeqCst);
        assert!(
            peak >= 2,
            "expected at least 2 concurrent in-flight dispatches, got peak {}",
            peak
        );
        assert_eq!(
            in_flight.load(Ordering::SeqCst),
            0,
            "all in-flight dispatches should have completed",
        );

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 2);
    }

    #[tokio::test]
    async fn message_dispatch_interrupts_in_flight_telegram_request_and_preserves_context() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(DelayedHistoryCaptureModelProvider {
            delay: Duration::from_millis(250),
            calls: std::sync::Mutex::new(Vec::new()),
        });

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: true,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
        let send_task = zeroclaw_spawn::spawn!(async move {
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "forwarded content".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-2".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "summarize this".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
        send_task.await.unwrap();

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("chat-1:"));
        assert!(sent_messages[0].contains("response-2"));
        drop(sent_messages);

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 2);
        let second_call = &calls[1];
        assert!(
            second_call
                .iter()
                .any(|(role, content)| { role == "user" && content.contains("forwarded content") })
        );
        assert!(
            second_call
                .iter()
                .any(|(role, content)| { role == "user" && content.contains("summarize this") })
        );
        assert!(
            !second_call.iter().any(|(role, _)| role == "assistant"),
            "cancelled turn should not persist an assistant response"
        );
    }

    #[tokio::test]
    async fn message_dispatch_interrupts_in_flight_slack_request_and_preserves_context() {
        let channel_impl = Arc::new(SlackRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(DelayedHistoryCaptureModelProvider {
            delay: Duration::from_millis(250),
            calls: std::sync::Mutex::new(Vec::new()),
        });

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: true,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
        let send_task = zeroclaw_spawn::spawn!(async move {
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "U123".to_string(),
                reply_target: "C123".to_string(),
                content: "first question".to_string(),
                channel: "slack".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: Some("1741234567.100001".to_string()),
                interruption_scope_id: Some("1741234567.100001".to_string()),
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-2".to_string(),
                sender: "U123".to_string(),
                reply_target: "C123".to_string(),
                content: "second question".to_string(),
                channel: "slack".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: Some("1741234567.100001".to_string()),
                interruption_scope_id: Some("1741234567.100001".to_string()),
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
        send_task.await.unwrap();

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("C123:"));
        assert!(sent_messages[0].contains("response-2"));
        drop(sent_messages);

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 2);
        let second_call = &calls[1];
        assert!(
            second_call
                .iter()
                .any(|(role, content)| { role == "user" && content.contains("first question") })
        );
        assert!(
            second_call
                .iter()
                .any(|(role, content)| { role == "user" && content.contains("second question") })
        );
        assert!(
            !second_call.iter().any(|(role, _)| role == "assistant"),
            "cancelled turn should not persist an assistant response"
        );
    }

    #[tokio::test]
    async fn message_dispatch_interrupts_in_flight_whatsapp_request_and_preserves_context() {
        let channel_impl = Arc::new(WhatsAppRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(DelayedHistoryCaptureModelProvider {
            delay: Duration::from_millis(250),
            calls: std::sync::Mutex::new(Vec::new()),
        });

        let mut channel_config = zeroclaw_config::schema::ChannelsConfig::default();
        channel_config.whatsapp.insert(
            "default".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                session_path: Some("/tmp/zeroclaw-whatsapp-session.db".into()),
                interrupt_on_new_message: true,
                ..Default::default()
            },
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: interrupt_on_new_message_config(&channel_config),
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
        let send_task = zeroclaw_spawn::spawn!(async move {
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "15555550123".to_string(),
                reply_target: "15555550123".to_string(),
                content: "first WhatsApp question".to_string(),
                channel: "whatsapp".to_string(),
                channel_alias: Some("default".to_string()),
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-2".to_string(),
                sender: "15555550123".to_string(),
                reply_target: "15555550123".to_string(),
                content: "second WhatsApp question".to_string(),
                channel: "whatsapp".to_string(),
                channel_alias: Some("default".to_string()),
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
        send_task.await.unwrap();

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("15555550123:"));
        assert!(sent_messages[0].contains("response-2"));
        drop(sent_messages);

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 2);
        let second_call = &calls[1];
        assert!(second_call.iter().any(|(role, content)| {
            role == "user" && content.contains("first WhatsApp question")
        }));
        assert!(second_call.iter().any(|(role, content)| {
            role == "user" && content.contains("second WhatsApp question")
        }));
        assert!(
            !second_call.iter().any(|(role, _)| role == "assistant"),
            "cancelled turn should not persist an assistant response"
        );
    }

    #[tokio::test]
    async fn message_dispatch_interrupt_scope_is_same_sender_same_chat() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(SlowModelProvider {
                delay: Duration::from_millis(180),
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: true,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
        let send_task = zeroclaw_spawn::spawn!(async move {
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-a".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "first chat".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "msg-b".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-2".to_string(),
                content: "second chat".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
        send_task.await.unwrap();

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 2);
        assert!(sent_messages.iter().any(|msg| msg.starts_with("chat-1:")));
        assert!(sent_messages.iter().any(|msg| msg.starts_with("chat-2:")));
    }

    #[tokio::test]
    async fn process_channel_message_cancels_scoped_typing_task() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(SlowModelProvider {
                delay: Duration::from_millis(20),
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "typing-msg".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-typing".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let starts = channel_impl.start_typing_calls.load(Ordering::SeqCst);
        let stops = channel_impl.stop_typing_calls.load(Ordering::SeqCst);
        assert_eq!(starts, 1, "start_typing should be called once");
        assert_eq!(stops, 1, "stop_typing should be called once");
    }

    #[tokio::test]
    async fn process_channel_message_adds_and_swaps_reactions() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(SlowModelProvider {
                delay: Duration::from_millis(5),
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "react-msg".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-react".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let added = channel_impl.reactions_added.lock().await;
        assert!(
            added.len() >= 2,
            "expected at least 2 reactions added (\u{1F440} then \u{2705}), got {}",
            added.len()
        );
        assert_eq!(added[0].2, "\u{1F440}", "first reaction should be eyes");
        assert_eq!(
            added.last().unwrap().2,
            "\u{2705}",
            "last reaction should be checkmark"
        );

        let removed = channel_impl.reactions_removed.lock().await;
        assert_eq!(removed.len(), 1, "eyes reaction should be removed once");
        assert_eq!(removed[0].2, "\u{1F440}");
    }

    #[test]
    fn prompt_contains_all_sections() {
        let ws = make_workspace();
        let tools = vec![("shell", "Run commands"), ("file_read", "Read files")];
        let prompt = build_system_prompt(ws.path(), "test-model", &tools, &[], None, None);

        // Section headers
        assert!(prompt.contains("## Tools"), "missing Tools section");
        assert!(prompt.contains("## Safety"), "missing Safety section");
        assert!(prompt.contains("## Workspace"), "missing Workspace section");
        assert!(
            prompt.contains("## Project Context"),
            "missing Project Context"
        );
        assert!(prompt.contains("## Current Date"), "missing Date section");
        assert!(
            !prompt.contains("## Current Date & Time"),
            "prompt should use date-only context"
        );
        assert!(prompt.contains("## Runtime"), "missing Runtime section");
    }

    #[test]
    fn prompt_injects_tools() {
        let ws = make_workspace();
        let tools = vec![
            ("shell", "Run commands"),
            ("memory_recall", "Search memory"),
        ];
        let prompt = build_system_prompt(ws.path(), "gpt-4o", &tools, &[], None, None);

        assert!(prompt.contains("**shell**"));
        assert!(prompt.contains("Run commands"));
        assert!(prompt.contains("**memory_recall**"));
    }

    #[test]
    fn prompt_includes_single_tool_protocol_block_after_append() {
        let ws = make_workspace();
        let tools = vec![("shell", "Run commands")];
        let mut prompt = build_system_prompt(ws.path(), "gpt-4o", &tools, &[], None, None);

        assert!(
            !prompt.contains("## Tool Use Protocol"),
            "build_system_prompt should not emit protocol block directly"
        );

        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        prompt.push_str(&build_tool_instructions(&tools_registry));

        assert_eq!(
            prompt.matches("## Tool Use Protocol").count(),
            1,
            "protocol block should appear exactly once in the final prompt"
        );
    }

    #[test]
    fn channel_strict_non_native_prompt_hides_text_tool_protocol() {
        let ws = make_workspace();
        let mut tool_descs = vec![("shell", "Run commands")];
        let mut deferred_section = "## Deferred MCP Tools\n\n- mcp__example".to_string();

        let expose_text_protocol =
            apply_text_tool_prompt_policy(false, true, &mut tool_descs, &mut deferred_section);

        let mut prompt = build_system_prompt_with_mode_and_autonomy(
            ws.path(),
            "gpt-4o",
            &tool_descs,
            &[],
            None,
            None,
            None,
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            false,
            0,
            false,
        );
        if expose_text_protocol {
            let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
            let effective_tool_names: HashSet<&str> =
                tools_registry.iter().map(|tool| tool.name()).collect();
            prompt.push_str(&build_tool_instructions_for_names(
                &tools_registry,
                &effective_tool_names,
            ));
        }
        if !deferred_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&deferred_section);
        }

        assert!(!expose_text_protocol);
        assert!(!prompt.contains("## Tools"));
        assert!(!prompt.contains("## Tool Use Protocol"));
        assert!(!prompt.contains("<tool_call>"));
        assert!(!prompt.contains("mcp__example"));
    }

    #[test]
    fn prompt_injects_safety() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(prompt.contains("Do not exfiltrate private data"));
        assert!(prompt.contains("Respect the runtime autonomy policy"));
        assert!(prompt.contains("Prefer `trash` over `rm`"));
    }

    #[test]
    fn prompt_injects_workspace_files() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(prompt.contains("### SOUL.md"), "missing SOUL.md header");
        assert!(prompt.contains("Be helpful"), "missing SOUL content");
        assert!(prompt.contains("### IDENTITY.md"), "missing IDENTITY.md");
        assert!(
            prompt.contains("Name: ZeroClaw"),
            "missing IDENTITY content"
        );
        assert!(prompt.contains("### USER.md"), "missing USER.md");
        assert!(prompt.contains("### AGENTS.md"), "missing AGENTS.md");
        assert!(prompt.contains("### TOOLS.md"), "missing TOOLS.md");
        // HEARTBEAT.md is intentionally excluded from channel prompts — it's only
        // relevant to the heartbeat worker and causes LLMs to emit spurious
        // "HEARTBEAT_OK" acknowledgments in channel conversations.
        assert!(
            !prompt.contains("### HEARTBEAT.md"),
            "HEARTBEAT.md should not be in channel prompt"
        );
        assert!(prompt.contains("### MEMORY.md"), "missing MEMORY.md");
        assert!(prompt.contains("User likes Rust"), "missing MEMORY content");
    }

    #[test]
    fn prompt_missing_file_markers() {
        let tmp = TempDir::new().unwrap();
        // Empty workspace — no files at all
        let prompt = build_system_prompt(tmp.path(), "model", &[], &[], None, None);

        assert!(prompt.contains("[File not found: SOUL.md]"));
        assert!(prompt.contains("[File not found: AGENTS.md]"));
        assert!(prompt.contains("[File not found: IDENTITY.md]"));
    }

    #[test]
    fn prompt_bootstrap_only_if_exists() {
        let ws = make_workspace();
        // No BOOTSTRAP.md — should not appear
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);
        assert!(
            !prompt.contains("### BOOTSTRAP.md"),
            "BOOTSTRAP.md should not appear when missing"
        );

        // Create BOOTSTRAP.md — should appear
        std::fs::write(ws.path().join("BOOTSTRAP.md"), "# Bootstrap\nFirst run.").unwrap();
        let prompt2 = build_system_prompt(ws.path(), "model", &[], &[], None, None);
        assert!(
            prompt2.contains("### BOOTSTRAP.md"),
            "BOOTSTRAP.md should appear when present"
        );
        assert!(prompt2.contains("First run"));
    }

    #[test]
    fn prompt_no_daily_memory_injection() {
        let ws = make_workspace();
        let memory_dir = ws.path().join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        std::fs::write(
            memory_dir.join(format!("{today}.md")),
            "# Daily\nSome note.",
        )
        .unwrap();

        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        // Daily notes should NOT be in the system prompt (on-demand via tools)
        assert!(
            !prompt.contains("Daily Notes"),
            "daily notes should not be auto-injected"
        );
        assert!(
            !prompt.contains("Some note"),
            "daily content should not be in prompt"
        );
    }

    #[test]
    fn prompt_runtime_metadata() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "claude-sonnet-4", &[], &[], None, None);

        assert!(prompt.contains("Model: claude-sonnet-4"));
        assert!(prompt.contains(&format!("OS: {}", std::env::consts::OS)));
        assert!(prompt.contains("Host:"));
    }

    #[test]
    fn prompt_skills_include_instructions_and_tools() {
        let ws = make_workspace();
        let skills = vec![zeroclaw_runtime::skills::Skill {
            name: "code-review".into(),
            description: "Review code for bugs".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![zeroclaw_runtime::skills::SkillTool {
                name: "lint".into(),
                description: "Run static checks".into(),
                kind: "shell".into(),
                command: "cargo clippy".into(),
                args: HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
            }],
            prompts: vec!["Always run cargo test before final response.".into()],
            location: None,
        }];

        let prompt = build_system_prompt(ws.path(), "model", &[], &skills, None, None);

        assert!(prompt.contains("<available_skills>"), "missing skills XML");
        assert!(prompt.contains("<name>code-review</name>"));
        assert!(prompt.contains("<description>Review code for bugs</description>"));
        assert!(prompt.contains("SKILL.md</location>"));
        assert!(prompt.contains("<instructions>"));
        assert!(
            prompt.contains(
                "<instruction>Always run cargo test before final response.</instruction>"
            )
        );
        // Registered tools (shell kind) appear under <callable_tools> with prefixed names
        assert!(prompt.contains("<callable_tools"));
        assert!(prompt.contains("<name>code-review__lint</name>"));
        assert!(!prompt.contains("loaded on demand"));
    }

    #[test]
    fn prompt_skills_compact_mode_omits_instructions_but_keeps_tools() {
        let ws = make_workspace();
        let skills = vec![zeroclaw_runtime::skills::Skill {
            name: "code-review".into(),
            description: "Review code for bugs".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![zeroclaw_runtime::skills::SkillTool {
                name: "lint".into(),
                description: "Run static checks".into(),
                kind: "shell".into(),
                command: "cargo clippy".into(),
                args: HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
            }],
            prompts: vec!["Always run cargo test before final response.".into()],
            location: None,
        }];

        let prompt = build_system_prompt_with_mode(
            ws.path(),
            "model",
            &[],
            &skills,
            None,
            None,
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
            AutonomyLevel::default(),
        );

        assert!(prompt.contains("<available_skills>"), "missing skills XML");
        assert!(prompt.contains("<name>code-review</name>"));
        assert!(prompt.contains("<location>skills/code-review/SKILL.md</location>"));
        assert!(prompt.contains("loaded on demand"));
        assert!(!prompt.contains("<instructions>"));
        assert!(
            !prompt.contains(
                "<instruction>Always run cargo test before final response.</instruction>"
            )
        );
        // Compact mode should still include tools so the LLM knows about them.
        // Registered tools (shell kind) appear under <callable_tools> with prefixed names.
        assert!(prompt.contains("<callable_tools"));
        assert!(prompt.contains("<name>code-review__lint</name>"));
    }

    #[test]
    fn prompt_skills_escape_reserved_xml_chars() {
        let ws = make_workspace();
        let skills = vec![zeroclaw_runtime::skills::Skill {
            name: "code<review>&".into(),
            description: "Review \"unsafe\" and 'risky' bits".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![zeroclaw_runtime::skills::SkillTool {
                name: "run\"linter\"".into(),
                description: "Run <lint> & report".into(),
                kind: "shell&exec".into(),
                command: "cargo clippy".into(),
                args: HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
            }],
            prompts: vec!["Use <tool_call> and & keep output \"safe\"".into()],
            location: None,
        }];

        let prompt = build_system_prompt(ws.path(), "model", &[], &skills, None, None);

        assert!(prompt.contains("<name>code&lt;review&gt;&amp;</name>"));
        assert!(prompt.contains(
            "<description>Review &quot;unsafe&quot; and &apos;risky&apos; bits</description>"
        ));
        assert!(prompt.contains("<name>run&quot;linter&quot;</name>"));
        assert!(prompt.contains("<description>Run &lt;lint&gt; &amp; report</description>"));
        assert!(prompt.contains("<kind>shell&amp;exec</kind>"));
        assert!(prompt.contains(
            "<instruction>Use &lt;tool_call&gt; and &amp; keep output &quot;safe&quot;</instruction>"
        ));
    }

    #[test]
    fn prompt_truncation() {
        let ws = make_workspace();
        // Write a file larger than BOOTSTRAP_MAX_CHARS
        let big_content = "x".repeat(BOOTSTRAP_MAX_CHARS + 1000);
        std::fs::write(ws.path().join("AGENTS.md"), &big_content).unwrap();

        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(
            prompt.contains("truncated at"),
            "large files should be truncated"
        );
        assert!(
            !prompt.contains(&big_content),
            "full content should not appear"
        );
    }

    #[test]
    fn prompt_empty_files_skipped() {
        let ws = make_workspace();
        std::fs::write(ws.path().join("TOOLS.md"), "").unwrap();

        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        // Empty file should not produce a header
        assert!(
            !prompt.contains("### TOOLS.md"),
            "empty files should be skipped"
        );
    }

    #[test]
    fn channel_log_truncation_is_utf8_safe_for_multibyte_text() {
        let msg = "Hello from ZeroClaw 🌍. Current status is healthy, and café-style UTF-8 text stays safe in logs.";

        // Reproduces the production crash path where channel logs truncate at 80 chars.
        let result =
            std::panic::catch_unwind(|| zeroclaw_runtime::util::truncate_with_ellipsis(msg, 80));
        assert!(
            result.is_ok(),
            "truncate_with_ellipsis should never panic on UTF-8"
        );

        let truncated = result.unwrap();
        assert!(!truncated.is_empty());
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn prompt_contains_channel_capabilities() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(
            prompt.contains("## Channel Capabilities"),
            "missing Channel Capabilities section"
        );
        assert!(
            prompt.contains("running as a messaging bot"),
            "missing channel context"
        );
        assert!(
            prompt.contains("NEVER repeat, describe, or echo credentials"),
            "missing security instruction"
        );
    }

    #[test]
    fn full_autonomy_prompt_executes_allowed_tools_without_extra_approval() {
        let ws = make_workspace();
        let config = zeroclaw_config::schema::RiskProfileConfig {
            level: zeroclaw_runtime::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let prompt = build_system_prompt_with_mode_and_autonomy(
            ws.path(),
            "model",
            &[],
            &[],
            None,
            None,
            Some(&config),
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            false,
            0,
            false,
        );

        assert!(
            prompt.contains("execute it directly instead of asking the user for extra approval"),
            "full autonomy should instruct direct execution for allowed tools"
        );
        assert!(
            prompt.contains("Never pretend you are waiting for a human approval"),
            "full autonomy should not simulate interactive approval flows"
        );
    }

    #[test]
    fn readonly_prompt_explains_policy_blocks_without_fake_approval() {
        let ws = make_workspace();
        let config = zeroclaw_config::schema::RiskProfileConfig {
            level: zeroclaw_runtime::security::AutonomyLevel::ReadOnly,
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let prompt = build_system_prompt_with_mode_and_autonomy(
            ws.path(),
            "model",
            &[],
            &[],
            None,
            None,
            Some(&config),
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            false,
            0,
            false,
        );

        assert!(
            prompt.contains("this runtime is read-only for side effects"),
            "read-only prompt should expose the runtime restriction"
        );
        assert!(
            prompt.contains("instead of simulating an approval flow"),
            "read-only prompt should explain restrictions instead of faking approval"
        );
    }

    #[test]
    fn prompt_workspace_path() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(prompt.contains(&format!("Working directory: `{}`", ws.path().display())));
    }

    #[test]
    fn full_autonomy_omits_approval_instructions() {
        let ws = make_workspace();
        let prompt = build_system_prompt_with_mode(
            ws.path(),
            "model",
            &[],
            &[],
            None,
            None,
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            AutonomyLevel::Full,
        );

        assert!(
            !prompt.contains("without asking"),
            "full autonomy prompt must not tell the model to ask before acting"
        );
        assert!(
            !prompt.contains("ask before acting externally"),
            "full autonomy prompt must not contain ask-before-acting instruction"
        );
        // Core safety rules should still be present
        assert!(
            prompt.contains("Do not exfiltrate private data"),
            "data exfiltration guard must remain"
        );
        assert!(
            prompt.contains("Prefer `trash` over `rm`"),
            "trash-over-rm hint must remain"
        );
    }

    #[test]
    fn supervised_autonomy_includes_approval_instructions() {
        let ws = make_workspace();
        let prompt = build_system_prompt_with_mode(
            ws.path(),
            "model",
            &[],
            &[],
            None,
            None,
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            AutonomyLevel::Supervised,
        );

        assert!(
            prompt.contains("without asking"),
            "supervised prompt must include ask-before-acting instruction"
        );
        assert!(
            prompt.contains("ask before acting externally"),
            "supervised prompt must include ask-before-acting instruction"
        );
    }

    #[test]
    fn channel_notify_observer_truncates_utf8_arguments_safely() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let observer = ChannelNotifyObserver {
            inner: Arc::new(NoopObserver),
            tx,
            tools_used: AtomicBool::new(false),
        };

        let payload = (0..300)
            .map(|n| serde_json::json!({ "content": format!("{}置tail", "a".repeat(n)) }))
            .map(|v| v.to_string())
            .find(|raw| raw.len() > 120 && !raw.is_char_boundary(120))
            .expect("should produce non-char-boundary data at byte index 120");

        observer.record_event(
            &zeroclaw_runtime::observability::traits::ObserverEvent::ToolCallStart {
                tool: "file_write".to_string(),
                tool_call_id: None,
                arguments: Some(payload),
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
        );

        let emitted = rx.try_recv().expect("observer should emit notify message");
        assert!(emitted.contains("`file_write`"));
        assert!(emitted.is_char_boundary(emitted.len()));
    }

    #[test]
    fn conversation_memory_key_uses_message_id() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "msg_abc123".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "hello".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        assert_eq!(conversation_memory_key(&msg), "slack_U123_msg_abc123");
    }

    #[test]
    fn followup_thread_id_prefers_thread_ts() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "slack_C123_1741234567.123456".into(),
            sender: "U123".into(),
            reply_target: "C123".into(),
            content: "hello".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: Some("1741234567.123456".into()),
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        assert_eq!(
            followup_thread_id(&msg).as_deref(),
            Some("1741234567.123456")
        );
    }

    #[test]
    fn followup_thread_id_falls_back_to_message_id() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "msg_abc123".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "hello".into(),
            channel: "cli".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        assert_eq!(followup_thread_id(&msg).as_deref(), Some("msg_abc123"));
    }

    #[test]
    fn followup_thread_id_does_not_open_matrix_thread_for_root_message() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "$event:server".into(),
            sender: "@alice:server".into(),
            reply_target: "!room:server".into(),
            content: "hello".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        assert_eq!(followup_thread_id(&msg), None);
    }

    #[test]
    fn matrix_root_conversation_history_key_omits_event_id() {
        let first = zeroclaw_api::channel::ChannelMessage {
            id: "$first:server".into(),
            sender: "@alice:server".into(),
            reply_target: "!room:server".into(),
            content: "send a.txt".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let second = zeroclaw_api::channel::ChannelMessage {
            id: "$second:server".into(),
            content: "send it again".into(),
            timestamp: 2,
            ..first.clone()
        };

        let key = conversation_history_key(&first);
        assert_eq!(key, conversation_history_key(&second));
        assert!(!key.contains("$first:server"));
        assert!(!key.contains("$second:server"));
    }

    #[test]
    fn matrix_self_anchored_root_history_key_omits_event_id() {
        let first = zeroclaw_api::channel::ChannelMessage {
            id: "$first:server".into(),
            sender: "@alice:server".into(),
            reply_target: "!room:server".into(),
            content: "call me boss".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: Some("$first:server".into()),
            interruption_scope_id: Some("$first:server".into()),
            attachments: vec![],
            subject: None,
        };
        let second = zeroclaw_api::channel::ChannelMessage {
            id: "$second:server".into(),
            content: "hello".into(),
            timestamp: 2,
            thread_ts: Some("$second:server".into()),
            interruption_scope_id: Some("$second:server".into()),
            ..first.clone()
        };

        let key = conversation_history_key(&first);
        assert_eq!(key, conversation_history_key(&second));
        assert!(!key.contains("$first:server"));
        assert!(!key.contains("$second:server"));
    }

    #[test]
    fn matrix_thread_conversation_history_key_uses_thread_root() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "$reply:server".into(),
            sender: "@alice:server".into(),
            reply_target: "!room:server".into(),
            content: "thread reply".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: Some("$root:server".into()),
            interruption_scope_id: Some("$root:server".into()),
            attachments: vec![],
            subject: None,
        };

        let key = conversation_history_key(&msg);
        assert!(key.contains("_root_server"));
        assert!(!key.contains("_reply_server"));
    }

    #[test]
    fn wecom_ws_conversation_history_key_uses_reply_target_scope() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "msg_wecom_ws".into(),
            sender: "zeroclaw_user".into(),
            reply_target: "group--room-1".into(),
            content: "hello".into(),
            channel: "wecom_ws".into(),
            channel_alias: Some("work".into()),
            timestamp: 1,
            thread_ts: Some("req-1".into()),
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        assert_eq!(
            conversation_history_key(&msg),
            "wecom_ws_work_group--room-1"
        );
        assert_eq!(interruption_scope_key(&msg), "wecom_ws_work_group--room-1");
    }

    #[test]
    fn parse_runtime_command_allows_model_switch_for_wecom_ws() {
        assert_eq!(
            parse_runtime_command("wecom_ws", "/models openrouter"),
            Some(ChannelRuntimeCommand::SetProvider("openrouter".into()))
        );
        assert_eq!(
            parse_runtime_command("wecom_ws", "/model qwen-max"),
            Some(ChannelRuntimeCommand::SetModel("qwen-max".into()))
        );
    }

    /// `/models <family>` must resolve to a configured alias-backed ref so the
    /// switched provider uses the alias entry's key/URI — never construct a bare
    /// family provider that ignores `[providers.models.<family>.<alias>]`.
    #[test]
    fn resolve_models_command_resolves_bare_family_to_configured_alias() {
        let mut config = zeroclaw_config::schema::Config::default();
        {
            let base = config
                .providers
                .models
                .ensure("openrouter", "default")
                .expect("openrouter slot must exist");
            base.api_key = Some("sk-configured".into());
            base.uri = Some("https://router.example/v1".into());
            base.model = Some("some-model".into());
        }

        match resolve_models_command(&config, "openrouter") {
            ModelsCommandResolution::Resolved(r) => assert_eq!(r, "openrouter.default"),
            other => panic!("expected Resolved(openrouter.default), got {other:?}"),
        }

        // The resolved ref must carry the configured alias credentials.
        let (key, uri) = provider_credentials_for_ref(&config, "openrouter.default");
        assert_eq!(key.as_deref(), Some("sk-configured"));
        assert_eq!(uri.as_deref(), Some("https://router.example/v1"));
    }

    /// A bare family with no configured alias has no credentialed provider to
    /// switch to — the command must fail clearly instead of building a bare one.
    #[test]
    fn resolve_models_command_rejects_family_without_alias() {
        let config = zeroclaw_config::schema::Config::default();
        match resolve_models_command(&config, "openrouter") {
            ModelsCommandResolution::NoAlias(f) => assert_eq!(f, "openrouter"),
            other => panic!("expected NoAlias(openrouter), got {other:?}"),
        }
    }

    /// A bare family with several configured aliases is ambiguous; the user must
    /// qualify which one rather than silently picking.
    #[test]
    fn resolve_models_command_flags_ambiguous_family() {
        let mut config = zeroclaw_config::schema::Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "default")
            .unwrap();
        config
            .providers
            .models
            .ensure("openrouter", "secondary")
            .unwrap();

        match resolve_models_command(&config, "openrouter") {
            ModelsCommandResolution::Ambiguous { family, aliases } => {
                assert_eq!(family, "openrouter");
                assert_eq!(
                    aliases,
                    vec!["default".to_string(), "secondary".to_string()]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// A dotted ref resolves only when the alias entry exists.
    #[test]
    fn resolve_models_command_accepts_existing_dotted_ref() {
        let mut config = zeroclaw_config::schema::Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "default")
            .unwrap();

        match resolve_models_command(&config, "openrouter.default") {
            ModelsCommandResolution::Resolved(r) => assert_eq!(r, "openrouter.default"),
            other => panic!("expected Resolved, got {other:?}"),
        }
        match resolve_models_command(&config, "openrouter.missing") {
            ModelsCommandResolution::NoAlias(r) => assert_eq!(r, "openrouter.missing"),
            other => panic!("expected NoAlias, got {other:?}"),
        }
    }

    /// An unrecognized family is rejected.
    #[test]
    fn resolve_models_command_rejects_unknown_family() {
        let config = zeroclaw_config::schema::Config::default();
        assert!(matches!(
            resolve_models_command(&config, "definitely-not-a-provider"),
            ModelsCommandResolution::Unknown
        ));
    }

    #[test]
    fn runtime_model_switch_resolves_bare_family_to_configured_alias() {
        let mut config = zeroclaw_config::schema::Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "default")
            .unwrap();

        let resolved = resolve_provider_ref_for_runtime_switch(&config, "openrouter").unwrap();

        assert_eq!(resolved, "openrouter.default");
    }

    #[test]
    fn runtime_model_switch_rejects_ambiguous_bare_family() {
        let mut config = zeroclaw_config::schema::Config::default();
        config
            .providers
            .models
            .ensure("openrouter", "default")
            .unwrap();
        config
            .providers
            .models
            .ensure("openrouter", "secondary")
            .unwrap();

        let err = resolve_provider_ref_for_runtime_switch(&config, "openrouter")
            .expect_err("ambiguous model switch provider should reject");

        assert!(err.to_string().contains("multiple configured aliases"));
    }

    #[test]
    fn explicit_wecom_group_address_bypasses_reply_intent_precheck() {
        assert!(is_explicitly_addressed_channel_message(
            "wecom_ws",
            "[WeCom group message addressed to this bot via @danya]\n@danya say hi"
        ));
        assert!(!is_explicitly_addressed_channel_message(
            "wecom_ws",
            "@danya say hi"
        ));
        assert!(!is_explicitly_addressed_channel_message(
            "telegram",
            "[WeCom group message addressed to this bot via @danya]\n@danya say hi"
        ));
    }

    #[test]
    fn conversation_memory_key_is_unique_per_message() {
        let msg1 = zeroclaw_api::channel::ChannelMessage {
            id: "msg_1".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "first".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let msg2 = zeroclaw_api::channel::ChannelMessage {
            id: "msg_2".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "second".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        assert_ne!(
            conversation_memory_key(&msg1),
            conversation_memory_key(&msg2)
        );
    }

    #[tokio::test]
    async fn autosave_keys_preserve_multiple_conversation_facts() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();

        let msg1 = zeroclaw_api::channel::ChannelMessage {
            id: "msg_1".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "I'm Paul".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let msg2 = zeroclaw_api::channel::ChannelMessage {
            id: "msg_2".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "I'm 45".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        mem.store(
            &conversation_memory_key(&msg1),
            &msg1.content,
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
        mem.store(
            &conversation_memory_key(&msg2),
            &msg2.content,
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let recalled = mem.recall("45", 5, None, None, None).await.unwrap();
        assert!(recalled.iter().any(|entry| entry.content.contains("45")));
    }

    #[tokio::test]
    async fn build_memory_context_includes_recalled_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        mem.store("age_fact", "Age is 45", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        let context = build_memory_context(&mem, "age", 0.0, None).await;
        assert!(context.contains(MEMORY_CONTEXT_OPEN));
        assert!(context.contains("Age is 45"));
    }

    #[tokio::test]
    async fn autosaved_conversation_memory_is_recalled_by_sender_scope() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "msg_1".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "Project codename is quartz".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let history_key = conversation_history_key(&msg);

        mem.store(
            &conversation_memory_key(&msg),
            &msg.content,
            MemoryCategory::Conversation,
            Some(&history_key),
        )
        .await
        .unwrap();

        let session_ids = sender_memory_session_ids(&msg, &history_key);
        let session_id_refs: Vec<Option<&str>> =
            session_ids.iter().map(|s| Some(s.as_str())).collect();
        let context =
            build_memory_context_for_sessions(&mem, "quartz", 0.0, &session_id_refs).await;

        assert!(
            context.contains("Project codename is quartz"),
            "sender recall should include autosaved memories stored under the current session key, got: {context}"
        );
    }

    #[tokio::test]
    async fn autosaved_group_conversation_memory_stays_session_scoped() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let group_a_msg = zeroclaw_api::channel::ChannelMessage {
            id: "msg_1".into(),
            sender: "U123".into(),
            reply_target: "group:alpha".into(),
            content: "Group alpha codename is quartz".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let group_b_msg = zeroclaw_api::channel::ChannelMessage {
            id: "msg_2".into(),
            sender: "U123".into(),
            reply_target: "group:beta".into(),
            content: "What was the codename?".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let group_a_history_key = conversation_history_key(&group_a_msg);
        let group_b_history_key = conversation_history_key(&group_b_msg);

        mem.store(
            &conversation_memory_key(&group_a_msg),
            &group_a_msg.content,
            MemoryCategory::Conversation,
            Some(&group_a_history_key),
        )
        .await
        .unwrap();

        let group_b_sender_session_ids =
            sender_memory_session_ids(&group_b_msg, &group_b_history_key);
        assert_eq!(group_b_sender_session_ids, vec!["U123".to_string()]);

        let group_b_sender_session_id_refs: Vec<Option<&str>> = group_b_sender_session_ids
            .iter()
            .map(|s| Some(s.as_str()))
            .collect();
        let sender_context =
            build_memory_context_for_sessions(&mem, "quartz", 0.0, &group_b_sender_session_id_refs)
                .await;
        let group_context =
            build_memory_context(&mem, "quartz", 0.0, Some(&group_b_history_key)).await;
        let source_group_context =
            build_memory_context(&mem, "quartz", 0.0, Some(&group_a_history_key)).await;

        assert!(
            sender_context.is_empty(),
            "sender scope must not leak autosaved group memory from another group, got: {sender_context}"
        );
        assert!(
            group_context.is_empty(),
            "target group scope must not include another group's autosaved memory, got: {group_context}"
        );
        assert!(
            source_group_context.contains("Group alpha codename is quartz"),
            "source group scope should still recall its own autosaved memory, got: {source_group_context}"
        );
    }

    #[tokio::test]
    async fn sender_session_ids_match_migrated_matrix_sender_rows() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let raw_sender = "@alice:server";
        let sanitized_sender = sanitize_session_key(raw_sender);
        assert_eq!(sanitized_sender, "_alice_server");

        mem.store(
            "alice_fact",
            "Alice favors filtered coffee",
            MemoryCategory::Conversation,
            Some(sanitized_sender.as_str()),
        )
        .await
        .unwrap();

        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "evt_1".into(),
            sender: raw_sender.into(),
            reply_target: "!room:server".into(),
            content: "what coffee does alice prefer?".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        let history_key = conversation_history_key(&msg);
        let session_ids = sender_memory_session_ids(&msg, &history_key);
        assert!(
            session_ids.contains(&sanitized_sender),
            "sender session ids must include sanitized sender, got: {session_ids:?}"
        );
        let session_id_refs: Vec<Option<&str>> =
            session_ids.iter().map(|s| Some(s.as_str())).collect();
        let context =
            build_memory_context_for_sessions(&mem, "coffee", 0.0, &session_id_refs).await;
        assert!(
            context.contains("Alice favors filtered coffee"),
            "sender recall must find migrated row stored under sanitized sender, got: {context}"
        );
    }

    /// Auto-saved photo messages must not surface through memory context,
    /// otherwise the image marker gets duplicated in the model_provider request.
    #[tokio::test]
    async fn build_memory_context_excludes_image_marker_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();

        // Simulate auto-save of a photo message containing an [IMAGE:] marker.
        mem.store(
            "telegram_user_msg_photo",
            "[IMAGE:/tmp/workspace/photo_1_2.jpg]\n\nDescribe this screenshot",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
        // Also store a plain text entry that shares a word with the query
        // so the FTS recall returns both entries.
        mem.store(
            "screenshot_preference",
            "User prefers screenshot descriptions to be concise",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_memory_context(&mem, "screenshot", 0.0, None).await;

        // The image-marker entry must be excluded to prevent duplication.
        assert!(
            !context.contains("[IMAGE:"),
            "memory context must not contain image markers, got: {context}"
        );
        // Plain text entries should still be included.
        assert!(
            context.contains("screenshot descriptions"),
            "plain text entry should remain in context, got: {context}"
        );
    }

    #[tokio::test]
    async fn process_channel_message_restores_per_sender_history_on_follow_ups() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureModelProvider::default());

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-a".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-b".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "follow up".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].len(), 2);
        assert_eq!(calls[0][0].0, "system");
        assert_eq!(calls[0][1].0, "user");
        assert_eq!(calls[1].len(), 4);
        assert_eq!(calls[1][0].0, "system");
        assert_eq!(calls[1][1].0, "user");
        assert_eq!(calls[1][2].0, "assistant");
        assert_eq!(calls[1][3].0, "user");
        assert!(calls[1][1].1.starts_with('['));
        assert!(calls[1][1].1.contains("hello"));
        assert!(calls[1][2].1.contains("response-1"));
        assert!(calls[1][3].1.starts_with('['));
        assert!(calls[1][3].1.contains("follow up"));
    }

    #[tokio::test]
    async fn process_channel_message_refreshes_available_skills_after_new_session() {
        let workspace = make_workspace();
        let mut config = Config {
            data_dir: workspace.path().to_path_buf(),
            ..Default::default()
        };
        config.skills.open_skills_enabled = false;

        let initial_skills =
            zeroclaw_runtime::skills::load_skills_with_config(workspace.path(), &config);
        assert!(initial_skills.is_empty());

        let default_identity = zeroclaw_config::schema::IdentityConfig::default();
        let initial_system_prompt = build_system_prompt_with_mode(
            workspace.path(),
            "test-model",
            &[],
            &initial_skills,
            Some(&default_identity),
            None,
            false,
            config.skills.prompt_injection_mode,
            AutonomyLevel::default(),
        );
        assert!(
            !initial_system_prompt.contains("refresh-test"),
            "initial prompt should not contain the new skill before it exists"
        );

        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new(initial_system_prompt),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(config.data_dir.clone()),
            prompt_config: Arc::new(config.clone()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-before-new".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-refresh".to_string(),
                content: "hello".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let skill_dir = workspace.path().join("skills").join("refresh-test");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: refresh-test\ndescription: Refresh the available skills section\n---\n# Refresh Test\nExpose this skill after /new.\n",
        )
        .unwrap();
        let refreshed_skills =
            zeroclaw_runtime::skills::load_skills_with_config(workspace.path(), &config);
        assert_eq!(refreshed_skills.len(), 1);
        assert_eq!(refreshed_skills[0].name, "refresh-test");
        assert!(
            refreshed_new_session_system_prompt(runtime_ctx.as_ref())
                .contains("<name>refresh-test</name>"),
            "fresh-session prompt should pick up skills added after startup"
        );

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-new-session".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-refresh".to_string(),
                content: "/new".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        {
            let histories = runtime_ctx
                .conversation_histories
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert!(
                histories.peek("telegram_chat-refresh_alice").is_none(),
                "/new should clear the cached sender history before the next message"
            );
        }

        {
            let pending_new_sessions = runtime_ctx
                .pending_new_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert!(
                pending_new_sessions.contains("telegram_chat-refresh_alice"),
                "/new should mark the sender for a fresh next-message prompt rebuild"
            );
        }

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-after-new".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-refresh".to_string(),
                content: "hello again".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 3,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        {
            let calls = provider_impl
                .calls
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0][0].0, "system");
            assert_eq!(calls[1][0].0, "system");
            assert!(
                !calls[0][0].1.contains("<name>refresh-test</name>"),
                "pre-/new prompt should not advertise a skill that did not exist yet"
            );
            assert!(
                calls[1][0].1.contains("<available_skills>"),
                "post-/new prompt should contain the refreshed skills block"
            );
            assert!(
                calls[1][0].1.contains("<name>refresh-test</name>"),
                "post-/new prompt should include skills discovered after the reset"
            );
        }

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert!(
            sent_messages.iter().any(|message| {
                message.contains("Conversation history cleared. Starting fresh.")
            })
        );
    }

    #[tokio::test]
    async fn process_channel_message_enriches_current_turn_without_persisting_context() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(RecallMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(RecallMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-ctx-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-ctx".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 2);
        // Memory context is injected into the system prompt, not the user message.
        assert_eq!(calls[0][0].0, "system");
        assert!(calls[0][0].1.contains(MEMORY_CONTEXT_OPEN));
        assert!(calls[0][0].1.contains("Age is 45"));
        assert_eq!(calls[0][1].0, "user");
        assert!(calls[0][1].1.starts_with('['));
        assert!(
            calls[0][1].1.contains("] hello"),
            "current channel user turn should be timestamped: {}",
            calls[0][1].1
        );

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .peek("test-channel_chat-ctx_alice")
            .expect("history should be stored for sender");
        assert_eq!(turns[0].role, "user");
        assert!(turns[0].content.starts_with('['));
        assert!(
            turns[0].content.contains("] hello"),
            "stored channel user turn should be timestamped: {}",
            turns[0].content
        );
        assert!(!turns[0].content.contains(MEMORY_CONTEXT_OPEN));
    }

    #[tokio::test]
    async fn process_channel_message_sends_image_payload_without_persisting_it() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureModelProvider {
            vision: true,
            ..Default::default()
        });
        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig {
                enabled: true,
                ..Default::default()
            },
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-image-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-image".to_string(),
                content: "please inspect this".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![zeroclaw_api::media::MediaAttachment {
                    file_name: "sticker.png".to_string(),
                    data: vec![1, 2, 3, 4],
                    mime_type: Some("image/png".to_string()),
                }],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 1);
        let current_user = calls[0]
            .iter()
            .rev()
            .find(|(role, _)| role == "user")
            .expect("provider call should include current user message");
        assert!(current_user.1.contains("[IMAGE:data:image/png;base64,"));
        assert!(current_user.1.contains("please inspect this"));
        drop(calls);

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .peek("test-channel_chat-image_alice")
            .expect("history should be stored for sender");
        assert_eq!(turns[0].role, "user");
        assert!(turns[0].content.starts_with('['));
        assert!(turns[0].content.contains("[Image: sticker.png attached"));
        assert!(turns[0].content.contains("please inspect this"));
        assert!(!turns[0].content.contains("[IMAGE:data:"));
        assert!(!turns[0].content.contains("AQIDBA"));
    }

    #[tokio::test]
    async fn process_channel_message_telegram_keeps_system_instruction_at_top_only() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
        let mut histories =
            lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
        histories.push(
            "telegram_chat-telegram_alice".to_string(),
            vec![
                ChatMessage::assistant("stale assistant"),
                ChatMessage::user("earlier user question"),
                ChatMessage::assistant("earlier assistant reply"),
            ],
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: provider_impl.clone(),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx.clone(),
            zeroclaw_api::channel::ChannelMessage {
                id: "tg-msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-telegram".to_string(),
                content: "hello".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 4);

        let roles = calls[0]
            .iter()
            .map(|(role, _)| role.as_str())
            .collect::<Vec<_>>();
        assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
        assert!(
            calls[0][0].1.contains("When responding on Telegram:"),
            "telegram channel instructions should be embedded into the system prompt"
        );
        assert!(
            calls[0][0].1.contains("For media attachments use markers:"),
            "telegram media marker guidance should live in the system prompt"
        );
        assert!(!calls[0].iter().skip(1).any(|(role, _)| role == "system"));
    }

    #[test]
    fn channel_delivery_instructions_for_discord_mandates_absolute_paths() {
        let block = channel_delivery_instructions("discord")
            .expect("discord channel must have a delivery-instructions block");
        assert!(
            block.contains("When responding on Discord:"),
            "discord block must identify itself"
        );
        assert!(
            block.contains("For media attachments use markers:"),
            "discord block must describe marker syntax"
        );
        assert!(
            block.contains("MUST be absolute"),
            "discord block must mandate absolute paths"
        );
        assert!(
            block.contains("workspace"),
            "discord block must reference workspace bounds"
        );
        assert!(
            block.contains("[IMAGE:<absolute-path>]"),
            "discord block must show the absolute-path marker form"
        );
    }

    #[test]
    fn extract_tool_context_summary_collects_alias_and_native_tool_calls() {
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant(
                r#"<toolcall>
{"name":"shell","arguments":{"command":"date"}}
</toolcall>"#,
            ),
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"1","name":"web_search","arguments":"{}"}]}"#,
            ),
        ];

        let summary = extract_tool_context_summary(&history, 1);
        assert_eq!(summary, "[Used tools: shell, web_search]");
    }

    #[test]
    fn extract_tool_context_summary_collects_prompt_mode_tool_result_names() {
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("Using markdown tool call fence"),
            ChatMessage::user(
                r#"[Tool results]
<tool_result name="http_request">
{"status":200}
</tool_result>
<tool_result name="shell">
Mon Feb 20
</tool_result>"#,
            ),
        ];

        let summary = extract_tool_context_summary(&history, 1);
        assert_eq!(summary, "[Used tools: http_request, shell]");
    }

    #[test]
    fn extract_tool_context_summary_respects_start_index() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"stale_tool","arguments":{}}
</tool_call>"#,
            ),
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"fresh_tool","arguments":{}}
</tool_call>"#,
            ),
        ];

        let summary = extract_tool_context_summary(&history, 1);
        assert_eq!(summary, "[Used tools: fresh_tool]");
    }

    #[test]
    fn strip_isolated_tool_json_artifacts_removes_tool_calls_and_results() {
        let mut known_tools = HashSet::new();
        known_tools.insert("schedule".to_string());

        let input = r#"{"name":"schedule","parameters":{"action":"create","message":"test"}}
{"name":"schedule","parameters":{"action":"cancel","task_id":"test"}}
Let me create the reminder properly:
{"name":"schedule","parameters":{"action":"create","message":"Go to sleep"}}
{"result":{"task_id":"abc","status":"scheduled"}}
Done reminder set for 1:38 AM."#;

        let result = strip_isolated_tool_json_artifacts(input, &known_tools);
        let normalized = result
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            normalized,
            "Let me create the reminder properly:\nDone reminder set for 1:38 AM."
        );
    }

    #[test]
    fn strip_isolated_tool_json_artifacts_preserves_non_tool_json() {
        let mut known_tools = HashSet::new();
        known_tools.insert("shell".to_string());

        let input = r#"{"name":"profile","parameters":{"timezone":"UTC"}}
This is an example JSON object for profile settings."#;

        let result = strip_isolated_tool_json_artifacts(input, &known_tools);
        assert_eq!(result, input);
    }

    // ── AIEOS Identity Tests (Issue #168) ─────────────────────────

    #[test]
    fn aieos_identity_from_file() {
        use tempfile::TempDir;
        use zeroclaw_config::schema::IdentityConfig;

        let tmp = TempDir::new().unwrap();
        let identity_path = tmp.path().join("aieos_identity.json");

        // Write AIEOS identity file
        let aieos_json = r#"{
            "identity": {
                "names": {"first": "Nova", "nickname": "Nov"},
                "bio": "A helpful AI assistant.",
                "origin": "Silicon Valley"
            },
            "psychology": {
                "mbti": "INTJ",
                "moral_compass": ["Be helpful", "Do no harm"]
            },
            "linguistics": {
                "style": "concise",
                "formality": "casual"
            }
        }"#;
        std::fs::write(&identity_path, aieos_json).unwrap();

        // Create identity config pointing to the file
        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: Some("aieos_identity.json".into()),
            aieos_inline: None,
        };

        let prompt = build_system_prompt(tmp.path(), "model", &[], &[], Some(&config), None);

        // Should contain AIEOS sections
        assert!(prompt.contains("## Identity"));
        assert!(prompt.contains("**Name:** Nova"));
        assert!(prompt.contains("**Nickname:** Nov"));
        assert!(prompt.contains("**Bio:** A helpful AI assistant."));
        assert!(prompt.contains("**Origin:** Silicon Valley"));

        assert!(prompt.contains("## Personality"));
        assert!(prompt.contains("**MBTI:** INTJ"));
        assert!(prompt.contains("**Moral Compass:**"));
        assert!(prompt.contains("- Be helpful"));

        assert!(prompt.contains("## Communication Style"));
        assert!(prompt.contains("**Style:** concise"));
        assert!(prompt.contains("**Formality Level:** casual"));

        // Should NOT contain OpenClaw bootstrap file headers
        assert!(!prompt.contains("### SOUL.md"));
        assert!(!prompt.contains("### IDENTITY.md"));
        assert!(!prompt.contains("[File not found"));
    }

    #[test]
    fn aieos_identity_from_inline() {
        use zeroclaw_config::schema::IdentityConfig;

        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: None,
            aieos_inline: Some(r#"{"identity":{"names":{"first":"Claw"}}}"#.into()),
        };

        let prompt = build_system_prompt(
            std::env::temp_dir().as_path(),
            "model",
            &[],
            &[],
            Some(&config),
            None,
        );

        assert!(prompt.contains("**Name:** Claw"));
        assert!(prompt.contains("## Identity"));
    }

    #[test]
    fn aieos_fallback_to_openclaw_on_parse_error() {
        use zeroclaw_config::schema::IdentityConfig;

        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: Some("nonexistent.json".into()),
            aieos_inline: None,
        };

        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

        // Should fall back to OpenClaw format when AIEOS file is not found
        // (Error is logged to stderr with filename, not included in prompt)
        assert!(prompt.contains("### SOUL.md"));
    }

    #[test]
    fn aieos_empty_uses_openclaw() {
        use zeroclaw_config::schema::IdentityConfig;

        // Format is "aieos" but neither path nor inline is set
        let config = IdentityConfig {
            format: "aieos".into(),
            aieos_path: None,
            aieos_inline: None,
        };

        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

        // Should use OpenClaw format (not configured for AIEOS)
        assert!(prompt.contains("### SOUL.md"));
        assert!(prompt.contains("Be helpful"));
    }

    #[test]
    fn openclaw_format_uses_bootstrap_files() {
        use zeroclaw_config::schema::IdentityConfig;

        let config = IdentityConfig {
            format: "openclaw".into(),
            aieos_path: Some("identity.json".into()),
            aieos_inline: None,
        };

        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

        // Should use OpenClaw format even if aieos_path is set
        assert!(prompt.contains("### SOUL.md"));
        assert!(prompt.contains("Be helpful"));
        assert!(!prompt.contains("## Identity"));
    }

    #[test]
    fn none_identity_config_uses_openclaw() {
        let ws = make_workspace();
        // Pass None for identity config
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        // Should use OpenClaw format
        assert!(prompt.contains("### SOUL.md"));
        assert!(prompt.contains("Be helpful"));
    }

    #[test]
    fn classify_health_ok_true() {
        let state = classify_health_result(&Ok(true));
        assert_eq!(state, ChannelHealthState::Healthy);
    }

    #[test]
    fn classify_health_ok_false() {
        let state = classify_health_result(&Ok(false));
        assert_eq!(state, ChannelHealthState::Unhealthy);
    }

    #[tokio::test]
    async fn classify_health_timeout() {
        let result = tokio::time::timeout(Duration::from_millis(1), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            true
        })
        .await;
        let state = classify_health_result(&result);
        assert_eq!(state, ChannelHealthState::Timeout);
    }

    #[cfg(feature = "channel-matrix")]
    #[test]
    fn matrix_state_dir_is_distinct_per_alias() {
        // Regression: two [channels.matrix.<alias>] blocks previously resolved
        // to the same <config>/state/matrix dir, so the second listener to
        // start restored the first's session.json and ran as the wrong Matrix
        // account. The alias component must keep them separate.
        let config_path = std::path::Path::new("/home/u/.zeroclaw/config.toml");
        let clamps = matrix_state_dir(config_path, "clamps");
        let bender = matrix_state_dir(config_path, "bender");
        assert_ne!(
            clamps, bender,
            "distinct matrix aliases must not share a state dir"
        );
        assert_eq!(
            clamps,
            std::path::Path::new("/home/u/.zeroclaw/state/matrix/clamps")
        );
        assert_eq!(
            bender,
            std::path::Path::new("/home/u/.zeroclaw/state/matrix/bender")
        );
    }

    #[cfg(feature = "channel-mattermost")]
    #[test]
    fn collect_configured_channels_includes_mattermost_when_configured() {
        let mut config = Config::default();
        config.channels.mattermost.insert(
            "default".to_string(),
            zeroclaw_config::schema::MattermostConfig {
                enabled: true,
                url: "https://mattermost.example.com".to_string(),
                bot_token: Some("test-token".to_string()),
                login_id: None,
                password: None,
                channel_ids: vec!["channel-1".to_string()],
                team_ids: vec![],
                discover_dms: None,
                thread_replies: Some(true),
                mention_only: Some(false),
                interrupt_on_new_message: false,
                proxy_url: None,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        // A channel is only collected when an enabled agent references it.
        config.agents.insert(
            "mattermost-default".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                channels: vec!["mattermost.default".into()],
                ..Default::default()
            },
        );

        let config_arc = Arc::new(RwLock::new(config));
        let channels = collect_configured_channels(&config_arc, "test", &[]);

        assert!(
            channels
                .iter()
                .any(|entry| entry.display_name == "Mattermost")
        );
        assert!(
            channels
                .iter()
                .any(|entry| entry.channel.name() == "mattermost")
        );
    }

    #[cfg(feature = "channel-mattermost")]
    #[test]
    fn collect_configured_channels_falls_back_when_agent_bindings_missing() {
        let mut config = Config::default();
        config.channels.mattermost.insert(
            "default".to_string(),
            zeroclaw_config::schema::MattermostConfig {
                enabled: true,
                url: "https://mattermost.example.com".to_string(),
                bot_token: Some("test-token".to_string()),
                login_id: None,
                password: None,
                channel_ids: vec!["channel-1".to_string()],
                team_ids: vec![],
                discover_dms: None,
                thread_replies: Some(true),
                mention_only: Some(false),
                interrupt_on_new_message: false,
                proxy_url: None,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        config.agents.clear();
        config.agents.insert(
            "legacy".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                enabled: true,
                channels: vec![],
                ..Default::default()
            },
        );

        let config_arc = Arc::new(RwLock::new(config));
        let channels = collect_configured_channels(&config_arc, "test", &[]);

        assert!(
            channels
                .iter()
                .any(|entry| entry.display_name == "Mattermost"),
            "enabled channels should still load when no enabled agent declares channel bindings"
        );
    }

    #[cfg(feature = "channel-email")]
    #[test]
    fn collect_configured_channels_skips_unreferenced_email() {
        let mut config = Config::default();
        config.channels.email.insert(
            "default".to_string(),
            zeroclaw_config::scattered_types::EmailConfig::default(),
        );

        let config_arc = Arc::new(RwLock::new(config));
        let channels = collect_configured_channels(&config_arc, "test", &[]);
        assert!(
            !channels.iter().any(|entry| entry.display_name == "Email"),
            "email with no agent reference should not be collected"
        );
    }

    #[cfg(feature = "channel-voice-call")]
    #[test]
    fn collect_configured_channels_skips_unreferenced_voice_call() {
        let mut config = Config::default();
        config.channels.voice_call.insert(
            "default".to_string(),
            zeroclaw_config::scattered_types::VoiceCallConfig::default(),
        );

        let config_arc = Arc::new(RwLock::new(config));
        let channels = collect_configured_channels(&config_arc, "test", &[]);
        assert!(
            !channels
                .iter()
                .any(|entry| entry.display_name == "Voice Call"),
            "voice-call with no agent reference should not be collected"
        );
    }

    struct AlwaysFailChannel {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    struct BlockUntilClosedChannel {
        name: String,
        calls: Arc<AtomicUsize>,
    }

    struct FailOnceChannel {
        name: String,
        calls: Arc<AtomicUsize>,
        err: Mutex<Option<anyhow::Error>>,
    }

    impl ::zeroclaw_api::attribution::Attributable for AlwaysFailChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait::async_trait]
    impl Channel for AlwaysFailChannel {
        fn name(&self) -> &str {
            self.name
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("listen boom")
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for BlockUntilClosedChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for FailOnceChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Discord,
            )
        }

        fn alias(&self) -> &str {
            "default"
        }
    }

    #[async_trait::async_trait]
    impl Channel for BlockUntilClosedChannel {
        fn name(&self) -> &str {
            &self.name
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tx.closed().await;
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl Channel for FailOnceChannel {
        fn name(&self) -> &str {
            &self.name
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = self.err.lock().unwrap_or_else(|e| e.into_inner()).take() {
                return Err(err);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn supervised_listener_marks_error_and_restarts_on_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let channel: Arc<dyn Channel> = Arc::new(AlwaysFailChannel {
            name: "test-supervised-fail",
            calls: Arc::clone(&calls),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

        tokio::time::sleep(Duration::from_millis(80)).await;
        drop(rx);
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_millis(500), handle).await;

        let snapshot = zeroclaw_runtime::health::snapshot_json();
        let component = &snapshot["components"]["channel:test-supervised-fail"];
        assert_eq!(component["status"], "error");
        assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
        assert!(
            component["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("listen boom")
        );
        assert!(calls.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn supervised_listener_refreshes_health_while_running() {
        let calls = Arc::new(AtomicUsize::new(0));
        let channel_name = format!("test-supervised-heartbeat-{}", uuid::Uuid::new_v4());
        let component_name = format!("channel:{channel_name}");
        let channel: Arc<dyn Channel> = Arc::new(BlockUntilClosedChannel {
            name: channel_name,
            calls: Arc::clone(&calls),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = spawn_supervised_listener_with_health_interval(
            channel,
            None,
            tx,
            1,
            1,
            Duration::from_millis(20),
            cancel.clone(),
        );

        tokio::time::sleep(Duration::from_millis(35)).await;
        let first_last_ok = zeroclaw_runtime::health::snapshot_json()["components"]
            [&component_name]["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(!first_last_ok.is_empty());

        tokio::time::sleep(Duration::from_millis(70)).await;
        let second_last_ok = zeroclaw_runtime::health::snapshot_json()["components"]
            [&component_name]["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let first = chrono::DateTime::parse_from_rfc3339(&first_last_ok)
            .expect("last_ok should be valid RFC3339");
        let second = chrono::DateTime::parse_from_rfc3339(&second_last_ok)
            .expect("last_ok should be valid RFC3339");
        assert!(second > first, "expected periodic health heartbeat refresh");

        cancel.cancel();
        let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(join.is_ok(), "listener should stop on cancel");
        assert!(calls.load(Ordering::SeqCst) >= 1);
        drop(rx);
    }

    #[tokio::test]
    async fn supervised_listener_does_not_restart_on_non_retryable_discord_http_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let channel_name = format!("discord-{}", uuid::Uuid::new_v4());
        let channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
            name: channel_name,
            calls: Arc::clone(&calls),
            err: Mutex::new(Some(anyhow::Error::msg("401 Unauthorized"))),
        });

        let component_name = format!("channel:{}", channel.name());
        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

        tokio::time::sleep(Duration::from_millis(80)).await;
        let snapshot = zeroclaw_runtime::health::snapshot_json();
        let component = &snapshot["components"][&component_name];
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(component["status"], "error");
        assert_eq!(component["restart_count"].as_u64().unwrap_or(0), 0);
        assert!(
            component["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("401 Unauthorized")
        );

        drop(rx);
        cancel.cancel();
        let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(join.is_ok(), "listener should stop on cancel");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "channel-discord")]
    #[tokio::test]
    async fn supervised_listener_enters_retry_path_on_discord_gateway_rate_limit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let channel_name = format!("discord-{}", uuid::Uuid::new_v4());
        let channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
            name: channel_name,
            calls: Arc::clone(&calls),
            err: Mutex::new(Some(anyhow::Error::msg(
                "discord gateway preflight rate-limited (429 Too Many Requests)",
            ))),
        });

        let component_name = format!("channel:{}", channel.name());
        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

        tokio::time::sleep(Duration::from_millis(80)).await;
        let snapshot = zeroclaw_runtime::health::snapshot_json();
        let component = &snapshot["components"][&component_name];
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(component["status"], "error");
        assert!(
            component["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("429 Too Many Requests")
        );
        assert!(
            component["restart_count"].as_u64().unwrap_or(0) >= 1,
            "Discord gateway 429 should back off through the retry path instead of parking"
        );

        drop(rx);
        cancel.cancel();
        let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(join.is_ok(), "listener should stop on cancel");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "channel-discord")]
    #[tokio::test]
    async fn supervised_listener_does_not_restart_on_fatal_discord_gateway_close_code() {
        let calls = Arc::new(AtomicUsize::new(0));
        let channel_name = format!("discord-{}", uuid::Uuid::new_v4());
        let channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
            name: channel_name,
            calls: Arc::clone(&calls),
            err: Mutex::new(Some(anyhow::Error::new(
                crate::discord::DiscordListenerFatalError::new(
                    "discord gateway closed with fatal code 4014: disallowed intent(s)",
                ),
            ))),
        });

        let component_name = format!("channel:{}", channel.name());
        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

        tokio::time::sleep(Duration::from_millis(80)).await;
        let snapshot = zeroclaw_runtime::health::snapshot_json();
        let component = &snapshot["components"][&component_name];
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(component["status"], "error");
        assert_eq!(component["restart_count"].as_u64().unwrap_or(0), 0);
        assert!(
            component["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("fatal code 4014")
        );

        drop(rx);
        cancel.cancel();
        let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(join.is_ok(), "listener should stop on cancel");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_retryable_listener_error_does_not_stop_other_listener_health() {
        let failing_calls = Arc::new(AtomicUsize::new(0));
        let healthy_calls = Arc::new(AtomicUsize::new(0));
        let failing_name = format!("discord-{}", uuid::Uuid::new_v4());
        let healthy_name = format!("test-supervised-sibling-{}", uuid::Uuid::new_v4());
        let failing_component = format!("channel:{failing_name}");
        let healthy_component = format!("channel:{healthy_name}");

        let failing_channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
            name: failing_name,
            calls: Arc::clone(&failing_calls),
            err: Mutex::new(Some(anyhow::Error::msg("401 Unauthorized"))),
        });
        let healthy_channel: Arc<dyn Channel> = Arc::new(BlockUntilClosedChannel {
            name: healthy_name,
            calls: Arc::clone(&healthy_calls),
        });

        let (failing_tx, failing_rx) =
            tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let (healthy_tx, healthy_rx) =
            tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
        let cancel = tokio_util::sync::CancellationToken::new();
        let failing_handle =
            spawn_supervised_listener(failing_channel, None, failing_tx, 1, 1, cancel.clone());
        let healthy_handle = spawn_supervised_listener_with_health_interval(
            healthy_channel,
            None,
            healthy_tx,
            1,
            1,
            Duration::from_millis(20),
            cancel.clone(),
        );

        tokio::time::sleep(Duration::from_millis(80)).await;

        let first_last_ok = zeroclaw_runtime::health::snapshot_json()["components"]
            [&healthy_component]["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(
            !first_last_ok.is_empty(),
            "healthy sibling should report health"
        );

        tokio::time::sleep(Duration::from_millis(70)).await;

        let snapshot = zeroclaw_runtime::health::snapshot_json();
        let failing = &snapshot["components"][&failing_component];
        let healthy = &snapshot["components"][&healthy_component];
        let second_last_ok = healthy["last_ok"].as_str().unwrap_or("").to_string();
        let first = chrono::DateTime::parse_from_rfc3339(&first_last_ok)
            .expect("healthy sibling last_ok should be valid RFC3339");
        let second = chrono::DateTime::parse_from_rfc3339(&second_last_ok)
            .expect("healthy sibling last_ok should be valid RFC3339");

        assert_eq!(failing_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failing["status"], "error");
        assert_eq!(failing["restart_count"].as_u64().unwrap_or(0), 0);
        assert!(
            failing["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("401 Unauthorized")
        );
        assert_eq!(healthy["status"], "ok");
        assert!(
            second > first,
            "healthy sibling should keep refreshing health"
        );
        assert!(healthy_calls.load(Ordering::SeqCst) >= 1);

        drop(failing_rx);
        drop(healthy_rx);
        cancel.cancel();
        let failing_join = tokio::time::timeout(Duration::from_millis(500), failing_handle).await;
        let healthy_join = tokio::time::timeout(Duration::from_millis(500), healthy_handle).await;
        assert!(
            failing_join.is_ok(),
            "non-retryable listener should stop on cancel"
        );
        assert!(
            healthy_join.is_ok(),
            "healthy sibling listener should stop on cancel"
        );
    }

    #[test]
    fn maybe_restart_daemon_systemd_args_regression() {
        assert_eq!(
            SYSTEMD_STATUS_ARGS,
            ["--user", "is-active", "zeroclaw.service"]
        );
        assert_eq!(
            SYSTEMD_RESTART_ARGS,
            ["--user", "restart", "zeroclaw.service"]
        );
    }

    #[test]
    fn maybe_restart_daemon_openrc_args_regression() {
        assert_eq!(OPENRC_STATUS_ARGS, ["zeroclaw", "status"]);
        assert_eq!(OPENRC_RESTART_ARGS, ["zeroclaw", "restart"]);
    }

    #[test]
    fn normalize_merges_consecutive_user_turns() {
        let turns = vec![ChatMessage::user("hello"), ChatMessage::user("world")];
        let result = normalize_cached_channel_turns(turns);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "hello\n\nworld");
    }

    #[test]
    fn normalize_preserves_strict_alternation() {
        let turns = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
            ChatMessage::user("bye"),
        ];
        let result = normalize_cached_channel_turns(turns);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[1].content, "hi");
        assert_eq!(result[2].content, "bye");
    }

    #[test]
    fn normalize_merges_multiple_consecutive_user_turns() {
        let turns = vec![
            ChatMessage::user("a"),
            ChatMessage::user("b"),
            ChatMessage::user("c"),
        ];
        let result = normalize_cached_channel_turns(turns);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "a\n\nb\n\nc");
    }

    #[test]
    fn normalize_empty_input() {
        let result = normalize_cached_channel_turns(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn channel_history_content_for_user_turn_strips_inline_image_payload() {
        let content =
            "[Image: sticker.webp attached]\n[IMAGE:data:image/png;base64,abcd]\n\nwhat is this?";

        let history_content = channel_history_content_for_user_turn(content);

        assert_eq!(
            history_content,
            "[Image: sticker.webp attached]\n\nwhat is this?"
        );
        assert!(!history_content.contains("[IMAGE:data:"));
        assert!(!history_content.contains("abcd"));
    }

    #[test]
    fn channel_history_content_for_image_only_turn_keeps_compact_placeholder() {
        let history_content =
            channel_history_content_for_user_turn("[IMAGE:data:image/png;base64,abcd]");

        assert_eq!(
            history_content,
            "[Image attachment processed by vision model]"
        );
    }

    #[test]
    fn strip_historical_image_payloads_preserves_current_turn_payload() {
        let mut turns = vec![
            ChatMessage::user("[Image: old.png attached]\n[IMAGE:data:image/png;base64,old]"),
            ChatMessage::assistant("I saw the old image."),
            ChatMessage::user("[Image: now.png attached]\n[IMAGE:data:image/png;base64,current]"),
        ];

        strip_historical_image_payloads(&mut turns);

        assert_eq!(turns.len(), 3);
        assert!(turns[0].content.contains("[Image: old.png attached]"));
        assert!(!turns[0].content.contains("[IMAGE:data:"));
        assert!(!turns[0].content.contains("base64,old"));
        assert!(
            turns[2]
                .content
                .contains("[IMAGE:data:image/png;base64,current]")
        );
    }

    // ── E2E: photo [IMAGE:] marker rejected by non-vision model_provider ───

    /// End-to-end test: a photo attachment message (containing `[IMAGE:]`
    /// marker) sent through `process_channel_message` with a non-vision
    /// model_provider must produce a `"⚠️ Error: …does not support vision"` reply
    /// on the recording channel — no real Telegram or LLM API required.
    #[tokio::test]
    async fn e2e_photo_attachment_rejected_by_non_vision_provider() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        // DummyModelProvider has default capabilities (vision: false).
        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("dummy".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("You are a helpful assistant.".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        // Simulate a photo attachment message with [IMAGE:] marker.
        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-photo-1".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-photo".to_string(),
                content: "[IMAGE:/tmp/workspace/photo_99_1.jpg]\n\nWhat is this?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1, "expected exactly one reply message");
        assert!(
            sent[0].contains("does not support vision"),
            "reply must mention vision capability error, got: {}",
            sent[0]
        );
        assert!(
            sent[0].contains("⚠️ Error"),
            "reply must start with error prefix, got: {}",
            sent[0]
        );
    }

    #[tokio::test]
    async fn e2e_failed_vision_turn_does_not_poison_follow_up_text_turn() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(DummyModelProvider),
            model_provider_ref: Arc::new("dummy".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("You are a helpful assistant.".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            Arc::clone(&runtime_ctx),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-photo-1".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-photo".to_string(),
                content: "[IMAGE:/tmp/workspace/photo_99_1.jpg]\n\nWhat is this?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        process_channel_message(
            Arc::clone(&runtime_ctx),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-text-2".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-photo".to_string(),
                content: "What is WAL?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 2, "expected one error and one successful reply");
        assert!(
            sent[0].contains("does not support vision"),
            "first reply must mention vision capability error, got: {}",
            sent[0]
        );
        assert!(
            sent[1].ends_with(":ok"),
            "second reply should succeed for text-only turn, got: {}",
            sent[1]
        );
        drop(sent);

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .peek("test-channel_chat-photo_zeroclaw_user")
            .expect("history should exist for sender");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert!(
            turns[0].content.contains("] What is WAL?"),
            "follow-up user turn should be timestamped: {}",
            turns[0].content
        );
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].content, "ok");
        assert!(
            turns.iter().all(|turn| !turn.content.contains("[IMAGE:")),
            "failed vision turn must not persist image marker content"
        );
    }

    #[tokio::test]
    async fn e2e_failed_non_retryable_turn_does_not_poison_follow_up_text_turn() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(FormatErrorModelProvider),
            model_provider_ref: Arc::new("dummy".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("You are a helpful assistant.".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 50000,
            context_token_budget: 128_000,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                std::time::Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
        });

        process_channel_message(
            Arc::clone(&runtime_ctx),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-bad-1".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-format".to_string(),
                content: "trigger format error".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        process_channel_message(
            Arc::clone(&runtime_ctx),
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-text-2".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-format".to_string(),
                content: "What is WAL?".to_string(),
                channel: "test-channel".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 2, "expected one error and one successful reply");
        assert!(
            sent[0].contains("Format Error"),
            "first reply must mention the request format error, got: {}",
            sent[0]
        );
        assert!(
            sent[1].ends_with(":ok"),
            "second reply should succeed for follow-up text, got: {}",
            sent[1]
        );
        drop(sent);

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .peek("test-channel_chat-format_zeroclaw_user")
            .expect("history should exist for sender");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert!(
            turns[0].content.contains("] What is WAL?"),
            "follow-up user turn should be timestamped: {}",
            turns[0].content
        );
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].content, "ok");
        assert!(
            turns
                .iter()
                .all(|turn| turn.content != "trigger format error"),
            "failed non-retryable turn must not persist in history"
        );
    }

    #[test]
    fn build_channel_by_id_unknown_channel_returns_error() {
        let config = Config::default();
        let config_arc = Arc::new(RwLock::new(config));
        match build_channel_by_id(&config_arc, "nonexistent") {
            Err(e) => {
                let err_msg = e.to_string();
                assert!(
                    err_msg.contains("Unknown channel"),
                    "expected 'Unknown channel' in error, got: {err_msg}"
                );
            }
            Ok(_) => panic!("should fail for unknown channel"),
        }
    }

    // ── Query classification in channel message processing ─────────

    #[tokio::test]
    async fn process_channel_message_applies_query_classification_route() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
        let vision_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let vision_model_provider: Arc<dyn ModelProvider> = vision_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(
            "test-provider".to_string(),
            Arc::clone(&agent_model_provider),
        );
        provider_cache_seed.insert("vision-provider".to_string(), vision_model_provider);

        let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
            enabled: true,
            rules: vec![zeroclaw_config::schema::ClassificationRule {
                hint: "vision".into(),
                keywords: vec!["analyze-image".into()],
                ..Default::default()
            }],
        };

        let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
            hint: "vision".into(),
            model_provider: "vision-provider".into(),
            model: "gpt-4-vision".into(),
            api_key: None,
        }];

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&agent_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(model_routes),
            query_classification: classification_config,
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-qc-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "please analyze-image from the dataset".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        // Vision model_provider should have been called instead of the default.
        assert_eq!(
            agent_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            vision_model_provider_impl.call_count.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            vision_model_provider_impl
                .models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            &["gpt-4-vision".to_string()]
        );
    }

    #[tokio::test]
    async fn process_channel_message_classification_disabled_uses_default_route() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
        let vision_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let vision_model_provider: Arc<dyn ModelProvider> = vision_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(
            "test-provider".to_string(),
            Arc::clone(&agent_model_provider),
        );
        provider_cache_seed.insert("vision-provider".to_string(), vision_model_provider);

        // Classification is disabled — matching keyword should NOT trigger reroute.
        let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
            enabled: false,
            rules: vec![zeroclaw_config::schema::ClassificationRule {
                hint: "vision".into(),
                keywords: vec!["analyze-image".into()],
                ..Default::default()
            }],
        };

        let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
            hint: "vision".into(),
            model_provider: "vision-provider".into(),
            model: "gpt-4-vision".into(),
            api_key: None,
        }];

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&agent_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(model_routes),
            query_classification: classification_config,
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-qc-disabled".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "please analyze-image from the dataset".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        // Default model_provider should be used since classification is disabled.
        assert_eq!(
            agent_model_provider_impl.call_count.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            vision_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn process_channel_message_classification_no_match_uses_default_route() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
        let vision_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let vision_model_provider: Arc<dyn ModelProvider> = vision_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(
            "test-provider".to_string(),
            Arc::clone(&agent_model_provider),
        );
        provider_cache_seed.insert("vision-provider".to_string(), vision_model_provider);

        // Classification enabled with a rule that won't match the message.
        let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
            enabled: true,
            rules: vec![zeroclaw_config::schema::ClassificationRule {
                hint: "vision".into(),
                keywords: vec!["analyze-image".into()],
                ..Default::default()
            }],
        };

        let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
            hint: "vision".into(),
            model_provider: "vision-provider".into(),
            model: "gpt-4-vision".into(),
            api_key: None,
        }];

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&agent_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(model_routes),
            query_classification: classification_config,
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-qc-nomatch".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "just a regular text message".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        // Default model_provider should be used since no classification rule matched.
        assert_eq!(
            agent_model_provider_impl.call_count.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            vision_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn process_channel_message_classification_priority_selects_highest() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
        let fast_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let fast_model_provider: Arc<dyn ModelProvider> = fast_model_provider_impl.clone();
        let code_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
        let code_model_provider: Arc<dyn ModelProvider> = code_model_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
        provider_cache_seed.insert(
            "test-provider".to_string(),
            Arc::clone(&agent_model_provider),
        );
        provider_cache_seed.insert("fast-provider".to_string(), fast_model_provider);
        provider_cache_seed.insert("code-provider".to_string(), code_model_provider);

        // Both rules match "code" keyword, but "code" rule has higher priority.
        let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
            enabled: true,
            rules: vec![
                zeroclaw_config::schema::ClassificationRule {
                    hint: "fast".into(),
                    keywords: vec!["code".into()],
                    priority: 1,
                    ..Default::default()
                },
                zeroclaw_config::schema::ClassificationRule {
                    hint: "code".into(),
                    keywords: vec!["code".into()],
                    priority: 10,
                    ..Default::default()
                },
            ],
        };

        let model_routes = vec![
            zeroclaw_config::schema::ModelRouteConfig {
                hint: "fast".into(),
                model_provider: "fast-provider".into(),
                model: "fast-model".into(),
                api_key: None,
            },
            zeroclaw_config::schema::ModelRouteConfig {
                hint: "code".into(),
                model_provider: "code-provider".into(),
                model: "code-model".into(),
                api_key: None,
            },
        ];

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::clone(&agent_model_provider),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: false,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(model_routes),
            query_classification: classification_config,
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        process_channel_message(
            runtime_ctx,
            zeroclaw_api::channel::ChannelMessage {
                id: "msg-qc-prio".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "write some code for me".to_string(),
                channel: "telegram".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,
            },
            CancellationToken::new(),
        )
        .await;

        // Higher-priority "code" rule (priority=10) should win over "fast" (priority=1).
        assert_eq!(
            agent_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            fast_model_provider_impl.call_count.load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            code_model_provider_impl.call_count.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            code_model_provider_impl
                .models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            &["code-model".to_string()]
        );
    }

    #[cfg(feature = "channel-telegram")]
    #[test]
    fn build_channel_by_id_unconfigured_telegram_returns_error() {
        let config = Config::default();
        let config_arc = Arc::new(RwLock::new(config));
        match build_channel_by_id(&config_arc, "telegram") {
            Err(e) => {
                let err_msg = e.to_string();
                assert!(
                    err_msg.contains("not configured"),
                    "expected 'not configured' in error, got: {err_msg}"
                );
            }
            Ok(_) => panic!("should fail when telegram is not configured"),
        }
    }

    #[cfg(feature = "channel-telegram")]
    #[test]
    fn build_channel_by_id_configured_telegram_succeeds() {
        let mut config = Config::default();
        config.channels.telegram.insert(
            "default".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                stream_mode: zeroclaw_config::schema::StreamMode::Off,
                draft_update_interval_ms: 1000,
                interrupt_on_new_message: false,
                mention_only: false,
                ack_reactions: None,
                proxy_url: None,
                approval_timeout_secs: 120,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        let config_arc = Arc::new(RwLock::new(config));
        match build_channel_by_id(&config_arc, "telegram") {
            Ok(channel) => assert_eq!(channel.name(), "telegram"),
            Err(e) => panic!("should succeed when telegram is configured: {e}"),
        }
    }

    #[cfg(feature = "channel-voice-call")]
    #[test]
    fn build_channel_by_id_unconfigured_voice_call_returns_error() {
        let config = Config::default();
        let config_arc = Arc::new(RwLock::new(config));
        match build_channel_by_id(&config_arc, "voice-call") {
            Err(e) => {
                let err_msg = e.to_string();
                assert!(
                    err_msg.contains("not configured"),
                    "expected 'not configured' in error, got: {err_msg}"
                );
            }
            Ok(_) => panic!("should fail when voice-call is not configured"),
        }
    }

    #[cfg(feature = "channel-voice-call")]
    #[test]
    fn build_channel_by_id_configured_voice_call_succeeds() {
        let mut config = Config::default();
        config.channels.voice_call.insert(
            "default".to_string(),
            zeroclaw_config::scattered_types::VoiceCallConfig {
                enabled: true,
                model_provider: zeroclaw_config::scattered_types::VoiceProvider::Twilio,
                account_id: "AC_TEST".to_string(),
                auth_token: "test_token".to_string(),
                from_number: "+15551234567".to_string(),
                webhook_port: 8090,
                require_outbound_approval: true,
                transcription_logging: true,
                tts_voice: None,
                max_call_duration_secs: 3600,
                webhook_base_url: None,
                excluded_tools: vec![],
            },
        );
        let config_arc = Arc::new(RwLock::new(config));
        match build_channel_by_id(&config_arc, "voice-call") {
            Ok(channel) => assert_eq!(channel.name(), "voice_call"),
            Err(e) => panic!("should succeed when voice-call is configured: {e}"),
        }
    }

    // ── is_stop_command tests ─────────────────────────────────────────────

    #[test]
    fn is_stop_command_matches_bare_slash_stop() {
        assert!(is_stop_command("/stop"));
    }

    #[test]
    fn is_stop_command_matches_with_leading_trailing_whitespace() {
        assert!(is_stop_command("  /stop  "));
    }

    #[test]
    fn is_stop_command_is_case_insensitive() {
        assert!(is_stop_command("/STOP"));
        assert!(is_stop_command("/Stop"));
    }

    #[test]
    fn is_stop_command_matches_with_bot_suffix() {
        assert!(is_stop_command("/stop@zeroclaw_bot"));
    }

    #[test]
    fn is_stop_command_rejects_other_slash_commands() {
        assert!(!is_stop_command("/new"));
        assert!(!is_stop_command("/model gpt-4"));
        assert!(!is_stop_command("/models"));
    }

    #[test]
    fn is_stop_command_rejects_plain_text() {
        assert!(!is_stop_command("stop"));
        assert!(!is_stop_command("please stop"));
        assert!(!is_stop_command(""));
    }

    #[test]
    fn is_stop_command_rejects_stop_as_substring() {
        assert!(!is_stop_command("/stopwatch"));
        assert!(!is_stop_command("/stop-all"));
    }

    #[test]
    fn interrupt_on_new_message_enabled_for_mattermost_when_true() {
        let cfg = InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: true,
            matrix: false,
            whatsapp: false,
        };
        assert!(cfg.enabled_for_channel("mattermost"));
    }

    #[test]
    fn interrupt_on_new_message_disabled_for_mattermost_by_default() {
        let cfg = InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        };
        assert!(!cfg.enabled_for_channel("mattermost"));
    }

    #[test]
    fn interrupt_on_new_message_enabled_for_discord() {
        let cfg = InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: true,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        };
        assert!(cfg.enabled_for_channel("discord"));
    }

    #[test]
    fn interrupt_on_new_message_enabled_for_whatsapp() {
        let cfg = InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: true,
        };
        assert!(cfg.enabled_for_channel("whatsapp"));
    }

    #[test]
    fn interrupt_on_new_message_config_reads_whatsapp_default_alias() {
        let mut channels = zeroclaw_config::schema::ChannelsConfig::default();
        channels.whatsapp.insert(
            "default".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                session_path: Some("/tmp/zeroclaw-whatsapp-session.db".into()),
                interrupt_on_new_message: true,
                ..Default::default()
            },
        );

        let cfg = interrupt_on_new_message_config(&channels);

        assert!(cfg.enabled_for_channel("whatsapp"));
        assert!(!cfg.enabled_for_channel("telegram"));
    }

    #[test]
    fn interrupt_on_new_message_disabled_for_discord_by_default() {
        let cfg = InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        };
        assert!(!cfg.enabled_for_channel("discord"));
    }

    // ── interruption_scope_key tests ──────────────────────────────────────

    #[test]
    fn interruption_scope_key_without_scope_id_is_three_component() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "1".into(),
            sender: "alice".into(),
            reply_target: "room".into(),
            content: "hi".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };
        assert_eq!(interruption_scope_key(&msg), "matrix_room_alice");
    }

    #[test]
    fn interruption_scope_key_with_scope_id_is_four_component() {
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "1".into(),
            sender: "alice".into(),
            reply_target: "room".into(),
            content: "hi".into(),
            channel: "matrix".into(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: Some("$thread1".into()),
            interruption_scope_id: Some("$thread1".into()),
            attachments: vec![],
            subject: None,
        };
        assert_eq!(interruption_scope_key(&msg), "matrix_room_alice_$thread1");
    }

    #[test]
    fn interruption_scope_key_thread_ts_alone_does_not_affect_key() {
        // thread_ts used for reply anchoring should not bleed into scope key
        let msg = zeroclaw_api::channel::ChannelMessage {
            id: "1".into(),
            sender: "alice".into(),
            reply_target: "C123".into(),
            content: "hi".into(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: Some("1234567890.000100".into()), // Slack top-level fallback
            interruption_scope_id: None,                 // but NOT a thread reply
            attachments: vec![],
            subject: None,
        };
        assert_eq!(interruption_scope_key(&msg), "slack_C123_alice");
    }

    #[tokio::test]
    async fn message_dispatch_different_threads_do_not_cancel_each_other() {
        let channel_impl = Arc::new(SlackRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            model_provider: Arc::new(SlowModelProvider {
                delay: Duration::from_millis(150),
            }),
            model_provider_ref: Arc::new("test-provider".to_string()),
            agent_alias: Arc::new("test-agent".to_string()),
            agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
            memory: Arc::new(NoopMemory),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    Arc::new(NoopMemory),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: Some(0.0),
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
            ))),
            pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
            provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: InterruptOnNewMessageConfig {
                telegram: false,
                slack: true,
                discord: false,
                mattermost: false,
                matrix: false,
                whatsapp: false,
            },
            multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
            media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
            transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
            agent_transcription_provider: String::new(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Vec::new()),
            autonomy_level: AutonomyLevel::default(),
            tool_call_dedup_exempt: Arc::new(Vec::new()),
            model_routes: Arc::new(Vec::new()),
            query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
            ack_reactions: true,
            show_tool_calls: true,
            session_store: None,
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            )),
            activated_tools: None,
            cost_tracking: None,
            pacing: zeroclaw_config::schema::PacingConfig::default(),
            max_tool_result_chars: 0,
            context_token_budget: 0,
            debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
                Duration::ZERO,
            )),
            receipt_generator: None,
            show_receipts_in_response: false,
            last_applied_config_stamp: Arc::new(Mutex::new(None)),
            runtime_defaults_override: Arc::new(Mutex::new(None)),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
        let send_task = zeroclaw_spawn::spawn!(async move {
            // Two messages from same sender but in different Slack threads —
            // they must NOT cancel each other.
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "1741234567.100001".to_string(),
                sender: "alice".to_string(),
                reply_target: "C123".to_string(),
                content: "thread-a question".to_string(),
                channel: "slack".to_string(),
                channel_alias: None,
                timestamp: 1,
                thread_ts: Some("1741234567.100001".to_string()),
                interruption_scope_id: Some("1741234567.100001".to_string()),
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            tx.send(zeroclaw_api::channel::ChannelMessage {
                id: "1741234567.200002".to_string(),
                sender: "alice".to_string(),
                reply_target: "C123".to_string(),
                content: "thread-b question".to_string(),
                channel: "slack".to_string(),
                channel_alias: None,
                timestamp: 2,
                thread_ts: Some("1741234567.200002".to_string()),
                interruption_scope_id: Some("1741234567.200002".to_string()),
                attachments: vec![],
                subject: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
        send_task.await.unwrap();

        // Both tasks should have completed — different threads, no cancellation.
        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(
            sent_messages.len(),
            2,
            "both Slack thread messages should complete, got: {sent_messages:?}"
        );
    }

    #[test]
    fn sanitize_channel_response_redacts_detected_credentials() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let leaked = "Temporary key: AKIAABCDEFGHIJKLMNOP"; // gitleaks:allow

        let result = sanitize_channel_response(leaked, &tools);

        assert!(!result.contains("AKIAABCDEFGHIJKLMNOP")); // gitleaks:allow
        assert!(result.contains("[REDACTED"));
    }

    #[test]
    fn sanitize_channel_response_passes_clean_text() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let clean_text = "This is a normal message with no credentials.";

        let result = sanitize_channel_response(clean_text, &tools);

        assert_eq!(result, clean_text);
    }

    #[test]
    fn sanitize_channel_response_preserves_schema_json_array_without_tools() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let schema = r#"[{"name":"planner","parameters":{"goal":"string"}}]"#;

        let result = sanitize_channel_response(schema, &tools);

        assert_eq!(result, schema);
    }

    #[test]
    fn sanitize_channel_response_preserves_tool_calls_audit_json() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let audit_json =
            r#"{"tool_calls":[{"id":"case-1","status":"queued","service":"billing"}]}"#;

        let result = sanitize_channel_response(audit_json, &tools);

        assert_eq!(result, audit_json);
    }

    #[test]
    fn sanitize_channel_response_preserves_reference_function_call_json_without_tools() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let reference_json =
            r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;

        let result = sanitize_channel_response(reference_json, &tools);

        assert_eq!(result, reference_json);
    }

    #[test]
    fn sanitize_channel_response_preserves_reference_function_call_json_with_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let reference_json =
            r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;

        let result = sanitize_channel_response(reference_json, &tools);

        assert_eq!(result, reference_json);
    }

    #[test]
    fn sanitize_channel_response_preserves_unknown_tool_calls_json_with_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let business_json = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}]}"#;

        let result = sanitize_channel_response(business_json, &tools);

        assert_eq!(result, business_json);
    }

    #[test]
    fn sanitize_channel_response_preserves_malformed_unknown_tool_calls_json_with_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let business_json = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#;

        let result = sanitize_channel_response(business_json, &tools);

        assert_eq!(result, business_json);
    }

    #[test]
    fn sanitize_channel_response_preserves_json_fenced_tool_protocol_example() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let example = r#"Here is a protocol example:
```json
{"tool_calls":[{"name":"shell","arguments":{"command":"pwd"}}]}
```"#;

        let result = sanitize_channel_response(example, &tools);

        assert_eq!(result, example);
    }

    #[test]
    fn sanitize_channel_response_removes_registered_tool_json_array() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let internal = r#"[{"name":"mock_price","parameters":{"symbol":"BTC"}}]"#;

        let result = sanitize_channel_response(internal, &tools);

        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_channel_response_removes_internal_tool_protocol_envelopes() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let internal = r#"{"toolcalls":[{"name":"mock_price","arguments":{"symbol":"BTC"}}]}"#;

        let result = sanitize_channel_response(internal, &tools);

        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_channel_response_removes_json_fenced_internal_tool_protocol() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let internal = r#"```json
{"tool_calls":[{"name":"mock_price","arguments":{"symbol":"BTC"}}]}
```"#;

        let result = sanitize_channel_response(internal, &tools);

        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_channel_response_removes_embedded_json_fenced_internal_tool_protocol() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let response = r#"Intro
```json
{"tool_calls":[{"name":"mock_price","arguments":{"symbol":"BTC"}}]}
```
Done."#;

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Intro"));
        assert!(result.contains("Done."));
        assert!(!result.contains("tool_calls"));
        assert!(!result.contains("mock_price"));
    }

    #[test]
    fn sanitize_channel_response_removes_embedded_tool_call_fence() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let response = r#"Let me call it:
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
Done."#;

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Done."));
        assert!(!result.contains("tool_call"));
        assert!(!result.contains("shell"));
        assert!(!result.contains("command"));
    }

    #[test]
    fn sanitize_channel_response_preserves_tool_call_fenced_example() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;

        let result = sanitize_channel_response(example, &tools);

        assert_eq!(result, example);
    }

    #[test]
    fn sanitize_channel_response_removes_standalone_tool_call_fence() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let internal = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;

        let result = sanitize_channel_response(internal, &tools);

        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_channel_response_removes_standalone_tool_name_fence() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let internal = r#"```tool shell
{"command":"pwd"}
```"#;

        let result = sanitize_channel_response(internal, &tools);

        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_channel_response_preserves_tool_call_tag_example() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let example = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
This is an example, not an invocation."#;

        let result = sanitize_channel_response(example, &tools);

        assert_eq!(result, example);
    }

    #[test]
    fn sanitize_channel_response_strips_tagged_tool_call_before_trailing_text() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
Done."#;

        let result = sanitize_channel_response(response, &tools);

        assert_eq!(result, "Done.");
    }

    #[test]
    fn sanitize_channel_response_removes_malformed_top_level_protocol() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let internal = r#"{"tool_call_id":"call_1","content":"raw"#;

        let result = sanitize_channel_response(internal, &tools);

        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_channel_response_removes_embedded_malformed_protocol_json() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let response =
            "Intro\n{\"tool_calls\":[{\"call_id\":\"call_1\",\"arguments\":{\"value\":\"X\"}\nDone";

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Intro"));
        assert!(result.contains("Done"));
        assert!(!result.contains("tool_calls"));
        assert!(!result.contains("arguments"));
    }

    #[test]
    fn sanitize_channel_response_removes_multiline_embedded_malformed_protocol_json() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let response = "Intro\n{\n  \"tool_calls\": [{\"call_id\":\"call_1\",\"arguments\":{\"value\":\"X\"}}\nDone";

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Intro"));
        assert!(result.contains("Done"));
        assert!(!result.contains("tool_calls"));
        assert!(!result.contains("arguments"));
    }

    #[test]
    fn sanitize_channel_response_keeps_protocol_explanation_text() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let explanation =
            "A markdown block starting with ```tool can be used in protocol examples.";

        let result = sanitize_channel_response(explanation, &tools);

        assert_eq!(result, explanation);
    }

    #[test]
    fn sanitize_channel_response_keeps_safe_protocol_envelope_content_with_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let response = "Intro text\n{\"content\":\"A markdown block starting with ```tool can be used in examples.\",\"tool_calls\":[{\"name\":\"mock_price\",\"arguments\":{\"symbol\":\"BTC\"}}]}\nDone.";

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Intro text"));
        assert!(result.contains("A markdown block starting with ```tool"));
        assert!(result.contains("Done."));
        assert!(!result.contains("tool_calls"));
    }

    #[test]
    fn sanitize_channel_response_removes_isolated_tool_result_envelope_content_with_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let response =
            "Intro text\n{\"tool_call_id\":\"call_1\",\"content\":\"raw tool output\"}\nDone.";

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Intro text"));
        assert!(result.contains("Done."));
        assert!(!result.contains("tool_call_id"));
        assert!(!result.contains("raw tool output"));
    }

    #[test]
    fn sanitize_channel_response_removes_nested_protocol_content_with_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let response = "Intro text\n{\"content\":\"{\\\"toolcalls\\\":[{\\\"name\\\":\\\"mock_price\\\",\\\"arguments\\\":{\\\"symbol\\\":\\\"BTC\\\"}}]}\",\"tool_calls\":[{\"name\":\"mock_price\",\"arguments\":{\"symbol\":\"BTC\"}}]}\nDone.";

        let result = sanitize_channel_response(response, &tools);

        assert!(result.contains("Intro text"));
        assert!(result.contains("Done."));
        assert!(!result.contains("toolcalls"));
        assert!(!result.contains("shell"));
    }

    #[test]
    fn sanitize_channel_response_strips_xml_tool_result_blocks() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let input = "<tool_result>\n{\"results\":[]}\n</tool_result>\n<tool_result>\n{\"command\":\"ls\",\"exit_code\":0}\n</tool_result>Here is what I found.";

        let result = sanitize_channel_response(input, &tools);

        assert!(!result.contains("tool_result"));
        assert!(!result.contains("exit_code"));
        assert!(result.contains("Here is what I found."));
    }

    #[test]
    fn sanitize_channel_response_strips_mixed_tool_result_and_text() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let input = "Let me check.\n<tool_result name=\"shell\">\noutput here\n</tool_result>\nThe answer is 42.";

        let result = sanitize_channel_response(input, &tools);

        assert!(!result.contains("<tool_result"));
        assert!(!result.contains("output here"));
        assert!(result.contains("The answer is 42."));
    }

    // ── Tests for strip_think_tags_inline (streaming draft sanitization) ──

    #[test]
    fn strip_think_tags_inline_removes_single_block() {
        assert_eq!(
            strip_think_tags_inline("<think>reasoning</think>Hello"),
            "Hello"
        );
    }

    #[test]
    fn strip_think_tags_inline_removes_multiple_blocks() {
        assert_eq!(
            strip_think_tags_inline("<think>a</think>X<think>b</think>Y"),
            "XY"
        );
    }

    #[test]
    fn strip_think_tags_inline_handles_unclosed_block() {
        assert_eq!(
            strip_think_tags_inline("visible<think>hidden tail"),
            "visible"
        );
    }

    #[test]
    fn strip_think_tags_inline_preserves_text_without_tags() {
        assert_eq!(strip_think_tags_inline("plain text"), "plain text");
    }

    #[test]
    fn strip_think_tags_inline_handles_empty_string() {
        assert_eq!(strip_think_tags_inline(""), "");
    }

    #[test]
    fn strip_think_tags_inline_strips_surrounding_whitespace() {
        assert_eq!(
            strip_think_tags_inline("<think>hidden</think>  Answer  "),
            "Answer"
        );
    }

    // ── Tests for #4827: tool context preservation ──────────────

    #[test]
    fn extract_current_turn_tool_messages_returns_intermediate_messages() {
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("older msg"),
            ChatMessage::assistant("older reply"),
            ChatMessage::user("block the iPad"),
            ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
            ChatMessage::tool("ok"),
            ChatMessage::assistant("Done, iPad is blocked."),
        ];

        let tool_msgs = extract_current_turn_tool_messages(&history);
        assert_eq!(tool_msgs.len(), 2);
        assert_eq!(tool_msgs[0].role, "assistant");
        assert!(tool_msgs[0].content.contains("tool_call"));
        assert_eq!(tool_msgs[1].role, "tool");
    }

    #[test]
    fn extract_current_turn_tool_messages_empty_when_no_tools() {
        let history = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("Hi there!"),
        ];

        let tool_msgs = extract_current_turn_tool_messages(&history);
        assert!(tool_msgs.is_empty());
    }

    #[test]
    fn extract_current_turn_tool_messages_multiple_tool_rounds() {
        let history = vec![
            ChatMessage::user("do two things"),
            ChatMessage::assistant("{\"tool_call\": \"read_skill\"}"),
            ChatMessage::tool("skill content"),
            ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
            ChatMessage::tool("shell output"),
            ChatMessage::assistant("All done."),
        ];

        let tool_msgs = extract_current_turn_tool_messages(&history);
        assert_eq!(tool_msgs.len(), 4);
    }

    #[test]
    fn is_tool_call_content_detects_tool_calls() {
        assert!(is_tool_call_content("{\"tool_call\": \"shell\"}"));
        assert!(is_tool_call_content("<tool_call>shell</tool_call>"));
        assert!(is_tool_call_content(
            "{\"name\": \"read_file\", \"args\": {}}"
        ));
        assert!(!is_tool_call_content("The iPad has been blocked."));
        assert!(!is_tool_call_content(""));
    }

    #[test]
    fn is_tool_call_content_does_not_misclassify_regular_name_json() {
        assert!(!is_tool_call_content(
            "{\"name\":\"Alice\",\"role\":\"admin\"}"
        ));
    }

    #[test]
    fn strip_old_tool_context_preserves_recent_tool_context_when_history_within_keep_window() {
        let ctx = router_test_ctx();
        let sender = "tool-window-short";
        seed_sender_history(
            ctx.as_ref(),
            sender,
            vec![
                ChatMessage::user("block the iPad"),
                ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
                ChatMessage::tool(r#"{"tool_call_id":"call_1","content":"ok"}"#),
                ChatMessage::assistant("Done, iPad is blocked."),
            ],
        );

        strip_old_tool_context(ctx.as_ref(), sender, 2);

        let turns = cloned_sender_history(ctx.as_ref(), sender);
        assert_eq!(
            turns.len(),
            4,
            "tool context in protected turns must not be stripped"
        );
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1].content.contains("tool_call"));
        assert_eq!(turns[2].role, "tool");
    }

    #[test]
    fn strip_old_tool_context_strips_tool_context_before_keep_window_boundary() {
        let ctx = router_test_ctx();
        let sender = "tool-window-boundary";
        seed_sender_history(
            ctx.as_ref(),
            sender,
            vec![
                ChatMessage::user("first task"),
                ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
                ChatMessage::tool("ok"),
                ChatMessage::assistant("first task done"),
                ChatMessage::user("second task"),
                ChatMessage::assistant("second task done"),
                ChatMessage::user("third task"),
                ChatMessage::assistant("third task done"),
            ],
        );

        strip_old_tool_context(ctx.as_ref(), sender, 2);

        let turns = cloned_sender_history(ctx.as_ref(), sender);
        assert_eq!(
            history_signature(&turns),
            vec![
                ("user".to_string(), "first task".to_string()),
                ("assistant".to_string(), "first task done".to_string()),
                ("user".to_string(), "second task".to_string()),
                ("assistant".to_string(), "second task done".to_string()),
                ("user".to_string(), "third task".to_string()),
                ("assistant".to_string(), "third task done".to_string()),
            ],
            "tool context older than the protected keep window should be stripped"
        );
    }

    #[test]
    fn strip_old_tool_context_removes_native_tool_call_assistant_messages() {
        let ctx = router_test_ctx();
        let sender = "tool-window-native";
        seed_sender_history(
            ctx.as_ref(),
            sender,
            vec![
                ChatMessage::user("first task"),
                ChatMessage::assistant(
                    r#"{"content":"Need to call tool","tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"pwd\"}"}]}"#,
                ),
                ChatMessage::tool(r#"{"tool_call_id":"call_1","content":"ok"}"#),
                ChatMessage::assistant("first task done"),
                ChatMessage::user("second task"),
                ChatMessage::assistant("second task done"),
                ChatMessage::user("third task"),
                ChatMessage::assistant("third task done"),
            ],
        );

        strip_old_tool_context(ctx.as_ref(), sender, 1);

        let turns = cloned_sender_history(ctx.as_ref(), sender);
        assert_eq!(
            history_signature(&turns),
            vec![
                ("user".to_string(), "first task".to_string()),
                ("assistant".to_string(), "first task done".to_string()),
                ("user".to_string(), "second task".to_string()),
                ("assistant".to_string(), "second task done".to_string()),
                ("user".to_string(), "third task".to_string()),
                ("assistant".to_string(), "third task done".to_string()),
            ],
            "native assistant tool-call JSON should be stripped together with old tool results"
        );
    }

    #[test]
    fn normalize_cached_channel_turns_passes_through_tool_messages() {
        let turns = vec![
            ChatMessage::user("block the iPad"),
            ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
            ChatMessage::tool("ok"),
            ChatMessage::assistant("iPad blocked."),
            ChatMessage::user("next question"),
        ];

        let normalized = normalize_cached_channel_turns(turns);
        // user, assistant(tool_call), tool, assistant(final), user
        assert_eq!(normalized.len(), 5);
        assert_eq!(normalized[2].role, "tool");
    }

    #[test]
    fn default_keep_tool_context_turns_is_two() {
        let config = zeroclaw_config::schema::AliasedAgentConfig::default();
        assert_eq!(config.resolved.keep_tool_context_turns, 2);
    }

    #[test]
    fn build_channel_system_prompt_includes_sender_id() {
        let prompt = build_channel_system_prompt(
            "You are a helpful assistant.",
            "mattermost",
            "channel123:root456",
            "user_abc123",
            "msg-xyz789",
            None,
        );
        // Pin the comma-separated tuple in the format string so a refactor
        // that splits, reorders, or rewords the context block fails loudly
        // rather than silently.
        assert!(
            prompt.contains(
                "channel=mattermost, reply_target=channel123:root456, \
                 sender=user_abc123, message_id=msg-xyz789"
            ),
            "prompt missing the joint channel-context tuple: {prompt}"
        );
    }

    #[test]
    fn build_channel_system_prompt_omits_context_when_reply_target_empty() {
        let prompt = build_channel_system_prompt(
            "Base prompt.",
            "mattermost",
            "",
            "user_abc123",
            "msg-xyz789",
            None,
        );
        assert!(!prompt.contains("sender="));
        assert!(!prompt.contains("Channel context:"));
    }

    #[test]
    fn build_channel_system_prompt_sender_distinguishes_users() {
        let prompt_a = build_channel_system_prompt(
            "Base.",
            "mattermost",
            "ch:thread",
            "user_aaa",
            "msg-1",
            None,
        );
        let prompt_b = build_channel_system_prompt(
            "Base.",
            "mattermost",
            "ch:thread",
            "user_bbb",
            "msg-1",
            None,
        );
        assert!(prompt_a.contains("sender=user_aaa"));
        assert!(prompt_b.contains("sender=user_bbb"));
        assert_ne!(prompt_a, prompt_b);
    }

    #[test]
    fn build_channel_system_prompt_refreshes_legacy_datetime_section_to_date_only() {
        let prompt = build_channel_system_prompt(
            "Base.\n\n## Current Date\n\nProject note, not generated date context.\n\n## Current Date & Time\n\n2026-01-01 01:02:03 (UTC)\n\n## Runtime\n\nHost: old\n",
            "mattermost",
            "ch:thread",
            "user_aaa",
            "msg-1",
            None,
        );

        assert!(prompt.contains("## Current Date\n\n"));
        assert!(prompt.contains("Project note, not generated date context."));
        assert!(!prompt.contains("## Current Date & Time"));
        assert!(!prompt.contains("01:02:03"));
        let generated_section = prompt
            .split("## Runtime")
            .next()
            .expect("prompt should contain runtime section before generated date assertion");
        let date_line = generated_section
            .rsplit("## Current Date\n\n")
            .next()
            .and_then(|rest| rest.lines().next())
            .expect("current date section should have a date line");
        assert_eq!(
            &date_line[..10],
            &chrono::Local::now().format("%Y-%m-%d").to_string()
        );
        assert!(
            date_line[10..].starts_with(" ("),
            "date line should contain only date plus UTC offset: {date_line}"
        );
    }

    #[test]
    fn build_channel_system_prompt_refreshes_current_date_section() {
        let prompt = build_channel_system_prompt(
            "Base.\n\n## Current Date\n\n2026-01-01 (+00:00)\n\n## Runtime\n\nHost: old\n",
            "mattermost",
            "ch:thread",
            "user_aaa",
            "msg-1",
            None,
        );

        assert!(prompt.contains("## Current Date\n\n"));
        assert!(!prompt.contains("2026-01-01 (+00:00)"));
        let date_line = prompt
            .split("## Current Date\n\n")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .expect("current date section should have a date line");
        assert_eq!(
            &date_line[..10],
            &chrono::Local::now().format("%Y-%m-%d").to_string()
        );
    }

    #[test]
    fn build_channel_system_prompt_for_message_propagates_channel_fields() {
        // The wrapper unpacks ChannelMessage into build_channel_system_prompt
        // args. Pin the rendered prompt against every msg.* field the LLM
        // is expected to see so a future refactor adding more fields can't
        // silently drop existing ones.
        let msg = channel_message("discord", None);
        let prompt = build_channel_system_prompt_for_message("Base.", &msg, None);
        assert!(
            prompt.contains("channel=discord, reply_target=r1, sender=u1, message_id=m1"),
            "wrapper did not propagate channel/reply_target/sender/message_id \
             from ChannelMessage: {prompt}"
        );
    }

    #[test]
    fn build_channel_system_prompt_webhook_cron_hint_carries_thread_id() {
        // On the webhook channel `reply_target` is the inbound thread/conversation
        // id, not a recipient. Using it as `delivery.to` would strip the thread
        // context from the cron-announce callback (see #6634). The hint must
        // place the sender in `to` and the reply_target in `thread_id`.
        let prompt = build_channel_system_prompt(
            "Base.",
            "webhook",
            "agent-chat:agent-1:thread-7",
            "user:abc",
            "msg-1",
            None,
        );
        assert!(
            prompt.contains("\"to\":\"user:abc\""),
            "webhook cron hint must use sender as `to`: {prompt}"
        );
        assert!(
            prompt.contains("\"thread_id\":\"agent-chat:agent-1:thread-7\""),
            "webhook cron hint must carry the reply_target as `thread_id`: {prompt}"
        );
        assert!(
            !prompt.contains("\"to\":\"agent-chat:agent-1:thread-7\""),
            "webhook cron hint must not put the thread id in `to`: {prompt}"
        );
    }

    #[test]
    fn build_channel_system_prompt_non_webhook_cron_hint_keeps_to_as_reply_target() {
        let prompt =
            build_channel_system_prompt("Base.", "slack", "C12345", "U67890", "msg-1", None);
        assert!(
            prompt.contains("\"to\":\"C12345\""),
            "non-webhook cron hint should keep reply_target as `to`: {prompt}"
        );
        assert!(
            !prompt.contains("\"thread_id\""),
            "non-webhook cron hint should not emit a thread_id field: {prompt}"
        );
    }

    #[tokio::test]
    #[cfg(feature = "channel-lark")]
    async fn deliver_announcement_routes_lark_to_lark_arm() {
        // Both names must enter the merged lark|feishu arm. Falling through
        // to `unsupported delivery channel` would mean the schema enum and
        // the match arm have drifted apart.
        let config = zeroclaw_config::schema::Config::default();

        for channel in ["lark.default", "feishu.default"] {
            let err = deliver_announcement(&config, channel, "oc_test_chat", None, "hi")
                .await
                .err()
                .unwrap_or_else(|| {
                    panic!("expected {channel} to bail because channel is not configured")
                });
            let msg = format!("{err:#}");
            assert!(
                !msg.contains("unsupported delivery channel"),
                "{channel} must route to lark|feishu arm, not fall through; got: {msg}"
            );
            assert!(
                msg.contains("[channels.lark.default] not configured"),
                "{channel} must report the real config table [channels.lark.default]; got: {msg}"
            );
        }
    }

    #[tokio::test]
    #[cfg(feature = "channel-lark")]
    async fn deliver_announcement_rejects_feishu_value_when_use_feishu_false() {
        // Reject (not warn): otherwise the message silently lands on the
        // Lark endpoint despite the user explicitly naming Feishu.
        let mut config = zeroclaw_config::schema::Config::default();
        config.channels.lark.insert(
            "work".to_string(),
            zeroclaw_config::schema::LarkConfig {
                enabled: true,
                use_feishu: false,
                app_id: "cli_test".to_string(),
                app_secret: "secret".to_string(),
                approval_timeout_secs: 300,
                per_user_session: false,
                ..Default::default()
            },
        );

        let err = deliver_announcement(&config, "feishu.work", "oc_test_chat", None, "hi")
            .await
            .expect_err("expected bail when channel=feishu but use_feishu=false");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("use_feishu=false"),
            "bail must explain the use_feishu mismatch; got: {msg}"
        );
        assert!(
            msg.contains("[channels.lark.work]"),
            "bail must point at the real config table; got: {msg}"
        );
    }

    fn email_msg(id: &str, subject: Option<&str>) -> ChannelMessage {
        ChannelMessage {
            subject: subject.map(Into::into),
            ..ChannelMessage::new(
                id,
                "user@example.com",
                "user@example.com",
                "Hello",
                "email",
                0,
            )
        }
    }

    #[test]
    fn reply_to_sets_in_reply_to_and_re_subject() {
        let msg = email_msg("<abc123@mail.example>", Some("Weekly report"));
        let sm = SendMessage::reply_to(&msg, "Here is the answer");
        assert_eq!(sm.in_reply_to.as_deref(), Some("<abc123@mail.example>"));
        assert_eq!(sm.subject.as_deref(), Some("Re: Weekly report"));
    }

    #[test]
    fn reply_to_does_not_double_re_prefix() {
        let msg = email_msg("<abc123@mail.example>", Some("Re: Weekly report"));
        let sm = SendMessage::reply_to(&msg, "Here is the answer");
        assert_eq!(sm.subject.as_deref(), Some("Re: Weekly report"));
    }

    #[test]
    fn reply_to_no_subject_still_sets_in_reply_to() {
        let msg = email_msg("<abc123@mail.example>", None);
        let sm = SendMessage::reply_to(&msg, "Here is the answer");
        assert_eq!(sm.in_reply_to.as_deref(), Some("<abc123@mail.example>"));
        assert!(sm.subject.is_none());
    }
}

#[cfg(test)]
mod omitted_feature_tests {
    /// When `channel-telegram` is not compiled, a configured Telegram entry must
    /// produce no channel in `collect_configured_channels`. This pins the behaviour
    /// that selective builds never silently include a channel whose feature was
    /// omitted, and ensures the `#[cfg(not(feature = "channel-telegram"))]` warn
    /// path compiles correctly.
    #[cfg(not(feature = "channel-telegram"))]
    #[test]
    fn collect_configured_channels_omits_telegram_when_compiled_out() {
        use super::*;
        let mut config = Config::default();
        config.channels.telegram.insert(
            "default".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                ..Default::default()
            },
        );
        let config_arc = Arc::new(RwLock::new(config));
        let channels = collect_configured_channels(&config_arc, "test", &[]);
        assert!(
            channels.iter().all(|c| c.display_name != "Telegram"),
            "Telegram must be absent from collect_configured_channels when \
             channel-telegram feature is not compiled in"
        );
    }
}
