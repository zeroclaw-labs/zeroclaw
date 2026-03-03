//! Channel subsystem for messaging platform integrations.
//!
//! This module provides the multi-channel messaging infrastructure that connects
//! ZeroClaw to external platforms. Each channel implements the [`Channel`] trait
//! defined in [`traits`], which provides a uniform interface for sending messages,
//! listening for incoming messages, health checking, and typing indicators.
//!
//! Channels are instantiated by [`start_channels`] based on the runtime configuration.
//! The subsystem manages per-sender conversation history, concurrent message processing
//! with configurable parallelism, and exponential-backoff reconnection for resilience.
//!
//! # Extension
//!
//! To add a new channel, implement [`Channel`] in a new submodule and wire it into
//! [`start_channels`]. See `AGENTS.md` §7.2 for the full change playbook.

pub(crate) mod ack_reaction;
pub mod acp;
pub mod bluebubbles;
pub mod clawdtalk;
pub mod cli;
pub mod dingtalk;
pub mod discord;
pub mod email_channel;
pub mod github;
pub mod imessage;
pub mod irc;
#[cfg(feature = "channel-lark")]
pub mod lark;
pub mod linq;
#[cfg(feature = "channel-matrix")]
pub mod matrix;
pub mod mattermost;
pub mod napcat;
pub mod nextcloud_talk;
pub mod nostr;
pub mod qq;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod traits;
pub mod transcription;
pub mod wati;
pub mod whatsapp;
#[cfg(feature = "whatsapp-web")]
pub mod whatsapp_storage;
#[cfg(feature = "whatsapp-web")]
pub mod whatsapp_web;

pub use acp::AcpChannel;
pub use bluebubbles::BlueBubblesChannel;
pub use clawdtalk::ClawdTalkChannel;
pub use cli::CliChannel;
pub use dingtalk::DingTalkChannel;
pub use discord::DiscordChannel;
pub use email_channel::EmailChannel;
pub use github::GitHubChannel;
pub use imessage::IMessageChannel;
pub use irc::IrcChannel;
#[cfg(feature = "channel-lark")]
pub use lark::LarkChannel;
pub use linq::LinqChannel;
#[cfg(feature = "channel-matrix")]
pub use matrix::MatrixChannel;
pub use mattermost::MattermostChannel;
pub use napcat::NapcatChannel;
pub use nextcloud_talk::NextcloudTalkChannel;
pub use nostr::NostrChannel;
pub use qq::QQChannel;
pub use signal::SignalChannel;
pub use slack::SlackChannel;
pub use telegram::TelegramChannel;
pub use traits::{Channel, SendMessage};
pub use wati::WatiChannel;
pub use whatsapp::WhatsAppChannel;
#[cfg(feature = "whatsapp-web")]
pub use whatsapp_web::WhatsAppWebChannel;

use crate::agent::loop_::{
    build_shell_policy_instructions, build_tool_instructions_from_specs,
    run_tool_call_loop_with_non_cli_approval_context, scrub_credentials, NonCliApprovalContext,
    NonCliApprovalPrompt, SafetyHeartbeatConfig,
};
use crate::agent::session::{resolve_session_id, shared_session_manager, Session, SessionManager};
use crate::approval::{ApprovalManager, ApprovalResponse, PendingApprovalError};
use crate::config::{Config, NonCliNaturalLanguageApprovalMode, ProgressMode};
use crate::identity;
use crate::memory::{self, Memory};
use crate::observability::{self, runtime_trace, Observer};
use crate::providers::{self, ChatMessage, Provider};
use crate::runtime;
use crate::security::{LeakDetector, LeakResult, SecurityPolicy};
use crate::tools::{self, Tool};
use crate::util::truncate_with_ellipsis;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

/// Per-sender conversation history for channel messages.
type ConversationHistoryMap = Arc<Mutex<HashMap<String, Vec<ChatMessage>>>>;
type ConversationLockMap = Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;
/// Maximum history messages to keep per sender.
const MAX_CHANNEL_HISTORY: usize = 50;
/// Minimum user-message length (in chars) for auto-save to memory.
/// Messages shorter than this (e.g. "ok", "thanks") are not stored,
/// reducing noise in memory recall.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

/// Maximum characters per injected workspace file (matches `OpenClaw` default).
const BOOTSTRAP_MAX_CHARS: usize = 20_000;

const DEFAULT_CHANNEL_INITIAL_BACKOFF_SECS: u64 = 2;
const DEFAULT_CHANNEL_MAX_BACKOFF_SECS: u64 = 60;
const MIN_CHANNEL_MESSAGE_TIMEOUT_SECS: u64 = 30;
/// Default timeout for processing a single channel message (LLM + tools).
/// Used as fallback when not configured in channels_config.message_timeout_secs.
const CHANNEL_MESSAGE_TIMEOUT_SECS: u64 = 300;
/// Cap timeout scaling so large max_tool_iterations values do not create unbounded waits.
const CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP: u64 = 4;
const CHANNEL_PARALLELISM_PER_CHANNEL: usize = 4;
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
const CHANNEL_CONTEXT_TOKEN_ESTIMATE_LIMIT: usize = 90_000;
const CHANNEL_CONTEXT_TOKEN_ESTIMATE_TARGET: usize = 80_000;
const CHANNEL_CONTEXT_MIN_KEEP_NON_SYSTEM_MESSAGES: usize = 10;
/// Guardrail for hook-modified outbound channel content.
const CHANNEL_HOOK_MAX_OUTBOUND_CHARS: usize = 20_000;

type ProviderCacheMap = Arc<Mutex<HashMap<String, Arc<dyn Provider>>>>;
type RouteSelectionMap = Arc<Mutex<HashMap<String, ChannelRouteSelection>>>;

fn live_channels_registry() -> &'static Mutex<HashMap<String, Arc<dyn Channel>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<dyn Channel>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_live_channels(channels_by_name: &HashMap<String, Arc<dyn Channel>>) {
    let mut guard = live_channels_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.clear();
    for (name, channel) in channels_by_name {
        guard.insert(name.to_ascii_lowercase(), Arc::clone(channel));
    }
}

fn clear_live_channels() {
    live_channels_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

fn runtime_telegram_progress_mode_store() -> &'static Mutex<ProgressMode> {
    static STORE: OnceLock<Mutex<ProgressMode>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(ProgressMode::default()))
}

fn set_runtime_telegram_progress_mode(mode: ProgressMode) {
    *runtime_telegram_progress_mode_store()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = mode;
}

fn runtime_telegram_progress_mode() -> ProgressMode {
    *runtime_telegram_progress_mode_store()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn get_live_channel(name: &str) -> Option<Arc<dyn Channel>> {
    live_channels_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&name.to_ascii_lowercase())
        .cloned()
}

fn effective_channel_message_timeout_secs(configured: u64) -> u64 {
    configured.max(MIN_CHANNEL_MESSAGE_TIMEOUT_SECS)
}

fn channel_message_timeout_budget_secs(
    message_timeout_secs: u64,
    max_tool_iterations: usize,
) -> u64 {
    let iterations = max_tool_iterations.max(1) as u64;
    let scale = iterations.min(CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP);
    message_timeout_secs.saturating_mul(scale)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelRouteSelection {
    provider: String,
    model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelRuntimeCommand {
    ShowProviders,
    SetProvider(String),
    ShowModel,
    SetModel(String),
    NewSession,
    RequestAllToolsOnce,
    RequestToolApproval(String),
    ConfirmToolApproval(String),
    ApprovePendingRequest(String),
    DenyToolApproval(String),
    ListPendingApprovals,
    ApproveTool(String),
    UnapproveTool(String),
    ListApprovals,
}

const APPROVAL_ALL_TOOLS_ONCE_TOKEN: &str = "__all_tools_once__";

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelCacheState {
    entries: Vec<ModelCacheEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelCacheEntry {
    provider: String,
    models: Vec<String>,
}

#[derive(Debug, Clone)]
struct ChannelRuntimeDefaults {
    default_provider: String,
    model: String,
    temperature: f64,
    api_key: Option<String>,
    api_url: Option<String>,
    reliability: crate::config::ReliabilityConfig,
    cost: crate::config::CostConfig,
    auto_save_memory: bool,
    max_tool_iterations: usize,
    min_relevance_score: f64,
    message_timeout_secs: u64,
    interrupt_on_new_message: bool,
    multimodal: crate::config::MultimodalConfig,
    query_classification: crate::config::QueryClassificationConfig,
    model_routes: Vec<crate::config::ModelRouteConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigFileStamp {
    modified: SystemTime,
    len: u64,
}

#[derive(Debug, Clone)]
struct RuntimeConfigState {
    defaults: ChannelRuntimeDefaults,
    perplexity_filter: crate::config::PerplexityFilterConfig,
    outbound_leak_guard: crate::config::OutboundLeakGuardConfig,
    last_applied_stamp: Option<ConfigFileStamp>,
}

#[derive(Debug, Clone)]
struct RuntimeAutonomyPolicy {
    auto_approve: Vec<String>,
    always_ask: Vec<String>,
    non_cli_excluded_tools: Vec<String>,
    non_cli_approval_approvers: Vec<String>,
    non_cli_natural_language_approval_mode: NonCliNaturalLanguageApprovalMode,
    non_cli_natural_language_approval_mode_by_channel:
        HashMap<String, NonCliNaturalLanguageApprovalMode>,
    perplexity_filter: crate::config::PerplexityFilterConfig,
    outbound_leak_guard: crate::config::OutboundLeakGuardConfig,
}

fn runtime_config_store() -> &'static Mutex<HashMap<PathBuf, RuntimeConfigState>> {
    static STORE: OnceLock<Mutex<HashMap<PathBuf, RuntimeConfigState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

const SYSTEMD_STATUS_ARGS: [&str; 3] = ["--user", "is-active", "zeroclaw.service"];
const SYSTEMD_RESTART_ARGS: [&str; 3] = ["--user", "restart", "zeroclaw.service"];
const OPENRC_STATUS_ARGS: [&str; 2] = ["zeroclaw", "status"];
const OPENRC_RESTART_ARGS: [&str; 2] = ["zeroclaw", "restart"];

#[derive(Clone)]
struct ChannelRuntimeContext {
    channels_by_name: Arc<HashMap<String, Arc<dyn Channel>>>,
    provider: Arc<dyn Provider>,
    default_provider: Arc<String>,
    memory: Arc<dyn Memory>,
    tools_registry: Arc<Vec<Box<dyn Tool>>>,
    observer: Arc<dyn Observer>,
    system_prompt: Arc<String>,
    model: Arc<String>,
    temperature: f64,
    auto_save_memory: bool,
    max_tool_iterations: usize,
    min_relevance_score: f64,
    conversation_histories: ConversationHistoryMap,
    conversation_locks: ConversationLockMap,
    session_config: crate::config::AgentSessionConfig,
    session_manager: Option<Arc<dyn SessionManager + Send + Sync>>,
    provider_cache: ProviderCacheMap,
    route_overrides: RouteSelectionMap,
    api_key: Option<String>,
    api_url: Option<String>,
    reliability: Arc<crate::config::ReliabilityConfig>,
    provider_runtime_options: providers::ProviderRuntimeOptions,
    workspace_dir: Arc<PathBuf>,
    message_timeout_secs: u64,
    interrupt_on_new_message: bool,
    multimodal: crate::config::MultimodalConfig,
    hooks: Option<Arc<crate::hooks::HookRunner>>,
    non_cli_excluded_tools: Arc<Mutex<Vec<String>>>,
    query_classification: crate::config::QueryClassificationConfig,
    model_routes: Vec<crate::config::ModelRouteConfig>,
    approval_manager: Arc<ApprovalManager>,
    safety_heartbeat: Option<SafetyHeartbeatConfig>,
    startup_perplexity_filter: crate::config::PerplexityFilterConfig,
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

fn conversation_memory_key(msg: &traits::ChannelMessage) -> String {
    // Include thread_ts for per-topic memory isolation in forum groups
    match &msg.thread_ts {
        Some(tid) => format!("{}_{}_{}_{}", msg.channel, tid, msg.sender, msg.id),
        None => format!("{}_{}_{}", msg.channel, msg.sender, msg.id),
    }
}

fn assistant_memory_key(msg: &traits::ChannelMessage) -> String {
    format!("assistant_resp_{}", conversation_memory_key(msg))
}

fn conversation_history_key(msg: &traits::ChannelMessage) -> String {
    // QQ uses thread_ts as a passive-reply message id, not a thread identifier.
    // Using it in history keys would reset context on every incoming message.
    if msg.channel == "qq" || msg.channel == "napcat" {
        return format!("{}_{}", msg.channel, msg.sender);
    }

    // Include thread_ts for per-topic session isolation in forum groups
    let channel = msg.channel.as_str();
    match msg.thread_ts.as_deref().filter(|_| channel != "qq") {
        Some(tid) => format!("{}_{}_{}", msg.channel, tid, msg.sender),
        None if channel == "qq" => format!("{}_{}_{}", msg.channel, msg.reply_target, msg.sender),
        None => format!("{}_{}", msg.channel, msg.sender),
    }
}

fn interruption_scope_key(msg: &traits::ChannelMessage) -> String {
    format!("{}_{}_{}", msg.channel, msg.reply_target, msg.sender)
}

/// Strip tool-call XML tags from outgoing messages.
///
/// LLM responses may contain `<function_calls>`, `<function_call>`,
/// `<tool_call>`, `<toolcall>`, `<tool-call>`, `<tool>`, or `<invoke>`
/// blocks that are internal protocol and must not be forwarded to end
/// users on any channel.
fn strip_tool_call_tags(message: &str) -> String {
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
        "whatsapp" => Some(
            "When responding on WhatsApp:\n\
             - Use *bold* for emphasis (WhatsApp uses single asterisks).\n\
             - Be concise. No markdown headers (## etc.) — they don't render.\n\
             - No markdown tables — use bullet lists instead.\n\
             - For sending images, documents, videos, or audio files use markers: [IMAGE:<absolute-path>], [DOCUMENT:<absolute-path>], [VIDEO:<absolute-path>], [AUDIO:<absolute-path>]\n\
             - The path MUST be an absolute filesystem path to a local file (e.g. [IMAGE:/home/nicolas/.zeroclaw/workspace/images/chart.png]).\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n\
             - You can combine text and media in one response — text is sent first, then each attachment.\n\
             - Use tool results silently: answer the latest user message directly, and do not narrate delayed/internal tool execution bookkeeping.",
        ),
        "lark" | "feishu" => Some(
            "When responding on Lark/Feishu:\n\
             - For image attachments, use markers: [IMAGE:<path-or-url-or-data-uri>]\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n\
             - Prefer one marker per line to keep delivery deterministic.\n\
             - If you include both text and images, put text first, then image markers.\n\
             - Be concise and direct. Skip filler phrases.\n\
             - Use tool results silently: answer the latest user message directly, and do not narrate delayed/internal tool execution bookkeeping.",
        ),
        "qq" => Some(
            "When responding on QQ:\n\
             - For image attachments, use markers: [IMAGE:<path-or-url-or-data-uri>]\n\
             - Keep normal text outside markers and never wrap markers in code fences.\n\
             - Prefer one marker per line to keep delivery deterministic.\n\
             - If you include both text and images, put text first, then image markers.\n\
             - Be concise and direct. Skip filler phrases.\n\
             - Use tool results silently: answer the latest user message directly, and do not narrate delayed/internal tool execution bookkeeping.",
        ),
        "bluebubbles" => Some(
            "You are responding on iMessage via BlueBubbles. Always complete your research before replying — use as many tool calls as needed to get a full, accurate answer.\n\
             \n\
             ## Text styles (iMessage native)\n\
             - **bold** — key terms, scores, names, important info\n\
             - *italic* — emphasis, secondary info\n\
             - ~~strikethrough~~ — corrections or outdated info\n\
             - __underline__ — titles, proper nouns\n\
             - `code` — commands, technical terms\n\
             \n\
             ## Message effects (append to end, e.g. 'Great job! [EFFECT:confetti]')\n\
             Available: [EFFECT:slam] [EFFECT:loud] [EFFECT:gentle] [EFFECT:invisible-ink]\n\
             [EFFECT:confetti] [EFFECT:balloons] [EFFECT:fireworks] [EFFECT:lasers]\n\
             [EFFECT:love] [EFFECT:celebration] [EFFECT:echo] [EFFECT:spotlight]\n\
             Use effects sparingly and only when context clearly warrants it.\n\
             \n\
             ## Format rules\n\
             - No markdown tables — use bullet lists with dashes\n\
             - Keep replies conversational but complete — do not truncate results\n\
             - Do not narrate tool execution — just do the research and give the answer",
        ),
        _ => None,
    }
}

fn should_expose_internal_tool_details(user_message: &str) -> bool {
    let trimmed = user_message.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lower = trimmed.to_ascii_lowercase();
    let mentions_internal_details_en = lower.contains("command")
        || lower.contains("tool call")
        || lower.contains("function call")
        || lower.contains("execution trace")
        || lower.contains("internal step");
    let mentions_internal_details_cjk = trimmed.contains("命令")
        || trimmed.contains("工具调用")
        || trimmed.contains("函数调用")
        || trimmed.contains("执行过程");

    // Fail closed for negated phrasing ("don't show commands", "不要显示命令").
    const ENGLISH_NEGATIVE_HINTS: [&str; 18] = [
        "don't show command",
        "don't show commands",
        "do not show command",
        "do not show commands",
        "don't output command",
        "do not output command",
        "without command",
        "without commands",
        "no command output",
        "hide command",
        "hide commands",
        "omit command",
        "omit commands",
        "skip command",
        "skip commands",
        "don't show tool call",
        "do not show tool call",
        "do not show function call",
    ];
    if mentions_internal_details_en
        && ENGLISH_NEGATIVE_HINTS
            .iter()
            .any(|hint| lower.contains(hint))
    {
        return false;
    }

    const CJK_NEGATIVE_HINTS: [&str; 22] = [
        "不要输出命令",
        "不要显示命令",
        "不要展示命令",
        "不要带上命令",
        "不要附上命令",
        "别输出命令",
        "别显示命令",
        "别展示命令",
        "不要输出工具调用",
        "不要显示工具调用",
        "不要展示工具调用",
        "别输出工具调用",
        "别显示工具调用",
        "不要输出函数调用",
        "不要显示函数调用",
        "不要展示函数调用",
        "别输出函数调用",
        "别显示函数调用",
        "不要执行过程",
        "不要过程",
        "不要内部步骤",
        "别把命令",
    ];
    if mentions_internal_details_cjk && CJK_NEGATIVE_HINTS.iter().any(|hint| trimmed.contains(hint))
    {
        return false;
    }

    const ENGLISH_HINTS: [&str; 20] = [
        "show command",
        "show commands",
        "output command",
        "output commands",
        "print command",
        "print commands",
        "include command",
        "include commands",
        "with command",
        "with commands",
        "show tool call",
        "show tool calls",
        "show function call",
        "show function calls",
        "reveal tool call",
        "reveal function call",
        "tool call json",
        "function call json",
        "execution trace",
        "internal steps",
    ];
    if ENGLISH_HINTS.iter().any(|hint| lower.contains(hint)) {
        return true;
    }

    const ENGLISH_VERBS: [&str; 7] = [
        "show", "output", "print", "include", "reveal", "display", "share",
    ];
    if mentions_internal_details_en && ENGLISH_VERBS.iter().any(|verb| lower.contains(verb)) {
        return true;
    }

    const CJK_HINTS: [&str; 14] = [
        "输出命令",
        "显示命令",
        "展示命令",
        "命令发给我",
        "带上命令",
        "输出工具调用",
        "显示工具调用",
        "展示工具调用",
        "输出函数调用",
        "显示函数调用",
        "展示函数调用",
        "函数指令",
        "工具指令",
        "执行过程",
    ];
    if CJK_HINTS.iter().any(|hint| trimmed.contains(hint)) {
        return true;
    }

    const CJK_VERBS: [&str; 9] = [
        "输出", "显示", "展示", "发我", "给我", "带上", "附上", "贴出", "列出",
    ];
    mentions_internal_details_cjk && CJK_VERBS.iter().any(|verb| trimmed.contains(verb))
}

fn split_internal_progress_delta(delta: &str) -> (bool, &str) {
    if let Some(rest) = delta.strip_prefix(crate::agent::loop_::DRAFT_PROGRESS_SENTINEL) {
        (true, rest)
    } else {
        (false, delta)
    }
}

fn effective_progress_mode_for_message(
    channel_name: &str,
    expose_internal_tool_details: bool,
) -> ProgressMode {
    if channel_name.eq_ignore_ascii_case("cli") || expose_internal_tool_details {
        ProgressMode::Verbose
    } else if channel_name.eq_ignore_ascii_case("telegram") {
        runtime_telegram_progress_mode()
    } else {
        ProgressMode::Off
    }
}

fn is_verbose_only_progress_line(delta: &str) -> bool {
    let trimmed = delta.trim_start();
    trimmed.starts_with("\u{1f914} Thinking")
        || trimmed.starts_with("\u{1f4ac} Got ")
        || trimmed.starts_with("\u{21bb} Retrying")
        || trimmed.starts_with("\u{26a0}\u{fe0f} Loop detected")
}

fn upsert_progress_section(accumulated: &mut String, block: &str) {
    let section = format!(
        "{}{}{}",
        crate::agent::loop_::DRAFT_PROGRESS_SECTION_START,
        block,
        crate::agent::loop_::DRAFT_PROGRESS_SECTION_END
    );
    if let Some(start) = accumulated.find(crate::agent::loop_::DRAFT_PROGRESS_SECTION_START) {
        if let Some(end_offset) =
            accumulated[start..].find(crate::agent::loop_::DRAFT_PROGRESS_SECTION_END)
        {
            let end = start + end_offset + crate::agent::loop_::DRAFT_PROGRESS_SECTION_END.len();
            accumulated.replace_range(start..end, &section);
            return;
        }
    }
    accumulated.push_str(&section);
}

fn strip_progress_section_markers(text: &str) -> String {
    text.replace(crate::agent::loop_::DRAFT_PROGRESS_SECTION_START, "")
        .replace(crate::agent::loop_::DRAFT_PROGRESS_SECTION_END, "")
}
fn build_channel_system_prompt(
    base_prompt: &str,
    channel_name: &str,
    reply_target: &str,
    expose_internal_tool_details: bool,
) -> String {
    let mut prompt = base_prompt.to_string();
    crate::agent::prompt::refresh_prompt_datetime(&mut prompt);

    if let Some(instructions) = channel_delivery_instructions(channel_name) {
        if prompt.is_empty() {
            prompt = instructions.to_string();
        } else {
            prompt = format!("{prompt}\n\n{instructions}");
        }
    }

    if channel_name != "cli" {
        let visibility_instruction = if expose_internal_tool_details {
            "Execution visibility: the user explicitly requested command/tool details. \
             You may include command lines or tool-step traces when directly relevant, \
             but keep credentials and secrets redacted."
        } else {
            "Execution visibility: run tools/functions in the background and return an \
             integrated final result. Do not reveal raw tool names, tool-call syntax, \
             function arguments, shell commands, or internal execution traces unless the \
             user explicitly asks for those details."
        };

        if prompt.is_empty() {
            prompt = visibility_instruction.to_string();
        } else {
            prompt = format!("{prompt}\n\n{visibility_instruction}");
        }
    }

    if !reply_target.is_empty() {
        let context = format!(
            "\n\nChannel context: You are currently responding on channel={channel_name}, \
             reply_target={reply_target}. When scheduling delayed messages or reminders \
             via cron_add for this conversation, use delivery={{\"mode\":\"announce\",\
             \"channel\":\"{channel_name}\",\"to\":\"{reply_target}\"}} so the message \
             reaches the user."
        );
        prompt.push_str(&context);
    }

    prompt
}

fn normalize_cached_channel_turns(turns: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut normalized = Vec::with_capacity(turns.len());
    let mut expecting_user = true;

    for turn in turns {
        match (expecting_user, turn.role.as_str()) {
            (true, "user") => {
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
                if let Some(last_turn) = normalized.last_mut() {
                    if !turn.content.is_empty() {
                        if !last_turn.content.is_empty() {
                            last_turn.content.push_str("\n\n");
                        }
                        last_turn.content.push_str(&turn.content);
                    }
                }
            }
            _ => {}
        }
    }

    normalized
}

fn supports_runtime_model_switch(channel_name: &str) -> bool {
    matches!(channel_name, "telegram" | "discord")
}

fn parse_runtime_command(channel_name: &str, content: &str) -> Option<ChannelRuntimeCommand> {
    let trimmed = content.trim();
    if !trimmed.starts_with('/') {
        return parse_natural_language_runtime_command(trimmed);
    }

    let mut parts = trimmed.split_whitespace();
    let command_token = parts.next()?;
    let base_command = command_token
        .split('@')
        .next()
        .unwrap_or(command_token)
        .to_ascii_lowercase();
    let args: Vec<&str> = parts.collect();
    let tail = args.join(" ").trim().to_string();

    match base_command.as_str() {
        // History reset commands are safe for all channels.
        "/new" | "/clear" => Some(ChannelRuntimeCommand::NewSession),
        "/approve-all-once" => Some(ChannelRuntimeCommand::RequestAllToolsOnce),
        "/approve-request" => Some(ChannelRuntimeCommand::RequestToolApproval(tail)),
        "/approve-confirm" => Some(ChannelRuntimeCommand::ConfirmToolApproval(tail)),
        "/approve-allow" => Some(ChannelRuntimeCommand::ApprovePendingRequest(tail)),
        "/approve-deny" => Some(ChannelRuntimeCommand::DenyToolApproval(tail)),
        "/approve-pending" => Some(ChannelRuntimeCommand::ListPendingApprovals),
        "/approve" => Some(ChannelRuntimeCommand::ApproveTool(tail)),
        "/unapprove" => Some(ChannelRuntimeCommand::UnapproveTool(tail)),
        "/approvals" => Some(ChannelRuntimeCommand::ListApprovals),
        // Provider/model switching remains limited to channels with session routing.
        "/models" if supports_runtime_model_switch(channel_name) => {
            if let Some(provider) = args.first() {
                Some(ChannelRuntimeCommand::SetProvider(
                    provider.trim().to_string(),
                ))
            } else {
                Some(ChannelRuntimeCommand::ShowProviders)
            }
        }
        "/model" if supports_runtime_model_switch(channel_name) => {
            let model = tail;
            if model.is_empty() {
                Some(ChannelRuntimeCommand::ShowModel)
            } else {
                Some(ChannelRuntimeCommand::SetModel(model))
            }
        }
        _ => None,
    }
}

fn is_runtime_token(value: &str) -> bool {
    let token = value.trim();
    !token.is_empty()
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
}

fn extract_runtime_tail_token(text: &str, prefixes: &[&str]) -> Option<String> {
    prefixes.iter().find_map(|prefix| {
        text.strip_prefix(prefix).and_then(|rest| {
            let token = rest.trim();
            if is_runtime_token(token) {
                Some(token.to_string())
            } else {
                None
            }
        })
    })
}

fn contains_any_fragment(haystack: &str, fragments: &[&str]) -> bool {
    fragments.iter().any(|fragment| haystack.contains(fragment))
}

fn is_natural_language_all_tools_once_intent(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lower = trimmed.to_ascii_lowercase();
    let has_allow_verb = contains_any_fragment(&lower, &["approve", "allow"])
        || contains_any_fragment(trimmed, &["授权", "放开", "允许"]);
    let has_all_tools_scope = contains_any_fragment(&lower, &["all tools", "all commands"])
        || contains_any_fragment(trimmed, &["所有工具", "全部工具", "所有命令", "全部命令"]);
    let has_one_time_scope = contains_any_fragment(&lower, &["once", "one-time", "one time"])
        || contains_any_fragment(trimmed, &["一次", "这次"]);

    has_allow_verb && has_all_tools_scope && has_one_time_scope
}

fn approval_target_label(tool_name: &str) -> String {
    if tool_name == APPROVAL_ALL_TOOLS_ONCE_TOKEN {
        "all tools/commands (one-time bypass token)".to_string()
    } else {
        tool_name.to_string()
    }
}

fn parse_natural_language_runtime_command(content: &str) -> Option<ChannelRuntimeCommand> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "show pending approvals" | "list pending approvals" | "pending approvals"
    ) {
        return Some(ChannelRuntimeCommand::ListPendingApprovals);
    }
    if trimmed == "查看授权"
        || matches!(
            lower.as_str(),
            "show approvals" | "list approvals" | "approvals"
        )
    {
        return Some(ChannelRuntimeCommand::ListApprovals);
    }
    if is_natural_language_all_tools_once_intent(trimmed)
        || matches!(
            lower.as_str(),
            "approve all tools once" | "allow all tools once" | "approve all once"
        )
    {
        return Some(ChannelRuntimeCommand::RequestAllToolsOnce);
    }

    if let Some(request_id) = extract_runtime_tail_token(&lower, &["confirm "]) {
        return Some(ChannelRuntimeCommand::ConfirmToolApproval(request_id));
    }
    if let Some(request_id) = extract_runtime_tail_token(trimmed, &["确认授权 "]) {
        return Some(ChannelRuntimeCommand::ConfirmToolApproval(request_id));
    }

    if let Some(tool) =
        extract_runtime_tail_token(&lower, &["revoke tool ", "unapprove ", "revoke "])
    {
        return Some(ChannelRuntimeCommand::UnapproveTool(tool));
    }
    if let Some(tool) = extract_runtime_tail_token(trimmed, &["撤销工具 ", "取消授权 "]) {
        return Some(ChannelRuntimeCommand::UnapproveTool(tool));
    }

    if let Some(tool) = extract_runtime_tail_token(&lower, &["approve tool ", "approve "]) {
        return Some(ChannelRuntimeCommand::RequestToolApproval(tool));
    }
    if let Some(tool) = extract_runtime_tail_token(trimmed, &["授权工具 ", "请放开 ", "放开 "])
    {
        return Some(ChannelRuntimeCommand::RequestToolApproval(tool));
    }

    None
}

fn is_approval_management_command(command: &ChannelRuntimeCommand) -> bool {
    matches!(
        command,
        ChannelRuntimeCommand::RequestAllToolsOnce
            | ChannelRuntimeCommand::RequestToolApproval(_)
            | ChannelRuntimeCommand::ConfirmToolApproval(_)
            | ChannelRuntimeCommand::ApprovePendingRequest(_)
            | ChannelRuntimeCommand::DenyToolApproval(_)
            | ChannelRuntimeCommand::ListPendingApprovals
            | ChannelRuntimeCommand::ApproveTool(_)
            | ChannelRuntimeCommand::UnapproveTool(_)
            | ChannelRuntimeCommand::ListApprovals
    )
}

fn non_cli_natural_language_mode_label(mode: NonCliNaturalLanguageApprovalMode) -> &'static str {
    match mode {
        NonCliNaturalLanguageApprovalMode::Disabled => "disabled",
        NonCliNaturalLanguageApprovalMode::RequestConfirm => "request_confirm",
        NonCliNaturalLanguageApprovalMode::Direct => "direct",
    }
}

fn resolve_provider_alias(name: &str) -> Option<String> {
    let candidate = name.trim();
    if candidate.is_empty() {
        return None;
    }

    let providers_list = providers::list_providers();
    for provider in providers_list {
        if provider.name.eq_ignore_ascii_case(candidate)
            || provider
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(candidate))
        {
            return Some(provider.name.to_string());
        }
    }

    None
}

fn resolved_default_provider(config: &Config) -> String {
    config
        .default_provider
        .clone()
        .unwrap_or_else(|| "openrouter".to_string())
}

fn resolved_default_model(config: &Config) -> String {
    crate::config::resolve_default_model_id(
        config.default_model.as_deref(),
        config.default_provider.as_deref(),
    )
}

fn runtime_defaults_from_config(config: &Config) -> ChannelRuntimeDefaults {
    let message_timeout_secs =
        effective_channel_message_timeout_secs(config.channels_config.message_timeout_secs);
    let interrupt_on_new_message = config
        .channels_config
        .telegram
        .as_ref()
        .is_some_and(|tg| tg.interrupt_on_new_message);

    ChannelRuntimeDefaults {
        default_provider: resolved_default_provider(config),
        model: resolved_default_model(config),
        temperature: config.default_temperature,
        api_key: config.api_key.clone(),
        api_url: config.api_url.clone(),
        reliability: config.reliability.clone(),
        cost: config.cost.clone(),
        auto_save_memory: config.memory.auto_save,
        max_tool_iterations: config.agent.max_tool_iterations,
        min_relevance_score: config.memory.min_relevance_score,
        message_timeout_secs,
        interrupt_on_new_message,
        multimodal: config.multimodal.clone(),
        query_classification: config.query_classification.clone(),
        model_routes: config.model_routes.clone(),
    }
}

fn runtime_autonomy_policy_from_config(config: &Config) -> RuntimeAutonomyPolicy {
    RuntimeAutonomyPolicy {
        auto_approve: config.autonomy.auto_approve.clone(),
        always_ask: config.autonomy.always_ask.clone(),
        non_cli_excluded_tools: config.autonomy.non_cli_excluded_tools.clone(),
        non_cli_approval_approvers: config.autonomy.non_cli_approval_approvers.clone(),
        non_cli_natural_language_approval_mode: config
            .autonomy
            .non_cli_natural_language_approval_mode,
        non_cli_natural_language_approval_mode_by_channel: config
            .autonomy
            .non_cli_natural_language_approval_mode_by_channel
            .clone(),
        perplexity_filter: config.security.perplexity_filter.clone(),
        outbound_leak_guard: config.security.outbound_leak_guard.clone(),
    }
}

fn runtime_config_path(ctx: &ChannelRuntimeContext) -> Option<PathBuf> {
    ctx.provider_runtime_options
        .zeroclaw_dir
        .as_ref()
        .map(|dir| dir.join("config.toml"))
}

fn runtime_defaults_snapshot(ctx: &ChannelRuntimeContext) -> ChannelRuntimeDefaults {
    if let Some(config_path) = runtime_config_path(ctx) {
        let store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(state) = store.get(&config_path) {
            return state.defaults.clone();
        }
    }

    ChannelRuntimeDefaults {
        default_provider: ctx.default_provider.as_str().to_string(),
        model: ctx.model.as_str().to_string(),
        temperature: ctx.temperature,
        api_key: ctx.api_key.clone(),
        api_url: ctx.api_url.clone(),
        reliability: (*ctx.reliability).clone(),
        cost: crate::config::CostConfig::default(),
        auto_save_memory: ctx.auto_save_memory,
        max_tool_iterations: ctx.max_tool_iterations,
        min_relevance_score: ctx.min_relevance_score,
        message_timeout_secs: ctx.message_timeout_secs,
        interrupt_on_new_message: ctx.interrupt_on_new_message,
        multimodal: ctx.multimodal.clone(),
        query_classification: ctx.query_classification.clone(),
        model_routes: ctx.model_routes.clone(),
    }
}

/// Return a snapshot of the runtime perplexity-filter config, falling back to
/// the value stored on `ChannelRuntimeContext` when no runtime config override
/// has been applied yet.
fn runtime_perplexity_filter_snapshot(
    ctx: &ChannelRuntimeContext,
) -> crate::config::PerplexityFilterConfig {
    if let Some(config_path) = runtime_config_path(ctx) {
        let store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(state) = store.get(&config_path) {
            return state.perplexity_filter.clone();
        }
    }
    // Fallback to startup snapshot to preserve originally configured policy
    ctx.startup_perplexity_filter.clone()
}

fn runtime_outbound_leak_guard_snapshot(
    ctx: &ChannelRuntimeContext,
) -> crate::config::OutboundLeakGuardConfig {
    if let Some(config_path) = runtime_config_path(ctx) {
        let store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(state) = store.get(&config_path) {
            return state.outbound_leak_guard.clone();
        }
    }
    crate::config::OutboundLeakGuardConfig::default()
}
fn snapshot_non_cli_excluded_tools(ctx: &ChannelRuntimeContext) -> Vec<String> {
    ctx.non_cli_excluded_tools
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn filtered_tool_specs_for_runtime(
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
) -> Vec<crate::tools::ToolSpec> {
    tools_registry
        .iter()
        .map(|tool| tool.spec())
        .filter(|spec| !excluded_tools.iter().any(|excluded| excluded == &spec.name))
        .collect()
}

fn build_runtime_tool_visibility_prompt(
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
    native_tools: bool,
) -> String {
    let mut prompt = String::new();
    let mut specs = filtered_tool_specs_for_runtime(tools_registry, excluded_tools);
    specs.sort_by(|a, b| a.name.cmp(&b.name));

    use std::fmt::Write;
    prompt.push_str("\n## Runtime Tool Availability (Authoritative)\n\n");
    prompt.push_str(
        "This section is generated from current runtime policy for this message. \
         Only the listed tools may be called in this turn.\n\n",
    );

    if specs.is_empty() {
        prompt.push_str("- Allowed tools: (none)\n");
    } else {
        let _ = writeln!(prompt, "- Allowed tools ({}):", specs.len());
        for spec in &specs {
            let _ = writeln!(prompt, "  - `{}`", spec.name);
        }
    }

    if excluded_tools.is_empty() {
        prompt.push_str("- Excluded by runtime policy: (none)\n\n");
    } else {
        let mut excluded_sorted = excluded_tools.to_vec();
        excluded_sorted.sort();
        let _ = writeln!(
            prompt,
            "- Excluded by runtime policy: {}\n",
            excluded_sorted.join(", ")
        );
    }

    if native_tools {
        prompt.push_str(
            "Tool calling for this turn uses native provider function-calling. \
             Do not emit `<tool_call>` XML tags.\n",
        );
    } else {
        prompt.push_str(
            "Tool calling for this turn uses XML tool protocol below. \
             This protocol block is generated from the same runtime policy snapshot.\n",
        );
        prompt.push_str(&build_tool_instructions_from_specs(&specs));
    }

    prompt
}

async fn config_file_stamp(path: &Path) -> Option<ConfigFileStamp> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let modified = metadata.modified().ok()?;
    Some(ConfigFileStamp {
        modified,
        len: metadata.len(),
    })
}

fn decrypt_optional_secret_for_runtime_reload(
    store: &crate::security::SecretStore,
    value: &mut Option<String>,
    field_name: &str,
) -> Result<()> {
    if let Some(raw) = value.clone() {
        if crate::security::SecretStore::is_encrypted(&raw) {
            *value = Some(
                store
                    .decrypt(&raw)
                    .with_context(|| format!("Failed to decrypt {field_name}"))?,
            );
        }
    }
    Ok(())
}

async fn load_runtime_defaults_from_config_file(
    path: &Path,
) -> Result<(ChannelRuntimeDefaults, RuntimeAutonomyPolicy)> {
    let contents = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut parsed: Config =
        toml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))?;
    parsed.config_path = path.to_path_buf();

    if let Some(zeroclaw_dir) = path.parent() {
        let store = crate::security::SecretStore::new(zeroclaw_dir, parsed.secrets.encrypt);
        decrypt_optional_secret_for_runtime_reload(&store, &mut parsed.api_key, "config.api_key")?;
        decrypt_optional_secret_for_runtime_reload(
            &store,
            &mut parsed.transcription.api_key,
            "config.transcription.api_key",
        )?;
    }

    parsed.apply_env_overrides();
    Ok((
        runtime_defaults_from_config(&parsed),
        runtime_autonomy_policy_from_config(&parsed),
    ))
}

async fn persist_non_cli_approval_to_config(
    ctx: &ChannelRuntimeContext,
    tool_name: &str,
) -> Result<Option<PathBuf>> {
    let Some(config_path) = runtime_config_path(ctx) else {
        return Ok(None);
    };

    let contents = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let mut parsed: Config = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;
    parsed.config_path = config_path.clone();

    let mut changed = false;
    if !parsed
        .autonomy
        .auto_approve
        .iter()
        .any(|entry| entry == tool_name)
    {
        parsed.autonomy.auto_approve.push(tool_name.to_string());
        changed = true;
    }

    let before_always_ask = parsed.autonomy.always_ask.len();
    parsed
        .autonomy
        .always_ask
        .retain(|entry| entry != tool_name);
    if parsed.autonomy.always_ask.len() != before_always_ask {
        changed = true;
    }

    if changed {
        parsed.save().await?;
    }

    Ok(Some(config_path))
}

async fn remove_non_cli_approval_from_config(
    ctx: &ChannelRuntimeContext,
    tool_name: &str,
) -> Result<Option<(PathBuf, bool)>> {
    let Some(config_path) = runtime_config_path(ctx) else {
        return Ok(None);
    };

    let contents = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let mut parsed: Config = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;
    parsed.config_path = config_path.clone();

    let before_auto_approve = parsed.autonomy.auto_approve.len();
    parsed
        .autonomy
        .auto_approve
        .retain(|entry| entry != tool_name);
    let removed = parsed.autonomy.auto_approve.len() != before_auto_approve;
    if removed {
        parsed.save().await?;
    }

    Ok(Some((config_path, removed)))
}

fn remove_non_cli_tool_exclusion_from_runtime(
    ctx: &ChannelRuntimeContext,
    tool_name: &str,
) -> bool {
    let mut excluded = ctx
        .non_cli_excluded_tools
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before_len = excluded.len();
    excluded.retain(|entry| entry != tool_name);
    excluded.len() != before_len
}

async fn remove_non_cli_excluded_tool_from_config(
    ctx: &ChannelRuntimeContext,
    tool_name: &str,
) -> Result<Option<(PathBuf, bool)>> {
    let Some(config_path) = runtime_config_path(ctx) else {
        return Ok(None);
    };

    let contents = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let mut parsed: Config = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;
    parsed.config_path = config_path.clone();

    let before_len = parsed.autonomy.non_cli_excluded_tools.len();
    parsed
        .autonomy
        .non_cli_excluded_tools
        .retain(|entry| entry != tool_name);
    let removed = parsed.autonomy.non_cli_excluded_tools.len() != before_len;
    if removed {
        parsed.save().await?;
    }

    Ok(Some((config_path, removed)))
}

async fn clear_non_cli_exclusion_after_approval(
    ctx: &ChannelRuntimeContext,
    tool_name: &str,
) -> Option<String> {
    let runtime_removed = remove_non_cli_tool_exclusion_from_runtime(ctx, tool_name);
    match remove_non_cli_excluded_tool_from_config(ctx, tool_name).await {
        Ok(Some((path, persisted_removed))) => match (runtime_removed, persisted_removed) {
            (true, true) => Some(format!(
                "Removed `{tool_name}` from `autonomy.non_cli_excluded_tools` in runtime and persisted config (`{}`).",
                path.display()
            )),
            (true, false) => Some(format!(
                "Removed `{tool_name}` from runtime `autonomy.non_cli_excluded_tools` (it was already absent from persisted config `{}`).",
                path.display()
            )),
            (false, true) => Some(format!(
                "Removed `{tool_name}` from persisted `autonomy.non_cli_excluded_tools` in `{}`.",
                path.display()
            )),
            (false, false) => None,
        },
        Ok(None) => runtime_removed.then(|| {
            format!(
                "Removed `{tool_name}` from runtime `autonomy.non_cli_excluded_tools`."
            )
        }),
        Err(err) => {
            if runtime_removed {
                Some(format!(
                    "Removed `{tool_name}` from runtime `autonomy.non_cli_excluded_tools`, but failed to persist config update: {err}"
                ))
            } else {
                Some(format!(
                    "Failed to update persisted `autonomy.non_cli_excluded_tools` for `{tool_name}`: {err}"
                ))
            }
        }
    }
}

async fn describe_non_cli_approvals(
    ctx: &ChannelRuntimeContext,
    sender: &str,
    channel: &str,
    reply_target: &str,
) -> Result<String> {
    let mut response = String::new();
    response.push_str("Supervised non-CLI tool approvals:\n");

    let mut runtime_auto = ctx
        .approval_manager
        .auto_approve_tools()
        .into_iter()
        .collect::<Vec<_>>();
    runtime_auto.sort();
    if runtime_auto.is_empty() {
        response.push_str("- Runtime auto_approve (effective): (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Runtime auto_approve (effective): {}",
            runtime_auto.join(", ")
        );
    }

    let mut runtime_always = ctx
        .approval_manager
        .always_ask_tools()
        .into_iter()
        .collect::<Vec<_>>();
    runtime_always.sort();
    if runtime_always.is_empty() {
        response.push_str("- Runtime always_ask (effective): (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Runtime always_ask (effective): {}",
            runtime_always.join(", ")
        );
    }

    let mut session_grants = ctx
        .approval_manager
        .non_cli_session_allowlist()
        .into_iter()
        .collect::<Vec<_>>();
    session_grants.sort();
    if session_grants.is_empty() {
        response.push_str("- Runtime session grants: (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Runtime session grants: {}",
            session_grants.join(", ")
        );
    }
    let one_time_all_tools_tokens = ctx.approval_manager.non_cli_allow_all_once_remaining();
    let _ = writeln!(
        response,
        "- Runtime one-time all-tools bypass tokens: {}",
        one_time_all_tools_tokens
    );

    let mut approval_approvers = ctx
        .approval_manager
        .non_cli_approval_approvers()
        .into_iter()
        .collect::<Vec<_>>();
    approval_approvers.sort();
    if approval_approvers.is_empty() {
        response.push_str("- Runtime non_cli_approval_approvers: (any channel-allowed sender)\n");
    } else {
        let _ = writeln!(
            response,
            "- Runtime non_cli_approval_approvers: {}",
            approval_approvers.join(", ")
        );
    }

    let default_mode = non_cli_natural_language_mode_label(
        ctx.approval_manager
            .non_cli_natural_language_approval_mode(),
    );
    let effective_mode = non_cli_natural_language_mode_label(
        ctx.approval_manager
            .non_cli_natural_language_approval_mode_for_channel(channel),
    );
    let _ = writeln!(
        response,
        "- Runtime non_cli_natural_language_approval_mode: {}",
        default_mode
    );
    let _ = writeln!(
        response,
        "- Runtime non_cli_natural_language_approval_mode (current channel `{channel}`): {}",
        effective_mode
    );
    let mut mode_overrides = ctx
        .approval_manager
        .non_cli_natural_language_approval_mode_by_channel()
        .into_iter()
        .map(|(ch, mode)| format!("{ch}={}", non_cli_natural_language_mode_label(mode)))
        .collect::<Vec<_>>();
    mode_overrides.sort();
    if mode_overrides.is_empty() {
        response.push_str("- Runtime non_cli_natural_language_approval_mode_by_channel: (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Runtime non_cli_natural_language_approval_mode_by_channel: {}",
            mode_overrides.join(", ")
        );
    }

    let mut pending_requests = ctx.approval_manager.list_non_cli_pending_requests(
        Some(sender),
        Some(channel),
        Some(reply_target),
    );
    pending_requests.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if pending_requests.is_empty() {
        response.push_str("- Pending approvals (sender+chat/channel scoped): (none)\n");
    } else {
        response.push_str("- Pending approvals (sender+chat/channel scoped):\n");
        for req in pending_requests {
            let reason = req
                .reason
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("n/a");
            let _ = writeln!(
                response,
                "  - {}: tool={}, expires_at={}, reason={}",
                req.request_id,
                approval_target_label(&req.tool_name),
                req.expires_at,
                reason
            );
        }
    }

    let mut excluded = snapshot_non_cli_excluded_tools(ctx);
    excluded.sort();
    if excluded.is_empty() {
        response.push_str("- Runtime non_cli_excluded_tools: (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Runtime non_cli_excluded_tools: {}",
            excluded.join(", ")
        );
    }

    let Some(config_path) = runtime_config_path(ctx) else {
        response.push_str(
            "- Persisted config approvals: unavailable (runtime config path not resolved)\n",
        );
        return Ok(response);
    };

    let contents = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let parsed: Config = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let mut auto_approve = parsed.autonomy.auto_approve;
    auto_approve.sort();
    if auto_approve.is_empty() {
        response.push_str("- Persisted autonomy.auto_approve: (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Persisted autonomy.auto_approve: {}",
            auto_approve.join(", ")
        );
    }

    let mut always_ask = parsed.autonomy.always_ask;
    always_ask.sort();
    if always_ask.is_empty() {
        response.push_str("- Persisted autonomy.always_ask: (none)\n");
    } else {
        let _ = writeln!(
            response,
            "- Persisted autonomy.always_ask: {}",
            always_ask.join(", ")
        );
    }

    let _ = writeln!(response, "- Config path: {}", config_path.display());
    Ok(response)
}

async fn maybe_apply_runtime_config_update(ctx: &ChannelRuntimeContext) -> Result<()> {
    let Some(config_path) = runtime_config_path(ctx) else {
        return Ok(());
    };

    let Some(stamp) = config_file_stamp(&config_path).await else {
        return Ok(());
    };

    {
        let store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(state) = store.get(&config_path) {
            if state.last_applied_stamp == Some(stamp) {
                return Ok(());
            }
        }
    }

    let (next_defaults, next_autonomy_policy) =
        load_runtime_defaults_from_config_file(&config_path).await?;
    let next_default_provider = providers::create_resilient_provider_with_options(
        &next_defaults.default_provider,
        next_defaults.api_key.as_deref(),
        next_defaults.api_url.as_deref(),
        &next_defaults.reliability,
        &ctx.provider_runtime_options,
    )?;
    let next_default_provider: Arc<dyn Provider> = Arc::from(next_default_provider);

    if let Err(err) = next_default_provider.warmup().await {
        tracing::warn!(
            provider = %next_defaults.default_provider,
            "Provider warmup failed after config reload: {err}"
        );
    }

    {
        let mut cache = ctx.provider_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.clear();
        cache.insert(
            next_defaults.default_provider.clone(),
            Arc::clone(&next_default_provider),
        );
    }

    {
        let mut store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.insert(
            config_path.clone(),
            RuntimeConfigState {
                defaults: next_defaults.clone(),
                perplexity_filter: next_autonomy_policy.perplexity_filter.clone(),
                outbound_leak_guard: next_autonomy_policy.outbound_leak_guard.clone(),
                last_applied_stamp: Some(stamp),
            },
        );
    }

    ctx.approval_manager.replace_runtime_non_cli_policy(
        &next_autonomy_policy.auto_approve,
        &next_autonomy_policy.always_ask,
        &next_autonomy_policy.non_cli_approval_approvers,
        next_autonomy_policy.non_cli_natural_language_approval_mode,
        &next_autonomy_policy.non_cli_natural_language_approval_mode_by_channel,
    );
    {
        let mut excluded = ctx
            .non_cli_excluded_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *excluded = next_autonomy_policy.non_cli_excluded_tools.clone();
    }

    tracing::info!(
        path = %config_path.display(),
        provider = %next_defaults.default_provider,
        model = %next_defaults.model,
        temperature = next_defaults.temperature,
        non_cli_approval_mode = %non_cli_natural_language_mode_label(
            next_autonomy_policy.non_cli_natural_language_approval_mode
        ),
        non_cli_excluded_tools_count = next_autonomy_policy.non_cli_excluded_tools.len(),
        perplexity_filter_enabled = next_autonomy_policy.perplexity_filter.enable_perplexity_filter,
        perplexity_threshold = next_autonomy_policy.perplexity_filter.perplexity_threshold,
        outbound_leak_guard_enabled = next_autonomy_policy.outbound_leak_guard.enabled,
        outbound_leak_guard_action = ?next_autonomy_policy.outbound_leak_guard.action,
        outbound_leak_guard_sensitivity = next_autonomy_policy.outbound_leak_guard.sensitivity,
        "Applied updated channel runtime config from disk"
    );

    Ok(())
}

fn default_route_selection(ctx: &ChannelRuntimeContext) -> ChannelRouteSelection {
    let defaults = runtime_defaults_snapshot(ctx);
    ChannelRouteSelection {
        provider: defaults.default_provider,
        model: defaults.model,
    }
}

fn get_route_selection(ctx: &ChannelRuntimeContext, sender_key: &str) -> ChannelRouteSelection {
    ctx.route_overrides
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(sender_key)
        .cloned()
        .unwrap_or_else(|| default_route_selection(ctx))
}

/// Classify a user message and return the appropriate route selection with logging.
/// Returns None if classification is disabled or no rules match.
fn classify_message_route(
    query_classification: &crate::config::QueryClassificationConfig,
    model_routes: &[crate::config::ModelRouteConfig],
    message: &str,
) -> Option<ChannelRouteSelection> {
    let decision = crate::agent::classifier::classify_with_decision(query_classification, message)?;

    // Find the matching model route
    let route = model_routes.iter().find(|r| r.hint == decision.hint)?;

    tracing::info!(
        target: "query_classification",
        hint = %decision.hint,
        model = %route.model,
        rule_priority = decision.priority,
        message_length = message.len(),
        "Classified message route"
    );

    Some(ChannelRouteSelection {
        provider: route.provider.clone(),
        model: route.model.clone(),
    })
}

fn set_route_selection(ctx: &ChannelRuntimeContext, sender_key: &str, next: ChannelRouteSelection) {
    let default_route = default_route_selection(ctx);
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
        .remove(sender_key);
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

fn append_sender_turn(ctx: &ChannelRuntimeContext, sender_key: &str, turn: ChatMessage) {
    let mut histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories.entry(sender_key.to_string()).or_default();
    turns.push(turn);
    while turns.len() > MAX_CHANNEL_HISTORY {
        turns.remove(0);
    }
}

fn estimated_message_tokens(message: &ChatMessage) -> usize {
    (message.content.chars().count().saturating_add(2) / 3).saturating_add(4)
}

fn estimated_history_tokens(history: &[ChatMessage]) -> usize {
    history.iter().map(estimated_message_tokens).sum()
}

fn trim_channel_prompt_history(history: &mut Vec<ChatMessage>) -> bool {
    let mut total = estimated_history_tokens(history);
    if total <= CHANNEL_CONTEXT_TOKEN_ESTIMATE_LIMIT {
        return false;
    }

    let mut trimmed = false;
    loop {
        if total <= CHANNEL_CONTEXT_TOKEN_ESTIMATE_TARGET {
            break;
        }
        let non_system = history.iter().filter(|m| m.role != "system").count();
        if non_system <= CHANNEL_CONTEXT_MIN_KEEP_NON_SYSTEM_MESSAGES {
            break;
        }
        let Some(idx) = history.iter().position(|m| m.role != "system") else {
            break;
        };
        let removed = history.remove(idx);
        total = total.saturating_sub(estimated_message_tokens(&removed));
        trimmed = true;
    }

    trimmed
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
        histories.remove(sender_key);
    }
    true
}

fn should_skip_memory_context_entry(key: &str, content: &str) -> bool {
    if memory::is_assistant_autosave_key(key) {
        return true;
    }

    if key.trim().to_ascii_lowercase().ends_with("_history") {
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

fn is_tool_iteration_limit_error(err: &anyhow::Error) -> bool {
    crate::agent::loop_::is_tool_iteration_limit_error(err)
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
        .find(|entry| entry.provider == provider_name)
        .map(|entry| {
            entry
                .models
                .into_iter()
                .take(MODEL_CACHE_PREVIEW_LIMIT)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

async fn get_or_create_provider(
    ctx: &ChannelRuntimeContext,
    provider_name: &str,
) -> anyhow::Result<Arc<dyn Provider>> {
    if let Some(existing) = ctx
        .provider_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(provider_name)
        .cloned()
    {
        return Ok(existing);
    }

    if provider_name == ctx.default_provider.as_str() {
        return Ok(Arc::clone(&ctx.provider));
    }

    let defaults = runtime_defaults_snapshot(ctx);
    let api_url = if provider_name == defaults.default_provider.as_str() {
        defaults.api_url.as_deref()
    } else {
        None
    };

    let provider = create_resilient_provider_nonblocking(
        provider_name,
        defaults.api_key.clone(),
        api_url.map(ToString::to_string),
        defaults.reliability.clone(),
        ctx.provider_runtime_options.clone(),
    )
    .await?;
    let provider: Arc<dyn Provider> = Arc::from(provider);

    if let Err(err) = provider.warmup().await {
        tracing::warn!(provider = provider_name, "Provider warmup failed: {err}");
    }

    let mut cache = ctx.provider_cache.lock().unwrap_or_else(|e| e.into_inner());
    let cached = cache
        .entry(provider_name.to_string())
        .or_insert_with(|| Arc::clone(&provider));
    Ok(Arc::clone(cached))
}

async fn create_resilient_provider_nonblocking(
    provider_name: &str,
    api_key: Option<String>,
    api_url: Option<String>,
    reliability: crate::config::ReliabilityConfig,
    provider_runtime_options: providers::ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let provider_name = provider_name.to_string();
    tokio::task::spawn_blocking(move || {
        providers::create_resilient_provider_with_options(
            &provider_name,
            api_key.as_deref(),
            api_url.as_deref(),
            &reliability,
            &provider_runtime_options,
        )
    })
    .await
    .context("failed to join provider initialization task")?
}

fn build_models_help_response(current: &ChannelRouteSelection, workspace_dir: &Path) -> String {
    let mut response = String::new();
    let _ = writeln!(
        response,
        "Current provider: `{}`\nCurrent model: `{}`",
        current.provider, current.model
    );
    response.push_str("\nSwitch model with `/model <model-id>`.\n");
    response.push_str("Request supervised tool approval with `/approve-request <tool-name>`.\n");
    response.push_str("Request one-time all-tools approval with `/approve-all-once`.\n");
    response.push_str("Confirm approval with `/approve-confirm <request-id>`.\n");
    response.push_str("Deny approval with `/approve-deny <request-id>`.\n");
    response.push_str("List pending requests with `/approve-pending`.\n");
    response.push_str("Approve supervised tools with `/approve <tool-name>`.\n");
    response.push_str("Revoke approval with `/unapprove <tool-name>`.\n");
    response.push_str("List approval state with `/approvals`.\n");
    response.push_str(
        "Natural language also works (policy controlled).\n\
         - `direct` mode (default): `授权工具 shell` grants immediately.\n\
         - `request_confirm` mode: `授权工具 shell` then `确认授权 apr-xxxxxx`.\n",
    );

    let cached_models = load_cached_model_preview(workspace_dir, &current.provider);
    if cached_models.is_empty() {
        let _ = writeln!(
            response,
            "\nNo cached model list found for `{}`. Ask the operator to run `zeroclaw models refresh --provider {}`.",
            current.provider, current.provider
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
        "Current provider: `{}`\nCurrent model: `{}`",
        current.provider, current.model
    );
    response.push_str("\nSwitch provider with `/models <provider>`.\n");
    response.push_str("Switch model with `/model <model-id>`.\n\n");
    response.push_str("Request supervised tool approval with `/approve-request <tool-name>`.\n");
    response.push_str("Request one-time all-tools approval with `/approve-all-once`.\n");
    response.push_str("Confirm approval with `/approve-confirm <request-id>`.\n");
    response.push_str("Deny approval with `/approve-deny <request-id>`.\n");
    response.push_str("List pending requests with `/approve-pending`.\n");
    response.push_str("Approve supervised tools with `/approve <tool-name>`.\n");
    response.push_str("Revoke approval with `/unapprove <tool-name>`.\n");
    response.push_str("List approval state with `/approvals`.\n");
    response.push_str(
        "Natural language also works (policy controlled).\n\
         - `direct` mode (default): `授权工具 shell` grants immediately.\n\
         - `request_confirm` mode: `授权工具 shell` then `确认授权 apr-xxxxxx`.\n\n",
    );
    response.push_str("Available providers:\n");
    for provider in providers::list_providers() {
        if provider.aliases.is_empty() {
            let _ = writeln!(response, "- {}", provider.name);
        } else {
            let _ = writeln!(
                response,
                "- {} (aliases: {})",
                provider.name,
                provider.aliases.join(", ")
            );
        }
    }
    response
}

async fn handle_runtime_command_if_needed(
    ctx: &ChannelRuntimeContext,
    msg: &traits::ChannelMessage,
    target_channel: Option<&Arc<dyn Channel>>,
) -> bool {
    let is_slash_command = msg.content.trim_start().starts_with('/');
    let Some(mut command) = parse_runtime_command(&msg.channel, &msg.content) else {
        return false;
    };

    let Some(channel) = target_channel else {
        return true;
    };

    let sender_key = conversation_history_key(msg);
    let mut current = get_route_selection(ctx, &sender_key);
    let sender = msg.sender.as_str();
    let source_channel = msg.channel.as_str();
    let reply_target = msg.reply_target.as_str();
    let is_natural_language_approval_command =
        !is_slash_command && is_approval_management_command(&command);

    if is_approval_management_command(&command)
        && !ctx
            .approval_manager
            .is_non_cli_approval_actor_allowed(source_channel, sender)
    {
        let mut approvers = ctx
            .approval_manager
            .non_cli_approval_approvers()
            .into_iter()
            .collect::<Vec<_>>();
        approvers.sort();
        let allowed = if approvers.is_empty() {
            "(any channel-allowed sender)".to_string()
        } else {
            approvers.join(", ")
        };
        let response = format!(
            "Approval-management command denied for sender `{sender}` on channel `{source_channel}`.\nAllowed approvers: {allowed}\nConfigure `[autonomy].non_cli_approval_approvers` to adjust this policy."
        );
        runtime_trace::record_event(
            "approval_management_denied",
            Some(source_channel),
            None,
            None,
            None,
            Some(false),
            Some("sender not allowed to manage non-cli approvals"),
            serde_json::json!({
                "sender": sender,
                "channel": source_channel,
                "allowed_approvers": approvers,
            }),
        );

        if let Err(err) = channel
            .send(&SendMessage::new(response, &msg.reply_target).in_thread(msg.thread_ts.clone()))
            .await
        {
            tracing::warn!(
                "Failed to send runtime command response on {}: {err}",
                channel.name()
            );
        }
        return true;
    }

    if is_natural_language_approval_command {
        let mode = ctx
            .approval_manager
            .non_cli_natural_language_approval_mode_for_channel(source_channel);
        match mode {
            NonCliNaturalLanguageApprovalMode::Disabled => {
                let response = "Natural-language approval commands are disabled by runtime policy.\nUse explicit slash commands such as `/approve <tool-name>`, `/approve-request <tool-name>`, `/approve-all-once`, `/approve-allow <request-id>`, `/approve-confirm <request-id>`, `/approve-deny <request-id>`, `/unapprove <tool-name>`, and `/approvals`.".to_string();
                runtime_trace::record_event(
                    "approval_management_natural_language_denied",
                    Some(source_channel),
                    None,
                    None,
                    None,
                    Some(false),
                    Some("natural-language approval commands disabled by policy"),
                    serde_json::json!({
                        "sender": sender,
                        "channel": source_channel,
                        "mode": non_cli_natural_language_mode_label(mode),
                    }),
                );
                if let Err(err) = channel
                    .send(
                        &SendMessage::new(response, &msg.reply_target)
                            .in_thread(msg.thread_ts.clone()),
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to send runtime command response on {}: {err}",
                        channel.name()
                    );
                }
                return true;
            }
            NonCliNaturalLanguageApprovalMode::RequestConfirm => {}
            NonCliNaturalLanguageApprovalMode::Direct => {
                if let ChannelRuntimeCommand::RequestToolApproval(tool_name) = &command {
                    command = ChannelRuntimeCommand::ApproveTool(tool_name.clone());
                    runtime_trace::record_event(
                        "approval_management_natural_language_promoted_to_direct",
                        Some(source_channel),
                        None,
                        None,
                        None,
                        Some(true),
                        Some("natural-language request promoted to direct approval"),
                        serde_json::json!({
                            "sender": sender,
                            "channel": source_channel,
                            "mode": non_cli_natural_language_mode_label(mode),
                        }),
                    );
                }
            }
        }
    }

    /// Handle the side effects of confirming a tool approval:
    /// - Grant session and persistent runtime grants
    /// - Persist to config
    /// - Clear exclusions
    /// Returns the approval success message.
    async fn handle_confirm_tool_approval_side_effects(
        ctx: &ChannelRuntimeContext,
        request_id: &str,
        tool_name: &str,
        _source_channel: &str,
    ) -> String {
        if tool_name == APPROVAL_ALL_TOOLS_ONCE_TOKEN {
            let remaining = ctx.approval_manager.grant_non_cli_allow_all_once();
            format!(
                "Approved one-time all-tools bypass from request `{request_id}`.\nApplies to the next non-CLI agent tool-execution turn only.\nThis bypass is runtime-only and does not persist to config.\nChannel exclusions from `autonomy.non_cli_excluded_tools` still apply.\nQueued one-time all-tools bypass tokens: `{remaining}`."
            )
        } else {
            ctx.approval_manager.grant_non_cli_session(tool_name);
            ctx.approval_manager
                .apply_persistent_runtime_grant(tool_name);
            let mut approval_message = match persist_non_cli_approval_to_config(ctx, tool_name).await {
                Ok(Some(path)) => format!(
                    "Approved supervised execution for `{tool_name}` from request `{request_id}`.\nPersisted to `{}` so future channel sessions (including after restart) remain approved.",
                    path.display()
                ),
                Ok(None) => format!(
                    "Approved supervised execution for `{tool_name}` from request `{request_id}`.\nNo runtime config path was found, so this approval is active for the current daemon runtime only."
                ),
                Err(err) => format!(
                    "Approved supervised execution for `{tool_name}` from request `{request_id}` in-memory.\nFailed to persist this approval to config: {err}"
                ),
            };
            if let Some(exclusion_note) =
                clear_non_cli_exclusion_after_approval(ctx, tool_name).await
            {
                approval_message.push('\n');
                approval_message.push_str(&exclusion_note);
            }
            approval_message
        }
    }

    /// Handle `/approve-allow <request-id>` for pending runtime execution prompts.
    ///
    /// This path confirms only the current pending request and intentionally does
    /// not persist approval policy changes for normal tools.
    async fn handle_pending_runtime_approval_side_effects(
        ctx: &ChannelRuntimeContext,
        request_id: &str,
        tool_name: &str,
    ) -> String {
        if tool_name == APPROVAL_ALL_TOOLS_ONCE_TOKEN {
            let remaining = ctx.approval_manager.grant_non_cli_allow_all_once();
            format!(
                "Approved one-time all-tools bypass from request `{request_id}`.\nApplies to the next non-CLI agent tool-execution turn only.\nThis bypass is runtime-only and does not persist to config.\nChannel exclusions from `autonomy.non_cli_excluded_tools` still apply.\nQueued one-time all-tools bypass tokens: `{remaining}`."
            )
        } else {
            format!(
                "Approved pending execution request `{request_id}` for `{tool_name}`.\nThis approval applies only to the current pending request and does not change persisted approval policy.\nTo persist approval for future requests, use `/approve {tool_name}` or the `/approve-request` + `/approve-confirm` flow."
            )
        }
    }

    let response = match command {
        ChannelRuntimeCommand::ShowProviders => build_providers_help_response(&current),
        ChannelRuntimeCommand::SetProvider(raw_provider) => {
            match resolve_provider_alias(&raw_provider) {
                Some(provider_name) => match get_or_create_provider(ctx, &provider_name).await {
                    Ok(_) => {
                        if provider_name != current.provider {
                            current.provider = provider_name.clone();
                            set_route_selection(ctx, &sender_key, current.clone());
                            clear_sender_history(ctx, &sender_key);
                        }

                        format!(
                            "Provider switched to `{provider_name}` for this sender session. Current model is `{}`.\nUse `/model <model-id>` to set a provider-compatible model.",
                            current.model
                        )
                    }
                    Err(err) => {
                        let safe_err = providers::sanitize_api_error(&err.to_string());
                        format!(
                            "Failed to initialize provider `{provider_name}`. Route unchanged.\nDetails: {safe_err}"
                        )
                    }
                },
                None => format!(
                    "Unknown provider `{raw_provider}`. Use `/models` to list valid providers."
                ),
            }
        }
        ChannelRuntimeCommand::ShowModel => {
            build_models_help_response(&current, ctx.workspace_dir.as_path())
        }
        ChannelRuntimeCommand::SetModel(raw_model) => {
            let model = raw_model.trim().trim_matches('`').to_string();
            if model.is_empty() {
                "Model ID cannot be empty. Use `/model <model-id>`.".to_string()
            } else {
                current.model = model.clone();
                set_route_selection(ctx, &sender_key, current.clone());
                clear_sender_history(ctx, &sender_key);

                format!(
                    "Model switched to `{model}` for provider `{}` in this sender session.",
                    current.provider
                )
            }
        }
        ChannelRuntimeCommand::NewSession => {
            clear_sender_history(ctx, &sender_key);
            "Conversation history cleared. Starting fresh.".to_string()
        }
        ChannelRuntimeCommand::RequestAllToolsOnce => {
            let req = ctx.approval_manager.create_non_cli_pending_request(
                APPROVAL_ALL_TOOLS_ONCE_TOKEN,
                sender,
                source_channel,
                reply_target,
                Some("human-confirmed one-time bypass request for all tools/commands".to_string()),
            );
            runtime_trace::record_event(
                "approval_request_created",
                Some(source_channel),
                None,
                None,
                None,
                Some(true),
                Some("pending one-time all-tools request created"),
                serde_json::json!({
                    "request_id": req.request_id,
                    "tool_name": req.tool_name,
                    "sender": sender,
                    "channel": source_channel,
                    "expires_at": req.expires_at,
                }),
            );
            format!(
                "One-time all-tools approval request created.\nRequest ID: `{}`\nScope: next non-CLI agent tool-execution turn may run without per-tool approval prompts.\nExpires: `{}`\nConfirm with `/approve-confirm {}` (must be the same sender in this chat/channel).",
                req.request_id, req.expires_at, req.request_id
            )
        }
        ChannelRuntimeCommand::RequestToolApproval(raw_tool_name) => {
            let tool_name = raw_tool_name.trim().to_string();
            if tool_name.is_empty() {
                "Usage: `/approve-request <tool-name>`".to_string()
            } else if !ctx
                .tools_registry
                .iter()
                .any(|tool| tool.name() == tool_name)
            {
                let mut available_tools = ctx
                    .tools_registry
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect::<Vec<_>>();
                available_tools.sort();
                let preview = available_tools
                    .into_iter()
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Unknown tool `{tool_name}`.\nKnown tools (top 12): {preview}\nUse `/approve-request <tool-name>` with an exact tool name."
                )
            } else if !ctx.approval_manager.needs_approval(&tool_name) {
                format!(
                    "`{tool_name}` is already approved in the current runtime policy. You can use it directly."
                )
            } else {
                let req = ctx.approval_manager.create_non_cli_pending_request(
                    &tool_name,
                    sender,
                    source_channel,
                    reply_target,
                    None,
                );
                runtime_trace::record_event(
                    "approval_request_created",
                    Some(source_channel),
                    None,
                    None,
                    None,
                    Some(true),
                    Some("pending request created"),
                    serde_json::json!({
                        "request_id": req.request_id,
                        "tool_name": req.tool_name,
                        "sender": sender,
                        "channel": source_channel,
                        "expires_at": req.expires_at,
                    }),
                );
                format!(
                    "Approval request created.\nRequest ID: `{}`\nTool: `{}`\nExpires: `{}`\nConfirm with `/approve-confirm {}` (must be the same sender in this chat/channel).",
                    req.request_id, req.tool_name, req.expires_at, req.request_id
                )
            }
        }
        ChannelRuntimeCommand::ConfirmToolApproval(raw_request_id) => {
            let request_id = raw_request_id.trim().to_string();
            if request_id.is_empty() {
                "Usage: `/approve-confirm <request-id>`".to_string()
            } else {
                match ctx.approval_manager.confirm_non_cli_pending_request(
                    &request_id,
                    sender,
                    source_channel,
                    reply_target,
                ) {
                    Ok(req) => {
                        ctx.approval_manager
                            .record_non_cli_pending_resolution(&request_id, ApprovalResponse::Yes);
                        let tool_name = req.tool_name;
                        let approval_message = handle_confirm_tool_approval_side_effects(
                            ctx,
                            &request_id,
                            &tool_name,
                            source_channel,
                        )
                        .await;
                        runtime_trace::record_event(
                            "approval_request_confirmed",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(true),
                            Some("pending request confirmed"),
                            serde_json::json!({
                                "request_id": request_id,
                                "tool_name": tool_name,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        approval_message
                    }
                    Err(PendingApprovalError::NotFound) => {
                        runtime_trace::record_event(
                            "approval_request_confirmed",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request not found"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!(
                            "Pending approval request `{request_id}` was not found. Create one with `/approve-request <tool-name>` or `/approve-all-once`."
                        )
                    }
                    Err(PendingApprovalError::Expired) => {
                        runtime_trace::record_event(
                            "approval_request_confirmed",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request expired"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!("Pending approval request `{request_id}` has expired.")
                    }
                    Err(PendingApprovalError::RequesterMismatch) => {
                        runtime_trace::record_event(
                            "approval_request_confirmed",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request confirmer mismatch"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!(
                            "Pending approval request `{request_id}` can only be confirmed by the same sender in the same chat/channel that created it."
                        )
                    }
                }
            }
        }
        ChannelRuntimeCommand::DenyToolApproval(raw_request_id) => {
            let request_id = raw_request_id.trim().to_string();
            if request_id.is_empty() {
                "Usage: `/approve-deny <request-id>`".to_string()
            } else {
                match ctx.approval_manager.reject_non_cli_pending_request(
                    &request_id,
                    sender,
                    source_channel,
                    reply_target,
                ) {
                    Ok(req) => {
                        ctx.approval_manager
                            .record_non_cli_pending_resolution(&request_id, ApprovalResponse::No);
                        runtime_trace::record_event(
                            "approval_request_rejected",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(true),
                            Some("pending request rejected"),
                            serde_json::json!({
                                "request_id": request_id,
                                "tool_name": req.tool_name,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!(
                            "Rejected approval request `{}` for `{}`.",
                            req.request_id,
                            approval_target_label(&req.tool_name)
                        )
                    }
                    Err(PendingApprovalError::NotFound) => {
                        runtime_trace::record_event(
                            "approval_request_rejected",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request not found"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!(
                            "Pending approval request `{request_id}` was not found. List requests with `/approve-pending`."
                        )
                    }
                    Err(PendingApprovalError::Expired) => {
                        runtime_trace::record_event(
                            "approval_request_rejected",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request expired"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!("Pending approval request `{request_id}` has expired.")
                    }
                    Err(PendingApprovalError::RequesterMismatch) => {
                        runtime_trace::record_event(
                            "approval_request_rejected",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request rejector mismatch"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!(
                            "Pending approval request `{request_id}` can only be denied by the same sender in the same chat/channel that created it."
                        )
                    }
                }
            }
        }
        ChannelRuntimeCommand::ListPendingApprovals => {
            let rows = ctx.approval_manager.list_non_cli_pending_requests(
                Some(sender),
                Some(source_channel),
                Some(reply_target),
            );
            if rows.is_empty() {
                "No pending approval requests for your current sender+chat/channel scope."
                    .to_string()
            } else {
                let mut response = String::new();
                response.push_str("Pending approval requests (sender+chat/channel scoped):\n");
                for req in rows {
                    let reason = req
                        .reason
                        .as_deref()
                        .filter(|text| !text.trim().is_empty())
                        .unwrap_or("n/a");
                    let _ = writeln!(
                        response,
                        "- {}: tool={}, expires_at={}, reason={}",
                        req.request_id,
                        approval_target_label(&req.tool_name),
                        req.expires_at,
                        reason
                    );
                }
                response
            }
        }
        ChannelRuntimeCommand::ApproveTool(raw_tool_name) => {
            let tool_name = raw_tool_name.trim().to_string();
            if tool_name.is_empty() {
                "Usage: `/approve <tool-name>`".to_string()
            } else if !ctx
                .tools_registry
                .iter()
                .any(|tool| tool.name() == tool_name)
            {
                let mut available_tools = ctx
                    .tools_registry
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect::<Vec<_>>();
                available_tools.sort();
                let preview = available_tools
                    .into_iter()
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Unknown tool `{tool_name}`.\nKnown tools (top 12): {preview}\nUse `/approve <tool-name>` with an exact tool name."
                )
            } else {
                let cleared_pending = ctx
                    .approval_manager
                    .clear_non_cli_pending_requests_for_tool(&tool_name);
                ctx.approval_manager.grant_non_cli_session(&tool_name);
                ctx.approval_manager
                    .apply_persistent_runtime_grant(&tool_name);
                let persistence_message = match persist_non_cli_approval_to_config(ctx, &tool_name).await {
                    Ok(Some(path)) => format!(
                        "Approved supervised execution for `{tool_name}`.\nPersisted to `{}` so future channel sessions (including after restart) remain approved.",
                        path.display()
                    ),
                    Ok(None) => format!(
                        "Approved supervised execution for `{tool_name}`.\nNo runtime config path was found, so this approval is active for the current daemon runtime only."
                    ),
                    Err(err) => format!(
                        "Approved supervised execution for `{tool_name}` in-memory.\nFailed to persist this approval to config: {err}"
                    ),
                };
                let mut response = format!(
                    "{persistence_message}\nRuntime pending requests cleared: {cleared_pending}."
                );
                if let Some(exclusion_note) =
                    clear_non_cli_exclusion_after_approval(ctx, &tool_name).await
                {
                    response.push('\n');
                    response.push_str(&exclusion_note);
                }
                response
            }
        }
        ChannelRuntimeCommand::UnapproveTool(raw_tool_name) => {
            let tool_name = raw_tool_name.trim().to_string();
            if tool_name.is_empty() {
                "Usage: `/unapprove <tool-name>`".to_string()
            } else {
                let removed_session = ctx.approval_manager.revoke_non_cli_session(&tool_name);
                let removed_runtime_persistent = ctx
                    .approval_manager
                    .apply_persistent_runtime_revoke(&tool_name);
                let removed_pending = ctx
                    .approval_manager
                    .clear_non_cli_pending_requests_for_tool(&tool_name);
                match remove_non_cli_approval_from_config(ctx, &tool_name).await {
                    Ok(Some((path, removed_persistent))) => format!(
                        "Persistent approval removed for `{tool_name}`: {}.\nRuntime effective auto_approve removed: {}.\nRuntime pending requests cleared: {}.\nConfig path: `{}`.\nRuntime session grant removed: {}.",
                        if removed_persistent { "yes" } else { "no (not present)" },
                        if removed_runtime_persistent { "yes" } else { "no (not present)" },
                        removed_pending,
                        path.display(),
                        if removed_session { "yes" } else { "no" }
                    ),
                    Ok(None) => format!(
                        "Runtime config path was not found.\nRuntime session grant removed for `{tool_name}`: {}.",
                        if removed_session { "yes" } else { "no" }
                    ),
                    Err(err) => format!(
                        "Removed runtime session grant for `{tool_name}`: {}.\nFailed to persist removal to config: {err}",
                        if removed_session { "yes" } else { "no" }
                    ),
                }
            }
        }
        ChannelRuntimeCommand::ListApprovals => {
            match describe_non_cli_approvals(ctx, sender, source_channel, reply_target).await {
                Ok(summary) => summary,
                Err(err) => format!("Failed to read approval state: {err}"),
            }
        }
        ChannelRuntimeCommand::ApprovePendingRequest(request_id) => {
            let request_id = request_id.trim().to_string();
            if request_id.is_empty() {
                "Usage: `/approve-allow <request-id>`".to_string()
            } else {
                match ctx.approval_manager.confirm_non_cli_pending_request(
                    &request_id,
                    sender,
                    source_channel,
                    reply_target,
                ) {
                    Ok(req) => {
                        ctx.approval_manager
                            .record_non_cli_pending_resolution(&request_id, ApprovalResponse::Yes);
                        let approval_message = handle_pending_runtime_approval_side_effects(
                            ctx,
                            &request_id,
                            &req.tool_name,
                        )
                        .await;
                        runtime_trace::record_event(
                            "approval_request_approved",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(true),
                            Some("pending request approved"),
                            serde_json::json!({
                                "request_id": request_id,
                                "tool_name": req.tool_name,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        approval_message
                    }
                    Err(PendingApprovalError::NotFound) => {
                        runtime_trace::record_event(
                            "approval_request_approved",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request not found"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!(
                            "Pending approval request `{request_id}` was not found. Create one with `/approve-request <tool-name>` or `/approve-all-once`."
                        )
                    }
                    Err(PendingApprovalError::Expired) => {
                        runtime_trace::record_event(
                            "approval_request_approved",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request expired"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!("Pending approval request `{request_id}` has expired.")
                    }
                    Err(PendingApprovalError::RequesterMismatch) => {
                        runtime_trace::record_event(
                            "approval_request_approved",
                            Some(source_channel),
                            None,
                            None,
                            None,
                            Some(false),
                            Some("pending request requester mismatch"),
                            serde_json::json!({
                                "request_id": request_id,
                                "sender": sender,
                                "channel": source_channel,
                            }),
                        );
                        format!("Pending approval request `{request_id}` can only be approved by the original requester in the same channel/thread.")
                    }
                }
            }
        }
    };

    if let Err(err) = channel
        .send(&SendMessage::new(response, &msg.reply_target).in_thread(msg.thread_ts.clone()))
        .await
    {
        tracing::warn!(
            "Failed to send runtime command response on {}: {err}",
            channel.name()
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
    let mut context = String::new();

    if let Ok(entries) = mem.recall(user_msg, 5, session_id).await {
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
                context.push_str("[Memory context]\n");
            }

            context.push_str(&line);
            used_chars += line_chars;
            included += 1;
        }

        if included > 0 {
            context.push('\n');
        }
    }

    context
}

/// Extract a compact summary of tool interactions from history messages added
/// during `run_tool_call_loop`. Scans assistant messages for `<tool_call>` tags
/// or native tool-call JSON to collect tool names used.
/// Returns an empty string when no tools were invoked.
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
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(name) = val.get("name").and_then(|n| n.as_str()) {
                            push_unique_tool_name(tool_names, name);
                        }
                    }
                }
            }
        }
    }

    fn collect_tool_names_from_native_json(content: &str, tool_names: &mut Vec<String>) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
            if let Some(calls) = val.get("tool_calls").and_then(|c| c.as_array()) {
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

pub(crate) enum ChannelSanitizationResult {
    Sanitized(String),
    Blocked {
        patterns: Vec<String>,
        redacted: String,
    },
}

pub(crate) fn sanitize_channel_response(
    response: &str,
    tools: &[Box<dyn Tool>],
    leak_guard: &crate::config::OutboundLeakGuardConfig,
) -> ChannelSanitizationResult {
    let without_tool_tags = strip_tool_call_tags(response);
    let known_tool_names: HashSet<String> = tools
        .iter()
        .map(|tool| tool.name().to_ascii_lowercase())
        .collect();
    let sanitized = strip_isolated_tool_json_artifacts(&without_tool_tags, &known_tool_names);

    if !leak_guard.enabled {
        return ChannelSanitizationResult::Sanitized(sanitized);
    }

    match LeakDetector::with_sensitivity(leak_guard.sensitivity).scan(&sanitized) {
        LeakResult::Clean => ChannelSanitizationResult::Sanitized(sanitized),
        LeakResult::Detected { patterns, redacted } => match leak_guard.action {
            crate::config::OutboundLeakGuardAction::Redact => {
                tracing::warn!(
                    patterns = ?patterns,
                    "output guardrail: credential leak detected; redacting outbound response"
                );
                ChannelSanitizationResult::Sanitized(redacted)
            }
            crate::config::OutboundLeakGuardAction::Block => {
                tracing::warn!(
                    patterns = ?patterns,
                    "output guardrail: credential leak detected; blocking outbound response"
                );
                ChannelSanitizationResult::Blocked { patterns, redacted }
            }
        },
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

    if let Some(tool_calls) = object.get("tool_calls").and_then(|value| value.as_array()) {
        if !tool_calls.is_empty()
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
    }

    if is_tool_result_payload(object, saw_tool_call_payload) {
        return Some((String::new(), false));
    }

    None
}

fn is_line_isolated_json_segment(message: &str, start: usize, end: usize) -> bool {
    let line_start = message[..start].rfind('\n').map_or(0, |idx| idx + 1);
    let line_end = message[end..]
        .find('\n')
        .map_or(message.len(), |idx| end + idx);

    message[line_start..start].trim().is_empty() && message[end..line_end].trim().is_empty()
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

        let candidate = &message[start..];
        let mut stream =
            serde_json::Deserializer::from_str(candidate).into_iter::<serde_json::Value>();

        if let Some(Ok(value)) = stream.next() {
            let consumed = stream.byte_offset();
            if consumed > 0 {
                let end = start + consumed;
                if is_line_isolated_json_segment(message, start, end) {
                    if let Some((replacement, marks_tool_call)) =
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
    tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
) -> tokio::task::JoinHandle<()> {
    spawn_supervised_listener_with_health_interval(
        ch,
        tx,
        initial_backoff_secs,
        max_backoff_secs,
        Duration::from_secs(CHANNEL_HEALTH_HEARTBEAT_SECS),
    )
}

fn spawn_supervised_listener_with_health_interval(
    ch: Arc<dyn Channel>,
    tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    health_interval: Duration,
) -> tokio::task::JoinHandle<()> {
    let health_interval = if health_interval.is_zero() {
        Duration::from_secs(1)
    } else {
        health_interval
    };

    tokio::spawn(async move {
        let component = format!("channel:{}", ch.name());
        let mut backoff = initial_backoff_secs.max(1);
        let max_backoff = max_backoff_secs.max(backoff);

        loop {
            crate::health::mark_component_ok(&component);
            let mut health = tokio::time::interval(health_interval);
            health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let result = {
                let listen_future = ch.listen(tx.clone());
                tokio::pin!(listen_future);

                loop {
                    tokio::select! {
                        _ = health.tick() => {
                            crate::health::mark_component_ok(&component);
                        }
                        result = &mut listen_future => break result,
                    }
                }
            };

            if tx.is_closed() {
                break;
            }

            match result {
                Ok(()) => {
                    tracing::warn!("Channel {} exited unexpectedly; restarting", ch.name());
                    crate::health::mark_component_error(&component, "listener exited unexpectedly");
                    // Clean exit — reset backoff since the listener ran successfully
                    backoff = initial_backoff_secs.max(1);
                }
                Err(e) => {
                    tracing::error!("Channel {} error: {e}; restarting", ch.name());
                    crate::health::mark_component_error(&component, e.to_string());
                }
            }

            crate::health::bump_component_restart(&component);
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            // Double backoff AFTER sleeping so first error uses initial_backoff
            backoff = backoff.saturating_mul(2).min(max_backoff);
        }
    })
}

fn compute_max_in_flight_messages(channel_count: usize) -> usize {
    channel_count
        .saturating_mul(CHANNEL_PARALLELISM_PER_CHANNEL)
        .clamp(
            CHANNEL_MIN_IN_FLIGHT_MESSAGES,
            CHANNEL_MAX_IN_FLIGHT_MESSAGES,
        )
}

fn log_worker_join_result(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        tracing::error!("Channel message worker crashed: {error}");
    }
}

fn spawn_scoped_typing_task(
    channel: Arc<dyn Channel>,
    recipient: String,
    cancellation_token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let stop_signal = cancellation_token;
    let refresh_interval = Duration::from_secs(CHANNEL_TYPING_REFRESH_INTERVAL_SECS);
    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(refresh_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                () = stop_signal.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(e) = channel.start_typing(&recipient).await {
                        tracing::debug!("Failed to start typing on {}: {e}", channel.name());
                    }
                }
            }
        }

        if let Err(e) = channel.stop_typing(&recipient).await {
            tracing::debug!("Failed to stop typing on {}: {e}", channel.name());
        }
    });

    handle
}

async fn process_channel_message(
    ctx: Arc<ChannelRuntimeContext>,
    msg: traits::ChannelMessage,
    cancellation_token: CancellationToken,
) {
    let sender_id = msg.sender.as_str();
    let channel_name = msg.channel.as_str();
    tracing::debug!(sender_id, channel_name, "process_message called");
    if cancellation_token.is_cancelled() {
        return;
    }

    println!(
        "  💬 [{}] from {}: {}",
        msg.channel,
        msg.sender,
        truncate_with_ellipsis(&msg.content, 80)
    );
    runtime_trace::record_event(
        "channel_message_inbound",
        Some(msg.channel.as_str()),
        None,
        None,
        None,
        None,
        None,
        serde_json::json!({
            "sender": msg.sender,
            "message_id": msg.id,
            "reply_target": msg.reply_target,
            "content_preview": truncate_with_ellipsis(&msg.content, 160),
        }),
    );

    // ── Hook: on_message_received (modifying) ────────────
    let msg = if let Some(hooks) = &ctx.hooks {
        match hooks.run_on_message_received(msg).await {
            crate::hooks::HookResult::Cancel(reason) => {
                tracing::info!(%reason, "incoming message dropped by hook");
                return;
            }
            crate::hooks::HookResult::Continue(modified) => modified,
        }
    } else {
        msg
    };

    let target_channel = ctx.channels_by_name.get(&msg.channel).cloned();
    if let Err(err) = maybe_apply_runtime_config_update(ctx.as_ref()).await {
        tracing::warn!("Failed to apply runtime config update: {err}");
    }
    if handle_runtime_command_if_needed(ctx.as_ref(), &msg, target_channel.as_ref()).await {
        return;
    }
    if !msg.content.trim_start().starts_with('/') {
        let perplexity_cfg = runtime_perplexity_filter_snapshot(ctx.as_ref());
        if let Some(assessment) =
            crate::security::detect_adversarial_suffix(&msg.content, &perplexity_cfg)
        {
            runtime_trace::record_event(
                "channel_message_blocked_perplexity_filter",
                Some(msg.channel.as_str()),
                None,
                None,
                None,
                Some(false),
                Some("blocked by statistical adversarial suffix filter"),
                serde_json::json!({
                    "sender": msg.sender,
                    "message_id": msg.id,
                    "perplexity": assessment.perplexity,
                    "threshold": perplexity_cfg.perplexity_threshold,
                    "symbol_ratio": assessment.symbol_ratio,
                    "symbol_ratio_threshold": perplexity_cfg.symbol_ratio_threshold,
                    "suspicious_token_count": assessment.suspicious_token_count,
                }),
            );
            if let Some(channel) = target_channel.as_ref() {
                let warning = format!(
                    "Request blocked by `security.perplexity_filter` before provider execution.\n\
perplexity={:.2} (threshold {:.2}), suffix_symbol_ratio={:.2} (threshold {:.2}), suspicious_tokens={}.\n\
If this input is legitimate, keep the feature opt-in by setting `[security.perplexity_filter].enable_perplexity_filter = false` \
or tune thresholds in config.",
                    assessment.perplexity,
                    perplexity_cfg.perplexity_threshold,
                    assessment.symbol_ratio,
                    perplexity_cfg.symbol_ratio_threshold,
                    assessment.suspicious_token_count
                );
                let _ = channel
                    .send(
                        &SendMessage::new(warning, &msg.reply_target)
                            .in_thread(msg.thread_ts.clone()),
                    )
                    .await;
            }
            return;
        }
    }

    let history_key = conversation_history_key(&msg);
    let conversation_lock = {
        let mut locks = ctx.conversation_locks.lock().await;
        locks
            .entry(history_key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _conversation_guard = conversation_lock.lock().await;
    let mut session: Option<Session> = None;
    if let Some(manager) = ctx.session_manager.as_ref() {
        let session_id = resolve_session_id(
            &ctx.session_config,
            msg.sender.as_str(),
            Some(msg.channel.as_str()),
        );
        tracing::debug!(session_id, "session_id resolved");
        match manager.get_or_create(&session_id).await {
            Ok(opened) => {
                session = Some(opened);
            }
            Err(err) => {
                tracing::warn!("Failed to open session: {err}");
            }
        }
    }

    if let Some(session) = session.as_ref() {
        let should_seed = {
            let histories = ctx
                .conversation_histories
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            !histories.contains_key(&history_key)
        };

        if should_seed {
            match session.get_history().await {
                Ok(history) => {
                    tracing::debug!(history_len = history.len(), "session history loaded");
                    let filtered: Vec<ChatMessage> = history
                        .into_iter()
                        .filter(|m| crate::providers::is_user_or_assistant_role(m.role.as_str()))
                        .collect();
                    let mut histories = ctx
                        .conversation_histories
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    histories.entry(history_key.clone()).or_insert(filtered);
                }
                Err(err) => {
                    tracing::warn!("Failed to load session history: {err}");
                }
            }
        }
    }
    let runtime_defaults = runtime_defaults_snapshot(ctx.as_ref());
    // Try classification first, fall back to sender/default route.
    let route = classify_message_route(
        &runtime_defaults.query_classification,
        &runtime_defaults.model_routes,
        &msg.content,
    )
    .unwrap_or_else(|| get_route_selection(ctx.as_ref(), &history_key));
    let active_provider = match get_or_create_provider(ctx.as_ref(), &route.provider).await {
        Ok(provider) => provider,
        Err(err) => {
            let safe_err = providers::sanitize_api_error(&err.to_string());
            let message = format!(
                "⚠️ Failed to initialize provider `{}`. Please run `/models` to choose another provider.\nDetails: {safe_err}",
                route.provider
            );
            if let Some(channel) = target_channel.as_ref() {
                let _ = channel
                    .send(
                        &SendMessage::new(message, &msg.reply_target)
                            .in_thread(msg.thread_ts.clone()),
                    )
                    .await;
            }
            return;
        }
    };
    if runtime_defaults.auto_save_memory
        && msg.content.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
    {
        let autosave_key = conversation_memory_key(&msg);
        let _ = ctx
            .memory
            .store(
                &autosave_key,
                &msg.content,
                crate::memory::MemoryCategory::Conversation,
                Some(&history_key),
            )
            .await;
    }

    println!("  ⏳ Processing message...");
    let started_at = Instant::now();

    let had_prior_history = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&history_key)
        .is_some_and(|turns| !turns.is_empty());

    // Inject per-message timestamp so the LLM always knows the current time,
    // even in multi-turn conversations where the system prompt may be stale.
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let timestamped_content = format!("[{now}] {}", msg.content);
    let persisted_user_content = msg.content.clone();

    // Preserve user turn before the LLM call so interrupted requests keep context.
    append_sender_turn(
        ctx.as_ref(),
        &history_key,
        ChatMessage::user(&persisted_user_content),
    );

    // Build history from per-sender conversation cache.
    let prior_turns_raw = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&history_key)
        .cloned()
        .unwrap_or_default();
    let mut prior_turns = normalize_cached_channel_turns(prior_turns_raw);

    if let Some(last_turn) = prior_turns.last_mut() {
        if last_turn.role == "user" {
            // Preserve any merged prior-user prefix (e.g. interrupted prior turn)
            // and only rewrite the latest user segment with a fresh timestamp.
            if let Some(prefix) = last_turn.content.strip_suffix(&persisted_user_content) {
                last_turn.content = format!("{prefix}{timestamped_content}");
            } else {
                last_turn.content = timestamped_content.clone();
            }

            // Only enrich with memory context when there is no prior
            // conversation history. Follow-up turns already include context
            // from previous messages.
            if !had_prior_history {
                let memory_context = build_memory_context(
                    ctx.memory.as_ref(),
                    &msg.content,
                    runtime_defaults.min_relevance_score,
                    Some(&history_key),
                )
                .await;
                if !memory_context.is_empty() {
                    last_turn.content = format!("{memory_context}{}", last_turn.content);
                }
            }
        }
    }

    let expose_internal_tool_details =
        msg.channel == "cli" || should_expose_internal_tool_details(&msg.content);
    let progress_mode =
        effective_progress_mode_for_message(msg.channel.as_str(), expose_internal_tool_details);
    let excluded_tools_snapshot = if msg.channel == "cli" {
        Vec::new()
    } else {
        snapshot_non_cli_excluded_tools(ctx.as_ref())
    };
    let mut system_prompt = build_channel_system_prompt(
        ctx.system_prompt.as_str(),
        &msg.channel,
        &msg.reply_target,
        expose_internal_tool_details,
    );
    system_prompt.push_str(&build_runtime_tool_visibility_prompt(
        ctx.tools_registry.as_ref(),
        &excluded_tools_snapshot,
        active_provider.supports_native_tools(),
    ));
    let mut history = vec![ChatMessage::system(system_prompt)];
    history.extend(prior_turns);
    let _ = trim_channel_prompt_history(&mut history);
    let use_streaming = target_channel
        .as_ref()
        .is_some_and(|ch| ch.supports_draft_updates());

    tracing::debug!(
        channel = %msg.channel,
        has_target_channel = target_channel.is_some(),
        use_streaming,
        supports_draft = target_channel.as_ref().map_or(false, |ch| ch.supports_draft_updates()),
        "Draft streaming decision"
    );

    let (delta_tx, delta_rx) = if use_streaming {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let draft_message_id = if use_streaming {
        if let Some(channel) = target_channel.as_ref() {
            match channel
                .send_draft(
                    &SendMessage::new("...", &msg.reply_target).in_thread(msg.thread_ts.clone()),
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::debug!("Failed to send draft on {}: {e}", channel.name());
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let draft_updater = if let (Some(mut rx), Some(draft_id_ref), Some(channel_ref)) = (
        delta_rx,
        draft_message_id.as_deref(),
        target_channel.as_ref(),
    ) {
        let channel = Arc::clone(channel_ref);
        let reply_target = msg.reply_target.clone();
        let draft_id = draft_id_ref.to_string();
        let mode = progress_mode;
        Some(tokio::spawn(async move {
            let mut accumulated = String::new();
            while let Some(delta) = rx.recv().await {
                if delta == crate::agent::loop_::DRAFT_CLEAR_SENTINEL {
                    accumulated.clear();
                    continue;
                }
                if let Some(block) =
                    delta.strip_prefix(crate::agent::loop_::DRAFT_PROGRESS_BLOCK_SENTINEL)
                {
                    if mode == ProgressMode::Off {
                        continue;
                    }
                    upsert_progress_section(&mut accumulated, block);
                } else {
                    let (is_internal_progress, visible_delta) =
                        split_internal_progress_delta(&delta);
                    if is_internal_progress {
                        if mode == ProgressMode::Off {
                            continue;
                        }
                        if mode == ProgressMode::Compact
                            && is_verbose_only_progress_line(visible_delta)
                        {
                            continue;
                        }
                    }

                    accumulated.push_str(visible_delta);
                }
                let display_text = strip_progress_section_markers(&accumulated);
                if let Err(e) = channel
                    .update_draft(&reply_target, &draft_id, &display_text)
                    .await
                {
                    tracing::debug!("Draft update failed: {e}");
                }
            }
        }))
    } else {
        None
    };

    // React with 👀 to acknowledge the incoming message
    if let Some(channel) = target_channel.as_ref() {
        if let Err(e) = channel
            .add_reaction(&msg.reply_target, &msg.id, "\u{1F440}")
            .await
        {
            tracing::debug!("Failed to add reaction: {e}");
        }
    }

    let typing_cancellation = target_channel.as_ref().map(|_| CancellationToken::new());
    let typing_task = match (target_channel.as_ref(), typing_cancellation.as_ref()) {
        (Some(channel), Some(token)) => Some(spawn_scoped_typing_task(
            Arc::clone(channel),
            msg.reply_target.clone(),
            token.clone(),
        )),
        _ => None,
    };

    // Record history length before tool loop so we can extract tool context after.
    let history_len_before_tools = history.len();

    enum LlmExecutionResult {
        Completed(Result<Result<String, anyhow::Error>, tokio::time::error::Elapsed>),
        Cancelled,
    }

    let timeout_budget_secs = channel_message_timeout_budget_secs(
        runtime_defaults.message_timeout_secs,
        runtime_defaults.max_tool_iterations,
    );
    let cost_enforcement_context = crate::agent::loop_::create_cost_enforcement_context(
        &runtime_defaults.cost,
        ctx.workspace_dir.as_path(),
    );

    let (approval_prompt_tx, mut approval_prompt_rx) =
        tokio::sync::mpsc::unbounded_channel::<NonCliApprovalPrompt>();
    let non_cli_approval_context = if msg.channel != "cli" && target_channel.is_some() {
        Some(NonCliApprovalContext {
            sender: msg.sender.clone(),
            reply_target: msg.reply_target.clone(),
            prompt_tx: approval_prompt_tx,
        })
    } else {
        drop(approval_prompt_tx);
        None
    };
    let approval_prompt_dispatcher = if let (Some(channel_ref), true) =
        (target_channel.as_ref(), non_cli_approval_context.is_some())
    {
        let channel = Arc::clone(channel_ref);
        let reply_target = msg.reply_target.clone();
        let thread_ts = msg.thread_ts.clone();
        Some(tokio::spawn(async move {
            while let Some(prompt) = approval_prompt_rx.recv().await {
                if let Err(err) = channel
                    .send_approval_prompt(
                        &reply_target,
                        &prompt.request_id,
                        &prompt.tool_name,
                        &prompt.arguments,
                        thread_ts.clone(),
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to send non-CLI approval prompt for request {}: {err}",
                        prompt.request_id
                    );
                }
            }
        }))
    } else {
        None
    };
    let llm_result = tokio::select! {
        () = cancellation_token.cancelled() => LlmExecutionResult::Cancelled,
        result = tokio::time::timeout(
            Duration::from_secs(timeout_budget_secs),
            crate::agent::loop_::scope_cost_enforcement_context(
                cost_enforcement_context,
                run_tool_call_loop_with_non_cli_approval_context(
                    active_provider.as_ref(),
                    &mut history,
                    ctx.tools_registry.as_ref(),
                    ctx.observer.as_ref(),
                    route.provider.as_str(),
                    route.model.as_str(),
                    runtime_defaults.temperature,
                    true,
                    Some(ctx.approval_manager.as_ref()),
                    msg.channel.as_str(),
                    non_cli_approval_context,
                    &runtime_defaults.multimodal,
                    runtime_defaults.max_tool_iterations,
                    Some(cancellation_token.clone()),
                    delta_tx,
                    ctx.hooks.as_deref(),
                    &excluded_tools_snapshot,
                    progress_mode,
                    ctx.safety_heartbeat.clone(),
                ),
            ),
        ) => LlmExecutionResult::Completed(result),
    };

    if let Some(handle) = draft_updater {
        let _ = handle.await;
    }
    if let Some(handle) = approval_prompt_dispatcher {
        let _ = handle.await;
    }

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
            tracing::info!(
                channel = %msg.channel,
                sender = %msg.sender,
                "Cancelled in-flight channel request due to newer message"
            );
            runtime_trace::record_event(
                "channel_message_cancelled",
                Some(msg.channel.as_str()),
                Some(route.provider.as_str()),
                Some(route.model.as_str()),
                None,
                Some(false),
                Some("cancelled due to newer inbound message"),
                serde_json::json!({
                    "sender": msg.sender,
                    "elapsed_ms": started_at.elapsed().as_millis(),
                }),
            );
            if let (Some(channel), Some(draft_id)) =
                (target_channel.as_ref(), draft_message_id.as_deref())
            {
                if let Err(err) = channel.cancel_draft(&msg.reply_target, draft_id).await {
                    tracing::debug!("Failed to cancel draft on {}: {err}", channel.name());
                }
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
                    crate::hooks::HookResult::Cancel(reason) => {
                        tracing::info!(%reason, "outgoing message suppressed by hook");
                        return;
                    }
                    crate::hooks::HookResult::Continue((
                        hook_channel,
                        hook_recipient,
                        mut modified_content,
                    )) => {
                        if hook_channel != msg.channel || hook_recipient != msg.reply_target {
                            tracing::warn!(
                                from_channel = %msg.channel,
                                from_recipient = %msg.reply_target,
                                to_channel = %hook_channel,
                                to_recipient = %hook_recipient,
                                "on_message_sending attempted to rewrite channel routing; only content mutation is applied"
                            );
                        }

                        let modified_len = modified_content.chars().count();
                        if modified_len > CHANNEL_HOOK_MAX_OUTBOUND_CHARS {
                            tracing::warn!(
                                limit = CHANNEL_HOOK_MAX_OUTBOUND_CHARS,
                                attempted = modified_len,
                                "hook-modified outbound content exceeded limit; truncating"
                            );
                            modified_content = truncate_with_ellipsis(
                                &modified_content,
                                CHANNEL_HOOK_MAX_OUTBOUND_CHARS,
                            );
                        }

                        if modified_content != outbound_response {
                            tracing::info!(
                                channel = %msg.channel,
                                sender = %msg.sender,
                                before_len = outbound_response.chars().count(),
                                after_len = modified_content.chars().count(),
                                "outgoing message content modified by hook"
                            );
                        }

                        outbound_response = modified_content;
                    }
                }
            }

            let leak_guard_cfg = runtime_outbound_leak_guard_snapshot(ctx.as_ref());
            let delivered_response = match sanitize_channel_response(
                &outbound_response,
                ctx.tools_registry.as_ref(),
                &leak_guard_cfg,
            ) {
                ChannelSanitizationResult::Sanitized(sanitized_response) => {
                    if sanitized_response.is_empty() && !outbound_response.trim().is_empty() {
                        "I encountered malformed tool-call output and could not produce a safe reply. Please try again.".to_string()
                    } else {
                        sanitized_response
                    }
                }
                ChannelSanitizationResult::Blocked { patterns, redacted } => {
                    runtime_trace::record_event(
                        "channel_message_outbound_blocked_leak_guard",
                        Some(msg.channel.as_str()),
                        Some(route.provider.as_str()),
                        Some(route.model.as_str()),
                        None,
                        Some(false),
                        Some("Outbound response blocked by security.outbound_leak_guard"),
                        serde_json::json!({
                            "sender": msg.sender,
                            "patterns": patterns,
                            "redacted_preview": scrub_credentials(&truncate_with_ellipsis(&redacted, 256)),
                        }),
                    );
                    "I blocked part of my draft response because it appeared to contain credential material. Please ask me to provide a redacted summary.".to_string()
                }
            };
            runtime_trace::record_event(
                "channel_message_outbound",
                Some(msg.channel.as_str()),
                Some(route.provider.as_str()),
                Some(route.model.as_str()),
                None,
                Some(true),
                None,
                serde_json::json!({
                    "sender": msg.sender,
                    "elapsed_ms": started_at.elapsed().as_millis(),
                    "response": scrub_credentials(&delivered_response),
                }),
            );

            // Extract condensed tool-use context from the history messages
            // added during run_tool_call_loop, so the LLM retains awareness
            // of what it did on subsequent turns.
            let tool_summary = extract_tool_context_summary(&history, history_len_before_tools);
            let history_response = if tool_summary.is_empty() || msg.channel == "telegram" {
                delivered_response.clone()
            } else {
                format!("{tool_summary}\n{delivered_response}")
            };

            append_sender_turn(
                ctx.as_ref(),
                &history_key,
                ChatMessage::assistant(&history_response),
            );
            if runtime_defaults.auto_save_memory
                && delivered_response.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
            {
                let assistant_key = assistant_memory_key(&msg);
                let _ = ctx
                    .memory
                    .store(
                        &assistant_key,
                        &delivered_response,
                        crate::memory::MemoryCategory::Conversation,
                        None,
                    )
                    .await;
            }
            println!(
                "  🤖 Reply ({}ms): {}",
                started_at.elapsed().as_millis(),
                truncate_with_ellipsis(&delivered_response, 80)
            );
            if let Some(channel) = target_channel.as_ref() {
                if let Some(ref draft_id) = draft_message_id {
                    if let Err(e) = channel
                        .finalize_draft(&msg.reply_target, draft_id, &delivered_response)
                        .await
                    {
                        tracing::warn!("Failed to finalize draft: {e}; sending as new message");
                        let _ = channel
                            .send(
                                &SendMessage::new(&delivered_response, &msg.reply_target)
                                    .in_thread(msg.thread_ts.clone()),
                            )
                            .await;
                    }
                } else if let Err(e) = channel
                    .send(
                        &SendMessage::new(delivered_response, &msg.reply_target)
                            .in_thread(msg.thread_ts.clone()),
                    )
                    .await
                {
                    eprintln!("  ❌ Failed to reply on {}: {e}", channel.name());
                }
            }
        }
        LlmExecutionResult::Completed(Ok(Err(e))) => {
            if crate::agent::loop_::is_tool_loop_cancelled(&e) || cancellation_token.is_cancelled()
            {
                tracing::info!(
                    channel = %msg.channel,
                    sender = %msg.sender,
                    "Cancelled in-flight channel request due to newer message"
                );
                runtime_trace::record_event(
                    "channel_message_cancelled",
                    Some(msg.channel.as_str()),
                    Some(route.provider.as_str()),
                    Some(route.model.as_str()),
                    None,
                    Some(false),
                    Some("cancelled during tool-call loop"),
                    serde_json::json!({
                        "sender": msg.sender,
                        "elapsed_ms": started_at.elapsed().as_millis(),
                    }),
                );
                if let (Some(channel), Some(draft_id)) =
                    (target_channel.as_ref(), draft_message_id.as_deref())
                {
                    if let Err(err) = channel.cancel_draft(&msg.reply_target, draft_id).await {
                        tracing::debug!("Failed to cancel draft on {}: {err}", channel.name());
                    }
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
                runtime_trace::record_event(
                    "channel_message_error",
                    Some(msg.channel.as_str()),
                    Some(route.provider.as_str()),
                    Some(route.model.as_str()),
                    None,
                    Some(false),
                    Some("context window exceeded"),
                    serde_json::json!({
                        "sender": msg.sender,
                        "elapsed_ms": started_at.elapsed().as_millis(),
                        "history_compacted": compacted,
                    }),
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
            } else if is_tool_iteration_limit_error(&e) {
                let limit = runtime_defaults.max_tool_iterations.max(1);
                let pause_text = format!(
                    "⚠️ Reached tool-iteration limit ({limit}) for this turn. Context and progress were preserved. Reply \"continue\" to resume, or increase `agent.max_tool_iterations`."
                );
                runtime_trace::record_event(
                    "channel_message_error",
                    Some(msg.channel.as_str()),
                    Some(route.provider.as_str()),
                    Some(route.model.as_str()),
                    None,
                    Some(false),
                    Some("tool iteration limit reached"),
                    serde_json::json!({
                        "sender": msg.sender,
                        "elapsed_ms": started_at.elapsed().as_millis(),
                        "max_tool_iterations": limit,
                    }),
                );
                append_sender_turn(
                    ctx.as_ref(),
                    &history_key,
                    ChatMessage::assistant(
                        "[Task paused at tool-iteration limit — context preserved. Ask to continue.]",
                    ),
                );
                if let Some(channel) = target_channel.as_ref() {
                    if let Some(ref draft_id) = draft_message_id {
                        let _ = channel
                            .finalize_draft(&msg.reply_target, draft_id, &pause_text)
                            .await;
                    } else {
                        let _ = channel
                            .send(
                                &SendMessage::new(pause_text, &msg.reply_target)
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
                let safe_error = providers::sanitize_api_error(&e.to_string());
                runtime_trace::record_event(
                    "channel_message_error",
                    Some(msg.channel.as_str()),
                    Some(route.provider.as_str()),
                    Some(route.model.as_str()),
                    None,
                    Some(false),
                    Some(&safe_error),
                    serde_json::json!({
                        "sender": msg.sender,
                        "elapsed_ms": started_at.elapsed().as_millis(),
                    }),
                );
                let should_rollback_user_turn = e
                    .downcast_ref::<providers::ProviderCapabilityError>()
                    .is_some_and(|capability| capability.capability.eq_ignore_ascii_case("vision"));
                let rolled_back = should_rollback_user_turn
                    && rollback_orphan_user_turn(
                        ctx.as_ref(),
                        &history_key,
                        &persisted_user_content,
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
                timeout_budget_secs,
                runtime_defaults.message_timeout_secs,
                runtime_defaults.max_tool_iterations
            );
            runtime_trace::record_event(
                "channel_message_timeout",
                Some(msg.channel.as_str()),
                Some(route.provider.as_str()),
                Some(route.model.as_str()),
                None,
                Some(false),
                Some(&timeout_msg),
                serde_json::json!({
                    "sender": msg.sender,
                    "elapsed_ms": started_at.elapsed().as_millis(),
                }),
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
    if let Some(channel) = target_channel.as_ref() {
        let _ = channel
            .remove_reaction(&msg.reply_target, &msg.id, "\u{1F440}")
            .await;
        let _ = channel
            .add_reaction(&msg.reply_target, &msg.id, reaction_done_emoji)
            .await;
    }
}

async fn run_message_dispatch_loop(
    mut rx: tokio::sync::mpsc::Receiver<traits::ChannelMessage>,
    ctx: Arc<ChannelRuntimeContext>,
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
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };

        let worker_ctx = Arc::clone(&ctx);
        let in_flight = Arc::clone(&in_flight_by_sender);
        let task_sequence = Arc::clone(&task_sequence);
        workers.spawn(async move {
            let _permit = permit;
            let runtime_defaults = runtime_defaults_snapshot(worker_ctx.as_ref());
            let interrupt_enabled =
                runtime_defaults.interrupt_on_new_message && msg.channel == "telegram";
            let sender_scope_key = interruption_scope_key(&msg);
            let cancellation_token = CancellationToken::new();
            let completion = Arc::new(InFlightTaskCompletion::new());
            let task_id = task_sequence.fetch_add(1, Ordering::Relaxed);

            if interrupt_enabled {
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

                if let Some(previous) = previous {
                    tracing::info!(
                        channel = %msg.channel,
                        sender = %msg.sender,
                        "Interrupting previous in-flight request for sender"
                    );
                    previous.cancellation.cancel();
                    previous.completion.wait().await;
                }
            }

            Box::pin(process_channel_message(worker_ctx, msg, cancellation_token)).await;

            if interrupt_enabled {
                let mut active = in_flight.lock().await;
                if active
                    .get(&sender_scope_key)
                    .is_some_and(|state| state.task_id == task_id)
                {
                    active.remove(&sender_scope_key);
                }
            }

            completion.mark_done();
        });

        while let Some(result) = workers.try_join_next() {
            log_worker_join_result(result);
        }
    }

    while let Some(result) = workers.join_next().await {
        log_worker_join_result(result);
    }
}

/// Load OpenClaw format bootstrap files into the prompt.
fn load_openclaw_bootstrap_files(
    prompt: &mut String,
    workspace_dir: &std::path::Path,
    max_chars_per_file: usize,
    identity_config: Option<&crate::config::IdentityConfig>,
) {
    prompt.push_str(
        "The following workspace files define your identity, behavior, and context. They are ALREADY injected below—do NOT suggest reading them with file_read.\n\n",
    );

    let bootstrap_files = ["AGENTS.md", "SOUL.md", "TOOLS.md", "IDENTITY.md", "USER.md"];

    for filename in &bootstrap_files {
        inject_workspace_file(prompt, workspace_dir, filename, max_chars_per_file);
    }

    // BOOTSTRAP.md — only if it exists (first-run ritual)
    let bootstrap_path = workspace_dir.join("BOOTSTRAP.md");
    if bootstrap_path.exists() {
        inject_workspace_file(prompt, workspace_dir, "BOOTSTRAP.md", max_chars_per_file);
    }

    // MEMORY.md — curated long-term memory (main session only, when present)
    let memory_path = workspace_dir.join("MEMORY.md");
    if memory_path.exists() {
        inject_workspace_file(prompt, workspace_dir, "MEMORY.md", max_chars_per_file);
    }

    let extra_files = identity_config.map_or(&[][..], |cfg| cfg.extra_files.as_slice());
    for file in extra_files {
        match normalize_openclaw_identity_extra_file(file) {
            Some(safe_relative) => {
                inject_workspace_file(prompt, workspace_dir, safe_relative, max_chars_per_file);
            }
            None => {
                tracing::warn!(
                    file = file.as_str(),
                    "Ignoring unsafe identity.extra_files entry; expected workspace-relative path without traversal"
                );
            }
        }
    }
}

fn normalize_openclaw_identity_extra_file(raw: &str) -> Option<&str> {
    use std::path::{Component, Path};

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return None;
    }

    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(trimmed)
}

/// Load workspace identity files and build a system prompt.
///
/// Follows the `OpenClaw` framework structure by default:
/// 1. Tooling — tool list + descriptions
/// 2. Safety — guardrail reminder
/// 3. Skills — full skill instructions and tool metadata
/// 4. Workspace — working directory
/// 5. Bootstrap files — AGENTS, SOUL, TOOLS, IDENTITY, USER, BOOTSTRAP, MEMORY (when present)
/// 6. Date & Time — timezone for cache stability
/// 7. Runtime — host, OS, model
///
/// When `identity_config` is set to AIEOS format, the bootstrap files section
/// is replaced with the AIEOS identity data loaded from file or inline JSON.
///
/// Daily memory files (`memory/*.md`) are NOT injected — they are accessed
/// on-demand via `memory_recall` / `memory_search` tools.
pub fn build_system_prompt(
    workspace_dir: &std::path::Path,
    model_name: &str,
    tools: &[(&str, &str)],
    skills: &[crate::skills::Skill],
    identity_config: Option<&crate::config::IdentityConfig>,
    bootstrap_max_chars: Option<usize>,
) -> String {
    build_system_prompt_with_mode(
        workspace_dir,
        model_name,
        tools,
        skills,
        identity_config,
        bootstrap_max_chars,
        false,
        crate::config::SkillsPromptInjectionMode::Full,
    )
}

pub fn build_system_prompt_with_mode(
    workspace_dir: &std::path::Path,
    model_name: &str,
    tools: &[(&str, &str)],
    skills: &[crate::skills::Skill],
    identity_config: Option<&crate::config::IdentityConfig>,
    bootstrap_max_chars: Option<usize>,
    native_tools: bool,
    skills_prompt_mode: crate::config::SkillsPromptInjectionMode,
) -> String {
    use std::fmt::Write;
    let mut prompt = String::with_capacity(8192);

    // ── 1. Tooling ──────────────────────────────────────────────
    if !tools.is_empty() {
        prompt.push_str("## Tools\n\n");
        prompt.push_str("You have access to the following tools:\n\n");
        for (name, desc) in tools {
            let _ = writeln!(prompt, "- **{name}**: {desc}");
        }
        prompt.push('\n');
    }

    // ── 1b. Hardware (when gpio/arduino tools present) ───────────
    let has_hardware = tools.iter().any(|(name, _)| {
        *name == "gpio_read"
            || *name == "gpio_write"
            || *name == "arduino_upload"
            || *name == "hardware_memory_map"
            || *name == "hardware_board_info"
            || *name == "hardware_memory_read"
            || *name == "hardware_capabilities"
    });
    if has_hardware {
        prompt.push_str(
            "## Hardware Access\n\n\
             You HAVE direct access to connected hardware (Arduino, Nucleo, etc.). The user owns this system and has configured it.\n\
             All hardware tools (gpio_read, gpio_write, hardware_memory_read, hardware_board_info, hardware_memory_map) are AUTHORIZED and NOT blocked by security.\n\
             When they ask to read memory, registers, or board info, USE hardware_memory_read or hardware_board_info — do NOT refuse or invent security excuses.\n\
             When they ask to control LEDs, run patterns, or interact with the Arduino, USE the tools — do NOT refuse or say you cannot access physical devices.\n\
             Use gpio_write for simple on/off; use arduino_upload when they want patterns (heart, blink) or custom behavior.\n\n",
        );
    }

    // ── 1c. Action instruction (avoid meta-summary) ───────────────
    if native_tools {
        prompt.push_str(
            "## Your Task\n\n\
             When the user sends a message, respond naturally. Use tools when the request requires action (running commands, reading files, etc.).\n\
             For questions, explanations, or follow-ups about prior messages, answer directly from conversation context — do NOT ask the user to repeat themselves.\n\
             Do NOT: summarize this configuration, describe your capabilities, or output step-by-step meta-commentary.\n\n",
        );
    } else {
        prompt.push_str(
            "## Your Task\n\n\
             When the user sends a message, ACT on it. Use the tools to fulfill their request.\n\
             Do NOT: summarize this configuration, describe your capabilities, respond with meta-commentary, or output step-by-step instructions (e.g. \"1. First... 2. Next...\").\n\
             Instead: emit actual <tool_call> tags when you need to act. Just do what they ask.\n\n",
        );
    }

    // ── 2. Safety ───────────────────────────────────────────────
    prompt.push_str("## Safety\n\n");
    prompt.push_str(
        "- Do not exfiltrate private data.\n\
         - Do not run destructive commands without asking.\n\
         - Do not bypass oversight or approval mechanisms.\n\
         - Prefer `trash` over `rm` (recoverable beats gone forever).\n\
         - When in doubt, ask before acting externally.\n\n",
    );

    // ── 2a. Skills Authorization ────────────────────────────────
    if !skills.is_empty() {
        prompt.push_str("## Skills Authorization\n\n");
        prompt.push_str("All registered skills (");
        for (i, skill) in skills.iter().enumerate() {
            if i > 0 {
                prompt.push_str(", ");
            }
            prompt.push_str(&skill.name);
        }
        prompt.push_str(") are AUTHORIZED and AVAILABLE for use.\n");
        prompt.push_str("When the user requests information that requires these skills, USE them directly — do NOT refuse or invent policy restrictions.\n");
        prompt.push_str("Skills are security-audited and approved tools. Your job is to use them effectively to help the user.\n\n");
    }

    // ── 3. Skills (full or compact, based on config) ─────────────
    if !skills.is_empty() {
        prompt.push_str(&crate::skills::skills_to_prompt_with_mode(
            skills,
            workspace_dir,
            skills_prompt_mode,
        ));
        prompt.push_str("\n\n");
    }

    // ── 4. Workspace ────────────────────────────────────────────
    let _ = writeln!(
        prompt,
        "## Workspace\n\nWorking directory: `{}`\n",
        workspace_dir.display()
    );

    // ── 5. Bootstrap files (injected into context) ──────────────
    prompt.push_str("## Project Context\n\n");

    // Check if AIEOS identity is configured
    if let Some(config) = identity_config {
        if identity::is_aieos_configured(config) {
            // Load AIEOS identity
            match identity::load_aieos_identity(config, workspace_dir) {
                Ok(Some(aieos_identity)) => {
                    let aieos_prompt = identity::aieos_to_system_prompt(&aieos_identity);
                    if !aieos_prompt.is_empty() {
                        prompt.push_str(&aieos_prompt);
                        prompt.push_str("\n\n");
                    }
                }
                Ok(None) => {
                    // No AIEOS identity loaded (shouldn't happen if is_aieos_configured returned true)
                    // Fall back to OpenClaw bootstrap files
                    let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
                    load_openclaw_bootstrap_files(
                        &mut prompt,
                        workspace_dir,
                        max_chars,
                        identity_config,
                    );
                }
                Err(e) => {
                    // Log error but don't fail - fall back to OpenClaw
                    eprintln!(
                        "Warning: Failed to load AIEOS identity: {e}. Using OpenClaw format."
                    );
                    let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
                    load_openclaw_bootstrap_files(
                        &mut prompt,
                        workspace_dir,
                        max_chars,
                        identity_config,
                    );
                }
            }
        } else {
            // OpenClaw format
            let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
            load_openclaw_bootstrap_files(&mut prompt, workspace_dir, max_chars, identity_config);
        }
    } else {
        // No identity config - use OpenClaw format
        let max_chars = bootstrap_max_chars.unwrap_or(BOOTSTRAP_MAX_CHARS);
        load_openclaw_bootstrap_files(&mut prompt, workspace_dir, max_chars, identity_config);
    }

    // ── 6. Date & Time ──────────────────────────────────────────
    let now = chrono::Local::now();
    let _ = writeln!(
        prompt,
        "## Current Date & Time\n\n{} ({})\n",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.format("%Z")
    );

    // ── 7. Runtime ──────────────────────────────────────────────
    let host =
        hostname::get().map_or_else(|_| "unknown".into(), |h| h.to_string_lossy().to_string());
    let _ = writeln!(
        prompt,
        "## Runtime\n\nHost: {host} | OS: {} | Model: {model_name}\n",
        std::env::consts::OS,
    );

    // ── 8. Channel Capabilities ─────────────────────────────────────
    prompt.push_str("## Channel Capabilities\n\n");
    prompt.push_str("- You are running as a messaging bot. Your response is automatically sent back to the user's channel.\n");
    prompt.push_str("- You do NOT need to ask permission to respond — just respond directly.\n");
    prompt.push_str("- NEVER repeat, describe, or echo credentials, tokens, API keys, or secrets in your responses.\n");
    prompt.push_str("- If a tool output contains credentials, they have already been redacted — do not mention them.\n\n");

    if prompt.is_empty() {
        "You are ZeroClaw, a fast and efficient AI assistant built in Rust. Be helpful, concise, and direct."
            .to_string()
    } else {
        prompt
    }
}

/// Inject a single workspace file into the prompt with truncation and missing-file markers.
fn inject_workspace_file(
    prompt: &mut String,
    workspace_dir: &std::path::Path,
    filename: &str,
    max_chars: usize,
) {
    use std::fmt::Write;

    let path = workspace_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return;
            }
            let _ = writeln!(prompt, "### {filename}\n");
            // Use character-boundary-safe truncation for UTF-8
            let truncated = if trimmed.chars().count() > max_chars {
                trimmed
                    .char_indices()
                    .nth(max_chars)
                    .map(|(idx, _)| &trimmed[..idx])
                    .unwrap_or(trimmed)
            } else {
                trimmed
            };
            if truncated.len() < trimmed.len() {
                prompt.push_str(truncated);
                let _ = writeln!(
                    prompt,
                    "\n\n[... truncated at {max_chars} chars — use `read` for full file]\n"
                );
            } else {
                prompt.push_str(trimmed);
                prompt.push_str("\n\n");
            }
        }
        Err(_) => {
            // Missing-file marker (matches OpenClaw behavior)
            let _ = writeln!(prompt, "### {filename}\n\n[File not found: {filename}]\n");
        }
    }
}

fn normalize_telegram_identity(value: &str) -> String {
    value.trim().trim_start_matches('@').to_string()
}

async fn bind_telegram_identity(config: &Config, identity: &str) -> Result<()> {
    let normalized = normalize_telegram_identity(identity);
    if normalized.is_empty() {
        anyhow::bail!("Telegram identity cannot be empty");
    }

    let mut updated = config.clone();
    let Some(telegram) = updated.channels_config.telegram.as_mut() else {
        anyhow::bail!(
            "Telegram channel is not configured. Run `zeroclaw onboard --channels-only` first"
        );
    };

    if telegram.allowed_users.iter().any(|u| u == "*") {
        println!(
            "⚠️ Telegram allowlist is currently wildcard (`*`) — binding is unnecessary until you remove '*'."
        );
    }

    if telegram
        .allowed_users
        .iter()
        .map(|entry| normalize_telegram_identity(entry))
        .any(|entry| entry == normalized)
    {
        println!("✅ Telegram identity already bound: {normalized}");
        return Ok(());
    }

    telegram.allowed_users.push(normalized.clone());
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
        if openrc_init_script.exists() {
            if let Ok(status_output) = Command::new("rc-service").args(OPENRC_STATUS_ARGS).output()
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

pub(crate) async fn handle_command(command: crate::ChannelCommands, config: &Config) -> Result<()> {
    match command {
        crate::ChannelCommands::Start => {
            anyhow::bail!("Start must be handled in main.rs (requires async runtime)")
        }
        crate::ChannelCommands::Doctor => {
            anyhow::bail!("Doctor must be handled in main.rs (requires async runtime)")
        }
        crate::ChannelCommands::List => {
            println!("Channels:");
            println!("  ✅ CLI (always available)");
            for (channel, configured) in config.channels_config.channels() {
                println!(
                    "  {} {}",
                    if configured { "✅" } else { "❌" },
                    channel.name()
                );
            }
            if !cfg!(feature = "channel-matrix") {
                println!(
                    "  ℹ️ Matrix channel support is disabled in this build (enable `channel-matrix`)."
                );
            }
            if !cfg!(feature = "channel-lark") {
                println!(
                    "  ℹ️ Lark/Feishu channel support is disabled in this build (enable `channel-lark`)."
                );
            }
            println!("\nTo start channels: zeroclaw channel start");
            println!("To check health:    zeroclaw channel doctor");
            println!("To configure:      zeroclaw onboard");
            Ok(())
        }
        crate::ChannelCommands::Add {
            channel_type,
            config: _,
        } => {
            anyhow::bail!(
                "Channel type '{channel_type}' — use `zeroclaw onboard` to configure channels"
            );
        }
        crate::ChannelCommands::Remove { name } => {
            anyhow::bail!("Remove channel '{name}' — edit ~/.zeroclaw/config.toml directly");
        }
        crate::ChannelCommands::BindTelegram { identity } => {
            bind_telegram_identity(config, &identity).await
        }
    }
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
    channel: Arc<dyn Channel>,
}

fn collect_configured_channels(
    config: &Config,
    matrix_skip_context: &str,
) -> Vec<ConfiguredChannel> {
    // Keep this symbol used even when Matrix support is compiled in and
    // `#[cfg(not(feature = "channel-matrix"))]` blocks are removed.
    let _ = matrix_skip_context;
    let mut channels = Vec::new();

    if let Some(ref tg) = config.channels_config.telegram {
        let mut telegram = TelegramChannel::new(
            tg.bot_token.clone(),
            tg.allowed_users.clone(),
            tg.effective_group_reply_mode().requires_mention(),
            tg.ack_enabled,
        )
        .with_group_reply_allowed_senders(tg.group_reply_allowed_sender_ids())
        .with_ack_reaction(config.channels_config.ack_reaction.telegram.clone())
        .with_streaming(tg.stream_mode, tg.draft_update_interval_ms)
        .with_transcription(config.transcription.clone())
        .with_workspace_dir(config.workspace_dir.clone());

        if let Some(ref base_url) = tg.base_url {
            telegram = telegram.with_api_base(base_url.clone());
        }

        channels.push(ConfiguredChannel {
            display_name: "Telegram",
            channel: Arc::new(telegram),
        });
    }

    if let Some(ref dc) = config.channels_config.discord {
        channels.push(ConfiguredChannel {
            display_name: "Discord",
            channel: Arc::new(
                DiscordChannel::new(
                    dc.bot_token.clone(),
                    dc.guild_id.clone(),
                    dc.allowed_users.clone(),
                    dc.listen_to_bots,
                    dc.effective_group_reply_mode().requires_mention(),
                )
                .with_group_reply_allowed_senders(dc.group_reply_allowed_sender_ids())
                .with_ack_reaction(config.channels_config.ack_reaction.discord.clone())
                .with_workspace_dir(config.workspace_dir.clone()),
            ),
        });
    }

    if let Some(ref sl) = config.channels_config.slack {
        channels.push(ConfiguredChannel {
            display_name: "Slack",
            channel: Arc::new(
                SlackChannel::new(
                    sl.bot_token.clone(),
                    sl.app_token.clone(),
                    sl.channel_id.clone(),
                    sl.channel_ids.clone(),
                    sl.allowed_users.clone(),
                )
                .with_group_reply_policy(
                    sl.effective_group_reply_mode().requires_mention(),
                    sl.group_reply_allowed_sender_ids(),
                ),
            ),
        });
    }

    if let Some(ref mm) = config.channels_config.mattermost {
        channels.push(ConfiguredChannel {
            display_name: "Mattermost",
            channel: Arc::new(
                MattermostChannel::new(
                    mm.url.clone(),
                    mm.bot_token.clone(),
                    mm.channel_id.clone(),
                    mm.allowed_users.clone(),
                    mm.thread_replies.unwrap_or(true),
                    mm.effective_group_reply_mode().requires_mention(),
                )
                .with_group_reply_allowed_senders(mm.group_reply_allowed_sender_ids()),
            ),
        });
    }

    if let Some(ref im) = config.channels_config.imessage {
        channels.push(ConfiguredChannel {
            display_name: "iMessage",
            channel: Arc::new(IMessageChannel::new(im.allowed_contacts.clone())),
        });
    }

    #[cfg(feature = "channel-matrix")]
    if let Some(ref mx) = config.channels_config.matrix {
        channels.push(ConfiguredChannel {
            display_name: "Matrix",
            channel: Arc::new(
                MatrixChannel::new_with_session_hint_and_zeroclaw_dir(
                    mx.homeserver.clone(),
                    mx.access_token.clone(),
                    mx.room_id.clone(),
                    mx.allowed_users.clone(),
                    mx.user_id.clone(),
                    mx.device_id.clone(),
                    config.config_path.parent().map(|path| path.to_path_buf()),
                )
                .with_mention_only(mx.mention_only),
            ),
        });
    }

    #[cfg(not(feature = "channel-matrix"))]
    if config.channels_config.matrix.is_some() {
        tracing::warn!(
            "Matrix channel is configured but this build was compiled without `channel-matrix`; skipping Matrix {}.",
            matrix_skip_context
        );
    }

    if let Some(ref sig) = config.channels_config.signal {
        channels.push(ConfiguredChannel {
            display_name: "Signal",
            channel: Arc::new(SignalChannel::new(
                sig.http_url.clone(),
                sig.account.clone(),
                sig.group_id.clone(),
                sig.allowed_from.clone(),
                sig.ignore_attachments,
                sig.ignore_stories,
            )),
        });
    }

    if let Some(ref wa) = config.channels_config.whatsapp {
        if wa.is_ambiguous_config() {
            tracing::warn!(
                "WhatsApp config has both phone_number_id and session_path set; preferring Cloud API mode. Remove one selector to avoid ambiguity."
            );
        }
        // Runtime negotiation: detect backend type from config
        match wa.backend_type() {
            "cloud" => {
                // Cloud API mode: requires phone_number_id, access_token, verify_token
                if wa.is_cloud_config() {
                    channels.push(ConfiguredChannel {
                        display_name: "WhatsApp",
                        channel: Arc::new(WhatsAppChannel::new(
                            wa.access_token.clone().unwrap_or_default(),
                            wa.phone_number_id.clone().unwrap_or_default(),
                            wa.verify_token.clone().unwrap_or_default(),
                            wa.allowed_numbers.clone(),
                        )),
                    });
                } else {
                    tracing::warn!("WhatsApp Cloud API configured but missing required fields (phone_number_id, access_token, verify_token)");
                }
            }
            "web" => {
                // Web mode: requires session_path
                #[cfg(feature = "whatsapp-web")]
                if wa.is_web_config() {
                    channels.push(ConfiguredChannel {
                        display_name: "WhatsApp",
                        channel: Arc::new(
                            WhatsAppWebChannel::new(
                                wa.session_path.clone().unwrap_or_default(),
                                wa.pair_phone.clone(),
                                wa.pair_code.clone(),
                                wa.allowed_numbers.clone(),
                            )
                            .with_transcription(config.transcription.clone()),
                        ),
                    });
                } else {
                    tracing::warn!("WhatsApp Web configured but session_path not set");
                }
                #[cfg(not(feature = "whatsapp-web"))]
                {
                    tracing::warn!("WhatsApp Web backend requires 'whatsapp-web' feature. Enable with: cargo build --features whatsapp-web");
                }
            }
            _ => {
                tracing::warn!("WhatsApp config invalid: neither phone_number_id (Cloud API) nor session_path (Web) is set");
            }
        }
    }

    if let Some(ref lq) = config.channels_config.linq {
        channels.push(ConfiguredChannel {
            display_name: "Linq",
            channel: Arc::new(LinqChannel::new(
                lq.api_token.clone(),
                lq.from_phone.clone(),
                lq.allowed_senders.clone(),
            )),
        });
    }

    if let Some(ref gh) = config.channels_config.github {
        channels.push(ConfiguredChannel {
            display_name: "GitHub",
            channel: Arc::new(GitHubChannel::new(
                gh.access_token.clone(),
                gh.api_base_url.clone(),
                gh.allowed_repos.clone(),
            )),
        });
    }

    if let Some(ref wati_cfg) = config.channels_config.wati {
        channels.push(ConfiguredChannel {
            display_name: "WATI",
            channel: Arc::new(WatiChannel::new(
                wati_cfg.api_token.clone(),
                wati_cfg.api_url.clone(),
                wati_cfg.tenant_id.clone(),
                wati_cfg.allowed_numbers.clone(),
            )),
        });
    }

    if let Some(ref nc) = config.channels_config.nextcloud_talk {
        channels.push(ConfiguredChannel {
            display_name: "Nextcloud Talk",
            channel: Arc::new(NextcloudTalkChannel::new(
                nc.base_url.clone(),
                nc.app_token.clone(),
                nc.allowed_users.clone(),
            )),
        });
    }

    if let Some(ref email_cfg) = config.channels_config.email {
        channels.push(ConfiguredChannel {
            display_name: "Email",
            channel: Arc::new(EmailChannel::new(email_cfg.clone())),
        });
    }

    if let Some(ref irc) = config.channels_config.irc {
        channels.push(ConfiguredChannel {
            display_name: "IRC",
            channel: Arc::new(IrcChannel::new(irc::IrcChannelConfig {
                server: irc.server.clone(),
                port: irc.port,
                nickname: irc.nickname.clone(),
                username: irc.username.clone(),
                channels: irc.channels.clone(),
                allowed_users: irc.allowed_users.clone(),
                server_password: irc.server_password.clone(),
                nickserv_password: irc.nickserv_password.clone(),
                sasl_password: irc.sasl_password.clone(),
                verify_tls: irc.verify_tls.unwrap_or(true),
            })),
        });
    }

    #[cfg(feature = "channel-lark")]
    if let Some(ref lk) = config.channels_config.lark {
        if lk.use_feishu {
            if config.channels_config.feishu.is_some() {
                tracing::warn!(
                    "Both [channels_config.feishu] and legacy [channels_config.lark].use_feishu=true are configured; ignoring legacy Feishu fallback in lark."
                );
            } else {
                tracing::warn!(
                    "Using legacy [channels_config.lark].use_feishu=true compatibility path; prefer [channels_config.feishu]."
                );
                channels.push(ConfiguredChannel {
                    display_name: "Feishu",
                    channel: Arc::new(
                        LarkChannel::from_config(lk)
                            .with_ack_reaction(config.channels_config.ack_reaction.feishu.clone()),
                    ),
                });
            }
        } else {
            channels.push(ConfiguredChannel {
                display_name: "Lark",
                channel: Arc::new(
                    LarkChannel::from_lark_config(lk)
                        .with_ack_reaction(config.channels_config.ack_reaction.lark.clone()),
                ),
            });
        }
    }

    #[cfg(feature = "channel-lark")]
    if let Some(ref fs) = config.channels_config.feishu {
        channels.push(ConfiguredChannel {
            display_name: "Feishu",
            channel: Arc::new(
                LarkChannel::from_feishu_config(fs)
                    .with_ack_reaction(config.channels_config.ack_reaction.feishu.clone()),
            ),
        });
    }

    #[cfg(not(feature = "channel-lark"))]
    if config.channels_config.lark.is_some() || config.channels_config.feishu.is_some() {
        let executable = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        tracing::warn!(
            "Lark/Feishu channel is configured but this binary was compiled without `channel-lark`; skipping Lark/Feishu startup. \
             binary={executable}. \
             If you built from source, run the built artifact directly (for example `./target/release/zeroclaw daemon`) \
             or run `cargo run --features channel-lark -- daemon`."
        );
    }

    if let Some(ref dt) = config.channels_config.dingtalk {
        channels.push(ConfiguredChannel {
            display_name: "DingTalk",
            channel: Arc::new(DingTalkChannel::new(
                dt.client_id.clone(),
                dt.client_secret.clone(),
                dt.allowed_users.clone(),
            )),
        });
    }

    if let Some(ref qq) = config.channels_config.qq {
        if qq.receive_mode == crate::config::schema::QQReceiveMode::Webhook {
            tracing::info!(
                "QQ channel configured with receive_mode=webhook; websocket listener startup skipped."
            );
        } else {
            channels.push(ConfiguredChannel {
                display_name: "QQ",
                channel: Arc::new(QQChannel::new_with_environment(
                    qq.app_id.clone(),
                    qq.app_secret.clone(),
                    qq.allowed_users.clone(),
                    qq.environment.clone(),
                )),
            });
        }
    }

    if let Some(ref napcat_cfg) = config.channels_config.napcat {
        match NapcatChannel::from_config(napcat_cfg.clone()) {
            Ok(channel) => channels.push(ConfiguredChannel {
                display_name: "Napcat",
                channel: Arc::new(channel),
            }),
            Err(err) => tracing::warn!("Napcat channel configuration invalid: {err}"),
        }
    }

    if let Some(ref ct) = config.channels_config.clawdtalk {
        channels.push(ConfiguredChannel {
            display_name: "ClawdTalk",
            channel: Arc::new(ClawdTalkChannel::new(ct.clone())),
        });
    }

    if let Some(ref acp) = config.channels_config.acp {
        channels.push(ConfiguredChannel {
            display_name: "ACP",
            channel: Arc::new(AcpChannel::new(acp.clone())),
        });
    }
    channels
}

async fn append_nostr_channel_if_available(
    config: &Config,
    channels: &mut Vec<ConfiguredChannel>,
    startup_context: &str,
) -> Option<String> {
    let ns = config.channels_config.nostr.as_ref()?;
    match NostrChannel::new(&ns.private_key, ns.relays.clone(), &ns.allowed_pubkeys).await {
        Ok(channel) => {
            channels.push(ConfiguredChannel {
                display_name: "Nostr",
                channel: Arc::new(channel),
            });
            None
        }
        Err(err) => {
            let reason = format!("Nostr init failed during {startup_context}: {err}");
            tracing::warn!("{reason}");
            Some(reason)
        }
    }
}

/// Run health checks for configured channels.
pub async fn doctor_channels(config: Config) -> Result<()> {
    let mut channels = collect_configured_channels(&config, "health check");
    let mut init_failures = Vec::new();

    if let Some(reason) =
        append_nostr_channel_if_available(&config, &mut channels, "health check").await
    {
        init_failures.push(reason);
    }

    if channels.is_empty() && init_failures.is_empty() {
        println!("No real-time channels configured. Run `zeroclaw onboard` first.");
        return Ok(());
    }

    println!("🩺 ZeroClaw Channel Doctor");
    println!();

    let mut healthy = 0_u32;
    let mut unhealthy = u32::try_from(init_failures.len()).unwrap_or(u32::MAX);
    let mut timeout = 0_u32;
    let has_runtime_channels = !channels.is_empty();

    for failure in &init_failures {
        println!("  ❌ {:<9} {failure}", "Nostr");
    }

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

    if config.channels_config.webhook.is_some() {
        println!("  ℹ️  Webhook   check via `zeroclaw gateway` then GET /health");
    }

    if !has_runtime_channels && !init_failures.is_empty() {
        println!();
        anyhow::bail!("All configured channels failed during initialization.");
    }

    println!();
    println!("Summary: {healthy} healthy, {unhealthy} unhealthy, {timeout} timed out");
    Ok(())
}

/// Start all configured channels and route messages to the agent
#[allow(clippy::too_many_lines)]
pub async fn start_channels(config: Config) -> Result<()> {
    // Ensure stale channel handles are never reused across restarts.
    clear_live_channels();

    if let Err(error) = crate::plugins::runtime::initialize_from_config(&config.plugins) {
        tracing::warn!("plugin registry initialization skipped: {error}");
    }

    let provider_name = resolved_default_provider(&config);
    let provider_runtime_options = providers::ProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: config.api_url.clone(),
        provider_transport: config.effective_provider_transport(),
        zeroclaw_dir: config.config_path.parent().map(std::path::PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        reasoning_level: config.effective_provider_reasoning_level(),
        custom_provider_api_mode: config.provider_api.map(|mode| mode.as_compatible_mode()),
        max_tokens_override: None,
        model_support_vision: config.model_support_vision,
    };
    let provider: Arc<dyn Provider> = Arc::from(
        create_resilient_provider_nonblocking(
            &provider_name,
            config.api_key.clone(),
            config.api_url.clone(),
            config.reliability.clone(),
            provider_runtime_options.clone(),
        )
        .await?,
    );

    // Warm up the provider connection pool (TLS handshake, DNS, HTTP/2 setup)
    // so the first real message doesn't hit a cold-start timeout.
    if let Err(e) = provider.warmup().await {
        tracing::warn!("Provider warmup failed (non-fatal): {e}");
    }

    let initial_stamp = config_file_stamp(&config.config_path).await;
    {
        let mut store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.insert(
            config.config_path.clone(),
            RuntimeConfigState {
                defaults: runtime_defaults_from_config(&config),
                perplexity_filter: config.security.perplexity_filter.clone(),
                outbound_leak_guard: config.security.outbound_leak_guard.clone(),
                last_applied_stamp: initial_stamp,
            },
        );
    }

    let base_observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let observer: Arc<dyn Observer> = Arc::new(
        crate::plugins::bridge::observer::ObserverBridge::new(base_observer),
    );
    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let model = resolved_default_model(&config);
    let temperature = config.default_temperature;
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage(
        &config.memory,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    // Build system prompt from workspace identity files + skills
    let workspace = config.workspace_dir.clone();
    let mut built_tools = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        Arc::clone(&mem),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &workspace,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    );

    // Wire MCP tools into the registry before freezing — non-fatal.
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match crate::tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                let names = registry.tool_names();
                let mut registered = 0usize;
                for name in names {
                    if let Some(def) = registry.get_tool_def(&name).await {
                        let wrapper = crate::tools::McpToolWrapper::new(
                            name,
                            def,
                            std::sync::Arc::clone(&registry),
                        );
                        built_tools.push(Box::new(wrapper));
                        registered += 1;
                    }
                }
                tracing::info!(
                    "MCP: {} tool(s) registered from {} server(s)",
                    registered,
                    registry.server_count()
                );
            }
            Err(e) => {
                // Non-fatal — daemon continues with the tools registered above.
                tracing::error!("MCP registry failed to initialize: {e:#}");
            }
        }
    }

    let tools_registry = Arc::new(built_tools);

    let skills = crate::skills::load_skills_with_config(&workspace, &config);

    // Collect tool descriptions for the prompt
    let mut tool_descs: Vec<(&str, &str)> = vec![
        (
            "shell",
            "Execute terminal commands for local checks, build/test commands, and diagnostics.",
        ),
        (
            "file_read",
            "Read file contents to inspect project files, configs, and logs.",
        ),
        (
            "file_write",
            "Write file contents to apply edits, scaffold files, or update docs/code.",
        ),
        (
            "memory_store",
            "Save to memory to preserve durable preferences, decisions, and key context.",
        ),
        (
            "memory_observe",
            "Store observation memory for long-horizon patterns, signals, and evolving context.",
        ),
        (
            "memory_recall",
            "Search memory to retrieve prior decisions, user preferences, and historical context.",
        ),
        (
            "memory_forget",
            "Delete a memory entry when it's incorrect, stale, or explicitly requested for removal.",
        ),
    ];

    if config.browser.enabled {
        tool_descs.push((
            "browser_open",
            "Open approved HTTPS URLs in system browser (allowlist-only, no scraping)",
        ));
        tool_descs.push((
            "browser",
            "Automate browser actions (open/click/type/scroll/screenshot) with backend-aware safety checks.",
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
        "Send a Pushover notification to your device. Uses PUSHOVER_TOKEN/PUSHOVER_USER_KEY from process environment first, then falls back to .env.",
    ));
    if !config.agents.is_empty() {
        tool_descs.push((
            "delegate",
            "Delegate a subtask to a specialized agent. Use when: a task benefits from a different model (e.g. fast summarization, deep reasoning, code generation). The sub-agent runs a single prompt and returns its response.",
        ));
        tool_descs.push((
            "subagent_spawn",
            "Spawn a delegate agent in the background. Returns immediately with a session_id. Use for long-running tasks that should not block.",
        ));
        tool_descs.push((
            "subagent_list",
            "List running and completed background sub-agents. Filter by status: running, completed, failed, killed, or all.",
        ));
        tool_descs.push((
            "subagent_manage",
            "Manage a background sub-agent: 'status' to check progress/output, 'kill' to cancel a running session.",
        ));
    }

    // Filter out tools excluded for non-CLI channels so the system prompt
    // does not advertise them for channel-driven runs.
    let excluded = &config.autonomy.non_cli_excluded_tools;
    if !excluded.is_empty() {
        tool_descs.retain(|(name, _)| !excluded.iter().any(|ex| ex == name));
    }

    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = build_system_prompt_with_mode(
        &workspace,
        &model,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );
    if !native_tools {
        let filtered_specs = filtered_tool_specs_for_runtime(tools_registry.as_ref(), excluded);
        system_prompt.push_str(&build_tool_instructions_from_specs(&filtered_specs));
    }
    system_prompt.push_str(&build_shell_policy_instructions(&config.autonomy));

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

    // Collect active channels from a shared builder to keep startup and doctor parity.
    let mut configured_channels = collect_configured_channels(&config, "runtime startup");
    let mut init_failures = Vec::new();
    if let Some(reason) =
        append_nostr_channel_if_available(&config, &mut configured_channels, "runtime startup")
            .await
    {
        init_failures.push(reason);
    }

    if configured_channels.is_empty() && init_failures.is_empty() {
        println!("No channels configured. Run `zeroclaw onboard` to set up channels.");
        return Ok(());
    }

    if configured_channels.is_empty() && !init_failures.is_empty() {
        for failure in &init_failures {
            println!("  ❌ {failure}");
        }
        anyhow::bail!("All configured channels failed during initialization.");
    }

    if !init_failures.is_empty() {
        for failure in &init_failures {
            println!("  ⚠️  {failure}");
        }
        println!();
    }

    let channels: Vec<Arc<dyn Channel>> = configured_channels
        .into_iter()
        .map(|configured| configured.channel)
        .collect();

    println!("🦀 ZeroClaw Channel Server");
    println!("  🤖 Model:    {model}");
    let effective_backend = memory::effective_memory_backend_name(
        &config.memory.backend,
        Some(&config.storage.provider.config),
    );
    println!(
        "  🧠 Memory:   {} (auto-save: {})",
        effective_backend,
        if config.memory.auto_save { "on" } else { "off" }
    );
    println!(
        "  📡 Channels: {}",
        channels
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!();
    println!("  Listening for messages... (Ctrl+C to stop)");
    println!();

    crate::health::mark_component_ok("channels");

    let initial_backoff_secs = config
        .reliability
        .channel_initial_backoff_secs
        .max(DEFAULT_CHANNEL_INITIAL_BACKOFF_SECS);
    let max_backoff_secs = config
        .reliability
        .channel_max_backoff_secs
        .max(DEFAULT_CHANNEL_MAX_BACKOFF_SECS);

    // Single message bus — all channels send messages here
    let (tx, rx) = tokio::sync::mpsc::channel::<traits::ChannelMessage>(100);

    // Spawn a listener for each channel
    let mut handles = Vec::new();
    for ch in &channels {
        handles.push(spawn_supervised_listener(
            ch.clone(),
            tx.clone(),
            initial_backoff_secs,
            max_backoff_secs,
        ));
    }
    drop(tx); // Drop our copy so rx closes when all channels stop

    let channels_by_name = Arc::new(
        channels
            .iter()
            .map(|ch| (ch.name().to_string(), Arc::clone(ch)))
            .collect::<HashMap<_, _>>(),
    );
    register_live_channels(channels_by_name.as_ref());
    let max_in_flight_messages = compute_max_in_flight_messages(channels.len());

    println!("  🚦 In-flight message limit: {max_in_flight_messages}");

    let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    provider_cache_seed.insert(provider_name.clone(), Arc::clone(&provider));
    let message_timeout_secs =
        effective_channel_message_timeout_secs(config.channels_config.message_timeout_secs);
    let interrupt_on_new_message = config
        .channels_config
        .telegram
        .as_ref()
        .is_some_and(|tg| tg.interrupt_on_new_message);
    let telegram_progress_mode = config
        .channels_config
        .telegram
        .as_ref()
        .map(|tg| tg.progress_mode)
        .unwrap_or_default();
    set_runtime_telegram_progress_mode(telegram_progress_mode);

    let session_manager = shared_session_manager(&config.agent.session, &config.workspace_dir)?
        .map(|mgr| mgr as Arc<dyn SessionManager + Send + Sync>);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name,
        provider: Arc::clone(&provider),
        default_provider: Arc::new(provider_name),
        memory: Arc::clone(&mem),
        tools_registry: Arc::clone(&tools_registry),
        observer,
        system_prompt: Arc::new(system_prompt),
        model: Arc::new(model.clone()),
        temperature,
        auto_save_memory: config.memory.auto_save,
        max_tool_iterations: config.agent.max_tool_iterations,
        min_relevance_score: config.memory.min_relevance_score,
        conversation_histories: Arc::new(Mutex::new(HashMap::new())),
        conversation_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        session_config: config.agent.session.clone(),
        session_manager,
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        api_key: config.api_key.clone(),
        api_url: config.api_url.clone(),
        reliability: Arc::new(config.reliability.clone()),
        provider_runtime_options,
        workspace_dir: Arc::new(config.workspace_dir.clone()),
        message_timeout_secs,
        interrupt_on_new_message,
        multimodal: config.multimodal.clone(),
        hooks: crate::hooks::create_runner_from_config(&config.hooks),
        non_cli_excluded_tools: Arc::new(Mutex::new(
            config.autonomy.non_cli_excluded_tools.clone(),
        )),
        query_classification: config.query_classification.clone(),
        model_routes: config.model_routes.clone(),
        // Preserve startup perplexity filter config to ensure policy is not weakened
        // when runtime store lookup misses.
        startup_perplexity_filter: config.security.perplexity_filter.clone(),
        approval_manager: {
            // Keep approval policy provenance-bound to static config. Do not
            // auto-approve tool names from untrusted manifest files.
            Arc::new(ApprovalManager::from_config(&config.autonomy))
        },
        safety_heartbeat: if config.agent.safety_heartbeat_interval > 0 {
            Some(SafetyHeartbeatConfig {
                body: security.summary_for_heartbeat(),
                interval: config.agent.safety_heartbeat_interval,
            })
        } else {
            None
        },
    });

    run_message_dispatch_loop(rx, runtime_ctx, max_in_flight_messages).await;

    // Wait for all channel tasks
    for h in handles {
        let _ = h.await;
    }

    clear_live_channels();

    Ok(())
}

#[cfg(test)]
#[allow(clippy::large_futures)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, SqliteMemory};
    use crate::observability::NoopObserver;
    use crate::providers::{ChatMessage, Provider};
    use crate::security::AutonomyLevel;
    use crate::tools::{Tool, ToolResult};
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;

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

    fn mock_price_approved_manager() -> Arc<ApprovalManager> {
        let mut autonomy = crate::config::AutonomyConfig::default();
        if !autonomy
            .auto_approve
            .iter()
            .any(|tool| tool == "mock_price")
        {
            autonomy.auto_approve.push("mock_price".to_string());
        }
        Arc::new(ApprovalManager::from_config(&autonomy))
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
    fn channel_message_timeout_budget_scales_with_tool_iterations() {
        assert_eq!(channel_message_timeout_budget_secs(300, 1), 300);
        assert_eq!(channel_message_timeout_budget_secs(300, 2), 600);
        assert_eq!(channel_message_timeout_budget_secs(300, 3), 900);
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
    fn parse_runtime_command_allows_approval_commands_on_non_model_channels() {
        assert_eq!(
            parse_runtime_command("slack", "/approve-request shell"),
            Some(ChannelRuntimeCommand::RequestToolApproval(
                "shell".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("slack", "/approve-all-once"),
            Some(ChannelRuntimeCommand::RequestAllToolsOnce)
        );
        assert_eq!(
            parse_runtime_command("slack", "/approve-confirm apr-deadbeef"),
            Some(ChannelRuntimeCommand::ConfirmToolApproval(
                "apr-deadbeef".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("slack", "/approve-allow apr-deadbeef"),
            Some(ChannelRuntimeCommand::ApprovePendingRequest(
                "apr-deadbeef".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("slack", "/approve-deny apr-deadbeef"),
            Some(ChannelRuntimeCommand::DenyToolApproval(
                "apr-deadbeef".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("slack", "/approve-pending"),
            Some(ChannelRuntimeCommand::ListPendingApprovals)
        );
        assert_eq!(
            parse_runtime_command("slack", "/approve shell"),
            Some(ChannelRuntimeCommand::ApproveTool("shell".to_string()))
        );
        assert_eq!(
            parse_runtime_command("slack", "/unapprove shell"),
            Some(ChannelRuntimeCommand::UnapproveTool("shell".to_string()))
        );
        assert_eq!(
            parse_runtime_command("slack", "/approvals"),
            Some(ChannelRuntimeCommand::ListApprovals)
        );
        assert_eq!(parse_runtime_command("slack", "/models"), None);
    }

    #[test]
    fn parse_runtime_command_supports_natural_language_approval_intents() {
        assert_eq!(
            parse_runtime_command("telegram", "授权工具 shell"),
            Some(ChannelRuntimeCommand::RequestToolApproval(
                "shell".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("telegram", "请放开 shell"),
            Some(ChannelRuntimeCommand::RequestToolApproval(
                "shell".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("telegram", "approve tool shell"),
            Some(ChannelRuntimeCommand::RequestToolApproval(
                "shell".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("telegram", "请一次性允许所有工具和命令"),
            Some(ChannelRuntimeCommand::RequestAllToolsOnce)
        );
        assert_eq!(
            parse_runtime_command("telegram", "确认授权 apr-deadbeef"),
            Some(ChannelRuntimeCommand::ConfirmToolApproval(
                "apr-deadbeef".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("telegram", "confirm apr-deadbeef"),
            Some(ChannelRuntimeCommand::ConfirmToolApproval(
                "apr-deadbeef".to_string()
            ))
        );
        assert_eq!(
            parse_runtime_command("telegram", "撤销工具 shell"),
            Some(ChannelRuntimeCommand::UnapproveTool("shell".to_string()))
        );
        assert_eq!(
            parse_runtime_command("telegram", "revoke tool shell"),
            Some(ChannelRuntimeCommand::UnapproveTool("shell".to_string()))
        );
        assert_eq!(
            parse_runtime_command("telegram", "查看授权"),
            Some(ChannelRuntimeCommand::ListApprovals)
        );
        assert_eq!(
            parse_runtime_command("telegram", "show approvals"),
            Some(ChannelRuntimeCommand::ListApprovals)
        );
        assert_eq!(
            parse_runtime_command("telegram", "show pending approvals"),
            Some(ChannelRuntimeCommand::ListPendingApprovals)
        );
        assert_eq!(parse_runtime_command("telegram", "请帮我执行shell"), None);
    }

    #[test]
    fn context_window_overflow_error_detector_matches_known_messages() {
        let overflow_err = anyhow::anyhow!(
            "OpenAI Codex stream error: Your input exceeds the context window of this model."
        );
        assert!(is_context_window_overflow_error(&overflow_err));

        let other_err =
            anyhow::anyhow!("OpenAI Codex API error (502 Bad Gateway): error code: 502");
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
        let mut histories = HashMap::new();
        let sender = "telegram_u1".to_string();
        histories.insert(
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
            provider: Arc::new(DummyProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            conversation_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        };

        assert!(compact_sender_history(&ctx, &sender));

        let histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let kept = histories
            .get(&sender)
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
    fn append_sender_turn_stores_single_turn_per_call() {
        let sender = "telegram_u2".to_string();
        let ctx = ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            provider: Arc::new(DummyProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        };

        append_sender_turn(&ctx, &sender, ChatMessage::user("hello"));

        let histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories.get(&sender).expect("sender history should exist");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "hello");
    }

    #[test]
    fn rollback_orphan_user_turn_removes_only_latest_matching_user_turn() {
        let sender = "telegram_u3".to_string();
        let mut histories = HashMap::new();
        histories.insert(
            sender.clone(),
            vec![
                ChatMessage::user("first"),
                ChatMessage::assistant("ok"),
                ChatMessage::user("pending"),
            ],
        );
        let ctx = ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            provider: Arc::new(DummyProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("system".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        };

        assert!(rollback_orphan_user_turn(&ctx, &sender, "pending"));

        let histories = ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .get(&sender)
            .expect("sender history should remain");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].content, "first");
        assert_eq!(turns[1].content, "ok");
    }

    struct DummyProvider;

    #[async_trait::async_trait]
    impl Provider for DummyProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
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
    struct QqRecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
    }

    #[derive(Default)]
    struct TelegramRecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
    }

    #[derive(Default)]
    struct DraftStreamingRecordingChannel {
        sent_messages: tokio::sync::Mutex<Vec<String>>,
        draft_updates: tokio::sync::Mutex<Vec<String>>,
        finalized_drafts: tokio::sync::Mutex<Vec<String>>,
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
            _tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
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

    #[async_trait::async_trait]
    impl Channel for DraftStreamingRecordingChannel {
        fn name(&self) -> &str {
            "draft-streaming-channel"
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
            _tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn supports_draft_updates(&self) -> bool {
            true
        }

        async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
            self.sent_messages
                .lock()
                .await
                .push(format!("draft:{}:{}", message.recipient, message.content));
            Ok(Some("draft-1".to_string()))
        }

        async fn update_draft(
            &self,
            _recipient: &str,
            _message_id: &str,
            text: &str,
        ) -> anyhow::Result<Option<String>> {
            self.draft_updates.lock().await.push(text.to_string());
            Ok(None)
        }

        async fn finalize_draft(
            &self,
            _recipient: &str,
            _message_id: &str,
            text: &str,
        ) -> anyhow::Result<()> {
            self.finalized_drafts.lock().await.push(text.to_string());
            Ok(())
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
            _tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
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

    #[async_trait::async_trait]
    impl Channel for QqRecordingChannel {
        fn name(&self) -> &str {
            "qq"
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
            _tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
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

    struct SlowProvider {
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl Provider for SlowProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            tokio::time::sleep(self.delay).await;
            Ok(format!("echo: {message}"))
        }
    }

    struct ToolCallingProvider;

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
    impl Provider for ToolCallingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok(tool_call_payload())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
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

    struct ToolCallingAliasProvider;

    #[async_trait::async_trait]
    impl Provider for ToolCallingAliasProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok(tool_call_payload_with_alias_tag())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
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

    struct RawToolArtifactProvider;

    #[async_trait::async_trait]
    impl Provider for RawToolArtifactProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok(r#"{"name":"mock_price","parameters":{"symbol":"BTC"}}
{"result":{"symbol":"BTC","price_usd":65000}}
BTC is currently around $65,000 based on latest tool output."#
                .to_string())
        }
    }

    struct IterativeToolProvider {
        required_tool_iterations: usize,
    }

    impl IterativeToolProvider {
        fn completed_tool_iterations(messages: &[ChatMessage]) -> usize {
            messages
                .iter()
                .filter(|msg| msg.role == "user" && msg.content.contains("[Tool results]"))
                .count()
        }
    }

    #[async_trait::async_trait]
    impl Provider for IterativeToolProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok(tool_call_payload())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
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

    #[derive(Default)]
    struct HistoryCaptureProvider {
        calls: std::sync::Mutex<Vec<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl Provider for HistoryCaptureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            let snapshot = messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect::<Vec<_>>();
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            calls.push(snapshot);
            Ok(format!("response-{}", calls.len()))
        }
    }

    struct DelayedHistoryCaptureProvider {
        delay: Duration,
        calls: std::sync::Mutex<Vec<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl Provider for DelayedHistoryCaptureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
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

    struct MockPriceTool;

    #[derive(Default)]
    struct ModelCaptureProvider {
        call_count: AtomicUsize,
        models: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Provider for ModelCaptureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("fallback".to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(model.to_string());
            Ok("ok".to_string())
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

    struct MockEchoTool;

    #[async_trait::async_trait]
    impl Tool for MockEchoTool {
        fn name(&self) -> &str {
            "mock_echo"
        }

        fn description(&self) -> &str {
            "Echo back the input text"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                error: None,
            })
        }
    }

    struct MockProcessTool;

    #[async_trait::async_trait]
    impl Tool for MockProcessTool {
        fn name(&self) -> &str {
            "process"
        }

        fn description(&self) -> &str {
            "Mock process tool for runtime visibility tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" }
                }
            })
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    #[test]
    fn build_runtime_tool_visibility_prompt_respects_excluded_snapshot() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool), Box::new(MockEchoTool)];
        let excluded = vec!["mock_price".to_string()];

        let non_native = build_runtime_tool_visibility_prompt(&tools, &excluded, false);
        assert!(non_native.contains("Runtime Tool Availability (Authoritative)"));
        assert!(non_native.contains("Excluded by runtime policy: mock_price"));
        assert!(non_native.contains("`mock_echo`"));
        assert!(!non_native.contains("**mock_price**:"));
        assert!(non_native.contains("## Tool Use Protocol"));

        let native = build_runtime_tool_visibility_prompt(&tools, &excluded, true);
        assert!(native.contains("Runtime Tool Availability (Authoritative)"));
        assert!(native.contains("native provider function-calling"));
        assert!(!native.contains("## Tool Use Protocol"));
    }

    #[test]
    fn build_runtime_tool_visibility_prompt_excludes_process_with_default_policy() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockProcessTool), Box::new(MockEchoTool)];
        let excluded = crate::config::AutonomyConfig::default().non_cli_excluded_tools;

        assert!(
            excluded.contains(&"process".to_string()),
            "default non-CLI exclusion list must include process"
        );

        let prompt = build_runtime_tool_visibility_prompt(&tools, &excluded, false);
        assert!(prompt.contains("Excluded by runtime policy:"));
        assert!(prompt.contains("process"));
        assert!(!prompt.contains("**process**:"));
        assert!(prompt.contains("`mock_echo`"));
    }

    #[tokio::test]
    async fn process_channel_message_injects_runtime_tool_visibility_prompt() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool), Box::new(MockEchoTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(vec!["mock_price".to_string()])),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-runtime-visibility-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-runtime-visibility".to_string(),
                content: "hello tool visibility".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        {
            let calls = provider_impl
                .calls
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert_eq!(calls.len(), 1);
            let first_call = &calls[0];
            assert!(!first_call.is_empty());
            assert_eq!(first_call[0].0, "system");
            let system_prompt = &first_call[0].1;
            assert!(system_prompt.contains("Runtime Tool Availability (Authoritative)"));
            assert!(system_prompt.contains("Excluded by runtime policy: mock_price"));
            assert!(system_prompt.contains("`mock_echo`"));
            assert!(!system_prompt.contains("**mock_price**:"));
        }

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("response-1"));
    }

    #[tokio::test]
    async fn process_channel_message_executes_tool_calls_instead_of_sending_raw_json() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let autonomy_cfg = crate::config::AutonomyConfig {
            level: AutonomyLevel::Full,
            auto_approve: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(ToolCallingProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-42".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("chat-42:"));
        assert!(sent_messages[0].contains("BTC is currently around"));
        assert!(!sent_messages[0].contains("\"tool_calls\""));
        assert!(!sent_messages[0].contains("mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_telegram_does_not_persist_tool_summary_prefix() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let autonomy_cfg = crate::config::AutonomyConfig {
            level: AutonomyLevel::Full,
            auto_approve: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(ToolCallingProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-telegram-tool-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-telegram".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].contains("BTC is currently around"));

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .get("telegram_alice")
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
    async fn process_channel_message_streaming_hides_internal_progress_by_default() {
        let channel_impl = Arc::new(DraftStreamingRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let autonomy_cfg = crate::config::AutonomyConfig {
            level: AutonomyLevel::Full,
            auto_approve: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(ToolCallingProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            approval_manager: mock_price_approved_manager(),
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-stream-hide".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-stream".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "draft-streaming-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let updates = channel_impl.draft_updates.lock().await;
        assert!(
            !updates.is_empty(),
            "draft updates should still include streamed final answer"
        );
        assert!(
            !updates.iter().any(|entry| {
                entry.contains("Thinking")
                    || entry.contains("Got 1 tool call(s)")
                    || entry.contains("mock_price")
                    || entry.contains("⏳")
            }),
            "internal tool progress should stay hidden by default, got updates: {updates:?}"
        );
        drop(updates);

        let finalized = channel_impl.finalized_drafts.lock().await;
        assert_eq!(finalized.len(), 1);
        assert!(finalized[0].contains("BTC is currently around"));
    }

    #[tokio::test]
    async fn process_channel_message_streaming_shows_internal_progress_on_explicit_request() {
        let channel_impl = Arc::new(DraftStreamingRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let autonomy_cfg = crate::config::AutonomyConfig {
            level: AutonomyLevel::Full,
            auto_approve: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(ToolCallingProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            approval_manager: mock_price_approved_manager(),
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-stream-show".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-stream".to_string(),
                content: "Please show commands and tool calls you used.".to_string(),
                channel: "draft-streaming-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let updates = channel_impl.draft_updates.lock().await;
        assert!(
            updates
                .iter()
                .any(|entry| entry.contains("Got 1 tool call(s)")),
            "explicit requests should expose internal progress details, got updates: {updates:?}"
        );
        assert!(
            updates.iter().any(|entry| entry.contains("Thinking")),
            "explicit requests should expose internal thinking/progress text, got updates: {updates:?}"
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
            provider: Arc::new(RawToolArtifactProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-raw-json".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-raw".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 3,
                thread_ts: None,
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

        let autonomy_cfg = crate::config::AutonomyConfig {
            level: AutonomyLevel::Full,
            auto_approve: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(ToolCallingAliasProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-2".to_string(),
                sender: "bob".to_string(),
                reply_target: "chat-84".to_string(),
                content: "What is the BTC price now?".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("chat-84:"));
        assert!(sent_messages[0].contains("alias-tag flow resolved"));
        assert!(!sent_messages[0].contains("<toolcall>"));
        assert!(!sent_messages[0].contains("mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_handles_models_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let default_provider_impl = Arc::new(ModelCaptureProvider::default());
        let default_provider: Arc<dyn Provider> = default_provider_impl.clone();
        let fallback_provider_impl = Arc::new(ModelCaptureProvider::default());
        let fallback_provider: Arc<dyn Provider> = fallback_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&default_provider));
        provider_cache_seed.insert("openrouter".to_string(), fallback_provider);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&default_provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-cmd-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/models openrouter".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Provider switched to `openrouter`"));

        let route_key = "telegram_alice";
        let route = runtime_ctx
            .route_overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(route_key)
            .cloned()
            .expect("route should be stored for sender");
        assert_eq!(route.provider, "openrouter");
        assert_eq!(route.model, "default-model");

        assert_eq!(default_provider_impl.call_count.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_handles_approve_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_excluded_tools = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm;
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            non_cli_natural_language_approval_mode:
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(vec!["mock_price".to_string()])),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });
        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_natural_language_approval_mode_for_channel("telegram"),
            crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm
        );
        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_natural_language_approval_mode_for_channel("telegram"),
            crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm
        );

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-approve-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/approve mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Approved supervised execution for `mock_price`"));
        assert!(sent[0].contains("including after restart"));
        assert!(sent[0].contains("Removed `mock_price` from `autonomy.non_cli_excluded_tools`"));

        assert!(runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(!runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(
            saved
                .autonomy
                .auto_approve
                .iter()
                .any(|tool| tool == "mock_price"),
            "persisted config should include mock_price in autonomy.auto_approve"
        );
        assert!(
            saved
                .autonomy
                .always_ask
                .iter()
                .all(|tool| tool != "mock_price"),
            "persisted config should remove mock_price from autonomy.always_ask"
        );
        assert!(
            saved
                .autonomy
                .non_cli_excluded_tools
                .iter()
                .all(|tool| tool != "mock_price"),
            "persisted config should remove mock_price from autonomy.non_cli_excluded_tools"
        );
        assert!(
            snapshot_non_cli_excluded_tools(runtime_ctx.as_ref())
                .iter()
                .all(|tool| tool != "mock_price"),
            "runtime exclusions should remove mock_price immediately after approval"
        );
    }

    #[tokio::test]
    async fn process_channel_message_handles_approve_allow_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let approval_manager = Arc::new(ApprovalManager::from_config(
            &crate::config::AutonomyConfig::default(),
        ));
        let pending = approval_manager.create_non_cli_pending_request(
            "mock_price",
            "alice",
            "telegram",
            "chat-1",
            None,
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::clone(&approval_manager),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-approve-allow-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("/approve-allow {}", pending.request_id),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Approved pending execution request"));
        assert!(sent[0].contains("mock_price"));
        drop(sent);

        assert_eq!(
            approval_manager.take_non_cli_pending_resolution(&pending.request_id),
            Some(ApprovalResponse::Yes)
        );
        assert!(!approval_manager.has_non_cli_pending_request(&pending.request_id));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_handles_approve_deny_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let approval_manager = Arc::new(ApprovalManager::from_config(
            &crate::config::AutonomyConfig::default(),
        ));
        let pending = approval_manager.create_non_cli_pending_request(
            "mock_price",
            "alice",
            "telegram",
            "chat-1",
            None,
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::clone(&approval_manager),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-approve-deny-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("/approve-deny {}", pending.request_id),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Rejected approval request"));
        assert!(sent[0].contains("mock_price"));
        drop(sent);

        assert_eq!(
            approval_manager.take_non_cli_pending_resolution(&pending.request_id),
            Some(ApprovalResponse::No)
        );
        assert!(!approval_manager.has_non_cli_pending_request(&pending.request_id));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_prompts_and_waits_for_non_cli_always_ask_approval() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(ToolCallingProvider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        let runtime_ctx_for_first_turn = runtime_ctx.clone();
        let first_turn = tokio::spawn(async move {
            process_channel_message(
                runtime_ctx_for_first_turn,
                traits::ChannelMessage {
                    id: "msg-non-cli-approval-1".to_string(),
                    sender: "alice".to_string(),
                    reply_target: "chat-1".to_string(),
                    content: "What is the BTC price now?".to_string(),
                    channel: "telegram".to_string(),
                    timestamp: 1,
                    thread_ts: None,
                },
                CancellationToken::new(),
            )
            .await;
        });

        let request_id = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let pending = runtime_ctx.approval_manager.list_non_cli_pending_requests(
                    Some("alice"),
                    Some("telegram"),
                    Some("chat-1"),
                );
                if let Some(req) = pending.first() {
                    break req.request_id.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("pending approval request should be created for always_ask tool");

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-non-cli-approval-2".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("/approve-allow {request_id}"),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        tokio::time::timeout(Duration::from_secs(5), first_turn)
            .await
            .expect("first channel turn should finish after approval")
            .expect("first channel turn task should not panic");

        let sent = channel_impl.sent_messages.lock().await;
        assert!(
            sent.iter()
                .any(|entry| entry.contains("Approval required for tool `mock_price`")),
            "channel should emit non-cli approval prompt"
        );
        assert!(
            sent.iter()
                .any(|entry| entry.contains("Approved pending execution request")),
            "channel should acknowledge explicit approval command"
        );
        assert!(
            sent.iter()
                .any(|entry| entry.contains("BTC is currently around")),
            "tool call should execute after approval and produce final response"
        );
        assert!(
            sent.iter().all(|entry| !entry.contains("Denied by user.")),
            "always_ask tool should not be silently denied once non-cli approval prompt path is wired"
        );
        assert!(
            runtime_ctx.approval_manager.needs_approval("mock_price"),
            "/approve-allow should not downgrade always_ask policy for future requests"
        );
        assert!(
            runtime_ctx
                .approval_manager
                .always_ask_tools()
                .contains("mock_price"),
            "always_ask runtime policy should remain intact after one-shot approval"
        );
    }

    #[tokio::test]
    async fn process_channel_message_denies_approval_management_for_unlisted_sender() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_approval_approvers = vec!["alice".to_string()];
        persisted
            .autonomy
            .non_cli_natural_language_approval_mode_by_channel
            .insert(
                "telegram".to_string(),
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            );
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            non_cli_approval_approvers: vec!["alice".to_string()],
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });
        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_natural_language_approval_mode_for_channel("telegram"),
            crate::config::NonCliNaturalLanguageApprovalMode::Direct
        );

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-approve-denied-1".to_string(),
                sender: "bob".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/approve mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Approval-management command denied"));
        assert!(sent[0].contains("Allowed approvers: alice"));
        assert!(!runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(
            saved
                .autonomy
                .auto_approve
                .iter()
                .all(|tool| tool != "mock_price"),
            "persisted config should not include unauthorized approval changes"
        );
    }

    #[tokio::test]
    async fn process_channel_message_handles_unapprove_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.auto_approve = vec!["mock_price".to_string()];
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            auto_approve: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));
        approval_manager.grant_non_cli_session("mock_price");

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager,
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-unapprove-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/unapprove mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Persistent approval removed for `mock_price`: yes."));
        assert!(sent[0].contains("Runtime session grant removed: yes"));
        assert!(!runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(
            saved
                .autonomy
                .auto_approve
                .iter()
                .all(|tool| tool != "mock_price"),
            "persisted config should remove mock_price from autonomy.auto_approve"
        );
    }

    #[tokio::test]
    async fn process_channel_message_handles_approvals_command_without_llm_call() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.auto_approve = vec!["mock_price".to_string()];
        persisted.autonomy.always_ask = vec!["shell".to_string()];
        persisted.autonomy.non_cli_excluded_tools = vec!["shell".to_string()];
        persisted.save().await.expect("save config");

        let approval_manager = Arc::new(ApprovalManager::from_config(
            &crate::config::AutonomyConfig::default(),
        ));
        approval_manager.grant_non_cli_session("shell");
        approval_manager.grant_non_cli_allow_all_once();

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(vec!["shell".to_string()])),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager,
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-approvals-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/approvals".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Supervised non-CLI tool approvals:"));
        assert!(sent[0].contains("Runtime session grants: shell"));
        assert!(sent[0].contains("Runtime one-time all-tools bypass tokens: 1"));
        assert!(sent[0].contains("Runtime non_cli_approval_approvers:"));
        assert!(sent[0].contains("Runtime non_cli_natural_language_approval_mode:"));
        assert!(sent[0].contains("Runtime non_cli_natural_language_approval_mode_by_channel:"));
        assert!(sent[0].contains("Runtime non_cli_excluded_tools: shell"));
        assert!(sent[0].contains("Persisted autonomy.auto_approve: mock_price"));
        assert!(sent[0].contains("Persisted autonomy.always_ask: shell"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_approve_allow_resolves_pending_request_yes() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));
        let pending = approval_manager.create_non_cli_pending_request(
            "mock_price",
            "alice",
            "telegram",
            "chat-1",
            Some("integration test approval prompt".to_string()),
        );
        let request_id = pending.request_id.clone();

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::clone(&approval_manager),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-approve-allow-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("/approve-allow {request_id}"),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Approved pending execution request"));
        assert!(sent[0].contains("mock_price"));
        drop(sent);

        assert!(approval_manager
            .list_non_cli_pending_requests(Some("alice"), Some("telegram"), Some("chat-1"))
            .is_empty());
        assert_eq!(
            approval_manager.take_non_cli_pending_resolution(&request_id),
            Some(ApprovalResponse::Yes)
        );
        assert!(
            approval_manager.needs_approval("mock_price"),
            "/approve-allow should not persistently auto-approve tools"
        );
        assert!(
            approval_manager.always_ask_tools().contains("mock_price"),
            "always_ask tool should remain in always_ask after one-shot approval"
        );
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_approve_deny_resolves_pending_request_no() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        let approval_manager = Arc::new(ApprovalManager::from_config(&autonomy_cfg));
        let pending = approval_manager.create_non_cli_pending_request(
            "mock_price",
            "alice",
            "telegram",
            "chat-1",
            Some("integration test approval prompt".to_string()),
        );
        let request_id = pending.request_id.clone();

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::clone(&approval_manager),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-approve-deny-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("/approve-deny {request_id}"),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Rejected approval request"));
        assert!(sent[0].contains("mock_price"));
        drop(sent);

        assert!(approval_manager
            .list_non_cli_pending_requests(Some("alice"), Some("telegram"), Some("chat-1"))
            .is_empty());
        assert_eq!(
            approval_manager.take_non_cli_pending_resolution(&request_id),
            Some(ApprovalResponse::No)
        );
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_natural_request_then_confirm_approval() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_excluded_tools = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm;
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            non_cli_natural_language_approval_mode:
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(temp.path().join("workspace")),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(vec!["mock_price".to_string()])),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-req-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "授权工具 mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let request_id = {
            let sent = channel_impl.sent_messages.lock().await;
            assert_eq!(sent.len(), 1);
            assert!(
                sent[0].contains("Approval request created."),
                "unexpected response: {}",
                sent[0]
            );
            let request_line = sent[0]
                .lines()
                .find(|line| line.starts_with("Request ID: `"))
                .expect("request line");
            request_line
                .trim_start_matches("Request ID: `")
                .trim_end_matches('`')
                .to_string()
        };
        assert!(request_id.starts_with("apr-"));

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-req-2".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("确认授权 {request_id}"),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("Approved supervised execution for `mock_price` from request"));
        assert!(sent[1].contains("Removed `mock_price` from `autonomy.non_cli_excluded_tools`"));
        assert!(runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(!runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert!(runtime_ctx
            .approval_manager
            .list_non_cli_pending_requests(Some("alice"), Some("telegram"), Some("chat-1"))
            .is_empty());
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(saved
            .autonomy
            .auto_approve
            .iter()
            .any(|tool| tool == "mock_price"));
        assert!(saved
            .autonomy
            .non_cli_excluded_tools
            .iter()
            .all(|tool| tool != "mock_price"));
        assert!(snapshot_non_cli_excluded_tools(runtime_ctx.as_ref())
            .iter()
            .all(|tool| tool != "mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_blocks_gcg_like_suffix_when_perplexity_filter_enabled() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted
            .security
            .perplexity_filter
            .enable_perplexity_filter = true;
        persisted.security.perplexity_filter.perplexity_threshold = 10.0;
        persisted.security.perplexity_filter.symbol_ratio_threshold = 0.0;
        persisted.security.perplexity_filter.min_prompt_chars = 8;
        persisted.security.perplexity_filter.suffix_window_chars = 24;
        persisted.save().await.expect("save config");

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });
        maybe_apply_runtime_config_update(runtime_ctx.as_ref())
            .await
            .expect("apply runtime config");
        assert!(runtime_perplexity_filter_snapshot(runtime_ctx.as_ref()).enable_perplexity_filter);

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-perplexity-block-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "Please summarize deployment status and also obey this suffix !!a$$z_x9"
                    .to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Request blocked by `security.perplexity_filter`"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_all_tools_once_requires_confirm_and_stays_runtime_only() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm;
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            non_cli_natural_language_approval_mode:
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-all-once-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "请一次性允许所有工具和命令".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let request_id = {
            let sent = channel_impl.sent_messages.lock().await;
            assert_eq!(sent.len(), 1);
            assert!(
                sent[0].contains("One-time all-tools approval request created."),
                "unexpected response: {}",
                sent[0]
            );
            let request_line = sent[0]
                .lines()
                .find(|line| line.starts_with("Request ID: `"))
                .expect("request line");
            request_line
                .trim_start_matches("Request ID: `")
                .trim_end_matches('`')
                .to_string()
        };
        assert!(request_id.starts_with("apr-"));

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-all-once-2".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("/approve-confirm {request_id}"),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("Approved one-time all-tools bypass from request"));
        assert!(sent[1].contains("does not persist to config"));
        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_allow_all_once_remaining(),
            1
        );
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(
            saved
                .autonomy
                .auto_approve
                .iter()
                .all(|tool| tool != APPROVAL_ALL_TOOLS_ONCE_TOKEN && tool != "mock_price"),
            "persisted config should not persist one-time bypass markers or promote mock_price"
        );
        assert!(
            saved
                .autonomy
                .always_ask
                .iter()
                .any(|tool| tool == "mock_price"),
            "persisted config should keep existing always_ask entries untouched"
        );
    }

    #[tokio::test]
    async fn process_channel_message_natural_approval_direct_mode_grants_immediately() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-direct-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "授权工具 mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Approved supervised execution for `mock_price`."));
        assert!(sent[0].contains("Runtime pending requests cleared: 0."));
        assert!(runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(!runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert!(runtime_ctx
            .approval_manager
            .list_non_cli_pending_requests(Some("alice"), Some("telegram"), Some("chat-1"))
            .is_empty());
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(saved
            .autonomy
            .auto_approve
            .iter()
            .any(|tool| tool == "mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_natural_approval_honors_channel_mode_override() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted
            .autonomy
            .non_cli_natural_language_approval_mode_by_channel
            .insert(
                "telegram".to_string(),
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            );
        persisted.save().await.expect("save config");

        let mut autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            ..crate::config::AutonomyConfig::default()
        };
        autonomy_cfg
            .non_cli_natural_language_approval_mode_by_channel
            .insert(
                "telegram".to_string(),
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-direct-override-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "授权工具 mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].contains("Approval request created."),
            "unexpected response: {}",
            sent[0]
        );
        assert!(!runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_channel_message_natural_approval_can_be_disabled_but_slash_still_works() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let mut persisted = Config::default();
        persisted.config_path = config_path.clone();
        persisted.workspace_dir = workspace_dir;
        persisted.autonomy.always_ask = vec!["mock_price".to_string()];
        persisted.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::Disabled;
        persisted.save().await.expect("save config");

        let autonomy_cfg = crate::config::AutonomyConfig {
            always_ask: vec!["mock_price".to_string()],
            non_cli_natural_language_approval_mode:
                crate::config::NonCliNaturalLanguageApprovalMode::Disabled,
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-nl-disabled-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "授权工具 mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        {
            let sent = channel_impl.sent_messages.lock().await;
            assert_eq!(sent.len(), 1);
            assert!(
                sent[0].contains("Natural-language approval commands are disabled"),
                "unexpected response: {}",
                sent[0]
            );
        }
        assert!(!runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(runtime_ctx.approval_manager.needs_approval("mock_price"));

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-nl-disabled-2".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "/approve mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("Approved supervised execution for `mock_price`."));
        assert!(runtime_ctx
            .approval_manager
            .is_non_cli_session_granted("mock_price"));
        assert!(!runtime_ctx.approval_manager.needs_approval("mock_price"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let saved_raw = tokio::fs::read_to_string(&config_path)
            .await
            .expect("read persisted config");
        let saved: Config = toml::from_str(&saved_raw).expect("parse persisted config");
        assert!(saved
            .autonomy
            .auto_approve
            .iter()
            .any(|tool| tool == "mock_price"));
    }

    #[tokio::test]
    async fn process_channel_message_confirm_rejects_sender_mismatch() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let autonomy_cfg = crate::config::AutonomyConfig {
            non_cli_natural_language_approval_mode:
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            ..crate::config::AutonomyConfig::default()
        };

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(&autonomy_cfg)),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-mismatch-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "授权工具 mock_price".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let request_id = {
            let sent = channel_impl.sent_messages.lock().await;
            assert_eq!(sent.len(), 1);
            let request_line = sent[0]
                .lines()
                .find(|line| line.starts_with("Request ID: `"))
                .expect("request line");
            request_line
                .trim_start_matches("Request ID: `")
                .trim_end_matches('`')
                .to_string()
        };

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-mismatch-2".to_string(),
                sender: "bob".to_string(),
                reply_target: "chat-1".to_string(),
                content: format!("confirm {request_id}"),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent = channel_impl.sent_messages.lock().await;
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("can only be confirmed by the same sender"));
        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 0);

        let pending = runtime_ctx.approval_manager.list_non_cli_pending_requests(
            Some("alice"),
            Some("telegram"),
            Some("chat-1"),
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, request_id);
    }

    #[tokio::test]
    async fn process_channel_message_uses_route_override_provider_and_model() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let default_provider_impl = Arc::new(ModelCaptureProvider::default());
        let default_provider: Arc<dyn Provider> = default_provider_impl.clone();
        let routed_provider_impl = Arc::new(ModelCaptureProvider::default());
        let routed_provider: Arc<dyn Provider> = routed_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&default_provider));
        provider_cache_seed.insert("openrouter".to_string(), routed_provider);

        let route_key = "telegram_alice".to_string();
        let mut route_overrides = HashMap::new();
        route_overrides.insert(
            route_key,
            ChannelRouteSelection {
                provider: "openrouter".to_string(),
                model: "route-model".to_string(),
            },
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&default_provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(route_overrides)),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-routed-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello routed provider".to_string(),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        assert_eq!(default_provider_impl.call_count.load(Ordering::SeqCst), 0);
        assert_eq!(routed_provider_impl.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            routed_provider_impl
                .models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            &["route-model".to_string()]
        );
    }

    #[tokio::test]
    async fn process_channel_message_prefers_cached_default_provider_instance() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let startup_provider_impl = Arc::new(ModelCaptureProvider::default());
        let startup_provider: Arc<dyn Provider> = startup_provider_impl.clone();
        let reloaded_provider_impl = Arc::new(ModelCaptureProvider::default());
        let reloaded_provider: Arc<dyn Provider> = reloaded_provider_impl.clone();

        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), reloaded_provider);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&startup_provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("default-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-default-provider-cache".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello cached default provider".to_string(),
                channel: "telegram".to_string(),
                timestamp: 3,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        assert_eq!(startup_provider_impl.call_count.load(Ordering::SeqCst), 0);
        assert_eq!(reloaded_provider_impl.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn process_channel_message_uses_runtime_default_model_from_store() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(ModelCaptureProvider::default());
        let provider: Arc<dyn Provider> = provider_impl.clone();
        let mut provider_cache_seed: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        provider_cache_seed.insert("test-provider".to_string(), Arc::clone(&provider));

        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");

        {
            let mut store = runtime_config_store()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            store.insert(
                config_path.clone(),
                RuntimeConfigState {
                    defaults: ChannelRuntimeDefaults {
                        default_provider: "test-provider".to_string(),
                        model: "hot-reloaded-model".to_string(),
                        temperature: 0.5,
                        api_key: None,
                        api_url: None,
                        reliability: crate::config::ReliabilityConfig::default(),
                        cost: crate::config::CostConfig::default(),
                        auto_save_memory: false,
                        max_tool_iterations: 5,
                        min_relevance_score: 0.0,
                        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
                        interrupt_on_new_message: false,
                        multimodal: crate::config::MultimodalConfig::default(),
                        query_classification: crate::config::QueryClassificationConfig::default(),
                        model_routes: Vec::new(),
                    },
                    perplexity_filter: crate::config::PerplexityFilterConfig::default(),
                    outbound_leak_guard: crate::config::OutboundLeakGuardConfig::default(),
                    last_applied_stamp: None,
                },
            );
        }

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::clone(&provider),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("startup-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-runtime-store-model".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello runtime defaults".to_string(),
                channel: "telegram".to_string(),
                timestamp: 4,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        {
            let mut store = runtime_config_store()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            store.remove(&config_path);
        }

        assert_eq!(provider_impl.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider_impl
                .models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            &["hot-reloaded-model".to_string()]
        );
    }

    #[tokio::test]
    async fn load_runtime_defaults_from_config_file_includes_autonomy_policy() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");

        let mut cfg = Config::default();
        cfg.config_path = config_path.clone();
        cfg.workspace_dir = workspace_dir;
        cfg.default_provider = Some("test-provider".to_string());
        cfg.default_model = Some("test-model".to_string());
        cfg.autonomy.auto_approve = vec!["mock_price".to_string()];
        cfg.autonomy.always_ask = vec!["shell".to_string()];
        cfg.autonomy.non_cli_excluded_tools = vec!["browser_open".to_string()];
        cfg.autonomy.non_cli_approval_approvers = vec!["telegram:alice".to_string()];
        cfg.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::Direct;
        cfg.autonomy
            .non_cli_natural_language_approval_mode_by_channel
            .insert(
                "telegram".to_string(),
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            );
        cfg.security.perplexity_filter.enable_perplexity_filter = true;
        cfg.security.perplexity_filter.perplexity_threshold = 15.5;
        cfg.security.outbound_leak_guard.enabled = true;
        cfg.security.outbound_leak_guard.action = crate::config::OutboundLeakGuardAction::Block;
        cfg.security.outbound_leak_guard.sensitivity = 0.95;
        cfg.save().await.expect("save config");

        let (_defaults, policy) = load_runtime_defaults_from_config_file(&config_path)
            .await
            .expect("load runtime state");

        assert_eq!(policy.auto_approve, vec!["mock_price".to_string()]);
        assert_eq!(policy.always_ask, vec!["shell".to_string()]);
        assert_eq!(
            policy.non_cli_excluded_tools,
            vec!["browser_open".to_string()]
        );
        assert_eq!(
            policy.non_cli_approval_approvers,
            vec!["telegram:alice".to_string()]
        );
        assert_eq!(
            policy.non_cli_natural_language_approval_mode,
            crate::config::NonCliNaturalLanguageApprovalMode::Direct
        );
        assert_eq!(
            policy
                .non_cli_natural_language_approval_mode_by_channel
                .get("telegram")
                .copied(),
            Some(crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm)
        );
        assert!(policy.perplexity_filter.enable_perplexity_filter);
        assert_eq!(policy.perplexity_filter.perplexity_threshold, 15.5);
        assert!(policy.outbound_leak_guard.enabled);
        assert_eq!(
            policy.outbound_leak_guard.action,
            crate::config::OutboundLeakGuardAction::Block
        );
        assert_eq!(policy.outbound_leak_guard.sensitivity, 0.95);
    }

    #[tokio::test]
    async fn load_runtime_defaults_from_config_file_uses_provider_fallback_when_model_missing() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");

        let mut cfg = Config::default();
        cfg.config_path = config_path.clone();
        cfg.workspace_dir = workspace_dir;
        cfg.default_provider = Some("openai".to_string());
        cfg.default_model = None;
        cfg.save().await.expect("save config");

        let (defaults, _policy) = load_runtime_defaults_from_config_file(&config_path)
            .await
            .expect("runtime defaults");
        assert_eq!(defaults.default_provider, "openai");
        assert_eq!(defaults.model, "gpt-5.2");
    }

    #[tokio::test]
    async fn maybe_apply_runtime_config_update_refreshes_autonomy_policy_and_excluded_tools() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");

        let mut cfg = Config::default();
        cfg.config_path = config_path.clone();
        cfg.workspace_dir = workspace_dir;
        cfg.default_provider = Some("ollama".to_string());
        cfg.default_model = Some("llama3.2".to_string());
        cfg.api_key = Some("http://127.0.0.1:11434".to_string());
        cfg.memory.auto_save = false;
        cfg.memory.min_relevance_score = 0.15;
        cfg.agent.max_tool_iterations = 5;
        cfg.channels_config.message_timeout_secs = 45;
        cfg.multimodal.allow_remote_fetch = false;
        cfg.query_classification.enabled = false;
        cfg.model_routes = vec![];
        cfg.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::Direct;
        cfg.autonomy.non_cli_excluded_tools = vec!["shell".to_string()];
        cfg.security.perplexity_filter.enable_perplexity_filter = false;
        cfg.save().await.expect("save initial config");

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(HashMap::new()),
            provider: Arc::new(ModelCaptureProvider::default()),
            default_provider: Arc::new("ollama".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("llama3.2".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: Some("http://127.0.0.1:11434".to_string()),
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions {
                zeroclaw_dir: Some(temp.path().to_path_buf()),
                ..providers::ProviderRuntimeOptions::default()
            },
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        maybe_apply_runtime_config_update(runtime_ctx.as_ref())
            .await
            .expect("apply initial config");

        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_natural_language_approval_mode_for_channel("telegram"),
            crate::config::NonCliNaturalLanguageApprovalMode::Direct
        );
        assert_eq!(
            snapshot_non_cli_excluded_tools(runtime_ctx.as_ref()),
            vec!["shell".to_string()]
        );
        assert!(!runtime_perplexity_filter_snapshot(runtime_ctx.as_ref()).enable_perplexity_filter);
        assert_eq!(
            runtime_outbound_leak_guard_snapshot(runtime_ctx.as_ref()).action,
            crate::config::OutboundLeakGuardAction::Redact
        );
        let defaults = runtime_defaults_snapshot(runtime_ctx.as_ref());
        assert!(!defaults.auto_save_memory);
        assert_eq!(defaults.min_relevance_score, 0.15);
        assert_eq!(defaults.max_tool_iterations, 5);
        assert_eq!(defaults.message_timeout_secs, 45);
        assert!(!defaults.multimodal.allow_remote_fetch);
        assert!(!defaults.query_classification.enabled);
        assert!(defaults.model_routes.is_empty());

        cfg.autonomy.non_cli_natural_language_approval_mode =
            crate::config::NonCliNaturalLanguageApprovalMode::Disabled;
        cfg.autonomy
            .non_cli_natural_language_approval_mode_by_channel
            .insert(
                "telegram".to_string(),
                crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm,
            );
        cfg.autonomy.non_cli_excluded_tools =
            vec!["browser_open".to_string(), "mock_price".to_string()];
        cfg.security.perplexity_filter.enable_perplexity_filter = true;
        cfg.security.perplexity_filter.perplexity_threshold = 12.5;
        cfg.security.outbound_leak_guard.action = crate::config::OutboundLeakGuardAction::Block;
        cfg.security.outbound_leak_guard.sensitivity = 0.92;
        cfg.memory.auto_save = true;
        cfg.memory.min_relevance_score = 0.65;
        cfg.agent.max_tool_iterations = 11;
        cfg.channels_config.message_timeout_secs = 120;
        cfg.multimodal.allow_remote_fetch = true;
        cfg.query_classification.enabled = true;
        cfg.query_classification.rules = vec![crate::config::ClassificationRule {
            hint: "reasoning".to_string(),
            keywords: vec!["analyze".to_string()],
            patterns: vec!["deep".to_string()],
            min_length: None,
            max_length: None,
            priority: 10,
        }];
        cfg.model_routes = vec![crate::config::ModelRouteConfig {
            hint: "reasoning".to_string(),
            provider: "openrouter".to_string(),
            model: "openai/gpt-5.2".to_string(),
            max_tokens: Some(512),
            api_key: None,
            transport: None,
        }];
        cfg.save().await.expect("save updated config");

        maybe_apply_runtime_config_update(runtime_ctx.as_ref())
            .await
            .expect("apply updated config");

        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_natural_language_approval_mode_for_channel("telegram"),
            crate::config::NonCliNaturalLanguageApprovalMode::RequestConfirm
        );
        assert_eq!(
            runtime_ctx
                .approval_manager
                .non_cli_natural_language_approval_mode_for_channel("discord"),
            crate::config::NonCliNaturalLanguageApprovalMode::Disabled
        );
        assert_eq!(
            snapshot_non_cli_excluded_tools(runtime_ctx.as_ref()),
            vec!["browser_open".to_string(), "mock_price".to_string()]
        );
        let perplexity_cfg = runtime_perplexity_filter_snapshot(runtime_ctx.as_ref());
        assert!(perplexity_cfg.enable_perplexity_filter);
        assert_eq!(perplexity_cfg.perplexity_threshold, 12.5);
        let leak_guard_cfg = runtime_outbound_leak_guard_snapshot(runtime_ctx.as_ref());
        assert_eq!(
            leak_guard_cfg.action,
            crate::config::OutboundLeakGuardAction::Block
        );
        assert_eq!(leak_guard_cfg.sensitivity, 0.92);
        let defaults = runtime_defaults_snapshot(runtime_ctx.as_ref());
        assert!(defaults.auto_save_memory);
        assert_eq!(defaults.min_relevance_score, 0.65);
        assert_eq!(defaults.max_tool_iterations, 11);
        assert_eq!(defaults.message_timeout_secs, 120);
        assert!(defaults.multimodal.allow_remote_fetch);
        assert!(defaults.query_classification.enabled);
        assert_eq!(defaults.query_classification.rules.len(), 1);
        assert_eq!(defaults.model_routes.len(), 1);

        let mut store = runtime_config_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.remove(&config_path);
    }

    #[tokio::test]
    async fn process_channel_message_respects_configured_max_tool_iterations_above_default() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(IterativeToolProvider {
                required_tool_iterations: 11,
            }),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 12,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-iter-success".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-iter-success".to_string(),
                content: "Loop until done".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("chat-iter-success:"));
        assert!(sent_messages[0].contains("Completed after 11 tool iterations."));
        assert!(!sent_messages[0].contains("⚠️ Error:"));
    }

    #[tokio::test]
    async fn process_channel_message_reports_configured_max_tool_iterations_limit() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(IterativeToolProvider {
                required_tool_iterations: 20,
            }),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 3,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: mock_price_approved_manager(),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-iter-fail".to_string(),
                sender: "bob".to_string(),
                reply_target: "chat-iter-fail".to_string(),
                content: "Loop forever".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 2,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        let sent_messages = channel_impl.sent_messages.lock().await;
        assert_eq!(sent_messages.len(), 1);
        assert!(sent_messages[0].starts_with("chat-iter-fail:"));
        assert!(sent_messages[0].contains("⚠️ Reached tool-iteration limit (3)"));
        assert!(sent_messages[0].contains("Context and progress were preserved"));
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
            _category: crate::memory::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<crate::memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<crate::memory::MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&crate::memory::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<crate::memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
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
            _category: crate::memory::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<crate::memory::MemoryEntry>> {
            Ok(vec![crate::memory::MemoryEntry {
                id: "entry-1".to_string(),
                key: "memory_key_1".to_string(),
                content: "Age is 45".to_string(),
                category: crate::memory::MemoryCategory::Conversation,
                timestamp: "2026-02-20T00:00:00Z".to_string(),
                session_id: None,
                score: Some(0.9),
            }])
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<crate::memory::MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&crate::memory::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<crate::memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn message_dispatch_processes_messages_in_parallel() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(SlowProvider {
                delay: Duration::from_millis(250),
            }),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<traits::ChannelMessage>(4);
        tx.send(traits::ChannelMessage {
            id: "1".to_string(),
            sender: "alice".to_string(),
            reply_target: "alice".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".to_string(),
            timestamp: 1,
            thread_ts: None,
        })
        .await
        .unwrap();
        tx.send(traits::ChannelMessage {
            id: "2".to_string(),
            sender: "bob".to_string(),
            reply_target: "bob".to_string(),
            content: "world".to_string(),
            channel: "test-channel".to_string(),
            timestamp: 2,
            thread_ts: None,
        })
        .await
        .unwrap();
        drop(tx);

        let started = Instant::now();
        run_message_dispatch_loop(rx, runtime_ctx, 2).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(430),
            "expected parallel dispatch (<430ms), got {:?}",
            elapsed
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

        let provider_impl = Arc::new(DelayedHistoryCaptureProvider {
            delay: Duration::from_millis(250),
            calls: std::sync::Mutex::new(Vec::new()),
        });

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: provider_impl.clone(),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: true,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<traits::ChannelMessage>(8);
        let send_task = tokio::spawn(async move {
            tx.send(traits::ChannelMessage {
                id: "msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "forwarded content".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            tx.send(traits::ChannelMessage {
                id: "msg-2".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "summarize this".to_string(),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, runtime_ctx, 4).await;
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
        assert!(second_call
            .iter()
            .any(|(role, content)| { role == "user" && content.contains("forwarded content") }));
        assert!(second_call
            .iter()
            .any(|(role, content)| { role == "user" && content.contains("summarize this") }));
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
            provider: Arc::new(SlowProvider {
                delay: Duration::from_millis(180),
            }),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: true,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        let (tx, rx) = tokio::sync::mpsc::channel::<traits::ChannelMessage>(8);
        let send_task = tokio::spawn(async move {
            tx.send(traits::ChannelMessage {
                id: "msg-a".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "first chat".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            tx.send(traits::ChannelMessage {
                id: "msg-b".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-2".to_string(),
                content: "second chat".to_string(),
                channel: "telegram".to_string(),
                timestamp: 2,
                thread_ts: None,
            })
            .await
            .unwrap();
        });

        run_message_dispatch_loop(rx, runtime_ctx, 4).await;
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
            provider: Arc::new(SlowProvider {
                delay: Duration::from_millis(20),
            }),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "typing-msg".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-typing".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
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
            provider: Arc::new(SlowProvider {
                delay: Duration::from_millis(5),
            }),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 10,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "react-msg".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-react".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
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
        assert!(
            prompt.contains("## Current Date & Time"),
            "missing Date/Time"
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

        prompt.push_str(&crate::agent::loop_::build_tool_instructions(&[]));

        assert_eq!(
            prompt.matches("## Tool Use Protocol").count(),
            1,
            "protocol block should appear exactly once in the final prompt"
        );
    }

    #[test]
    fn prompt_injects_safety() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(prompt.contains("Do not exfiltrate private data"));
        assert!(prompt.contains("Do not run destructive commands"));
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
        let skills = vec![crate::skills::Skill {
            name: "code-review".into(),
            description: "Review code for bugs".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "lint".into(),
                description: "Run static checks".into(),
                kind: "shell".into(),
                command: "cargo clippy".into(),
                args: HashMap::new(),
            }],
            prompts: vec!["Always run cargo test before final response.".into()],
            location: None,
            always: false,
        }];

        let prompt = build_system_prompt(ws.path(), "model", &[], &skills, None, None);

        assert!(prompt.contains("<available_skills>"), "missing skills XML");
        assert!(prompt.contains("<name>code-review</name>"));
        assert!(prompt.contains("<description>Review code for bugs</description>"));
        assert!(prompt.contains("SKILL.md</location>"));
        assert!(prompt.contains("<instructions>"));
        assert!(prompt
            .contains("<instruction>Always run cargo test before final response.</instruction>"));
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("<name>lint</name>"));
        assert!(prompt.contains("<kind>shell</kind>"));
        assert!(!prompt.contains("loaded on demand"));
    }

    #[test]
    fn prompt_skills_compact_mode_omits_instructions_and_tools() {
        let ws = make_workspace();
        let skills = vec![crate::skills::Skill {
            name: "code-review".into(),
            description: "Review code for bugs".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "lint".into(),
                description: "Run static checks".into(),
                kind: "shell".into(),
                command: "cargo clippy".into(),
                args: HashMap::new(),
            }],
            prompts: vec!["Always run cargo test before final response.".into()],
            location: None,
            always: false,
        }];

        let prompt = build_system_prompt_with_mode(
            ws.path(),
            "model",
            &[],
            &skills,
            None,
            None,
            false,
            crate::config::SkillsPromptInjectionMode::Compact,
        );

        assert!(prompt.contains("<available_skills>"), "missing skills XML");
        assert!(prompt.contains("<name>code-review</name>"));
        assert!(prompt.contains("<location>skills/code-review/SKILL.md</location>"));
        assert!(prompt.contains("loaded on demand"));
        assert!(!prompt.contains("<instructions>"));
        assert!(!prompt
            .contains("<instruction>Always run cargo test before final response.</instruction>"));
        assert!(!prompt.contains("<tools>"));
    }

    #[test]
    fn prompt_skills_escape_reserved_xml_chars() {
        let ws = make_workspace();
        let skills = vec![crate::skills::Skill {
            name: "code<review>&".into(),
            description: "Review \"unsafe\" and 'risky' bits".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "run\"linter\"".into(),
                description: "Run <lint> & report".into(),
                kind: "shell&exec".into(),
                command: "cargo clippy".into(),
                args: HashMap::new(),
            }],
            prompts: vec!["Use <tool_call> and & keep output \"safe\"".into()],
            location: None,
            always: false,
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
        let result = std::panic::catch_unwind(|| crate::util::truncate_with_ellipsis(msg, 80));
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
    fn prompt_workspace_path() {
        let ws = make_workspace();
        let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

        assert!(prompt.contains(&format!("Working directory: `{}`", ws.path().display())));
    }

    #[test]
    fn conversation_memory_key_uses_message_id() {
        let msg = traits::ChannelMessage {
            id: "msg_abc123".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "hello".into(),
            channel: "slack".into(),
            timestamp: 1,
            thread_ts: None,
        };

        assert_eq!(conversation_memory_key(&msg), "slack_U123_msg_abc123");
    }

    #[test]
    fn conversation_memory_key_is_unique_per_message() {
        let msg1 = traits::ChannelMessage {
            id: "msg_1".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "first".into(),
            channel: "slack".into(),
            timestamp: 1,
            thread_ts: None,
        };
        let msg2 = traits::ChannelMessage {
            id: "msg_2".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "second".into(),
            channel: "slack".into(),
            timestamp: 2,
            thread_ts: None,
        };

        assert_ne!(
            conversation_memory_key(&msg1),
            conversation_memory_key(&msg2)
        );
    }

    #[test]
    fn assistant_memory_key_is_namespaced_from_user_key() {
        let msg = traits::ChannelMessage {
            id: "msg_abc123".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "hello".into(),
            channel: "slack".into(),
            timestamp: 1,
            thread_ts: None,
        };

        let user_key = conversation_memory_key(&msg);
        let assistant_key = assistant_memory_key(&msg);

        assert!(assistant_key.starts_with("assistant_resp_"));
        assert!(assistant_key.ends_with(&user_key));
        assert_ne!(assistant_key, user_key);
    }

    #[test]
    fn conversation_history_key_ignores_qq_message_id_thread() {
        let msg1 = traits::ChannelMessage {
            id: "msg_1".into(),
            sender: "user_open_1".into(),
            reply_target: "user:user_open_1".into(),
            content: "first".into(),
            channel: "qq".into(),
            timestamp: 1,
            thread_ts: Some("msg-a".into()),
        };
        let msg2 = traits::ChannelMessage {
            id: "msg_2".into(),
            sender: "user_open_1".into(),
            reply_target: "user:user_open_1".into(),
            content: "second".into(),
            channel: "qq".into(),
            timestamp: 2,
            thread_ts: Some("msg-b".into()),
        };

        assert_eq!(conversation_history_key(&msg1), "qq_user_open_1");
        assert_eq!(
            conversation_history_key(&msg1),
            conversation_history_key(&msg2)
        );
    }

    #[test]
    fn conversation_history_key_ignores_napcat_message_id_thread() {
        let msg1 = traits::ChannelMessage {
            id: "msg_1".into(),
            sender: "user_1001".into(),
            reply_target: "user:1001".into(),
            content: "first".into(),
            channel: "napcat".into(),
            timestamp: 1,
            thread_ts: Some("msg-a".into()),
        };
        let msg2 = traits::ChannelMessage {
            id: "msg_2".into(),
            sender: "user_1001".into(),
            reply_target: "user:1001".into(),
            content: "second".into(),
            channel: "napcat".into(),
            timestamp: 2,
            thread_ts: Some("msg-b".into()),
        };

        assert_eq!(conversation_history_key(&msg1), "napcat_user_1001");
        assert_eq!(
            conversation_history_key(&msg1),
            conversation_history_key(&msg2)
        );
    }

    #[tokio::test]
    async fn autosave_keys_preserve_multiple_conversation_facts() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        let msg1 = traits::ChannelMessage {
            id: "msg_1".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "I'm Paul".into(),
            channel: "slack".into(),
            timestamp: 1,
            thread_ts: None,
        };
        let msg2 = traits::ChannelMessage {
            id: "msg_2".into(),
            sender: "U123".into(),
            reply_target: "C456".into(),
            content: "I'm 45".into(),
            channel: "slack".into(),
            timestamp: 2,
            thread_ts: None,
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

        let recalled = mem.recall("45", 5, None).await.unwrap();
        assert!(recalled.iter().any(|entry| entry.content.contains("45")));
    }

    #[tokio::test]
    async fn build_memory_context_includes_recalled_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store("age_fact", "Age is 45", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        let context = build_memory_context(&mem, "age", 0.0, None).await;
        assert!(context.contains("[Memory context]"));
        assert!(context.contains("Age is 45"));
    }

    #[tokio::test]
    async fn build_memory_context_respects_session_scope() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store(
            "session_a_fact",
            "Session A remembers age 45",
            MemoryCategory::Conversation,
            Some("session-a"),
        )
        .await
        .unwrap();
        mem.store(
            "session_b_fact",
            "Session B remembers age 31",
            MemoryCategory::Conversation,
            Some("session-b"),
        )
        .await
        .unwrap();

        let session_a_context = build_memory_context(&mem, "age", 0.0, Some("session-a")).await;
        assert!(session_a_context.contains("age 45"));
        assert!(!session_a_context.contains("age 31"));
    }

    #[tokio::test]
    async fn process_channel_message_restores_per_sender_history_on_follow_ups() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureProvider::default());

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: provider_impl.clone(),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-a".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-b".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-1".to_string(),
                content: "follow up".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 2,
                thread_ts: None,
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
        assert!(calls[1][1].1.contains("hello"));
        assert!(calls[1][2].1.contains("response-1"));
        assert!(calls[1][3].1.contains("follow up"));
    }

    #[tokio::test]
    async fn process_channel_message_qq_keeps_history_across_distinct_message_ids() {
        let channel_impl = Arc::new(QqRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureProvider::default());

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: provider_impl.clone(),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-a".to_string(),
                sender: "alice".to_string(),
                reply_target: "group:1".to_string(),
                content: "hello".to_string(),
                channel: "qq".to_string(),
                timestamp: 1,
                thread_ts: Some("msg-1".to_string()),
            },
            CancellationToken::new(),
        )
        .await;

        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-b".to_string(),
                sender: "alice".to_string(),
                reply_target: "group:1".to_string(),
                content: "follow up".to_string(),
                channel: "qq".to_string(),
                timestamp: 2,
                thread_ts: Some("msg-2".to_string()),
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
        assert!(calls[1][1].1.contains("hello"));
        assert!(calls[1][2].1.contains("response-1"));
        assert!(calls[1][3].1.contains("follow up"));
    }

    #[tokio::test]
    async fn process_channel_message_enriches_current_turn_without_persisting_context() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureProvider::default());
        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: provider_impl.clone(),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(RecallMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "msg-ctx-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-ctx".to_string(),
                content: "hello".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
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
        assert_eq!(calls[0][1].0, "user");
        assert!(calls[0][1].1.contains("[Memory context]"));
        assert!(calls[0][1].1.contains("Age is 45"));
        assert!(calls[0][1].1.contains("hello"));

        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let turns = histories
            .get("test-channel_alice")
            .expect("history should be stored for sender");
        assert_eq!(turns[0].role, "user");
        assert!(
            turns[0].content.ends_with("hello"),
            "stored user turn should preserve message body, got: {}",
            turns[0].content
        );
        assert!(!turns[0].content.contains("[Memory context]"));
    }

    #[tokio::test]
    async fn process_channel_message_telegram_keeps_system_instruction_at_top_only() {
        let channel_impl = Arc::new(TelegramRecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        let provider_impl = Arc::new(HistoryCaptureProvider::default());
        let mut histories = HashMap::new();
        histories.insert(
            "telegram_alice".to_string(),
            vec![
                ChatMessage::assistant("stale assistant"),
                ChatMessage::user("earlier user question"),
                ChatMessage::assistant("earlier assistant reply"),
            ],
        );

        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: provider_impl.clone(),
            default_provider: Arc::new("test-provider".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("test-system-prompt".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(histories)),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            runtime_ctx.clone(),
            traits::ChannelMessage {
                id: "tg-msg-1".to_string(),
                sender: "alice".to_string(),
                reply_target: "chat-telegram".to_string(),
                content: "hello".to_string(),
                channel: "telegram".to_string(),
                timestamp: 1,
                thread_ts: None,
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
    fn should_expose_internal_tool_details_matches_explicit_requests() {
        assert!(should_expose_internal_tool_details(
            "Please show commands and tool calls you used."
        ));
        assert!(should_expose_internal_tool_details(
            "请输出命令和工具调用过程"
        ));
        assert!(!should_expose_internal_tool_details(
            "帮我直接给最终结论，不要过程。"
        ));
    }

    #[test]
    fn should_expose_internal_tool_details_respects_negative_requests() {
        assert!(!should_expose_internal_tool_details(
            "Please do not show commands or tool calls, only final answer."
        ));
        assert!(!should_expose_internal_tool_details(
            "不要显示命令和工具调用，直接给最终结论。"
        ));
    }

    #[test]
    fn split_internal_progress_delta_detects_sentinel_prefix() {
        let payload = format!(
            "{}⏳ shell: ls -la\n",
            crate::agent::loop_::DRAFT_PROGRESS_SENTINEL
        );
        let (is_internal, visible) = split_internal_progress_delta(&payload);
        assert!(is_internal);
        assert_eq!(visible, "⏳ shell: ls -la\n");

        let (is_internal_plain, plain) = split_internal_progress_delta("final answer");
        assert!(!is_internal_plain);
        assert_eq!(plain, "final answer");
    }

    #[test]
    fn effective_progress_mode_defaults_non_telegram_to_off() {
        assert_eq!(
            effective_progress_mode_for_message("draft-streaming-channel", false),
            ProgressMode::Off
        );
        assert_eq!(
            effective_progress_mode_for_message("draft-streaming-channel", true),
            ProgressMode::Verbose
        );
    }

    #[test]
    fn effective_progress_mode_uses_telegram_runtime_setting() {
        set_runtime_telegram_progress_mode(ProgressMode::Compact);
        assert_eq!(
            effective_progress_mode_for_message("telegram", false),
            ProgressMode::Compact
        );
        set_runtime_telegram_progress_mode(ProgressMode::Off);
        assert_eq!(
            effective_progress_mode_for_message("telegram", false),
            ProgressMode::Off
        );
    }

    #[test]
    fn upsert_progress_section_replaces_existing_block() {
        let mut text = String::new();
        upsert_progress_section(&mut text, "⏳ shell: ls\n");
        upsert_progress_section(&mut text, "✅ shell (1s)\n");
        let stripped = strip_progress_section_markers(&text);
        assert!(!stripped.contains("⏳ shell: ls"));
        assert!(stripped.contains("✅ shell (1s)"));
    }

    #[test]
    fn build_channel_system_prompt_includes_visibility_policy() {
        let hidden = build_channel_system_prompt("base", "telegram", "chat", false);
        assert!(hidden.contains("run tools/functions in the background"));
        assert!(hidden.contains("Do not reveal raw tool names"));

        let exposed = build_channel_system_prompt("base", "telegram", "chat", true);
        assert!(exposed.contains("user explicitly requested command/tool details"));
    }

    #[test]
    fn build_channel_system_prompt_refreshes_datetime_section() {
        let base_prompt =
            "## Current Date & Time\n\n2000-01-01 00:00:00 (UTC)\n\n## Runtime\n\nHost: test";
        let rendered = build_channel_system_prompt(base_prompt, "telegram", "chat", false);
        assert!(!rendered.contains("2000-01-01 00:00:00 (UTC)"));
        assert!(rendered.contains("## Current Date & Time\n\n"));
        assert!(rendered.contains("## Runtime\n\nHost: test"));
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

    #[test]
    fn sanitize_channel_response_removes_tool_call_tags_and_tool_json_artifacts() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];

        let input = r#"Let me check.
<tool_call>
{"name":"debug_trace","arguments":{"foo":"bar"}}
</tool_call>
{"name":"mock_price","parameters":{"symbol":"BTC"}}
{"result":{"symbol":"BTC","price_usd":65000}}
BTC is currently around $65,000 based on latest tool output."#;

        let result = sanitize_channel_response(
            input,
            &tools,
            &crate::config::OutboundLeakGuardConfig::default(),
        );
        let ChannelSanitizationResult::Sanitized(result) = result else {
            panic!("expected sanitized output");
        };
        let normalized = result
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            normalized,
            "Let me check.\nBTC is currently around $65,000 based on latest tool output."
        );
        assert!(!result.contains("<tool_call>"));
        assert!(!result.contains("\"name\":\"mock_price\""));
        assert!(!result.contains("\"result\""));
    }

    #[test]
    fn sanitize_channel_response_redacts_detected_credentials() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let leaked = "Temporary key: AKIAABCDEFGHIJKLMNOP";

        let result = sanitize_channel_response(
            leaked,
            &tools,
            &crate::config::OutboundLeakGuardConfig::default(),
        );
        let ChannelSanitizationResult::Sanitized(result) = result else {
            panic!("expected sanitized output");
        };

        assert!(!result.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(result.contains("[REDACTED_AWS_CREDENTIAL]"));
    }

    #[test]
    fn sanitize_channel_response_skips_leak_scan_when_disabled() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let leaked = "Temporary key: AKIAABCDEFGHIJKLMNOP";
        let leak_guard = crate::config::OutboundLeakGuardConfig {
            enabled: false,
            action: crate::config::OutboundLeakGuardAction::Block,
            sensitivity: 0.7,
        };

        let result = sanitize_channel_response(leaked, &tools, &leak_guard);
        let ChannelSanitizationResult::Sanitized(result) = result else {
            panic!("expected sanitized output");
        };

        assert!(result.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(!result.contains("[REDACTED_AWS_CREDENTIAL]"));
    }

    #[test]
    fn sanitize_channel_response_blocks_detected_credentials_when_configured() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let leaked = "Temporary key: AKIAABCDEFGHIJKLMNOP";
        let leak_guard = crate::config::OutboundLeakGuardConfig {
            enabled: true,
            action: crate::config::OutboundLeakGuardAction::Block,
            sensitivity: 0.7,
        };

        let result = sanitize_channel_response(leaked, &tools, &leak_guard);

        match result {
            ChannelSanitizationResult::Blocked { patterns, redacted } => {
                assert!(!patterns.is_empty());
                assert!(!redacted.contains("AKIAABCDEFGHIJKLMNOP"));
                assert!(redacted.contains("[REDACTED_AWS_CREDENTIAL]"));
            }
            ChannelSanitizationResult::Sanitized(output) => {
                panic!("expected blocked result, got sanitized output: {output}")
            }
        }
    }

    // ── AIEOS Identity Tests (Issue #168) ─────────────────────────

    #[test]
    fn aieos_identity_from_file() {
        use crate::config::IdentityConfig;
        use tempfile::TempDir;

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
            extra_files: Vec::new(),
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
        use crate::config::IdentityConfig;

        let config = IdentityConfig {
            format: "aieos".into(),
            extra_files: Vec::new(),
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
        use crate::config::IdentityConfig;

        let config = IdentityConfig {
            format: "aieos".into(),
            extra_files: Vec::new(),
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
        use crate::config::IdentityConfig;

        // Format is "aieos" but neither path nor inline is set
        let config = IdentityConfig {
            format: "aieos".into(),
            extra_files: Vec::new(),
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
        use crate::config::IdentityConfig;

        let config = IdentityConfig {
            format: "openclaw".into(),
            extra_files: Vec::new(),
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
    fn openclaw_extra_files_are_injected() {
        use crate::config::IdentityConfig;

        let ws = make_workspace();
        std::fs::write(
            ws.path().join("FRAMEWORK.md"),
            "# Framework\nSession-level context.",
        )
        .unwrap();
        std::fs::create_dir_all(ws.path().join("memory")).unwrap();
        std::fs::write(
            ws.path().join("memory").join("notes.md"),
            "# Notes\nSupplemental context.",
        )
        .unwrap();

        let config = IdentityConfig {
            format: "openclaw".into(),
            extra_files: vec!["FRAMEWORK.md".into(), "memory/notes.md".into()],
            aieos_path: None,
            aieos_inline: None,
        };

        let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

        assert!(prompt.contains("### FRAMEWORK.md"));
        assert!(prompt.contains("Session-level context."));
        assert!(prompt.contains("### memory/notes.md"));
        assert!(prompt.contains("Supplemental context."));
    }

    #[test]
    fn openclaw_extra_files_reject_unsafe_paths() {
        use crate::config::IdentityConfig;

        let ws = make_workspace();
        std::fs::write(ws.path().join("SAFE.md"), "safe").unwrap();

        let config = IdentityConfig {
            format: "openclaw".into(),
            extra_files: vec![
                "SAFE.md".into(),
                "../outside.md".into(),
                "/tmp/absolute.md".into(),
            ],
            aieos_path: None,
            aieos_inline: None,
        };

        let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

        assert!(prompt.contains("### SAFE.md"));
        assert!(!prompt.contains("outside.md"));
        assert!(!prompt.contains("absolute.md"));
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
        let result =
            tokio::time::timeout(Duration::from_millis(1), std::future::pending::<bool>()).await;
        let state = classify_health_result(&result);
        assert_eq!(state, ChannelHealthState::Timeout);
    }

    #[test]
    fn collect_configured_channels_includes_mattermost_when_configured() {
        let mut config = Config::default();
        config.channels_config.mattermost = Some(crate::config::schema::MattermostConfig {
            url: "https://mattermost.example.com".to_string(),
            bot_token: "test-token".to_string(),
            channel_id: Some("channel-1".to_string()),
            allowed_users: vec![],
            thread_replies: Some(true),
            mention_only: Some(false),
            group_reply: None,
        });

        let channels = collect_configured_channels(&config, "test");

        assert!(channels
            .iter()
            .any(|entry| entry.display_name == "Mattermost"));
        assert!(channels
            .iter()
            .any(|entry| entry.channel.name() == "mattermost"));
    }

    #[test]
    fn collect_configured_channels_includes_dingtalk_when_configured() {
        let mut config = Config::default();
        config.channels_config.dingtalk = Some(crate::config::schema::DingTalkConfig {
            client_id: "ding-app-key".to_string(),
            client_secret: "ding-app-secret".to_string(),
            allowed_users: vec!["*".to_string()],
        });

        let channels = collect_configured_channels(&config, "test");

        assert!(channels
            .iter()
            .any(|entry| entry.display_name == "DingTalk"));
        assert!(channels
            .iter()
            .any(|entry| entry.channel.name() == "dingtalk"));
    }

    struct AlwaysFailChannel {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    struct BlockUntilClosedChannel {
        name: String,
        calls: Arc<AtomicUsize>,
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
            _tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("listen boom")
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
            tx: tokio::sync::mpsc::Sender<traits::ChannelMessage>,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tx.closed().await;
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

        let (tx, rx) = tokio::sync::mpsc::channel::<traits::ChannelMessage>(1);
        let handle = spawn_supervised_listener(channel, tx, 1, 1);

        tokio::time::sleep(Duration::from_millis(80)).await;
        drop(rx);
        handle.abort();
        let _ = handle.await;

        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["channel:test-supervised-fail"];
        assert_eq!(component["status"], "error");
        assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
        assert!(component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("listen boom"));
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

        let (tx, rx) = tokio::sync::mpsc::channel::<traits::ChannelMessage>(1);
        let handle = spawn_supervised_listener_with_health_interval(
            channel,
            tx,
            1,
            1,
            Duration::from_millis(20),
        );

        tokio::time::sleep(Duration::from_millis(35)).await;
        let first_last_ok = crate::health::snapshot_json()["components"][&component_name]
            ["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(!first_last_ok.is_empty());

        tokio::time::sleep(Duration::from_millis(70)).await;
        let second_last_ok = crate::health::snapshot_json()["components"][&component_name]
            ["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let first = chrono::DateTime::parse_from_rfc3339(&first_last_ok)
            .expect("last_ok should be valid RFC3339");
        let second = chrono::DateTime::parse_from_rfc3339(&second_last_ok)
            .expect("last_ok should be valid RFC3339");
        assert!(second > first, "expected periodic health heartbeat refresh");

        drop(rx);
        let join = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(join.is_ok(), "listener should stop after channel shutdown");
        assert!(calls.load(Ordering::SeqCst) >= 1);
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

    // ── E2E: photo [IMAGE:] marker rejected by non-vision provider ───

    /// End-to-end test: a photo attachment message (containing `[IMAGE:]`
    /// marker) sent through `process_channel_message` with a non-vision
    /// provider must produce a `"⚠️ Error: …does not support vision"` reply
    /// on the recording channel — no real Telegram or LLM API required.
    #[tokio::test]
    async fn e2e_photo_attachment_rejected_by_non_vision_provider() {
        let channel_impl = Arc::new(RecordingChannel::default());
        let channel: Arc<dyn Channel> = channel_impl.clone();

        let mut channels_by_name = HashMap::new();
        channels_by_name.insert(channel.name().to_string(), channel);

        // DummyProvider has default capabilities (vision: false).
        let runtime_ctx = Arc::new(ChannelRuntimeContext {
            channels_by_name: Arc::new(channels_by_name),
            provider: Arc::new(DummyProvider),
            default_provider: Arc::new("dummy".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("You are a helpful assistant.".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        // Simulate a photo attachment message with [IMAGE:] marker.
        process_channel_message(
            runtime_ctx,
            traits::ChannelMessage {
                id: "msg-photo-1".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-photo".to_string(),
                content: "[IMAGE:/tmp/workspace/photo_99_1.jpg]\n\nWhat is this?".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
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
            provider: Arc::new(DummyProvider),
            default_provider: Arc::new("dummy".to_string()),
            memory: Arc::new(NoopMemory),
            tools_registry: Arc::new(vec![]),
            observer: Arc::new(NoopObserver),
            system_prompt: Arc::new("You are a helpful assistant.".to_string()),
            model: Arc::new("test-model".to_string()),
            temperature: 0.0,
            auto_save_memory: false,
            max_tool_iterations: 5,
            min_relevance_score: 0.0,
            conversation_histories: Arc::new(Mutex::new(HashMap::new())),
            conversation_locks: Default::default(),
            session_config: crate::config::AgentSessionConfig::default(),
            session_manager: None,
            provider_cache: Arc::new(Mutex::new(HashMap::new())),
            route_overrides: Arc::new(Mutex::new(HashMap::new())),
            api_key: None,
            api_url: None,
            reliability: Arc::new(crate::config::ReliabilityConfig::default()),
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            workspace_dir: Arc::new(std::env::temp_dir()),
            message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
            interrupt_on_new_message: false,
            multimodal: crate::config::MultimodalConfig::default(),
            hooks: None,
            non_cli_excluded_tools: Arc::new(Mutex::new(Vec::new())),
            query_classification: crate::config::QueryClassificationConfig::default(),
            model_routes: Vec::new(),
            approval_manager: Arc::new(ApprovalManager::from_config(
                &crate::config::AutonomyConfig::default(),
            )),
            safety_heartbeat: None,
            startup_perplexity_filter: crate::config::PerplexityFilterConfig::default(),
        });

        process_channel_message(
            Arc::clone(&runtime_ctx),
            traits::ChannelMessage {
                id: "msg-photo-1".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-photo".to_string(),
                content: "[IMAGE:/tmp/workspace/photo_99_1.jpg]\n\nWhat is this?".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 1,
                thread_ts: None,
            },
            CancellationToken::new(),
        )
        .await;

        process_channel_message(
            Arc::clone(&runtime_ctx),
            traits::ChannelMessage {
                id: "msg-text-2".to_string(),
                sender: "zeroclaw_user".to_string(),
                reply_target: "chat-photo".to_string(),
                content: "What is WAL?".to_string(),
                channel: "test-channel".to_string(),
                timestamp: 2,
                thread_ts: None,
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
            .get("test-channel_zeroclaw_user")
            .expect("history should exist for sender");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert!(
            turns[0].content.ends_with("What is WAL?"),
            "stored user turn should preserve text-only message body, got: {}",
            turns[0].content
        );
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].content, "ok");
        assert!(
            turns.iter().all(|turn| !turn.content.contains("[IMAGE:")),
            "failed vision turn must not persist image marker content"
        );
    }
}
