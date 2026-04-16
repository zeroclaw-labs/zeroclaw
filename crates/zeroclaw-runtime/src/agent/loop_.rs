use crate::approval::{ApprovalManager, ApprovalRequest, ApprovalResponse};

/// CLI channel factory, injected by the binary. Returns a `Box<dyn Channel>` for interactive mode.
pub static CLI_CHANNEL_FN: std::sync::OnceLock<
    Box<dyn Fn() -> Box<dyn zeroclaw_api::channel::Channel> + Send + Sync>,
> = std::sync::OnceLock::new();

/// Register the CLI channel factory. Called once at startup by the binary.
pub fn register_cli_channel_fn(
    f: Box<dyn Fn() -> Box<dyn zeroclaw_api::channel::Channel> + Send + Sync>,
) {
    let _ = CLI_CHANNEL_FN.set(f);
}

/// Peripheral tools factory type — takes owned config so the returned future is 'static.
pub type PeripheralToolsFn = Box<
    dyn Fn(
            zeroclaw_config::schema::PeripheralsConfig,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Vec<Box<dyn Tool>>>> + Send>,
        > + Send
        + Sync,
>;

/// Peripheral tools factory, injected by the binary when hardware feature is on.
static PERIPHERAL_TOOLS_FN: std::sync::OnceLock<PeripheralToolsFn> = std::sync::OnceLock::new();

/// Register the peripheral tools factory. Called once at startup by the binary.
pub fn register_peripheral_tools_fn(f: PeripheralToolsFn) {
    let _ = PERIPHERAL_TOOLS_FN.set(f);
}
use crate::cost::types::BudgetCheck;
use crate::i18n::ToolDescriptions;
use crate::observability::{self, Observer, ObserverEvent, runtime_trace};
use crate::platform;
use crate::security::{AutonomyLevel, SecurityPolicy};
use crate::tools::{self, Tool};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use futures_util::StreamExt;
use regex::Regex;
use std::collections::HashSet;
use std::fmt::Write;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::provider::StreamEvent;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{self, Memory, MemoryCategory, decay};
use zeroclaw_providers::multimodal;
use zeroclaw_providers::{
    self, ChatMessage, ChatRequest, Provider, ProviderCapabilityError, ToolCall,
};

// Cost tracking moved to `super::cost`.
pub use super::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext, check_tool_loop_budget,
    record_tool_loop_cost_usage,
};

/// Minimum characters per chunk when relaying LLM text to a streaming draft.
const STREAM_CHUNK_MIN_CHARS: usize = 80;
/// Rolling window size for detecting streamed tool-call payload markers.
const STREAM_TOOL_MARKER_WINDOW_CHARS: usize = 512;

/// Default maximum agentic tool-use iterations per user message to prevent runaway loops.
/// Used as a safe fallback when `max_tool_iterations` is unset or configured as zero.
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 10;

// History management moved to `super::history`.
pub use super::history::{
    emergency_history_trim, estimate_history_tokens, fast_trim_tool_results,
    load_interactive_session_history, save_interactive_session_history, trim_history,
    truncate_tool_result,
};

/// Minimum user-message length (in chars) for auto-save to memory.
/// Matches the channel-side constant in `channels/mod.rs`.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

/// Callback type for checking if model has been switched during tool execution.
/// Returns Some((provider, model)) if a switch was requested, None otherwise.
pub type ModelSwitchCallback = Arc<Mutex<Option<(String, String)>>>;

/// Global model switch request state - used for runtime model switching via model_switch tool.
/// This is set by the model_switch tool and checked by the agent loop.
#[allow(clippy::type_complexity)]
static MODEL_SWITCH_REQUEST: LazyLock<Arc<Mutex<Option<(String, String)>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

/// Get the global model switch request state
pub fn get_model_switch_state() -> ModelSwitchCallback {
    Arc::clone(&MODEL_SWITCH_REQUEST)
}

/// Clear any pending model switch request
pub fn clear_model_switch_request() {
    if let Ok(guard) = MODEL_SWITCH_REQUEST.lock() {
        let mut guard = guard;
        *guard = None;
    }
}

fn glob_match(pattern: &str, name: &str) -> bool {
    match pattern.find('*') {
        None => pattern == name,
        Some(star) => {
            let prefix = &pattern[..star];
            let suffix = &pattern[star + 1..];
            name.starts_with(prefix)
                && name.ends_with(suffix)
                && name.len() >= prefix.len() + suffix.len()
        }
    }
}

/// Returns the subset of `tool_specs` that should be sent to the LLM for this turn.
///
/// Rules (mirrors NullClaw `filterToolSpecsForTurn`):
/// - Built-in tools (names that do not start with `"mcp_"`) always pass through.
/// - When `groups` is empty, all tools pass through (backward compatible default).
/// - An MCP tool is included if at least one group matches it:
///   - `always` group: included unconditionally if any pattern matches the tool name.
///   - `dynamic` group: included if any pattern matches AND the user message contains
///     at least one keyword (case-insensitive substring).
pub fn filter_tool_specs_for_turn(
    tool_specs: Vec<crate::tools::ToolSpec>,
    groups: &[zeroclaw_config::schema::ToolFilterGroup],
    user_message: &str,
) -> Vec<crate::tools::ToolSpec> {
    use zeroclaw_config::schema::ToolFilterGroupMode;

    if groups.is_empty() {
        return tool_specs;
    }

    let msg_lower = user_message.to_ascii_lowercase();

    tool_specs
        .into_iter()
        .filter(|spec| {
            // Built-in tools always pass through.
            if !spec.name.starts_with("mcp_") {
                return true;
            }
            // MCP tool: include if any active group matches.
            groups.iter().any(|group| {
                let pattern_matches = group.tools.iter().any(|pat| glob_match(pat, &spec.name));
                if !pattern_matches {
                    return false;
                }
                match group.mode {
                    ToolFilterGroupMode::Always => true,
                    ToolFilterGroupMode::Dynamic => group
                        .keywords
                        .iter()
                        .any(|kw| msg_lower.contains(&kw.to_ascii_lowercase())),
                }
            })
        })
        .collect()
}

/// Filters a tool spec list by an optional capability allowlist.
///
/// When `allowed` is `None`, all specs pass through unchanged.
/// When `allowed` is `Some(list)`, only specs whose name appears in the list
/// are retained. Unknown names in the allowlist are silently ignored.
pub fn filter_by_allowed_tools(
    specs: Vec<crate::tools::ToolSpec>,
    allowed: Option<&[String]>,
) -> Vec<crate::tools::ToolSpec> {
    match allowed {
        None => specs,
        Some(list) => specs
            .into_iter()
            .filter(|spec| list.iter().any(|name| name == &spec.name))
            .collect(),
    }
}

// Re-export from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::TOOL_LOOP_THREAD_ID;

// Re-export tool call parsing from the standalone parser crate.
pub use zeroclaw_tool_call_parser::{
    ParsedToolCall, build_native_assistant_history_from_parsed_calls,
    canonicalize_json_for_tool_signature, detect_tool_call_parse_issue, parse_tool_calls,
    strip_think_tags, strip_tool_result_blocks,
};

/// Run a future with the thread ID set in task-local storage.
/// Rate-limiting reads this to assign per-sender buckets.
pub async fn scope_thread_id<F>(thread_id: Option<String>, future: F) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_THREAD_ID.scope(thread_id, future).await
}

/// Computes the list of MCP tool names that should be excluded for a given turn
/// based on `tool_filter_groups` and the user message.
///
/// Returns an empty `Vec` when `groups` is empty (no filtering).
fn compute_excluded_mcp_tools(
    tools_registry: &[Box<dyn Tool>],
    groups: &[zeroclaw_config::schema::ToolFilterGroup],
    user_message: &str,
) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    let filtered_specs = filter_tool_specs_for_turn(
        tools_registry.iter().map(|t| t.spec()).collect(),
        groups,
        user_message,
    );
    let included: HashSet<&str> = filtered_specs.iter().map(|s| s.name.as_str()).collect();
    tools_registry
        .iter()
        .filter(|t| t.name().starts_with("mcp_") && !included.contains(t.name()))
        .map(|t| t.name().to_string())
        .collect()
}

static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(token|api[_-]?key|password|secret|user[_-]?key|bearer|credential)["']?\s*[:=]\s*(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\.]{8,}))"#).unwrap()
});

/// Scrub credentials from tool output to prevent accidental exfiltration.
/// Replaces known credential patterns with a redacted placeholder while preserving
/// a small prefix for context.
pub fn scrub_credentials(input: &str) -> String {
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

            // Preserve first 4 chars for context, then redact.
            // Use char_indices to find the byte offset of the 4th character
            // so we never slice in the middle of a multi-byte UTF-8 sequence.
            let prefix = if val.len() > 4 {
                val.char_indices()
                    .nth(4)
                    .map(|(byte_idx, _)| &val[..byte_idx])
                    .unwrap_or(val)
            } else {
                ""
            };

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
/// Minimum interval between progress sends to avoid flooding the draft channel.
pub const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Delta sent from the agent loop to the channel's draft updater.
/// Append-only — no clear/reset variant exists by design.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// Response text to append to the message buffer.
    Text(String),
    /// Ephemeral tool progress (not part of the response body).
    Status(String),
}

/// Backwards-compatible alias while callers are migrated.
pub type DraftEvent = StreamDelta;

pub use zeroclaw_api::TOOL_CHOICE_OVERRIDE;

/// Convert a tool registry to OpenAI function-calling format for native tool support.
#[cfg(test)]
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

/// Build context preamble by searching memory for relevant entries.
/// Entries with a hybrid score below `min_relevance_score` are dropped to
/// prevent unrelated memories from bleeding into the conversation.
/// Core memories are exempt from time decay (evergreen).
async fn build_context(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
) -> String {
    let mut context = String::new();

    // Pull relevant memories for this message
    if let Ok(mut entries) = mem.recall(user_msg, 5, session_id, None, None).await {
        // Apply time decay: older non-Core memories score lower
        decay::apply_time_decay(&mut entries, decay::DEFAULT_HALF_LIFE_DAYS);

        let relevant: Vec<_> = entries
            .iter()
            .filter(|e| match e.score {
                Some(score) => score >= min_relevance_score,
                None => true,
            })
            .collect();

        if !relevant.is_empty() {
            context.push_str("[Memory context]\n");
            for entry in &relevant {
                if zeroclaw_memory::is_assistant_autosave_key(&entry.key) {
                    continue;
                }
                if zeroclaw_memory::should_skip_autosave_content(&entry.content) {
                    continue;
                }
                // Skip entries containing tool_result blocks — they can leak
                // stale tool output from previous heartbeat ticks into new
                // sessions, presenting the LLM with orphan tool_result data.
                if entry.content.contains("<tool_result") {
                    continue;
                }
                let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
            }
            if context == "[Memory context]\n" {
                context.clear();
            } else {
                context.push_str("[/Memory context]\n\n");
            }
        }
    }

    context
}

/// Build hardware datasheet context from RAG when peripherals are enabled.
/// Includes pin-alias lookup (e.g. "red_led" → 13) when query matches, plus retrieved chunks.
fn build_hardware_context(
    rag: &crate::rag::HardwareRag,
    user_msg: &str,
    boards: &[String],
    chunk_limit: usize,
) -> String {
    if rag.is_empty() || boards.is_empty() {
        return String::new();
    }

    let mut context = String::new();

    // Pin aliases: when user says "red led", inject "red_led: 13" for matching boards
    let pin_ctx = rag.pin_alias_context(user_msg, boards);
    if !pin_ctx.is_empty() {
        context.push_str(&pin_ctx);
    }

    let chunks = rag.retrieve(user_msg, boards, chunk_limit);
    if chunks.is_empty() && pin_ctx.is_empty() {
        return String::new();
    }

    if !chunks.is_empty() {
        context.push_str("[Hardware documentation]\n");
    }
    for chunk in chunks {
        let board_tag = chunk.board.as_deref().unwrap_or("generic");
        let _ = writeln!(
            context,
            "--- {} ({}) ---\n{}\n",
            chunk.source, board_tag, chunk.content
        );
    }
    context.push('\n');
    context
}

// Tool execution moved to `super::tool_execution`.
pub use super::tool_execution::{
    ToolExecutionOutcome, execute_tools_parallel, execute_tools_sequential,
    should_execute_tools_in_parallel,
};

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

#[cfg(test)]
fn resolve_display_text(
    response_text: &str,
    parsed_text: &str,
    has_tool_calls: bool,
    has_native_tool_calls: bool,
) -> String {
    if has_tool_calls {
        if !parsed_text.is_empty() {
            return parsed_text.to_string();
        }
        if has_native_tool_calls {
            return response_text.to_string();
        }
        return String::new();
    }

    if parsed_text.is_empty() {
        response_text.to_string()
    } else {
        parsed_text.to_string()
    }
}

#[derive(Debug)]
pub struct ToolLoopCancelled;

impl std::fmt::Display for ToolLoopCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for ToolLoopCancelled {}

pub fn is_tool_loop_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.is::<ToolLoopCancelled>())
}

#[derive(Debug)]
pub struct ModelSwitchRequested {
    pub provider: String,
    pub model: String,
}

impl std::fmt::Display for ModelSwitchRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model switch requested to {} {}",
            self.provider, self.model
        )
    }
}

impl std::error::Error for ModelSwitchRequested {}

pub fn is_model_switch_requested(err: &anyhow::Error) -> Option<(String, String)> {
    err.chain()
        .filter_map(|source| source.downcast_ref::<ModelSwitchRequested>())
        .map(|e| (e.provider.clone(), e.model.clone()))
        .next()
}

#[derive(Debug, Default)]
struct StreamedChatOutcome {
    response_text: String,
    tool_calls: Vec<ToolCall>,
    forwarded_live_deltas: bool,
}

async fn consume_provider_streaming_response(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    request_tools: Option<&[crate::tools::ToolSpec]>,
    model: &str,
    temperature: f64,
    cancellation_token: Option<&CancellationToken>,
    on_delta: Option<&tokio::sync::mpsc::Sender<DraftEvent>>,
) -> Result<StreamedChatOutcome> {
    let mut provider_stream = provider.stream_chat(
        ChatRequest {
            messages,
            tools: request_tools,
        },
        model,
        temperature,
        zeroclaw_providers::traits::StreamOptions::new(true),
    );
    let mut outcome = StreamedChatOutcome::default();
    let mut delta_sender = on_delta;
    let mut suppress_forwarding = false;
    let mut marker_window = String::new();

    loop {
        let next_chunk = if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                chunk = provider_stream.next() => chunk,
            }
        } else {
            provider_stream.next().await
        };

        let Some(event_result) = next_chunk else {
            break;
        };

        let event = event_result.map_err(|err| anyhow::anyhow!("provider stream error: {err}"))?;
        match event {
            StreamEvent::Final => break,
            StreamEvent::ToolCall(tool_call) => {
                outcome.tool_calls.push(tool_call);
                suppress_forwarding = true;
            }
            StreamEvent::PreExecutedToolCall { .. } | StreamEvent::PreExecutedToolResult { .. } => {
                // Pre-executed tool events are for observability only.
                // They are forwarded to the gateway via turn_streamed but
                // do not affect the agent's tool dispatch loop.
            }
            StreamEvent::TextDelta(chunk) => {
                if chunk.delta.is_empty() {
                    continue;
                }

                outcome.response_text.push_str(&chunk.delta);
                marker_window.push_str(&chunk.delta);

                if marker_window.len() > STREAM_TOOL_MARKER_WINDOW_CHARS {
                    let keep_from = marker_window.len() - STREAM_TOOL_MARKER_WINDOW_CHARS;
                    let boundary = marker_window
                        .char_indices()
                        .find(|(idx, _)| *idx >= keep_from)
                        .map_or(0, |(idx, _)| idx);
                    marker_window.drain(..boundary);
                }

                if !suppress_forwarding && {
                    let lowered = marker_window.to_ascii_lowercase();
                    lowered.contains("<tool_call")
                        || lowered.contains("<toolcall")
                        || lowered.contains("\"tool_calls\"")
                } {
                    suppress_forwarding = true;
                }

                if suppress_forwarding {
                    continue;
                }

                if let Some(tx) = delta_sender {
                    outcome.forwarded_live_deltas = true;
                    if tx.send(StreamDelta::Text(chunk.delta)).await.is_err() {
                        delta_sender = None;
                    }
                }
            }
        }
    }

    Ok(outcome)
}

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
/// When `silent` is true, suppresses stdout (for channel use).
#[allow(clippy::too_many_arguments)]
pub async fn agent_turn(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    channel_name: &str,
    channel_reply_target: Option<&str>,
    multimodal_config: &zeroclaw_config::schema::MultimodalConfig,
    max_tool_iterations: usize,
    approval: Option<&ApprovalManager>,
    excluded_tools: &[String],
    dedup_exempt_tools: &[String],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    model_switch_callback: Option<ModelSwitchCallback>,
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
        approval,
        channel_name,
        channel_reply_target,
        multimodal_config,
        max_tool_iterations,
        None,
        None,
        None,
        excluded_tools,
        dedup_exempt_tools,
        activated_tools,
        model_switch_callback,
        &zeroclaw_config::schema::PacingConfig::default(),
        0,    // max_tool_result_chars: 0 = disabled (legacy callers)
        0,    // context_token_budget: 0 = disabled (legacy callers)
        None, // shared_budget: no shared budget for legacy callers
    )
    .await
}

fn maybe_inject_channel_delivery_defaults(
    tool_name: &str,
    tool_args: &mut serde_json::Value,
    channel_name: &str,
    channel_reply_target: Option<&str>,
) {
    if tool_name != "cron_add" {
        return;
    }

    if !matches!(
        channel_name,
        "telegram" | "discord" | "slack" | "mattermost" | "matrix"
    ) {
        return;
    }

    let Some(reply_target) = channel_reply_target
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let Some(args) = tool_args.as_object_mut() else {
        return;
    };

    let is_agent_job = args
        .get("job_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|job_type| job_type.eq_ignore_ascii_case("agent"))
        || args
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|prompt| !prompt.trim().is_empty());
    if !is_agent_job {
        return;
    }

    let default_delivery = || {
        serde_json::json!({
            "mode": "announce",
            "channel": channel_name,
            "to": reply_target,
        })
    };

    match args.get_mut("delivery") {
        None => {
            args.insert("delivery".to_string(), default_delivery());
        }
        Some(serde_json::Value::Null) => {
            *args.get_mut("delivery").expect("delivery key exists") = default_delivery();
        }
        Some(serde_json::Value::Object(delivery)) => {
            if delivery
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mode| mode.eq_ignore_ascii_case("none"))
            {
                return;
            }

            delivery
                .entry("mode".to_string())
                .or_insert_with(|| serde_json::Value::String("announce".to_string()));

            let needs_channel = delivery
                .get("channel")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if needs_channel {
                delivery.insert(
                    "channel".to_string(),
                    serde_json::Value::String(channel_name.to_string()),
                );
            }

            let needs_target = delivery
                .get("to")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if needs_target {
                delivery.insert(
                    "to".to_string(),
                    serde_json::Value::String(reply_target.to_string()),
                );
            }
        }
        Some(_) => {}
    }
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
    channel_reply_target: Option<&str>,
    multimodal_config: &zeroclaw_config::schema::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<DraftEvent>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    dedup_exempt_tools: &[String],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    model_switch_callback: Option<ModelSwitchCallback>,
    pacing: &zeroclaw_config::schema::PacingConfig,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    shared_budget: Option<Arc<std::sync::atomic::AtomicUsize>>,
) -> Result<String> {
    let max_iterations = if max_tool_iterations == 0 {
        DEFAULT_MAX_TOOL_ITERATIONS
    } else {
        max_tool_iterations
    };

    let turn_id = Uuid::new_v4().to_string();
    let loop_started_at = Instant::now();
    let loop_ignore_tools: HashSet<&str> = pacing
        .loop_ignore_tools
        .iter()
        .map(String::as_str)
        .collect();
    let mut consecutive_identical_outputs: usize = 0;
    let mut last_tool_output_hash: Option<u64> = None;

    let mut loop_detector = crate::agent::loop_detector::LoopDetector::new(
        crate::agent::loop_detector::LoopDetectorConfig {
            enabled: pacing.loop_detection_enabled,
            window_size: pacing.loop_detection_window_size,
            max_repeats: pacing.loop_detection_max_repeats,
        },
    );

    // Accumulated display text across all tool-loop calls.
    let mut accumulated_display_text = String::new();

    for iteration in 0..max_iterations {
        let mut seen_tool_signatures: HashSet<(String, String)> = HashSet::new();

        if cancellation_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolLoopCancelled.into());
        }

        // Shared iteration budget: parent + subagents share a global counter
        if let Some(ref budget) = shared_budget {
            let remaining = budget.load(std::sync::atomic::Ordering::Relaxed);
            if remaining == 0 {
                tracing::warn!("Shared iteration budget exhausted at iteration {iteration}");
                break;
            }
            budget.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        // Preemptive context management: trim history before it overflows
        if context_token_budget > 0 {
            let estimated = estimate_history_tokens(history);
            if estimated > context_token_budget {
                tracing::info!(
                    estimated,
                    budget = context_token_budget,
                    iteration = iteration + 1,
                    "Preemptive context trim: estimated tokens exceed budget"
                );
                let chars_saved = fast_trim_tool_results(history, 4);
                if chars_saved > 0 {
                    tracing::info!(chars_saved, "Preemptive fast-trim applied");
                }
                // If still over budget, use the history pruner for deeper cleanup
                let recheck = estimate_history_tokens(history);
                if recheck > context_token_budget {
                    let stats = crate::agent::history_pruner::prune_history(
                        history,
                        &crate::agent::history_pruner::HistoryPrunerConfig {
                            enabled: true,
                            max_tokens: context_token_budget,
                            keep_recent: 4,
                            collapse_tool_results: true,
                        },
                    );
                    if stats.dropped_messages > 0 || stats.collapsed_pairs > 0 {
                        tracing::info!(
                            collapsed = stats.collapsed_pairs,
                            dropped = stats.dropped_messages,
                            "Preemptive history prune applied"
                        );
                    }
                }
            }
        }

        // Check if model switch was requested via model_switch tool
        if let Some(ref callback) = model_switch_callback
            && let Ok(guard) = callback.lock()
            && let Some((new_provider, new_model)) = guard.as_ref()
            && (new_provider != provider_name || new_model != model)
        {
            tracing::info!(
                "Model switch detected: {} {} -> {} {}",
                provider_name,
                model,
                new_provider,
                new_model
            );
            return Err(ModelSwitchRequested {
                provider: new_provider.clone(),
                model: new_model.clone(),
            }
            .into());
        }

        // Rebuild tool_specs each iteration so newly activated deferred tools appear.
        let mut tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
            .iter()
            .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
            .map(|tool| tool.spec())
            .collect();
        if let Some(at) = activated_tools {
            for spec in at.lock().unwrap().tool_specs() {
                if !excluded_tools.iter().any(|ex| ex == &spec.name) {
                    tool_specs.push(spec);
                }
            }
        }
        let use_native_tools = provider.supports_native_tools() && !tool_specs.is_empty();

        let image_marker_count = multimodal::count_image_markers(history);

        // ── Vision provider routing ──────────────────────────
        // When the default provider lacks vision support but a dedicated
        // vision_provider is configured, create it on demand and use it
        // for this iteration.  Otherwise, preserve the original error.
        let vision_provider_box: Option<Box<dyn Provider>> = if image_marker_count > 0
            && !provider.supports_vision()
        {
            if let Some(ref vp) = multimodal_config.vision_provider {
                let vp_instance = zeroclaw_providers::create_provider(vp, None)
                    .map_err(|e| anyhow::anyhow!("failed to create vision provider '{vp}': {e}"))?;
                if !vp_instance.supports_vision() {
                    return Err(ProviderCapabilityError {
                        provider: vp.clone(),
                        capability: "vision".to_string(),
                        message: format!(
                            "configured vision_provider '{vp}' does not support vision input"
                        ),
                    }
                    .into());
                }
                Some(vp_instance)
            } else {
                return Err(ProviderCapabilityError {
                        provider: provider_name.to_string(),
                        capability: "vision".to_string(),
                        message: format!(
                            "received {image_marker_count} image marker(s), but this provider does not support vision input"
                        ),
                    }
                    .into());
            }
        } else {
            None
        };

        let (active_provider, active_provider_name, active_model): (&dyn Provider, &str, &str) =
            if let Some(ref vp_box) = vision_provider_box {
                let vp_name = multimodal_config
                    .vision_provider
                    .as_deref()
                    .unwrap_or(provider_name);
                let vm = multimodal_config.vision_model.as_deref().unwrap_or(model);
                (vp_box.as_ref(), vp_name, vm)
            } else {
                (provider, provider_name, model)
            };

        let prepared_messages =
            multimodal::prepare_messages_for_provider(history, multimodal_config).await?;

        // ── Progress: LLM thinking ────────────────────────────
        if let Some(ref tx) = on_delta {
            let phase = if iteration == 0 {
                "\u{1f914} Thinking...\n".to_string()
            } else {
                format!("\u{1f914} Thinking (round {})...\n", iteration + 1)
            };
            let _ = tx.send(StreamDelta::Status(phase)).await;
        }

        observer.record_event(&ObserverEvent::LlmRequest {
            provider: active_provider_name.to_string(),
            model: active_model.to_string(),
            messages_count: history.len(),
            prompt_content: None,
        });
        runtime_trace::record_event(
            "llm_request",
            Some(channel_name),
            Some(active_provider_name),
            Some(active_model),
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
            hooks.fire_llm_input(history, model).await;
        }

        // Budget enforcement — block if limit exceeded (no-op when not scoped)
        if let Some(BudgetCheck::Exceeded {
            current_usd,
            limit_usd,
            period,
        }) = check_tool_loop_budget()
        {
            return Err(anyhow::anyhow!(
                "Budget exceeded: ${:.4} of ${:.2} {:?} limit. Cannot make further API calls until the budget resets.",
                current_usd,
                limit_usd,
                period
            ));
        }

        // Unified path via Provider::chat so provider-specific native tool logic
        // (OpenAI/Anthropic/OpenRouter/compatible adapters) is honored.
        let request_tools = if use_native_tools {
            Some(tool_specs.as_slice())
        } else {
            None
        };
        let should_consume_provider_stream = on_delta.is_some()
            && provider.supports_streaming()
            && (request_tools.is_none() || provider.supports_streaming_tool_events());
        tracing::debug!(
            has_on_delta = on_delta.is_some(),
            supports_streaming = provider.supports_streaming(),
            should_consume_provider_stream,
            "Streaming decision for iteration {}",
            iteration + 1,
        );
        let mut streamed_live_deltas = false;

        let chat_result = if should_consume_provider_stream {
            match consume_provider_streaming_response(
                active_provider,
                &prepared_messages.messages,
                request_tools,
                active_model,
                temperature,
                cancellation_token.as_ref(),
                on_delta.as_ref(),
            )
            .await
            {
                Ok(streamed) => {
                    streamed_live_deltas = streamed.forwarded_live_deltas;
                    Ok(zeroclaw_providers::ChatResponse {
                        text: Some(streamed.response_text),
                        tool_calls: streamed.tool_calls,
                        usage: None,
                        reasoning_content: None,
                    })
                }
                Err(stream_err) => {
                    tracing::warn!(
                        provider = active_provider_name,
                        model = active_model,
                        iteration = iteration + 1,
                        "provider streaming failed, falling back to non-streaming chat: {stream_err}"
                    );
                    runtime_trace::record_event(
                        "llm_stream_fallback",
                        Some(channel_name),
                        Some(active_provider_name),
                        Some(active_model),
                        Some(&turn_id),
                        Some(false),
                        Some("provider stream failed; fallback to non-streaming chat"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "error": scrub_credentials(&stream_err.to_string()),
                        }),
                    );
                    {
                        let chat_future = active_provider.chat(
                            ChatRequest {
                                messages: &prepared_messages.messages,
                                tools: request_tools,
                            },
                            active_model,
                            temperature,
                        );
                        if let Some(token) = cancellation_token.as_ref() {
                            tokio::select! {
                                () = token.cancelled() => Err(ToolLoopCancelled.into()),
                                result = chat_future => result,
                            }
                        } else {
                            chat_future.await
                        }
                    }
                }
            }
        } else {
            // Non-streaming path: wrap with optional per-step timeout from
            // pacing config to catch hung model responses.
            let chat_future = active_provider.chat(
                ChatRequest {
                    messages: &prepared_messages.messages,
                    tools: request_tools,
                },
                active_model,
                temperature,
            );

            match pacing.step_timeout_secs {
                Some(step_secs) if step_secs > 0 => {
                    let step_timeout = Duration::from_secs(step_secs);
                    if let Some(token) = cancellation_token.as_ref() {
                        tokio::select! {
                            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                            result = tokio::time::timeout(step_timeout, chat_future) => {
                                match result {
                                    Ok(inner) => inner,
                                    Err(_) => anyhow::bail!(
                                        "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                                    ),
                                }
                            },
                        }
                    } else {
                        match tokio::time::timeout(step_timeout, chat_future).await {
                            Ok(inner) => inner,
                            Err(_) => anyhow::bail!(
                                "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                            ),
                        }
                    }
                }
                _ => {
                    if let Some(token) = cancellation_token.as_ref() {
                        tokio::select! {
                            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                            result = chat_future => result,
                        }
                    } else {
                        chat_future.await
                    }
                }
            }
        };

        let (
            response_text,
            parsed_text,
            tool_calls,
            assistant_history_content,
            native_tool_calls,
            _parse_issue_detected,
            response_streamed_live,
        ) = match chat_result {
            Ok(resp) => {
                let (resp_input_tokens, resp_output_tokens) = resp
                    .usage
                    .as_ref()
                    .map(|u| (u.input_tokens, u.output_tokens))
                    .unwrap_or((None, None));

                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: model.to_string(),
                    duration: llm_started_at.elapsed(),
                    success: true,
                    error_message: None,
                    input_tokens: resp_input_tokens,
                    output_tokens: resp_output_tokens,
                    response_content: None,
                });

                // Record cost via task-local tracker (no-op when not scoped)
                let _ = resp
                    .usage
                    .as_ref()
                    .and_then(|usage| record_tool_loop_cost_usage(provider_name, model, usage));

                let response_text = resp.text_or_empty().to_string();
                // First try native structured tool calls (OpenAI-format).
                // Fall back to text-based parsing (XML tags, markdown blocks,
                // GLM format) only if the provider returned no native calls —
                // this ensures we support both native and prompt-guided models.
                let mut calls: Vec<ParsedToolCall> = resp
                    .tool_calls
                    .iter()
                    .map(|call| ParsedToolCall {
                        name: call.name.clone(),
                        arguments: serde_json::from_str::<serde_json::Value>(&call.arguments)
                            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
                        tool_call_id: Some(call.id.clone()),
                    })
                    .collect();
                let mut parsed_text = String::new();

                if calls.is_empty() {
                    let (fallback_text, fallback_calls) = parse_tool_calls(&response_text);
                    if !fallback_text.is_empty() {
                        parsed_text = fallback_text;
                    }
                    calls = fallback_calls;
                }

                let parse_issue = detect_tool_call_parse_issue(&response_text, &calls);
                if let Some(ref issue) = parse_issue {
                    runtime_trace::record_event(
                        "tool_call_parse_issue",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some(issue.as_str()),
                        serde_json::json!({
                            "iteration": iteration + 1,
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
                    Some(model),
                    Some(&turn_id),
                    Some(true),
                    None,
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                        "input_tokens": resp_input_tokens,
                        "output_tokens": resp_output_tokens,
                        "raw_response": scrub_credentials(&response_text),
                        "native_tool_calls": resp.tool_calls.len(),
                        "parsed_tool_calls": calls.len(),
                    }),
                );

                // Preserve native tool call IDs in assistant history so role=tool
                // follow-up messages can reference the exact call id.
                let reasoning_content = resp.reasoning_content.clone();
                let assistant_history_content = if resp.tool_calls.is_empty() {
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
                        &resp.tool_calls,
                        reasoning_content.as_deref(),
                    )
                };

                let native_calls = resp.tool_calls;
                (
                    response_text,
                    parsed_text,
                    calls,
                    assistant_history_content,
                    native_calls,
                    parse_issue.is_some(),
                    streamed_live_deltas,
                )
            }
            Err(e) => {
                let safe_error = zeroclaw_providers::sanitize_api_error(&e.to_string());
                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: model.to_string(),
                    duration: llm_started_at.elapsed(),
                    success: false,
                    error_message: Some(safe_error.clone()),
                    input_tokens: None,
                    output_tokens: None,
                    response_content: None,
                });
                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(&safe_error),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                    }),
                );

                // Context overflow recovery: trim history and retry
                if zeroclaw_providers::reliable::is_context_window_exceeded(&e) {
                    tracing::warn!(
                        iteration = iteration + 1,
                        "Context window exceeded, attempting in-loop recovery"
                    );

                    // Step 1: fast-trim old tool results (cheap)
                    let chars_saved = fast_trim_tool_results(history, 4);
                    if chars_saved > 0 {
                        tracing::info!(
                            chars_saved,
                            "Context recovery: trimmed old tool results, retrying"
                        );
                        continue;
                    }

                    // Step 2: emergency drop oldest non-system messages
                    let dropped = emergency_history_trim(history, 4);
                    if dropped > 0 {
                        tracing::info!(dropped, "Context recovery: dropped old messages, retrying");
                        continue;
                    }

                    // Nothing left to trim — truly unrecoverable
                    tracing::error!("Context overflow unrecoverable: no trimmable messages");
                }

                return Err(e);
            }
        };

        let display_text = if parsed_text.is_empty() {
            response_text.clone()
        } else {
            parsed_text
        };
        // ── Progress: LLM responded ─────────────────────────────
        if let Some(ref tx) = on_delta {
            let llm_secs = llm_started_at.elapsed().as_secs();
            if !tool_calls.is_empty() {
                let _ = tx
                    .send(StreamDelta::Status(format!(
                        "\u{1f4ac} Got {} tool call(s) ({llm_secs}s)\n",
                        tool_calls.len()
                    )))
                    .await;
            }
        }

        if tool_calls.is_empty() {
            runtime_trace::record_event(
                "turn_final_response",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(true),
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "text": scrub_credentials(&display_text),
                }),
            );
            // No tool calls — this is the final response.
            accumulated_display_text.push_str(&display_text);

            // If text wasn't streamed live, send it now via post-hoc chunking.
            // When streamed live, the channel already received the deltas.
            if let Some(ref tx) = on_delta
                && !response_streamed_live
            {
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
                        && tx
                            .send(StreamDelta::Text(std::mem::take(&mut chunk)))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
                if !chunk.is_empty() {
                    let _ = tx.send(StreamDelta::Text(chunk)).await;
                }
            }

            history.push(ChatMessage::assistant(response_text.clone()));
            return Ok(accumulated_display_text);
        }

        // Accumulate text from this iteration (tool calls present, loop continues).
        accumulated_display_text.push_str(&display_text);

        // Native tool-call providers can return assistant text separately from
        // the structured call payload; relay it to draft-capable channels.
        if !display_text.is_empty() {
            if !native_tool_calls.is_empty()
                && let Some(ref tx) = on_delta
            {
                let mut narration = display_text.clone();
                if !narration.ends_with('\n') {
                    narration.push('\n');
                }
                let _ = tx.send(StreamDelta::Text(narration)).await;
            }
            if !silent {
                print!("{display_text}");
                let _ = std::io::stdout().flush();
            }
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
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&cancelled),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": call.name,
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        if let Some(ref tx) = on_delta {
                            let _ = tx
                                .send(StreamDelta::Status(format!(
                                    "\u{274c} {}: {}\n",
                                    call.name,
                                    truncate_with_ellipsis(&scrub_credentials(&cancelled), 200)
                                )))
                                .await;
                        }
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

            maybe_inject_channel_delivery_defaults(
                &tool_name,
                &mut tool_args,
                channel_name,
                channel_reply_target,
            );

            // ── Approval hook ────────────────────────────────
            if let Some(mgr) = approval
                && mgr.needs_approval(&tool_name)
            {
                let request = ApprovalRequest {
                    tool_name: tool_name.clone(),
                    arguments: tool_args.clone(),
                };

                // Interactive CLI: prompt the operator.
                // Non-interactive (channels): auto-deny since no operator
                // is present to approve.
                let decision = if mgr.is_non_interactive() {
                    ApprovalResponse::No
                } else {
                    mgr.prompt_cli(&request)
                };

                mgr.record_decision(&tool_name, &tool_args, decision, channel_name);

                if decision == ApprovalResponse::No {
                    let denied = "Denied by user.".to_string();
                    runtime_trace::record_event(
                        "tool_call_result",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some(&denied),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "tool": tool_name.clone(),
                            "arguments": scrub_credentials(&tool_args.to_string()),
                        }),
                    );
                    if let Some(ref tx) = on_delta {
                        let _ = tx
                            .send(StreamDelta::Status(format!(
                                "\u{274c} {}: {}\n",
                                tool_name, denied
                            )))
                            .await;
                    }
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

            let signature = {
                let canonical_args = canonicalize_json_for_tool_signature(&tool_args);
                let args_json =
                    serde_json::to_string(&canonical_args).unwrap_or_else(|_| "{}".to_string());
                (tool_name.trim().to_ascii_lowercase(), args_json)
            };
            let dedup_exempt = dedup_exempt_tools.iter().any(|e| e == &tool_name);
            if !dedup_exempt && !seen_tool_signatures.insert(signature) {
                let duplicate = format!(
                    "Skipped duplicate tool call '{tool_name}' with identical arguments in this turn."
                );
                runtime_trace::record_event(
                    "tool_call_result",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
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
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(StreamDelta::Status(format!(
                            "\u{274c} {}: {}\n",
                            tool_name, duplicate
                        )))
                        .await;
                }
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
                Some(model),
                Some(&turn_id),
                None,
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": tool_name.clone(),
                    "arguments": scrub_credentials(&tool_args.to_string()),
                }),
            );

            // ── Progress: tool start ────────────────────────────
            if let Some(ref tx) = on_delta {
                let hint = {
                    let raw = match tool_name.as_str() {
                        "shell" => tool_args.get("command").and_then(|v| v.as_str()),
                        "file_read" | "file_write" => {
                            tool_args.get("path").and_then(|v| v.as_str())
                        }
                        _ => tool_args
                            .get("action")
                            .and_then(|v| v.as_str())
                            .or_else(|| tool_args.get("query").and_then(|v| v.as_str())),
                    };
                    match raw {
                        Some(s) => truncate_with_ellipsis(s, 60),
                        None => String::new(),
                    }
                };
                let progress = if hint.is_empty() {
                    format!("\u{23f3} {}\n", tool_name)
                } else {
                    format!("\u{23f3} {}: {hint}\n", tool_name)
                };
                tracing::debug!(tool = %tool_name, "Sending progress start to draft");
                let _ = tx.send(StreamDelta::Status(progress)).await;
            }

            executable_indices.push(idx);
            executable_calls.push(ParsedToolCall {
                name: tool_name,
                arguments: tool_args,
                tool_call_id: call.tool_call_id.clone(),
            });
        }

        let executed_outcomes = if allow_parallel_execution && executable_calls.len() > 1 {
            execute_tools_parallel(
                &executable_calls,
                tools_registry,
                activated_tools,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        } else {
            execute_tools_sequential(
                &executable_calls,
                tools_registry,
                activated_tools,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        };

        for ((idx, call), outcome) in executable_indices
            .iter()
            .zip(executable_calls.iter())
            .zip(executed_outcomes.into_iter())
        {
            runtime_trace::record_event(
                "tool_call_result",
                Some(channel_name),
                Some(provider_name),
                Some(model),
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
                let tool_result_obj = crate::tools::ToolResult {
                    success: outcome.success,
                    output: outcome.output.clone(),
                    error: None,
                };
                hooks
                    .fire_after_tool_call(&call.name, &tool_result_obj, outcome.duration)
                    .await;
            }

            // ── Progress: tool completion ───────────────────────
            if let Some(ref tx) = on_delta {
                let secs = outcome.duration.as_secs();
                let progress_msg = if outcome.success {
                    format!("\u{2705} {} ({secs}s)\n", call.name)
                } else if let Some(ref reason) = outcome.error_reason {
                    format!(
                        "\u{274c} {} ({secs}s): {}\n",
                        call.name,
                        truncate_with_ellipsis(reason, 200)
                    )
                } else {
                    format!("\u{274c} {} ({secs}s)\n", call.name)
                };
                tracing::debug!(tool = %call.name, secs, "Sending progress complete to draft");
                let _ = tx.send(StreamDelta::Status(progress_msg)).await;
            }

            ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
        }

        // Collect tool results and build per-tool output for loop detection.
        // Only non-ignored tool outputs contribute to the identical-output hash.
        let mut detection_relevant_output = String::new();
        // Use enumerate *before* filter_map so result_index stays aligned with
        // tool_calls even when some ordered_results entries are None.
        for (result_index, (tool_name, tool_call_id, outcome)) in ordered_results
            .into_iter()
            .enumerate()
            .filter_map(|(i, opt)| opt.map(|v| (i, v)))
        {
            if !loop_ignore_tools.contains(tool_name.as_str()) {
                detection_relevant_output.push_str(&outcome.output);

                // Feed the pattern-based loop detector with name + args + result.
                let args = tool_calls
                    .get(result_index)
                    .map(|c| &c.arguments)
                    .unwrap_or(&serde_json::Value::Null);
                let det_result = loop_detector.record(&tool_name, args, &outcome.output);
                match det_result {
                    crate::agent::loop_detector::LoopDetectionResult::Ok => {}
                    crate::agent::loop_detector::LoopDetectionResult::Warning(ref msg) => {
                        tracing::warn!(tool = %tool_name, %msg, "loop detector warning");
                        // Inject a system nudge so the LLM adjusts strategy.
                        history.push(ChatMessage::system(format!("[Loop Detection] {msg}")));
                    }
                    crate::agent::loop_detector::LoopDetectionResult::Block(ref msg) => {
                        tracing::warn!(tool = %tool_name, %msg, "loop detector blocked tool call");
                        // Replace the tool output with the block message.
                        // We still continue the loop so the LLM sees the block feedback.
                        history.push(ChatMessage::system(format!(
                            "[Loop Detection — BLOCKED] {msg}"
                        )));
                    }
                    crate::agent::loop_detector::LoopDetectionResult::Break(msg) => {
                        runtime_trace::record_event(
                            "loop_detector_circuit_breaker",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&msg),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": tool_name,
                            }),
                        );
                        anyhow::bail!("Agent loop aborted by loop detector: {msg}");
                    }
                }
            }
            let result_output = truncate_tool_result(&outcome.output, max_tool_result_chars);
            individual_results.push((tool_call_id, result_output.clone()));
            let _ = writeln!(
                tool_results,
                "<tool_result name=\"{}\">\n{}\n</tool_result>",
                tool_name, result_output
            );
        }

        // ── Time-gated loop detection ──────────────────────────
        // When pacing.loop_detection_min_elapsed_secs is set, identical-output
        // loop detection activates after the task has been running that long.
        // This avoids false-positive aborts on long-running browser/research
        // workflows while keeping aggressive protection for quick tasks.
        // When not configured, identical-output detection is disabled (preserving
        // existing behavior where only max_iterations prevents runaway loops).
        let loop_detection_active = match pacing.loop_detection_min_elapsed_secs {
            Some(min_secs) => loop_started_at.elapsed() >= Duration::from_secs(min_secs),
            None => false, // disabled when not configured (backwards compatible)
        };

        if loop_detection_active && !detection_relevant_output.is_empty() {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            detection_relevant_output.hash(&mut hasher);
            let current_hash = hasher.finish();

            if last_tool_output_hash == Some(current_hash) {
                consecutive_identical_outputs += 1;
            } else {
                consecutive_identical_outputs = 0;
                last_tool_output_hash = Some(current_hash);
            }

            // Bail if we see 3+ consecutive identical tool outputs (clear runaway).
            if consecutive_identical_outputs >= 3 {
                runtime_trace::record_event(
                    "tool_loop_identical_output_abort",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some("identical tool output detected 3 consecutive times"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "consecutive_identical": consecutive_identical_outputs,
                    }),
                );
                anyhow::bail!(
                    "Agent loop aborted: identical tool output detected {} consecutive times",
                    consecutive_identical_outputs
                );
            }
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
    }

    runtime_trace::record_event(
        "tool_loop_exhausted",
        Some(channel_name),
        Some(provider_name),
        Some(model),
        Some(&turn_id),
        Some(false),
        Some("agent exceeded maximum tool iterations"),
        serde_json::json!({
            "max_iterations": max_iterations,
        }),
    );

    // Graceful shutdown: ask the LLM for a final summary without tools
    tracing::warn!(
        max_iterations,
        "Max iterations reached, requesting final summary"
    );
    history.push(ChatMessage::user(
        "You have reached the maximum number of tool iterations. \
         Please provide your best answer based on the work completed so far. \
         Summarize what you accomplished and what remains to be done."
            .to_string(),
    ));

    let summary_request = zeroclaw_providers::ChatRequest {
        messages: history,
        tools: None, // No tools — force a text response
    };
    match provider.chat(summary_request, model, temperature).await {
        Ok(resp) => {
            let text = resp.text.unwrap_or_default();
            if text.is_empty() {
                anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
            }
            accumulated_display_text.push_str(&text);
            Ok(accumulated_display_text)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Final summary LLM call failed, bailing");
            anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
        }
    }
}

/// Build the tool instruction block for the system prompt so the LLM knows
/// how to invoke tools.
pub fn build_tool_instructions(
    tools_registry: &[Box<dyn Tool>],
    tool_descriptions: Option<&ToolDescriptions>,
) -> String {
    let mut instructions = String::new();
    instructions.push_str("\n## Tool Use Protocol\n\n");
    instructions.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    instructions.push_str("```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n```\n\n");
    instructions.push_str(
        "CRITICAL: Output actual <tool_call> tags—never describe steps or give examples.\n\n",
    );
    instructions.push_str("Example: User says \"what's the date?\". You MUST respond with:\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n</tool_call>\n\n");
    instructions.push_str("You may use multiple tool calls in a single response. ");
    instructions.push_str("After tool execution, results appear in <tool_result> tags. ");
    instructions
        .push_str("Continue reasoning with the results until you can give a final answer.\n\n");
    instructions.push_str("### Available Tools\n\n");

    for tool in tools_registry {
        let desc = tool_descriptions
            .and_then(|td| td.get(tool.name()))
            .unwrap_or_else(|| tool.description());
        let _ = writeln!(
            instructions,
            "**{}**: {}\nParameters: `{}`\n",
            tool.name(),
            desc,
            tool.parameters_schema()
        );
    }

    instructions
}

// ── CLI Entrypoint ───────────────────────────────────────────────────────
// Wires up all subsystems (observer, runtime, security, memory, tools,
// provider, hardware RAG, peripherals) and enters either single-shot or
// interactive REPL mode. The interactive loop manages history compaction
// and hard trimming to keep the context window bounded.

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
    peripheral_overrides: Vec<String>,
    interactive: bool,
    session_state_file: Option<PathBuf>,
    allowed_tools: Option<Vec<String>>,
) -> Result<String> {
    // ── Wire up agnostic subsystems ──────────────────────────────
    let base_observer = observability::create_observer(&config.observability);
    let observer: Arc<dyn Observer> = Arc::from(base_observer);
    let runtime: Arc<dyn platform::RuntimeAdapter> =
        Arc::from(platform::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    let fallback_provider_loop = config.providers.fallback_provider();

    // ── Memory (the brain) ────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(zeroclaw_memory::create_memory_with_storage_and_routes(
        &config.memory,
        &config.providers.embedding_routes,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        fallback_provider_loop.and_then(|e| e.api_key.as_deref()),
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
    let (
        mut tools_registry,
        delegate_handle,
        _reaction_handle,
        _channel_map_handle,
        _ask_user_handle,
        _escalate_handle,
    ) = tools::all_tools_with_runtime(
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
        fallback_provider_loop.and_then(|e| e.api_key.as_deref()),
        &config,
        None,
    );

    let peripheral_tools: Vec<Box<dyn Tool>> = if let Some(f) = PERIPHERAL_TOOLS_FN.get() {
        f(config.peripherals.clone()).await.unwrap_or_default()
    } else {
        vec![]
    };
    if !peripheral_tools.is_empty() {
        tracing::info!(count = peripheral_tools.len(), "Peripheral tools added");
        tools_registry.extend(peripheral_tools);
    }

    // ── Capability-based tool access control ─────────────────────
    // When `allowed_tools` is `Some(list)`, restrict the tool registry to only
    // those tools whose name appears in the list. Unknown names are silently
    // ignored. When `None`, all tools remain available (backward compatible).
    if let Some(ref allow_list) = allowed_tools {
        tools_registry.retain(|t| allow_list.iter().any(|name| name == t.name()));
        tracing::info!(
            allowed = allow_list.len(),
            retained = tools_registry.len(),
            "Applied capability-based tool access filter"
        );
    }

    // ── Wire MCP tools (non-fatal) — CLI path ────────────────────
    // NOTE: MCP tools are injected after built-in tool filtering
    // (filter_primary_agent_tools_or_fail / agent.allowed_tools / agent.denied_tools).
    // MCP servers are user-declared external integrations; the built-in allow/deny
    // filter is not appropriate for them and would silently drop all MCP tools when
    // a restrictive allowlist is configured. Keep this block after any such filter call.
    //
    // When `deferred_loading` is enabled, MCP tools are NOT added to the registry
    // eagerly. Instead, a `tool_search` built-in is registered so the LLM can
    // fetch schemas on demand. This reduces context window waste.
    let mut deferred_section = String::new();
    let mut activated_handle: Option<
        std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>,
    > = None;
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match crate::tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                if config.mcp.deferred_loading {
                    // Deferred path: build stubs and register tool_search
                    let deferred_set = crate::tools::DeferredMcpToolSet::from_registry(
                        std::sync::Arc::clone(&registry),
                    )
                    .await;
                    tracing::info!(
                        "MCP deferred: {} tool stub(s) from {} server(s)",
                        deferred_set.len(),
                        registry.server_count()
                    );
                    deferred_section = crate::tools::build_deferred_tools_section(&deferred_set);
                    let activated = std::sync::Arc::new(std::sync::Mutex::new(
                        crate::tools::ActivatedToolSet::new(),
                    ));
                    activated_handle = Some(std::sync::Arc::clone(&activated));
                    tools_registry.push(Box::new(crate::tools::ToolSearchTool::new(
                        deferred_set,
                        activated,
                    )));
                } else {
                    // Eager path: register all MCP tools directly
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    for name in names {
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: std::sync::Arc<dyn Tool> =
                                std::sync::Arc::new(crate::tools::McpToolWrapper::new(
                                    name,
                                    def,
                                    std::sync::Arc::clone(&registry),
                                ));
                            if let Some(ref handle) = delegate_handle {
                                handle.write().push(std::sync::Arc::clone(&wrapper));
                            }
                            tools_registry.push(Box::new(crate::tools::ArcToolRef(wrapper)));
                            registered += 1;
                        }
                    }
                    tracing::info!(
                        "MCP: {} tool(s) registered from {} server(s)",
                        registered,
                        registry.server_count()
                    );
                }
            }
            Err(e) => {
                tracing::error!("MCP registry failed to initialize: {e:#}");
            }
        }
    }

    // ── Resolve provider ─────────────────────────────────────────
    let mut provider_name = provider_override
        .as_deref()
        .or(config.providers.fallback.as_deref())
        .unwrap_or("openrouter")
        .to_string();

    let mut model_name = model_override
        .as_deref()
        .or(fallback_provider_loop.and_then(|e| e.model.as_deref()))
        .unwrap_or("anthropic/claude-sonnet-4")
        .to_string();

    let provider_runtime_options =
        zeroclaw_providers::provider_runtime_options_from_config(&config);

    let mut provider: Box<dyn Provider> = zeroclaw_providers::create_routed_provider_with_options(
        &provider_name,
        fallback_provider_loop.and_then(|e| e.api_key.as_deref()),
        fallback_provider_loop.and_then(|e| e.base_url.as_deref()),
        &config.reliability,
        &config.providers.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;

    let model_switch_callback = get_model_switch_state();

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

    // ── Load locale-aware tool descriptions ────────────────────────
    let i18n_locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(crate::i18n::detect_locale);
    let i18n_search_dirs = crate::i18n::default_search_dirs(&config.workspace_dir);
    let i18n_descs = crate::i18n::ToolDescriptions::load(&i18n_locale, &i18n_search_dirs);

    // ── Build system prompt from workspace MD files (OpenClaw framework) ──
    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);

    // Register skill-defined tools as callable tool specs in the tool registry
    // so the LLM can invoke them via native function calling, not just XML prompts.
    tools::register_skill_tools(&mut tools_registry, &skills, security.clone());

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
    let mut system_prompt = crate::agent::system_prompt::build_system_prompt_with_mode_and_autonomy(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        Some(&config.autonomy),
        native_tools,
        config.skills.prompt_injection_mode,
        config.agent.compact_context,
        config.agent.max_system_prompt_chars,
    );

    // Append structured tool-use instructions with schemas (only for non-native providers)
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry, Some(&i18n_descs)));
    }

    // Append deferred MCP tool names so the LLM knows what is available
    if !deferred_section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&deferred_section);
    }

    // ── Approval manager (supervised mode) ───────────────────────
    let approval_manager = if interactive {
        Some(ApprovalManager::from_config(&config.autonomy))
    } else {
        None
    };
    let channel_name = if interactive { "cli" } else { "daemon" };
    let memory_session_id = session_state_file.as_deref().and_then(|path| {
        let raw = path.to_string_lossy().trim().to_string();
        if raw.is_empty() {
            None
        } else {
            Some(format!("cli:{raw}"))
        }
    });

    // ── Cost tracking context (scoped for CLI / cron / web agents) ──
    let cost_tracking_context: Option<ToolLoopCostTrackingContext> =
        crate::cost::CostTracker::get_or_init_global(config.cost.clone(), &config.workspace_dir)
            .map(|tracker| {
                ToolLoopCostTrackingContext::new(tracker, Arc::new(config.cost.prices.clone()))
            });

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();

    let mut final_output = String::new();

    // Save the base system prompt before any thinking modifications so
    // the interactive loop can restore it between turns.
    let base_system_prompt = system_prompt.clone();

    if let Some(msg) = message {
        // ── Parse thinking directive from user message ─────────
        let (thinking_directive, effective_msg) =
            match crate::agent::thinking::parse_thinking_directive(&msg) {
                Some((level, remaining)) => {
                    tracing::info!(thinking_level = ?level, "Thinking directive parsed from message");
                    (Some(level), remaining)
                }
                None => (None, msg.clone()),
            };
        let thinking_level = crate::agent::thinking::resolve_thinking_level(
            thinking_directive,
            None,
            &config.agent.thinking,
        );
        let thinking_params = crate::agent::thinking::apply_thinking_level(thinking_level);
        let effective_temperature = crate::agent::thinking::clamp_temperature(
            temperature + thinking_params.temperature_adjustment,
        );

        // Prepend thinking system prompt prefix when present.
        if let Some(ref prefix) = thinking_params.system_prompt_prefix {
            system_prompt = format!("{prefix}\n\n{system_prompt}");
        }

        // Auto-save user message to memory (skip short/trivial messages)
        if config.memory.auto_save
            && effective_msg.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
            && !zeroclaw_memory::should_skip_autosave_content(&effective_msg)
        {
            let user_key = autosave_memory_key("user_msg");
            let _ = mem
                .store(
                    &user_key,
                    &effective_msg,
                    MemoryCategory::Conversation,
                    memory_session_id.as_deref(),
                )
                .await;
        }

        // Inject memory + hardware RAG context into user message
        let mem_context = build_context(
            mem.as_ref(),
            &effective_msg,
            config.memory.min_relevance_score,
            memory_session_id.as_deref(),
        )
        .await;
        let rag_limit = if config.agent.compact_context { 2 } else { 5 };
        let hw_context = hardware_rag
            .as_ref()
            .map(|r| build_hardware_context(r, &effective_msg, &board_names, rag_limit))
            .unwrap_or_default();
        let context = format!("{mem_context}{hw_context}");
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = if context.is_empty() {
            format!("[{now}] {effective_msg}")
        } else {
            format!("{context}[{now}] {effective_msg}")
        };

        let mut history = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&enriched),
        ];

        // Prune history for token efficiency (when enabled).
        if config.agent.history_pruning.enabled {
            let _stats = crate::agent::history_pruner::prune_history(
                &mut history,
                &config.agent.history_pruning,
            );
        }

        // Compute per-turn excluded MCP tools from tool_filter_groups.
        let excluded_tools = compute_excluded_mcp_tools(
            &tools_registry,
            &config.agent.tool_filter_groups,
            &effective_msg,
        );

        #[allow(unused_assignments)]
        let mut response = String::new();
        loop {
            match TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(
                    cost_tracking_context.clone(),
                    run_tool_call_loop(
                        provider.as_ref(),
                        &mut history,
                        &tools_registry,
                        observer.as_ref(),
                        &provider_name,
                        &model_name,
                        effective_temperature,
                        false,
                        approval_manager.as_ref(),
                        channel_name,
                        None,
                        &config.multimodal,
                        config.agent.max_tool_iterations,
                        None,
                        None,
                        None,
                        &excluded_tools,
                        &config.agent.tool_call_dedup_exempt,
                        activated_handle.as_ref(),
                        Some(model_switch_callback.clone()),
                        &config.pacing,
                        config.agent.max_tool_result_chars,
                        config.agent.max_context_tokens,
                        None, // shared_budget
                    ),
                )
                .await
            {
                Ok(resp) => {
                    response = resp;
                    break;
                }
                Err(e) => {
                    if let Some((new_provider, new_model)) = is_model_switch_requested(&e) {
                        tracing::info!(
                            "Model switch requested, switching from {} {} to {} {}",
                            provider_name,
                            model_name,
                            new_provider,
                            new_model
                        );

                        provider = zeroclaw_providers::create_routed_provider_with_options(
                            &new_provider,
                            fallback_provider_loop.and_then(|e| e.api_key.as_deref()),
                            fallback_provider_loop.and_then(|e| e.base_url.as_deref()),
                            &config.reliability,
                            &config.providers.model_routes,
                            &new_model,
                            &provider_runtime_options,
                        )?;

                        provider_name = new_provider;
                        model_name = new_model;

                        clear_model_switch_request();

                        observer.record_event(&ObserverEvent::AgentStart {
                            provider: provider_name.to_string(),
                            model: model_name.to_string(),
                        });

                        continue;
                    }
                    return Err(e);
                }
            }
        }

        // After successful multi-step execution, attempt autonomous skill creation.
        if config.skills.skill_creation.enabled {
            let tool_calls = crate::skills::creator::extract_tool_calls_from_history(&history);
            if tool_calls.len() >= 2 {
                let creator = crate::skills::creator::SkillCreator::new(
                    config.workspace_dir.clone(),
                    config.skills.skill_creation.clone(),
                );
                match creator.create_from_execution(&msg, &tool_calls, None).await {
                    Ok(Some(slug)) => {
                        tracing::info!(slug, "Auto-created skill from execution");
                    }
                    Ok(None) => {
                        tracing::debug!("Skill creation skipped (duplicate or disabled)");
                    }
                    Err(e) => tracing::warn!("Skill creation failed: {e}"),
                }
            }
        }
        final_output = response.clone();
        println!("{response}");
        observer.record_event(&ObserverEvent::TurnComplete);
    } else {
        println!("🦀 ZeroClaw Interactive Mode");
        println!("Type /help for commands.\n");
        let cli = CLI_CHANNEL_FN
            .get()
            .expect("CLI channel factory not registered — call register_cli_channel_fn at startup")(
        );

        // Persistent conversation history across turns
        let mut history = if let Some(path) = session_state_file.as_deref() {
            load_interactive_session_history(path, &system_prompt)?
        } else {
            vec![ChatMessage::system(&system_prompt)]
        };

        loop {
            print!("> ");
            let _ = std::io::stdout().flush();

            // Read raw bytes to avoid UTF-8 validation errors when PTY
            // transport splits multi-byte characters at frame boundaries
            // (e.g. CJK input with spaces over kubectl exec / SSH).
            let mut raw = Vec::new();
            match std::io::BufRead::read_until(&mut std::io::stdin().lock(), b'\n', &mut raw) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    eprintln!("\nError reading input: {e}\n");
                    break;
                }
            }
            let input = String::from_utf8_lossy(&raw).into_owned();

            let user_input = input.trim().to_string();
            if user_input.is_empty() {
                continue;
            }
            match user_input.as_str() {
                "/quit" | "/exit" => break,
                "/help" => {
                    println!("Available commands:");
                    println!("  /help             Show this help message");
                    println!("  /clear /new       Clear conversation history");
                    println!("  /quit /exit       Exit interactive mode");
                    println!(
                        "  /think:<level>    Set reasoning depth (off|minimal|low|medium|high|max)\n"
                    );
                    continue;
                }
                "/clear" | "/new" => {
                    println!(
                        "This will clear the current conversation and delete all session memory."
                    );
                    println!("Core memories (long-term facts/preferences) will be preserved.");
                    print!("Continue? [y/N] ");
                    let _ = std::io::stdout().flush();

                    let mut confirm_raw = Vec::new();
                    if std::io::BufRead::read_until(
                        &mut std::io::stdin().lock(),
                        b'\n',
                        &mut confirm_raw,
                    )
                    .is_err()
                    {
                        continue;
                    }
                    let confirm = String::from_utf8_lossy(&confirm_raw);
                    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
                        println!("Cancelled.\n");
                        continue;
                    }

                    history.clear();
                    history.push(ChatMessage::system(&system_prompt));
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
                    if let Some(path) = session_state_file.as_deref() {
                        save_interactive_session_history(path, &history)?;
                    }
                    continue;
                }
                _ => {}
            }

            // ── Parse thinking directive from interactive input ───
            let (thinking_directive, effective_input) =
                match crate::agent::thinking::parse_thinking_directive(&user_input) {
                    Some((level, remaining)) => {
                        tracing::info!(thinking_level = ?level, "Thinking directive parsed");
                        (Some(level), remaining)
                    }
                    None => (None, user_input.clone()),
                };
            let thinking_level = crate::agent::thinking::resolve_thinking_level(
                thinking_directive,
                None,
                &config.agent.thinking,
            );
            let thinking_params = crate::agent::thinking::apply_thinking_level(thinking_level);
            let turn_temperature = crate::agent::thinking::clamp_temperature(
                temperature + thinking_params.temperature_adjustment,
            );

            // For non-Medium levels, temporarily patch the system prompt with prefix.
            let turn_system_prompt;
            if let Some(ref prefix) = thinking_params.system_prompt_prefix {
                turn_system_prompt = format!("{prefix}\n\n{system_prompt}");
                // Update the system message in history for this turn.
                if let Some(sys_msg) = history.first_mut()
                    && sys_msg.role == "system"
                {
                    sys_msg.content = turn_system_prompt.clone();
                }
            }

            // Auto-save conversation turns (skip short/trivial messages)
            if config.memory.auto_save
                && effective_input.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
                && !zeroclaw_memory::should_skip_autosave_content(&effective_input)
            {
                let user_key = autosave_memory_key("user_msg");
                let _ = mem
                    .store(
                        &user_key,
                        &effective_input,
                        MemoryCategory::Conversation,
                        memory_session_id.as_deref(),
                    )
                    .await;
            }

            // Inject memory + hardware RAG context into user message
            let mem_context = build_context(
                mem.as_ref(),
                &effective_input,
                config.memory.min_relevance_score,
                memory_session_id.as_deref(),
            )
            .await;
            let rag_limit = if config.agent.compact_context { 2 } else { 5 };
            let hw_context = hardware_rag
                .as_ref()
                .map(|r| build_hardware_context(r, &effective_input, &board_names, rag_limit))
                .unwrap_or_default();
            let context = format!("{mem_context}{hw_context}");
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
            let enriched = if context.is_empty() {
                format!("[{now}] {effective_input}")
            } else {
                format!("{context}[{now}] {effective_input}")
            };

            history.push(ChatMessage::user(&enriched));

            // Compute per-turn excluded MCP tools from tool_filter_groups.
            let excluded_tools = compute_excluded_mcp_tools(
                &tools_registry,
                &config.agent.tool_filter_groups,
                &effective_input,
            );

            // Set up streaming channel so tool progress and response
            // content are printed progressively instead of buffered.
            let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);
            let content_was_streamed =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let content_streamed_flag = content_was_streamed.clone();
            let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());

            let consumer_handle = tokio::spawn(async move {
                use std::io::Write;
                while let Some(event) = delta_rx.recv().await {
                    match event {
                        StreamDelta::Status(text) => {
                            if is_tty {
                                let _ = write!(std::io::stderr(), "\x1b[2m{text}\x1b[0m");
                            } else {
                                let _ = write!(std::io::stderr(), "{text}");
                            }
                            let _ = std::io::stderr().flush();
                        }
                        StreamDelta::Text(text) => {
                            content_streamed_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                            print!("{text}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
            });

            // Ctrl+C cancels the in-flight turn instead of killing the process.
            let cancel_token = CancellationToken::new();
            let cancel_token_clone = cancel_token.clone();
            let ctrlc_handle = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_token_clone.cancel();
                }
            });

            let response = loop {
                match TOOL_LOOP_COST_TRACKING_CONTEXT
                    .scope(
                        cost_tracking_context.clone(),
                        run_tool_call_loop(
                            provider.as_ref(),
                            &mut history,
                            &tools_registry,
                            observer.as_ref(),
                            &provider_name,
                            &model_name,
                            turn_temperature,
                            true,
                            approval_manager.as_ref(),
                            channel_name,
                            None,
                            &config.multimodal,
                            config.agent.max_tool_iterations,
                            Some(cancel_token.clone()),
                            Some(delta_tx.clone()),
                            None,
                            &excluded_tools,
                            &config.agent.tool_call_dedup_exempt,
                            activated_handle.as_ref(),
                            Some(model_switch_callback.clone()),
                            &config.pacing,
                            config.agent.max_tool_result_chars,
                            config.agent.max_context_tokens,
                            None, // shared_budget
                        ),
                    )
                    .await
                {
                    Ok(resp) => break resp,
                    Err(e) => {
                        if is_tool_loop_cancelled(&e) {
                            eprintln!("\n\x1b[2m(cancelled)\x1b[0m");
                            break String::new();
                        }
                        if let Some((new_provider, new_model)) = is_model_switch_requested(&e) {
                            tracing::info!(
                                "Model switch requested, switching from {} {} to {} {}",
                                provider_name,
                                model_name,
                                new_provider,
                                new_model
                            );

                            provider = zeroclaw_providers::create_routed_provider_with_options(
                                &new_provider,
                                fallback_provider_loop.and_then(|e| e.api_key.as_deref()),
                                fallback_provider_loop.and_then(|e| e.base_url.as_deref()),
                                &config.reliability,
                                &config.providers.model_routes,
                                &new_model,
                                &provider_runtime_options,
                            )?;

                            provider_name = new_provider;
                            model_name = new_model;

                            clear_model_switch_request();

                            observer.record_event(&ObserverEvent::AgentStart {
                                provider: provider_name.to_string(),
                                model: model_name.to_string(),
                            });

                            continue;
                        }
                        // Context overflow recovery: compress and retry
                        if zeroclaw_providers::reliable::is_context_window_exceeded(&e) {
                            tracing::warn!(
                                "Context overflow in interactive loop, attempting recovery"
                            );
                            let mut compressor =
                                crate::agent::context_compressor::ContextCompressor::new(
                                    config.agent.context_compression.clone(),
                                    config.agent.max_context_tokens,
                                )
                                .with_memory(mem.clone());
                            let error_msg = format!("{e}");
                            match compressor
                                .compress_on_error(
                                    &mut history,
                                    provider.as_ref(),
                                    &model_name,
                                    &error_msg,
                                )
                                .await
                            {
                                Ok(true) => {
                                    tracing::info!(
                                        "Context recovered via compression, retrying turn"
                                    );
                                    continue;
                                }
                                Ok(false) => {
                                    tracing::warn!("Compression ran but couldn't reduce enough");
                                }
                                Err(compress_err) => {
                                    tracing::warn!(
                                        error = %compress_err,
                                        "Compression failed during recovery"
                                    );
                                }
                            }
                        }

                        eprintln!("\nError: {e}\n");
                        break String::new();
                    }
                }
            };

            // Clean up: stop the Ctrl+C listener and flush streaming events.
            ctrlc_handle.abort();
            drop(delta_tx);
            let _ = consumer_handle.await;

            final_output = response.clone();
            if content_was_streamed.load(std::sync::atomic::Ordering::Relaxed) {
                println!();
            } else if let Err(e) = zeroclaw_api::channel::Channel::send(
                &*cli,
                &zeroclaw_api::channel::SendMessage::new(format!("\n{response}\n"), "user"),
            )
            .await
            {
                eprintln!("\nError sending CLI response: {e}\n");
            }
            observer.record_event(&ObserverEvent::TurnComplete);

            // Context compression before hard trimming to preserve long-context signal.
            {
                let compressor = crate::agent::context_compressor::ContextCompressor::new(
                    config.agent.context_compression.clone(),
                    config.agent.max_context_tokens,
                )
                .with_memory(mem.clone());
                match compressor
                    .compress_if_needed(&mut history, provider.as_ref(), &model_name)
                    .await
                {
                    Ok(result) if result.compressed => {
                        tracing::info!(
                            passes = result.passes_used,
                            before = result.tokens_before,
                            after = result.tokens_after,
                            "Context compression complete"
                        );
                    }
                    Ok(_) => {} // No compression needed
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Context compression failed, falling back to history trim"
                        );
                        trim_history(&mut history, config.agent.max_history_messages / 2);
                    }
                }
            }

            // Hard cap as a safety net.
            trim_history(&mut history, config.agent.max_history_messages);

            // Restore base system prompt (remove per-turn thinking prefix).
            if thinking_params.system_prompt_prefix.is_some()
                && let Some(sys_msg) = history.first_mut()
                && sys_msg.role == "system"
            {
                sys_msg.content.clone_from(&base_system_prompt);
            }

            if let Some(path) = session_state_file.as_deref() {
                save_interactive_session_history(path, &history)?;
            }
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
pub async fn process_message(
    config: Config,
    message: &str,
    session_id: Option<&str>,
) -> Result<String> {
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let runtime: Arc<dyn platform::RuntimeAdapter> =
        Arc::from(platform::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let fallback_provider_pm = config.providers.fallback_provider();
    let approval_manager = ApprovalManager::for_non_interactive(&config.autonomy);
    let mem: Arc<dyn Memory> = Arc::from(zeroclaw_memory::create_memory_with_storage_and_routes(
        &config.memory,
        &config.providers.embedding_routes,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        fallback_provider_pm.and_then(|e| e.api_key.as_deref()),
    )?);

    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let (
        mut tools_registry,
        delegate_handle_pm,
        _reaction_handle_pm,
        _channel_map_handle_pm,
        _ask_user_handle_pm,
        _escalate_handle_pm,
    ) = tools::all_tools_with_runtime(
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
        fallback_provider_pm.and_then(|e| e.api_key.as_deref()),
        &config,
        None,
    );
    let peripheral_tools: Vec<Box<dyn Tool>> = if let Some(f) = PERIPHERAL_TOOLS_FN.get() {
        f(config.peripherals.clone()).await.unwrap_or_default()
    } else {
        vec![]
    };
    tools_registry.extend(peripheral_tools);

    // ── Wire MCP tools (non-fatal) — process_message path ────────
    // NOTE: Same ordering contract as the CLI path above — MCP tools must be
    // injected after filter_primary_agent_tools_or_fail (or equivalent built-in
    // tool allow/deny filtering) to avoid MCP tools being silently dropped.
    let mut deferred_section = String::new();
    let mut activated_handle_pm: Option<
        std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>,
    > = None;
    if config.mcp.enabled && !config.mcp.servers.is_empty() {
        tracing::info!(
            "Initializing MCP client — {} server(s) configured",
            config.mcp.servers.len()
        );
        match crate::tools::McpRegistry::connect_all(&config.mcp.servers).await {
            Ok(registry) => {
                let registry = std::sync::Arc::new(registry);
                if config.mcp.deferred_loading {
                    let deferred_set = crate::tools::DeferredMcpToolSet::from_registry(
                        std::sync::Arc::clone(&registry),
                    )
                    .await;
                    tracing::info!(
                        "MCP deferred: {} tool stub(s) from {} server(s)",
                        deferred_set.len(),
                        registry.server_count()
                    );
                    deferred_section = crate::tools::build_deferred_tools_section(&deferred_set);
                    let activated = std::sync::Arc::new(std::sync::Mutex::new(
                        crate::tools::ActivatedToolSet::new(),
                    ));
                    activated_handle_pm = Some(std::sync::Arc::clone(&activated));
                    tools_registry.push(Box::new(crate::tools::ToolSearchTool::new(
                        deferred_set,
                        activated,
                    )));
                } else {
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    for name in names {
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: std::sync::Arc<dyn Tool> =
                                std::sync::Arc::new(crate::tools::McpToolWrapper::new(
                                    name,
                                    def,
                                    std::sync::Arc::clone(&registry),
                                ));
                            if let Some(ref handle) = delegate_handle_pm {
                                handle.write().push(std::sync::Arc::clone(&wrapper));
                            }
                            tools_registry.push(Box::new(crate::tools::ArcToolRef(wrapper)));
                            registered += 1;
                        }
                    }
                    tracing::info!(
                        "MCP: {} tool(s) registered from {} server(s)",
                        registered,
                        registry.server_count()
                    );
                }
            }
            Err(e) => {
                tracing::error!("MCP registry failed to initialize: {e:#}");
            }
        }
    }

    let provider_name = config.providers.fallback.as_deref().unwrap_or("openrouter");
    let model_name = fallback_provider_pm
        .and_then(|e| e.model.clone())
        .unwrap_or_else(|| "anthropic/claude-sonnet-4-20250514".into());
    let provider_runtime_options =
        zeroclaw_providers::provider_runtime_options_from_config(&config);
    let provider: Box<dyn Provider> = zeroclaw_providers::create_routed_provider_with_options(
        provider_name,
        fallback_provider_pm.and_then(|e| e.api_key.as_deref()),
        fallback_provider_pm.and_then(|e| e.base_url.as_deref()),
        &config.reliability,
        &config.providers.model_routes,
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

    // ── Load locale-aware tool descriptions ────────────────────────
    let i18n_locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(crate::i18n::detect_locale);
    let i18n_search_dirs = crate::i18n::default_search_dirs(&config.workspace_dir);
    let i18n_descs = crate::i18n::ToolDescriptions::load(&i18n_locale, &i18n_search_dirs);

    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);

    // Register skill-defined tools as callable tool specs (process_message path).
    tools::register_skill_tools(&mut tools_registry, &skills, security.clone());

    let mut tool_descs: Vec<(&str, &str)> = vec![
        ("shell", "Execute terminal commands."),
        ("file_read", "Read file contents."),
        ("file_write", "Write file contents."),
        ("memory_store", "Save to memory."),
        ("memory_recall", "Search memory."),
        ("memory_forget", "Delete a memory entry."),
        (
            "model_routing_config",
            "Configure default model, scenario routing, and delegate agents.",
        ),
        ("screenshot", "Capture a screenshot."),
        ("image_info", "Read image metadata."),
    ];
    if matches!(
        config.skills.prompt_injection_mode,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Compact
    ) {
        tool_descs.push((
            "read_skill",
            "Load the full source for an available skill by name.",
        ));
    }
    if config.browser.enabled {
        tool_descs.push(("browser_open", "Open approved URLs in browser."));
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

    // Filter out tools excluded for non-CLI channels (gateway counts as non-CLI).
    // Skip when autonomy is `Full` — full-autonomy agents keep all tools.
    if config.autonomy.level != AutonomyLevel::Full {
        let excluded = &config.autonomy.non_cli_excluded_tools;
        if !excluded.is_empty() {
            tool_descs.retain(|(name, _)| !excluded.iter().any(|ex| ex == name));
        }
    }

    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };
    let native_tools = provider.supports_native_tools();
    let mut system_prompt = crate::agent::system_prompt::build_system_prompt_with_mode_and_autonomy(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        Some(&config.autonomy),
        native_tools,
        config.skills.prompt_injection_mode,
        config.agent.compact_context,
        config.agent.max_system_prompt_chars,
    );
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry, Some(&i18n_descs)));
    }
    if !deferred_section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&deferred_section);
    }

    // ── Parse thinking directive from user message ─────────────
    let (thinking_directive, effective_message) =
        match crate::agent::thinking::parse_thinking_directive(message) {
            Some((level, remaining)) => {
                tracing::info!(thinking_level = ?level, "Thinking directive parsed from message");
                (Some(level), remaining)
            }
            None => (None, message.to_string()),
        };
    let thinking_level = crate::agent::thinking::resolve_thinking_level(
        thinking_directive,
        None,
        &config.agent.thinking,
    );
    let thinking_params = crate::agent::thinking::apply_thinking_level(thinking_level);
    let effective_temperature = crate::agent::thinking::clamp_temperature(
        config
            .providers
            .fallback_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            + thinking_params.temperature_adjustment,
    );

    // Prepend thinking system prompt prefix when present.
    if let Some(ref prefix) = thinking_params.system_prompt_prefix {
        system_prompt = format!("{prefix}\n\n{system_prompt}");
    }

    let effective_msg_ref = effective_message.as_str();
    let mem_context = build_context(
        mem.as_ref(),
        effective_msg_ref,
        config.memory.min_relevance_score,
        session_id,
    )
    .await;
    let rag_limit = if config.agent.compact_context { 2 } else { 5 };
    let hw_context = hardware_rag
        .as_ref()
        .map(|r| build_hardware_context(r, effective_msg_ref, &board_names, rag_limit))
        .unwrap_or_default();
    let context = format!("{mem_context}{hw_context}");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let enriched = if context.is_empty() {
        format!("[{now}] {effective_message}")
    } else {
        format!("{context}[{now}] {effective_message}")
    };

    let mut history = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user(&enriched),
    ];
    let mut excluded_tools = compute_excluded_mcp_tools(
        &tools_registry,
        &config.agent.tool_filter_groups,
        effective_msg_ref,
    );
    if config.autonomy.level != AutonomyLevel::Full {
        excluded_tools.extend(config.autonomy.non_cli_excluded_tools.iter().cloned());
    }

    agent_turn(
        provider.as_ref(),
        &mut history,
        &tools_registry,
        observer.as_ref(),
        provider_name,
        &model_name,
        effective_temperature,
        true,
        "daemon",
        None,
        &config.multimodal,
        config.agent.max_tool_iterations,
        Some(&approval_manager),
        &excluded_tools,
        &config.agent.tool_call_dedup_exempt,
        activated_handle_pm.as_ref(),
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        emergency_history_trim, estimate_history_tokens, fast_trim_tool_results,
        load_interactive_session_history, save_interactive_session_history, truncate_tool_result,
    };
    use crate::agent::history::{DEFAULT_MAX_HISTORY_MESSAGES, InteractiveSessionState};
    use crate::agent::tool_execution::execute_one_tool;
    use tempfile::tempdir;
    use zeroclaw_providers::ChatMessage;
    use zeroclaw_tool_call_parser::parse_tool_calls;

    // ── truncate_tool_result tests ────────────────────────────────

    #[test]
    fn truncate_tool_result_short_passthrough() {
        let output = "short output";
        assert_eq!(truncate_tool_result(output, 100), output);
    }

    #[test]
    fn truncate_tool_result_exact_boundary() {
        let output = "a".repeat(100);
        assert_eq!(truncate_tool_result(&output, 100), output);
    }

    #[test]
    fn truncate_tool_result_zero_disables() {
        let output = "a".repeat(200_000);
        assert_eq!(truncate_tool_result(&output, 0), output);
    }

    #[test]
    fn truncate_tool_result_truncates_with_marker() {
        let output = "a".repeat(200);
        let result = truncate_tool_result(&output, 100);
        assert!(result.contains("[... "));
        assert!(result.contains("characters truncated ...]\n\n"));
        // Head should be ~2/3 of 100 = 66, tail ~1/3 = 34
        assert!(result.starts_with("aaa"));
        assert!(result.ends_with("aaa"));
        // Result should be shorter than original
        assert!(result.len() < output.len());
    }

    #[test]
    fn truncate_tool_result_preserves_head_tail_ratio() {
        let output: String = (0u32..1000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let result = truncate_tool_result(&output, 300);
        // Head = 2/3 of 300 = 200 chars, tail = 100 chars
        // Find the marker
        let marker_start = result.find("[... ").unwrap();
        let marker_end = result.find("characters truncated ...]\n\n").unwrap()
            + "characters truncated ...]\n\n".len();
        let head = &result[..marker_start - 2]; // subtract \n\n
        let tail = &result[marker_end..];
        assert!(
            head.len() >= 190 && head.len() <= 210,
            "head len={}",
            head.len()
        );
        assert!(
            tail.len() >= 90 && tail.len() <= 110,
            "tail len={}",
            tail.len()
        );
    }

    #[test]
    fn truncate_tool_result_utf8_boundary_safety() {
        // Create string with multi-byte chars: each emoji is 4 bytes
        let output = "🦀".repeat(100); // 400 bytes
        // This should not panic even with a limit that falls mid-char
        let result = truncate_tool_result(&output, 50);
        assert!(result.contains("[... "));
        // Verify the result is valid UTF-8 (would panic otherwise)
        let _ = result.len();
    }

    #[test]
    fn truncate_tool_result_very_small_max() {
        let output = "abcdefghijklmnopqrstuvwxyz";
        // With max=5, head=3 tail=2 — result includes marker overhead
        // but should not panic and should contain truncation marker
        let result = truncate_tool_result(output, 5);
        assert!(result.contains("[... "));
        // Head (3 chars) + tail (2 chars) from original should be preserved
        assert!(result.starts_with("abc"));
        assert!(result.ends_with("yz"));
    }

    // ── truncate_tool_message tests ─────────────────────────────

    #[test]
    fn truncate_tool_message_preserves_json_structure() {
        use crate::agent::history::truncate_tool_message;
        let big_content = "x".repeat(5000);
        let msg = serde_json::json!({
            "tool_call_id": "call_abc123",
            "content": big_content,
        })
        .to_string();
        let result = truncate_tool_message(&msg, 2000);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["tool_call_id"], "call_abc123");
        assert!(parsed["content"].as_str().unwrap().contains("[... "));
    }

    #[test]
    fn truncate_tool_message_plain_text_fallback() {
        use crate::agent::history::truncate_tool_message;
        let plain = "a".repeat(5000);
        let result = truncate_tool_message(&plain, 2000);
        assert!(result.contains("[... "));
        assert!(result.len() < 5000);
    }

    #[test]
    fn truncate_tool_message_short_passthrough() {
        use crate::agent::history::truncate_tool_message;
        let msg = r#"{"tool_call_id":"call_1","content":"ok"}"#;
        assert_eq!(truncate_tool_message(msg, 2000), msg);
    }

    // ── fast_trim_tool_results tests ────────────────────────────

    #[test]
    fn fast_trim_protects_recent_messages() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::tool("a".repeat(5000)),
            ChatMessage::tool("b".repeat(5000)),
            ChatMessage::user("recent user msg"),
            ChatMessage::tool("c".repeat(5000)), // recent, should be protected
        ];
        // protect_last_n = 2 → last 2 messages protected
        let saved = fast_trim_tool_results(&mut history, 2);
        assert!(saved > 0);
        // First two tool messages should be trimmed
        assert!(history[1].content.len() <= 2100);
        assert!(history[2].content.len() <= 2100);
        // Last tool message (protected) should be unchanged
        assert_eq!(history[4].content.len(), 5000);
    }

    #[test]
    fn fast_trim_skips_non_tool_messages() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("a".repeat(5000)),
            ChatMessage::assistant("b".repeat(5000)),
        ];
        let saved = fast_trim_tool_results(&mut history, 0);
        assert_eq!(saved, 0);
        assert_eq!(history[1].content.len(), 5000);
        assert_eq!(history[2].content.len(), 5000);
    }

    #[test]
    fn fast_trim_small_tool_results_unchanged() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::tool("short result"),
        ];
        let saved = fast_trim_tool_results(&mut history, 0);
        assert_eq!(saved, 0);
        assert_eq!(history[1].content, "short result");
    }

    // ── emergency_history_trim tests ──────────────────────────────

    #[test]
    fn emergency_trim_preserves_system() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("msg1"),
            ChatMessage::assistant("resp1"),
            ChatMessage::user("msg2"),
            ChatMessage::assistant("resp2"),
            ChatMessage::user("msg3"),
        ];
        let dropped = emergency_history_trim(&mut history, 2);
        assert!(dropped > 0);
        // System message should always be preserved
        assert_eq!(history[0].role, "system");
        assert_eq!(history[0].content, "sys");
        // Last 2 messages should be preserved
        let len = history.len();
        assert_eq!(history[len - 1].content, "msg3");
    }

    #[test]
    fn emergency_trim_preserves_recent() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old1"),
            ChatMessage::user("old2"),
            ChatMessage::user("recent1"),
            ChatMessage::user("recent2"),
        ];
        let dropped = emergency_history_trim(&mut history, 2);
        assert!(dropped > 0);
        // Last 2 should be preserved
        let len = history.len();
        assert_eq!(history[len - 1].content, "recent2");
        assert_eq!(history[len - 2].content, "recent1");
    }

    #[test]
    fn emergency_trim_nothing_to_drop() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("only user msg"),
        ];
        // protect_last = 1, system is protected → only 1 droppable
        // target_drop = 2/3 = 0 → nothing dropped
        let dropped = emergency_history_trim(&mut history, 1);
        assert_eq!(dropped, 0);
    }

    // ── estimate_history_tokens tests ─────────────────────────────

    #[test]
    fn estimate_tokens_empty_history() {
        let history: Vec<ChatMessage> = vec![];
        assert_eq!(estimate_history_tokens(&history), 0);
    }

    #[test]
    fn estimate_tokens_single_message() {
        // 40 chars → 40.div_ceil(4) + 4 = 10 + 4 = 14 tokens
        let msg = "a".repeat(40);
        let history = vec![ChatMessage::user(&msg)];
        let est = estimate_history_tokens(&history);
        assert_eq!(est, 14);
    }

    #[test]
    fn estimate_tokens_multiple_messages() {
        let history = vec![
            ChatMessage::system("system prompt here"), // 18 chars → 18/4=4 +4=8 (div_ceil: 5+4=9)
            ChatMessage::user("hello"),                // 5 chars → 5/4=1 +4=5 (div_ceil: 2+4=6)
            ChatMessage::assistant("world"),           // 5 chars → 5/4=1 +4=5 (div_ceil: 2+4=6)
        ];
        let est = estimate_history_tokens(&history);
        // Each message: content_len.div_ceil(4) + 4
        // 18.div_ceil(4)=5, 5.div_ceil(4)=2, 5.div_ceil(4)=2 → 5+4 + 2+4 + 2+4 = 21
        assert_eq!(est, 21);
    }

    #[test]
    fn estimate_tokens_large_tool_result() {
        let big = "x".repeat(40_000);
        let history = vec![ChatMessage::tool(&big)];
        let est = estimate_history_tokens(&history);
        // 40000.div_ceil(4) + 4 = 10000 + 4 = 10004
        assert_eq!(est, 10_004);
    }

    // ── shared_budget tests ───────────────────────────────────────

    #[test]
    fn shared_budget_decrement_logic() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let budget = Arc::new(AtomicUsize::new(3));

        // Simulate 3 iterations decrementing
        for i in 0..3 {
            let remaining = budget.load(Ordering::Relaxed);
            assert!(remaining > 0, "Budget should be >0 at iteration {i}");
            budget.fetch_sub(1, Ordering::Relaxed);
        }

        // Budget should now be 0
        assert_eq!(budget.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_budget_none_has_no_effect() {
        // When shared_budget is None, the check is simply skipped
        let budget: Option<Arc<std::sync::atomic::AtomicUsize>> = None;
        assert!(budget.is_none());
    }

    // ── existing tests ────────────────────────────────────────────

    #[test]
    fn interactive_session_state_round_trips_history() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];

        save_interactive_session_history(&path, &history).unwrap();
        let restored = load_interactive_session_history(&path, "fallback").unwrap();

        assert_eq!(restored.len(), 3);
        assert_eq!(restored[0].role, "system");
        assert_eq!(restored[1].content, "hello");
        assert_eq!(restored[2].content, "hi");
    }

    #[test]
    fn interactive_session_state_adds_missing_system_prompt() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let payload = serde_json::to_string_pretty(&InteractiveSessionState {
            version: 1,
            history: vec![ChatMessage::user("orphan")],
        })
        .unwrap();
        std::fs::write(&path, payload).unwrap();

        let restored = load_interactive_session_history(&path, "fallback system").unwrap();

        assert_eq!(restored[0].role, "system");
        assert_eq!(restored[0].content, "fallback system");
        assert_eq!(restored[1].content, "orphan");
    }

    use super::*;
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn scrub_credentials_redacts_bearer_token() {
        let input = "API_KEY=sk-1234567890abcdef; token: 1234567890; password=\"secret123456\"";
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("API_KEY=sk-1*[REDACTED]"));
        assert!(scrubbed.contains("token: 1234*[REDACTED]"));
        assert!(scrubbed.contains("password=\"secr*[REDACTED]\""));
        assert!(!scrubbed.contains("abcdef"));
        assert!(!scrubbed.contains("secret123456"));
    }

    #[test]
    fn scrub_credentials_redacts_json_api_key() {
        let input = r#"{"api_key": "sk-1234567890", "other": "public"}"#;
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("\"api_key\": \"sk-1*[REDACTED]\""));
        assert!(scrubbed.contains("public"));
    }

    #[tokio::test]
    async fn execute_one_tool_does_not_panic_on_utf8_boundary() {
        let call_arguments = (0..600)
            .map(|n| serde_json::json!({ "content": format!("{}：tail", "a".repeat(n)) }))
            .find(|args| {
                let raw = args.to_string();
                raw.len() > 300 && !raw.is_char_boundary(300)
            })
            .expect("should produce a sample whose byte index 300 is not a char boundary");

        let observer = NoopObserver;
        let result =
            execute_one_tool("unknown_tool", call_arguments, &[], None, &observer, None).await;
        assert!(result.is_ok(), "execute_one_tool should not panic or error");

        let outcome = result.unwrap();
        assert!(!outcome.success);
        assert!(outcome.output.contains("Unknown tool: unknown_tool"));
    }

    #[tokio::test]
    async fn execute_one_tool_resolves_unique_activated_tool_suffix() {
        let observer = NoopObserver;
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "docker-mcp__extract_text",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("docker-mcp__extract_text".into(), activated_tool);

        let outcome = execute_one_tool(
            "extract_text",
            serde_json::json!({ "value": "ok" }),
            &[],
            Some(&activated),
            &observer,
            None,
        )
        .await
        .expect("suffix alias should execute the unique activated tool");

        assert!(outcome.success);
        assert_eq!(outcome.output, "counted:ok");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    use crate::observability::NoopObserver;
    use tempfile::TempDir;
    use zeroclaw_api::provider::{ProviderCapabilities, StreamChunk, StreamEvent, StreamOptions};
    use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};
    use zeroclaw_providers::ChatResponse;
    use zeroclaw_providers::router::{Route, RouterProvider};

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
                prompt_caching: false,
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
            let marker_count =
                zeroclaw_providers::multimodal::count_image_markers(request.messages);
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
            })
        }
    }

    struct ScriptedProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        capabilities: ProviderCapabilities,
    }

    impl ScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
                .collect();
            Self {
                responses: Arc::new(Mutex::new(scripted)),
                capabilities: ProviderCapabilities::default(),
            }
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

    struct StreamingScriptedProvider {
        responses: Arc<Mutex<VecDeque<String>>>,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(
                    responses.into_iter().map(ToString::to_string).collect(),
                )),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Provider for StreamingScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming scripted provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when streaming succeeds")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamChunk>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            let response = self
                .responses
                .lock()
                .expect("responses lock should be valid")
                .pop_front()
                .unwrap_or_default();

            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamChunk::delta(response)),
                Ok(StreamChunk::final_chunk()),
            ]))
        }
    }

    enum NativeStreamTurn {
        ToolCall(ToolCall),
        Text(String),
    }

    struct StreamingNativeToolEventProvider {
        turns: Arc<Mutex<VecDeque<NativeStreamTurn>>>,
        stream_calls: Arc<AtomicUsize>,
        stream_tool_requests: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingNativeToolEventProvider {
        fn with_turns(turns: Vec<NativeStreamTurn>) -> Self {
            Self {
                turns: Arc::new(Mutex::new(turns.into())),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                stream_tool_requests: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Provider for StreamingNativeToolEventProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming native tool event provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when native streaming events succeed")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamEvent>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            if request.tools.is_some_and(|tools| !tools.is_empty()) {
                self.stream_tool_requests.fetch_add(1, Ordering::SeqCst);
            }
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            let turn = self
                .turns
                .lock()
                .expect("turns lock should be valid")
                .pop_front()
                .expect("streaming turns should have scripted output");
            match turn {
                NativeStreamTurn::ToolCall(tool_call) => {
                    Box::pin(futures_util::stream::iter(vec![
                        Ok(StreamEvent::ToolCall(tool_call)),
                        Ok(StreamEvent::Final),
                    ]))
                }
                NativeStreamTurn::Text(text) => Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(text))),
                    Ok(StreamEvent::Final),
                ])),
            }
        }
    }

    struct RouteAwareStreamingProvider {
        response: String,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
        last_model: Arc<Mutex<String>>,
    }

    impl RouteAwareStreamingProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
                last_model: Arc::new(Mutex::new(String::new())),
            }
        }
    }

    #[async_trait]
    impl Provider for RouteAwareStreamingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in route-aware stream tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when routed streaming succeeds")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<StreamChunk>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            *self
                .last_model
                .lock()
                .expect("last_model lock should be valid") = model.to_string();
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamChunk::delta(self.response.clone())),
                Ok(StreamChunk::final_chunk()),
            ]))
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

    struct RecordingArgsTool {
        name: String,
        recorded_args: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl RecordingArgsTool {
        fn new(name: &str, recorded_args: Arc<Mutex<Vec<serde_json::Value>>>) -> Self {
            Self {
                name: name.to_string(),
                recorded_args,
            }
        }
    }

    #[async_trait]
    impl Tool for RecordingArgsTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Records tool arguments for regression tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "schedule": { "type": "object" },
                    "delivery": { "type": "object" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.recorded_args
                .lock()
                .expect("recorded args lock should be valid")
                .push(args.clone());
            Ok(crate::tools::ToolResult {
                success: true,
                output: args.to_string(),
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

    /// A tool that always returns a failure with a given error reason.
    struct FailingTool {
        tool_name: String,
        error_reason: String,
    }

    impl FailingTool {
        #[allow(dead_code)]
        fn new(name: &str, error_reason: &str) -> Self {
            Self {
                tool_name: name.to_string(),
                error_reason: error_reason.to_string(),
            }
        }
    }

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "A tool that always fails for testing failure surfacing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.error_reason.clone()),
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("provider without vision support should fail");

        assert!(err.to_string().contains("provider_capability_error"));
        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 1,
            allow_remote_fetch: false,
            ..Default::default()
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
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("oversized payload must fail");

        assert!(
            err.to_string()
                .contains("multimodal image size limit exceeded")
        );
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("valid multimodal payload should pass");

        assert_eq!(result, "vision-ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// When `vision_provider` is not set and the default provider lacks vision
    /// support, the original `ProviderCapabilityError` should be returned.
    #[tokio::test]
    async fn run_tool_call_loop_no_vision_provider_config_preserves_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "check [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should fail without vision_provider config");

        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// When `vision_provider` is set but the provider factory cannot resolve
    /// the name, a descriptive error should be returned (not the generic
    /// capability error).
    #[tokio::test]
    async fn run_tool_call_loop_vision_provider_creation_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("some-model".to_string()),
            ..Default::default()
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
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should fail when vision provider cannot be created");

        assert!(
            err.to_string().contains("failed to create vision provider"),
            "expected creation failure error, got: {}",
            err
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// Messages without image markers should use the default provider even
    /// when `vision_provider` is configured.
    #[tokio::test]
    async fn run_tool_call_loop_no_images_uses_default_provider() {
        let provider = ScriptedProvider::from_text_responses(vec!["hello world"]);

        let mut history = vec![ChatMessage::user("just text, no images".to_string())];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("some-model".to_string()),
            ..Default::default()
        };

        // Even though vision_provider points to a nonexistent provider, this
        // should succeed because there are no image markers to trigger routing.
        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "scripted",
            "scripted-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("text-only messages should succeed with default provider");

        assert_eq!(result, "hello world");
    }

    /// When `vision_provider` is set but `vision_model` is not, the default
    /// model should be used as fallback for the vision provider.
    #[tokio::test]
    async fn run_tool_call_loop_vision_provider_without_model_falls_back() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "look [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        // vision_provider set but vision_model is None — the code should
        // fall back to the default model. Since the provider name is invalid,
        // we just verify the error path references the correct provider.
        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: None,
            ..Default::default()
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
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should fail due to nonexistent vision provider");

        // Verify the routing was attempted (not the generic capability error).
        assert!(
            err.to_string().contains("failed to create vision provider"),
            "expected creation failure, got: {}",
            err
        );
    }

    /// Empty `[IMAGE:]` markers (which are preserved as literal text by the
    /// parser) should not trigger vision provider routing.
    #[tokio::test]
    async fn run_tool_call_loop_empty_image_markers_use_default_provider() {
        let provider = ScriptedProvider::from_text_responses(vec!["handled"]);

        let mut history = vec![ChatMessage::user(
            "empty marker [IMAGE:] should be ignored".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            ..Default::default()
        };

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "scripted",
            "scripted-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("empty image markers should not trigger vision routing");

        assert_eq!(result, "handled");
    }

    /// Multiple image markers should still trigger vision routing when
    /// vision_provider is configured.
    #[tokio::test]
    async fn run_tool_call_loop_multiple_images_trigger_vision_routing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "two images [IMAGE:data:image/png;base64,aQ==] and [IMAGE:data:image/png;base64,bQ==]"
                .to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("llava:7b".to_string()),
            ..Default::default()
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
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should attempt vision provider creation for multiple images");

        assert!(
            err.to_string().contains("failed to create vision provider"),
            "expected creation failure for multiple images, got: {}",
            err
        );
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
        let approval_cfg = zeroclaw_config::schema::AutonomyConfig::default();
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
        let approval_cfg = zeroclaw_config::schema::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::AutonomyConfig::default()
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

        let approval_cfg = zeroclaw_config::schema::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::AutonomyConfig::default()
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("parallel execution should complete");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
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
    async fn run_tool_call_loop_injects_channel_delivery_defaults_for_cron_add() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"remind me later","schedule":{"kind":"every","every_ms":60000}}}
</tool_call>"#,
            "done",
        ]);

        let recorded_args = Arc::new(Mutex::new(Vec::new()));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(RecordingArgsTool::new(
            "cron_add",
            Arc::clone(&recorded_args),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("schedule a reminder"),
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
            "telegram",
            Some("chat-42"),
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("cron_add delivery defaults should be injected");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );

        let recorded = recorded_args
            .lock()
            .expect("recorded args lock should be valid");
        let delivery = recorded[0]["delivery"].clone();
        assert_eq!(
            delivery,
            serde_json::json!({
                "mode": "announce",
                "channel": "telegram",
                "to": "chat-42",
            })
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_preserves_explicit_cron_delivery_none() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"run silently","schedule":{"kind":"every","every_ms":60000},"delivery":{"mode":"none"}}}
</tool_call>"#,
            "done",
        ]);

        let recorded_args = Arc::new(Mutex::new(Vec::new()));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(RecordingArgsTool::new(
            "cron_add",
            Arc::clone(&recorded_args),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("schedule a quiet cron job"),
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
            "telegram",
            Some("chat-42"),
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("explicit delivery mode should be preserved");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );

        let recorded = recorded_args
            .lock()
            .expect("recorded args lock should be valid");
        assert_eq!(recorded[0]["delivery"], serde_json::json!({"mode": "none"}));
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("loop should finish after deduplicating repeated calls");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
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
    async fn run_tool_call_loop_allows_low_risk_shell_in_non_interactive_mode() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>"#,
            "done",
        ]);

        let tmp = TempDir::new().expect("temp dir");
        let security = Arc::new(crate::security::SecurityPolicy {
            autonomy: crate::security::AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            ..crate::security::SecurityPolicy::default()
        });
        let runtime: Arc<dyn crate::platform::RuntimeAdapter> =
            Arc::new(crate::platform::NativeRuntime::new());
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(
            crate::tools::shell::ShellTool::new(security, runtime),
        )];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;
        let approval_mgr = ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::AutonomyConfig::default(),
        );

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("non-interactive shell should succeed for low-risk command");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        assert!(tool_results.content.contains("hello"));
        assert!(!tool_results.content.contains("Denied by user."));
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_allows_repeated_calls() {
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
        let exempt = vec!["count_tool".to_string()];

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &exempt,
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("loop should finish with exempt tool executing twice");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            2,
            "exempt tool should execute both duplicate calls"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(
            !tool_results.content.contains("Skipped duplicate tool call"),
            "exempt tool calls should not be suppressed"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_only_affects_listed_tools() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"other_tool","arguments":{"value":"B"}}
</tool_call>
<tool_call>
{"name":"other_tool","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let count_invocations = Arc::new(AtomicUsize::new(0));
        let other_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(CountingTool::new(
                "count_tool",
                Arc::clone(&count_invocations),
            )),
            Box::new(CountingTool::new(
                "other_tool",
                Arc::clone(&other_invocations),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let exempt = vec!["count_tool".to_string()];

        let _result = run_tool_call_loop(
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &exempt,
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("loop should complete");

        assert_eq!(
            count_invocations.load(Ordering::SeqCst),
            2,
            "exempt tool should execute both calls"
        );
        assert_eq!(
            other_invocations.load(Ordering::SeqCst),
            1,
            "non-exempt tool should still be deduped"
        );
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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("native fallback id flow should complete");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
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
    async fn run_tool_call_loop_relays_native_tool_call_text_via_on_delta() {
        let provider = ScriptedProvider {
            responses: Arc::new(Mutex::new(VecDeque::from(vec![
                ChatResponse {
                    text: Some("Task started. Waiting 30 seconds before checking status.".into()),
                    tool_calls: vec![ToolCall {
                        id: "call_wait".into(),
                        name: "count_tool".into(),
                        arguments: r#"{"value":"A"}"#.into(),
                    }],
                    usage: None,
                    reasoning_content: None,
                },
                ChatResponse {
                    text: Some("Final answer".into()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                },
            ]))),
            capabilities: ProviderCapabilities {
                native_tool_calling: true,
                ..ProviderCapabilities::default()
            },
        };

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
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("native tool-call text should be relayed through on_delta");

        let mut deltas: Vec<DraftEvent> = Vec::new();
        while let Some(delta) = rx.recv().await {
            deltas.push(delta);
        }

        assert!(
            deltas
                .iter()
                .any(|delta| matches!(delta, StreamDelta::Text(t) if t == "Task started. Waiting 30 seconds before checking status.\n")),
            "native assistant text should be relayed to on_delta"
        );
        assert!(
            deltas
                .iter()
                .any(|delta| matches!(delta, StreamDelta::Status(t) if t.starts_with("\u{1f4ac} Got 1 tool call(s)"))),
            "tool-call progress line should still be relayed"
        );
        assert!(
            result.ends_with("Final answer"),
            "accumulated result should end with final answer, got: {result}"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_consumes_provider_stream_for_final_response() {
        let provider =
            StreamingScriptedProvider::from_text_responses(vec!["streamed final answer"]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("say hi"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("streaming provider should complete");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                StreamDelta::Status(_) => {}
                StreamDelta::Text(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert_eq!(result, "streamed final answer");
        assert_eq!(
            visible_deltas, "streamed final answer",
            "draft should receive upstream deltas once without post-hoc duplication"
        );
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_streaming_path_preserves_tool_loop_semantics() {
        let provider = StreamingScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
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
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            5,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("streaming tool loop should execute tool and finish");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                StreamDelta::Status(_) => {}
                StreamDelta::Text(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
        assert!(
            !visible_deltas.contains("<tool_call"),
            "draft text should not leak streamed tool payload markers"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_native_tool_events_without_chat_fallback() {
        let provider = StreamingNativeToolEventProvider::with_turns(vec![
            NativeStreamTurn::ToolCall(ToolCall {
                id: "call_native_1".to_string(),
                name: "count_tool".to_string(),
                arguments: r#"{"value":"A"}"#.to_string(),
            }),
            NativeStreamTurn::Text("done".to_string()),
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run native tools"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            5,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("native streaming events should preserve tool loop semantics");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                StreamDelta::Status(_) => {}
                StreamDelta::Text(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(provider.stream_tool_requests.load(Ordering::SeqCst), 2);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
    }

    #[tokio::test]
    async fn run_tool_call_loop_routed_streaming_uses_live_provider_deltas_once() {
        let default_provider = RouteAwareStreamingProvider::new("default answer");
        let default_stream_calls = Arc::clone(&default_provider.stream_calls);
        let default_chat_calls = Arc::clone(&default_provider.chat_calls);

        let routed_provider = RouteAwareStreamingProvider::new("routed streamed answer");
        let routed_stream_calls = Arc::clone(&routed_provider.stream_calls);
        let routed_chat_calls = Arc::clone(&routed_provider.chat_calls);
        let routed_last_model = Arc::clone(&routed_provider.last_model);

        let router = RouterProvider::new(
            vec![
                ("default".to_string(), Box::new(default_provider)),
                ("fast".to_string(), Box::new(routed_provider)),
            ],
            vec![(
                "fast".to_string(),
                Route {
                    provider_name: "fast".to_string(),
                    model: "routed-model".to_string(),
                },
            )],
            "default-model".to_string(),
        );

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("say hi"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

        let result = run_tool_call_loop(
            &router,
            &mut history,
            &tools_registry,
            &observer,
            "router",
            "hint:fast",
            0.0,
            true,
            None,
            "telegram",
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("routed streaming provider should complete");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                StreamDelta::Status(_) => {}
                StreamDelta::Text(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert_eq!(result, "routed streamed answer");
        assert_eq!(
            visible_deltas, "routed streamed answer",
            "routed draft should receive upstream deltas once without post-hoc duplication"
        );
        assert_eq!(default_stream_calls.load(Ordering::SeqCst), 0);
        assert_eq!(routed_stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(default_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(routed_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            routed_last_model
                .lock()
                .expect("routed_last_model lock should be valid")
                .as_str(),
            "routed-model"
        );
    }

    #[test]
    fn agent_turn_executes_activated_tool_from_wrapper() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should initialize");

        runtime.block_on(async {
            let provider = ScriptedProvider::from_text_responses(vec![
                r#"<tool_call>
{"name":"pixel__get_api_health","arguments":{"value":"ok"}}
</tool_call>"#,
                "done",
            ]);

            let invocations = Arc::new(AtomicUsize::new(0));
            let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
            let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
                "pixel__get_api_health",
                Arc::clone(&invocations),
            ));
            activated
                .lock()
                .unwrap()
                .activate("pixel__get_api_health".into(), activated_tool);

            let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
            let mut history = vec![
                ChatMessage::system("test-system"),
                ChatMessage::user("use the activated MCP tool"),
            ];
            let observer = NoopObserver;

            let result = agent_turn(
                &provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                0.0,
                true,
                "daemon",
                None,
                &zeroclaw_config::schema::MultimodalConfig::default(),
                4,
                None,
                &[],
                &[],
                Some(&activated),
                None,
            )
            .await
            .expect("wrapper path should execute activated tools");

            assert!(
                result.ends_with("done"),
                "result should end with 'done', got: {result}"
            );
            assert_eq!(invocations.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn resolve_display_text_hides_raw_payload_for_tool_only_turns() {
        let display = resolve_display_text(
            "<tool_call>{\"name\":\"memory_store\"}</tool_call>",
            "",
            true,
            false,
        );
        assert!(display.is_empty());
    }

    #[test]
    fn resolve_display_text_keeps_plain_text_for_tool_turns() {
        let display = resolve_display_text(
            "<tool_call>{\"name\":\"shell\"}</tool_call>",
            "Let me check that.",
            true,
            false,
        );
        assert_eq!(display, "Let me check that.");
    }

    #[test]
    fn resolve_display_text_uses_response_text_for_native_tool_turns() {
        let display = resolve_display_text("Task started.", "", true, true);
        assert_eq!(display, "Task started.");
    }

    #[test]
    fn resolve_display_text_uses_response_text_for_final_turns() {
        let display = resolve_display_text("Final answer", "", false, false);
        assert_eq!(display, "Final answer");
    }

    #[test]
    fn build_tool_instructions_includes_all_tools() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &zeroclaw_config::schema::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let instructions = build_tool_instructions(&tools, None);

        assert!(instructions.contains("## Tool Use Protocol"));
        assert!(instructions.contains("<tool_call>"));
        assert!(instructions.contains("shell"));
        assert!(instructions.contains("file_read"));
        assert!(instructions.contains("file_write"));
    }

    #[test]
    fn tools_to_openai_format_produces_valid_schema() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &zeroclaw_config::schema::AutonomyConfig::default(),
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

        let recalled = mem.recall("45", 5, None, None, None).await.unwrap();
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
    fn strip_think_tags_removes_single_block() {
        assert_eq!(strip_think_tags("<think>reasoning</think>Hello"), "Hello");
    }

    #[test]
    fn strip_think_tags_removes_multiple_blocks() {
        assert_eq!(strip_think_tags("<think>a</think>X<think>b</think>Y"), "XY");
    }

    #[test]
    fn strip_think_tags_handles_unclosed_block() {
        assert_eq!(strip_think_tags("visible<think>hidden"), "visible");
    }

    #[test]
    fn strip_think_tags_preserves_text_without_tags() {
        assert_eq!(strip_think_tags("plain text"), "plain text");
    }

    #[test]
    fn parse_tool_calls_strips_think_before_tool_call() {
        // Qwen regression: <think> tags before <tool_call> tags should be
        // stripped, allowing the tool call to be parsed correctly.
        let response = "<think>I need to list files to understand the project</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n</tool_call>";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should parse tool call after stripping think tags"
        );
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls"
        );
        assert!(text.is_empty(), "think content should not appear as text");
    }

    #[test]
    fn parse_tool_calls_strips_think_only_returns_empty() {
        // When response is only <think> tags with no tool calls, should
        // return empty text and no calls.
        let response = "<think>Just thinking, no action needed</think>";
        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_qwen_think_with_multiple_tool_calls() {
        let response = "<think>I need to check two things</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n</tool_call>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n</tool_call>";
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert_eq!(
            calls[1].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn strip_tool_result_blocks_preserves_clean_text() {
        let input = "Hello, this is a normal response.";
        assert_eq!(strip_tool_result_blocks(input), input);
    }

    #[test]
    fn strip_tool_result_blocks_returns_empty_for_only_tags() {
        let input = "<tool_result name=\"memory_recall\" status=\"ok\">\n{}\n</tool_result>";
        assert_eq!(strip_tool_result_blocks(input), "");
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

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - JSON Extraction
    // ═══════════════════════════════════════════════════════════════════════

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
    fn scrub_credentials_multibyte_chars_no_panic() {
        // Regression test for #3024: byte index 4 is not a char boundary
        // when the captured value contains multi-byte UTF-8 characters.
        // The regex only matches quoted values for non-ASCII content, since
        // capture group 4 is restricted to [a-zA-Z0-9_\-\.].
        let input = "password=\"\u{4f60}\u{7684}WiFi\u{5bc6}\u{7801}ab\"";
        let result = scrub_credentials(input);
        assert!(
            result.contains("[REDACTED]"),
            "multi-byte quoted value should be redacted without panic, got: {result}"
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
        let mut history: Vec<ChatMessage> = vec![];
        trim_history(&mut history, 10);
        assert!(history.is_empty());
    }

    #[test]
    fn trim_history_system_only() {
        let mut history = vec![ChatMessage::system("system prompt")];
        trim_history(&mut history, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "system");
    }

    #[test]
    fn trim_history_exactly_at_limit() {
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("msg 1"),
            ChatMessage::assistant("reply 1"),
        ];
        trim_history(&mut history, 2); // 2 non-system messages = exactly at limit
        assert_eq!(history.len(), 3, "should not trim when exactly at limit");
    }

    #[test]
    fn trim_history_removes_oldest_non_system() {
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("old msg"),
            ChatMessage::assistant("old reply"),
            ChatMessage::user("new msg"),
            ChatMessage::assistant("new reply"),
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
        use crate::agent::system_prompt::build_system_prompt_with_mode;

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
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            crate::security::AutonomyLevel::default(),
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

    // ── glob_match tests ──────────────────────────────────────────────────────

    #[test]
    fn glob_match_exact_no_wildcard() {
        assert!(glob_match("mcp_browser_navigate", "mcp_browser_navigate"));
        assert!(!glob_match("mcp_browser_navigate", "mcp_browser_click"));
    }

    #[test]
    fn glob_match_prefix_wildcard() {
        // Suffix pattern: mcp_browser_*
        assert!(glob_match("mcp_browser_*", "mcp_browser_navigate"));
        assert!(glob_match("mcp_browser_*", "mcp_browser_click"));
        assert!(!glob_match("mcp_browser_*", "mcp_filesystem_read"));

        // Prefix pattern: *_read
        assert!(glob_match("*_read", "mcp_filesystem_read"));
        assert!(!glob_match("*_read", "mcp_filesystem_write"));

        // Infix: mcp_*_navigate
        assert!(glob_match("mcp_*_navigate", "mcp_browser_navigate"));
        assert!(!glob_match("mcp_*_navigate", "mcp_browser_click"));
    }

    #[test]
    fn glob_match_star_matches_everything() {
        assert!(glob_match("*", "anything_at_all"));
        assert!(glob_match("*", ""));
    }

    // ── filter_tool_specs_for_turn tests ──────────────────────────────────────

    fn make_spec(name: &str) -> crate::tools::ToolSpec {
        crate::tools::ToolSpec {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }
    }

    #[test]
    fn filter_tool_specs_no_groups_returns_all() {
        let specs = vec![
            make_spec("shell_exec"),
            make_spec("mcp_browser_navigate"),
            make_spec("mcp_filesystem_read"),
        ];
        let result = filter_tool_specs_for_turn(specs, &[], "hello");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn filter_tool_specs_always_group_includes_matching_mcp_tool() {
        use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![
            make_spec("shell_exec"),
            make_spec("mcp_browser_navigate"),
            make_spec("mcp_filesystem_read"),
        ];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Always,
            tools: vec!["mcp_filesystem_*".into()],
            keywords: vec![],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "anything");
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        // Built-in passes through, matched MCP passes, unmatched MCP excluded.
        assert!(names.contains(&"shell_exec"));
        assert!(names.contains(&"mcp_filesystem_read"));
        assert!(!names.contains(&"mcp_browser_navigate"));
    }

    #[test]
    fn filter_tool_specs_dynamic_group_included_on_keyword_match() {
        use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![make_spec("shell_exec"), make_spec("mcp_browser_navigate")];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["browse".into(), "website".into()],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "please browse this page");
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"shell_exec"));
        assert!(names.contains(&"mcp_browser_navigate"));
    }

    #[test]
    fn filter_tool_specs_dynamic_group_excluded_on_no_keyword_match() {
        use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![make_spec("shell_exec"), make_spec("mcp_browser_navigate")];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["browse".into(), "website".into()],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "read the file /etc/hosts");
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"shell_exec"));
        assert!(!names.contains(&"mcp_browser_navigate"));
    }

    #[test]
    fn filter_tool_specs_dynamic_keyword_match_is_case_insensitive() {
        use zeroclaw_config::schema::{ToolFilterGroup, ToolFilterGroupMode};

        let specs = vec![make_spec("mcp_browser_navigate")];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["Browse".into()],
            filter_builtins: false,
        }];
        let result = filter_tool_specs_for_turn(specs, &groups, "BROWSE the site");
        assert_eq!(result.len(), 1);
    }

    // ── Token-based compaction tests ──────────────────────────

    #[test]
    fn estimate_history_tokens_empty() {
        assert_eq!(super::estimate_history_tokens(&[]), 0);
    }

    #[test]
    fn estimate_history_tokens_single_message() {
        let history = vec![ChatMessage::user("hello world")]; // 11 chars
        let tokens = super::estimate_history_tokens(&history);
        // 11.div_ceil(4) + 4 = 3 + 4 = 7
        assert_eq!(tokens, 7);
    }

    #[test]
    fn estimate_history_tokens_multiple_messages() {
        let history = vec![
            ChatMessage::system("You are helpful."), // 16 chars → 4 + 4 = 8
            ChatMessage::user("What is Rust?"),      // 13 chars → 4 + 4 = 8
            ChatMessage::assistant("A language."),   // 11 chars → 3 + 4 = 7
        ];
        let tokens = super::estimate_history_tokens(&history);
        assert_eq!(tokens, 23);
    }

    #[tokio::test]
    async fn run_tool_call_loop_surfaces_tool_failure_reason_in_on_delta() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"failing_shell","arguments":{"command":"rm -rf /"}}
</tool_call>"#,
            "I could not execute that command.",
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(FailingTool::new(
            "failing_shell",
            "Command not allowed by security policy: rm -rf /",
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("delete everything"),
        ];
        let observer = NoopObserver;

        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

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
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("tool loop should complete");

        // Collect all messages sent to the on_delta channel.
        let mut deltas = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            deltas.push(msg);
        }

        let all_deltas: String = deltas
            .iter()
            .map(|d| match d {
                StreamDelta::Status(t) | StreamDelta::Text(t) => t.as_str(),
            })
            .collect();

        // The failure reason should appear in the progress messages.
        assert!(
            all_deltas.contains("Command not allowed by security policy"),
            "on_delta messages should include the tool failure reason, got: {all_deltas}"
        );

        // Should also contain the cross mark (❌) icon to indicate failure.
        assert!(
            all_deltas.contains('\u{274c}'),
            "on_delta messages should include ❌ for failed tool calls, got: {all_deltas}"
        );

        assert!(
            result.ends_with("I could not execute that command."),
            "result should end with error message, got: {result}"
        );
    }

    // ── filter_by_allowed_tools tests ─────────────────────────────────────

    #[test]
    fn filter_by_allowed_tools_none_passes_all() {
        let specs = vec![
            make_spec("shell"),
            make_spec("memory_store"),
            make_spec("file_read"),
        ];
        let result = filter_by_allowed_tools(specs, None);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn filter_by_allowed_tools_some_restricts_to_listed() {
        let specs = vec![
            make_spec("shell"),
            make_spec("memory_store"),
            make_spec("file_read"),
        ];
        let allowed = vec!["shell".to_string(), "memory_store".to_string()];
        let result = filter_by_allowed_tools(specs, Some(&allowed));
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"memory_store"));
        assert!(!names.contains(&"file_read"));
    }

    #[test]
    fn filter_by_allowed_tools_unknown_names_silently_ignored() {
        let specs = vec![make_spec("shell"), make_spec("file_read")];
        let allowed = vec![
            "shell".to_string(),
            "nonexistent_tool".to_string(),
            "another_missing".to_string(),
        ];
        let result = filter_by_allowed_tools(specs, Some(&allowed));
        let names: Vec<&str> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"shell"));
    }

    #[test]
    fn filter_by_allowed_tools_empty_list_excludes_all() {
        let specs = vec![make_spec("shell"), make_spec("file_read")];
        let allowed: Vec<String> = vec![];
        let result = filter_by_allowed_tools(specs, Some(&allowed));
        assert!(result.is_empty());
    }

    // ── Cost tracking tests ──

    #[tokio::test]
    async fn cost_tracking_records_usage_when_scoped() {
        use super::{
            TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext, run_tool_call_loop,
        };
        use crate::cost::CostTracker;
        use crate::observability::noop::NoopObserver;
        use std::collections::HashMap;
        use zeroclaw_config::schema::ModelPricing;

        let provider = ScriptedProvider {
            responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(1_000),
                    output_tokens: Some(200),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            }]))),
            capabilities: ProviderCapabilities::default(),
        };
        let observer = NoopObserver;
        let workspace = tempfile::TempDir::new().unwrap();
        let mut cost_config = zeroclaw_config::schema::CostConfig {
            enabled: true,
            ..zeroclaw_config::schema::CostConfig::default()
        };
        cost_config.prices = HashMap::from([(
            "mock-model".to_string(),
            ModelPricing {
                input: 3.0,
                output: 15.0,
            },
        )]);
        let tracker = Arc::new(CostTracker::new(cost_config.clone(), workspace.path()).unwrap());
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(cost_config.prices.clone()),
        );
        let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

        let result = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                run_tool_call_loop(
                    &provider,
                    &mut history,
                    &[],
                    &observer,
                    "mock-provider",
                    "mock-model",
                    0.0,
                    true,
                    None,
                    "test",
                    None,
                    &zeroclaw_config::schema::MultimodalConfig::default(),
                    2,
                    None,
                    None,
                    None,
                    &[],
                    &[],
                    None,
                    None,
                    &zeroclaw_config::schema::PacingConfig::default(),
                    0,
                    0,
                    None,
                ),
            )
            .await
            .expect("tool loop should succeed");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.total_tokens, 1_200);
        assert!(summary.session_cost_usd > 0.0);
    }

    #[tokio::test]
    async fn cost_tracking_enforces_budget() {
        use super::{
            TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext, run_tool_call_loop,
        };
        use crate::cost::CostTracker;
        use crate::observability::noop::NoopObserver;
        use std::collections::HashMap;
        use zeroclaw_config::schema::ModelPricing;

        let provider = ScriptedProvider::from_text_responses(vec!["should not reach this"]);
        let observer = NoopObserver;
        let workspace = tempfile::TempDir::new().unwrap();
        let cost_config = zeroclaw_config::schema::CostConfig {
            enabled: true,
            daily_limit_usd: 0.001, // very low limit
            ..zeroclaw_config::schema::CostConfig::default()
        };
        let tracker = Arc::new(CostTracker::new(cost_config.clone(), workspace.path()).unwrap());
        // Record a usage that already exceeds the limit
        tracker
            .record_usage(crate::cost::types::TokenUsage::new(
                "mock-model",
                100_000,
                50_000,
                1.0,
                1.0,
            ))
            .unwrap();

        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(HashMap::from([(
                "mock-model".to_string(),
                ModelPricing {
                    input: 1.0,
                    output: 1.0,
                },
            )])),
        );
        let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

        let err = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                run_tool_call_loop(
                    &provider,
                    &mut history,
                    &[],
                    &observer,
                    "mock-provider",
                    "mock-model",
                    0.0,
                    true,
                    None,
                    "test",
                    None,
                    &zeroclaw_config::schema::MultimodalConfig::default(),
                    2,
                    None,
                    None,
                    None,
                    &[],
                    &[],
                    None,
                    None,
                    &zeroclaw_config::schema::PacingConfig::default(),
                    0,
                    0,
                    None,
                ),
            )
            .await
            .expect_err("should fail with budget exceeded");

        assert!(
            err.to_string().contains("Budget exceeded"),
            "error should mention budget: {err}"
        );
    }

    #[tokio::test]
    async fn cost_tracking_is_noop_without_scope() {
        use super::run_tool_call_loop;
        use crate::observability::noop::NoopObserver;

        // No TOOL_LOOP_COST_TRACKING_CONTEXT scoped — should run fine
        let provider = ScriptedProvider {
            responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(500),
                    output_tokens: Some(100),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            }]))),
            capabilities: ProviderCapabilities::default(),
        };
        let observer = NoopObserver;
        let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &[],
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "test",
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            2,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("should succeed without cost scope");

        assert_eq!(result, "ok");
    }
}
