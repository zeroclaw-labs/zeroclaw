use crate::approval::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use crate::config::schema::{CostEnforcementMode, ModelPricing};
use crate::config::{Config, ProgressMode};
use crate::cost::{BudgetCheck, CostTracker, UsagePeriod};
use crate::memory::{self, Memory, MemoryCategory};
use crate::multimodal;
use crate::observability::{self, runtime_trace, Observer, ObserverEvent};
use crate::providers::{
    self, ChatMessage, ChatRequest, NormalizedStopReason, Provider, ProviderCapabilityError,
    ToolCall,
};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use regex::{Regex, RegexSet};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config as RlConfig, Context, Editor, Helper};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::future::Future;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod context;
pub(crate) mod detection;
mod execution;
mod history;
mod parsing;

use context::{build_context, build_hardware_context};
use detection::{DetectionVerdict, LoopDetectionConfig, LoopDetector};
use execution::{
    execute_tools_parallel, execute_tools_sequential, should_execute_tools_in_parallel,
    ToolExecutionOutcome,
};
#[cfg(test)]
use history::{apply_compaction_summary, build_compaction_transcript};
use history::{auto_compact_history, trim_history};
#[allow(unused_imports)]
use parsing::{
    default_param_for_tool, detect_tool_call_parse_issue, extract_json_values, map_tool_name_alias,
    parse_arguments_value, parse_glm_shortened_body, parse_glm_style_tool_calls,
    parse_perl_style_tool_calls, parse_structured_tool_calls, parse_tool_call_value,
    parse_tool_calls, parse_tool_calls_from_json_value, tool_call_signature, ParsedToolCall,
};

/// Minimum characters per chunk when relaying LLM text to a streaming draft.
const STREAM_CHUNK_MIN_CHARS: usize = 80;

/// Default maximum agentic tool-use iterations per user message to prevent runaway loops.
/// Used as a safe fallback when `max_tool_iterations` is unset or configured as zero.
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 20;

/// Maximum continuation retries when a provider reports max-token truncation.
const MAX_TOKENS_CONTINUATION_MAX_ATTEMPTS: usize = 3;
/// Absolute safety cap for merged continuation output.
const MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS: usize = 120_000;
/// Deterministic continuation instruction appended as a user message.
const MAX_TOKENS_CONTINUATION_PROMPT: &str = "Previous response was truncated by token limit.\nContinue exactly from where you left off.\nIf you intended a tool call, emit one complete tool call payload only.\nDo not repeat already-sent text.";
/// Notice appended when continuation budget is exhausted before completion.
const MAX_TOKENS_CONTINUATION_NOTICE: &str =
    "\n\n[Response may be truncated due to continuation limits. Reply \"continue\" to resume.]";

/// Minimum user-message length (in chars) for auto-save to memory.
/// Matches the channel-side constant in `channels/mod.rs`.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

fn should_treat_provider_as_vision_capable(provider_name: &str, provider: &dyn Provider) -> bool {
    if provider.supports_vision() {
        return true;
    }

    // Guardrail for issue #2107: some anthropic setups have reported false
    // negatives from provider capability probing even though Claude models
    // accept image inputs. Keep the preflight permissive for anthropic routes
    // and rely on upstream API validation if a specific model cannot handle
    // vision.
    let normalized = provider_name.trim().to_ascii_lowercase();
    normalized == "anthropic" || normalized.starts_with("anthropic-custom:")
}

/// Slash-command definitions for interactive-mode completion.
/// Each entry: (trigger aliases, display label, description).
const SLASH_COMMANDS: &[(&[&str], &str, &str)] = &[
    (&["/help"], "/help", "Show this help message"),
    (
        &["/clear", "/new"],
        "/clear /new",
        "Clear conversation history",
    ),
    (&["/quit", "/exit"], "/quit /exit", "Exit interactive mode"),
];

struct SlashCommandCompleter;

impl Completer for SlashCommandCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if !line.starts_with('/') {
            return Ok((0, vec![]));
        }

        let prefix = &line[..pos];
        let matches: Vec<Pair> = SLASH_COMMANDS
            .iter()
            .filter(|(triggers, _, _)| triggers.iter().any(|trigger| trigger.starts_with(prefix)))
            .map(|(triggers, label, desc)| {
                let replacement = triggers
                    .iter()
                    .find(|trigger| trigger.starts_with(prefix))
                    .unwrap_or(&triggers[0]);
                Pair {
                    display: format!("  {:<14}{desc:<64}", label),
                    replacement: replacement.to_string(),
                }
            })
            .collect();

        Ok((0, matches))
    }
}

impl Hinter for SlashCommandCompleter {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        if !line.starts_with('/') || pos == 0 {
            return None;
        }

        let prefix = &line[..pos];
        SLASH_COMMANDS
            .iter()
            .flat_map(|(triggers, _, _)| triggers.iter())
            .find(|trigger| trigger.starts_with(prefix) && **trigger != prefix)
            .map(|trigger| trigger[pos..].to_string())
    }
}

impl Highlighter for SlashCommandCompleter {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[90m{hint}\x1b[0m"))
    }
}

impl Validator for SlashCommandCompleter {}
impl Helper for SlashCommandCompleter {}

static SENSITIVE_KEY_PATTERNS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)token",
        r"(?i)api[_-]?key",
        r"(?i)password",
        r"(?i)secret",
        r"(?i)user[_-]?key",
        r"(?i)bearer",
        r"(?i)credential",
    ])
    .unwrap()
});

static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(token|api[_-]?key|password|secret|user[_-]?key|bearer|credential)["']?\s*[:=]\s*(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\.]{8,}))"#).unwrap()
});

/// Detect "I'll do X" style deferred-action replies that often indicate a missing
/// follow-up tool call in agentic flows.
static DEFERRED_ACTION_WITHOUT_TOOL_CALL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        \b(
            i(?:'ll|\s+will)|
            i\s+am\s+going\s+to|
            let\s+me|
            let(?:'s|\s+us)|
            we(?:'ll|\s+will)
        )\b
        [^.!?\n]{0,160}
        \b(
            check|look|search|browse|open|read|write|run|execute|call|
            inspect|analy(?:s|z)e|verify|list|fetch|try|see|continue
        )\b",
    )
    .unwrap()
});

/// Detect common CJK deferred-action phrases (e.g., Chinese "让我…查看")
/// that imply a follow-up tool call should occur.
static CJK_DEFERRED_ACTION_CUE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(让我|我来|我会|我们来|我们会|我先|先让我|马上)").unwrap());

/// Action verbs commonly used when promising to perform tool-backed work in CJK text.
static CJK_DEFERRED_ACTION_VERB_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(查看|检查|搜索|查找|浏览|打开|读取|写入|运行|执行|调用|分析|验证|列出|获取|尝试|试试|继续|处理|修复|看看|看一看|看一下)").unwrap()
});

/// Fast check for CJK scripts (Han/Hiragana/Katakana/Hangul) so we only run
/// additional regexes when non-Latin text is present.
static CJK_SCRIPT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]").unwrap()
});

/// Scrub credentials from tool output to prevent accidental exfiltration.
/// Replaces known credential patterns with a redacted placeholder while preserving
/// a small prefix for context.
pub(crate) fn scrub_credentials(input: &str) -> String {
    SENSITIVE_KV_REGEX
        .replace_all(input, |caps: &regex::Captures| {
            let full_match = &caps[0];
            let key = &caps[1];
            let val = caps
                .get(2)
                .or(caps.get(3))
                .or(caps.get(4))
                .map(|m| m.as_str())
                .unwrap_or("");

            // Preserve first 4 chars for context, then redact
            let prefix = if val.len() > 4 { &val[..4] } else { "" };

            if full_match.contains(':') {
                if full_match.contains('"') {
                    format!("\"{}\": \"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}: {}*[REDACTED]", key, prefix)
                }
            } else if full_match.contains('=') {
                if full_match.contains('"') {
                    format!("{}=\"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}={}*[REDACTED]", key, prefix)
                }
            } else {
                format!("{}: {}*[REDACTED]", key, prefix)
            }
        })
        .to_string()
}

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
/// Prefer passing the config-driven value via `run_tool_call_loop`; this constant is only
/// used when callers omit the parameter.
const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

/// Minimum interval between progress sends to avoid flooding the draft channel.
pub(crate) const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Sentinel value sent through on_delta to signal the draft updater to clear accumulated text.
/// Used before streaming the final answer so progress lines are replaced by the clean response.
pub(crate) const DRAFT_CLEAR_SENTINEL: &str = "\x00CLEAR\x00";
/// Sentinel prefix for internal progress deltas (thinking/tool execution trace).
/// Channel layers can suppress these messages by default and only expose them
/// when the user explicitly asks for command/tool execution details.
pub(crate) const DRAFT_PROGRESS_SENTINEL: &str = "\x00PROGRESS\x00";
/// Sentinel prefix for full in-place progress blocks.
pub(crate) const DRAFT_PROGRESS_BLOCK_SENTINEL: &str = "\x00PROGRESS_BLOCK\x00";
/// Progress-section marker inserted into accumulated streaming drafts.
pub(crate) const DRAFT_PROGRESS_SECTION_START: &str = "\n<!-- progress-start -->\n";
/// Progress-section marker inserted into accumulated streaming drafts.
pub(crate) const DRAFT_PROGRESS_SECTION_END: &str = "\n<!-- progress-end -->\n";

tokio::task_local! {
    static TOOL_LOOP_REPLY_TARGET: Option<String>;
}

const AUTO_CRON_DELIVERY_CHANNELS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "lark",
    "feishu",
];

const NON_CLI_APPROVAL_WAIT_TIMEOUT_SECS: u64 = 300;
const NON_CLI_APPROVAL_POLL_INTERVAL_MS: u64 = 250;
const MISSING_TOOL_CALL_RETRY_PROMPT: &str = "Internal correction: your last reply indicated you were about to take an action, but no valid tool call was emitted. If a tool is needed, emit it now using the required <tool_call>...</tool_call> format. If no tool is needed, provide the complete final answer now and do not defer action.";

#[derive(Debug, Clone)]
pub(crate) struct NonCliApprovalPrompt {
    pub request_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct NonCliApprovalContext {
    pub sender: String,
    pub reply_target: String,
    pub prompt_tx: tokio::sync::mpsc::UnboundedSender<NonCliApprovalPrompt>,
}

tokio::task_local! {
    static TOOL_LOOP_NON_CLI_APPROVAL_CONTEXT: Option<NonCliApprovalContext>;
    static LOOP_DETECTION_CONFIG: LoopDetectionConfig;
    static SAFETY_HEARTBEAT_CONFIG: Option<SafetyHeartbeatConfig>;
    static TOOL_LOOP_PROGRESS_MODE: ProgressMode;
    static TOOL_LOOP_COST_ENFORCEMENT_CONTEXT: Option<CostEnforcementContext>;
}

/// Configuration for periodic safety-constraint re-injection (heartbeat).
#[derive(Clone)]
pub(crate) struct SafetyHeartbeatConfig {
    /// Pre-rendered security policy summary text.
    pub body: String,
    /// Inject a heartbeat every `interval` tool iterations (0 = disabled).
    pub interval: usize,
}

#[derive(Clone)]
pub(crate) struct CostEnforcementContext {
    tracker: Arc<CostTracker>,
    prices: HashMap<String, ModelPricing>,
    mode: CostEnforcementMode,
    route_down_model: Option<String>,
    reserve_percent: u8,
}

pub(crate) fn create_cost_enforcement_context(
    cost_config: &crate::config::CostConfig,
    workspace_dir: &Path,
) -> Option<CostEnforcementContext> {
    if !cost_config.enabled {
        return None;
    }
    let tracker = match CostTracker::new(cost_config.clone(), workspace_dir) {
        Ok(tracker) => Arc::new(tracker),
        Err(error) => {
            tracing::warn!("Cost budget preflight disabled: failed to initialize tracker: {error}");
            return None;
        }
    };
    let route_down_model = cost_config
        .enforcement
        .route_down_model
        .clone()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Some(CostEnforcementContext {
        tracker,
        prices: cost_config.prices.clone(),
        mode: cost_config.enforcement.mode,
        route_down_model,
        reserve_percent: cost_config.enforcement.reserve_percent.min(100),
    })
}

pub(crate) async fn scope_cost_enforcement_context<F>(
    context: Option<CostEnforcementContext>,
    future: F,
) -> F::Output
where
    F: Future,
{
    TOOL_LOOP_COST_ENFORCEMENT_CONTEXT
        .scope(context, future)
        .await
}

fn should_inject_safety_heartbeat(counter: usize, interval: usize) -> bool {
    interval > 0 && counter > 0 && counter % interval == 0
}

fn should_emit_verbose_progress(mode: ProgressMode) -> bool {
    mode == ProgressMode::Verbose
}

fn should_emit_tool_progress(mode: ProgressMode) -> bool {
    mode != ProgressMode::Off
}

fn estimate_prompt_tokens(
    messages: &[ChatMessage],
    tools: Option<&[crate::tools::ToolSpec]>,
) -> u64 {
    let message_chars: usize = messages
        .iter()
        .map(|msg| {
            msg.role
                .len()
                .saturating_add(msg.content.chars().count())
                .saturating_add(16)
        })
        .sum();
    let tool_chars: usize = tools
        .map(|specs| {
            specs
                .iter()
                .map(|spec| serde_json::to_string(spec).map_or(0, |value| value.chars().count()))
                .sum()
        })
        .unwrap_or(0);
    let total_chars = message_chars.saturating_add(tool_chars);
    let char_estimate = (total_chars as f64 / 4.0).ceil() as u64;
    let framing_overhead = (messages.len() as u64).saturating_mul(6).saturating_add(64);
    char_estimate.saturating_add(framing_overhead)
}

fn lookup_model_pricing(
    prices: &HashMap<String, ModelPricing>,
    provider: &str,
    model: &str,
) -> (f64, f64) {
    let full_name = format!("{provider}/{model}");
    if let Some(pricing) = prices.get(&full_name) {
        return (pricing.input, pricing.output);
    }
    if let Some(pricing) = prices.get(model) {
        return (pricing.input, pricing.output);
    }
    for (key, pricing) in prices {
        let key_model = key.split('/').next_back().unwrap_or(key);
        if model.starts_with(key_model) || key_model.starts_with(model) {
            return (pricing.input, pricing.output);
        }
        let normalized_model = model.replace('-', ".");
        let normalized_key = key_model.replace('-', ".");
        if normalized_model.contains(&normalized_key) || normalized_key.contains(&normalized_model)
        {
            return (pricing.input, pricing.output);
        }
    }
    (3.0, 15.0)
}

fn estimate_request_cost_usd(
    context: &CostEnforcementContext,
    provider: &str,
    model: &str,
    messages: &[ChatMessage],
    tools: Option<&[crate::tools::ToolSpec]>,
) -> f64 {
    let reserve_multiplier = 1.0 + (f64::from(context.reserve_percent) / 100.0);
    let input_tokens = estimate_prompt_tokens(messages, tools);
    let output_tokens = (input_tokens / 4).max(256);
    let input_tokens = ((input_tokens as f64) * reserve_multiplier).ceil() as u64;
    let output_tokens = ((output_tokens as f64) * reserve_multiplier).ceil() as u64;

    let (input_price, output_price) = lookup_model_pricing(&context.prices, provider, model);
    let input_cost = (input_tokens as f64 / 1_000_000.0) * input_price.max(0.0);
    let output_cost = (output_tokens as f64 / 1_000_000.0) * output_price.max(0.0);
    input_cost + output_cost
}

fn usage_period_label(period: UsagePeriod) -> &'static str {
    match period {
        UsagePeriod::Session => "session",
        UsagePeriod::Day => "daily",
        UsagePeriod::Month => "monthly",
    }
}

fn budget_exceeded_message(
    model: &str,
    estimated_cost_usd: f64,
    current_usd: f64,
    limit_usd: f64,
    period: UsagePeriod,
) -> String {
    format!(
        "Budget enforcement blocked request for model '{model}': projected cost (+${estimated_cost_usd:.4}) exceeds {period_label} limit (${limit_usd:.2}, current ${current_usd:.2}).",
        period_label = usage_period_label(period)
    )
}

#[derive(Debug, Clone)]
struct ProgressEntry {
    name: String,
    hint: String,
    completion: Option<(bool, u64)>,
}

#[derive(Debug, Default)]
struct ProgressTracker {
    entries: Vec<ProgressEntry>,
}

impl ProgressTracker {
    fn add(&mut self, tool_name: &str, hint: &str) -> usize {
        let idx = self.entries.len();
        self.entries.push(ProgressEntry {
            name: tool_name.to_string(),
            hint: hint.to_string(),
            completion: None,
        });
        idx
    }

    fn complete(&mut self, idx: usize, success: bool, secs: u64) {
        if let Some(entry) = self.entries.get_mut(idx) {
            entry.completion = Some((success, secs));
        }
    }

    fn render_delta(&self) -> String {
        let mut out = String::from(DRAFT_PROGRESS_BLOCK_SENTINEL);
        for entry in &self.entries {
            match entry.completion {
                None => {
                    let _ = write!(out, "\u{23f3} {}", entry.name);
                    if !entry.hint.is_empty() {
                        let _ = write!(out, ": {}", entry.hint);
                    }
                    out.push('\n');
                }
                Some((true, secs)) => {
                    let _ = writeln!(out, "\u{2705} {} ({secs}s)", entry.name);
                }
                Some((false, secs)) => {
                    let _ = writeln!(out, "\u{274c} {} ({secs}s)", entry.name);
                }
            }
        }
        out
    }
}
/// Extract a short hint from tool call arguments for progress display.
fn truncate_tool_args_for_progress(name: &str, args: &serde_json::Value, max_len: usize) -> String {
    let hint = match name {
        "shell" => args.get("command").and_then(|v| v.as_str()),
        "file_read" | "file_write" => args.get("path").and_then(|v| v.as_str()),
        "composio_execute" => args.get("action_name").and_then(|v| v.as_str()),
        "memory_recall" => args.get("query").and_then(|v| v.as_str()),
        "memory_store" => args.get("key").and_then(|v| v.as_str()),
        "web_search" => args.get("query").and_then(|v| v.as_str()),
        "http_request" => args.get("url").and_then(|v| v.as_str()),
        "browser_navigate" | "browser_screenshot" | "browser_click" | "browser_type" => {
            args.get("url").and_then(|v| v.as_str())
        }
        _ => args
            .get("action")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("query").and_then(|v| v.as_str())),
    };
    match hint {
        Some(s) => truncate_with_ellipsis(s, max_len),
        None => String::new(),
    }
}

fn looks_like_deferred_action_without_tool_call(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    if DEFERRED_ACTION_WITHOUT_TOOL_CALL_REGEX.is_match(trimmed) {
        return true;
    }

    CJK_SCRIPT_REGEX.is_match(trimmed)
        && CJK_DEFERRED_ACTION_CUE_REGEX.is_match(trimmed)
        && CJK_DEFERRED_ACTION_VERB_REGEX.is_match(trimmed)
}

fn merge_continuation_text(existing: &str, next: &str) -> String {
    if next.is_empty() {
        return existing.to_string();
    }
    if existing.is_empty() {
        return next.to_string();
    }
    if existing.ends_with(next) {
        return existing.to_string();
    }
    if next.starts_with(existing) {
        return next.to_string();
    }

    let mut prefix_ends: Vec<usize> = next.char_indices().map(|(idx, _)| idx).collect();
    prefix_ends.push(next.len());
    for prefix_end in prefix_ends.into_iter().rev() {
        if prefix_end == 0 || prefix_end > existing.len() {
            continue;
        }
        if existing.ends_with(&next[..prefix_end]) {
            return format!("{existing}{}", &next[prefix_end..]);
        }
    }

    format!("{existing}{next}")
}

fn add_optional_u64(lhs: Option<u64>, rhs: Option<u64>) -> Option<u64> {
    match (lhs, rhs) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn stop_reason_name(reason: &NormalizedStopReason) -> &'static str {
    match reason {
        NormalizedStopReason::EndTurn => "end_turn",
        NormalizedStopReason::ToolCall => "tool_call",
        NormalizedStopReason::MaxTokens => "max_tokens",
        NormalizedStopReason::ContextWindowExceeded => "context_window_exceeded",
        NormalizedStopReason::SafetyBlocked => "safety_blocked",
        NormalizedStopReason::Cancelled => "cancelled",
        NormalizedStopReason::Unknown(_) => "unknown",
    }
}

fn maybe_inject_cron_add_delivery(
    tool_name: &str,
    tool_args: &mut serde_json::Value,
    channel_name: &str,
    reply_target: Option<&str>,
) {
    if tool_name != "cron_add"
        || !AUTO_CRON_DELIVERY_CHANNELS
            .iter()
            .any(|supported| supported == &channel_name)
    {
        return;
    }

    let Some(reply_target) = reply_target.map(str::trim).filter(|v| !v.is_empty()) else {
        return;
    };

    let Some(args_obj) = tool_args.as_object_mut() else {
        return;
    };

    let is_agent_job = match args_obj.get("job_type").and_then(serde_json::Value::as_str) {
        Some("agent") => true,
        Some(_) => false,
        None => args_obj.contains_key("prompt"),
    };
    if !is_agent_job {
        return;
    }

    let delivery = args_obj
        .entry("delivery".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(delivery_obj) = delivery.as_object_mut() else {
        return;
    };

    let mode = delivery_obj
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    if mode.eq_ignore_ascii_case("none") || mode.trim().is_empty() {
        delivery_obj.insert(
            "mode".to_string(),
            serde_json::Value::String("announce".to_string()),
        );
    } else if !mode.eq_ignore_ascii_case("announce") {
        // Respect explicitly chosen non-announce modes.
        return;
    }

    let needs_channel = delivery_obj
        .get("channel")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| value.trim().is_empty());
    if needs_channel {
        delivery_obj.insert(
            "channel".to_string(),
            serde_json::Value::String(channel_name.to_string()),
        );
    }

    let needs_target = delivery_obj
        .get("to")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| value.trim().is_empty());
    if needs_target {
        delivery_obj.insert(
            "to".to_string(),
            serde_json::Value::String(reply_target.to_string()),
        );
    }
}

async fn await_non_cli_approval_decision(
    mgr: &ApprovalManager,
    request_id: &str,
    sender: &str,
    channel_name: &str,
    reply_target: &str,
    cancellation_token: Option<&CancellationToken>,
) -> ApprovalResponse {
    let started = Instant::now();

    loop {
        if let Some(decision) = mgr.take_non_cli_pending_resolution(request_id) {
            return decision;
        }

        if !mgr.has_non_cli_pending_request(request_id) {
            // Fail closed when the request disappears without an explicit resolution.
            return ApprovalResponse::No;
        }

        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            return ApprovalResponse::No;
        }

        if started.elapsed() >= Duration::from_secs(NON_CLI_APPROVAL_WAIT_TIMEOUT_SECS) {
            let _ =
                mgr.reject_non_cli_pending_request(request_id, sender, channel_name, reply_target);
            let _ = mgr.take_non_cli_pending_resolution(request_id);
            return ApprovalResponse::No;
        }

        tokio::time::sleep(Duration::from_millis(NON_CLI_APPROVAL_POLL_INTERVAL_MS)).await;
    }
}

/// Convert a tool registry to OpenAI function-calling format for native tool support.
fn tools_to_openai_format(tools_registry: &[Box<dyn Tool>]) -> Vec<serde_json::Value> {
    tools_registry
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters_schema()
                }
            })
        })
        .collect()
}

fn autosave_memory_key(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

/// Build assistant history entry in JSON format for native tool-call APIs.
/// `convert_messages` in the OpenRouter provider parses this JSON to reconstruct
/// the proper `NativeMessage` with structured `tool_calls`.
fn build_native_assistant_history(
    text: &str,
    tool_calls: &[ToolCall],
    reasoning_content: Option<&str>,
) -> String {
    let calls_json: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "arguments": tc.arguments,
            })
        })
        .collect();

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    obj.to_string()
}

fn build_native_assistant_history_from_parsed_calls(
    text: &str,
    tool_calls: &[ParsedToolCall],
    reasoning_content: Option<&str>,
) -> Option<String> {
    let calls_json = tool_calls
        .iter()
        .map(|tc| {
            Some(serde_json::json!({
                "id": tc.tool_call_id.clone()?,
                "name": tc.name,
                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
            }))
        })
        .collect::<Option<Vec<_>>>()?;

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    Some(obj.to_string())
}

fn build_assistant_history_with_tool_calls(text: &str, tool_calls: &[ToolCall]) -> String {
    let mut parts = Vec::new();

    if !text.trim().is_empty() {
        parts.push(text.trim().to_string());
    }

    for call in tool_calls {
        let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .unwrap_or_else(|_| serde_json::Value::String(call.arguments.clone()));
        let payload = serde_json::json!({
            "id": call.id,
            "name": call.name,
            "arguments": arguments,
        });
        parts.push(format!("<tool_call>\n{payload}\n</tool_call>"));
    }

    parts.join("\n")
}

#[derive(Debug)]
pub(crate) struct ToolLoopCancelled;

impl std::fmt::Display for ToolLoopCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for ToolLoopCancelled {}

pub(crate) fn is_tool_loop_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.is::<ToolLoopCancelled>())
}

pub(crate) fn is_tool_iteration_limit_error(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source
            .to_string()
            .contains("Agent exceeded maximum tool iterations")
    })
}

pub(crate) fn is_loop_detection_error(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source
            .to_string()
            .contains("Agent stopped early due to detected loop pattern")
    })
}

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
/// When `silent` is true, suppresses stdout (for channel use).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn agent_turn(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
) -> Result<String> {
    run_tool_call_loop(
        provider,
        history,
        tools_registry,
        observer,
        provider_name,
        model,
        temperature,
        silent,
        None,
        "channel",
        multimodal_config,
        max_tool_iterations,
        None,
        None,
        None,
        &[],
    )
    .await
}

/// Run the tool loop with channel reply_target context, used by channel runtimes
/// to auto-populate delivery routing for scheduled reminders.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_call_loop_with_reply_target(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    reply_target: Option<&str>,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    progress_mode: ProgressMode,
) -> Result<String> {
    TOOL_LOOP_PROGRESS_MODE
        .scope(
            progress_mode,
            TOOL_LOOP_REPLY_TARGET.scope(
                reply_target.map(str::to_string),
                run_tool_call_loop(
                    provider,
                    history,
                    tools_registry,
                    observer,
                    provider_name,
                    model,
                    temperature,
                    silent,
                    approval,
                    channel_name,
                    multimodal_config,
                    max_tool_iterations,
                    cancellation_token,
                    on_delta,
                    hooks,
                    excluded_tools,
                ),
            ),
        )
        .await
}

/// Run the tool loop with optional non-CLI approval context scoped to this task.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_call_loop_with_non_cli_approval_context(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    non_cli_approval_context: Option<NonCliApprovalContext>,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    progress_mode: ProgressMode,
    safety_heartbeat: Option<SafetyHeartbeatConfig>,
) -> Result<String> {
    let reply_target = non_cli_approval_context
        .as_ref()
        .map(|ctx| ctx.reply_target.clone());

    TOOL_LOOP_PROGRESS_MODE
        .scope(
            progress_mode,
            SAFETY_HEARTBEAT_CONFIG.scope(
                safety_heartbeat,
                TOOL_LOOP_NON_CLI_APPROVAL_CONTEXT.scope(
                    non_cli_approval_context,
                    TOOL_LOOP_REPLY_TARGET.scope(
                        reply_target,
                        run_tool_call_loop(
                            provider,
                            history,
                            tools_registry,
                            observer,
                            provider_name,
                            model,
                            temperature,
                            silent,
                            approval,
                            channel_name,
                            multimodal_config,
                            max_tool_iterations,
                            cancellation_token,
                            on_delta,
                            hooks,
                            excluded_tools,
                        ),
                    ),
                ),
            ),
        )
        .await
}

// ── Agent Tool-Call Loop ──────────────────────────────────────────────────
// Core agentic iteration: send conversation to the LLM, parse any tool
// calls from the response, execute them, append results to history, and
// repeat until the LLM produces a final text-only answer.
//
// Loop invariant: at the start of each iteration, `history` contains the
// full conversation so far (system prompt + user messages + prior tool
// results). The loop exits when:
//   • the LLM returns no tool calls (final answer), or
//   • max_iterations is reached (runaway safety), or
//   • the cancellation token fires (external abort).

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_call_loop(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
) -> Result<String> {
    let non_cli_approval_context = TOOL_LOOP_NON_CLI_APPROVAL_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten();
    let channel_reply_target = TOOL_LOOP_REPLY_TARGET
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .or_else(|| {
            non_cli_approval_context
                .as_ref()
                .map(|ctx| ctx.reply_target.clone())
        });

    let max_iterations = if max_tool_iterations == 0 {
        DEFAULT_MAX_TOOL_ITERATIONS
    } else {
        max_tool_iterations
    };

    let tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
        .iter()
        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
        .map(|tool| tool.spec())
        .collect();
    let use_native_tools = provider.supports_native_tools() && !tool_specs.is_empty();
    let turn_id = Uuid::new_v4().to_string();
    let mut seen_tool_signatures: HashSet<(String, String)> = HashSet::new();
    let mut missing_tool_call_retry_used = false;
    let mut missing_tool_call_retry_prompt: Option<String> = None;
    let ld_config = LOOP_DETECTION_CONFIG
        .try_with(Clone::clone)
        .unwrap_or_default();
    let mut loop_detector = LoopDetector::new(ld_config);
    let mut loop_detection_prompt: Option<String> = None;
    let heartbeat_config = SAFETY_HEARTBEAT_CONFIG
        .try_with(Clone::clone)
        .ok()
        .flatten();
    let progress_mode = TOOL_LOOP_PROGRESS_MODE
        .try_with(|mode| *mode)
        .unwrap_or(ProgressMode::Verbose);
    let cost_enforcement_context = TOOL_LOOP_COST_ENFORCEMENT_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten();
    let mut progress_tracker = ProgressTracker::default();
    let mut active_model = model.to_string();
    let bypass_non_cli_approval_for_turn =
        approval.is_some_and(|mgr| channel_name != "cli" && mgr.consume_non_cli_allow_all_once());
    if bypass_non_cli_approval_for_turn {
        runtime_trace::record_event(
            "approval_bypass_one_time_all_tools_consumed",
            Some(channel_name),
            Some(provider_name),
            Some(active_model.as_str()),
            Some(&turn_id),
            Some(true),
            Some("consumed one-time non-cli allow-all approval token"),
            serde_json::json!({}),
        );
    }

    for iteration in 0..max_iterations {
        if cancellation_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolLoopCancelled.into());
        }

        let image_marker_count = multimodal::count_image_markers(history);
        let provider_supports_vision =
            should_treat_provider_as_vision_capable(provider_name, provider);
        if image_marker_count > 0 && !provider_supports_vision {
            return Err(ProviderCapabilityError {
                provider: provider_name.to_string(),
                capability: "vision".to_string(),
                message: format!(
                    "received {image_marker_count} image marker(s), but this provider does not support vision input"
                ),
            }
            .into());
        }

        let prepared_messages = multimodal::prepare_messages_for_provider_with_provider_hint(
            history,
            multimodal_config,
            Some(provider_name),
        )
        .await?;
        let mut request_messages = prepared_messages.messages.clone();
        if let Some(prompt) = missing_tool_call_retry_prompt.take() {
            request_messages.push(ChatMessage::user(prompt));
        }
        if let Some(prompt) = loop_detection_prompt.take() {
            request_messages.push(ChatMessage::user(prompt));
        }

        // ── Safety heartbeat: periodic security-constraint re-injection ──
        if let Some(ref hb) = heartbeat_config {
            if should_inject_safety_heartbeat(iteration, hb.interval) {
                let reminder = format!(
                    "[Safety Heartbeat — round {}/{}]\n{}",
                    iteration + 1,
                    max_iterations,
                    hb.body
                );
                request_messages.push(ChatMessage::user(reminder));
            }
        }
        // Unified path via Provider::chat so provider-specific native tool logic
        // (OpenAI/Anthropic/OpenRouter/compatible adapters) is honored.
        let request_tools = if use_native_tools {
            Some(tool_specs.as_slice())
        } else {
            None
        };

        // ── Progress: LLM thinking ────────────────────────────
        if should_emit_verbose_progress(progress_mode) {
            if let Some(ref tx) = on_delta {
                let phase = if iteration == 0 {
                    "\u{1f914} Thinking...\n".to_string()
                } else {
                    format!("\u{1f914} Thinking (round {})...\n", iteration + 1)
                };
                let _ = tx.send(format!("{DRAFT_PROGRESS_SENTINEL}{phase}")).await;
            }
        }

        if let Some(cost_ctx) = cost_enforcement_context.as_ref() {
            let mut estimated_cost_usd = estimate_request_cost_usd(
                cost_ctx,
                provider_name,
                active_model.as_str(),
                &request_messages,
                request_tools,
            );

            let mut budget_check = match cost_ctx.tracker.check_budget(estimated_cost_usd) {
                Ok(check) => Some(check),
                Err(error) => {
                    tracing::warn!("Cost preflight check failed: {error}");
                    None
                }
            };

            if matches!(cost_ctx.mode, CostEnforcementMode::RouteDown)
                && matches!(budget_check, Some(BudgetCheck::Exceeded { .. }))
            {
                if let Some(route_down_model) = cost_ctx
                    .route_down_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    if route_down_model != active_model {
                        let previous_model = active_model.clone();
                        active_model = route_down_model.to_string();
                        estimated_cost_usd = estimate_request_cost_usd(
                            cost_ctx,
                            provider_name,
                            active_model.as_str(),
                            &request_messages,
                            request_tools,
                        );
                        budget_check = match cost_ctx.tracker.check_budget(estimated_cost_usd) {
                            Ok(check) => Some(check),
                            Err(error) => {
                                tracing::warn!(
                                    "Cost preflight check failed after route-down: {error}"
                                );
                                None
                            }
                        };
                        runtime_trace::record_event(
                            "cost_budget_route_down",
                            Some(channel_name),
                            Some(provider_name),
                            Some(active_model.as_str()),
                            Some(&turn_id),
                            Some(true),
                            Some("budget exceeded on primary model; route-down candidate applied"),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "from_model": previous_model,
                                "to_model": active_model,
                                "estimated_cost_usd": estimated_cost_usd,
                            }),
                        );
                    }
                }
            }

            if let Some(check) = budget_check {
                match check {
                    BudgetCheck::Allowed => {}
                    BudgetCheck::Warning {
                        current_usd,
                        limit_usd,
                        period,
                    } => {
                        tracing::warn!(
                            model = active_model.as_str(),
                            period = usage_period_label(period),
                            current_usd,
                            limit_usd,
                            estimated_cost_usd,
                            "Cost budget warning threshold reached"
                        );
                        runtime_trace::record_event(
                            "cost_budget_warning",
                            Some(channel_name),
                            Some(provider_name),
                            Some(active_model.as_str()),
                            Some(&turn_id),
                            Some(true),
                            Some("budget warning threshold reached"),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "period": usage_period_label(period),
                                "current_usd": current_usd,
                                "limit_usd": limit_usd,
                                "estimated_cost_usd": estimated_cost_usd,
                            }),
                        );
                    }
                    BudgetCheck::Exceeded {
                        current_usd,
                        limit_usd,
                        period,
                    } => match cost_ctx.mode {
                        CostEnforcementMode::Warn => {
                            tracing::warn!(
                                model = active_model.as_str(),
                                period = usage_period_label(period),
                                current_usd,
                                limit_usd,
                                estimated_cost_usd,
                                "Cost budget exceeded (warn mode): continuing request"
                            );
                            runtime_trace::record_event(
                                "cost_budget_exceeded_warn_mode",
                                Some(channel_name),
                                Some(provider_name),
                                Some(active_model.as_str()),
                                Some(&turn_id),
                                Some(true),
                                Some("budget exceeded but proceeding due to warn mode"),
                                serde_json::json!({
                                    "iteration": iteration + 1,
                                    "period": usage_period_label(period),
                                    "current_usd": current_usd,
                                    "limit_usd": limit_usd,
                                    "estimated_cost_usd": estimated_cost_usd,
                                }),
                            );
                        }
                        CostEnforcementMode::RouteDown | CostEnforcementMode::Block => {
                            let message = budget_exceeded_message(
                                active_model.as_str(),
                                estimated_cost_usd,
                                current_usd,
                                limit_usd,
                                period,
                            );
                            runtime_trace::record_event(
                                "cost_budget_blocked",
                                Some(channel_name),
                                Some(provider_name),
                                Some(active_model.as_str()),
                                Some(&turn_id),
                                Some(false),
                                Some(&message),
                                serde_json::json!({
                                    "iteration": iteration + 1,
                                    "period": usage_period_label(period),
                                    "current_usd": current_usd,
                                    "limit_usd": limit_usd,
                                    "estimated_cost_usd": estimated_cost_usd,
                                }),
                            );
                            return Err(anyhow::anyhow!(message));
                        }
                    },
                }
            }
        }

        observer.record_event(&ObserverEvent::LlmRequest {
            provider: provider_name.to_string(),
            model: active_model.clone(),
            messages_count: history.len(),
        });
        runtime_trace::record_event(
            "llm_request",
            Some(channel_name),
            Some(provider_name),
            Some(active_model.as_str()),
            Some(&turn_id),
            None,
            None,
            serde_json::json!({
                "iteration": iteration + 1,
                "messages_count": history.len(),
            }),
        );

        let llm_started_at = Instant::now();

        // Fire void hook before LLM call
        if let Some(hooks) = hooks {
            hooks.fire_llm_input(history, active_model.as_str()).await;
        }

        let chat_future = provider.chat(
            ChatRequest {
                messages: &request_messages,
                tools: request_tools,
            },
            active_model.as_str(),
            temperature,
        );

        let chat_result = if let Some(token) = cancellation_token.as_ref() {
            tokio::select! {
                () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                result = chat_future => result,
            }
        } else {
            chat_future.await
        };

        let (
            response_text,
            parsed_text,
            tool_calls,
            assistant_history_content,
            native_tool_calls,
            parse_issue_detected,
        ) = match chat_result {
            Ok(resp) => {
                let mut response_text = resp.text_or_empty().to_string();
                let mut native_calls = resp.tool_calls;
                let mut reasoning_content = resp.reasoning_content.clone();
                let mut stop_reason = resp.stop_reason.clone();
                let mut raw_stop_reason = resp.raw_stop_reason.clone();
                let (mut resp_input_tokens, mut resp_output_tokens) = resp
                    .usage
                    .as_ref()
                    .map(|u| (u.input_tokens, u.output_tokens))
                    .unwrap_or((None, None));

                if let Some(reason) = stop_reason.as_ref() {
                    runtime_trace::record_event(
                        "stop_reason_observed",
                        Some(channel_name),
                        Some(provider_name),
                        Some(active_model.as_str()),
                        Some(&turn_id),
                        Some(true),
                        None,
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "normalized_reason": stop_reason_name(reason),
                            "raw_reason": raw_stop_reason.clone(),
                        }),
                    );
                }

                let mut continuation_attempts = 0usize;
                let mut continuation_termination_reason: Option<&'static str> = None;
                let mut continuation_error: Option<String> = None;
                let mut output_chars = response_text.chars().count();

                while matches!(stop_reason, Some(NormalizedStopReason::MaxTokens))
                    && native_calls.is_empty()
                    && continuation_attempts < MAX_TOKENS_CONTINUATION_MAX_ATTEMPTS
                    && output_chars < MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS
                {
                    continuation_attempts += 1;
                    runtime_trace::record_event(
                        "continuation_attempt",
                        Some(channel_name),
                        Some(provider_name),
                        Some(active_model.as_str()),
                        Some(&turn_id),
                        Some(true),
                        None,
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "attempt": continuation_attempts,
                            "output_chars": output_chars,
                            "max_output_chars": MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS,
                        }),
                    );

                    let mut continuation_messages = request_messages.clone();
                    continuation_messages.push(ChatMessage::assistant(response_text.clone()));
                    continuation_messages.push(ChatMessage::user(
                        MAX_TOKENS_CONTINUATION_PROMPT.to_string(),
                    ));

                    let continuation_future = provider.chat(
                        ChatRequest {
                            messages: &continuation_messages,
                            tools: request_tools,
                        },
                        active_model.as_str(),
                        temperature,
                    );
                    let continuation_result = if let Some(token) = cancellation_token.as_ref() {
                        tokio::select! {
                            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                            result = continuation_future => result,
                        }
                    } else {
                        continuation_future.await
                    };

                    let continuation_resp = match continuation_result {
                        Ok(response) => response,
                        Err(error) => {
                            continuation_termination_reason = Some("provider_error");
                            continuation_error =
                                Some(crate::providers::sanitize_api_error(&error.to_string()));
                            break;
                        }
                    };

                    if let Some(usage) = continuation_resp.usage.as_ref() {
                        resp_input_tokens = add_optional_u64(resp_input_tokens, usage.input_tokens);
                        resp_output_tokens =
                            add_optional_u64(resp_output_tokens, usage.output_tokens);
                    }

                    let next_text = continuation_resp.text_or_empty().to_string();
                    let merged_text = merge_continuation_text(&response_text, &next_text);
                    let merged_chars = merged_text.chars().count();
                    if merged_chars > MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS {
                        response_text = merged_text
                            .chars()
                            .take(MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS)
                            .collect();
                        output_chars = MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS;
                        stop_reason = Some(NormalizedStopReason::MaxTokens);
                        continuation_termination_reason = Some("output_cap");
                        break;
                    }
                    response_text = merged_text;
                    output_chars = merged_chars;

                    if continuation_resp.reasoning_content.is_some() {
                        reasoning_content = continuation_resp.reasoning_content.clone();
                    }
                    if !continuation_resp.tool_calls.is_empty() {
                        native_calls = continuation_resp.tool_calls;
                    }
                    stop_reason = continuation_resp.stop_reason;
                    raw_stop_reason = continuation_resp.raw_stop_reason;

                    if let Some(reason) = stop_reason.as_ref() {
                        runtime_trace::record_event(
                            "stop_reason_observed",
                            Some(channel_name),
                            Some(provider_name),
                            Some(active_model.as_str()),
                            Some(&turn_id),
                            Some(true),
                            None,
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "continuation_attempt": continuation_attempts,
                                "normalized_reason": stop_reason_name(reason),
                                "raw_reason": raw_stop_reason.clone(),
                            }),
                        );
                    }
                }

                if continuation_attempts > 0 && continuation_termination_reason.is_none() {
                    continuation_termination_reason =
                        if matches!(stop_reason, Some(NormalizedStopReason::MaxTokens)) {
                            if output_chars >= MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS {
                                Some("output_cap")
                            } else {
                                Some("retry_limit")
                            }
                        } else {
                            Some("completed")
                        };
                }

                if let Some(terminal_reason) = continuation_termination_reason {
                    runtime_trace::record_event(
                        "continuation_terminated",
                        Some(channel_name),
                        Some(provider_name),
                        Some(active_model.as_str()),
                        Some(&turn_id),
                        Some(terminal_reason == "completed"),
                        continuation_error.as_deref(),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "attempts": continuation_attempts,
                            "terminal_reason": terminal_reason,
                            "output_chars": output_chars,
                        }),
                    );
                }

                if continuation_attempts > 0
                    && matches!(stop_reason, Some(NormalizedStopReason::MaxTokens))
                    && native_calls.is_empty()
                    && !response_text.ends_with(MAX_TOKENS_CONTINUATION_NOTICE)
                {
                    response_text.push_str(MAX_TOKENS_CONTINUATION_NOTICE);
                }

                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: active_model.clone(),
                    duration: llm_started_at.elapsed(),
                    success: true,
                    error_message: None,
                    input_tokens: resp_input_tokens,
                    output_tokens: resp_output_tokens,
                });

                // First try native structured tool calls (OpenAI-format).
                // Fall back to text-based parsing (XML tags, markdown blocks,
                // GLM format) only if the provider returned no native calls —
                // this ensures we support both native and prompt-guided models.
                let structured_parse = parse_structured_tool_calls(&native_calls);
                let invalid_native_tool_json_count = structured_parse.invalid_json_arguments;
                let mut calls = structured_parse.calls;
                if invalid_native_tool_json_count > 0 {
                    // Safety policy: when native tool-call args are partially truncated
                    // or malformed, do not execute any parsed subset in this turn.
                    calls.clear();
                }
                let mut parsed_text = String::new();

                if invalid_native_tool_json_count == 0 && calls.is_empty() {
                    let (fallback_text, fallback_calls) = parse_tool_calls(&response_text);
                    if !fallback_text.is_empty() {
                        parsed_text = fallback_text;
                    }
                    calls = fallback_calls;
                }

                let mut parse_issue = detect_tool_call_parse_issue(&response_text, &calls);
                if parse_issue.is_none() && invalid_native_tool_json_count > 0 {
                    parse_issue = Some(format!(
                        "provider returned {invalid_native_tool_json_count} native tool call(s) with invalid JSON arguments"
                    ));
                }
                if let Some(parse_issue) = parse_issue.as_deref() {
                    runtime_trace::record_event(
                        "tool_call_parse_issue",
                        Some(channel_name),
                        Some(provider_name),
                        Some(active_model.as_str()),
                        Some(&turn_id),
                        Some(false),
                        Some(parse_issue),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "invalid_native_tool_json_count": invalid_native_tool_json_count,
                            "response_excerpt": truncate_with_ellipsis(
                                &scrub_credentials(&response_text),
                                600
                            ),
                        }),
                    );
                }

                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(true),
                    None,
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                        "input_tokens": resp_input_tokens,
                        "output_tokens": resp_output_tokens,
                        "raw_response": scrub_credentials(&response_text),
                        "native_tool_calls": native_calls.len(),
                        "parsed_tool_calls": calls.len(),
                        "continuation_attempts": continuation_attempts,
                        "stop_reason": stop_reason.as_ref().map(stop_reason_name),
                        "raw_stop_reason": raw_stop_reason,
                    }),
                );

                // Preserve native tool call IDs in assistant history so role=tool
                // follow-up messages can reference the exact call id.
                let assistant_history_content = if native_calls.is_empty() {
                    if use_native_tools {
                        build_native_assistant_history_from_parsed_calls(
                            &response_text,
                            &calls,
                            reasoning_content.as_deref(),
                        )
                        .unwrap_or_else(|| response_text.clone())
                    } else {
                        response_text.clone()
                    }
                } else {
                    build_native_assistant_history(
                        &response_text,
                        &native_calls,
                        reasoning_content.as_deref(),
                    )
                };

                (
                    response_text,
                    parsed_text,
                    calls,
                    assistant_history_content,
                    native_calls,
                    parse_issue.is_some(),
                )
            }
            Err(e) => {
                let safe_error = crate::providers::sanitize_api_error(&e.to_string());
                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: active_model.clone(),
                    duration: llm_started_at.elapsed(),
                    success: false,
                    error_message: Some(safe_error.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(false),
                    Some(&safe_error),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                    }),
                );
                return Err(e);
            }
        };

        let display_text = if parsed_text.is_empty() {
            response_text.clone()
        } else {
            parsed_text
        };

        // ── Progress: LLM responded ─────────────────────────────
        if should_emit_verbose_progress(progress_mode) {
            if let Some(ref tx) = on_delta {
                let llm_secs = llm_started_at.elapsed().as_secs();
                if !tool_calls.is_empty() {
                    let _ = tx
                        .send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}\u{1f4ac} Got {} tool call(s) ({llm_secs}s)\n",
                            tool_calls.len()
                        ))
                        .await;
                }
            }
        }

        if tool_calls.is_empty() {
            let missing_tool_call_signal =
                parse_issue_detected || looks_like_deferred_action_without_tool_call(&display_text);
            let missing_tool_call_followthrough = !missing_tool_call_retry_used
                && iteration + 1 < max_iterations
                && !tool_specs.is_empty()
                && missing_tool_call_signal;
            if missing_tool_call_followthrough {
                missing_tool_call_retry_used = true;
                missing_tool_call_retry_prompt = Some(MISSING_TOOL_CALL_RETRY_PROMPT.to_string());
                let retry_reason = if parse_issue_detected {
                    "parse_issue_detected"
                } else {
                    "deferred_action_text_detected"
                };

                runtime_trace::record_event(
                    "tool_call_followthrough_retry",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(true),
                    Some("llm response implied follow-up action but emitted no tool call"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "reason": retry_reason,
                        "response_excerpt": truncate_with_ellipsis(&scrub_credentials(&display_text), 600),
                    }),
                );

                if should_emit_verbose_progress(progress_mode) {
                    if let Some(ref tx) = on_delta {
                        let _ = tx
                            .send(format!(
                                "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Retrying: response deferred action without a tool call\n"
                            ))
                            .await;
                    }
                }

                continue;
            }

            if missing_tool_call_retry_used && !tool_specs.is_empty() && missing_tool_call_signal {
                runtime_trace::record_event(
                    "tool_call_followthrough_failed",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(false),
                    Some("llm response still implied follow-up action but emitted no tool call after retry"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "response_excerpt": truncate_with_ellipsis(&scrub_credentials(&display_text), 600),
                    }),
                );
                anyhow::bail!(
                    "Model deferred action without emitting a tool call after retry; refusing to return unverified completion."
                );
            }

            runtime_trace::record_event(
                "turn_final_response",
                Some(channel_name),
                Some(provider_name),
                Some(active_model.as_str()),
                Some(&turn_id),
                Some(true),
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "text": scrub_credentials(&display_text),
                }),
            );
            // No tool calls — this is the final response.
            // If a streaming sender is provided, relay the text in small chunks
            // so the channel can progressively update the draft message.
            if let Some(ref tx) = on_delta {
                // Clear accumulated progress lines before streaming the final answer.
                let _ = tx.send(DRAFT_CLEAR_SENTINEL.to_string()).await;
                // Split on whitespace boundaries, accumulating chunks of at least
                // STREAM_CHUNK_MIN_CHARS characters for progressive draft updates.
                let mut chunk = String::new();
                for word in display_text.split_inclusive(char::is_whitespace) {
                    if cancellation_token
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                    {
                        return Err(ToolLoopCancelled.into());
                    }
                    chunk.push_str(word);
                    if chunk.len() >= STREAM_CHUNK_MIN_CHARS
                        && tx.send(std::mem::take(&mut chunk)).await.is_err()
                    {
                        break; // receiver dropped
                    }
                }
                if !chunk.is_empty() {
                    let _ = tx.send(chunk).await;
                }
            }
            history.push(ChatMessage::assistant(response_text.clone()));
            return Ok(display_text);
        }

        // Print any text the LLM produced alongside tool calls (unless silent)
        if !silent && !display_text.is_empty() {
            print!("{display_text}");
            let _ = std::io::stdout().flush();
        }

        // Execute tool calls and build results. `individual_results` tracks per-call output so
        // native-mode history can emit one role=tool message per tool call with the correct ID.
        //
        // When multiple tool calls are present and interactive CLI approval is not needed, run
        // tool executions concurrently for lower wall-clock latency.
        let mut tool_results = String::new();
        let mut individual_results: Vec<(Option<String>, String)> = Vec::new();
        let mut ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>> =
            (0..tool_calls.len()).map(|_| None).collect();
        let allow_parallel_execution = should_execute_tools_in_parallel(&tool_calls, approval);
        let mut executable_indices: Vec<usize> = Vec::new();
        let mut executable_calls: Vec<ParsedToolCall> = Vec::new();
        let mut progress_indices: Vec<Option<usize>> = Vec::new();

        for (idx, call) in tool_calls.iter().enumerate() {
            // ── Hook: before_tool_call (modifying) ──────────
            let mut tool_name = call.name.clone();
            let mut tool_args = call.arguments.clone();
            if let Some(hooks) = hooks {
                match hooks
                    .run_before_tool_call(tool_name.clone(), tool_args.clone())
                    .await
                {
                    crate::hooks::HookResult::Cancel(reason) => {
                        tracing::info!(tool = %call.name, %reason, "tool call cancelled by hook");
                        let cancelled = format!("Cancelled by hook: {reason}");
                        runtime_trace::record_event(
                            "tool_call_result",
                            Some(channel_name),
                            Some(provider_name),
                            Some(active_model.as_str()),
                            Some(&turn_id),
                            Some(false),
                            Some(&cancelled),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": call.name,
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        ordered_results[idx] = Some((
                            call.name.clone(),
                            call.tool_call_id.clone(),
                            ToolExecutionOutcome {
                                output: cancelled,
                                success: false,
                                error_reason: Some(scrub_credentials(&reason)),
                                duration: Duration::ZERO,
                            },
                        ));
                        continue;
                    }
                    crate::hooks::HookResult::Continue((name, args)) => {
                        tool_name = name;
                        tool_args = args;
                    }
                }
            }

            maybe_inject_cron_add_delivery(
                &tool_name,
                &mut tool_args,
                channel_name,
                channel_reply_target.as_deref(),
            );

            if excluded_tools.iter().any(|ex| ex == &tool_name) {
                let blocked = format!("Tool '{tool_name}' is not available in this channel.");
                runtime_trace::record_event(
                    "tool_call_result",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(false),
                    Some(&blocked),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "tool": tool_name.clone(),
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "blocked_by_channel_policy": true,
                    }),
                );
                ordered_results[idx] = Some((
                    tool_name.clone(),
                    call.tool_call_id.clone(),
                    ToolExecutionOutcome {
                        output: blocked.clone(),
                        success: false,
                        error_reason: Some(blocked),
                        duration: Duration::ZERO,
                    },
                ));
                continue;
            }

            // ── Approval hook ────────────────────────────────
            if let Some(mgr) = approval {
                let non_cli_session_granted =
                    channel_name != "cli" && mgr.is_non_cli_session_granted(&tool_name);
                if bypass_non_cli_approval_for_turn || non_cli_session_granted {
                    mgr.record_decision(
                        &tool_name,
                        &tool_args,
                        ApprovalResponse::Yes,
                        channel_name,
                    );
                    if non_cli_session_granted {
                        runtime_trace::record_event(
                            "approval_bypass_non_cli_session_grant",
                            Some(channel_name),
                            Some(provider_name),
                            Some(active_model.as_str()),
                            Some(&turn_id),
                            Some(true),
                            Some("using runtime non-cli session approval grant"),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": tool_name.clone(),
                            }),
                        );
                    }
                } else if mgr.needs_approval(&tool_name) {
                    let request = ApprovalRequest {
                        tool_name: tool_name.clone(),
                        arguments: tool_args.clone(),
                    };

                    let decision = if channel_name == "cli" {
                        mgr.prompt_cli(&request)
                    } else if let Some(ctx) = non_cli_approval_context.as_ref() {
                        let pending = mgr.create_non_cli_pending_request(
                            &tool_name,
                            &ctx.sender,
                            channel_name,
                            &ctx.reply_target,
                            Some(
                                "interactive approval required for supervised non-cli tool execution"
                                    .to_string(),
                            ),
                        );

                        let _ = ctx.prompt_tx.send(NonCliApprovalPrompt {
                            request_id: pending.request_id.clone(),
                            tool_name: tool_name.clone(),
                            arguments: tool_args.clone(),
                        });

                        await_non_cli_approval_decision(
                            mgr,
                            &pending.request_id,
                            &ctx.sender,
                            channel_name,
                            &ctx.reply_target,
                            cancellation_token.as_ref(),
                        )
                        .await
                    } else {
                        ApprovalResponse::No
                    };

                    mgr.record_decision(&tool_name, &tool_args, decision, channel_name);

                    if decision == ApprovalResponse::No {
                        let denied = "Denied by user.".to_string();
                        runtime_trace::record_event(
                            "tool_call_result",
                            Some(channel_name),
                            Some(provider_name),
                            Some(active_model.as_str()),
                            Some(&turn_id),
                            Some(false),
                            Some(&denied),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": tool_name.clone(),
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        ordered_results[idx] = Some((
                            tool_name.clone(),
                            call.tool_call_id.clone(),
                            ToolExecutionOutcome {
                                output: denied.clone(),
                                success: false,
                                error_reason: Some(denied),
                                duration: Duration::ZERO,
                            },
                        ));
                        continue;
                    }
                }
            }

            let signature = tool_call_signature(&tool_name, &tool_args);
            if !seen_tool_signatures.insert(signature) {
                let duplicate = format!(
                    "Skipped duplicate tool call '{tool_name}' with identical arguments in this turn."
                );
                runtime_trace::record_event(
                    "tool_call_result",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(false),
                    Some(&duplicate),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "tool": tool_name.clone(),
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "deduplicated": true,
                    }),
                );
                ordered_results[idx] = Some((
                    tool_name.clone(),
                    call.tool_call_id.clone(),
                    ToolExecutionOutcome {
                        output: duplicate.clone(),
                        success: false,
                        error_reason: Some(duplicate),
                        duration: Duration::ZERO,
                    },
                ));
                continue;
            }

            runtime_trace::record_event(
                "tool_call_start",
                Some(channel_name),
                Some(provider_name),
                Some(active_model.as_str()),
                Some(&turn_id),
                None,
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": tool_name.clone(),
                    "arguments": scrub_credentials(&tool_args.to_string()),
                }),
            );

            let progress_idx = if should_emit_tool_progress(progress_mode) {
                let hint = truncate_tool_args_for_progress(&tool_name, &tool_args, 60);
                let idx = progress_tracker.add(&tool_name, &hint);
                if let Some(ref tx) = on_delta {
                    tracing::debug!(tool = %tool_name, "Sending progress start to draft");
                    let _ = tx.send(progress_tracker.render_delta()).await;
                }
                Some(idx)
            } else {
                None
            };

            executable_indices.push(idx);
            executable_calls.push(ParsedToolCall {
                name: tool_name,
                arguments: tool_args,
                tool_call_id: call.tool_call_id.clone(),
            });
            progress_indices.push(progress_idx);
        }

        let executed_outcomes = if allow_parallel_execution && executable_calls.len() > 1 {
            execute_tools_parallel(
                &executable_calls,
                tools_registry,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        } else {
            execute_tools_sequential(
                &executable_calls,
                tools_registry,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        };

        for (((idx, call), mut outcome), progress_idx) in executable_indices
            .iter()
            .zip(executable_calls.iter())
            .zip(executed_outcomes.into_iter())
            .zip(progress_indices.iter())
        {
            runtime_trace::record_event(
                "tool_call_result",
                Some(channel_name),
                Some(provider_name),
                Some(active_model.as_str()),
                Some(&turn_id),
                Some(outcome.success),
                outcome.error_reason.as_deref(),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": call.name.clone(),
                    "duration_ms": outcome.duration.as_millis(),
                    "output": scrub_credentials(&outcome.output),
                }),
            );

            // ── Hook: after_tool_call (void) ─────────────────
            if let Some(hooks) = hooks {
                let mut tool_result_obj = crate::tools::ToolResult {
                    success: outcome.success,
                    output: outcome.output.clone(),
                    error: outcome.error_reason.clone(),
                };
                match hooks
                    .run_tool_result_persist(call.name.clone(), tool_result_obj.clone())
                    .await
                {
                    crate::hooks::HookResult::Continue(next) => {
                        tool_result_obj = next;
                        outcome.success = tool_result_obj.success;
                        outcome.output = tool_result_obj.output.clone();
                        outcome.error_reason = tool_result_obj.error.clone();
                    }
                    crate::hooks::HookResult::Cancel(reason) => {
                        outcome.success = false;
                        outcome.error_reason = Some(scrub_credentials(&reason));
                        outcome.output = format!("Tool result blocked by hook: {reason}");
                        tool_result_obj.success = false;
                        tool_result_obj.error = Some(reason);
                        tool_result_obj.output = outcome.output.clone();
                    }
                }
                hooks
                    .fire_after_tool_call(&call.name, &tool_result_obj, outcome.duration)
                    .await;
            }

            if let Some(idx) = progress_idx {
                let secs = outcome.duration.as_secs();
                progress_tracker.complete(*idx, outcome.success, secs);
                if let Some(ref tx) = on_delta {
                    tracing::debug!(tool = %call.name, secs, "Sending progress complete to draft");
                    let _ = tx.send(progress_tracker.render_delta()).await;
                }
            }

            // ── Loop detection: record call ──────────────────────
            {
                let sig = tool_call_signature(&call.name, &call.arguments);
                loop_detector.record_call(&sig.0, &sig.1, &outcome.output, outcome.success);
            }

            ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
        }

        for (tool_name, tool_call_id, outcome) in ordered_results.into_iter().flatten() {
            individual_results.push((tool_call_id, outcome.output.clone()));
            let _ = writeln!(
                tool_results,
                "<tool_result name=\"{}\">\n{}\n</tool_result>",
                tool_name, outcome.output
            );
        }

        // Add assistant message with tool calls + tool results to history.
        // Native mode: use JSON-structured messages so convert_messages() can
        // reconstruct proper OpenAI-format tool_calls and tool result messages.
        // Prompt mode: use XML-based text format as before.
        history.push(ChatMessage::assistant(assistant_history_content));
        if native_tool_calls.is_empty() {
            let all_results_have_ids = use_native_tools
                && !individual_results.is_empty()
                && individual_results
                    .iter()
                    .all(|(tool_call_id, _)| tool_call_id.is_some());
            if all_results_have_ids {
                for (tool_call_id, result) in &individual_results {
                    let tool_msg = serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "content": result,
                    });
                    history.push(ChatMessage::tool(tool_msg.to_string()));
                }
            } else {
                history.push(ChatMessage::user(format!("[Tool results]\n{tool_results}")));
            }
        } else {
            for (native_call, (_, result)) in
                native_tool_calls.iter().zip(individual_results.iter())
            {
                let tool_msg = serde_json::json!({
                    "tool_call_id": native_call.id,
                    "content": result,
                });
                history.push(ChatMessage::tool(tool_msg.to_string()));
            }
        }

        // ── Loop detection: check verdict ────────────────────────
        match loop_detector.check() {
            DetectionVerdict::Continue => {}
            DetectionVerdict::InjectWarning(warning) => {
                runtime_trace::record_event(
                    "loop_detected_warning",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(false),
                    Some("loop pattern detected, injecting self-correction prompt"),
                    serde_json::json!({ "iteration": iteration + 1, "warning": &warning }),
                );
                if should_emit_verbose_progress(progress_mode) {
                    if let Some(ref tx) = on_delta {
                        let _ = tx
                            .send(format!(
                                "{DRAFT_PROGRESS_SENTINEL}\u{26a0}\u{fe0f} Loop detected, attempting self-correction\n"
                            ))
                            .await;
                    }
                }
                loop_detection_prompt = Some(warning);
            }
            DetectionVerdict::HardStop(reason) => {
                runtime_trace::record_event(
                    "loop_detected_hard_stop",
                    Some(channel_name),
                    Some(provider_name),
                    Some(active_model.as_str()),
                    Some(&turn_id),
                    Some(false),
                    Some("loop persisted after warning, stopping early"),
                    serde_json::json!({ "iteration": iteration + 1, "reason": &reason }),
                );
                anyhow::bail!(
                    "Agent stopped early due to detected loop pattern (iteration {}/{}): {}",
                    iteration + 1,
                    max_iterations,
                    reason
                );
            }
        }
    }

    runtime_trace::record_event(
        "tool_loop_exhausted",
        Some(channel_name),
        Some(provider_name),
        Some(active_model.as_str()),
        Some(&turn_id),
        Some(false),
        Some("agent exceeded maximum tool iterations"),
        serde_json::json!({
            "max_iterations": max_iterations,
        }),
    );
    anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
}

/// Build the tool instruction block for the system prompt from concrete tool
/// specs so the LLM knows how to invoke tools.
pub(crate) fn build_tool_instructions(tools_registry: &[Box<dyn Tool>]) -> String {
    let specs: Vec<crate::tools::ToolSpec> =
        tools_registry.iter().map(|tool| tool.spec()).collect();
    build_tool_instructions_from_specs(&specs)
}

/// Build the tool instruction block for the system prompt from concrete tool
/// specs so the LLM knows how to invoke tools.
pub(crate) fn build_tool_instructions_from_specs(tool_specs: &[crate::tools::ToolSpec]) -> String {
    let mut instructions = String::new();
    instructions.push_str("\n## Tool Use Protocol\n\n");
    instructions.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    instructions.push_str("```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n```\n\n");
    instructions.push_str(
        "CRITICAL: Output actual <tool_call> tags—never describe steps or give examples.\n\n",
    );
    instructions.push_str(
        "When a tool is needed, emit a real call (not prose), for example:\n\
<tool_call>\n\
{\"name\":\"tool_name\",\"arguments\":{}}\n\
</tool_call>\n\n",
    );
    instructions.push_str("You may use multiple tool calls in a single response. ");
    instructions.push_str("After tool execution, results appear in <tool_result> tags. ");
    instructions
        .push_str("Continue reasoning with the results until you can give a final answer.\n\n");
    instructions.push_str("### Available Tools\n\n");

    for tool in tool_specs {
        let _ = writeln!(
            instructions,
            "**{}**: {}\nParameters: `{}`\n",
            tool.name, tool.description, tool.parameters
        );
    }

    instructions
}

/// Build shell-policy instructions for the system prompt so the model is aware
/// of command-level execution constraints before it emits tool calls.
pub(crate) fn build_shell_policy_instructions(autonomy: &crate::config::AutonomyConfig) -> String {
    let mut instructions = String::new();
    instructions.push_str("\n## Shell Policy\n\n");
    instructions
        .push_str("When using the `shell` tool, follow these runtime constraints exactly.\n\n");

    let autonomy_label = match autonomy.level {
        crate::security::AutonomyLevel::ReadOnly => "read_only",
        crate::security::AutonomyLevel::Supervised => "supervised",
        crate::security::AutonomyLevel::Full => "full",
    };
    let _ = writeln!(instructions, "- Autonomy level: `{autonomy_label}`");

    if autonomy.level == crate::security::AutonomyLevel::ReadOnly {
        instructions.push_str(
            "- Shell execution is disabled in `read_only` mode. Do not emit shell tool calls.\n",
        );
        return instructions;
    }

    let normalized: BTreeSet<String> = autonomy
        .allowed_commands
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if normalized.contains("*") {
        instructions.push_str(
            "- Allowed commands: wildcard `*` is configured (any command name/path may be allowlisted).\n",
        );
    } else if normalized.is_empty() {
        instructions
            .push_str("- Allowed commands: none configured. Any shell command will be rejected.\n");
    } else {
        const MAX_DISPLAY_COMMANDS: usize = 64;
        let shown: Vec<String> = normalized
            .iter()
            .take(MAX_DISPLAY_COMMANDS)
            .map(|cmd| format!("`{cmd}`"))
            .collect();
        let hidden = normalized.len().saturating_sub(MAX_DISPLAY_COMMANDS);
        let _ = write!(instructions, "- Allowed commands: {}", shown.join(", "));
        if hidden > 0 {
            let _ = write!(instructions, " (+{hidden} more)");
        }
        instructions.push('\n');
    }

    if autonomy.level == crate::security::AutonomyLevel::Supervised
        && autonomy.require_approval_for_medium_risk
    {
        instructions.push_str(
            "- Medium-risk shell commands require explicit approval in `supervised` mode.\n",
        );
    }
    if autonomy.block_high_risk_commands {
        instructions.push_str(
            "- High-risk shell commands are blocked even when command names are allowed.\n",
        );
    }
    instructions.push_str(
        "- If a requested command is outside policy, choose allowed alternatives and explain the limitation.\n",
    );

    instructions
}

// ── CLI Entrypoint ───────────────────────────────────────────────────────
// Wires up all subsystems (observer, runtime, security, memory, tools,
// provider, hardware RAG, peripherals) and enters either single-shot or
// interactive REPL mode. The interactive loop manages history compaction
// and hard trimming to keep the context window bounded.

#[allow(clippy::too_many_lines)]
/// Run the agent loop with the given configuration.
///
/// When `hooks` is `Some`, the supplied [`HookRunner`](crate::hooks::HookRunner)
/// is invoked at every tool-call boundary (`before_tool_call` /
/// `on_after_tool_call`), enabling library consumers to inject safety,
/// audit, or transformation logic without patching the crate.
pub async fn run(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
    peripheral_overrides: Vec<String>,
    interactive: bool,
    hooks: Option<&crate::hooks::HookRunner>,
) -> Result<String> {
    if let Err(error) = crate::plugins::runtime::initialize_from_config(&config.plugins) {
        tracing::warn!("plugin registry initialization skipped: {error}");
    }

    // ── Wire up agnostic subsystems ──────────────────────────────
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

    // ── Memory (the brain) ────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage(
        &config.memory,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    tracing::info!(backend = mem.name(), "Memory initialized");

    // ── Peripherals (merge peripheral tools into registry) ─
    if !peripheral_overrides.is_empty() {
        tracing::info!(
            peripherals = ?peripheral_overrides,
            "Peripheral overrides from CLI (config boards take precedence)"
        );
    }

    // ── Tools (including memory tools and peripherals) ────────────
    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let mut tools_registry = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        mem.clone(),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    );

    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    if !peripheral_tools.is_empty() {
        tracing::info!(count = peripheral_tools.len(), "Peripheral tools added");
        tools_registry.extend(peripheral_tools);
    }

    // ── Resolve provider ─────────────────────────────────────────
    let provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter");

    let model_name = crate::config::resolve_default_model_id(
        model_override
            .as_deref()
            .or(config.default_model.as_deref()),
        Some(provider_name),
    );

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

    let provider: Box<dyn Provider> = providers::create_routed_provider_with_options(
        provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;

    observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
    });

    // ── Hardware RAG (datasheet retrieval when peripherals + datasheet_dir) ──
    let hardware_rag: Option<crate::rag::HardwareRag> = config
        .peripherals
        .datasheet_dir
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|dir| crate::rag::HardwareRag::load(&config.workspace_dir, dir.trim()))
        .and_then(Result::ok)
        .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
    if let Some(ref rag) = hardware_rag {
        tracing::info!(chunks = rag.len(), "Hardware RAG loaded");
    }

    let board_names: Vec<String> = config
        .peripherals
        .boards
        .iter()
        .map(|b| b.board.clone())
        .collect();

    // ── Build system prompt from workspace MD files (OpenClaw framework) ──
    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);
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
            "memory_observe",
            "Store observation memory. Use when: capturing patterns/signals that should remain searchable over long horizons.",
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
    tool_descs.push((
        "cron_add",
        "Create a cron job. Supports schedule kinds: cron, at, every; and job types: shell or agent.",
    ));
    tool_descs.push((
        "cron_list",
        "List all cron jobs with schedule, status, and metadata.",
    ));
    tool_descs.push(("cron_remove", "Remove a cron job by job_id."));
    tool_descs.push((
        "cron_update",
        "Patch a cron job (schedule, enabled, command/prompt, model, delivery, session_target).",
    ));
    tool_descs.push((
        "cron_run",
        "Force-run a cron job immediately and record a run history entry.",
    ));
    tool_descs.push(("cron_runs", "Show recent run history for a cron job."));
    tool_descs.push((
        "screenshot",
        "Capture a screenshot of the current screen. Returns file path and base64-encoded PNG. Use when: visual verification, UI inspection, debugging displays.",
    ));
    tool_descs.push((
        "image_info",
        "Read image file metadata (format, dimensions, size) and optionally base64-encode it. Use when: inspecting images, preparing visual data for analysis.",
    ));
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
            "Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). Use action='list' to discover, 'execute' to run (optionally with connected_account_id), 'connect' to OAuth.",
        ));
    }
    tool_descs.push((
        "schedule",
        "Manage scheduled tasks (create/list/get/cancel/pause/resume). Supports recurring cron and one-shot delays.",
    ));
    tool_descs.push((
        "model_routing_config",
        "Configure default model, scenario routing, and delegate agents. Use for natural-language requests like: 'set conversation to kimi and coding to gpt-5.3-codex'.",
    ));
    tool_descs.push((
        "web_search_config",
        "Configure web search providers/keys/fallbacks at runtime.",
    ));
    tool_descs.push((
        "web_access_config",
        "Configure shared URL access policy (first-visit approval, allowlist/blocklist, approved domains).",
    ));
    if !config.agents.is_empty() {
        tool_descs.push((
            "delegate",
            "Delegate a sub-task to a specialized agent. Use when: task needs different model/capability, or to parallelize work.",
        ));
    }
    if config.peripherals.enabled && !config.peripherals.boards.is_empty() {
        tool_descs.push((
            "gpio_read",
            "Read GPIO pin value (0 or 1) on connected hardware (STM32, Arduino). Use when: checking sensor/button state, LED status.",
        ));
        tool_descs.push((
            "gpio_write",
            "Set GPIO pin high (1) or low (0) on connected hardware. Use when: turning LED on/off, controlling actuators.",
        ));
        tool_descs.push((
            "arduino_upload",
            "Upload agent-generated Arduino sketch. Use when: user asks for 'make a heart', 'blink pattern', or custom LED behavior on Arduino. You write the full .ino code; ZeroClaw compiles and uploads it. Pin 13 = built-in LED on Uno.",
        ));
        tool_descs.push((
            "hardware_memory_map",
            "Return flash and RAM address ranges for connected hardware. Use when: user asks for 'upper and lower memory addresses', 'memory map', or 'readable addresses'.",
        ));
        tool_descs.push((
            "hardware_board_info",
            "Return full board info (chip, architecture, memory map) for connected hardware. Use when: user asks for 'board info', 'what board do I have', 'connected hardware', 'chip info', or 'what hardware'.",
        ));
        tool_descs.push((
            "hardware_memory_read",
            "Read actual memory/register values from Nucleo via USB. Use when: user asks to 'read register values', 'read memory', 'dump lower memory 0-126', 'give address and value'. Params: address (hex, default 0x20000000), length (bytes, default 128).",
        ));
        tool_descs.push((
            "hardware_capabilities",
            "Query connected hardware for reported GPIO pins and LED pin. Use when: user asks what pins are available.",
        ));
    }
    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = crate::channels::build_system_prompt_with_mode(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );

    // Append structured tool-use instructions with schemas (only for non-native providers)
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry));
    }
    system_prompt.push_str(&build_shell_policy_instructions(&config.autonomy));

    let configured_hooks = crate::hooks::create_runner_from_config(&config.hooks);
    let effective_hooks = hooks.or(configured_hooks.as_deref());

    // ── Approval manager (supervised mode) ───────────────────────
    let approval_manager = if interactive {
        Some(ApprovalManager::from_config(&config.autonomy))
    } else {
        None
    };
    let channel_name = if interactive { "cli" } else { "daemon" };

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();
    let cost_enforcement_context =
        create_cost_enforcement_context(&config.cost, &config.workspace_dir);

    let mut final_output = String::new();

    if let Some(msg) = message {
        // Auto-save user message to memory (skip short/trivial messages)
        if config.memory.auto_save && msg.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
            let user_key = autosave_memory_key("user_msg");
            let _ = mem
                .store(&user_key, &msg, MemoryCategory::Conversation, None)
                .await;
        }

        // Inject memory + hardware RAG context into user message
        let mem_context =
            build_context(mem.as_ref(), &msg, config.memory.min_relevance_score, None).await;
        let rag_limit = if config.agent.compact_context { 2 } else { 5 };
        let hw_context = hardware_rag
            .as_ref()
            .map(|r| build_hardware_context(r, &msg, &board_names, rag_limit))
            .unwrap_or_default();
        let context = format!("{mem_context}{hw_context}");
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = if context.is_empty() {
            format!("[{now}] {msg}")
        } else {
            format!("{context}[{now}] {msg}")
        };

        let mut history = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&enriched),
        ];

        let ld_cfg = LoopDetectionConfig {
            no_progress_threshold: config.agent.loop_detection_no_progress_threshold,
            ping_pong_cycles: config.agent.loop_detection_ping_pong_cycles,
            failure_streak_threshold: config.agent.loop_detection_failure_streak,
        };
        let hb_cfg = if config.agent.safety_heartbeat_interval > 0 {
            Some(SafetyHeartbeatConfig {
                body: security.summary_for_heartbeat(),
                interval: config.agent.safety_heartbeat_interval,
            })
        } else {
            None
        };
        let response = scope_cost_enforcement_context(
            cost_enforcement_context.clone(),
            SAFETY_HEARTBEAT_CONFIG.scope(
                hb_cfg,
                LOOP_DETECTION_CONFIG.scope(
                    ld_cfg,
                    run_tool_call_loop(
                        provider.as_ref(),
                        &mut history,
                        &tools_registry,
                        observer.as_ref(),
                        provider_name,
                        &model_name,
                        temperature,
                        false,
                        approval_manager.as_ref(),
                        channel_name,
                        &config.multimodal,
                        config.agent.max_tool_iterations,
                        None,
                        None,
                        effective_hooks,
                        &[],
                    ),
                ),
            ),
        )
        .await?;
        final_output = response.clone();
        if config.memory.auto_save && response.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
            let assistant_key = autosave_memory_key("assistant_resp");
            let _ = mem
                .store(
                    &assistant_key,
                    &response,
                    MemoryCategory::Conversation,
                    None,
                )
                .await;
        }
        println!("{response}");
        observer.record_event(&ObserverEvent::TurnComplete);
    } else {
        println!("🦀 ZeroClaw Interactive Mode");
        println!("Type /help for commands.\n");
        let cli = crate::channels::CliChannel::new();

        // Persistent conversation history across turns
        let mut history = vec![ChatMessage::system(&system_prompt)];
        let mut interactive_turn: usize = 0;
        // Reusable readline editor for UTF-8 input support
        let mut rl = Editor::with_config(
            RlConfig::builder()
                .completion_type(CompletionType::List)
                .build(),
        )?;
        rl.set_helper(Some(SlashCommandCompleter));

        loop {
            let input = match rl.readline("> ") {
                Ok(line) => line,
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                    break;
                }
                Err(e) => {
                    eprintln!("\nError reading input: {e}\n");
                    break;
                }
            };

            let user_input = input.trim().to_string();
            if user_input.is_empty() {
                continue;
            }
            rl.add_history_entry(&input)?;
            match user_input.as_str() {
                "/quit" | "/exit" => break,
                "/help" => {
                    println!("Available commands:");
                    println!("  /help        Show this help message");
                    println!("  /clear /new  Clear conversation history");
                    println!("  /quit /exit  Exit interactive mode\n");
                    continue;
                }
                "/clear" | "/new" => {
                    println!(
                        "This will clear the current conversation and delete all session memory."
                    );
                    println!("Core memories (long-term facts/preferences) will be preserved.");
                    let confirm = rl.readline("Continue? [y/N] ").unwrap_or_default();

                    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
                        println!("Cancelled.\n");
                        continue;
                    }

                    // Ensure prior prompts are not navigable after reset.
                    rl.clear_history()?;
                    history.clear();
                    history.push(ChatMessage::system(&system_prompt));
                    interactive_turn = 0;
                    // Clear conversation and daily memory
                    let mut cleared = 0;
                    for category in [MemoryCategory::Conversation, MemoryCategory::Daily] {
                        let entries = mem.list(Some(&category), None).await.unwrap_or_default();
                        for entry in entries {
                            if mem.forget(&entry.key).await.unwrap_or(false) {
                                cleared += 1;
                            }
                        }
                    }
                    if cleared > 0 {
                        println!("Conversation cleared ({cleared} memory entries removed).\n");
                    } else {
                        println!("Conversation cleared.\n");
                    }
                    continue;
                }
                _ => {}
            }

            // Auto-save conversation turns (skip short/trivial messages)
            if config.memory.auto_save && user_input.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
                let user_key = autosave_memory_key("user_msg");
                let _ = mem
                    .store(&user_key, &user_input, MemoryCategory::Conversation, None)
                    .await;
            }

            // Inject memory + hardware RAG context into user message
            let mem_context = build_context(
                mem.as_ref(),
                &user_input,
                config.memory.min_relevance_score,
                None,
            )
            .await;
            let rag_limit = if config.agent.compact_context { 2 } else { 5 };
            let hw_context = hardware_rag
                .as_ref()
                .map(|r| build_hardware_context(r, &user_input, &board_names, rag_limit))
                .unwrap_or_default();
            let context = format!("{mem_context}{hw_context}");
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
            let enriched = if context.is_empty() {
                format!("[{now}] {user_input}")
            } else {
                format!("{context}[{now}] {user_input}")
            };

            if let Some(system_message) = history.first_mut() {
                if system_message.role == "system" {
                    crate::agent::prompt::refresh_prompt_datetime(&mut system_message.content);
                }
            }

            history.push(ChatMessage::user(&enriched));
            interactive_turn += 1;

            // Inject interactive safety heartbeat at configured turn intervals
            if should_inject_safety_heartbeat(
                interactive_turn,
                config.agent.safety_heartbeat_turn_interval,
            ) {
                let reminder = format!(
                    "[Safety Heartbeat — turn {}]\n{}",
                    interactive_turn,
                    security.summary_for_heartbeat()
                );
                history.push(ChatMessage::user(reminder));
            }

            let ld_cfg = LoopDetectionConfig {
                no_progress_threshold: config.agent.loop_detection_no_progress_threshold,
                ping_pong_cycles: config.agent.loop_detection_ping_pong_cycles,
                failure_streak_threshold: config.agent.loop_detection_failure_streak,
            };
            let hb_cfg = if config.agent.safety_heartbeat_interval > 0 {
                Some(SafetyHeartbeatConfig {
                    body: security.summary_for_heartbeat(),
                    interval: config.agent.safety_heartbeat_interval,
                })
            } else {
                None
            };
            let response = match scope_cost_enforcement_context(
                cost_enforcement_context.clone(),
                SAFETY_HEARTBEAT_CONFIG.scope(
                    hb_cfg,
                    LOOP_DETECTION_CONFIG.scope(
                        ld_cfg,
                        run_tool_call_loop(
                            provider.as_ref(),
                            &mut history,
                            &tools_registry,
                            observer.as_ref(),
                            provider_name,
                            &model_name,
                            temperature,
                            false,
                            approval_manager.as_ref(),
                            channel_name,
                            &config.multimodal,
                            config.agent.max_tool_iterations,
                            None,
                            None,
                            effective_hooks,
                            &[],
                        ),
                    ),
                ),
            )
            .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    if is_tool_iteration_limit_error(&e) {
                        let pause_notice = format!(
                            "⚠️ Reached tool-iteration limit ({}). Context and progress are preserved. \
                            Reply \"continue\" to resume, or increase `agent.max_tool_iterations` in config.",
                            config.agent.max_tool_iterations.max(DEFAULT_MAX_TOOL_ITERATIONS)
                        );
                        history.push(ChatMessage::assistant(&pause_notice));
                        eprintln!("\n{pause_notice}\n");
                        continue;
                    }
                    if is_loop_detection_error(&e) {
                        let notice =
                            "\u{26a0}\u{fe0f} Loop pattern detected and agent stopped early. \
                            Context preserved. Reply \"continue\" to resume, or adjust \
                            loop_detection_* thresholds in config.";
                        history.push(ChatMessage::assistant(notice));
                        eprintln!("\n{notice}\n");
                        continue;
                    }
                    eprintln!("\nError: {e}\n");
                    continue;
                }
            };
            final_output = response.clone();
            if config.memory.auto_save && response.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
                let assistant_key = autosave_memory_key("assistant_resp");
                let _ = mem
                    .store(
                        &assistant_key,
                        &response,
                        MemoryCategory::Conversation,
                        None,
                    )
                    .await;
            }
            if let Err(e) = crate::channels::Channel::send(
                &cli,
                &crate::channels::traits::SendMessage::new(format!("\n{response}\n"), "user"),
            )
            .await
            {
                eprintln!("\nError sending CLI response: {e}\n");
            }
            observer.record_event(&ObserverEvent::TurnComplete);

            // Auto-compaction before hard trimming to preserve long-context signal.
            if let Ok(compacted) = auto_compact_history(
                &mut history,
                provider.as_ref(),
                &model_name,
                config.agent.max_history_messages,
                effective_hooks,
                Some(mem.as_ref()),
            )
            .await
            {
                if compacted {
                    println!("🧹 Auto-compaction complete");
                }
            }

            // Hard cap as a safety net.
            trim_history(&mut history, config.agent.max_history_messages);
        }
    }

    let duration = start.elapsed();
    observer.record_event(&ObserverEvent::AgentEnd {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
        duration,
        tokens_used: None,
        cost_usd: None,
    });

    Ok(final_output)
}

/// Process a single message through the full agent (with tools, peripherals, memory).
/// Used by channels (Telegram, Discord, etc.) to enable hardware and tool use.
pub async fn process_message(config: Config, message: &str) -> Result<String> {
    process_message_with_session(config, message, None).await
}

pub async fn process_message_with_session(
    config: Config,
    message: &str,
    session_id: Option<&str>,
) -> Result<String> {
    if let Err(error) = crate::plugins::runtime::initialize_from_config(&config.plugins) {
        tracing::warn!("plugin registry initialization skipped: {error}");
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
    let mut tools_registry = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        mem.clone(),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    );
    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    tools_registry.extend(peripheral_tools);

    let provider_name = config.default_provider.as_deref().unwrap_or("openrouter");
    let model_name = crate::config::resolve_default_model_id(
        config.default_model.as_deref(),
        Some(provider_name),
    );
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
    let provider: Box<dyn Provider> = providers::create_routed_provider_with_options(
        provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;

    let hardware_rag: Option<crate::rag::HardwareRag> = config
        .peripherals
        .datasheet_dir
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|dir| crate::rag::HardwareRag::load(&config.workspace_dir, dir.trim()))
        .and_then(Result::ok)
        .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
    let board_names: Vec<String> = config
        .peripherals
        .boards
        .iter()
        .map(|b| b.board.clone())
        .collect();

    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);
    let mut tool_descs: Vec<(&str, &str)> = vec![
        ("shell", "Execute terminal commands."),
        ("file_read", "Read file contents."),
        ("file_write", "Write file contents."),
        ("memory_store", "Save to memory."),
        ("memory_observe", "Store observation memory."),
        ("memory_recall", "Search memory."),
        ("memory_forget", "Delete a memory entry."),
        (
            "model_routing_config",
            "Configure default model, scenario routing, and delegate agents.",
        ),
        (
            "web_search_config",
            "Configure web search providers/keys/fallbacks.",
        ),
        (
            "web_access_config",
            "Configure shared URL access policy for network tools.",
        ),
        ("screenshot", "Capture a screenshot."),
        ("image_info", "Read image metadata."),
    ];
    if config.browser.enabled {
        tool_descs.push(("browser_open", "Open approved URLs in browser."));
        tool_descs.push(("browser", "Automate browser interactions."));
    }
    if config.composio.enabled {
        tool_descs.push(("composio", "Execute actions on 1000+ apps via Composio."));
    }
    if config.peripherals.enabled && !config.peripherals.boards.is_empty() {
        tool_descs.push(("gpio_read", "Read GPIO pin value on connected hardware."));
        tool_descs.push((
            "gpio_write",
            "Set GPIO pin high or low on connected hardware.",
        ));
        tool_descs.push((
            "arduino_upload",
            "Upload Arduino sketch. Use for 'make a heart', custom patterns. You write full .ino code; ZeroClaw uploads it.",
        ));
        tool_descs.push((
            "hardware_memory_map",
            "Return flash and RAM address ranges. Use when user asks for memory addresses or memory map.",
        ));
        tool_descs.push((
            "hardware_board_info",
            "Return full board info (chip, architecture, memory map). Use when user asks for board info, what board, connected hardware, or chip info.",
        ));
        tool_descs.push((
            "hardware_memory_read",
            "Read actual memory/register values from Nucleo. Use when user asks to read registers, read memory, dump lower memory 0-126, or give address and value.",
        ));
        tool_descs.push((
            "hardware_capabilities",
            "Query connected hardware for reported GPIO pins and LED pin. Use when user asks what pins are available.",
        ));
    }
    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = crate::channels::build_system_prompt_with_mode(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry));
    }
    system_prompt.push_str(&build_shell_policy_instructions(&config.autonomy));

    let mem_context = build_context(
        mem.as_ref(),
        message,
        config.memory.min_relevance_score,
        session_id,
    )
    .await;
    let rag_limit = if config.agent.compact_context { 2 } else { 5 };
    let hw_context = hardware_rag
        .as_ref()
        .map(|r| build_hardware_context(r, message, &board_names, rag_limit))
        .unwrap_or_default();
    let context = format!("{mem_context}{hw_context}");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let enriched = if context.is_empty() {
        format!("[{now}] {message}")
    } else {
        format!("{context}[{now}] {message}")
    };

    let mut history = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user(&enriched),
    ];

    let cost_enforcement_context =
        create_cost_enforcement_context(&config.cost, &config.workspace_dir);
    let hb_cfg = if config.agent.safety_heartbeat_interval > 0 {
        Some(SafetyHeartbeatConfig {
            body: security.summary_for_heartbeat(),
            interval: config.agent.safety_heartbeat_interval,
        })
    } else {
        None
    };
    scope_cost_enforcement_context(
        cost_enforcement_context,
        SAFETY_HEARTBEAT_CONFIG.scope(
            hb_cfg,
            agent_turn(
                provider.as_ref(),
                &mut history,
                &tools_registry,
                observer.as_ref(),
                provider_name,
                &model_name,
                config.default_temperature,
                true,
                &config.multimodal,
                config.agent.max_tool_iterations,
            ),
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn test_scrub_credentials() {
        let input = "API_KEY=sk-1234567890abcdef; token: 1234567890; password=\"secret123456\"";
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("API_KEY=sk-1*[REDACTED]"));
        assert!(scrubbed.contains("token: 1234*[REDACTED]"));
        assert!(scrubbed.contains("password=\"secr*[REDACTED]\""));
        assert!(!scrubbed.contains("abcdef"));
        assert!(!scrubbed.contains("secret123456"));
    }

    #[test]
    fn test_scrub_credentials_json() {
        let input = r#"{"api_key": "sk-1234567890", "other": "public"}"#;
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("\"api_key\": \"sk-1*[REDACTED]\""));
        assert!(scrubbed.contains("public"));
    }

    #[test]
    fn maybe_inject_cron_add_delivery_populates_agent_delivery_from_channel_context() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "prompt": "remind me later"
        });

        maybe_inject_cron_add_delivery("cron_add", &mut args, "telegram", Some("-10012345"));

        assert_eq!(args["delivery"]["mode"], "announce");
        assert_eq!(args["delivery"]["channel"], "telegram");
        assert_eq!(args["delivery"]["to"], "-10012345");
    }

    #[test]
    fn maybe_inject_cron_add_delivery_does_not_override_explicit_target() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "prompt": "remind me later",
            "delivery": {
                "mode": "announce",
                "channel": "discord",
                "to": "C123"
            }
        });

        maybe_inject_cron_add_delivery("cron_add", &mut args, "telegram", Some("-10012345"));

        assert_eq!(args["delivery"]["channel"], "discord");
        assert_eq!(args["delivery"]["to"], "C123");
    }

    #[test]
    fn maybe_inject_cron_add_delivery_skips_shell_jobs() {
        let mut args = serde_json::json!({
            "job_type": "shell",
            "command": "echo hello"
        });

        maybe_inject_cron_add_delivery("cron_add", &mut args, "telegram", Some("-10012345"));

        assert!(args.get("delivery").is_none());
    }

    #[test]
    fn maybe_inject_cron_add_delivery_supports_lark_and_feishu_channels() {
        let mut lark_args = serde_json::json!({
            "job_type": "agent",
            "prompt": "daily summary"
        });
        maybe_inject_cron_add_delivery("cron_add", &mut lark_args, "lark", Some("oc_xxx"));
        assert_eq!(lark_args["delivery"]["channel"], "lark");
        assert_eq!(lark_args["delivery"]["to"], "oc_xxx");

        let mut feishu_args = serde_json::json!({
            "job_type": "agent",
            "prompt": "daily summary"
        });
        maybe_inject_cron_add_delivery("cron_add", &mut feishu_args, "feishu", Some("oc_yyy"));
        assert_eq!(feishu_args["delivery"]["channel"], "feishu");
        assert_eq!(feishu_args["delivery"]["to"], "oc_yyy");
    }

    #[test]
    fn safety_heartbeat_interval_zero_disables_injection() {
        for counter in [0, 1, 2, 10, 100] {
            assert!(
                !should_inject_safety_heartbeat(counter, 0),
                "counter={counter} should not inject when interval=0"
            );
        }
    }

    #[test]
    fn safety_heartbeat_interval_one_injects_every_non_initial_step() {
        assert!(!should_inject_safety_heartbeat(0, 1));
        for counter in 1..=6 {
            assert!(
                should_inject_safety_heartbeat(counter, 1),
                "counter={counter} should inject when interval=1"
            );
        }
    }

    #[test]
    fn safety_heartbeat_injects_only_on_exact_multiples() {
        let interval = 3;
        let injected: Vec<usize> = (0..=10)
            .filter(|counter| should_inject_safety_heartbeat(*counter, interval))
            .collect();
        assert_eq!(injected, vec![3, 6, 9]);
    }

    use crate::memory::{Memory, MemoryCategory, SqliteMemory};
    use crate::observability::NoopObserver;
    use crate::providers::traits::ProviderCapabilities;
    use crate::providers::ChatResponse;
    use tempfile::TempDir;

    struct NonVisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for NonVisionProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    struct VisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for VisionProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: true,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let marker_count = crate::multimodal::count_image_markers(request.messages);
            if marker_count == 0 {
                anyhow::bail!("expected image markers in request messages");
            }

            if request.tools.is_some() {
                anyhow::bail!("no tools should be attached for this test");
            }

            Ok(ChatResponse {
                text: Some("vision-ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            })
        }
    }

    struct ScriptedProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        capabilities: ProviderCapabilities,
    }

    impl ScriptedProvider {
        fn from_scripted_responses(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(VecDeque::from(responses))),
                capabilities: ProviderCapabilities::default(),
            }
        }

        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
                .collect();
            Self::from_scripted_responses(scripted)
        }

        fn with_native_tool_support(mut self) -> Self {
            self.capabilities.native_tool_calling = true;
            self
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in scripted provider tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted provider exhausted responses"))
        }
    }

    struct CountingTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Counts executions for loop-stability tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("counted:{value}"),
                error: None,
            })
        }
    }

    struct DelayTool {
        name: String,
        delay_ms: u64,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl DelayTool {
        fn new(
            name: &str,
            delay_ms: u64,
            active: Arc<AtomicUsize>,
            max_active: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                name: name.to_string(),
                delay_ms,
                active,
                max_active,
            }
        }
    }

    struct FailingTool;

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "failing_tool"
        }

        fn description(&self) -> &str {
            "Fails deterministically for error-propagation tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: String::new(),
                error: Some("boom".to_string()),
            })
        }
    }

    struct ErrorCaptureHook {
        seen_errors: Arc<Mutex<Vec<Option<String>>>>,
    }

    #[async_trait]
    impl crate::hooks::HookHandler for ErrorCaptureHook {
        fn name(&self) -> &str {
            "error-capture"
        }

        async fn on_after_tool_call(
            &self,
            _tool: &str,
            result: &crate::tools::ToolResult,
            _duration: Duration,
        ) {
            self.seen_errors
                .lock()
                .expect("hook error buffer lock should be valid")
                .push(result.error.clone());
        }
    }

    #[async_trait]
    impl Tool for DelayTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Delay tool for testing parallel tool execution"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"]
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now_active, Ordering::SeqCst);

            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;

            self.active.fetch_sub(1, Ordering::SeqCst);

            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();

            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("ok:{value}"),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_structured_error_for_non_vision_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("provider without vision support should fail");

        assert!(err.to_string().contains("provider_capability_error"));
        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_allows_anthropic_route_on_vision_probe_false_negative() {
        let provider = ScriptedProvider::from_text_responses(vec!["vision-ok"]);
        let mut history = vec![ChatMessage::user(
            "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "anthropic",
            "opus-4-6",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("anthropic route should not fail on a false-negative vision capability probe");

        assert_eq!(result, "vision-ok");
    }

    #[tokio::test]
    async fn run_tool_call_loop_rejects_oversized_image_payload() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let oversized_payload = STANDARD.encode(vec![0_u8; (1024 * 1024) + 1]);
        let mut history = vec![ChatMessage::user(format!(
            "[IMAGE:data:image/png;base64,{oversized_payload}]"
        ))];

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;
        let multimodal = crate::config::MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 1,
            allow_remote_fetch: false,
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("oversized payload must fail");

        assert!(err
            .to_string()
            .contains("multimodal image size limit exceeded"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_valid_multimodal_request_flow() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "Analyze this [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("valid multimodal payload should pass");

        assert_eq!(result, "vision-ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_for_single_call() {
        let calls = vec![ParsedToolCall {
            name: "file_read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
            tool_call_id: None,
        }];

        assert!(!should_execute_tools_in_parallel(&calls, None));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_when_approval_is_required() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig::default();
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(!should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_true_when_cli_has_no_interactive_approvals() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_multiple_tools_with_ordered_results() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"delay_a","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"delay_b","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(DelayTool::new(
                "delay_a",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
            Box::new(DelayTool::new(
                "delay_b",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
        ];

        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("parallel execution should complete");

        assert_eq!(result, "done");
        assert!(
            max_active.load(Ordering::SeqCst) >= 1,
            "tools should execute successfully"
        );

        let tool_results_message = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        let idx_a = tool_results_message
            .content
            .find("name=\"delay_a\"")
            .expect("delay_a result should be present");
        let idx_b = tool_results_message
            .content
            .find("name=\"delay_b\"")
            .expect("delay_b result should be present");
        assert!(
            idx_a < idx_b,
            "tool results should preserve input order for tool call mapping"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_denies_supervised_tools_on_non_cli_channels() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = ApprovalManager::from_config(&crate::config::AutonomyConfig::default());

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("tool loop should complete with denied tool execution");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            0,
            "shell tool must not execute when approval is unavailable on non-CLI channels"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_uses_non_cli_session_grant_without_waiting_for_prompt() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = ApprovalManager::from_config(&crate::config::AutonomyConfig::default());
        approval_mgr.grant_non_cli_session("shell");

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("tool loop should consume non-cli session grants");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "shell tool should execute when runtime non-cli session grant exists"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_waits_for_non_cli_approval_resolution() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = Arc::new(ApprovalManager::from_config(
            &crate::config::AutonomyConfig::default(),
        ));
        let (prompt_tx, mut prompt_rx) =
            tokio::sync::mpsc::unbounded_channel::<NonCliApprovalPrompt>();
        let approval_mgr_for_task = Arc::clone(&approval_mgr);
        let approval_task = tokio::spawn(async move {
            let prompt = prompt_rx
                .recv()
                .await
                .expect("approval prompt should arrive");
            approval_mgr_for_task
                .confirm_non_cli_pending_request(
                    &prompt.request_id,
                    "alice",
                    "telegram",
                    "chat-approval",
                )
                .expect("pending approval should confirm");
            approval_mgr_for_task
                .record_non_cli_pending_resolution(&prompt.request_id, ApprovalResponse::Yes);
        });

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop_with_non_cli_approval_context(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(approval_mgr.as_ref()),
            "telegram",
            Some(NonCliApprovalContext {
                sender: "alice".to_string(),
                reply_target: "chat-approval".to_string(),
                prompt_tx,
            }),
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            ProgressMode::Verbose,
            None,
        )
        .await
        .expect("tool loop should continue after non-cli approval");

        approval_task.await.expect("approval task should complete");
        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "shell tool should execute after non-cli approval is resolved"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_consumes_one_time_non_cli_allow_all_token() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = ApprovalManager::from_config(&crate::config::AutonomyConfig::default());
        approval_mgr.grant_non_cli_allow_all_once();
        assert_eq!(approval_mgr.non_cli_allow_all_once_remaining(), 1);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell once"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("tool loop should consume one-time allow-all token");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "shell tool should execute after consuming one-time allow-all token"
        );
        assert_eq!(approval_mgr.non_cli_allow_all_once_remaining(), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_blocks_tools_excluded_for_channel() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;
        let excluded_tools = vec!["shell".to_string()];

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &excluded_tools,
        )
        .await
        .expect("tool loop should complete with blocked tool execution");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            0,
            "excluded tool must not execute even if the model requests it"
        );

        let tool_results_message = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        assert!(
            tool_results_message
                .content
                .contains("not available in this channel"),
            "blocked reason should be visible to the model"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_deduplicates_repeated_tool_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should finish after deduplicating repeated calls");

        assert_eq!(result, "done");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate tool call with same args should not execute twice"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(tool_results.content.contains("counted:A"));
        assert!(tool_results.content.contains("Skipped duplicate tool call"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_native_mode_preserves_fallback_tool_call_ids() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"content":"Need to call tool","tool_calls":[{"id":"call_abc","name":"count_tool","arguments":"{\"value\":\"X\"}"}]}"#,
            "done",
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("native fallback id flow should complete");

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_abc\"")
            }),
            "tool result should preserve parsed fallback tool_call_id in native mode"
        );
        assert!(
            history
                .iter()
                .all(|msg| !(msg.role == "user" && msg.content.starts_with("[Tool results]"))),
            "native mode should use role=tool history instead of prompt fallback wrapper"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_once_when_response_defers_action_without_tool_call() {
        let provider = ScriptedProvider::from_text_responses(vec![
            "I'll check that right away.",
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"retry"}}
</tool_call>"#,
            "done after tool",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("please check the workspace"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should recover after one deferred-action reply");

        assert_eq!(result, "done after tool");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the fallback retry should lead to an actual tool execution"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_errors_when_deferred_action_repeats_without_tool_call() {
        let provider = ScriptedProvider::from_text_responses(vec![
            "I'll check that right away.",
            "Let me inspect that in detail now.",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("please check the workspace"),
        ];
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("second deferred response without tool call should hard-fail");

        let err_text = err.to_string();
        assert!(
            err_text.contains("deferred action without emitting a tool call"),
            "unexpected error text: {err_text}"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "tool should not execute when model never emits a tool call"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_when_native_tool_args_are_truncated_json() {
        let provider = ScriptedProvider::from_scripted_responses(vec![
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "call_bad".to_string(),
                    name: "count_tool".to_string(),
                    arguments: "{\"value\":\"truncated\"".to_string(),
                }],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "call_good".to_string(),
                    name: "count_tool".to_string(),
                    arguments: "{\"value\":\"fixed\"}".to_string(),
                }],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::ToolCall),
                raw_stop_reason: Some("tool_calls".to_string()),
            },
            ChatResponse {
                text: Some("done after native retry".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::EndTurn),
                raw_stop_reason: Some("stop".to_string()),
            },
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run native call"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("truncated native arguments should trigger safe retry");

        assert_eq!(result, "done after native retry");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "only the repaired native tool call should execute"
        );
        assert!(
            history.iter().any(|msg| {
                msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_good\"")
            }),
            "tool history should include only the repaired tool_call_id"
        );
        assert!(
            history.iter().all(|msg| {
                !(msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_bad\""))
            }),
            "invalid truncated native call must not execute"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_ignores_text_fallback_when_native_tool_args_are_truncated_json() {
        let provider = ScriptedProvider::from_scripted_responses(vec![
            ChatResponse {
                text: Some(
                    r#"<tool_call>
{"name":"count_tool","arguments":{"value":"from_text_fallback"}}
</tool_call>"#
                        .to_string(),
                ),
                tool_calls: vec![ToolCall {
                    id: "call_bad".to_string(),
                    name: "count_tool".to_string(),
                    arguments: "{\"value\":\"truncated\"".to_string(),
                }],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "call_good".to_string(),
                    name: "count_tool".to_string(),
                    arguments: "{\"value\":\"from_native_fixed\"}".to_string(),
                }],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::ToolCall),
                raw_stop_reason: Some("tool_calls".to_string()),
            },
            ChatResponse {
                text: Some("done after safe retry".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::EndTurn),
                raw_stop_reason: Some("stop".to_string()),
            },
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run native call"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("invalid native args should force retry without text fallback execution");

        assert_eq!(result, "done after safe retry");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "only repaired native call should execute after retry"
        );
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("counted:from_text_fallback")),
            "text fallback tool call must not execute when native JSON args are invalid"
        );
        assert!(
            history
                .iter()
                .any(|msg| msg.content.contains("counted:from_native_fixed")),
            "repaired native call should execute after retry"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_valid_native_tool_call_with_max_tokens_stop_reason() {
        let provider = ScriptedProvider::from_scripted_responses(vec![
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "call_valid".to_string(),
                    name: "count_tool".to_string(),
                    arguments: "{\"value\":\"from_valid_native\"}".to_string(),
                }],
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some("done after valid native tool".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::EndTurn),
                raw_stop_reason: Some("stop".to_string()),
            },
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run native call"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("valid native tool calls must execute even when stop_reason is max_tokens");

        assert_eq!(result, "done after valid native tool");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "valid native tool call should execute exactly once"
        );
        assert!(
            history.iter().any(|msg| {
                msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_valid\"")
            }),
            "tool history should preserve valid native tool_call_id"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_continues_when_stop_reason_is_max_tokens() {
        let provider = ScriptedProvider::from_scripted_responses(vec![
            ChatResponse {
                text: Some("part 1 ".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some("part 2".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::EndTurn),
                raw_stop_reason: Some("stop".to_string()),
            },
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("continue this"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("max-token continuation should complete");

        assert_eq!(result, "part 1 part 2");
        assert!(
            !result.contains("Response may be truncated"),
            "continuation should not emit truncation notice when it ends cleanly"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_appends_notice_when_continuation_budget_exhausts() {
        let provider = ScriptedProvider::from_scripted_responses(vec![
            ChatResponse {
                text: Some("A".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some("B".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some("C".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some("D".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("long output"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("continuation should degrade to partial output");

        assert!(result.starts_with("ABCD"));
        assert!(
            result.contains("Response may be truncated due to continuation limits"),
            "result should include truncation notice when continuation cap is hit"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_clamps_continuation_output_to_hard_cap() {
        let oversized_chunk = "B".repeat(MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS);
        let provider = ScriptedProvider::from_scripted_responses(vec![
            ChatResponse {
                text: Some("A".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::MaxTokens),
                raw_stop_reason: Some("length".to_string()),
            },
            ChatResponse {
                text: Some(oversized_chunk),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: Some(NormalizedStopReason::EndTurn),
                raw_stop_reason: Some("stop".to_string()),
            },
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("long output"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("continuation should clamp oversized merge");

        assert!(
            result.ends_with(MAX_TOKENS_CONTINUATION_NOTICE),
            "hard-cap truncation should append continuation notice"
        );
        let capped_output = result
            .strip_suffix(MAX_TOKENS_CONTINUATION_NOTICE)
            .expect("result should end with continuation notice");
        assert_eq!(
            capped_output.chars().count(),
            MAX_TOKENS_CONTINUATION_MAX_OUTPUT_CHARS
        );
        assert!(
            capped_output.starts_with('A'),
            "capped output should preserve earlier text before continuation chunk"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_preserves_failed_tool_error_for_after_hook() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"failing_tool","arguments":{}}
</tool_call>"#,
            "done",
        ]);
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(FailingTool)];
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run failing tool"),
        ];

        let seen_errors = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = crate::hooks::HookRunner::new();
        hooks.register(Box::new(ErrorCaptureHook {
            seen_errors: Arc::clone(&seen_errors),
        }));

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            Some(&hooks),
            &[],
        )
        .await
        .expect("loop should complete");

        assert_eq!(result, "done");
        let recorded = seen_errors
            .lock()
            .expect("hook error buffer lock should be valid");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].as_deref(), Some("boom"));
    }

    #[test]
    fn merge_continuation_text_deduplicates_partial_overlap() {
        let merged = merge_continuation_text("The result is wor", "world.");
        assert_eq!(merged, "The result is world.");
    }

    #[test]
    fn merge_continuation_text_handles_unicode_overlap() {
        let merged = merge_continuation_text("你好世界", "世界和平");
        assert_eq!(merged, "你好世界和平");
    }

    #[test]
    fn parse_tool_calls_extracts_single_call() {
        let response = r#"Let me check that.
<tool_call>
{"name": "shell", "arguments": {"command": "ls -la"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_extracts_multiple_calls() {
        let response = r#"<tool_call>
{"name": "file_read", "arguments": {"path": "a.txt"}}
</tool_call>
<tool_call>
{"name": "file_read", "arguments": {"path": "b.txt"}}
</tool_call>"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_returns_text_only_when_no_calls() {
        let response = "Just a normal response with no tools.";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Just a normal response with no tools.");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_malformed_json() {
        let response = r#"<tool_call>
not valid json
</tool_call>
Some text after."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.contains("Some text after."));
    }

    #[test]
    fn parse_tool_calls_text_before_and_after() {
        let response = r#"Before text.
<tool_call>
{"name": "shell", "arguments": {"command": "echo hi"}}
</tool_call>
After text."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Before text."));
        assert!(text.contains("After text."));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn parse_tool_calls_handles_openai_format() {
        // OpenAI-style response with tool_calls array
        let response = r#"{"content": "Let me check that for you.", "tool_calls": [{"type": "function", "function": {"name": "shell", "arguments": "{\"command\": \"ls -la\"}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that for you.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_openai_format_multiple_calls() {
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"a.txt\"}"}}, {"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"b.txt\"}"}}]}"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_openai_format_without_content() {
        // Some providers don't include content field with tool_calls
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "memory_recall", "arguments": "{}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty()); // No content field
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
    }

    #[test]
    fn parse_tool_calls_handles_openai_message_wrapper_with_content() {
        let response = r#"{
            "message": {
                "role": "assistant",
                "content": "<think>plan</think>\nI will call a tool.",
                "tool_calls": [
                    {
                        "id": "chatcmpl-tool-a18c01b8849eb05d",
                        "type": "function",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\": \"ls -la\"}"
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
        assert!(text.contains("I will call a tool."));
    }

    #[test]
    fn parse_tool_calls_handles_openai_choices_message_wrapper() {
        let response = r#"{
            "id": "chatcmpl-123",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Checking now.",
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "shell",
                                    "arguments": "{\"command\":\"pwd\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ]
        }"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Checking now.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn parse_tool_calls_preserves_openai_tool_call_ids() {
        let response = r#"{"tool_calls":[{"id":"call_42","function":{"name":"shell","arguments":"{\"command\":\"pwd\"}"}}]}"#;
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("call_42"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_json_inside_tool_call_tag() {
        let response = r#"<tool_call>
```json
{"name": "file_write", "arguments": {"path": "test.py", "content": "print('ok')"}}
```
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "test.py"
        );
    }

    #[test]
    fn parse_tool_calls_handles_noisy_tool_call_tag_body() {
        let response = r#"<tool_call>
I will now call the tool with this payload:
{"name": "shell", "arguments": {"command": "pwd"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_inline_attributes_with_send_message_alias() {
        let response = r#"<tool_call>send_message channel="user_channel" message="Hello! How can I assist you today?"</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "user_channel"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "Hello! How can I assist you today?"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_function_style_arguments() {
        let response = r#"<tool_call>message_send(channel="general", message="test")</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "general"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "test"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_nested_tool_payload() {
        let response = r#"<tool_call>
<memory_recall>
<query>project roadmap</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "project roadmap"
        );
    }

    #[test]
    fn parse_tool_calls_ignores_xml_thinking_wrapper() {
        let response = r#"<tool_call>
<thinking>Need to inspect memory first</thinking>
<memory_recall>
<query>recent deploy notes</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "recent deploy notes"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_with_json_arguments() {
        let response = r#"<tool_call>
<shell>{"command":"pwd"}</shell>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_direct_xml_tool_tags_without_wrapper() {
        let response = r#"<shell>dir "C:\Users\u\Desktop" /b</shell>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            r#"dir "C:\Users\u\Desktop" /b"#
        );
    }

    #[test]
    fn parse_tool_calls_handles_mixed_direct_xml_blocks_with_guidance_text() {
        let response = r#"
// check desktop images
<shell>dir "C:\Users\u\Desktop" /b | findstr /i "\.jpg$ \.png$"</shell>

// create a file
<file_write>
<path>C:\Users\u\Desktop\new_file.txt</path>
<content></content>
</file_write>
"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            r#"dir "C:\Users\u\Desktop" /b | findstr /i "\.jpg$ \.png$""#
        );
        assert_eq!(calls[1].name, "file_write");
        assert_eq!(
            calls[1].arguments.get("path").unwrap().as_str().unwrap(),
            r#"C:\Users\u\Desktop\new_file.txt"#
        );
        assert!(text.contains("check desktop images"));
        assert!(text.contains("create a file"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_fence() {
        let response = r#"I'll check that.
```tool_call
{"name": "shell", "arguments": {"command": "pwd"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
        assert!(text.contains("I'll check that."));
        assert!(text.contains("Done."));
        assert!(!text.contains("```tool_call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_hybrid_close_tag() {
        let response = r#"Preface
```tool-call
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert!(!text.contains("```tool-call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_invoke_fence() {
        let response = r#"Checking.
```invoke
{"name": "shell", "arguments": {"command": "date"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Checking."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_format() {
        // Issue #1420: xAI grok models use ```tool <name> format
        let response = r#"I'll write a test file.
```tool file_write
{"path": "/home/user/test.txt", "content": "Hello world"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "/home/user/test.txt"
        );
        assert!(text.contains("I'll write a test file."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_shell() {
        // Issue #1420: Test shell command in ```tool shell format
        let response = r#"```tool shell
{"command": "ls -la"}
```"#;

        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_multiple_tool_name_fences() {
        // Multiple tool calls in ```tool <name> format
        let response = r#"First, I'll write a file.
```tool file_write
{"path": "/tmp/a.txt", "content": "A"}
```
Then read it.
```tool file_read
{"path": "/tmp/a.txt"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(calls[1].name, "file_read");
        assert!(text.contains("First, I'll write a file."));
        assert!(text.contains("Then read it."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_toolcall_tag_alias() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_dash_call_tag_alias() {
        let response = r#"<tool-call>
{"name": "shell", "arguments": {"command": "whoami"}}
</tool-call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "whoami"
        );
    }

    #[test]
    fn parse_tool_calls_handles_invoke_tag_alias() {
        let response = r#"<invoke>
{"name": "shell", "arguments": {"command": "uptime"}}
</invoke>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_parameter_format() {
        let response = r#"<minimax:tool_call>
<invoke name="shell">
<parameter name="command">sqlite3 /tmp/test.db ".tables"</parameter>
</invoke>
</minimax:tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            r#"sqlite3 /tmp/test.db ".tables""#
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_with_surrounding_text() {
        let response = r#"Preface
<minimax:tool_call>
<invoke name='http_request'>
<parameter name='url'>https://example.com</parameter>
<parameter name='method'>GET</parameter>
</invoke>
</minimax:tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "http_request");
        assert_eq!(
            calls[0].arguments.get("url").unwrap().as_str().unwrap(),
            "https://example.com"
        );
        assert_eq!(
            calls[0].arguments.get("method").unwrap().as_str().unwrap(),
            "GET"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_toolcall_alias_and_cross_close_tag() {
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"date"}}
</minimax:toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_perl_style_tool_call_blocks() {
        let response = r#"TOOL_CALL
{tool => "shell", args => { --command "uname -a" }}}
/TOOL_CALL"#;

        let calls = parse_perl_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uname -a"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_unclosed_tool_call_with_json() {
        let response = r#"I will call the tool now.
<tool_call>
{"name": "shell", "arguments": {"command": "uptime -p"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I will call the tool now."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime -p"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_mismatched_close_tag() {
        let response = r#"<tool_call>
{"name": "shell", "arguments": {"command": "uptime"}}
</arg_value>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_cross_alias_closing_tags() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn parse_tool_calls_rejects_raw_tool_json_without_tags() {
        // SECURITY: Raw JSON without explicit wrappers should NOT be parsed
        // This prevents prompt injection attacks where malicious content
        // could include JSON that mimics a tool call.
        let response = r#"Sure, creating the file now.
{"name": "file_write", "arguments": {"path": "hello.py", "content": "print('hello')"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Sure, creating the file now."));
        assert_eq!(
            calls.len(),
            0,
            "Raw JSON without wrappers should not be parsed"
        );
    }

    #[test]
    fn build_tool_instructions_includes_all_tools() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let instructions = build_tool_instructions(&tools);

        assert!(instructions.contains("## Tool Use Protocol"));
        assert!(instructions.contains("<tool_call>"));
        assert!(instructions.contains("shell"));
        assert!(instructions.contains("file_read"));
        assert!(instructions.contains("file_write"));
    }

    #[test]
    fn build_shell_policy_instructions_lists_allowlist() {
        let mut autonomy = crate::config::AutonomyConfig::default();
        autonomy.level = crate::security::AutonomyLevel::Supervised;
        autonomy.allowed_commands = vec!["grep".into(), "cat".into(), "grep".into()];

        let instructions = build_shell_policy_instructions(&autonomy);

        assert!(instructions.contains("## Shell Policy"));
        assert!(instructions.contains("Autonomy level: `supervised`"));
        assert!(instructions.contains("`cat`"));
        assert!(instructions.contains("`grep`"));
    }

    #[test]
    fn build_shell_policy_instructions_handles_wildcard() {
        let mut autonomy = crate::config::AutonomyConfig::default();
        autonomy.level = crate::security::AutonomyLevel::Full;
        autonomy.allowed_commands = vec!["*".into()];

        let instructions = build_shell_policy_instructions(&autonomy);

        assert!(instructions.contains("Autonomy level: `full`"));
        assert!(instructions.contains("wildcard `*`"));
    }

    #[test]
    fn build_shell_policy_instructions_read_only_disables_shell() {
        let mut autonomy = crate::config::AutonomyConfig::default();
        autonomy.level = crate::security::AutonomyLevel::ReadOnly;

        let instructions = build_shell_policy_instructions(&autonomy);

        assert!(instructions.contains("Autonomy level: `read_only`"));
        assert!(instructions.contains("Shell execution is disabled"));
    }

    #[test]
    fn tools_to_openai_format_produces_valid_schema() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let formatted = tools_to_openai_format(&tools);

        assert!(!formatted.is_empty());
        for tool_json in &formatted {
            assert_eq!(tool_json["type"], "function");
            assert!(tool_json["function"]["name"].is_string());
            assert!(tool_json["function"]["description"].is_string());
            assert!(!tool_json["function"]["name"].as_str().unwrap().is_empty());
        }
        // Verify known tools are present
        let names: Vec<&str> = formatted
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"file_read"));
    }

    #[test]
    fn trim_history_preserves_system_prompt() {
        let mut history = vec![ChatMessage::system("system prompt")];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 20 {
            history.push(ChatMessage::user(format!("msg {i}")));
        }
        let original_len = history.len();
        assert!(original_len > DEFAULT_MAX_HISTORY_MESSAGES + 1);

        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);

        // System prompt preserved
        assert_eq!(history[0].role, "system");
        assert_eq!(history[0].content, "system prompt");
        // Trimmed to limit
        assert_eq!(history.len(), DEFAULT_MAX_HISTORY_MESSAGES + 1); // +1 for system
                                                                     // Most recent messages preserved
        let last = &history[history.len() - 1];
        assert_eq!(
            last.content,
            format!("msg {}", DEFAULT_MAX_HISTORY_MESSAGES + 19)
        );
    }

    #[test]
    fn trim_history_noop_when_within_limit() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn build_compaction_transcript_formats_roles() {
        let messages = vec![
            ChatMessage::user("I like dark mode"),
            ChatMessage::assistant("Got it"),
        ];
        let transcript = build_compaction_transcript(&messages);
        assert!(transcript.contains("USER: I like dark mode"));
        assert!(transcript.contains("ASSISTANT: Got it"));
    }

    #[test]
    fn apply_compaction_summary_replaces_old_segment() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old 1"),
            ChatMessage::assistant("old 2"),
            ChatMessage::user("recent 1"),
            ChatMessage::assistant("recent 2"),
        ];

        apply_compaction_summary(&mut history, 1, 3, "- user prefers concise replies");

        assert_eq!(history.len(), 4);
        assert!(history[1].content.contains("Compaction summary"));
        assert!(history[2].content.contains("recent 1"));
        assert!(history[3].content.contains("recent 2"));
    }

    #[test]
    fn autosave_memory_key_has_prefix_and_uniqueness() {
        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        assert!(key1.starts_with("user_msg_"));
        assert!(key2.starts_with("user_msg_"));
        assert_ne!(key1, key2);
    }

    #[tokio::test]
    async fn autosave_memory_keys_preserve_multiple_turns() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        mem.store(&key1, "I'm Paul", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        mem.store(&key2, "I'm 45", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let recalled = mem.recall("45", 5, None).await.unwrap();
        assert!(recalled.iter().any(|entry| entry.content.contains("45")));
    }

    #[tokio::test]
    async fn build_context_ignores_legacy_assistant_autosave_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store(
            "assistant_resp_poisoned",
            "User suffered a fabricated event",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_msg_real",
            "User asked for concise status updates",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, "status updates", 0.0, None).await;
        assert!(context.contains("user_msg_real"));
        assert!(!context.contains("assistant_resp_poisoned"));
        assert!(!context.contains("fabricated event"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Tool Call Parsing Edge Cases
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_calls_handles_empty_tool_result() {
        // Recovery: Empty tool_result tag should be handled gracefully
        let response = r#"I'll run that command.
<tool_result name="shell">

</tool_result>
Done."#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Done."));
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_arguments_value_handles_null() {
        // Recovery: null arguments are returned as-is (Value::Null)
        let value = serde_json::json!(null);
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_null());
    }

    #[test]
    fn parse_tool_calls_handles_empty_tool_calls_array() {
        // Recovery: Empty tool_calls array returns original response (no tool parsing)
        let response = r#"{"content": "Hello", "tool_calls": []}"#;
        let (text, calls) = parse_tool_calls(response);
        // When tool_calls is empty, the entire JSON is returned as text
        assert!(text.contains("Hello"));
        assert!(calls.is_empty());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_malformed_payloads() {
        let response =
            "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}</tool_call>";
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(
            issue.is_some(),
            "malformed tool payload should be flagged for diagnostics"
        );
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_normal_text() {
        let issue = detect_tool_call_parse_issue("Thanks, done.", &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn looks_like_deferred_action_without_tool_call_detects_action_promises() {
        assert!(looks_like_deferred_action_without_tool_call(
            "Webpage opened, let's see what's new here."
        ));
        assert!(looks_like_deferred_action_without_tool_call(
            "It seems absolute paths are blocked. Let me try using a relative path."
        ));
        assert!(looks_like_deferred_action_without_tool_call(
            "看起来绝对路径不可用，让我尝试使用当前目录的相对路径。"
        ));
        assert!(looks_like_deferred_action_without_tool_call(
            "页面已打开，让我获取快照查看详细信息。"
        ));
    }

    #[test]
    fn looks_like_deferred_action_without_tool_call_ignores_final_answers() {
        assert!(!looks_like_deferred_action_without_tool_call(
            "The latest update is already shown above."
        ));
        assert!(!looks_like_deferred_action_without_tool_call(
            "最新结果已经在上面整理完成。"
        ));
    }

    #[test]
    fn parse_tool_calls_handles_whitespace_only_name() {
        // Recovery: Whitespace-only tool name should return None
        let value = serde_json::json!({"function": {"name": "   ", "arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_calls_handles_empty_string_arguments() {
        // Recovery: Empty string arguments should be handled
        let value = serde_json::json!({"name": "test", "arguments": ""});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - History Management
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn trim_history_with_no_system_prompt() {
        // Recovery: History without system prompt should trim correctly
        let mut history = vec![];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 20 {
            history.push(ChatMessage::user(format!("msg {i}")));
        }
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), DEFAULT_MAX_HISTORY_MESSAGES);
    }

    #[test]
    fn trim_history_preserves_role_ordering() {
        // Recovery: After trimming, role ordering should remain consistent
        let mut history = vec![ChatMessage::system("system")];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 10 {
            history.push(ChatMessage::user(format!("user {i}")));
            history.push(ChatMessage::assistant(format!("assistant {i}")));
        }
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[history.len() - 1].role, "assistant");
    }

    #[test]
    fn trim_history_with_only_system_prompt() {
        // Recovery: Only system prompt should not be trimmed
        let mut history = vec![ChatMessage::system("system prompt")];
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Arguments Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_arguments_value_handles_invalid_json_string() {
        // Recovery: Invalid JSON string should return empty object
        let value = serde_json::Value::String("not valid json".to_string());
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_arguments_value_handles_none() {
        // Recovery: None arguments should return empty object
        let result = parse_arguments_value(None);
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - JSON Extraction
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn extract_json_values_handles_empty_string() {
        // Recovery: Empty input should return empty vec
        let result = extract_json_values("");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_whitespace_only() {
        // Recovery: Whitespace only should return empty vec
        let result = extract_json_values("   \n\t  ");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_multiple_objects() {
        // Recovery: Multiple JSON objects should all be extracted
        let input = r#"{"a": 1}{"b": 2}{"c": 3}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn extract_json_values_handles_arrays() {
        // Recovery: JSON arrays should be extracted
        let input = r#"[1, 2, 3]{"key": "value"}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 2);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Constants Validation
    // ═══════════════════════════════════════════════════════════════════════

    const _: () = {
        assert!(DEFAULT_MAX_TOOL_ITERATIONS > 0);
        assert!(DEFAULT_MAX_TOOL_ITERATIONS <= 100);
        assert!(DEFAULT_MAX_HISTORY_MESSAGES > 0);
        assert!(DEFAULT_MAX_HISTORY_MESSAGES <= 1000);
    };

    #[test]
    fn constants_bounds_are_compile_time_checked() {
        // Bounds are enforced by the const assertions above.
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Tool Call Value Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_call_value_handles_missing_name_field() {
        // Recovery: Missing name field should return None
        let value = serde_json::json!({"function": {"arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_call_value_handles_top_level_name() {
        // Recovery: Tool call with name at top level (non-OpenAI format)
        let value = serde_json::json!({"name": "test_tool", "arguments": {}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test_tool");
    }

    #[test]
    fn parse_tool_call_value_accepts_top_level_parameters_alias() {
        let value = serde_json::json!({
            "name": "schedule",
            "parameters": {"action": "create", "message": "test"}
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "schedule");
        assert_eq!(
            result.arguments.get("action").and_then(|v| v.as_str()),
            Some("create")
        );
    }

    #[test]
    fn parse_tool_call_value_accepts_function_parameters_alias() {
        let value = serde_json::json!({
            "function": {
                "name": "shell",
                "parameters": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("date")
        );
    }

    #[test]
    fn parse_tool_call_value_recovers_shell_command_from_raw_string_arguments() {
        let value = serde_json::json!({
            "name": "shell",
            "arguments": "uname -a"
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("uname -a")
        );
    }

    #[test]
    fn parse_tool_call_value_recovers_shell_command_from_cmd_alias() {
        let value = serde_json::json!({
            "function": {
                "name": "shell",
                "arguments": {"cmd": "pwd"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("pwd")
        );
    }

    #[test]
    fn parse_tool_call_value_preserves_tool_call_id_aliases() {
        let value = serde_json::json!({
            "call_id": "legacy_1",
            "function": {
                "name": "shell",
                "arguments": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.tool_call_id.as_deref(), Some("legacy_1"));
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_empty_array() {
        // Recovery: Empty tool_calls array should return empty vec
        let value = serde_json::json!({"tool_calls": []});
        let result = parse_tool_calls_from_json_value(&value);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_missing_tool_calls() {
        // Recovery: Missing tool_calls field should fall through
        let value = serde_json::json!({"name": "test", "arguments": {}});
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_top_level_array() {
        // Recovery: Top-level array of tool calls
        let value = serde_json::json!([
            {"name": "tool_a", "arguments": {}},
            {"name": "tool_b", "arguments": {}}
        ]);
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_structured_tool_calls_recovers_shell_command_from_string_payload() {
        let calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: "ls -la".to_string(),
        }];
        let parsed = parse_structured_tool_calls(&calls);
        assert_eq!(parsed.invalid_json_arguments, 0);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].name, "shell");
        assert_eq!(
            parsed.calls[0]
                .arguments
                .get("command")
                .and_then(|v| v.as_str()),
            Some("ls -la")
        );
    }

    #[test]
    fn parse_structured_tool_calls_skips_truncated_json_payloads() {
        let calls = vec![ToolCall {
            id: "call_bad".to_string(),
            name: "count_tool".to_string(),
            arguments: "{\"value\":\"unterminated\"".to_string(),
        }];
        let parsed = parse_structured_tool_calls(&calls);
        assert_eq!(parsed.calls.len(), 0);
        assert_eq!(parsed.invalid_json_arguments, 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // GLM-Style Tool Call Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_glm_style_browser_open_url() {
        let response = "browser_open/url>https://example.com";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert!(calls[0].1["command"].as_str().unwrap().contains("curl"));
        assert!(calls[0].1["command"]
            .as_str()
            .unwrap()
            .contains("example.com"));
    }

    #[test]
    fn parse_glm_style_shell_command() {
        let response = "shell/command>ls -la";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "ls -la");
    }

    #[test]
    fn parse_glm_style_http_request() {
        let response = "http_request/url>https://api.example.com/data";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "http_request");
        assert_eq!(calls[0].1["url"], "https://api.example.com/data");
        assert_eq!(calls[0].1["method"], "GET");
    }

    #[test]
    fn parse_glm_style_plain_url() {
        let response = "https://example.com/api";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert!(calls[0].1["command"].as_str().unwrap().contains("curl"));
    }

    #[test]
    fn parse_glm_style_json_args() {
        let response = r#"shell/{"command": "echo hello"}"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "echo hello");
    }

    #[test]
    fn parse_glm_style_multiple_calls() {
        let response = r#"shell/command>ls
browser_open/url>https://example.com"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn parse_glm_style_tool_call_integration() {
        // Integration test: GLM format should be parsed in parse_tool_calls
        let response = "Checking...\nbrowser_open/url>https://example.com\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(text.contains("Checking"));
        assert!(text.contains("Done"));
    }

    #[test]
    fn parse_glm_style_rejects_non_http_url_param() {
        let response = "browser_open/url>javascript:alert(1)";
        let calls = parse_glm_style_tool_calls(response);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_unclosed_tool_call_tag() {
        let response = "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert_eq!(text, "Done");
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): parse_tool_calls robustness — malformed/edge-case inputs
    // Prevents: Pattern 4 issues #746, #418, #777, #848
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_tool_calls_empty_input_returns_empty() {
        let (text, calls) = parse_tool_calls("");
        assert!(calls.is_empty(), "empty input should produce no tool calls");
        assert!(text.is_empty(), "empty input should produce no text");
    }

    #[test]
    fn parse_tool_calls_whitespace_only_returns_empty_calls() {
        let (text, calls) = parse_tool_calls("   \n\t  ");
        assert!(calls.is_empty());
        assert!(text.is_empty() || text.trim().is_empty());
    }

    #[test]
    fn parse_tool_calls_nested_xml_tags_handled() {
        // Double-wrapped tool call should still parse the inner call
        let response = r#"<tool_call><tool_call>{"name":"echo","arguments":{"msg":"hi"}}</tool_call></tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        // Should find at least one tool call
        assert!(
            !calls.is_empty(),
            "nested XML tags should still yield at least one tool call"
        );
    }

    #[test]
    fn parse_tool_calls_truncated_json_no_panic() {
        // Incomplete JSON inside tool_call tags
        let response = r#"<tool_call>{"name":"shell","arguments":{"command":"ls"</tool_call>"#;
        let (_text, _calls) = parse_tool_calls(response);
        // Should not panic — graceful handling of truncated JSON
    }

    #[test]
    fn parse_tool_calls_empty_json_object_in_tag() {
        let response = "<tool_call>{}</tool_call>";
        let (_text, calls) = parse_tool_calls(response);
        // Empty JSON object has no name field — should not produce valid tool call
        assert!(
            calls.is_empty(),
            "empty JSON object should not produce a tool call"
        );
    }

    #[test]
    fn parse_tool_calls_closing_tag_only_returns_text() {
        let response = "Some text </tool_call> more text";
        let (text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "closing tag only should not produce calls"
        );
        assert!(
            !text.is_empty(),
            "text around orphaned closing tag should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_very_large_arguments_no_panic() {
        let large_arg = "x".repeat(100_000);
        let response = format!(
            r#"<tool_call>{{"name":"echo","arguments":{{"message":"{}"}}}}</tool_call>"#,
            large_arg
        );
        let (_text, calls) = parse_tool_calls(&response);
        assert_eq!(calls.len(), 1, "large arguments should still parse");
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_special_characters_in_arguments() {
        let response = r#"<tool_call>{"name":"echo","arguments":{"message":"hello \"world\" <>&'\n\t"}}</tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_text_with_embedded_json_not_extracted() {
        // Raw JSON without any tags should NOT be extracted as a tool call
        let response = r#"Here is some data: {"name":"echo","arguments":{"message":"hi"}} end."#;
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "raw JSON in text without tags should not be extracted"
        );
    }

    #[test]
    fn parse_tool_calls_multiple_formats_mixed() {
        // Mix of text and properly tagged tool call
        let response = r#"I'll help you with that.

<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>

Let me check the result."#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should extract one tool call from mixed content"
        );
        assert_eq!(calls[0].name, "shell");
        assert!(
            text.contains("help you"),
            "text before tool call should be preserved"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): scrub_credentials edge cases
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn scrub_credentials_empty_input() {
        let result = scrub_credentials("");
        assert_eq!(result, "");
    }

    #[test]
    fn scrub_credentials_no_sensitive_data() {
        let input = "normal text without any secrets";
        let result = scrub_credentials(input);
        assert_eq!(
            result, input,
            "non-sensitive text should pass through unchanged"
        );
    }

    #[test]
    fn scrub_credentials_short_values_not_redacted() {
        // Values shorter than 8 chars should not be redacted
        let input = r#"api_key="short""#;
        let result = scrub_credentials(input);
        assert_eq!(result, input, "short values should not be redacted");
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): trim_history edge cases
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn trim_history_empty_history() {
        let mut history: Vec<crate::providers::ChatMessage> = vec![];
        trim_history(&mut history, 10);
        assert!(history.is_empty());
    }

    #[test]
    fn trim_history_system_only() {
        let mut history = vec![crate::providers::ChatMessage::system("system prompt")];
        trim_history(&mut history, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "system");
    }

    #[test]
    fn trim_history_exactly_at_limit() {
        let mut history = vec![
            crate::providers::ChatMessage::system("system"),
            crate::providers::ChatMessage::user("msg 1"),
            crate::providers::ChatMessage::assistant("reply 1"),
        ];
        trim_history(&mut history, 2); // 2 non-system messages = exactly at limit
        assert_eq!(history.len(), 3, "should not trim when exactly at limit");
    }

    #[test]
    fn trim_history_removes_oldest_non_system() {
        let mut history = vec![
            crate::providers::ChatMessage::system("system"),
            crate::providers::ChatMessage::user("old msg"),
            crate::providers::ChatMessage::assistant("old reply"),
            crate::providers::ChatMessage::user("new msg"),
            crate::providers::ChatMessage::assistant("new reply"),
        ];
        trim_history(&mut history, 2);
        assert_eq!(history.len(), 3); // system + 2 kept
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].content, "new msg");
    }

    /// When `build_system_prompt_with_mode` is called with `native_tools = true`,
    /// the output must contain ZERO XML protocol artifacts. In the native path
    /// `build_tool_instructions` is never called, so the system prompt alone
    /// must be clean of XML tool-call protocol.
    #[test]
    fn native_tools_system_prompt_contains_zero_xml() {
        use crate::channels::build_system_prompt_with_mode;

        let tool_summaries: Vec<(&str, &str)> = vec![
            ("shell", "Execute shell commands"),
            ("file_read", "Read files"),
        ];

        let system_prompt = build_system_prompt_with_mode(
            std::path::Path::new("/tmp"),
            "test-model",
            &tool_summaries,
            &[],  // no skills
            None, // no identity config
            None, // no bootstrap_max_chars
            true, // native_tools
            crate::config::SkillsPromptInjectionMode::Full,
        );

        // Must contain zero XML protocol artifacts
        assert!(
            !system_prompt.contains("<tool_call>"),
            "Native prompt must not contain <tool_call>"
        );
        assert!(
            !system_prompt.contains("</tool_call>"),
            "Native prompt must not contain </tool_call>"
        );
        assert!(
            !system_prompt.contains("<tool_result>"),
            "Native prompt must not contain <tool_result>"
        );
        assert!(
            !system_prompt.contains("</tool_result>"),
            "Native prompt must not contain </tool_result>"
        );
        assert!(
            !system_prompt.contains("## Tool Use Protocol"),
            "Native prompt must not contain XML protocol header"
        );

        // Positive: native prompt should still list tools and contain task instructions
        assert!(
            system_prompt.contains("shell"),
            "Native prompt must list tool names"
        );
        assert!(
            system_prompt.contains("## Your Task"),
            "Native prompt should contain task instructions"
        );
    }

    // ── Cross-Alias & GLM Shortened Body Tests ──────────────────────────

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_json() {
        // <tool_call> opened but closed with </invoke> — JSON body
        let input = r#"<tool_call>{"name": "shell", "arguments": {"command": "ls"}}</invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_glm_shortened() {
        // <tool_call>shell>uname -a</invoke> — GLM shortened inside cross-alias tags
        let input = "<tool_call>shell>uname -a</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_shortened_body_in_matched_tags() {
        // <tool_call>shell>pwd</tool_call> — GLM shortened in matched tags
        let input = "<tool_call>shell>pwd</tool_call>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_yaml_style_in_tags() {
        // <tool_call>shell>\ncommand: date\napproved: true</invoke>
        let input = "<tool_call>shell>\ncommand: date\napproved: true</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert_eq!(calls[0].arguments["approved"], true);
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_attribute_style_in_tags() {
        // <tool_call>shell command="date" /></tool_call>
        let input = r#"<tool_call>shell command="date" /></tool_call>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_file_read_shortened_in_cross_alias() {
        // <tool_call>file_read path=".env" /></invoke>
        let input = r#"<tool_call>file_read path=".env" /></invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments["path"], ".env");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_unclosed_glm_shortened_no_close_tag() {
        // <tool_call>shell>ls -la (no close tag at all)
        let input = "<tool_call>shell>ls -la";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_text_before_cross_alias() {
        // Text before and after cross-alias tool call
        let input = "Let me check that.\n<tool_call>shell>uname -a</invoke>\nDone.";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.contains("Let me check that."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_glm_shortened_body_url_to_curl() {
        // URL values for shell should be wrapped in curl
        let call = parse_glm_shortened_body("shell>https://example.com/api").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_browser_open_maps_to_shell_command() {
        // browser_open aliases to shell, and shortened calls must still emit
        // shell's canonical "command" argument.
        let call = parse_glm_shortened_body("browser_open>https://example.com").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_memory_recall() {
        // memory_recall>some query — default param is "query"
        let call = parse_glm_shortened_body("memory_recall>recent meetings").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "recent meetings");
    }

    #[test]
    fn parse_glm_shortened_body_function_style_alias_maps_to_message_send() {
        let call =
            parse_glm_shortened_body(r#"sendmessage(channel="alerts", message="hi")"#).unwrap();
        assert_eq!(call.name, "message_send");
        assert_eq!(call.arguments["channel"], "alerts");
        assert_eq!(call.arguments["message"], "hi");
    }

    #[test]
    fn map_tool_name_alias_direct_coverage() {
        assert_eq!(map_tool_name_alias("bash"), "shell");
        assert_eq!(map_tool_name_alias("filelist"), "file_list");
        assert_eq!(map_tool_name_alias("memorystore"), "memory_store");
        assert_eq!(map_tool_name_alias("memoryforget"), "memory_forget");
        assert_eq!(map_tool_name_alias("http"), "http_request");
        assert_eq!(
            map_tool_name_alias("totally_unknown_tool"),
            "totally_unknown_tool"
        );
    }

    #[test]
    fn default_param_for_tool_coverage() {
        assert_eq!(default_param_for_tool("shell"), "command");
        assert_eq!(default_param_for_tool("bash"), "command");
        assert_eq!(default_param_for_tool("file_read"), "path");
        assert_eq!(default_param_for_tool("memory_recall"), "query");
        assert_eq!(default_param_for_tool("memory_store"), "content");
        assert_eq!(default_param_for_tool("http_request"), "url");
        assert_eq!(default_param_for_tool("browser_open"), "url");
        assert_eq!(default_param_for_tool("unknown_tool"), "input");
    }

    #[test]
    fn parse_glm_shortened_body_rejects_empty() {
        assert!(parse_glm_shortened_body("").is_none());
        assert!(parse_glm_shortened_body("   ").is_none());
    }

    #[test]
    fn parse_glm_shortened_body_rejects_invalid_tool_name() {
        // Tool names with special characters should be rejected
        assert!(parse_glm_shortened_body("not-a-tool>value").is_none());
        assert!(parse_glm_shortened_body("tool name>value").is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests for history builders
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_native_assistant_history_includes_reasoning_content() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }];
        let result = build_native_assistant_history("answer", &calls, Some("thinking step"));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("thinking step"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_omits_reasoning_content_when_none() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }];
        let result = build_native_assistant_history("answer", &calls, None);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_includes_reasoning_content() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls(
            "answer",
            &calls,
            Some("deep thought"),
        );
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("deep thought"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_omits_reasoning_content_when_none() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls("answer", &calls, None);
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }

    #[test]
    fn progress_mode_gates_work_as_expected() {
        assert!(should_emit_verbose_progress(ProgressMode::Verbose));
        assert!(!should_emit_verbose_progress(ProgressMode::Compact));
        assert!(!should_emit_verbose_progress(ProgressMode::Off));

        assert!(should_emit_tool_progress(ProgressMode::Verbose));
        assert!(should_emit_tool_progress(ProgressMode::Compact));
        assert!(!should_emit_tool_progress(ProgressMode::Off));
    }

    #[test]
    fn progress_tracker_renders_in_place_block() {
        let mut tracker = ProgressTracker::default();
        let first = tracker.add("shell", "ls -la");
        let second = tracker.add("web_search", "rust async test");
        let started = tracker.render_delta();
        assert!(started.starts_with(DRAFT_PROGRESS_BLOCK_SENTINEL));
        assert!(started.contains("⏳ shell: ls -la"));
        assert!(started.contains("⏳ web_search: rust async test"));

        tracker.complete(first, true, 2);
        tracker.complete(second, false, 1);
        let completed = tracker.render_delta();
        assert!(completed.contains("✅ shell (2s)"));
        assert!(completed.contains("❌ web_search (1s)"));
    }
}
