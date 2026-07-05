use crate::approval::ApprovalManager;

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

/// Public helper for other crates (e.g. channels orchestrator) to load
/// peripheral tools through the registered factory. Returns empty vec
/// when nothing is registered (hardware feature off or not yet wired).
pub async fn load_peripheral_tools(
    config: zeroclaw_config::schema::PeripheralsConfig,
) -> Vec<Box<dyn Tool>> {
    if let Some(f) = PERIPHERAL_TOOLS_FN.get() {
        f(config).await.unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Channel map factory type — builds `channel_key → Arc<dyn Channel>` map.
/// Injected by the binary so `zeroclaw-runtime` doesn't depend on
/// `zeroclaw-channels`.
type ChannelMapFn = Box<
    dyn Fn()
            -> std::collections::HashMap<String, std::sync::Arc<dyn zeroclaw_api::channel::Channel>>
        + Send
        + Sync,
>;

/// Channel map factory, injected by the binary.
static CHANNEL_MAP_FN: std::sync::OnceLock<ChannelMapFn> = std::sync::OnceLock::new();

/// Register the channel map factory. Called once at startup by the binary.
pub fn register_channel_map_fn(f: ChannelMapFn) {
    let _ = CHANNEL_MAP_FN.set(f);
}

/// Populate all channel-driven tool handles from the registered factory.
/// Returns the number of channels seeded.
///
/// Parameter order matches the return tuple of `all_tools_with_runtime`:
/// Seed all channel-driven tool handles from the registered channel map factory.
/// Returns the number of channels seeded. Parameters match the return order of
/// `all_tools_with_runtime`:
///   ask_user_handle = `Option<PerToolChannelHandle>`
///   channel_room_handle = `Option<PerToolChannelHandle>`
///   reaction_handle = `PerToolChannelHandle` (NOT Option)
///   poll_handle = `Option<PerToolChannelHandle>`
///   escalate_handle = `Option<PerToolChannelHandle>`
pub(crate) fn seed_channel_handles(
    ask_user_handle: &Option<tools::PerToolChannelHandle>,
    channel_room_handle: &Option<tools::PerToolChannelHandle>,
    reaction_handle: &tools::PerToolChannelHandle,
    poll_handle: &Option<tools::PerToolChannelHandle>,
    escalate_handle: &Option<tools::PerToolChannelHandle>,
) -> usize {
    let Some(factory) = CHANNEL_MAP_FN.get() else {
        return 0;
    };
    let map = factory();
    if map.is_empty() {
        return 0;
    }

    let handles = [
        ask_user_handle.as_ref(),
        channel_room_handle.as_ref(),
        Some(reaction_handle),
        poll_handle.as_ref(),
        escalate_handle.as_ref(),
    ];

    let mut count = 0;
    for (name, ch) in &map {
        for handle in handles.iter().flatten() {
            handle
                .write()
                .insert(name.clone(), std::sync::Arc::clone(ch));
        }
        count += 1;
    }
    count
}

/// Snapshot the live `channel_key → Arc<dyn Channel>` map from the injected
/// channel-map factory as a [`tools::PerToolChannelHandle`], for channel-less
/// turn paths (`process_message`) that must reach a live approver channel to
/// honor a risk profile's cross-channel `approval_route`. Returns `None` when no
/// factory is registered (e.g. CLI/tests) or no channels are live — callers then
/// keep today's channel-less behavior (the gate auto-denies gated tools).
pub(crate) fn live_channel_registry() -> Option<tools::PerToolChannelHandle> {
    let factory = CHANNEL_MAP_FN.get()?;
    let map = factory();
    if map.is_empty() {
        return None;
    }
    Some(Arc::new(parking_lot::RwLock::new(map)))
}
use crate::observability::{self, Observer, ObserverEvent};
use crate::platform;
use crate::security::{AutonomyLevel, SecurityPolicy};
use crate::tools::scoped;
use crate::tools::{self, Tool};
use crate::util::truncate_with_ellipsis;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashSet;
use std::fmt::Write;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::channel::Channel;
use zeroclaw_api::ingress::IngressContext;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{
    self, MEMORY_CONTEXT_CLOSE, MEMORY_CONTEXT_OPEN, Memory, MemoryCategory, decay,
};
#[cfg(test)]
use zeroclaw_providers::ChatRequest;
use zeroclaw_providers::{self, ChatMessage, ModelProvider, ToolCall};

// Cost tracking moved to `super::cost`.
pub use super::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext, TurnUsage,
    check_tool_loop_budget, record_tool_loop_cost_usage,
};

// History management moved to `super::history`.
pub use super::history::{
    append_or_merge_system_message, canonicalize_tool_result_media_markers,
    estimate_history_tokens, load_interactive_session_history, normalize_system_messages,
    save_interactive_session_history, trim_history, truncate_tool_result,
};

/// Minimum user-message length (in chars) for auto-save to memory.
/// Matches the channel-side constant in `channels/mod.rs`.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

/// Maximum bytes of an interactive stdin line accepted by the
/// ZeroClaw REPL. Caps the per-line allocation so a pipe of
/// `head -c 10G /dev/zero | zeroclaw chat` cannot blow up RSS; the
/// line is truncated to this size and the user is warned. Matches
/// the size class of the gateway HTTP body cap (64 KiB) and the
/// largest per-message cap in the channel orchestrator (4 KiB notify
/// detail × 128-deep queue = 512 KiB).
pub(crate) const MAX_INTERACTIVE_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB

/// Result of [`read_capped_line`].
#[derive(Debug)]
pub(crate) enum CappedLine {
    /// A full line under the cap, with the trailing `\n` stripped.
    Line(String),
    /// The physical line exceeded `cap`. The remainder has been
    /// drained to the next `\n` or EOF, so the caller must treat this
    /// as a discarded line and must not feed it into the model path.
    Truncated,
    /// EOF with no bytes read.
    Eof,
}

/// Read a single line from `reader` bounded at `cap` bytes. Returns
/// the line with the trailing `\n` stripped (lossily decoded, since
/// PTY transport can split multi-byte characters at frame boundaries)
/// or [`CappedLine::Truncated`] when the cap was hit. When truncated,
/// the rest of the physical line is drained using a fixed-size scratch
/// buffer so the next call starts at the next line and no unbounded
/// allocation occurs (Audacity88 review #8463).
pub(crate) fn read_capped_line<R: std::io::BufRead>(
    reader: R,
    cap: usize,
) -> std::io::Result<CappedLine> {
    let mut raw = Vec::new();
    // +1 headroom so the cap detection is unambiguous: a buffer that
    // reaches exactly `cap` bytes without a `\n` was truncated; a
    // buffer shorter than `cap` has the full line.
    let mut limited = reader.take((cap + 1) as u64);
    std::io::BufRead::read_until(&mut limited, b'\n', &mut raw)?;
    let truncated = raw.len() > cap;
    if truncated {
        // Drain the rest of the physical line without accumulating it
        // in memory; `read_until` into a `Vec` would re-introduce the
        // original OOM vector.
        let mut inner = limited.into_inner();
        discard_until_newline(&mut inner)?;
        return Ok(CappedLine::Truncated);
    } else if raw.last() == Some(&b'\n') {
        // Strip the trailing `\n` that `read_until` leaves behind. The
        // lossy decode runs after the strip so the result has no
        // trailing newline regardless of the cap path.
        raw.pop();
    }
    if raw.is_empty() {
        return Ok(CappedLine::Eof);
    }
    Ok(CappedLine::Line(String::from_utf8_lossy(&raw).into_owned()))
}

/// Discard bytes from `reader` until the next `\n` or EOF, using only
/// `BufRead::fill_buf` / `consume`. This avoids the unbounded allocation
/// that `read_until(..., &mut Vec::new())` would incur on an oversized
/// physical line, and it stops exactly at the newline so the next line
/// is not consumed.
fn discard_until_newline<R: std::io::BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let buf = reader.fill_buf()?;
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            return Ok(());
        }
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }
        reader.consume(len);
    }
}

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

/// Drop tools from `tools` that fail either gate.
///
/// 1. The parent agent's `SecurityPolicy.allowed_tools` allowlist plus
///    `SecurityPolicy.excluded_tools` denylist, evaluated via
///    `SecurityPolicy::is_tool_allowed`.
/// 2. The caller-supplied `caller_allowed` filter (the existing
///    `agent::run`-level `allowed_tools` parameter).
///
/// A tool survives only when BOTH gates admit its name. `None` on
/// either gate is unrestricted for that gate alone. Built-in tools,
/// MCP tools, and skill tools all flow through the same filter; the
/// helper does not know or care about category.
pub fn apply_policy_tool_filter(
    tools: &mut Vec<Box<dyn Tool>>,
    policy: Option<&zeroclaw_config::policy::SecurityPolicy>,
    caller_allowed: Option<&[String]>,
) {
    tools.retain(|t| {
        let name = t.name();
        let policy_ok = policy.is_none_or(|p| p.is_tool_allowed(name));
        let caller_ok = caller_allowed.is_none_or(|list| list.iter().any(|n| n == name));
        policy_ok && caller_ok
    });
}

/// Build the MCP tool-access policy for an agent from its `SecurityPolicy`
/// (`allowed_tools` + `excluded_tools`) and an optional caller-supplied
/// allowlist. Shared by the runtime agent loop and the channels orchestrator
/// so every MCP registration site gates through identical logic.
pub fn mcp_tool_access_policy(
    security: &zeroclaw_config::policy::SecurityPolicy,
    caller_allowed: Option<&[String]>,
) -> Option<zeroclaw_tools::tool_search::ToolAccessPolicy> {
    zeroclaw_tools::tool_search::ToolAccessPolicy::from_security(
        security.allowed_tools.as_deref(),
        security.excluded_tools.as_deref(),
        caller_allowed,
    )
}

/// Whether an MCP tool name is admitted by `policy` (a `None` policy admits
/// everything). The risk-profile denylist always wins; the allowlist
/// auto-admits `<server>__<tool>` names so a restrictive allowlist does not
/// silently drop a configured server's tools.
pub fn eager_mcp_tool_allowed(
    name: &str,
    policy: Option<&zeroclaw_tools::tool_search::ToolAccessPolicy>,
) -> bool {
    policy.is_none_or(|policy| policy.is_tool_allowed(name))
}

pub(crate) fn mcp_allowed_tool_count<'a>(
    names: impl IntoIterator<Item = &'a str>,
    policy: Option<&zeroclaw_tools::tool_search::ToolAccessPolicy>,
) -> usize {
    names
        .into_iter()
        .filter(|name| eager_mcp_tool_allowed(name, policy))
        .count()
}

/// Append a pre-rendered pinned-MCP-resources section onto the system-prompt
/// MCP accumulator (`deferred_section`).
///
/// This MUST be called *after* the `deferred_loading` branch, which reassigns
/// `deferred_section` with `=` (via `build_deferred_tools_section_filtered`)
/// and would otherwise clobber any earlier-pushed pinned content. Centralizing
/// the append keeps both `run()` and `process_message()` consistent and pins
/// the ordering invariant in one testable place. No-op for an empty section.
pub(crate) fn append_pinned_mcp_section(deferred_section: &mut String, pinned_section: &str) {
    if pinned_section.is_empty() {
        return;
    }
    deferred_section.push_str("\n\n");
    deferred_section.push_str(pinned_section);
}

/// Register an eager MCP tool wrapper into `tools` (and the delegate handle,
/// when present) only if `policy` admits it. Returns `true` when the tool was
/// registered, `false` when the policy dropped it.
pub fn register_eager_mcp_tool_if_allowed(
    wrapper: std::sync::Arc<dyn Tool>,
    tools: &mut Vec<Box<dyn Tool>>,
    delegate_handle: Option<&tools::DelegateParentToolsHandle>,
    policy: Option<&zeroclaw_tools::tool_search::ToolAccessPolicy>,
) -> bool {
    if !eager_mcp_tool_allowed(wrapper.name(), policy) {
        return false;
    }
    if let Some(handle) = delegate_handle {
        handle.write().push(std::sync::Arc::clone(&wrapper));
    }
    tools.push(Box::new(tools::ArcToolRef(wrapper)));
    true
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
pub use zeroclaw_api::TOOL_LOOP_SESSION_KEY;
pub use zeroclaw_api::TOOL_LOOP_THREAD_ID;

// Re-export tool call parsing from the standalone parser crate.
pub use zeroclaw_tool_call_parser::{
    ParsedToolCall, ToolProtocolEnvelopeKind, build_native_assistant_history_from_parsed_calls,
    canonicalize_json_for_tool_signature, classify_tool_protocol_envelope,
    contains_tool_protocol_tag_call, detect_tool_call_parse_issue,
    looks_like_malformed_tool_protocol_envelope,
    looks_like_malformed_tool_protocol_envelope_for_known_tools, looks_like_tool_protocol_envelope,
    looks_like_tool_protocol_example, parse_tool_calls, strip_think_tags, strip_tool_result_blocks,
    tool_protocol_envelope_mentions_known_tool,
};

/// Run a future with the thread ID set in task-local storage.
/// Rate-limiting reads this to assign per-sender buckets.
pub async fn scope_thread_id<F>(thread_id: Option<String>, future: F) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_THREAD_ID.scope(thread_id, future).await
}

/// Run a future with the session key set in task-local storage.
/// The scope wraps the entire agent turn, so all tools invoked during
/// the turn (including nested calls) see the same session key.
/// SessionsCurrentTool reads this to identify the active session.
pub async fn scope_session_key<F>(session_key: Option<String>, future: F) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_SESSION_KEY.scope(session_key, future).await
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

pub fn native_tool_specs_present_for_turn(
    model_provider: &dyn ModelProvider,
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
    activated_tools: Option<&Arc<Mutex<crate::tools::ActivatedToolSet>>>,
) -> Result<bool> {
    if !model_provider.supports_native_tools() {
        return Ok(false);
    }

    let iteration_tool_specs = super::turn::build_iteration_tool_specs(
        model_provider,
        tools_registry,
        excluded_tools,
        activated_tools,
    )?;
    Ok(!iteration_tool_specs.tool_specs.is_empty())
}

/// Elide inlined base64 image data URIs from message content before export.
/// Keeps `[IMAGE:<path>]` / `[IMAGE:https://…]` markers; replaces only
/// `[IMAGE:data:…]` payloads (hundreds of KB of base64) with a short placeholder.
/// (base64 and the data-URI body never contain `]`, so bounding on `]` is safe.)
///
/// Only `[IMAGE:data:…]` markers currently carry inline data URIs; other media
/// markers (`[PHOTO:]`/`[VIDEO:]`/`[DOCUMENT:]`/`[FILE:]`/`[VOICE:]`/`[AUDIO:]`)
/// carry paths/URLs, not inline bytes (verified against
/// `prepare_messages_for_provider`). Extend this regex if that ever changes.
static IMAGE_DATA_URI_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[IMAGE:data:[^\]]*\]").unwrap());

fn elide_image_data(content: &str) -> String {
    IMAGE_DATA_URI_REGEX
        .replace_all(content, "[IMAGE:<image data elided>]")
        .into_owned()
}

/// Best-effort sanitize message content for trace export: elide image bytes, then
/// run BOTH credential scrubbers. `scrub_secret_patterns` (prefix-based: sk-/ghp_/
/// xoxb-/…) and `scrub_credentials` (key=value / bearer=) are disjoint; neither
/// alone covers free-form content. Residual secrets/PII may remain — this is
/// disclosed in the PR/docs, not eliminated.
pub(crate) fn scrub_for_export(content: &str) -> String {
    scrub_credentials(&zeroclaw_providers::scrub_secret_patterns(
        &elide_image_data(content),
    ))
}

/// Capture and sanitize the prompt/completion content for one `llm.call` so the
/// OTel exporter can emit the GenAI message-content attributes.
///
/// Returns `None` unless the `observability-otel` feature is active, so non-OTel
/// builds pay no cloning/scrubbing cost. The system message is split into
/// `system_instructions`; the rest become `input`. Every string passes through
/// [`scrub_for_export`] (image elision + dual credential scrub).
///
/// This function does NOT consult any OTel content policy — it only constructs
/// a credential-scrubbed snapshot when the `observability-otel` feature is
/// enabled. Whether that snapshot is actually exported (and at what privacy
/// level: `off` / `redacted` / `full`) is decided by the owning
/// `OtelObserver`'s instance content config at the OTel export boundary, not
/// here. Keeping the policy out of the capture path avoids process-global
/// mutable state and the cross-observer drift it caused.
pub(crate) fn capture_llm_messages(
    messages: &[ChatMessage],
    output_text: Option<&str>,
    output_tool_calls: &[ToolCall],
) -> Option<zeroclaw_api::observability_traits::LlmMessageSnapshot> {
    if !cfg!(feature = "observability-otel") {
        return None;
    }

    use zeroclaw_api::observability_traits::{
        LlmMessageSnapshot, MessageSnapshot, ToolCallSnapshot,
    };

    let system_instructions = messages
        .iter()
        .find(|m| m.role == "system")
        .map(|m| scrub_for_export(&m.content));

    let input = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| MessageSnapshot {
            role: m.role.clone(),
            content: scrub_for_export(&m.content),
        })
        .collect();

    let output_text = output_text.filter(|t| !t.is_empty()).map(scrub_for_export);

    let output_tool_calls = output_tool_calls
        .iter()
        .map(|tc| ToolCallSnapshot {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments_json: scrub_for_export(&tc.arguments),
        })
        .collect();

    Some(LlmMessageSnapshot {
        input,
        output_text,
        output_tool_calls,
        system_instructions,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_system_prompt_for_turn(
    agent_workspace: &std::path::Path,
    model_name: &str,
    tool_descs: &[(&str, &str)],
    deferred_section: &str,
    skills: &[crate::skills::Skill],
    identity_config: Option<&zeroclaw_config::schema::IdentityConfig>,
    bootstrap_max_chars: Option<usize>,
    risk_profile: &zeroclaw_config::schema::RiskProfileConfig,
    model_provider: &dyn ModelProvider,
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
    activated_tools: Option<&Arc<Mutex<crate::tools::ActivatedToolSet>>>,
    strict_tool_parsing: bool,
    skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
    compact_context: bool,
    max_system_prompt_chars: usize,
    inject_memory: bool,
    show_tool_calls: bool,
    thinking_prefix: Option<&str>,
) -> Result<String> {
    let native_tools = model_provider.supports_native_tools();
    let native_tool_specs_present = native_tool_specs_present_for_turn(
        model_provider,
        tools_registry,
        excluded_tools,
        activated_tools,
    )?;
    let excluded_tool_names: HashSet<&str> = excluded_tools.iter().map(String::as_str).collect();
    let effective_tool_names: HashSet<&str> = tools_registry
        .iter()
        .map(|tool| tool.name())
        .filter(|name| !excluded_tool_names.contains(*name))
        .collect();
    let mut turn_tool_descs = tool_descs.to_vec();
    turn_tool_descs.retain(|(name, _)| effective_tool_names.contains(name));
    let mut turn_deferred_section = deferred_section.to_string();
    let expose_text_tool_protocol = apply_text_tool_prompt_policy(
        native_tools,
        strict_tool_parsing,
        &mut turn_tool_descs,
        &mut turn_deferred_section,
    );
    let mut system_prompt = crate::agent::system_prompt::build_system_prompt_with_mode_and_autonomy(
        agent_workspace,
        model_name,
        &turn_tool_descs,
        skills,
        identity_config,
        bootstrap_max_chars,
        Some(risk_profile),
        native_tool_specs_present,
        skills_prompt_mode,
        compact_context,
        max_system_prompt_chars,
        inject_memory,
        show_tool_calls,
    );

    if expose_text_tool_protocol {
        system_prompt.push_str(&build_tool_instructions_for_names(
            tools_registry,
            &effective_tool_names,
        ));
    }
    if !turn_deferred_section.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&turn_deferred_section);
    }
    if let Some(prefix) = thinking_prefix {
        system_prompt = format!("{prefix}\n\n{system_prompt}");
    }

    Ok(system_prompt)
}

/// Build a `query_summary` field for memory observability events from a raw
/// user query.
///
/// Applies [`scrub_credentials`] first, then truncates to ≤200 content
/// chars via [`truncate_with_ellipsis`] (which appends a 3-char `...`
/// ellipsis when truncation occurred, so summaries are ≤203 chars total).
/// The order matters: `scrub_credentials` may insert placeholder
/// substrings, so truncating first risks chopping a half-token.
///
/// Returns `None` for empty input so observers can distinguish "no query
/// recorded" from "empty query string". Always call this helper at memory
/// emit sites — never inline the scrub-then-truncate pattern, since that
/// invites drift where one site accidentally skips the scrubber.
pub fn make_query_summary(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    Some(truncate_with_ellipsis(&scrub_credentials(raw), 200))
}

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
///
/// `exclude_conversation` skips `MemoryCategory::Conversation` entries
/// regardless of their key shape. Set to `true` for autonomous/scheduled
/// runs (cron, daemon heartbeat) so chat memory cannot leak into prompts
/// the user did not initiate. / #5456.
async fn build_context(
    mem: &dyn Memory,
    observer: &dyn Observer,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
    exclude_conversation: bool,
) -> String {
    let mut context = String::new();
    let backend = mem.name().to_string();

    // Pull relevant memories for this message. The original code used
    // `if let Ok(...)` to silently swallow recall errors; we keep that
    // behavior but refactor to an explicit match so the failure path can
    // still emit a `MemoryRecall` observer event.
    let start = std::time::Instant::now();
    let recall_result = mem.recall(user_msg, 5, session_id, None, None).await;
    let duration = start.elapsed();
    let query_summary = make_query_summary(user_msg);

    match recall_result {
        Ok(mut entries) => {
            observer.record_event(&ObserverEvent::MemoryRecall {
                query_summary,
                duration,
                num_entries: entries.len(),
                backend,
                success: true,
            });

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
                let mut included = false;
                for entry in &relevant {
                    // Scheduled (cron / heartbeat) runs must not see chat-origin
                    // memories. The autosave-key checks below catch the agent's
                    // own autosaves but miss Conversation entries written by
                    // channel handlers (Discord, gateway, WhatsApp, …) under
                    // their own keys. / #5456.
                    if exclude_conversation
                        && matches!(entry.category, MemoryCategory::Conversation)
                    {
                        continue;
                    }
                    if zeroclaw_memory::is_assistant_autosave_key(&entry.key) {
                        continue;
                    }
                    // Skip raw per-turn user messages: re-injecting them causes each
                    // recalled entry to embed all prior generations, growing exponentially.
                    // Consolidated knowledge is already promoted to Core/Daily entries.
                    if zeroclaw_memory::is_user_autosave_key(&entry.key) {
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
                    if !included {
                        context.push_str(MEMORY_CONTEXT_OPEN);
                        context.push('\n');
                        included = true;
                    }
                    let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
                }
                if included {
                    context.push_str(MEMORY_CONTEXT_CLOSE);
                    context.push_str("\n\n");
                }
            }
        }
        Err(_) => {
            observer.record_event(&ObserverEvent::MemoryRecall {
                query_summary,
                duration,
                num_entries: 0,
                backend,
                success: false,
            });
            // Preserve original swallow behavior — recall errors are not
            // propagated; an empty context string is returned.
        }
    }

    context
}

/// Build hardware datasheet context from RAG when peripherals are enabled.
/// Includes pin-alias lookup (e.g. "red_led" → 13) when query matches, plus retrieved chunks.
fn build_hardware_context(
    rag: &crate::rag::HardwareRag,
    observer: &dyn Observer,
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

    let start = std::time::Instant::now();
    let chunks = rag.retrieve(user_msg, boards, chunk_limit);
    let duration = start.elapsed();
    observer.record_event(&ObserverEvent::RagRetrieve {
        query_summary: make_query_summary(user_msg),
        duration,
        num_chunks: chunks.len(),
        num_boards: boards.len(),
    });

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
pub use super::tool_execution::{ToolExecutionOutcome, should_execute_tools_in_parallel};

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
/// When `silent` is true, suppresses stdout (for channel use).
#[allow(clippy::too_many_arguments)]
pub async fn agent_turn(
    model_provider: &dyn ModelProvider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: Option<f64>,
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
    strict_tool_parsing: bool,
    parallel_tools: bool,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    channel: Option<&dyn Channel>,
) -> Result<String> {
    let turn_id = uuid::Uuid::new_v4().to_string();
    run_tool_call_loop(ToolLoop {
        exec: ResolvedAgentExecution::resolve(
            ResolvedModelAccess {
                model_provider,
                provider_name,
                model,
                temperature,
            },
            ResolvedIo {
                tools_registry,
                observer,
                silent,
                approval,
                multimodal_config,
                hooks: None,
                activated_tools,
                model_switch_callback,
                receipt_generator: None,
            },
            ResolvedRuntimeKnobs {
                max_tool_iterations,
                excluded_tools,
                dedup_exempt_tools,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing,
                parallel_tools,
                max_tool_result_chars,
                context_token_budget,
                knobs: &LoopKnobs::default(),
            },
        ),
        history,
        channel_name,
        channel_reply_target,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None, // no shared budget for agent_turn callers
        channel,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted. Real per-transport
        // stamping is PR C (RFC #6971 §4).
        ingress: IngressContext::internal(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
}

// ── Agent Tool-Call Loop ──────────────────────────────────────────────────
// The turn engine lives in `super::turn` — `run_tool_call_loop` plus one
// file per step (run sheet in agent/turn/mod.rs). `crate::agent::loop_`
// stays the canonical public path via these re-exports.
pub(crate) use super::turn::StreamCancelledAfterOutput;
#[cfg(test)]
pub(crate) use super::turn::{
    DEFAULT_MAX_TOOL_ITERATIONS, MAX_MALFORMED_TOOL_PROTOCOL_RETRIES,
    build_native_assistant_history, consume_provider_streaming_response,
    maybe_inject_channel_delivery_defaults, resolve_display_text,
};
pub use super::turn::{
    DraftEvent, LoopKnobs, MaxIterationBehavior, ModelSwitchCallback, ModelSwitchRequested,
    PROGRESS_MIN_INTERVAL_MS, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess,
    ResolvedRuntimeKnobs, StreamDelta, ToolLoop, ToolLoopCancelled, drain_steering_messages,
    is_model_switch_requested, is_tool_loop_cancelled, run_tool_call_loop, scrub_credentials,
};

/// Build the tool instruction block for the system prompt so the LLM knows
/// how to invoke tools.
pub fn build_tool_instructions(tools_registry: &[Box<dyn Tool>]) -> String {
    build_tool_instructions_for_tools(tools_registry.iter().map(|tool| tool.as_ref()))
}

/// Build tool instructions for the subset of registered tools that are
/// effective for the current prompt.
pub fn build_tool_instructions_for_names(
    tools_registry: &[Box<dyn Tool>],
    effective_tool_names: &HashSet<&str>,
) -> String {
    build_tool_instructions_for_tools(
        tools_registry
            .iter()
            .map(|tool| tool.as_ref())
            .filter(|tool| effective_tool_names.contains(tool.name())),
    )
}

fn build_tool_instructions_for_tools<'a>(tools: impl IntoIterator<Item = &'a dyn Tool>) -> String {
    let tools: Vec<&dyn Tool> = tools.into_iter().collect();
    if tools.is_empty() {
        return String::new();
    }

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

    for tool in tools {
        let desc = tool.description();
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

fn retain_registered_tool_descriptions(
    tool_descs: &mut Vec<(&str, &str)>,
    tools_registry: &[Box<dyn Tool>],
) {
    let registered_tool_names: HashSet<&str> =
        tools_registry.iter().map(|tool| tool.name()).collect();
    tool_descs.retain(|(name, _)| registered_tool_names.contains(name));
}

pub fn apply_text_tool_prompt_policy(
    native_tools: bool,
    strict_tool_parsing: bool,
    tool_descs: &mut Vec<(&str, &str)>,
    deferred_section: &mut String,
) -> bool {
    let expose_text_tool_protocol = !native_tools && !strict_tool_parsing;
    if !native_tools && strict_tool_parsing {
        tool_descs.clear();
        deferred_section.clear();
    }
    expose_text_tool_protocol
}

// ── CLI Entrypoint ───────────────────────────────────────────────────────
// Wires up all subsystems (observer, runtime, security, memory, tools,
// model_provider, hardware RAG, peripherals) and enters either single-shot or
// interactive REPL mode. The interactive loop manages history compaction
// and hard trimming to keep the context window bounded.

/// Optional per-call overrides for [`run`].
///
/// SubAgent spawn paths use this to inject the validated child policy
/// returned from [`SecurityPolicy::ensure_no_escalation_beyond`] (and,
/// once caller-supplied allowlist narrowing lands, the
/// validated agent-scoped memory wrapper). Without this hook the run
/// path rebuilds both surfaces from config, so the validator's
/// guarantees never reach the agent loop. `None` on either field
/// preserves the from-config behavior — the same shape as a fresh
/// interactive launch.
#[derive(Default)]
pub struct AgentRunOverrides {
    pub security: Option<Arc<SecurityPolicy>>,
    pub memory: Option<Arc<dyn Memory>>,
    /// `true` when the run is a SubAgent invocation. SubAgents must not
    /// spawn further subagents (depth-1 cap). The agent loop reads this
    /// when constructing the `spawn_subagent` tool so the depth-cap
    /// refusal fires at the tool, not after a child run is already
    /// underway. Default `false` keeps top-level / cron-launched /
    /// CLI-launched agents at depth 0.
    pub is_subagent: bool,
}

/// Build the dotted provider ref (`"openai.qwertfoozp"`) from the agent's
/// configured `model_provider` field. Returns `None` when the agent has no
/// `model_provider` set or when the ref does not resolve to a known alias.
///
/// Using the full dotted ref (rather than just the family type) ensures the
/// alias-aware factory path is taken, so config fields such as
/// `requires_openai_auth` reach `dispatch_family_factory` instead of being
/// silently dropped.
fn agent_provider_composite(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> Option<String> {
    config
        .resolved_model_provider_for_agent(agent_alias)
        .map(|(ty, alias, _)| format!("{ty}.{alias}"))
}

/// Return the owned agent config direct-turn setup needs, with runtime-profile
/// values baked into `resolved`.
fn resolved_agent_for_turn(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> Result<zeroclaw_config::schema::AliasedAgentConfig> {
    let agent = config
        .resolved_agent_config(agent_alias)
        .with_context(|| format!("agents.{agent_alias} is not configured"))?;
    #[cfg(test)]
    if let Some(hook) = RESOLVED_AGENT_FOR_TURN_TEST_HOOK
        .lock()
        .expect("resolved-agent test hook lock should not be poisoned")
        .as_ref()
        .cloned()
    {
        hook(agent_alias, agent.resolved.max_tool_iterations);
    }
    Ok(agent)
}

#[cfg(test)]
type ResolvedAgentForTurnTestHook = Arc<dyn Fn(&str, usize) + Send + Sync>;

#[cfg(test)]
static RESOLVED_AGENT_FOR_TURN_TEST_HOOK: LazyLock<Mutex<Option<ResolvedAgentForTurnTestHook>>> =
    LazyLock::new(|| Mutex::new(None));

/// Resolve (api_key, uri) for `provider_name`, preferring the alias-specific
/// config when `provider_name` is a dotted `<family>.<alias>` reference.
/// Falls back to `fallback` (the agent's configured provider) for bare family
/// names or when the alias isn't found.
///
/// This prevents `-p openai.shartgpt` (OAuth, no key) from inheriting the
/// agent's current provider key (e.g. an xai key), which would trigger the
/// API key prefix-mismatch preflight and block providers that authenticate
/// via OAuth rather than an explicit API key.
fn api_key_and_uri_for_provider(
    config: &zeroclaw_config::schema::Config,
    provider_name: &str,
    fallback: Option<&zeroclaw_config::schema::ModelProviderConfig>,
) -> (Option<String>, Option<String>) {
    if let Some((fam, al)) = provider_name.split_once('.')
        && let Some(entry) = config.providers.models.find(fam, al)
    {
        return (entry.api_key.clone(), entry.uri.clone());
    }
    (
        fallback.and_then(|e| e.api_key.clone()),
        fallback.and_then(|e| e.uri.clone()),
    )
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    agent_alias: &str,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: Option<f64>,
    peripheral_overrides: Vec<String>,
    interactive: bool,
    session_state_file: Option<PathBuf>,
    allowed_tools: Option<Vec<String>>,
    overrides: AgentRunOverrides,
) -> Result<String> {
    use ::zeroclaw_log::Instrument;
    let agent = resolved_agent_for_turn(&config, agent_alias)?;
    crate::agent::thinking::validate_thinking_config(&agent.resolved.thinking);
    let risk_profile = config
        .risk_profile_for_agent(agent_alias)
        .with_context(|| {
            format!(
                "agents.{agent_alias}.risk_profile does not name a configured risk_profiles entry"
            )
        })?
        .clone();
    let memory_composite = {
        use zeroclaw_config::multi_agent::MemoryBackendKind;
        match agent.memory.backend {
            MemoryBackendKind::Markdown => format!("markdown.{agent_alias}"),
            MemoryBackendKind::None => "none".to_string(),
            _ => {
                let raw = config.memory.backend.trim();
                if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
                    "none".to_string()
                } else {
                    let (kind, alias) = raw.split_once('.').unwrap_or((raw, "default"));
                    format!("{kind}.{alias}")
                }
            }
        }
    };
    let __zc_alias = agent_alias.to_string();
    let __zc_attribution_span =
        ::zeroclaw_log::attribution_span!(&crate::agent::AgentAttribution(__zc_alias.as_str()));
    let __zc_scope_span = ::zeroclaw_log::info_span!(
        target: "zeroclaw_log_internal_scope",
        "zeroclaw_scope",
        risk_profile = %agent.risk_profile,
        runtime_profile = %agent.runtime_profile,
        memory_namespace = %memory_composite,
    );
    let __zc_body = async move {
        let agent_alias: &str = __zc_alias.as_str();
        // ── Effective per-agent runtime tunables ──────────────────────
        // Profile values (when set) override the agent's inline fields.
        // See `Config::resolved_agent_config` for precedence rules.
        let eff_max_history_messages = agent.resolved.max_history_messages;
        let eff_max_context_tokens = agent.resolved.max_context_tokens;
        let eff_compact_context = agent.resolved.compact_context;
        let eff_max_system_prompt_chars = agent.resolved.max_system_prompt_chars;
        let base_observer = observability::create_observer(&config.observability);
        let observer: Arc<dyn Observer> = Arc::from(base_observer);
        let turn_id = uuid::Uuid::new_v4().to_string();
        let channel_name = if interactive { "cli" } else { "daemon" };
        // CLI one-shot / REPL (`interactive = true`) exits before the OTLP batch
        // exporter's background interval fires. Hold a FlushGuard for the rest of
        // this body so every return path — including `?` errors — pushes buffered
        // telemetry before the runtime is torn down. Daemon/cron/subagent callers
        // pass `interactive = false` and skip this; they rely on periodic export.
        let _flush_guard = interactive.then(|| observability::FlushGuard::new(observer.clone()));
        if interactive
            && matches!(
                config.observability.backend,
                zeroclaw_config::schema::ObservabilityBackend::Prometheus
            )
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Observability backend is Prometheus (pull/scrape model): a one-shot CLI process \
                 exits before any scraper can pull, so its telemetry will not be collected. \
                 Prometheus is intended for long-running (daemon) deployments."
            );
        }
        let runtime: Arc<dyn platform::RuntimeAdapter> =
            Arc::from(platform::create_runtime(&config.runtime)?);
        let is_subagent_caller = overrides.is_subagent;
        let security = match overrides.security {
            Some(sec) => sec,
            None => Arc::new(SecurityPolicy::for_agent(&config, agent_alias)?),
        };

        let agent_provider_resolved = config
            .resolved_model_provider_for_agent(agent_alias)
            .map(|(ty, alias, cfg)| (ty, alias.to_string(), cfg.clone()));
        let agent_model_provider = agent_provider_resolved.as_ref().map(|(_, _, cfg)| cfg);

        // ── Memory (the brain) ────────────────────────────────────────
        // Per-agent memory: the inner backend is the install-wide store
        // (or, for Markdown agents, the agent's own dir composed with
        // peer dirs); the wrapper stamps every store with the bound
        // agent's UUID and filters every recall by the resolved
        // `read_memory_from` allowlist. When the caller supplies a
        // pre-built memory handle (SubAgent narrowing path), use that
        // instead so the validator's allowlist subset reaches the loop.
        let mem: Arc<dyn Memory> = match overrides.memory {
            Some(m) => m,
            None => {
                zeroclaw_memory::create_memory_for_agent(
                    &config,
                    agent_alias,
                    agent_model_provider.and_then(|e| e.api_key.as_deref()),
                )
                .await?
            }
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                .with_category(::zeroclaw_log::EventCategory::Memory)
                .with_attrs(::serde_json::json!({"backend": mem.name()})),
            "Memory initialized"
        );

        // ── Peripherals (merge peripheral tools into registry) ─
        if !peripheral_overrides.is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({"peripherals": peripheral_overrides})),
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

        // Build SOP engine when sops_dir is configured so SOP tools are
        // available on this path (CLI agent run).
        let (sop_engine, sop_audit) = if config.sop.sops_dir.is_some() {
            let sop_mem: Arc<dyn zeroclaw_memory::Memory> =
                zeroclaw_memory::create_memory_for_agent(&config, agent_alias, None).await?;
            let (engine, audit) =
                crate::sop::build_sop_engine(config.sop.clone(), &config.data_dir, sop_mem);
            (Some(engine), Some(audit))
        } else {
            (None, None)
        };

        let all_tools_result = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            &risk_profile,
            agent_alias,
            runtime.clone(),
            mem.clone(),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.data_dir,
            &config.agents,
            agent_model_provider.and_then(|e| e.api_key.as_deref()),
            &config,
            None,
            is_subagent_caller,
            None,
            sop_engine,
            sop_audit,
            None,
        );
        let skills = crate::skills::load_skills_for_agent_from_config(&config, agent_alias);
        // Route the per-agent tool registry through the one gated seam
        // (peripherals -> built-in filter -> MCP scope+gate -> skills), identical
        // to the behavior this path hand-rolled. `caller_allowed` carries the
        // run() per-run allowlist; connect_peripherals is true (execution path).
        let scoped::ScopedAssembled {
            registry,
            delegate_handle: _,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            deferred_section,
            activated_handle,
        } = scoped::ScopedToolRegistry::assemble(scoped::ScopedAssembly {
            config: &config,
            agent_alias,
            security: &security,
            built: all_tools_result,
            skills: &skills,
            runtime: runtime.clone(),
            caller_allowed: allowed_tools.as_deref(),
            connect_mcp: true,
            connect_peripherals: true,
            exclude_memory: false,
            emit_assembly_logs: true,
        })
        .await;
        let tools_registry = registry.into_inner();

        // Populate all channel-driven tool handles from the registered factory.
        let count = seed_channel_handles(
            &ask_user_handle,
            &channel_room_handle,
            &reaction_handle,
            &poll_handle,
            &escalate_handle,
        );
        if count > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Register)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_attrs(::serde_json::json!({"count": count})),
                &format!("Registered {} channel(s) for CLI agent", count),
            );
        }

        // ── Resolve model_provider ─────────────────────────────────────────
        let agent_provider_ref = agent_provider_composite(&config, agent_alias);
        let mut provider_name = provider_override
            .as_deref()
            .or(agent_provider_ref.as_deref())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"agent_alias": agent_alias})),
                    "agent loop refused: agent.model_provider unresolved and no --provider override"
                );
                anyhow::Error::msg(format!(
                    "agents.{agent_alias}.model_provider does not resolve and no provider override \
                     was passed on the CLI. Either set `[agents.{agent_alias}] model_provider` or \
                     pass --provider."
                ))
            })?
            .to_string();

        let mut model_name = match model_override
            .as_deref()
            .or(agent_model_provider.and_then(|e| e.model.as_deref()))
        {
            Some(m) => m.to_string(),
            None => anyhow::bail!(
                "no model configured for agent {agent_alias}: \
             [providers.models.{provider_name}.<alias>].model is unset and --model was not passed"
            ),
        };

        {
            let span = zeroclaw_log::Span::current();
            let mp_composite = match agent_provider_resolved.as_ref() {
                Some((ty, alias, _)) => format!("{ty}.{alias}"),
                None => provider_name.clone(),
            };
            span.record("model_provider", mp_composite.as_str());
            span.record("model", model_name.as_str());
        }

        let provider_runtime_options = match agent_provider_resolved.as_ref() {
            Some((ty, alias, _)) => {
                zeroclaw_providers::provider_runtime_options_for_alias(&config, ty, alias)
            }
            None => zeroclaw_providers::provider_runtime_options_for_agent(&config, agent_alias),
        };

        // Resolve api_key and uri from the actual provider being constructed.
        // For dotted aliases (e.g. "openai.shartgpt"), look up the alias-specific
        // config so a -p override does not leak the agent's current provider key
        // (e.g. an xai key) to a different provider family that doesn't expect it.
        let (initial_api_key, initial_uri) =
            api_key_and_uri_for_provider(&config, &provider_name, agent_model_provider);
        let mut model_provider: Box<dyn ModelProvider> =
            zeroclaw_providers::create_routed_model_provider_with_options(
                &config,
                &provider_name,
                initial_api_key.as_deref(),
                initial_uri.as_deref(),
                &config.reliability,
                &config.model_routes,
                &model_name,
                &provider_runtime_options,
            )?;

        let model_switch_callback = get_model_switch_state();

        observer.record_event(&ObserverEvent::AgentStart {
            model_provider: provider_name.to_string(),
            model: model_name.to_string(),
            channel: Some(channel_name.to_string()),
            agent_alias: Some(agent_alias.to_string()),
            turn_id: Some(turn_id.clone()),
        });

        // ── Hardware RAG (datasheet retrieval when peripherals + datasheet_dir) ──
        let hardware_rag: Option<crate::rag::HardwareRag> = config
            .peripherals
            .datasheet_dir
            .as_ref()
            .filter(|d| !d.trim().is_empty())
            .map(|dir| crate::rag::HardwareRag::load(&config.data_dir, dir.trim()))
            .and_then(Result::ok)
            .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
        if let Some(ref rag) = hardware_rag {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({"chunks": rag.len()})),
                "Hardware RAG loaded"
            );
        }

        let board_names: Vec<String> = config
            .peripherals
            .boards
            .iter()
            .map(|b| b.board.clone())
            .collect();

        // ── Initialize locale-aware tool descriptions ──────────────────
        let i18n_locale = config
            .locale
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(crate::i18n::detect_locale);
        crate::i18n::init(&i18n_locale);

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
            "channel_room",
            "Create channel rooms and invite users through active channels. Use with Matrix channel keys such as matrix.default.",
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
        retain_registered_tool_descriptions(&mut tool_descs, &tools_registry);
        let bootstrap_max_chars = if eff_compact_context {
            Some(6000)
        } else {
            None
        };
        let prompt_excluded_tools = message
            .as_deref()
            .map(|msg| {
                compute_excluded_mcp_tools(&tools_registry, &agent.resolved.tool_filter_groups, msg)
            })
            .unwrap_or_default();
        let agent_workspace = config.agent_workspace_dir(agent_alias);
        let mut system_prompt = build_system_prompt_for_turn(
            &agent_workspace,
            &model_name,
            &tool_descs,
            &deferred_section,
            &skills,
            Some(&agent.identity),
            bootstrap_max_chars,
            &risk_profile,
            model_provider.as_ref(),
            &tools_registry,
            &prompt_excluded_tools,
            activated_handle.as_ref(),
            agent.resolved.strict_tool_parsing,
            config.skills.prompt_injection_mode,
            eff_compact_context,
            eff_max_system_prompt_chars,
            true,
            config.channels.show_tool_calls,
            None,
        )?;

        // ── Approval manager (supervised mode) ───────────────────────
        let approval_manager = if interactive {
            Some(ApprovalManager::from_risk_profile(&risk_profile))
        } else {
            None
        };
        let memory_session_id = session_state_file.as_deref().and_then(|path| {
            let raw = path.to_string_lossy().trim().to_string();
            if raw.is_empty() {
                None
            } else {
                // Match the sanitized form persisted by memory backend migrations.
                Some(zeroclaw_api::session_keys::sanitize_session_key(&format!(
                    "cli:{raw}"
                )))
            }
        });

        // ── Cost tracking context (scoped for CLI / cron / web agents) ──
        let cost_tracking_context =
            crate::agent::cost::tool_loop_cost_tracking_context_for_agent(&config, agent_alias);

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
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_attrs(::serde_json::json!({"thinking_level": level})),
                            "Thinking directive parsed from message"
                        );
                        (Some(level), remaining)
                    }
                    None => (None, msg.clone()),
                };
            let thinking_level = crate::agent::thinking::resolve_thinking_level(
                thinking_directive,
                None,
                &agent.resolved.thinking,
            );
            let thinking_params = crate::agent::thinking::apply_thinking_level_with_config(
                thinking_level,
                &agent.resolved.thinking,
            );
            let effective_temperature: Option<f64> = temperature.map(|t| {
                crate::agent::thinking::clamp_temperature(
                    t + thinking_params.temperature_adjustment,
                )
            });

            // Compute per-turn excluded MCP tools from tool_filter_groups before
            // building the turn prompt so tool availability matches the specs
            // sent to the provider.
            let excluded_tools = compute_excluded_mcp_tools(
                &tools_registry,
                &agent.resolved.tool_filter_groups,
                &effective_msg,
            );
            system_prompt = build_system_prompt_for_turn(
                &agent_workspace,
                &model_name,
                &tool_descs,
                &deferred_section,
                &skills,
                Some(&agent.identity),
                bootstrap_max_chars,
                &risk_profile,
                model_provider.as_ref(),
                &tools_registry,
                &excluded_tools,
                activated_handle.as_ref(),
                agent.resolved.strict_tool_parsing,
                config.skills.prompt_injection_mode,
                eff_compact_context,
                eff_max_system_prompt_chars,
                true,
                config.channels.show_tool_calls,
                thinking_params.system_prompt_prefix.as_deref(),
            )?;

            let excluded_tool_names: HashSet<&str> =
                excluded_tools.iter().map(String::as_str).collect();
            let runtime_capability_names = tools_registry
                .iter()
                .map(|tool| tool.name())
                .filter(|name| !excluded_tool_names.contains(*name))
                .collect::<Vec<_>>();
            if let Some(suggestion) = crate::skills::render_missing_skill_install_suggestion(
                &effective_msg,
                &skills,
                &runtime_capability_names,
                &config.data_dir,
                &config.skills.extra_registries,
                config.skills.install_suggestions.enabled,
            ) {
                final_output = suggestion;
                println!("{final_output}");
                observer.record_event(&ObserverEvent::TurnComplete);
                return Ok(final_output);
            }

            // Auto-save user message to memory (skip short/trivial messages)
            if config.memory.auto_save
                && effective_msg.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
                && !zeroclaw_memory::should_skip_autosave_content(&effective_msg)
            {
                let user_key = autosave_memory_key("user_msg");
                let store_start = std::time::Instant::now();
                let store_result = mem
                    .store(
                        &user_key,
                        &effective_msg,
                        MemoryCategory::Conversation,
                        memory_session_id.as_deref(),
                    )
                    .await;
                observer.record_event(&ObserverEvent::MemoryStore {
                    category: MemoryCategory::Conversation.to_string(),
                    backend: mem.name().to_string(),
                    duration: store_start.elapsed(),
                    success: store_result.is_ok(),
                });
            }

            // Inject memory + hardware RAG context into user message.
            // Exclude Conversation-category memories when:
            //   - non-interactive (cron, daemon heartbeat): chat history must
            //     not leak into autonomous executions / #5456, OR
            //   - no session scope is available (memory_session_id is None):
            //     without a session filter, Conversation entries from other
            //     channels (Matrix, Discord, …) would bleed into this session.
            let exclude_conv = !interactive || memory_session_id.is_none();
            let mem_context = build_context(
                mem.as_ref(),
                &*observer,
                &effective_msg,
                config.memory.min_relevance_score,
                memory_session_id.as_deref(),
                exclude_conv,
            )
            .await;
            let rag_limit = if eff_compact_context { 2 } else { 5 };
            let hw_context = hardware_rag
                .as_ref()
                .map(|r| {
                    build_hardware_context(r, &*observer, &effective_msg, &board_names, rag_limit)
                })
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

            // Compute per-turn excluded MCP tools from tool_filter_groups.
            let excluded_tools = compute_excluded_mcp_tools(
                &tools_registry,
                &agent.resolved.tool_filter_groups,
                &effective_msg,
            );

            #[allow(unused_assignments)]
            let mut response = String::new();
            loop {
                if let Some(sys_msg) = history.first_mut()
                    && sys_msg.role == "system"
                {
                    sys_msg.content = build_system_prompt_for_turn(
                        &agent_workspace,
                        &model_name,
                        &tool_descs,
                        &deferred_section,
                        &skills,
                        Some(&agent.identity),
                        bootstrap_max_chars,
                        &risk_profile,
                        model_provider.as_ref(),
                        &tools_registry,
                        &excluded_tools,
                        activated_handle.as_ref(),
                        agent.resolved.strict_tool_parsing,
                        config.skills.prompt_injection_mode,
                        eff_compact_context,
                        eff_max_system_prompt_chars,
                        true,
                        config.channels.show_tool_calls,
                        thinking_params.system_prompt_prefix.as_deref(),
                    )?;
                }
                match zeroclaw_api::NATIVE_THINKING_OVERRIDE
                    .scope(
                        thinking_params.native_thinking,
                        TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                            cost_tracking_context.clone(),
                            run_tool_call_loop(ToolLoop {
                                exec: ResolvedAgentExecution::resolve(
                                    ResolvedModelAccess {
                                        model_provider: model_provider.as_ref(),
                                        provider_name: &provider_name,
                                        model: &model_name,
                                        temperature: effective_temperature,
                                    },
                                    ResolvedIo {
                                        tools_registry: &tools_registry,
                                        observer: observer.as_ref(),
                                        silent: false,
                                        approval: approval_manager.as_ref(),
                                        multimodal_config: &config.multimodal,
                                        hooks: None,
                                        activated_tools: activated_handle.as_ref(),
                                        model_switch_callback: Some(model_switch_callback.clone()),
                                        receipt_generator: None,
                                    },
                                    ResolvedRuntimeKnobs {
                                        max_tool_iterations: agent.resolved.max_tool_iterations,
                                        excluded_tools: &excluded_tools,
                                        dedup_exempt_tools: &agent.resolved.tool_call_dedup_exempt,
                                        pacing: &config.pacing,
                                        strict_tool_parsing: agent.resolved.strict_tool_parsing,
                                        parallel_tools: agent.resolved.parallel_tools,
                                        max_tool_result_chars: agent.resolved.max_tool_result_chars,
                                        context_token_budget: agent
                                            .resolved
                                            .effective_context_budget(),
                                        knobs: &LoopKnobs::default(),
                                    },
                                ),
                                history: &mut history,
                                channel_name,
                                channel_reply_target: None,
                                cancellation_token: None,
                                on_delta: None,
                                shared_budget: None,
                                channel: None,
                                collected_receipts: None,
                                event_tx: None,
                                steering: None,
                                new_messages_out: None,
                                image_cache: None,
                                // Phase 1: stamp Internal/Trusted. Real per-transport
                                // stamping is PR C (RFC #6971 §4).
                                ingress: IngressContext::internal(),
                                agent_alias: Some(agent_alias),
                                turn_id: &turn_id,
                            }),
                        ),
                    )
                    .await
                {
                    Ok(resp) => {
                        response = resp;
                        break;
                    }
                    Err(e) => {
                        if let Some((new_model_provider, new_model)) = is_model_switch_requested(&e)
                        {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Migrate
                                )
                                .with_category(::zeroclaw_log::EventCategory::Provider),
                                &format!(
                                    "Model switch requested, switching from {} {} to {} {}",
                                    provider_name, model_name, new_model_provider, new_model
                                )
                            );

                            let (switch_api_key, switch_uri) = api_key_and_uri_for_provider(
                                &config,
                                &new_model_provider,
                                agent_model_provider,
                            );
                            model_provider =
                                zeroclaw_providers::create_routed_model_provider_with_options(
                                    &config,
                                    &new_model_provider,
                                    switch_api_key.as_deref(),
                                    switch_uri.as_deref(),
                                    &config.reliability,
                                    &config.model_routes,
                                    &new_model,
                                    &zeroclaw_providers::options_for_provider_ref(
                                        &config,
                                        &new_model_provider,
                                        &zeroclaw_providers::provider_runtime_options_for_agent(
                                            &config,
                                            agent_alias,
                                        ),
                                    ),
                                )?;

                            provider_name = new_model_provider;
                            model_name = new_model;

                            clear_model_switch_request();

                            observer.record_event(&ObserverEvent::AgentStart {
                                model_provider: provider_name.to_string(),
                                model: model_name.to_string(),
                                channel: Some(channel_name.to_string()),
                                agent_alias: Some(agent_alias.to_string()),
                                turn_id: Some(turn_id.clone()),
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
                        config.data_dir.clone(),
                        config.skills.skill_creation.clone(),
                    );
                    match creator.create_from_execution(&msg, &tool_calls, None).await {
                        Ok(Some(slug)) => {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Register
                                )
                                .with_category(::zeroclaw_log::EventCategory::Agent)
                                .with_attrs(::serde_json::json!({"slug": slug})),
                                "Auto-created skill from execution"
                            );
                        }
                        Ok(None) => {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Skip
                                )
                                .with_category(::zeroclaw_log::EventCategory::Agent),
                                "Skill creation skipped (duplicate or disabled)"
                            );
                        }
                        Err(e) => ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Skill creation failed"
                        ),
                    }
                }
            }
            // Emit the user-visible response before any background work so the
            // skill-review fork can never delay the user's answer.
            final_output = response;
            println!("{final_output}");
            observer.record_event(&ObserverEvent::TurnComplete);

            // Background skill review fork — post-turn, opt-in
            // (`skills.skill-improvement.enabled`, default false). Runs a forked
            // agent with a restricted toolset (skills_list, skill_view,
            // skill_manage) over a snapshot of the conversation and decides
            // whether to patch SKILL.md, add a support file, or do nothing.
            //
            // Scoped under TOOL_LOOP_COST_TRACKING_CONTEXT with the same
            // `cost_tracking_context` as the parent turn, so the fork's provider
            // calls are recorded against — and bounded by — the same cost
            // tracker and budget (the scope wrapping the parent turn's
            // `run_tool_call_loop` has already exited by this point).
            // See `crate::skills::review::maybe_run_skill_review`.
            if config.skills.skill_improvement.enabled {
                let review_workspace = config.agent_workspace_dir(agent_alias);
                let review_config = config.skills.skill_improvement.clone();
                let failed_slugs: Vec<String> =
                    crate::skills::improver::extract_skill_executions_from_history(&history)
                        .into_iter()
                        .filter_map(|(slug, ok)| if ok { None } else { Some(slug) })
                        .collect();
                TOOL_LOOP_COST_TRACKING_CONTEXT
                    .scope(
                        cost_tracking_context.clone(),
                        crate::skills::review::maybe_run_skill_review(
                            review_workspace,
                            review_config,
                            config.skills.allow_scripts,
                            history.clone(),
                            failed_slugs,
                            model_provider.as_ref(),
                            &provider_name,
                            &model_name,
                            observer.as_ref(),
                            &config.multimodal,
                            &config.pacing,
                            agent.resolved.max_tool_result_chars,
                            agent.resolved.max_context_tokens,
                            None, // cancellation_token — no parent token in single-shot run
                            Some(agent_alias),
                        ),
                    )
                    .await;
            }
        } else {
            println!("🦀 ZeroClaw Interactive Mode");
            println!("Type /help for commands.\n");
            let cli = CLI_CHANNEL_FN.get().expect(
                "CLI channel factory not registered — call register_cli_channel_fn at startup",
            )();

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
                // Capped at MAX_INTERACTIVE_INPUT_BYTES so a pipe of
                // `head -c 10G /dev/zero | zeroclaw chat` cannot blow up RSS.
                let input = {
                    let stdin = std::io::stdin().lock();
                    match read_capped_line(stdin, MAX_INTERACTIVE_INPUT_BYTES) {
                        Ok(CappedLine::Eof) => break,
                        Ok(CappedLine::Line(s)) => s,
                        Ok(CappedLine::Truncated) => {
                            eprintln!(
                                "\nWarning: input line exceeds {} bytes and was discarded.",
                                MAX_INTERACTIVE_INPUT_BYTES
                            );
                            continue;
                        }
                        Err(e) => {
                            eprintln!("\nError reading input: {e}\n");
                            break;
                        }
                    }
                };

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

                        let confirm = {
                            let stdin = std::io::stdin().lock();
                            match read_capped_line(stdin, MAX_INTERACTIVE_INPUT_BYTES) {
                                Ok(CappedLine::Line(s)) => s,
                                Ok(CappedLine::Truncated) | Ok(CappedLine::Eof) | Err(_) => {
                                    println!("Cancelled.\n");
                                    continue;
                                }
                            }
                        };
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
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_category(::zeroclaw_log::EventCategory::Agent)
                                .with_attrs(::serde_json::json!({"thinking_level": level})),
                                "Thinking directive parsed"
                            );
                            (Some(level), remaining)
                        }
                        None => (None, user_input.clone()),
                    };
                let thinking_level = crate::agent::thinking::resolve_thinking_level(
                    thinking_directive,
                    None,
                    &agent.resolved.thinking,
                );
                let thinking_params = crate::agent::thinking::apply_thinking_level_with_config(
                    thinking_level,
                    &agent.resolved.thinking,
                );
                let turn_temperature: Option<f64> = temperature.map(|t| {
                    crate::agent::thinking::clamp_temperature(
                        t + thinking_params.temperature_adjustment,
                    )
                });

                // Compute per-turn excluded MCP tools from tool_filter_groups
                // before the provider call; the system prompt is rebuilt from
                // this same set immediately before each attempt.
                let excluded_tools = compute_excluded_mcp_tools(
                    &tools_registry,
                    &agent.resolved.tool_filter_groups,
                    &effective_input,
                );

                let excluded_tool_names: HashSet<&str> =
                    excluded_tools.iter().map(String::as_str).collect();
                let runtime_capability_names = tools_registry
                    .iter()
                    .map(|tool| tool.name())
                    .filter(|name| !excluded_tool_names.contains(*name))
                    .collect::<Vec<_>>();
                if let Some(suggestion) = crate::skills::render_missing_skill_install_suggestion(
                    &effective_input,
                    &skills,
                    &runtime_capability_names,
                    &config.data_dir,
                    &config.skills.extra_registries,
                    config.skills.install_suggestions.enabled,
                ) {
                    final_output = suggestion;
                    if let Err(e) = zeroclaw_api::channel::Channel::send(
                        &*cli,
                        &zeroclaw_api::channel::SendMessage::new(
                            format!("\n{final_output}\n"),
                            "user",
                        ),
                    )
                    .await
                    {
                        eprintln!("\nError sending CLI response: {e}\n");
                    }
                    observer.record_event(&ObserverEvent::TurnComplete);
                    if let Some(sys_msg) = history.first_mut()
                        && sys_msg.role == "system"
                    {
                        sys_msg.content.clone_from(&base_system_prompt);
                    }
                    continue;
                }

                // Auto-save conversation turns (skip short/trivial messages)
                if config.memory.auto_save
                    && effective_input.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS
                    && !zeroclaw_memory::should_skip_autosave_content(&effective_input)
                {
                    let user_key = autosave_memory_key("user_msg");
                    let store_start = std::time::Instant::now();
                    let store_result = mem
                        .store(
                            &user_key,
                            &effective_input,
                            MemoryCategory::Conversation,
                            memory_session_id.as_deref(),
                        )
                        .await;
                    observer.record_event(&ObserverEvent::MemoryStore {
                        category: MemoryCategory::Conversation.to_string(),
                        backend: mem.name().to_string(),
                        duration: store_start.elapsed(),
                        success: store_result.is_ok(),
                    });
                }

                // Inject memory + hardware RAG context into user message.
                // Keep Conversation memories only when a session scope is
                // available; without one, cross-channel entries (Matrix,
                // Discord, …) would bleed into this interactive session.
                let mem_context = build_context(
                    mem.as_ref(),
                    &*observer,
                    &effective_input,
                    config.memory.min_relevance_score,
                    memory_session_id.as_deref(),
                    memory_session_id.is_none(),
                )
                .await;
                let rag_limit = if eff_compact_context { 2 } else { 5 };
                let hw_context = hardware_rag
                    .as_ref()
                    .map(|r| {
                        build_hardware_context(
                            r,
                            &*observer,
                            &effective_input,
                            &board_names,
                            rag_limit,
                        )
                    })
                    .unwrap_or_default();
                let context = format!("{mem_context}{hw_context}");
                let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
                let enriched = if context.is_empty() {
                    format!("[{now}] {effective_input}")
                } else {
                    format!("{context}[{now}] {effective_input}")
                };

                history.push(ChatMessage::user(&enriched));

                // Set up streaming channel so tool progress and response
                // content are printed progressively instead of buffered.
                let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);
                let content_was_streamed =
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let content_streamed_flag = content_was_streamed.clone();
                let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());

                let consumer_handle = zeroclaw_spawn::spawn!(async move {
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
                                content_streamed_flag
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                print!("{text}");
                                let _ = std::io::stdout().flush();
                            }
                        }
                    }
                });

                // Ctrl+C cancels the in-flight turn instead of killing the process.
                let cancel_token = CancellationToken::new();
                let cancel_token_clone = cancel_token.clone();
                let ctrlc_handle = zeroclaw_spawn::spawn!(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        cancel_token_clone.cancel();
                    }
                });

                let response = loop {
                    if let Some(sys_msg) = history.first_mut()
                        && sys_msg.role == "system"
                    {
                        sys_msg.content = build_system_prompt_for_turn(
                            &agent_workspace,
                            &model_name,
                            &tool_descs,
                            &deferred_section,
                            &skills,
                            Some(&agent.identity),
                            bootstrap_max_chars,
                            &risk_profile,
                            model_provider.as_ref(),
                            &tools_registry,
                            &excluded_tools,
                            activated_handle.as_ref(),
                            agent.resolved.strict_tool_parsing,
                            config.skills.prompt_injection_mode,
                            eff_compact_context,
                            eff_max_system_prompt_chars,
                            true,
                            config.channels.show_tool_calls,
                            thinking_params.system_prompt_prefix.as_deref(),
                        )?;
                    }
                    match zeroclaw_api::NATIVE_THINKING_OVERRIDE
                        .scope(
                            thinking_params.native_thinking,
                            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                                cost_tracking_context.clone(),
                                run_tool_call_loop(ToolLoop {
                                    exec: ResolvedAgentExecution::resolve(
                                        ResolvedModelAccess {
                                            model_provider: model_provider.as_ref(),
                                            provider_name: &provider_name,
                                            model: &model_name,
                                            temperature: turn_temperature,
                                        },
                                        ResolvedIo {
                                            tools_registry: &tools_registry,
                                            observer: observer.as_ref(),
                                            silent: true,
                                            approval: approval_manager.as_ref(),
                                            multimodal_config: &config.multimodal,
                                            hooks: None,
                                            activated_tools: activated_handle.as_ref(),
                                            model_switch_callback: Some(
                                                model_switch_callback.clone(),
                                            ),
                                            receipt_generator: None,
                                        },
                                        ResolvedRuntimeKnobs {
                                            max_tool_iterations: agent.resolved.max_tool_iterations,
                                            excluded_tools: &excluded_tools,
                                            dedup_exempt_tools: &agent
                                                .resolved
                                                .tool_call_dedup_exempt,
                                            pacing: &config.pacing,
                                            strict_tool_parsing: agent.resolved.strict_tool_parsing,
                                            parallel_tools: agent.resolved.parallel_tools,
                                            max_tool_result_chars: agent
                                                .resolved
                                                .max_tool_result_chars,
                                            context_token_budget: agent
                                                .resolved
                                                .effective_context_budget(),
                                            knobs: &LoopKnobs::default(),
                                        },
                                    ),
                                    history: &mut history,
                                    channel_name,
                                    channel_reply_target: None,
                                    cancellation_token: Some(cancel_token.clone()),
                                    on_delta: Some(delta_tx.clone()),
                                    shared_budget: None,
                                    channel: None,
                                    collected_receipts: None,
                                    event_tx: None,
                                    steering: None,
                                    new_messages_out: None,
                                    image_cache: None,
                                    // Phase 1: stamp Internal/Trusted. Real per-transport
                                    // stamping is PR C (RFC #6971 §4).
                                    ingress: IngressContext::internal(),
                                    agent_alias: Some(agent_alias),
                                    turn_id: &turn_id,
                                }),
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
                            if let Some((new_model_provider, new_model)) =
                                is_model_switch_requested(&e)
                            {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Migrate
                                    )
                                    .with_category(::zeroclaw_log::EventCategory::Provider),
                                    &format!(
                                        "Model switch requested, switching from {} {} to {} {}",
                                        provider_name, model_name, new_model_provider, new_model
                                    )
                                );

                                let (switch_api_key2, switch_uri2) = api_key_and_uri_for_provider(
                                    &config,
                                    &new_model_provider,
                                    agent_model_provider,
                                );
                                model_provider =
                                    zeroclaw_providers::create_routed_model_provider_with_options(
                                        &config,
                                        &new_model_provider,
                                        switch_api_key2.as_deref(),
                                        switch_uri2.as_deref(),
                                        &config.reliability,
                                        &config.model_routes,
                                        &new_model,
                                        &zeroclaw_providers::options_for_provider_ref(
                                            &config,
                                            &new_model_provider,
                                            &zeroclaw_providers::provider_runtime_options_for_agent(
                                                &config,
                                                agent_alias,
                                            ),
                                        ),
                                    )?;

                                provider_name = new_model_provider;
                                model_name = new_model;

                                clear_model_switch_request();

                                observer.record_event(&ObserverEvent::AgentStart {
                                    model_provider: provider_name.to_string(),
                                    model: model_name.to_string(),
                                    channel: Some(channel_name.to_string()),
                                    agent_alias: Some(agent_alias.to_string()),
                                    turn_id: Some(turn_id.clone()),
                                });

                                continue;
                            }
                            // Context overflow recovery: drop oldest whole
                            // turns and retry. No summarization, no splicing.
                            if zeroclaw_providers::reliable::is_context_window_exceeded(&e) {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Retry
                                    )
                                    .with_category(::zeroclaw_log::EventCategory::Agent),
                                    "Context overflow in interactive loop, attempting recovery"
                                );
                                let taken = std::mem::take(&mut history);
                                let result = crate::agent::history_trim::trim_to_recent_turns(
                                    taken,
                                    eff_max_context_tokens,
                                );
                                if result.trimmed {
                                    let mut trimmed = result.history;
                                    let system_count =
                                        trimmed.iter().take_while(|m| m.role == "system").count();
                                    trimmed.insert(
                                        system_count,
                                        crate::agent::history_trim::breadcrumb(),
                                    );
                                    history = trimmed;
                                    {
                                        let __zc_trim_span = ::zeroclaw_log::info_span!(
                                            target: "zeroclaw_log_internal_scope",
                                            "zeroclaw_scope",
                                            model = %model_name,
                                            model_provider = %provider_name,
                                        );
                                        let _zc_trim_guard = __zc_trim_span.entered();
                                        ::zeroclaw_log::record!(
                                            INFO,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Retry
                                            )
                                            .with_category(::zeroclaw_log::EventCategory::Agent)
                                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                                            .with_attrs(::serde_json::json!({
                                                "dropped_messages": result.dropped_messages,
                                                "dropped_turns": result.dropped_turns,
                                                "kept_turns": result.kept_turns,
                                            })),
                                            "Context recovered via whole-turn trim, retrying turn"
                                        );
                                    }
                                    continue;
                                }
                                history = result.history;
                                {
                                    let __zc_trim_span = ::zeroclaw_log::info_span!(
                                        target: "zeroclaw_log_internal_scope",
                                        "zeroclaw_scope",
                                        model = %model_name,
                                        model_provider = %provider_name,
                                    );
                                    let _zc_trim_guard = __zc_trim_span.entered();
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Fail
                                        )
                                        .with_category(::zeroclaw_log::EventCategory::Agent)
                                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                                        "Context overflow but only one turn remains; cannot trim further"
                                    );
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

                final_output = response;
                if content_was_streamed.load(std::sync::atomic::Ordering::Relaxed) {
                    println!();
                } else if let Err(e) = zeroclaw_api::channel::Channel::send(
                    &*cli,
                    &zeroclaw_api::channel::SendMessage::new(format!("\n{final_output}\n"), "user"),
                )
                .await
                {
                    eprintln!("\nError sending CLI response: {e}\n");
                }
                observer.record_event(&ObserverEvent::TurnComplete);

                // Hard cap as a safety net.
                trim_history(&mut history, eff_max_history_messages);

                // Restore base system prompt after the per-turn tool framing
                // and optional thinking prefix have been applied.
                if let Some(sys_msg) = history.first_mut()
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
        // Populate aggregate token usage from the cost-tracking context that
        // scoped every `run_tool_call_loop` call above — mirroring the streamed
        // turn path (`Agent::turn_streamed` → `TurnGuard`). The CLI path does
        // not set the `TOOL_LOOP_TURN_USAGE` task-local, so `snapshot_turn_usage`
        // reads the context's own accumulator, which holds the session-wide
        // totals. Without this the CLI `AgentEnd` reported `tokens_used: None`
        // even though usage was tracked.
        let tokens_used = cost_tracking_context.as_ref().and_then(|ctx| {
            let usage = ctx.snapshot_turn_usage();
            (usage.input_tokens > 0 || usage.output_tokens > 0).then_some(
                zeroclaw_api::observability_traits::TurnTokenUsage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                },
            )
        });
        observer.record_event(&ObserverEvent::AgentEnd {
            model_provider: provider_name.to_string(),
            model: model_name.to_string(),
            duration,
            tokens_used,
            cost_usd: None,
            channel: Some(channel_name.to_string()),
            agent_alias: Some(agent_alias.to_string()),
            turn_id: Some(turn_id),
        });

        Ok(final_output)
    };
    __zc_body
        .instrument(__zc_scope_span)
        .instrument(__zc_attribution_span)
        .await
}

/// Process a single message through the full agent (with tools, peripherals, memory).
/// Used by channels (Telegram, Discord, etc.) to enable hardware and tool use.
pub async fn process_message(
    config: Config,
    agent_alias: &str,
    message: &str,
    session_id: Option<&str>,
) -> Result<String> {
    use ::zeroclaw_log::Instrument;
    let agent = resolved_agent_for_turn(&config, agent_alias)?;
    crate::agent::thinking::validate_thinking_config(&agent.resolved.thinking);
    let risk_profile = config
        .risk_profile_for_agent(agent_alias)
        .with_context(|| {
            format!(
                "agents.{agent_alias}.risk_profile does not name a configured risk_profiles entry"
            )
        })?
        .clone();
    let memory_composite = {
        use zeroclaw_config::multi_agent::MemoryBackendKind;
        match agent.memory.backend {
            MemoryBackendKind::Markdown => format!("markdown.{agent_alias}"),
            MemoryBackendKind::None => "none".to_string(),
            _ => {
                let raw = config.memory.backend.trim();
                if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
                    "none".to_string()
                } else {
                    let (kind, alias) = raw.split_once('.').unwrap_or((raw, "default"));
                    format!("{kind}.{alias}")
                }
            }
        }
    };
    let __zc_alias = agent_alias.to_string();
    let __zc_message = message.to_string();
    let __zc_session_id = session_id.map(str::to_string);
    let __zc_attribution_span =
        ::zeroclaw_log::attribution_span!(&crate::agent::AgentAttribution(__zc_alias.as_str()));
    let __zc_scope_span = ::zeroclaw_log::info_span!(
        target: "zeroclaw_log_internal_scope",
        "zeroclaw_scope",
        risk_profile = %agent.risk_profile,
        runtime_profile = %agent.runtime_profile,
        memory_namespace = %memory_composite,
    );
    let __zc_body = async move {
        let agent_alias: &str = __zc_alias.as_str();
        let message: &str = __zc_message.as_str();
        let session_id: Option<&str> = __zc_session_id.as_deref();

        // ── Effective per-agent runtime tunables ──────────────────────
        // Profile values (when set) override the agent's inline fields.
        // See `Config::resolved_agent_config` for precedence rules.
        let eff_compact_context = agent.resolved.compact_context;
        let eff_max_system_prompt_chars = agent.resolved.max_system_prompt_chars;

        let observer: Arc<dyn Observer> =
            Arc::from(observability::create_observer(&config.observability));
        let runtime: Arc<dyn platform::RuntimeAdapter> =
            Arc::from(platform::create_runtime(&config.runtime)?);
        let security = Arc::new(SecurityPolicy::for_agent(&config, agent_alias)?);
        let (provider_name, provider_alias, agent_model_provider) = match config
            .resolved_model_provider_for_agent(agent_alias)
        {
            Some(resolved) => (resolved.0, resolved.1.to_string(), Some(resolved.2.clone())),
            None => {
                let agent_ref = agent.model_provider.as_str();
                if !agent_ref.is_empty() {
                    anyhow::bail!(
                        "agents.{agent_alias}.model_provider = \"{agent_ref}\" does not resolve to \
                     a configured [providers.models.<type>.<alias>] entry"
                    );
                }
                anyhow::bail!(
                    "agents.{agent_alias}.model_provider is empty \u{2014} set it to a configured \
                 \"<type>.<alias>\" (e.g. \"anthropic.{agent_alias}\")"
                );
            }
        };
        let approval_manager = ApprovalManager::for_non_interactive(&risk_profile);
        let mem: Arc<dyn Memory> = zeroclaw_memory::create_memory_for_agent(
            &config,
            agent_alias,
            agent_model_provider
                .as_ref()
                .and_then(|e| e.api_key.as_deref()),
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

        // Build SOP engine when sops_dir is configured so SOP tools are
        // available on this path (process_message CLI agent).
        let (sop_engine, sop_audit) = if config.sop.sops_dir.is_some() {
            let sop_mem: Arc<dyn zeroclaw_memory::Memory> =
                zeroclaw_memory::create_memory_for_agent(&config, agent_alias, None).await?;
            let (engine, audit) =
                crate::sop::build_sop_engine(config.sop.clone(), &config.data_dir, sop_mem);
            (Some(engine), Some(audit))
        } else {
            (None, None)
        };

        let all_tools_result_pm = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            &risk_profile,
            agent_alias,
            runtime.clone(),
            mem.clone(),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.data_dir,
            &config.agents,
            agent_model_provider
                .as_ref()
                .and_then(|e| e.api_key.as_deref()),
            &config,
            None,
            false,
            None,
            sop_engine,
            sop_audit,
            None,
        );
        let skills = crate::skills::load_skills_for_agent_from_config(&config, agent_alias);
        // Route the per-agent tool registry through the one gated seam - the same
        // seam run() uses. This UNIFIES process_message's built-in filter with every
        // other construction path: `assemble` applies the plain policy filter
        // (allowed_tools + excluded_tools), replacing filter_channel_builtin_tools,
        // which admitted the canonical read-only defaults past allowed_tools at
        // non-Full autonomy. Only process_message (i.e. gateway live chat and peer
        // delegation) had that admit; the real channels already use the plain filter.
        // See the PR body for the hardening rationale.
        let scoped::ScopedAssembled {
            registry,
            delegate_handle: _,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            mut deferred_section,
            activated_handle: activated_handle_pm,
        } = scoped::ScopedToolRegistry::assemble(scoped::ScopedAssembly {
            config: &config,
            agent_alias,
            security: &security,
            built: all_tools_result_pm,
            skills: &skills,
            runtime: runtime.clone(),
            caller_allowed: None,
            connect_mcp: true,
            connect_peripherals: true,
            exclude_memory: false,
            emit_assembly_logs: true,
        })
        .await;
        let tools_registry = registry.into_inner();

        // Populate all channel-driven tool handles from the registered factory.
        let count = seed_channel_handles(
            &ask_user_handle,
            &channel_room_handle,
            &reaction_handle,
            &poll_handle,
            &escalate_handle,
        );
        if count > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Register)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_attrs(::serde_json::json!({"count": count})),
                &format!("Registered {} channel(s) for process_message agent", count),
            );
        }

        let model_name = match agent_model_provider
            .as_ref()
            .and_then(|e| e.model.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(m) => m.to_string(),
            None => anyhow::bail!(
                "agents.{agent_alias}.model_provider resolves to a model_provider entry with no \
             `model` set. Configure [providers.models.{provider_name}.<alias>] model = \"...\"."
            ),
        };
        let provider_runtime_options = zeroclaw_providers::provider_runtime_options_for_alias(
            &config,
            provider_name,
            provider_alias.as_str(),
        );
        let model_provider: Box<dyn ModelProvider> =
            zeroclaw_providers::create_routed_model_provider_with_options(
                &config,
                &format!("{provider_name}.{provider_alias}"),
                agent_model_provider
                    .as_ref()
                    .and_then(|e| e.api_key.as_deref()),
                agent_model_provider.as_ref().and_then(|e| e.uri.as_deref()),
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
            .map(|dir| crate::rag::HardwareRag::load(&config.data_dir, dir.trim()))
            .and_then(Result::ok)
            .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
        let board_names: Vec<String> = config
            .peripherals
            .boards
            .iter()
            .map(|b| b.board.clone())
            .collect();

        // ── Initialize locale-aware tool descriptions ──────────────────
        let i18n_locale = config
            .locale
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(crate::i18n::detect_locale);
        crate::i18n::init(&i18n_locale);

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
        tool_descs.push((
            "channel_room",
            "Create channel rooms and invite users through active channels.",
        ));
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

        // ── Compute final effective tool set BEFORE prompt construction ──
        // This ensures the system prompt, tool instructions, and channel target
        // injection all reflect the same policy-filtered tool set that will be
        // used at execution time. Without this, the prompt could advertise
        // tools (and their target identifiers) that the execution denylist
        // would block — a control boundary violation.
        //
        // We strip the leading `/think:<level>` directive before filtering
        // so the prompt-construction and request-execution paths see the
        // same user-message shape. Otherwise a `tool_filter_groups` dynamic
        // keyword that happens to appear inside `/think:high` (or the
        // directive token itself — `"think"`, `"high"`, `"max"`, …) would
        // make the prompt advertise tools the request then excludes, or
        // vice versa. Issue #8054 Surface 4.
        let effective_message_for_filter =
            crate::agent::thinking::strip_thinking_directive(message);
        let mut excluded_tools = compute_excluded_mcp_tools(
            &tools_registry,
            &agent.resolved.tool_filter_groups,
            effective_message_for_filter.as_ref(),
        );
        {
            let active_profile = &risk_profile;
            if active_profile.level != AutonomyLevel::Full {
                excluded_tools.extend(active_profile.excluded_tools.iter().cloned());
            }
        }

        // Filter tool descriptions to match the effective set.
        tool_descs.retain(|(name, _)| !excluded_tools.iter().any(|ex| ex == name));

        // Derive effective tool names from the filtered set so prompt builders
        // and channel target guards see the correct state.
        let effective_tool_names: HashSet<&str> = tools_registry
            .iter()
            .map(|tool| tool.name())
            .filter(|name| !excluded_tools.iter().any(|ex| ex == *name))
            .collect();
        tool_descs.retain(|(name, _)| effective_tool_names.contains(name));

        let bootstrap_max_chars = if eff_compact_context {
            Some(6000)
        } else {
            None
        };
        let native_tools = model_provider.supports_native_tools();
        let native_tool_specs_present = native_tool_specs_present_for_turn(
            model_provider.as_ref(),
            &tools_registry,
            &excluded_tools,
            activated_handle_pm.as_ref(),
        )?;
        let expose_text_tool_protocol = apply_text_tool_prompt_policy(
            native_tools,
            agent.resolved.strict_tool_parsing,
            &mut tool_descs,
            &mut deferred_section,
        );
        let agent_workspace = config.agent_workspace_dir(agent_alias);
        let mut system_prompt =
            crate::agent::system_prompt::build_system_prompt_with_mode_and_autonomy(
                &agent_workspace,
                &model_name,
                &tool_descs,
                &skills,
                Some(&agent.identity),
                bootstrap_max_chars,
                Some(&risk_profile),
                native_tool_specs_present,
                config.skills.prompt_injection_mode,
                eff_compact_context,
                eff_max_system_prompt_chars,
                false,
                config.channels.show_tool_calls,
            );
        if expose_text_tool_protocol {
            system_prompt.push_str(&build_tool_instructions_for_names(
                &tools_registry,
                &effective_tool_names,
            ));
        }
        if !deferred_section.is_empty() {
            system_prompt.push('\n');
            system_prompt.push_str(&deferred_section);
        }

        // ── Parse thinking directive from user message ─────────────
        let (thinking_directive, effective_message) =
            match crate::agent::thinking::parse_thinking_directive(message) {
                Some((level, remaining)) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_attrs(::serde_json::json!({"thinking_level": level})),
                        "Thinking directive parsed from message"
                    );
                    (Some(level), remaining)
                }
                None => (None, message.to_string()),
            };
        let thinking_level = crate::agent::thinking::resolve_thinking_level(
            thinking_directive,
            None,
            &agent.resolved.thinking,
        );
        let thinking_params = crate::agent::thinking::apply_thinking_level_with_config(
            thinking_level,
            &agent.resolved.thinking,
        );
        let effective_temperature: Option<f64> = agent_model_provider
            .as_ref()
            .and_then(|e| e.temperature)
            .map(|t| {
                crate::agent::thinking::clamp_temperature(
                    t + thinking_params.temperature_adjustment,
                )
            });

        // Prepend thinking system prompt prefix when present.
        if let Some(ref prefix) = thinking_params.system_prompt_prefix {
            system_prompt = format!("{prefix}\n\n{system_prompt}");
        }

        let effective_msg_ref = effective_message.as_str();
        let runtime_capability_names: Vec<&str> = effective_tool_names.iter().copied().collect();
        if let Some(suggestion) = crate::skills::render_missing_skill_install_suggestion(
            effective_msg_ref,
            &skills,
            &runtime_capability_names,
            &config.data_dir,
            &config.skills.extra_registries,
            config.skills.install_suggestions.enabled,
        ) {
            return Ok(suggestion);
        }

        // process_message is the channel entrypoint (Discord, Telegram, gateway,
        // etc.) — recall is scoped to the channel's session_id, so retrieving the
        // user's own Conversation history within their session is intended.
        let mem_context = build_context(
            mem.as_ref(),
            &*observer,
            effective_msg_ref,
            config.memory.min_relevance_score,
            session_id,
            false,
        )
        .await;
        let rag_limit = if eff_compact_context { 2 } else { 5 };
        let hw_context = hardware_rag
            .as_ref()
            .map(|r| {
                build_hardware_context(r, &*observer, effective_msg_ref, &board_names, rag_limit)
            })
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
            &agent.resolved.tool_filter_groups,
            effective_msg_ref,
        );
        {
            let active_profile = &risk_profile;
            if active_profile.level != AutonomyLevel::Full {
                excluded_tools.extend(active_profile.excluded_tools.iter().cloned());
            }
        }

        // ── Cross-channel HITL on the channel-less path ───────────────────
        // process_message (gateway chat/webhook dispatch, agent-to-agent peer
        // messages) runs with no originating channel, so a gated tool can only
        // reach a human through the profile's `approval_route`. When one is set
        // and a live channel registry is available, hand the turn a route-only
        // approval bridge: it asks the named approver alone, bounded by
        // `timeout_secs` and fail-closed by default. Absent a route (or with no
        // live channels) this stays `None` and the gate keeps today's
        // non-interactive auto-deny.
        let routed_approval_channel = risk_profile.approval_route.as_ref().and_then(|route| {
            live_channel_registry().map(|handles| {
                crate::agent::agent::RoutedApprovalChannel::new(handles, route.clone())
            })
        });
        let routed_approval_channel_ref = routed_approval_channel
            .as_ref()
            .map(|c| c as &dyn zeroclaw_api::channel::Channel);

        zeroclaw_api::NATIVE_THINKING_OVERRIDE
            .scope(
                thinking_params.native_thinking,
                agent_turn(
                    model_provider.as_ref(),
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
                    agent.resolved.max_tool_iterations,
                    Some(&approval_manager),
                    &excluded_tools,
                    &agent.resolved.tool_call_dedup_exempt,
                    activated_handle_pm.as_ref(),
                    None,
                    agent.resolved.strict_tool_parsing,
                    agent.resolved.parallel_tools,
                    agent.resolved.max_tool_result_chars,
                    agent.resolved.max_context_tokens,
                    // Cross-channel HITL: a route-only approval bridge when the
                    // profile sets `approval_route` and channels are live, else
                    // `None` (today's channel-less auto-deny). See above.
                    routed_approval_channel_ref,
                ),
            )
            .await
    };
    __zc_body
        .instrument(__zc_scope_span)
        .instrument(__zc_attribution_span)
        .await
}

#[cfg(test)]
mod tests {
    use super::{
        apply_text_tool_prompt_policy, build_context, estimate_history_tokens,
        load_interactive_session_history, make_query_summary,
        maybe_inject_channel_delivery_defaults, save_interactive_session_history,
        seed_channel_handles, truncate_tool_result,
    };
    use crate::agent::history::{DEFAULT_MAX_HISTORY_MESSAGES, InteractiveSessionState};
    use crate::agent::tool_execution::{ToolDispatchContext, execute_one_tool};
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::tempdir;
    use zeroclaw_api::channel::{
        Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
    };
    use zeroclaw_providers::{ChatMessage, ToolCall};
    use zeroclaw_tool_call_parser::parse_tool_calls;

    fn extract_sop_started_run_id(content: &str) -> Option<String> {
        content
            .split("SOP run started: ")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .map(str::to_string)
    }

    fn sop_started_run_id_from_history(history: &[ChatMessage]) -> Option<String> {
        history.iter().find_map(|msg| {
            if let Some(content) = msg.content.strip_prefix("[Tool results]\n")
                && let Some(run_id) = extract_sop_started_run_id(content)
            {
                return Some(run_id);
            }

            if msg.role == "tool"
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content)
                && let Some(content) = value.get("content").and_then(|content| content.as_str())
            {
                return extract_sop_started_run_id(content);
            }

            extract_sop_started_run_id(&msg.content)
        })
    }

    zeroclaw_api::mock_tool_attribution!(
        CountingTool,
        CredentialOutputTool,
        EmptySuccessTool,
        RecordingArgsTool,
        DelayTool,
        FailingTool,
        NamedMockTool,
        CompletesAndSignalsTool,
        CancelsTurnTool,
        VerboseTool,
    );

    struct SeedMockChannel;

    impl ::zeroclaw_api::attribution::Attributable for SeedMockChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Matrix,
            )
        }

        fn alias(&self) -> &str {
            "default"
        }
    }

    #[async_trait::async_trait]
    impl Channel for SeedMockChannel {
        fn name(&self) -> &str {
            "matrix"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    // ── seed_channel_handles tests ───────────────────────────────

    #[test]
    fn seed_channel_handles_populates_channel_room_handle() {
        let channel = Arc::new(SeedMockChannel) as Arc<dyn Channel>;
        super::register_channel_map_fn(Box::new(move || {
            let mut map = HashMap::new();
            map.insert("matrix.default".to_string(), Arc::clone(&channel));
            map
        }));

        let ask_user_handle = Arc::new(RwLock::new(HashMap::new()));
        let channel_room_handle = Arc::new(RwLock::new(HashMap::new()));
        let reaction = Arc::new(RwLock::new(HashMap::new()));
        let poll_handle = Arc::new(RwLock::new(HashMap::new()));
        let escalate_handle = Arc::new(RwLock::new(HashMap::new()));

        let count = seed_channel_handles(
            &Some(Arc::clone(&ask_user_handle)),
            &Some(Arc::clone(&channel_room_handle)),
            &reaction,
            &Some(Arc::clone(&poll_handle)),
            &Some(Arc::clone(&escalate_handle)),
        );

        assert_eq!(count, 1);
        assert!(ask_user_handle.read().contains_key("matrix.default"));
        assert!(channel_room_handle.read().contains_key("matrix.default"));
        assert!(reaction.read().contains_key("matrix.default"));
        assert!(poll_handle.read().contains_key("matrix.default"));
        assert!(escalate_handle.read().contains_key("matrix.default"));
    }

    // ── maybe_inject_channel_delivery_defaults tests ──────────────

    #[test]
    fn cron_delivery_defaults_include_dingtalk_channel() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "prompt": "remind me later",
            "schedule": { "kind": "every", "every_ms": 60000 }
        });

        maybe_inject_channel_delivery_defaults("cron_add", &mut args, "dingtalk", Some("chat-42"));

        assert_eq!(
            args["delivery"],
            serde_json::json!({
                "mode": "announce",
                "channel": "dingtalk",
                "to": "chat-42",
            })
        );
    }

    #[test]
    fn cron_delivery_defaults_do_not_guess_webhook_shape() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "prompt": "remind me later",
            "schedule": { "kind": "every", "every_ms": 60000 }
        });

        maybe_inject_channel_delivery_defaults("cron_add", &mut args, "webhook", Some("thread-42"));

        assert!(
            args.get("delivery").is_none(),
            "webhook delivery needs sender/thread context and must not reuse reply_target as to"
        );
    }

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

    #[test]
    fn load_interactive_session_merges_non_leading_system_messages() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let payload = serde_json::to_string_pretty(&InteractiveSessionState {
            version: 1,
            history: vec![
                ChatMessage::system("base system"),
                ChatMessage::user("first question"),
                ChatMessage::assistant("first answer"),
                ChatMessage::system("late loop-detection guidance"),
                ChatMessage::user("follow-up"),
            ],
        })
        .unwrap();
        std::fs::write(&path, payload).unwrap();

        let restored = load_interactive_session_history(&path, "fallback").unwrap();

        assert_eq!(
            restored
                .iter()
                .filter(|message| message.role == "system")
                .count(),
            1,
            "loaded session must not contain non-leading system messages: {:?}",
            restored
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(restored[0].role, "system");
        assert!(restored[0].content.contains("base system"));
        assert!(restored[0].content.contains("late loop-detection guidance"));
        assert_eq!(
            restored
                .iter()
                .map(|message| message.role.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "user", "assistant", "user"]
        );
    }

    #[test]
    fn load_interactive_session_replaces_empty_system_messages_with_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let payload = serde_json::to_string_pretty(&InteractiveSessionState {
            version: 1,
            history: vec![
                ChatMessage::system(""),
                ChatMessage::user("follow-up"),
                ChatMessage::system(""),
            ],
        })
        .unwrap();
        std::fs::write(&path, payload).unwrap();

        let restored = load_interactive_session_history(&path, "fallback system").unwrap();

        assert_eq!(
            restored
                .iter()
                .map(|message| (message.role.as_str(), message.content.as_str()))
                .collect::<Vec<_>>(),
            vec![("system", "fallback system"), ("user", "follow-up")]
        );
    }

    /// Regression test for issue #5813: a persisted session whose assistant
    /// (tool_use) was lost to compaction must self-heal on load so the next
    /// API call doesn't fail with "unexpected tool_use_id found in tool_result
    /// blocks".
    #[test]
    fn load_interactive_session_heals_orphaned_tool_result() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let orphan_tool = ChatMessage::tool(
            r#"{"tool_call_id":"toolu_01OrphanFromCompaction","content":"stale result"}"#,
        );
        let payload = serde_json::to_string_pretty(&InteractiveSessionState {
            version: 1,
            history: vec![
                ChatMessage::system("sys"),
                orphan_tool,
                ChatMessage::user("next question"),
            ],
        })
        .unwrap();
        std::fs::write(&path, payload).unwrap();

        let restored = load_interactive_session_history(&path, "fallback").unwrap();

        assert!(
            !restored.iter().any(|m| m.role == "tool"),
            "orphaned tool_result should be removed on load; got roles {:?}",
            restored.iter().map(|m| &m.role).collect::<Vec<_>>()
        );
    }

    use super::*;
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        let meta = crate::agent::turn::context::TurnMeta {
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let result = execute_one_tool(
            "unknown_tool",
            call_arguments,
            None,
            ToolDispatchContext {
                tools_registry: &[],
                activated_tools: None,
                excluded_tools: &[],
            },
            &meta,
            &observer,
            None,
            None,
            None,
        )
        .await;
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

        let meta = crate::agent::turn::context::TurnMeta {
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let outcome = execute_one_tool(
            "extract_text",
            serde_json::json!({ "value": "ok" }),
            None,
            ToolDispatchContext {
                tools_registry: &[],
                activated_tools: Some(&activated),
                excluded_tools: &[],
            },
            &meta,
            &observer,
            None,
            None, // receipt_generator
            None, // event_tx
        )
        .await
        .expect("suffix alias should execute the unique activated tool");

        assert!(outcome.success);
        assert_eq!(outcome.output, "counted:ok");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn execute_one_tool_recovers_poisoned_activated_tool_lock() {
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
        let poisoned = Arc::clone(&activated);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().expect("test mutex should lock");
            panic!("poison activated-tools lock");
        })
        .join();

        let meta = crate::agent::turn::context::TurnMeta {
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let outcome = execute_one_tool(
            "extract_text",
            serde_json::json!({ "value": "ok" }),
            None,
            ToolDispatchContext {
                tools_registry: &[],
                activated_tools: Some(&activated),
                excluded_tools: &[],
            },
            &meta,
            &observer,
            None,
            None,
            None,
        )
        .await
        .expect("poisoned activated-tools lock should recover for read");

        assert!(outcome.success);
        assert_eq!(outcome.output, "counted:ok");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn execute_one_tool_normalizes_empty_success_output() {
        let observer = NoopObserver;
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EmptySuccessTool)];

        let meta = crate::agent::turn::context::TurnMeta {
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let outcome = execute_one_tool(
            "empty_success",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
            },
            &meta,
            &observer,
            None,
            None, // receipt_generator
            None, // event_tx
        )
        .await
        .expect("empty successful tool output should still execute");

        assert!(outcome.success);
        assert_eq!(outcome.output, "(no output)");
        assert!(outcome.error_reason.is_none());
    }

    #[tokio::test]
    async fn execute_one_tool_keeps_data_path_raw_and_scrubs_only_observer() {
        struct CapturingResults {
            results: std::sync::Mutex<Vec<Option<String>>>,
        }
        impl crate::observability::Observer for CapturingResults {
            fn record_event(&self, event: &crate::observability::ObserverEvent) {
                if let crate::observability::ObserverEvent::ToolCall { result, .. } = event {
                    self.results.lock().unwrap().push(result.clone());
                }
            }
            fn record_metric(&self, _metric: &crate::observability::traits::ObserverMetric) {}
            fn name(&self) -> &str {
                "capturing-results"
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn flush(&self) {}
        }

        let observer = CapturingResults {
            results: std::sync::Mutex::new(Vec::new()),
        };
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(CredentialOutputTool)];

        let meta = crate::agent::turn::context::TurnMeta {
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let outcome = execute_one_tool(
            "credential_output",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
            },
            &meta,
            &observer,
            None,
            None, // receipt_generator
            None, // event_tx
        )
        .await
        .expect("tool should execute");

        // Data path (fed to the model and HMAC receipts) carries raw bytes.
        assert_eq!(outcome.output, "api_key = \"sk-live-abcd1234efgh5678\"");
        assert!(!outcome.output.contains("[REDACTED]"));

        // Observer (human-facing log/dashboard render) is scrubbed.
        let captured = observer.results.lock().unwrap();
        let result = captured
            .first()
            .and_then(|r| r.as_deref())
            .expect("observer must receive a tool result");
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("abcd1234efgh5678"));
    }
    use crate::observability::NoopObserver;
    use tempfile::TempDir;
    use zeroclaw_api::model_provider::{
        ProviderCapabilities, StreamChunk, StreamEvent, StreamOptions,
    };
    use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};
    use zeroclaw_providers::ChatResponse;
    use zeroclaw_providers::router::{Route, RouterModelProvider};

    macro_rules! impl_test_model_provider_attribution {
        ($ty:ty) => {
            impl ::zeroclaw_api::attribution::Attributable for $ty {
                fn role(&self) -> ::zeroclaw_api::attribution::Role {
                    ::zeroclaw_api::attribution::Role::Provider(
                        ::zeroclaw_api::attribution::ProviderKind::Model(
                            ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                        ),
                    )
                }

                fn alias(&self) -> &str {
                    stringify!($ty)
                }
            }
        };
    }

    struct NonVisionModelProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for NonVisionModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for NonVisionModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "NonVisionModelProvider"
        }
    }

    struct VisionModelProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for VisionModelProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: true,
                prompt_caching: false,
                extended_thinking: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
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
    impl ::zeroclaw_api::attribution::Attributable for VisionModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "VisionModelProvider"
        }
    }

    struct ScriptedModelProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        capabilities: ProviderCapabilities,
    }

    impl ScriptedModelProvider {
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

        /// Build a native-tool-calling provider: one turn of structured
        /// `tool_calls`, then a plain-text turn.
        fn from_native_tool_calls(calls: Vec<(&str, &str, &str)>, final_text: &str) -> Self {
            let tool_turn = ChatResponse {
                text: None,
                tool_calls: calls
                    .into_iter()
                    .map(|(id, name, args)| ToolCall {
                        id: id.to_string(),
                        name: name.to_string(),
                        arguments: args.to_string(),
                        extra_content: None,
                    })
                    .collect(),
                usage: None,
                reasoning_content: None,
            };
            let final_turn = ChatResponse {
                text: Some(final_text.to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            };
            let capabilities = ProviderCapabilities {
                native_tool_calling: true,
                ..ProviderCapabilities::default()
            };
            Self {
                responses: Arc::new(Mutex::new(VecDeque::from(vec![tool_turn, final_turn]))),
                capabilities,
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedModelProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in scripted model_provider tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::Error::msg("scripted model_provider exhausted responses"))
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ScriptedModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ScriptedModelProvider"
        }
    }

    struct RecordingModelProvider {
        requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
        capabilities: ProviderCapabilities,
    }

    impl RecordingModelProvider {
        fn new() -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                capabilities: ProviderCapabilities::default(),
            }
        }

        fn with_vision_support(mut self) -> Self {
            self.capabilities.vision = true;
            self
        }
    }

    #[async_trait]
    impl ModelProvider for RecordingModelProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in recording provider tests");
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.requests
                .lock()
                .expect("requests lock should be valid")
                .push(request.messages.to_vec());
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for RecordingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "RecordingModelProvider"
        }
    }

    struct StreamingScriptedModelProvider {
        responses: Arc<Mutex<VecDeque<String>>>,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingScriptedModelProvider {
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
    impl ModelProvider for StreamingScriptedModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming scripted model_provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
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
            _temperature: Option<f64>,
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
    impl ::zeroclaw_api::attribution::Attributable for StreamingScriptedModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamingScriptedModelProvider"
        }
    }

    enum NativeStreamTurn {
        ToolCall(ToolCall),
        Text(String),
        TextChunks(Vec<String>),
        /// Emit a single text delta with associated reasoning content. Used by
        /// regression tests for issue #6059 (DeepSeek V4 thinking-mode replay).
        TextWithReasoning {
            text: String,
            reasoning: String,
        },
        NarrationThenToolCall {
            narration_chunks: Vec<String>,
            tool_call: ToolCall,
        },
        ToolCallThenNarration {
            tool_call: ToolCall,
            narration_chunks: Vec<String>,
        },
    }

    struct StreamingNativeToolEventModelProvider {
        turns: Arc<Mutex<VecDeque<NativeStreamTurn>>>,
        stream_calls: Arc<AtomicUsize>,
        stream_tool_requests: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingNativeToolEventModelProvider {
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
    impl ModelProvider for StreamingNativeToolEventModelProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
                extended_thinking: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming native tool event model_provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
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
            _temperature: Option<f64>,
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
                NativeStreamTurn::TextChunks(chunks) => {
                    let mut events: Vec<_> = chunks
                        .into_iter()
                        .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text))))
                        .collect();
                    events.push(Ok(StreamEvent::Final));
                    Box::pin(futures_util::stream::iter(events))
                }
                NativeStreamTurn::TextWithReasoning { text, reasoning } => {
                    Box::pin(futures_util::stream::iter(vec![
                        Ok(StreamEvent::TextDelta(StreamChunk::reasoning(reasoning))),
                        Ok(StreamEvent::TextDelta(StreamChunk::delta(text))),
                        Ok(StreamEvent::Final),
                    ]))
                }
                NativeStreamTurn::NarrationThenToolCall {
                    narration_chunks,
                    tool_call,
                } => {
                    let mut events: Vec<_> = narration_chunks
                        .into_iter()
                        .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text))))
                        .collect();
                    events.push(Ok(StreamEvent::ToolCall(tool_call)));
                    events.push(Ok(StreamEvent::Final));
                    Box::pin(futures_util::stream::iter(events))
                }
                NativeStreamTurn::ToolCallThenNarration {
                    tool_call,
                    narration_chunks,
                } => {
                    let mut events: Vec<_> = vec![Ok(StreamEvent::ToolCall(tool_call))];
                    events.extend(
                        narration_chunks
                            .into_iter()
                            .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(text)))),
                    );
                    events.push(Ok(StreamEvent::Final));
                    Box::pin(futures_util::stream::iter(events))
                }
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for StreamingNativeToolEventModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamingNativeToolEventModelProvider"
        }
    }

    struct RouteAwareStreamingModelProvider {
        response: String,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
        last_model: Arc<Mutex<String>>,
    }

    impl RouteAwareStreamingModelProvider {
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
    impl ModelProvider for RouteAwareStreamingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in route-aware stream tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
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
            _temperature: Option<f64>,
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
    impl ::zeroclaw_api::attribution::Attributable for RouteAwareStreamingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "RouteAwareStreamingModelProvider"
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

    struct ApprovingChannel {
        approval_requests: Arc<AtomicUsize>,
    }

    impl ApprovingChannel {
        fn new(approval_requests: Arc<AtomicUsize>) -> Self {
            Self { approval_requests }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ApprovingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
            )
        }

        fn alias(&self) -> &str {
            "approving-test"
        }
    }

    #[async_trait]
    impl Channel for ApprovingChannel {
        fn name(&self) -> &str {
            "approving-test"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.approval_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ChannelApprovalResponse::Approve))
        }
    }

    struct CredentialOutputTool;

    #[async_trait]
    impl Tool for CredentialOutputTool {
        fn name(&self) -> &str {
            "credential_output"
        }

        fn description(&self) -> &str {
            "Returns success with a credential-shaped value in its output"
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
                success: true,
                output: "api_key = \"sk-live-abcd1234efgh5678\"".to_string(),
                error: None,
            })
        }
    }

    struct EmptySuccessTool;

    #[async_trait]
    impl Tool for EmptySuccessTool {
        fn name(&self) -> &str {
            "empty_success"
        }

        fn description(&self) -> &str {
            "Returns success with no stdout"
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
                success: true,
                output: String::new(),
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

    /// A **user-supplied** image on a non-vision provider with no configured
    /// `vision_model_provider` must surface a structured capability error
    /// (channels render it back to the user) — we never silently ignore an
    /// image the user actually sent.
    #[tokio::test]
    async fn run_tool_call_loop_returns_structured_error_for_non_vision_provider() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = NonVisionModelProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let err = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect_err("user image on a non-vision provider should error");

        assert!(err.to_string().contains("provider_capability_error"));
        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_skips_oversized_image_payload() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = RecordingModelProvider::new().with_vision_support();
        let recorded_requests = Arc::clone(&model_provider.requests);

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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("oversized payload should be skipped and continue as text-only");

        assert_eq!(result, "done");
        let requests = recorded_requests
            .lock()
            .expect("recorded requests lock should be valid");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].len(), 1);
        assert!(
            requests[0][0]
                .content
                .contains("1 attached image(s) could not be loaded")
        );
        assert!(!requests[0][0].content.contains("[IMAGE:"));
        assert!(!requests[0][0].content.contains(&oversized_payload));
    }

    /// Regression: a non-vision provider must not be permanently poisoned by an
    /// image marker left in history from an EARLIER turn. The capability error
    /// is scoped to the image the user *just* sent (see
    /// `run_tool_call_loop_returns_structured_error_for_non_vision_provider`);
    /// a carried-over marker degrades to text-only so the next plain-text turn
    /// succeeds instead of re-failing forever. Reproduces the reported bug where
    /// one image to a non-vision provider made every subsequent text turn fail
    /// (the RPC/streaming path persists the user message into the long-lived
    /// session history before the loop runs, so a failed image turn leaves its
    /// marker behind).
    #[tokio::test]
    async fn run_tool_call_loop_degrades_carried_over_image_on_non_vision_provider() {
        let model_provider = RecordingModelProvider::new();
        let recorded_requests = Arc::clone(&model_provider.requests);

        // An earlier image turn left its marker in history; the latest user
        // message is plain text.
        let mut history = vec![
            ChatMessage::user(
                "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
            ),
            ChatMessage::user("what is WAL?".to_string()),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;
        let turn_id = uuid::Uuid::new_v4().to_string();

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("a carried-over image must not fail a plain-text turn");

        assert_eq!(result, "done");

        // The provider was actually called (no hard capability error) and the
        // carried-over marker was stripped before reaching the text-only model.
        let requests = recorded_requests
            .lock()
            .expect("recorded requests lock should be valid");
        assert_eq!(requests.len(), 1, "exactly one provider call expected");
        let sent_blob = requests[0]
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !sent_blob.contains("[IMAGE:"),
            "carried-over image marker must be stripped, got: {sent_blob}"
        );
        assert!(
            sent_blob.contains("[media attachment]"),
            "stripped marker should become the text placeholder, got: {sent_blob}"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_valid_multimodal_request_flow() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = VisionModelProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "Analyze this [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("valid multimodal payload should pass");

        assert_eq!(result, "vision-ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A **tool-result** image marker (e.g. from `image_info`/`screenshot`)
    /// on a non-vision provider with no `vision_model_provider` must NOT abort
    /// the turn. The user did not send an image, so the loop degrades
    /// gracefully: markers are stripped and the text-only provider is still
    /// called so the conversation continues (and any accompanying
    /// text/metadata survives).
    #[tokio::test]
    async fn run_tool_call_loop_degrades_tool_result_image_for_non_vision_provider() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = NonVisionModelProvider {
            calls: Arc::clone(&calls),
        };

        // Marker lives in a tool result, not a user message.
        let mut history = vec![
            ChatMessage::user("inspect the screenshot".to_string()),
            ChatMessage::tool(
                "File: /tmp/x.png\n[IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
            ),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("text-only fallback should succeed, not abort the turn");

        // Provider was invoked (no hard capability error) and returned text.
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// When `vision_model_provider` is set but the model_provider factory cannot resolve
    /// the name, a descriptive error should be returned (not the generic
    /// capability error).
    #[tokio::test]
    async fn run_tool_call_loop_vision_provider_creation_failure() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = NonVisionModelProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("some-model".to_string()),
            ..Default::default()
        };

        let err = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect_err("should fail when vision model_provider cannot be created");

        assert!(
            err.to_string()
                .contains("failed to create vision model_provider"),
            "expected creation failure error, got: {}",
            err
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// Messages without image markers should use the default model_provider even
    /// when `vision_model_provider` is configured.
    #[tokio::test]
    async fn run_tool_call_loop_no_images_uses_default_provider() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec!["hello world"]);

        let mut history = vec![ChatMessage::user("just text, no images".to_string())];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("some-model".to_string()),
            ..Default::default()
        };

        // Even though vision_model_provider points to a nonexistent model_provider, this
        // should succeed because there are no image markers to trigger routing.
        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "scripted",
                    model: "scripted-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("text-only messages should succeed with default model_provider");

        assert_eq!(result, "hello world");
    }

    /// Behavior-identical guard (RFC #6971 phase 1): the always-on ingress
    /// policy layer must not change a turn's output. A plain turn with the
    /// `IngressContext::internal()` envelope, and the same turn with a fully
    /// external/untrusted channel envelope, both disposition to `Loop` under
    /// the default policy and must produce the identical final answer the
    /// engine produced before the layer existed.
    #[tokio::test]
    async fn run_tool_call_loop_ingress_default_loop_is_behavior_identical() {
        async fn run_with(ctx: IngressContext) -> String {
            let model_provider =
                ScriptedModelProvider::from_text_responses(vec!["identical answer"]);
            let mut history = vec![ChatMessage::user("hello".to_string())];
            let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
            let observer = NoopObserver;
            let turn_id = uuid::Uuid::new_v4().to_string();

            run_tool_call_loop(ToolLoop {
                exec: ResolvedAgentExecution {
                    model_access: ResolvedModelAccess {
                        model_provider: &model_provider,
                        provider_name: "scripted",
                        model: "scripted-model",
                        temperature: Some(0.0),
                    },
                    tools_registry: &tools_registry,
                    observer: &observer,
                    silent: true,
                    approval: None,
                    multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                    max_tool_iterations: 3,
                    hooks: None,
                    excluded_tools: &[],
                    dedup_exempt_tools: &[],
                    activated_tools: None,
                    model_switch_callback: None,
                    pacing: &zeroclaw_config::schema::PacingConfig::default(),
                    strict_tool_parsing: false,
                    parallel_tools: false,
                    max_tool_result_chars: 0,
                    context_token_budget: 0,
                    receipt_generator: None,
                    knobs: &LoopKnobs::default(),
                },
                history: &mut history,
                channel_name: "cli",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                shared_budget: None,
                channel: None,
                collected_receipts: None,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                ingress: ctx,
                agent_alias: None,
                turn_id: &turn_id,
            })
            .await
            .expect("default-Loop ingress must run the turn exactly as today")
        }

        let internal = run_with(IngressContext::internal()).await;
        let external = run_with(IngressContext {
            message_id: Some("ghc_9001".to_string()),
            source_class: zeroclaw_api::ingress::SourceClass::External,
            sender: Some("attacker".to_string()),
            transport: zeroclaw_api::ingress::Transport::Channel {
                kind: "github".to_string(),
                alias: "gh".to_string(),
            },
            trust: zeroclaw_api::ingress::TrustClass::Untrusted,
        })
        .await;

        assert_eq!(internal, "identical answer");
        assert_eq!(
            internal, external,
            "default-Loop policy must produce identical output regardless of envelope"
        );
    }

    /// When `vision_model_provider` is set but `vision_model` is not, the default
    /// model should be used as fallback for the vision model_provider.
    #[tokio::test]
    async fn run_tool_call_loop_vision_provider_without_model_falls_back() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = NonVisionModelProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "look [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        // vision_model_provider set but vision_model is None — the code should
        // fall back to the default model. Since the model_provider name is invalid,
        // we just verify the error path references the correct model_provider.
        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: None,
            ..Default::default()
        };

        let err = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect_err("should fail due to nonexistent vision model_provider");

        // Verify the routing was attempted (not the generic capability error).
        assert!(
            err.to_string()
                .contains("failed to create vision model_provider"),
            "expected creation failure, got: {}",
            err
        );
    }

    /// Empty `[IMAGE:]` markers (which are preserved as literal text by the
    /// parser) should not trigger vision model_provider routing.
    #[tokio::test]
    async fn run_tool_call_loop_empty_image_markers_use_default_provider() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec!["handled"]);

        let mut history = vec![ChatMessage::user(
            "empty marker [IMAGE:] should be ignored".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
            ..Default::default()
        };

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "scripted",
                    model: "scripted-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("empty image markers should not trigger vision routing");

        assert_eq!(result, "handled");
    }

    /// Multiple image markers should still trigger vision routing when
    /// vision_model_provider is configured.
    #[tokio::test]
    async fn run_tool_call_loop_multiple_images_trigger_vision_routing() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = NonVisionModelProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "two images [IMAGE:data:image/png;base64,aQ==] and [IMAGE:data:image/png;base64,bQ==]"
                .to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("llava:7b".to_string()),
            ..Default::default()
        };

        let err = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &multimodal,
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect_err("should attempt vision model_provider creation for multiple images");

        assert!(
            err.to_string()
                .contains("failed to create vision model_provider"),
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
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig::default();
        let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

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
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
            level: crate::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

        assert!(should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_multiple_tools_with_ordered_results() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
            level: crate::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
    async fn run_tool_call_loop_executes_queued_sop_steps_after_sop_execute() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"sop_execute","arguments":{"name":"live-sop"}}
</tool_call>"#,
            "step one done",
            "step two done",
            "outer done",
        ]);

        let sop = crate::sop::Sop {
            name: "live-sop".to_string(),
            description: "live sop".to_string(),
            version: "1".to_string(),
            priority: crate::sop::SopPriority::Normal,
            execution_mode: crate::sop::SopExecutionMode::Auto,
            triggers: vec![crate::sop::SopTrigger::Manual],
            steps: vec![
                crate::sop::SopStep {
                    number: 1,
                    title: "First".to_string(),
                    body: "Do the first step".to_string(),
                    suggested_tools: Vec::new(),
                    requires_confirmation: false,
                    kind: crate::sop::SopStepKind::default(),
                    schema: None,
                    ..crate::sop::SopStep::default()
                },
                crate::sop::SopStep {
                    number: 2,
                    title: "Second".to_string(),
                    body: "Do the second step".to_string(),
                    suggested_tools: Vec::new(),
                    requires_confirmation: false,
                    kind: crate::sop::SopStepKind::default(),
                    schema: None,
                    ..crate::sop::SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        };
        let mut engine = crate::sop::SopEngine::new(zeroclaw_config::schema::SopConfig::default());
        engine.replace_sops_for_test(vec![sop]);
        let engine = Arc::new(Mutex::new(engine));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(crate::tools::SopExecuteTool::new(
            Arc::clone(&engine),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("start the live sop"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 6,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "agent",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: Some("test-agent"),
            turn_id: &turn_id,
        })
        .await
        .expect("live SOP execution should complete");

        assert_eq!(result, "outer done");
        let started = sop_started_run_id_from_history(&history)
            .expect("sop_execute tool result should include a run id");
        let engine = engine.lock().unwrap();
        let run = engine
            .get_run(&started)
            .expect("run should remain queryable after completion");
        assert_eq!(run.status, crate::sop::SopRunStatus::Completed);
        assert_eq!(run.step_results.len(), 2);
        assert_eq!(run.step_results[0].output, "step one done");
        assert_eq!(run.step_results[1].output, "step two done");
    }

    #[tokio::test]
    async fn run_tool_call_loop_enforces_sop_step_tool_scope() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider {
            responses: Arc::new(Mutex::new(VecDeque::from(vec![
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "outer-sop".to_string(),
                        name: "sop_execute".to_string(),
                        arguments: r#"{"name":"scoped-sop"}"#.to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                },
                ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "step-denied".to_string(),
                        name: "denied_tool".to_string(),
                        arguments: r#"{"value":"blocked"}"#.to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                },
                ChatResponse {
                    text: Some("step recovered".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                },
                ChatResponse {
                    text: Some("outer done".to_string()),
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

        let sop = crate::sop::Sop {
            name: "scoped-sop".to_string(),
            description: "scoped sop".to_string(),
            version: "1".to_string(),
            priority: crate::sop::SopPriority::Normal,
            execution_mode: crate::sop::SopExecutionMode::Auto,
            triggers: vec![crate::sop::SopTrigger::Manual],
            steps: vec![crate::sop::SopStep {
                number: 1,
                title: "Scoped".to_string(),
                body: "Use only allowed tools".to_string(),
                scope: Some(crate::sop::StepToolScope {
                    allow: Some(vec!["allowed_tool".to_string()]),
                    deny: Vec::new(),
                }),
                ..crate::sop::SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        };
        let mut engine = crate::sop::SopEngine::new(zeroclaw_config::schema::SopConfig {
            step_scope_enforce: true,
            ..zeroclaw_config::schema::SopConfig::default()
        });
        engine.replace_sops_for_test(vec![sop]);
        let engine = Arc::new(Mutex::new(engine));

        let allowed_invocations = Arc::new(AtomicUsize::new(0));
        let denied_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(crate::tools::SopExecuteTool::new(Arc::clone(&engine))),
            Box::new(CountingTool::new(
                "allowed_tool",
                Arc::clone(&allowed_invocations),
            )),
            Box::new(CountingTool::new(
                "denied_tool",
                Arc::clone(&denied_invocations),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("start the scoped sop"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 6,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "agent",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: Some("test-agent"),
            turn_id: &turn_id,
        })
        .await
        .expect("scoped SOP execution should complete");

        assert_eq!(result, "outer done");
        assert_eq!(allowed_invocations.load(Ordering::SeqCst), 0);
        assert_eq!(denied_invocations.load(Ordering::SeqCst), 0);
        assert!(
            history.iter().any(|msg| msg
                .content
                .contains("Tool not available in this turn: denied_tool")),
            "denied tool call should be recorded as unavailable in history: {history:?}"
        );

        let started = sop_started_run_id_from_history(&history)
            .expect("sop_execute tool result should include a run id");
        let engine = engine.lock().unwrap();
        let run = engine
            .get_run(&started)
            .expect("run should remain queryable after completion");
        assert_eq!(run.status, crate::sop::SopRunStatus::Completed);
        assert_eq!(run.step_results.len(), 1);
        assert_eq!(run.step_results[0].output, "step recovered");
    }

    /// Regression: a native provider emitting multiple parallel tool calls
    /// in one turn must yield one role=tool message per call, each keyed to
    /// its own tool_call_id and output.
    #[tokio::test]
    async fn run_tool_call_loop_native_emits_tool_message_per_parallel_call() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_native_tool_calls(
            vec![
                ("call_a", "delay_a", r#"{"value":"A"}"#),
                ("call_b", "delay_b", r#"{"value":"B"}"#),
                ("call_c", "delay_c", r#"{"value":"C"}"#),
            ],
            "done",
        );

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(DelayTool::new(
                "delay_a",
                10,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
            Box::new(DelayTool::new(
                "delay_b",
                10,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
            Box::new(DelayTool::new(
                "delay_c",
                10,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
        ];

        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
            level: crate::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run three tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: true,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("native parallel execution should complete");

        assert!(result.ends_with("done"), "got: {result}");

        let tool_messages: Vec<&ChatMessage> =
            history.iter().filter(|msg| msg.role == "tool").collect();
        assert_eq!(
            tool_messages.len(),
            3,
            "every parallel native call must yield its own role=tool message, got: {tool_messages:?}"
        );

        for (id, value) in [("call_a", "A"), ("call_b", "B"), ("call_c", "C")] {
            let msg = tool_messages
                .iter()
                .find(|m| m.content.contains(id))
                .unwrap_or_else(|| panic!("missing tool message for {id}: {tool_messages:?}"));
            assert!(
                msg.content.contains(&format!("ok:{value}")),
                "tool_call_id {id} must carry its own output ok:{value}, got: {}",
                msg.content
            );
        }
    }

    struct CompletesAndSignalsTool {
        completed_tx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    #[async_trait]
    impl Tool for CompletesAndSignalsTool {
        fn name(&self) -> &str {
            "fast_tool"
        }
        fn description(&self) -> &str {
            "Completes immediately and signals a sibling to cancel the turn"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{}})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            if let Some(tx) = self.completed_tx.lock().await.take() {
                let _ = tx.send(());
            }
            Ok(crate::tools::ToolResult {
                success: true,
                output: "fast-done".to_string(),
                error: None,
            })
        }
    }

    struct CancelsTurnTool {
        token: CancellationToken,
        wait_for: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Tool for CancelsTurnTool {
        fn name(&self) -> &str {
            "cancel_tool"
        }
        fn description(&self) -> &str {
            "Waits for a sibling to finish, cancels the turn, then never returns"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{}})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            if let Some(rx) = self.wait_for.lock().await.take() {
                let _ = rx.await;
            }
            self.token.cancel();
            // The executor's select drops this future once the token fires; the
            // pending await keeps this call from ever returning Ok.
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    // A parallel sibling that finished before the cancellation already emitted
    // its real terminal ToolResult; the cancel cleanup must not emit a second,
    // interrupted result for that same tool_call_id (#7778 regression).
    #[tokio::test]
    async fn run_tool_call_loop_parallel_cancel_no_double_terminal_for_completed_call() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_native_tool_calls(
            vec![
                ("call_fast", "fast_tool", "{}"),
                ("call_cancel", "cancel_tool", "{}"),
            ],
            "done",
        );

        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let token = CancellationToken::new();
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(CompletesAndSignalsTool {
                completed_tx: tokio::sync::Mutex::new(Some(completed_tx)),
            }),
            Box::new(CancelsTurnTool {
                token: token.clone(),
                wait_for: tokio::sync::Mutex::new(Some(completed_rx)),
            }),
        ];

        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
            level: crate::security::AutonomyLevel::Full,
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run two tool calls"),
        ];
        let observer = NoopObserver;
        let (event_tx, mut event_rx) =
            tokio::sync::mpsc::channel::<zeroclaw_api::agent::TurnEvent>(64);

        let _ = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: true,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: Some(token.clone()),
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: Some(event_tx),
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await;

        drop(tools_registry);
        let mut results_by_id: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        while let Ok(event) = event_rx.try_recv() {
            if let zeroclaw_api::agent::TurnEvent::ToolResult { id, output, .. } = event {
                results_by_id.entry(id).or_default().push(output);
            }
        }

        let fast_results = results_by_id
            .get("call_fast")
            .map(Vec::as_slice)
            .unwrap_or_default();
        assert_eq!(
            fast_results.len(),
            1,
            "completed parallel call must get exactly one terminal ToolResult, got: {fast_results:?}"
        );
        assert_eq!(fast_results[0], "fast-done");

        let cancel_results = results_by_id
            .get("call_cancel")
            .map(Vec::as_slice)
            .unwrap_or_default();
        assert_eq!(
            cancel_results.len(),
            1,
            "cancelled-in-flight call must get exactly one interrupted ToolResult, got: {cancel_results:?}"
        );
        assert_eq!(
            cancel_results[0],
            crate::i18n::get_required_cli_string("turn-tool-interrupted-before-result")
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_injects_channel_delivery_defaults_for_cron_add() {
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let turn_id = uuid::Uuid::new_v4().to_string();

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: Some("chat-42"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: Some("chat-42"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
    async fn run_tool_call_loop_injects_channel_delivery_defaults_for_lark() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "lark",
            channel_reply_target: Some("chat-99"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("lark cron_add delivery defaults should be injected");

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
                "channel": "lark",
                "to": "chat-99",
            })
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_injects_channel_delivery_defaults_for_feishu() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"feishu reminder","schedule":{"kind":"every","every_ms":60000}}}
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
            ChatMessage::user("schedule a feishu reminder"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "feishu",
            channel_reply_target: Some("chat-77"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("feishu cron_add delivery defaults should be injected");

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
                "channel": "feishu",
                "to": "chat-77",
            })
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_deduplicates_repeated_tool_calls() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        );

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
    async fn run_tool_call_loop_aborts_repeated_prompt_required_shell_before_reprompting() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let repeated_shell_call = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#;
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            repeated_shell_call,
            repeated_shell_call,
            "should not reach final response",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "shell",
            Arc::clone(&invocations),
        ))];

        let approval_requests = Arc::new(AtomicUsize::new(0));
        let channel = ApprovingChannel::new(Arc::clone(&approval_requests));
        let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("repeat shell"),
        ];
        let observer = NoopObserver;
        let knobs = LoopKnobs {
            dedup_enabled: false,
            ..LoopKnobs::default()
        };

        let err = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &knobs,
            },
            history: &mut history,
            channel_name: "acp",
            channel_reply_target: Some("operator"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: Some(&channel),
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect_err("identical prompt-required shell call should abort before another prompt");

        let err = err.to_string();
        assert!(
            err.contains("repeated prompt-required tool call 'shell'"),
            "unexpected error: {err}"
        );
        assert_eq!(
            approval_requests.load(Ordering::SeqCst),
            1,
            "the repeated shell call should not issue a second approval request"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "the repeated shell call should not execute a second time"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_skips_same_round_prompt_required_shell_duplicate_without_reprompting()
     {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "shell",
            Arc::clone(&invocations),
        ))];

        let approval_requests = Arc::new(AtomicUsize::new(0));
        let channel = ApprovingChannel::new(Arc::clone(&approval_requests));
        let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("repeat shell in one response"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "acp",
            channel_reply_target: Some("operator"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: Some(&channel),
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("same-round prompt-required duplicate should use duplicate result path");

        assert!(
            result.ends_with("done"),
            "result should end with 'done', got: {result}"
        );
        assert_eq!(
            approval_requests.load(Ordering::SeqCst),
            1,
            "same-round duplicate should not issue a second approval request"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "same-round duplicate should not execute a second time"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(tool_results.content.contains("counted:"));
        assert!(tool_results.content.contains("Skipped duplicate tool call"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_prompt_guard_ignores_same_round_dedup_exempt_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let repeated_shell_call = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#;
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            repeated_shell_call,
            repeated_shell_call,
            "should not reach final response",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "shell",
            Arc::clone(&invocations),
        ))];

        let approval_requests = Arc::new(AtomicUsize::new(0));
        let channel = ApprovingChannel::new(Arc::clone(&approval_requests));
        let approval_mgr = ApprovalManager::for_non_interactive_backchannel(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("repeat dedup-exempt shell"),
        ];
        let observer = NoopObserver;
        let dedup_exempt = vec!["shell".to_string()];

        let err = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval_mgr),
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &dedup_exempt,
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "acp",
            channel_reply_target: Some("operator"),
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: Some(&channel),
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect_err("dedup-exempt prompt-required repeats should still abort before reprompting");

        let err = err.to_string();
        assert!(
            err.contains("repeated prompt-required tool call 'shell'"),
            "unexpected error: {err}"
        );
        assert_eq!(
            approval_requests.load(Ordering::SeqCst),
            1,
            "dedup-exempt repeated shell should not issue a second approval request"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "dedup-exempt repeated shell should not execute a second time"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_allows_repeated_calls() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &exempt,
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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

    /// Identical-prompt calls to re-entrant agent tools (spawn_subagent /
    /// delegate) must both run even with no config exemption — fan-out is
    /// intentional, not a duplicate to collapse.
    #[tokio::test]
    async fn run_tool_call_loop_reentrant_agent_tools_are_dedup_exempt_by_default() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"spawn_subagent","arguments":{"prompt":"same"}}
</tool_call>
<tool_call>
{"name":"spawn_subagent","arguments":{"prompt":"same"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "spawn_subagent",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("fan out two identical subagents"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("loop should finish running both identical subagent calls");

        assert!(result.ends_with("done"), "got: {result}");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            2,
            "both identical spawn_subagent calls must execute"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_only_affects_listed_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let _result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &exempt,
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
    async fn run_tool_call_loop_retries_malformed_tool_protocol_without_leaking_json() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let provider = ScriptedModelProvider::from_text_responses(vec![
            r#"{"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
            "Recovered answer.",
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("malformed tool protocol should retry and recover");

        assert_eq!(result, "Recovered answer.");
        assert!(!result.contains("toolcalls"));
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "malformed alias payload should not execute as a tool call"
        );
        assert!(
            history
                .iter()
                .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
            "history should include internal parser feedback for the model"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_preserves_unknown_function_call_json_with_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let business_json =
            r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;
        let provider = ScriptedModelProvider::from_text_responses(vec![business_json]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a support case JSON object"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("business JSON should be returned as normal text");

        assert_eq!(result, business_json);
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "business JSON must not execute any runtime tool"
        );
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "business JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_preserves_malformed_unknown_tool_calls_json_with_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let business_json = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#;
        let provider = ScriptedModelProvider::from_text_responses(vec![business_json]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a partial support case JSON object"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("unknown business JSON should be returned as normal text");

        assert_eq!(result, business_json);
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "business JSON must not execute any runtime tool"
        );
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "business JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_after_repeated_malformed_tool_protocol() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let provider = ScriptedModelProvider::from_text_responses(vec![
            r#"{"toolcalls":[{"call_id":"call_1","arguments":{"value":"X"}}]}"#,
            r#"{"toolcalls":[{"call_id":"call_2","arguments":{"value":"Y"}}]}"#,
            r#"{"toolcalls":[{"call_id":"call_3","arguments":{"value":"Z"}}]}"#,
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 6,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("malformed tool protocol should return a safe fallback");

        assert_eq!(
            result,
            crate::i18n::get_required_cli_string("channel-runtime-malformed-tool-output")
        );
        assert!(!result.contains("toolcalls"));
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "malformed protocol should never be executed as a tool call"
        );
        let feedback_count = history
            .iter()
            .filter(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]"))
            .count();
        assert_eq!(feedback_count, MAX_MALFORMED_TOOL_PROTOCOL_RETRIES);
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_toolcalls_reference_json_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let reference_json = r#"{"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#;
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![reference_json]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a toolcalls reference JSON object"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("toolcalls reference JSON should remain visible without tools");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(result, reference_json);
        assert_eq!(visible_deltas, reference_json);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "toolcalls reference JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_toolcalls_reference_json_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let reference_json = r#"{"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#;
        let provider = ScriptedModelProvider::from_text_responses(vec![reference_json]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a toolcalls reference JSON object"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("toolcalls reference JSON should remain visible without tools");

        assert_eq!(result, reference_json);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "toolcalls reference JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_schema_json_array_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let schema = r#"[{"name":"planner","parameters":{"goal":"string"}}]"#;
        let provider = ScriptedModelProvider::from_text_responses(vec![schema]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a JSON schema array"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("schema JSON should remain visible without tools");

        assert_eq!(result, schema);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "plain schema JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_tool_calls_audit_json_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let audit_json =
            r#"{"tool_calls":[{"id":"case-1","status":"queued","service":"billing"}]}"#;
        let provider = ScriptedModelProvider::from_text_responses(vec![audit_json]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a tool call audit JSON object"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("audit JSON should remain visible without tools");

        assert_eq!(result, audit_json);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "business tool_calls JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_function_call_reference_json_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let reference_json =
            r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;
        let provider = ScriptedModelProvider::from_text_responses(vec![reference_json]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("return a function_call reference JSON object"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("reference JSON should remain visible without tools");

        assert_eq!(result, reference_json);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "reference function_call JSON must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_tool_call_tag_example_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let example = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
This is an example, not an invocation."#;
        let provider = ScriptedModelProvider::from_text_responses(vec![example]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("show a tool_call tag example"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("tool_call tag examples should remain visible without tools");

        assert_eq!(result, example);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "tool_call tag examples must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_tool_call_fenced_example_with_registered_tool() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let example = r#"```tool_call
{"name":"count_tool","arguments":{"value":"X"}}
```
This is an example, not an invocation."#;
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![example]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("show a registered tool_call fenced example"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("registered tool_call fenced examples should remain visible");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(result, example);
        assert_eq!(visible_deltas, example);
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "tool-call examples must not execute registered tools"
        );
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "tool-call examples must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_tool_call_tag_example_with_registered_tool() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let example = r#"<tool_call>
{"name":"count_tool","arguments":{"value":"X"}}
</tool_call>
This is an example, not an invocation."#;
        let provider = ScriptedModelProvider::from_text_responses(vec![example]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("show a registered tool_call tag example"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("registered tool_call tag examples should remain visible");

        assert_eq!(result, example);
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "tool-call tag examples must not execute registered tools"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_tagged_tool_call_with_trailing_text_without_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let leaked = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
Done."#;
        let provider =
            ScriptedModelProvider::from_text_responses(vec![leaked, "Recovered answer."]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run without tools"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("tagged tool protocol with trailing text should retry and recover");

        assert_eq!(result, "Recovered answer.");
        assert!(!result.contains("<tool_call>"));
        assert!(
            history
                .iter()
                .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
            "tagged tool protocol with trailing text must trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_embedded_fenced_tool_call_without_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let leaked = r#"Let me call it:
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
Done."#;
        let provider =
            ScriptedModelProvider::from_text_responses(vec![leaked, "Recovered answer."]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run without tools"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("embedded fenced tool protocol should retry and recover");

        assert_eq!(result, "Recovered answer.");
        assert!(!result.contains("```tool_call"));
        assert!(
            history
                .iter()
                .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
            "embedded fenced tool protocol must trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_malformed_tool_protocol_fenced_call_without_tools() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let leaked = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;
        let provider =
            ScriptedModelProvider::from_text_responses(vec![leaked, "Recovered answer."]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run without tools"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("standalone tool_call fence should retry and recover without tools");

        assert_eq!(result, "Recovered answer.");
        assert!(!result.contains("```tool_call"));
        assert!(
            history
                .iter()
                .any(|msg| msg.role == "user" && msg.content.contains("[Tool call parse error]")),
            "standalone tool_call fence must trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_tool_call_fenced_example_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![example]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("show a tool_call fenced example"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("tool_call fenced examples should remain visible without tools");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(result, example);
        assert_eq!(visible_deltas, example);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "tool_call fenced examples must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_split_tool_call_fenced_example_when_no_tools_are_enabled() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        struct SplitFencedExampleProvider;
        impl_test_model_provider_attribution!(SplitFencedExampleProvider);

        #[async_trait]
        impl ModelProvider for SplitFencedExampleProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("chat should not be called when streaming succeeds")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        "```tool_call\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n```",
                    ))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        "\nThis is an example, not an invocation.",
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;
        let provider = SplitFencedExampleProvider;
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("show a split tool_call fenced example"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("split tool_call fenced examples should remain visible without tools");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(result, example);
        assert_eq!(visible_deltas, example);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "split tool_call fenced examples must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_json_fenced_tool_protocol_example_when_no_tools_are_enabled()
     {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let example = r#"```json
{"tool_calls":[{"name":"shell","arguments":{"command":"pwd"}}]}
```
This is an example, not an invocation."#;
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![example]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("show a JSON tool_calls example"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("JSON-fenced tool protocol examples should remain visible without tools");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(result, example);
        assert_eq!(visible_deltas, example);
        assert!(
            history
                .iter()
                .all(|msg| !msg.content.contains("[Tool call parse error]")),
            "JSON-fenced tool protocol examples must not trigger internal parser feedback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_streamed_tool_call_fence_without_draft_leak() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![
            r#"```tool_call
{"name":"count_tool","arguments":{"value":"X"}}
```"#,
            "Final answer.",
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("use the tool"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "matrix",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("streamed fenced tool call should execute and continue");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(result, "Final answer.");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(visible_deltas, "Final answer.");
        assert!(
            !visible_deltas.contains("```tool_call"),
            "streamed fenced tool call must not reach draft updates before execution"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_sanitizes_native_tool_call_text_before_display_and_history() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider {
            responses: Arc::new(Mutex::new(VecDeque::from(vec![
                ChatResponse {
                    text: Some(
                        "<think>private chain of thought</think>Task started. Waiting 30 seconds before checking status."
                            .into(),
                    ),
                    tool_calls: vec![ToolCall {
                        id: "call_wait".into(),
                        name: "count_tool".into(),
                        arguments: r#"{"value":"A"}"#.into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: Some("provider reasoning".into()),
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
            "native assistant text should be sanitized and relayed to on_delta"
        );
        assert!(
            deltas
                .iter()
                .any(|delta| matches!(delta, StreamDelta::Status(t) if t.starts_with("\u{1f4ac} Got 1 tool call(s)"))),
            "tool-call progress line should still be relayed"
        );
        assert_eq!(
            result, "Final answer",
            "final delivered result should not include intermediate tool-call narration"
        );
        assert!(!result.contains("private chain of thought"));
        assert!(!result.contains("<think>"));
        assert!(
            deltas.iter().all(|delta| match delta {
                StreamDelta::Status(text) | StreamDelta::Text(text) =>
                    !text.contains("private chain of thought") && !text.contains("<think>"),
            }),
            "draft deltas must not expose inline think tags: {deltas:?}"
        );
        let assistant_tool_history = history
            .iter()
            .find(|message| message.content.contains("\"tool_calls\""))
            .expect("native tool-call turn should persist assistant history");
        let parsed: serde_json::Value =
            serde_json::from_str(&assistant_tool_history.content).unwrap();
        assert_eq!(
            parsed["content"].as_str(),
            Some("Task started. Waiting 30 seconds before checking status.")
        );
        assert_eq!(
            parsed["reasoning_content"].as_str(),
            Some("provider reasoning")
        );
        assert!(
            !assistant_tool_history
                .content
                .contains("private chain of thought")
        );
        assert!(!assistant_tool_history.content.contains("<think>"));
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_consumes_provider_stream_for_final_response() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider =
            StreamingScriptedModelProvider::from_text_responses(vec!["streamed final answer"]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("say hi"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("streaming model_provider should complete");

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
        assert_eq!(model_provider.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(model_provider.chat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_streaming_path_preserves_tool_loop_semantics() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = StreamingScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 5,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
        assert_eq!(model_provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(model_provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
        assert!(
            !visible_deltas.contains("<tool_call"),
            "draft text should not leak streamed tool payload markers"
        );
    }

    // Gemini 2.5 Flash emits text alongside its XML tool calls: a ``tool_code``
    // Python block + hallucinated result + premature prose in the same response
    // turn. None of that text should reach the user — only the final clean reply
    // (iteration with no tool calls) should be accumulated.
    #[tokio::test]
    async fn parsed_tool_call_iteration_text_is_suppressed() {
        let gemini_turn1 = concat!(
            "<tool_call>\n",
            "{\"name\":\"count_tool\",\"arguments\":{\"value\":\"A\"}}\n",
            "</tool_call>\n",
            "```tool_code\nprint(count_tool(value='A'))\n```\n",
            "```\n{\"result\": \"counted:ok\"}\n```\n",
            "I already have the answer, it is counted:ok.",
        );
        let provider = ScriptedModelProvider::from_text_responses(vec![gemini_turn1, "done"]);
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

        let turn_id = uuid::Uuid::new_v4().to_string();
        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 5,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: zeroclaw_api::ingress::IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("should complete");

        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "tool should run once"
        );
        assert_eq!(
            result, "done",
            "hallucinated text from tool-call iteration must be suppressed; got: {result:?}"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_buffers_split_tool_protocol_markers() {
        struct SplitToolProtocolProvider;
        impl_test_model_provider_attribution!(SplitToolProtocolProvider);

        #[async_trait]
        impl ModelProvider for SplitToolProtocolProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(r#"{"tool"#))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"calls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = SplitToolProtocolProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            Some(&[crate::tools::ToolSpec {
                name: "count_tool".to_string(),
                description: "Count values".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"toolcalls\""));
        assert_eq!(
            visible_deltas, "",
            "split internal protocol markers must not reach draft updates"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_buffers_top_level_tool_call_array() {
        struct TopLevelToolArrayProvider;
        impl_test_model_provider_attribution!(TopLevelToolArrayProvider);

        #[async_trait]
        impl ModelProvider for TopLevelToolArrayProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"[{"name":"count_tool","arguments":{"value":"X"}}]"#,
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = TopLevelToolArrayProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            Some(&[crate::tools::ToolSpec {
                name: "count_tool".to_string(),
                description: "Count values".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"name\""));
        assert_eq!(
            visible_deltas, "",
            "top-level tool-call arrays must not reach draft updates"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_preserves_schema_array_without_tools() {
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![
            r#"[{"name":"planner","parameters":{"goal":"string"}}]"#,
        ]);
        let messages = vec![ChatMessage::user("return a JSON schema array")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(
            outcome.response_text,
            r#"[{"name":"planner","parameters":{"goal":"string"}}]"#
        );
        assert_eq!(visible_deltas, outcome.response_text);
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_preserves_unknown_function_call_json_with_tools() {
        let response = r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![response]);
        let messages = vec![ChatMessage::user("return a support case JSON object")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            Some(&[crate::tools::ToolSpec {
                name: "count_tool".to_string(),
                description: "Count values".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(outcome.response_text, response);
        assert_eq!(visible_deltas, response);
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_preserves_malformed_unknown_tool_calls_json_with_tools()
     {
        let response = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#;
        let provider = StreamingScriptedModelProvider::from_text_responses(vec![response]);
        let messages = vec![ChatMessage::user(
            "return a partial support case JSON object",
        )];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            Some(&[crate::tools::ToolSpec {
                name: "count_tool".to_string(),
                description: "Count values".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(outcome.response_text, response);
        assert_eq!(visible_deltas, response);
        assert!(
            !outcome.suppressed_protocol,
            "unknown business JSON must not be suppressed as internal protocol"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_buffers_malformed_tool_protocol_json() {
        struct MalformedToolProtocolProvider;
        impl_test_model_provider_attribution!(MalformedToolProtocolProvider);

        #[async_trait]
        impl ModelProvider for MalformedToolProtocolProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(r#"{"tool_"#))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"calls":[{"call_id":"call_1","arguments":{"value":"X"}}]}"#,
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = MalformedToolProtocolProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"tool_calls\""));
        assert_eq!(
            visible_deltas, "",
            "malformed internal protocol JSON must not reach draft updates"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_drops_truncated_protocol_at_finish() {
        struct TruncatedProtocolProvider;
        impl_test_model_provider_attribution!(TruncatedProtocolProvider);

        #[async_trait]
        impl ModelProvider for TruncatedProtocolProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"{"tool_call_id":"call_1","content":"raw"#,
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = TruncatedProtocolProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"tool_call_id\""));
        assert_eq!(
            visible_deltas, "",
            "truncated internal protocol must not be released at stream finish"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_preserves_json_fenced_tool_protocol_without_tools()
    {
        struct JsonFencedToolProtocolProvider;
        impl_test_model_provider_attribution!(JsonFencedToolProtocolProvider);

        #[async_trait]
        impl ModelProvider for JsonFencedToolProtocolProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("```json\n"))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"{"tool_calls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                    ))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("\n```"))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = JsonFencedToolProtocolProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"tool_calls\""));
        assert_eq!(
            visible_deltas, outcome.response_text,
            "json-fenced protocol-shaped JSON should remain visible when no tools are active"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_buffers_tool_call_fence_with_tools() {
        struct ToolCallFenceProvider;
        impl_test_model_provider_attribution!(ToolCallFenceProvider);

        #[async_trait]
        impl ModelProvider for ToolCallFenceProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("```tool_call\n"))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"{"name":"count_tool","arguments":{"value":"X"}}"#,
                    ))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("\n```"))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = ToolCallFenceProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            Some(&[crate::tools::ToolSpec {
                name: "count_tool".to_string(),
                description: "Count values".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("```tool_call"));
        assert_eq!(
            visible_deltas, "",
            "streamed tool_call fences with registered tools must not reach draft updates"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_preserves_plain_prefix_before_protocol_without_tools()
     {
        struct PrefixedToolProtocolProvider;
        impl_test_model_provider_attribution!(PrefixedToolProtocolProvider);

        #[async_trait]
        impl ModelProvider for PrefixedToolProtocolProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"Visible prefix {"toolcalls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = PrefixedToolProtocolProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"toolcalls\""));
        assert_eq!(
            visible_deltas, outcome.response_text,
            "prefixed protocol-shaped JSON should remain visible when no tools are active"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_preserves_split_protocol_after_plain_prefix_without_tools()
     {
        struct SplitPrefixedToolProtocolProvider;
        impl_test_model_provider_attribution!(SplitPrefixedToolProtocolProvider);

        #[async_trait]
        impl ModelProvider for SplitPrefixedToolProtocolProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"Visible prefix {"tool"#,
                    ))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(
                        r#"calls":[{"name":"count_tool","arguments":{"value":"X"}}]}"#,
                    ))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }

        let provider = SplitPrefixedToolProtocolProvider;
        let messages = vec![ChatMessage::user("hi")];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert!(outcome.response_text.contains("\"toolcalls\""));
        assert_eq!(
            visible_deltas, outcome.response_text,
            "split prefixed protocol-shaped JSON should remain visible when no tools are active"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_native_tool_events_without_chat_fallback() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
            NativeStreamTurn::ToolCall(ToolCall {
                id: "call_native_1".to_string(),
                name: "count_tool".to_string(),
                arguments: r#"{"value":"A"}"#.to_string(),
                extra_content: None,
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 5,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
        assert_eq!(model_provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            model_provider.stream_tool_requests.load(Ordering::SeqCst),
            2
        );
        assert_eq!(model_provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
    }

    #[tokio::test]
    async fn run_tool_call_loop_does_not_duplicate_streamed_narration_before_native_tool_call() {
        let narration = "About to check the count.";
        let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
            NativeStreamTurn::NarrationThenToolCall {
                narration_chunks: vec!["About to ".to_string(), "check the count.".to_string()],
                tool_call: ToolCall {
                    id: "call_narrate_1".to_string(),
                    name: "count_tool".to_string(),
                    arguments: r#"{"value":"A"}"#.to_string(),
                    extra_content: None,
                },
            },
            NativeStreamTurn::Text("done".to_string()),
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("narrate then run native tool"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

        let turn_id = uuid::Uuid::new_v4().to_string();

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 5,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("narration-then-tool streaming should preserve tool loop semantics");

        let mut accumulated = String::new();
        let mut text_deltas: Vec<String> = Vec::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                accumulated.push_str(&text);
                text_deltas.push(text);
            }
        }

        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            result.ends_with("done"),
            "final response should end with 'done', got: {result}"
        );
        assert_eq!(
            accumulated.matches(narration).count(),
            1,
            "narration must appear exactly once in the accumulated draft; deltas={text_deltas:?} accumulated={accumulated:?}"
        );
        assert!(
            result.contains("done"),
            "final turn must not be truncated; got: {result}"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_forwards_native_narration_emitted_after_tool_call() {
        let trailing = "Let me check the count.";
        let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
            NativeStreamTurn::ToolCallThenNarration {
                tool_call: ToolCall {
                    id: "call_trailing_1".to_string(),
                    name: "count_tool".to_string(),
                    arguments: r#"{"value":"A"}"#.to_string(),
                    extra_content: None,
                },
                narration_chunks: vec!["Let me ".to_string(), "check the count.".to_string()],
            },
            NativeStreamTurn::Text("done".to_string()),
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run native tool then narrate"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

        let turn_id = uuid::Uuid::new_v4().to_string();

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 5,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("tool-then-narration streaming should preserve tool loop semantics");

        let mut accumulated = String::new();
        let mut text_deltas: Vec<String> = Vec::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                accumulated.push_str(&text);
                text_deltas.push(text);
            }
        }

        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            accumulated.contains(trailing),
            "narration emitted after the native tool call must reach the user; deltas={text_deltas:?} accumulated={accumulated:?}"
        );
        assert_eq!(
            accumulated.matches(trailing).count(),
            1,
            "post-tool narration must be forwarded exactly once, not dropped then re-sent; deltas={text_deltas:?} accumulated={accumulated:?}"
        );
        assert!(
            result.ends_with("done"),
            "final response should end with 'done', got: {result}"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_preserves_guard_withheld_narration_tail_before_tool_call() {
        let narration_full = "Checking <tool";
        let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
            NativeStreamTurn::NarrationThenToolCall {
                narration_chunks: vec!["Checking ".to_string(), "<tool".to_string()],
                tool_call: ToolCall {
                    id: "call_tail_1".to_string(),
                    name: "count_tool".to_string(),
                    arguments: r#"{"value":"A"}"#.to_string(),
                    extra_content: None,
                },
            },
            NativeStreamTurn::Text("done".to_string()),
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("narrate with a guard-buffered tail then run a native tool"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

        let turn_id = uuid::Uuid::new_v4().to_string();

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 5,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("guard-withheld narration tail should not break the tool loop");

        let mut accumulated = String::new();
        let mut text_deltas: Vec<String> = Vec::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                accumulated.push_str(&text);
                text_deltas.push(text);
            }
        }

        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            accumulated.contains(narration_full),
            "guard-withheld tail must still reach the draft exactly once; deltas={text_deltas:?} accumulated={accumulated:?}"
        );
        assert_eq!(
            accumulated.matches("<tool").count(),
            1,
            "narration tail must not be duplicated; deltas={text_deltas:?} accumulated={accumulated:?}"
        );
        assert!(
            result.ends_with("done"),
            "final turn must not be truncated; got: {result}"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_strips_split_think_tags_before_forwarding() {
        let model_provider =
            StreamingNativeToolEventModelProvider::with_turns(vec![NativeStreamTurn::TextChunks(
                vec![
                    "<thi".to_string(),
                    "nk>private stream reasoning</thi".to_string(),
                    "nk>visible answer".to_string(),
                ],
            )]);
        let messages = vec![ChatMessage::user("hi")];
        let tools = [crate::tools::ToolSpec {
            name: "count_tool".to_string(),
            description: "Count values".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);

        let outcome = consume_provider_streaming_response(
            &model_provider,
            &messages,
            Some(&tools),
            "mock-model",
            Some(0.0),
            None,
            Some(&tx),
            None, // event_tx
            true,
        )
        .await
        .expect("streaming should finish");
        drop(tx);

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Text(text) = delta {
                visible_deltas.push_str(&text);
            }
        }

        assert_eq!(outcome.response_text, "visible answer");
        assert_eq!(visible_deltas, "visible answer");
        assert!(!outcome.response_text.contains("private stream reasoning"));
        assert!(!outcome.response_text.contains("<think>"));
        assert!(!visible_deltas.contains("private stream reasoning"));
        assert!(!visible_deltas.contains("<think>"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_routed_streaming_uses_live_provider_deltas_once() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let default_model_provider = RouteAwareStreamingModelProvider::new("default answer");
        let default_stream_calls = Arc::clone(&default_model_provider.stream_calls);
        let default_chat_calls = Arc::clone(&default_model_provider.chat_calls);

        let routed_model_provider = RouteAwareStreamingModelProvider::new("routed streamed answer");
        let routed_stream_calls = Arc::clone(&routed_model_provider.stream_calls);
        let routed_chat_calls = Arc::clone(&routed_model_provider.chat_calls);
        let routed_last_model = Arc::clone(&routed_model_provider.last_model);

        let router = RouterModelProvider::new(
            "test",
            vec![
                ("default".to_string(), Box::new(default_model_provider)),
                ("fast".to_string(), Box::new(routed_model_provider)),
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &router,
                    provider_name: "router",
                    model: "hint:fast",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("routed streaming model_provider should complete");

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
            let model_provider = ScriptedModelProvider::from_text_responses(vec![
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
                &model_provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                Some(0.0),
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
                false,
                false, // parallel_tools
                0,     // max_tool_result_chars: disabled for test
                0,     // context_token_budget: disabled for test
                None,  // channel
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
    fn agent_turn_strict_tool_parsing_ignores_activated_tool_text_from_wrapper() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should initialize");

        runtime.block_on(async {
            let model_provider = ScriptedModelProvider::from_text_responses(vec![
                r#"<think>private reasoning</think>
<tool_call>
{"name":"pixel__get_api_health","arguments":{"value":"ignored"}}
</tool_call>"#,
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
                ChatMessage::user("do not infer activated tool calls from text"),
            ];
            let observer = NoopObserver;

            let result = agent_turn(
                &model_provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                Some(0.0),
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
                true,
                false, // parallel_tools
                0,     // max_tool_result_chars: disabled for test
                0,     // context_token_budget: disabled for test
                None,  // channel
            )
            .await
            .expect("strict wrapper path should preserve fallback-looking text");

            assert_eq!(invocations.load(Ordering::SeqCst), 0);
            assert!(
                result.contains("<tool_call>"),
                "strict parser should return fallback-looking text, got: {result}"
            );
            assert!(
                !result.contains("private reasoning"),
                "strict parser should still strip think tags from final text, got: {result}"
            );
        });
    }

    // ── Regression tests for trimming-budget forwarding through agent_turn ────

    /// A mock tool that produces a configurable-length output string for
    /// testing that `max_tool_result_chars` is forwarded through
    /// `agent_turn` to `run_tool_call_loop`.
    struct VerboseTool {
        name: String,
        output_len: usize,
        invocations: Arc<AtomicUsize>,
    }

    impl VerboseTool {
        fn new(name: &str, output_len: usize, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                output_len,
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for VerboseTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Returns a long output for trimming-budget regression tests"
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
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let output = format!(
                "verbose-start-{}-{}",
                self.name,
                "X".repeat(self.output_len)
            );
            Ok(crate::tools::ToolResult {
                success: true,
                output,
                error: None,
            })
        }
    }

    /// When `max_tool_result_chars` is set to a non-zero value, `agent_turn`
    /// should forward it to `run_tool_call_loop`, which truncates oversized
    /// tool results. This test verifies the old hardcoded-0 bug (where
    /// trimming was silently disabled) does not regress.
    #[test]
    fn agent_turn_forwards_max_tool_result_chars_to_truncate_tool_result() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should initialize");

        runtime.block_on(async {
            let model_provider = ScriptedModelProvider::from_text_responses(vec![
                r#"<tool_call>
{"name":"verbose_checker","arguments":{"value":"check"}}
</tool_call>"#,
                "done",
            ]);

            let invocations = Arc::new(AtomicUsize::new(0));
            let verbose_tool: Box<dyn Tool> = Box::new(VerboseTool::new(
                "verbose_checker",
                500, // produce 500+ chars of output
                Arc::clone(&invocations),
            ));
            let tools_registry: Vec<Box<dyn Tool>> = vec![verbose_tool];
            let mut history = vec![
                ChatMessage::system("test-system"),
                ChatMessage::user("check"),
            ];
            let observer = NoopObserver;

            let _result = agent_turn(
                &model_provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                Some(0.0),
                true,
                "daemon",
                None,
                &zeroclaw_config::schema::MultimodalConfig::default(),
                4,
                None,
                &[],
                &[],
                None,
                None,
                false,
                false,
                100, // max_tool_result_chars: truncate at 100 chars
                0,   // context_token_budget: disabled
                None,
            )
            .await
            .expect("agent_turn should complete");

            assert_eq!(
                invocations.load(Ordering::SeqCst),
                1,
                "tool should be called once"
            );

            // The tool result in history should be truncated (contain the
            // truncation marker "...") rather than preserving the full 500+ char output.
            let all_content: String = history
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<&str>>()
                .join(" ");

            assert!(
                !all_content.contains(&"X".repeat(500)),
                "tool result should not contain 500 consecutive X chars when truncated to 100 chars"
            );
        });
    }

    /// Control test: when `max_tool_result_chars = 0` (disabled), the full
    /// tool result must be preserved in history without truncation.
    #[test]
    fn agent_turn_with_zero_budget_preserves_full_tool_result() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should initialize");

        runtime.block_on(async {
            let model_provider = ScriptedModelProvider::from_text_responses(vec![
                r#"<tool_call>
{"name":"verbose_checker","arguments":{"value":"check"}}
</tool_call>"#,
                "done",
            ]);

            let invocations = Arc::new(AtomicUsize::new(0));
            let verbose_tool: Box<dyn Tool> = Box::new(VerboseTool::new(
                "verbose_checker",
                500,
                Arc::clone(&invocations),
            ));
            let tools_registry: Vec<Box<dyn Tool>> = vec![verbose_tool];
            let mut history = vec![
                ChatMessage::system("test-system"),
                ChatMessage::user("check"),
            ];
            let observer = NoopObserver;

            let _result = agent_turn(
                &model_provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                Some(0.0),
                true,
                "daemon",
                None,
                &zeroclaw_config::schema::MultimodalConfig::default(),
                4,
                None,
                &[],
                &[],
                None,
                None,
                false,
                false,
                0, // max_tool_result_chars: disabled (no truncation)
                0, // context_token_budget: disabled
                None,
            )
            .await
            .expect("agent_turn should complete");

            assert_eq!(invocations.load(Ordering::SeqCst), 1);

            // Check that the full output is preserved somewhere in history
            // (tool results may appear under different roles depending on the
            // message format used by run_tool_call_loop).
            let all_content: String = history
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<&str>>()
                .join(" ");

            assert!(
                all_content.contains(&"X".repeat(500)),
                "full tool result should be preserved somewhere in history when max_tool_result_chars=0, \
                 history roles: {:?}",
                history.iter().map(|m| m.role.clone()).collect::<Vec<_>>()
            );
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
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
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
    fn build_tool_instructions_empty_registry_returns_empty() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let instructions = build_tool_instructions(&tools);

        assert!(instructions.is_empty());
    }

    #[test]
    fn tools_to_openai_format_produces_valid_schema() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
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
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();

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
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        mem.store(
            "assistant_resp_poisoned",
            "User suffered a fabricated event",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_preference",
            "User asked for concise status updates",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, &NoopObserver, "status updates", 0.0, None, false).await;
        assert!(context.contains("user_preference"));
        assert!(!context.contains("assistant_resp_poisoned"));
        assert!(!context.contains("fabricated event"));
    }

    #[tokio::test]
    async fn build_context_ignores_user_autosave_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        mem.store(
            "user_msg",
            "Original user message with full conversation history",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_msg_a1b2c3d4",
            "Follow-up user message embedding prior context verbatim",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_preference",
            "User prefers concise answers",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, &NoopObserver, "answers", 0.0, None, false).await;
        assert!(context.contains("user_preference"));
        assert!(!context.contains("user_msg"));
        assert!(!context.contains("embedding prior context"));
    }

    /// Regression: cron / heartbeat runs must not surface chat-origin
    /// `Conversation` memories — the leak path the #5456 prefix filter
    /// missed because `agent::run` performs a second, unfiltered recall
    /// inside `build_context`.
    #[tokio::test]
    async fn build_context_excludes_conversation_when_flag_set() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        // A Conversation entry written by a chat channel with a non-autosave
        // key (autosave keys are already skipped by the existing filters).
        mem.store(
            "discord:guild:chan:msg-42",
            "Reminder for Alice: the API key is in 1Password vault Foo.",
            MemoryCategory::Conversation,
            Some("discord:guild:chan"),
        )
        .await
        .unwrap();
        // A non-Conversation memory that should still surface so we know the
        // function still does its job — only Conversation should be dropped.
        mem.store(
            "team_oncall",
            "Primary on-call rotates every Monday at 09:00 UTC.",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, &NoopObserver, "Alice on-call", 0.0, None, true).await;
        assert!(
            !context.contains("Alice"),
            "Conversation memory leaked into scheduled context: {context}"
        );
        assert!(
            !context.contains("API key"),
            "Conversation memory leaked into scheduled context: {context}"
        );
        assert!(
            context.contains("team_oncall"),
            "Non-Conversation memory should still surface: {context}"
        );
    }

    #[test]
    fn make_query_summary_redacts_credentials_and_caps_length() {
        // Empty input → None (so observers can distinguish "no query
        // recorded" from "empty query string").
        assert!(make_query_summary("").is_none());

        // Plain query passes through unchanged when within the cap.
        let plain = make_query_summary("hello world").unwrap();
        assert_eq!(plain, "hello world");

        // Credential pattern is scrubbed by `scrub_credentials` before
        // the truncation step. The raw token must not appear in the
        // emitted summary.
        let scrubbed =
            make_query_summary("connect with api_key: sk-proj-abcdef1234567890zzz").unwrap();
        assert!(
            !scrubbed.contains("sk-proj-abcdef1234567890zzz"),
            "raw credential leaked into query_summary: {scrubbed:?}"
        );

        // Long input is truncated. `truncate_with_ellipsis(s, 200)` keeps up
        // to 200 content chars and appends "..." when it had to truncate,
        // for a total ceiling of 203 chars.
        let long_input = "a".repeat(500);
        let truncated = make_query_summary(&long_input).unwrap();
        assert!(
            truncated.chars().count() <= 203,
            "expected ≤203 chars (200 content + ellipsis), got {}",
            truncated.chars().count()
        );
        assert!(
            truncated.ends_with("..."),
            "expected trailing ellipsis on truncated input, got {truncated:?}"
        );
    }

    /// Captured `MemoryRecall` event used by the wiring-contract tests below.
    struct CapturedRecall {
        query_summary: Option<String>,
        backend: String,
        success: bool,
    }

    /// Counting observer that records every `MemoryRecall` event so the test
    /// can assert that the runtime hot path emits the variant once per
    /// `build_context` call. Locks in the wiring contract that motivated the
    /// memory-OTel work.
    #[derive(Default)]
    struct RecallCountingObserver {
        recalls: parking_lot::Mutex<Vec<CapturedRecall>>,
    }

    impl crate::observability::Observer for RecallCountingObserver {
        fn record_event(&self, event: &crate::observability::ObserverEvent) {
            if let crate::observability::ObserverEvent::MemoryRecall {
                query_summary,
                backend,
                success,
                ..
            } = event
            {
                self.recalls.lock().push(CapturedRecall {
                    query_summary: query_summary.clone(),
                    backend: backend.clone(),
                    success: *success,
                });
            }
        }

        fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}

        fn name(&self) -> &str {
            "recall-counting-observer"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn build_context_emits_memory_recall_event() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let observer = RecallCountingObserver::default();

        let _ = build_context(&mem, &observer, "any query", 0.0, None, false).await;

        let recalls = observer.recalls.lock();
        assert_eq!(
            recalls.len(),
            1,
            "build_context must emit exactly one MemoryRecall event per call"
        );
        assert_eq!(recalls[0].query_summary.as_deref(), Some("any query"));
        assert_eq!(recalls[0].backend, "sqlite");
        assert!(
            recalls[0].success,
            "successful recall must report success = true"
        );
    }

    /// Memory backend whose `recall` always returns `Err`. Used to exercise
    /// the failure arm of `build_context`'s explicit match — the runtime
    /// swallows the error and returns an empty context, but observers must
    /// still see a `MemoryRecall { success: false }` event.
    struct FailingRecallMemory;

    #[async_trait]
    impl zeroclaw_memory::Memory for FailingRecallMemory {
        fn name(&self) -> &str {
            "failing-recall"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
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
            anyhow::bail!("simulated recall failure")
        }
        async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
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
            false
        }
        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
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
    impl ::zeroclaw_api::attribution::Attributable for FailingRecallMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "FailingRecallMemory"
        }
    }

    #[tokio::test]
    async fn build_context_emits_memory_recall_event_on_failure() {
        let mem = FailingRecallMemory;
        let observer = RecallCountingObserver::default();

        let context = build_context(&mem, &observer, "any query", 0.0, None, false).await;
        assert!(
            context.is_empty(),
            "recall failure must still produce empty context (swallow behavior)"
        );

        let recalls = observer.recalls.lock();
        assert_eq!(
            recalls.len(),
            1,
            "build_context must emit exactly one MemoryRecall event even on Err"
        );
        assert_eq!(recalls[0].query_summary.as_deref(), Some("any query"));
        assert_eq!(recalls[0].backend, "failing-recall");
        assert!(
            !recalls[0].success,
            "failed recall must report success = false"
        );
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
    fn trim_history_keeps_first_user_anchor_and_recent_tail() {
        // The framing anchor (first user message) must survive trim so the
        // model doesn't start a turn thinking "Continue" is the first thing
        // it ever saw. Middle messages are the ones that get dropped.
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("anchor: what's the task"),
            ChatMessage::assistant("middle reply 1"),
            ChatMessage::user("middle user 1"),
            ChatMessage::assistant("middle reply 2"),
            ChatMessage::user("recent user"),
            ChatMessage::assistant("recent reply"),
        ];
        // max_history = 3 → keep anchor + 2 most recent (=3 non-system).
        trim_history(&mut history, 3);
        assert_eq!(history[0].role, "system");
        assert_eq!(
            history[1].content, "anchor: what's the task",
            "first user message (framing anchor) must survive"
        );
        let last = history.last().expect("history not empty");
        assert_eq!(last.content, "recent reply", "tail must be preserved");
    }

    #[test]
    fn trim_history_falls_back_to_tail_when_max_history_is_one() {
        // With max_history=1 there's no room for both anchor and tail; fall
        // back to plain head-drop so we don't produce a degenerate window.
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("anchor"),
            ChatMessage::assistant("middle"),
            ChatMessage::user("recent"),
        ];
        trim_history(&mut history, 1);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].content, "recent");
    }

    /// When `build_system_prompt_with_mode` is called with
    /// `native_tool_specs_present = true`, the output must contain ZERO XML
    /// protocol artifacts and must not inject the duplicate non-native tools
    /// summary.
    #[test]
    fn native_tools_system_prompt_contains_zero_xml() {
        use crate::agent::system_prompt::build_system_prompt_with_mode;

        let workspace = tempdir().unwrap();
        let tool_summaries: Vec<(&str, &str)> = vec![
            ("shell", "Execute shell commands"),
            ("file_read", "Read files"),
        ];

        let system_prompt = build_system_prompt_with_mode(
            workspace.path(),
            "test-model",
            &tool_summaries,
            &[],  // no skills
            None, // no identity config
            None, // no bootstrap_max_chars
            true, // native_tool_specs_present
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

        // Positive: native prompt should still contain native-task framing.
        assert!(
            !system_prompt.contains("## Tools"),
            "Native prompt should skip the duplicate tools summary"
        );
        assert!(
            system_prompt.contains("## Your Task"),
            "Native prompt should contain task instructions"
        );
    }

    #[test]
    fn native_tool_specs_prompt_allows_actions_without_text_tools() {
        use crate::agent::system_prompt::build_system_prompt_with_mode;

        let workspace = tempdir().unwrap();
        let provider =
            ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn crate::tools::Tool>> =
            vec![Box::new(CountingTool::new("shell", invocations))];

        let native_tool_specs_present =
            super::native_tool_specs_present_for_turn(&provider, &tools_registry, &[], None)
                .expect("native spec availability should be derivable");
        assert!(native_tool_specs_present);

        let system_prompt = build_system_prompt_with_mode(
            workspace.path(),
            "test-model",
            &[],
            &[],
            None,
            None,
            native_tool_specs_present,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            crate::security::AutonomyLevel::default(),
        );

        assert!(
            !system_prompt.contains("No tools are available"),
            "Native prompt with effective native specs must not deny tool availability"
        );
        assert!(
            system_prompt.contains("Use tools when the request requires action"),
            "Native prompt with effective native specs should authorize action tool use"
        );
    }

    #[test]
    fn native_capable_provider_with_zero_effective_tools_keeps_no_tools_boundary() {
        use crate::agent::system_prompt::build_system_prompt_with_mode;

        let provider =
            ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn crate::tools::Tool>> =
            vec![Box::new(CountingTool::new("shell", invocations))];
        let excluded_tools = vec!["shell".to_string()];

        let native_tool_specs_present = super::native_tool_specs_present_for_turn(
            &provider,
            &tools_registry,
            &excluded_tools,
            None,
        )
        .expect("native spec availability should be derivable");
        assert!(!native_tool_specs_present);

        let system_prompt = build_system_prompt_with_mode(
            std::path::Path::new("/tmp"),
            "test-model",
            &[],
            &[],
            None,
            None,
            native_tool_specs_present,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            crate::security::AutonomyLevel::default(),
        );

        assert!(
            system_prompt.contains("No tools are available for this turn"),
            "Native-capable providers with zero effective specs must keep the no-tools boundary"
        );
    }

    #[test]
    fn native_tool_specs_present_signal_uses_effective_specs_not_provider_capability() {
        let provider =
            ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
        assert!(zeroclaw_providers::ModelProvider::supports_native_tools(
            &provider
        ));

        let no_tools: Vec<Box<dyn crate::tools::Tool>> = Vec::new();
        let native_tool_specs_present =
            super::native_tool_specs_present_for_turn(&provider, &no_tools, &[], None)
                .expect("native spec availability should be derivable");

        assert!(
            !native_tool_specs_present,
            "Provider native-tool capability alone must not imply tools are available"
        );
    }

    #[test]
    fn interactive_turn_system_prompt_uses_effective_dynamic_mcp_specs() {
        use crate::agent::system_prompt::{NATIVE_TOOLS_TASK_FRAMING, NO_TOOLS_TASK_FRAMING};
        use zeroclaw_config::schema::{
            RiskProfileConfig, SkillsPromptInjectionMode, ToolFilterGroup, ToolFilterGroupMode,
        };

        let workspace = tempdir().unwrap();
        let provider =
            ScriptedModelProvider::from_text_responses(vec!["ok"]).with_native_tool_support();
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn crate::tools::Tool>> = vec![Box::new(CountingTool::new(
            "mcp_browser_navigate",
            invocations,
        ))];
        let groups = vec![ToolFilterGroup {
            mode: ToolFilterGroupMode::Dynamic,
            tools: vec!["mcp_browser_*".into()],
            keywords: vec!["browse".into()],
            filter_builtins: false,
        }];
        let tool_descs: Vec<(&str, &str)> = Vec::new();
        let risk_profile = RiskProfileConfig::default();

        // Interactive startup with no message used to save this as
        // `base_system_prompt`, which advertises native tool availability.
        let startup_prompt = super::build_system_prompt_for_turn(
            workspace.path(),
            "test-model",
            &tool_descs,
            "",
            &[],
            None,
            None,
            &risk_profile,
            &provider,
            &tools_registry,
            &[],
            None,
            false,
            SkillsPromptInjectionMode::Full,
            false,
            usize::MAX,
            true,
            false,
            None,
        )
        .expect("startup prompt should build");
        assert!(startup_prompt.contains(NATIVE_TOOLS_TASK_FRAMING));
        assert!(!startup_prompt.contains(NO_TOOLS_TASK_FRAMING));

        let excluded_tools =
            super::compute_excluded_mcp_tools(&tools_registry, &groups, "read the local file");
        assert_eq!(excluded_tools, vec!["mcp_browser_navigate".to_string()]);
        let no_tools_turn_prompt = super::build_system_prompt_for_turn(
            workspace.path(),
            "test-model",
            &tool_descs,
            "",
            &[],
            None,
            None,
            &risk_profile,
            &provider,
            &tools_registry,
            &excluded_tools,
            None,
            false,
            SkillsPromptInjectionMode::Full,
            false,
            usize::MAX,
            true,
            false,
            None,
        )
        .expect("no-tools turn prompt should build");
        assert!(
            no_tools_turn_prompt.contains(NO_TOOLS_TASK_FRAMING),
            "turn prompt must not inherit startup native-tool framing when dynamic filters exclude all MCP specs"
        );
        assert!(!no_tools_turn_prompt.contains(NATIVE_TOOLS_TASK_FRAMING));

        let included_tools =
            super::compute_excluded_mcp_tools(&tools_registry, &groups, "browse the site");
        assert!(included_tools.is_empty());
        let tools_turn_prompt = super::build_system_prompt_for_turn(
            workspace.path(),
            "test-model",
            &tool_descs,
            "",
            &[],
            None,
            None,
            &risk_profile,
            &provider,
            &tools_registry,
            &included_tools,
            None,
            false,
            SkillsPromptInjectionMode::Full,
            false,
            usize::MAX,
            true,
            false,
            None,
        )
        .expect("tools turn prompt should build");
        assert!(tools_turn_prompt.contains(NATIVE_TOOLS_TASK_FRAMING));
        assert!(!tools_turn_prompt.contains(NO_TOOLS_TASK_FRAMING));
    }

    #[test]
    fn non_native_system_prompt_with_no_tools_contains_zero_tool_protocol() {
        use crate::agent::system_prompt::build_system_prompt_with_mode;

        let tool_summaries: Vec<(&str, &str)> = vec![];

        let system_prompt = build_system_prompt_with_mode(
            std::path::Path::new("/tmp"),
            "test-model",
            &tool_summaries,
            &[],
            None,
            None,
            false,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            crate::security::AutonomyLevel::default(),
        );

        assert!(
            !system_prompt.contains("## Tools"),
            "No-tools prompt must not include a Tools section"
        );
        assert!(
            !system_prompt.contains("## Tool Use Protocol"),
            "No-tools prompt must not include tool protocol"
        );
        assert!(
            !system_prompt.contains("<tool_call>"),
            "No-tools prompt must not mention XML tool calls"
        );
        assert!(
            !system_prompt.contains("<tool_result>"),
            "No-tools prompt must not mention XML tool results"
        );
        assert!(
            !system_prompt.contains("Use the tools"),
            "No-tools prompt must not instruct the model to use unavailable tools"
        );
        assert!(
            system_prompt.contains("No tools are available for this turn"),
            "No-tools prompt should explicitly describe the current capability boundary"
        );
    }

    #[test]
    fn strict_non_native_prompt_policy_hides_text_tool_protocol_inputs() {
        let mut tool_descs = vec![("shell", "Run commands")];
        let mut deferred_section = "## Deferred MCP Tools\n\n- mcp__example".to_string();

        let expose_text_protocol =
            apply_text_tool_prompt_policy(false, true, &mut tool_descs, &mut deferred_section);

        assert!(!expose_text_protocol);
        assert!(
            tool_descs.is_empty(),
            "strict non-native prompt paths must not advertise text tools"
        );
        assert!(
            deferred_section.is_empty(),
            "strict non-native prompt paths must not advertise deferred text tools"
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
            extra_content: None,
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
            extra_content: None,
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

    /// Regression test for issue #6059 — DeepSeek V4 thinking-mode tool-call
    /// replay rejected with `400` because the assistant's prior
    /// `reasoning_content` was missing from the next request.
    ///
    /// Before the fix, the streaming consumer dropped reasoning chunks on the
    /// floor (`chunk.delta.is_empty()` short-circuit + hardcoded
    /// `reasoning_content: None` on the synthesized `ChatResponse`). After
    /// the fix, reasoning deltas accumulate into `StreamedChatOutcome` and
    /// surface on the response so the agent's history layer can persist them
    /// and replay them on subsequent turns.
    #[tokio::test]
    async fn consume_provider_streaming_response_captures_reasoning_content() {
        let model_provider = StreamingNativeToolEventModelProvider::with_turns(vec![
            NativeStreamTurn::TextWithReasoning {
                text: "Listing the directory now.".to_string(),
                reasoning: "I need to call the shell tool to list files.".to_string(),
            },
        ]);
        let messages = vec![ChatMessage::user(
            "List the folders in the current directory",
        )];

        let outcome = consume_provider_streaming_response(
            &model_provider,
            &messages,
            None,
            "deepseek-v4-pro",
            Some(0.2),
            None,
            None,
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should succeed");

        assert_eq!(outcome.response_text, "Listing the directory now.");
        assert_eq!(
            outcome.reasoning_content,
            "I need to call the shell tool to list files."
        );
        assert!(
            outcome.tool_calls.is_empty(),
            "this turn does not emit native tool calls"
        );
    }

    #[tokio::test]
    async fn consume_provider_streaming_response_accumulates_split_reasoning_chunks() {
        // Scripted multi-event stream: two reasoning chunks straddling a text
        // delta. The outcome should concatenate the reasoning chunks in order
        // and keep them out of the visible response text.
        struct MultiChunkModelProvider;

        #[async_trait]
        impl ModelProvider for MultiChunkModelProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                anyhow::bail!("not used in this test")
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("not used in this test")
            }

            fn supports_streaming(&self) -> bool {
                true
            }

            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                zeroclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::reasoning("Step 1: "))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("Hello "))),
                    Ok(StreamEvent::TextDelta(StreamChunk::reasoning(
                        "consider options.",
                    ))),
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("there."))),
                    Ok(StreamEvent::Final),
                ]))
            }
        }
        impl ::zeroclaw_api::attribution::Attributable for MultiChunkModelProvider {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "MultiChunkModelProvider"
            }
        }

        let model_provider = MultiChunkModelProvider;
        let messages = vec![ChatMessage::user("hi")];

        let outcome = consume_provider_streaming_response(
            &model_provider,
            &messages,
            None,
            "deepseek-v4-flash",
            Some(0.2),
            None,
            None,
            None, // event_tx
            false,
        )
        .await
        .expect("streaming should succeed");

        assert_eq!(outcome.response_text, "Hello there.");
        assert_eq!(outcome.reasoning_content, "Step 1: consider options.");
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
        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 4,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "telegram",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(tx),
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
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
            TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoop, ToolLoopCostTrackingContext,
            run_tool_call_loop,
        };
        use crate::cost::CostTracker;
        use crate::observability::noop::NoopObserver;
        use std::collections::HashMap;

        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider = ScriptedModelProvider {
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
        let cost_config = zeroclaw_config::schema::CostConfig {
            enabled: true,
            ..zeroclaw_config::schema::CostConfig::default()
        };
        let tracker = Arc::new(CostTracker::new(cost_config.clone(), workspace.path()).unwrap());
        let mut model_pricing: HashMap<String, f64> = HashMap::new();
        model_pricing.insert("mock-model.input".to_string(), 3.0);
        model_pricing.insert("mock-model.output".to_string(), 15.0);
        let mut pricing: crate::agent::cost::ModelProviderPricing = HashMap::new();
        pricing.insert("mock-provider".to_string(), model_pricing);
        let ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(pricing));
        let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

        let result = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                run_tool_call_loop(ToolLoop {
                    exec: ResolvedAgentExecution {
                        model_access: ResolvedModelAccess {
                            model_provider: &model_provider,
                            provider_name: "mock-provider",
                            model: "mock-model",
                            temperature: Some(0.0),
                        },
                        tools_registry: &[],
                        observer: &observer,
                        silent: true,
                        approval: None,
                        multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                        max_tool_iterations: 2,
                        hooks: None,
                        excluded_tools: &[],
                        dedup_exempt_tools: &[],
                        activated_tools: None,
                        model_switch_callback: None,
                        pacing: &zeroclaw_config::schema::PacingConfig::default(),
                        strict_tool_parsing: false,
                        parallel_tools: false,
                        max_tool_result_chars: 0,
                        context_token_budget: 0,
                        receipt_generator: None,
                        knobs: &LoopKnobs::default(),
                    },
                    history: &mut history,
                    channel_name: "test",
                    channel_reply_target: None,
                    cancellation_token: None,
                    on_delta: None,
                    shared_budget: None,
                    channel: None,
                    collected_receipts: None,
                    event_tx: None,
                    steering: None,
                    new_messages_out: None,
                    image_cache: None,
                    // Phase 1: stamp Internal/Trusted. Real per-transport
                    // stamping is PR C (RFC #6971 §4).
                    ingress: IngressContext::internal(),
                    agent_alias: None,
                    turn_id: &turn_id,
                }),
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
    async fn tool_loop_normalizes_non_leading_system_messages_before_provider_request() {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let provider = RecordingModelProvider::new();
        let requests = Arc::clone(&provider.requests);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("base system"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::system("late loop-detection guidance"),
            ChatMessage::user("follow-up"),
        ];

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &provider,
                    provider_name: "recording-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &[],
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 2,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("tool loop should complete");

        assert_eq!(result, "done");
        let requests = requests.lock().expect("requests lock should be valid");
        assert_eq!(requests.len(), 1);
        let sent = &requests[0];
        assert_eq!(sent[0].role, "system");
        assert_eq!(
            sent.iter().filter(|msg| msg.role == "system").count(),
            1,
            "provider request must not contain non-leading system messages: {:?}",
            sent.iter().map(|msg| msg.role.as_str()).collect::<Vec<_>>()
        );
        assert!(sent[0].content.contains("base system"));
        assert!(sent[0].content.contains("late loop-detection guidance"));
        assert_eq!(
            sent.iter().map(|msg| msg.role.as_str()).collect::<Vec<_>>(),
            vec!["system", "user", "assistant", "user"]
        );
    }

    #[tokio::test]
    async fn cost_tracking_enforces_budget() {
        use super::{
            TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoop, ToolLoopCostTrackingContext,
            run_tool_call_loop,
        };
        use crate::cost::CostTracker;
        use crate::observability::noop::NoopObserver;
        use std::collections::HashMap;

        let turn_id = uuid::Uuid::new_v4().to_string();
        let model_provider =
            ScriptedModelProvider::from_text_responses(vec!["should not reach this"]);
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
                0,
                1.0,
                1.0,
                0.0,
            ))
            .unwrap();

        let mut model_pricing: HashMap<String, f64> = HashMap::new();
        model_pricing.insert("mock-model.input".to_string(), 1.0);
        model_pricing.insert("mock-model.output".to_string(), 1.0);
        let mut pricing: crate::agent::cost::ModelProviderPricing = HashMap::new();
        pricing.insert("mock-provider".to_string(), model_pricing);
        let ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(pricing));
        let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

        let err = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                run_tool_call_loop(ToolLoop {
                    exec: ResolvedAgentExecution {
                        model_access: ResolvedModelAccess {
                            model_provider: &model_provider,
                            provider_name: "mock-provider",
                            model: "mock-model",
                            temperature: Some(0.0),
                        },
                        tools_registry: &[],
                        observer: &observer,
                        silent: true,
                        approval: None,
                        multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                        max_tool_iterations: 2,
                        hooks: None,
                        excluded_tools: &[],
                        dedup_exempt_tools: &[],
                        activated_tools: None,
                        model_switch_callback: None,
                        pacing: &zeroclaw_config::schema::PacingConfig::default(),
                        strict_tool_parsing: false,
                        parallel_tools: false,
                        max_tool_result_chars: 0,
                        context_token_budget: 0,
                        receipt_generator: None,
                        knobs: &LoopKnobs::default(),
                    },
                    history: &mut history,
                    channel_name: "test",
                    channel_reply_target: None,
                    cancellation_token: None,
                    on_delta: None,
                    shared_budget: None,
                    channel: None,
                    collected_receipts: None,
                    event_tx: None,
                    steering: None,
                    new_messages_out: None,
                    image_cache: None,
                    // Phase 1: stamp Internal/Trusted. Real per-transport
                    // stamping is PR C (RFC #6971 §4).
                    ingress: IngressContext::internal(),
                    agent_alias: None,
                    turn_id: &turn_id,
                }),
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
        let turn_id = uuid::Uuid::new_v4().to_string();
        use super::{ToolLoop, run_tool_call_loop};
        use crate::observability::noop::NoopObserver;

        // No TOOL_LOOP_COST_TRACKING_CONTEXT scoped — should run fine
        let model_provider = ScriptedModelProvider {
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

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &[],
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 2,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            // Phase 1: stamp Internal/Trusted. Real per-transport
            // stamping is PR C (RFC #6971 §4).
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: &turn_id,
        })
        .await
        .expect("should succeed without cost scope");

        assert_eq!(result, "ok");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn trim_record_carries_model_attribution() {
        use super::{ToolLoop, run_tool_call_loop};
        use crate::observability::noop::NoopObserver;
        use ::zeroclaw_log::Instrument;

        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let model_provider = ScriptedModelProvider {
            responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            }]))),
            capabilities: ProviderCapabilities::default(),
        };
        let observer = NoopObserver;

        let big = "x".repeat(4000);
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user(format!("turn1 {big}")),
            ChatMessage::assistant("a1"),
            ChatMessage::user(format!("turn2 {big}")),
            ChatMessage::assistant("a2"),
            ChatMessage::user("turn3 short"),
        ];

        let _ = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "anthropic.personal",
                    model: "claude-opus-4-8",
                    temperature: Some(0.0),
                },
                tools_registry: &[],
                observer: &observer,
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 1,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 50,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: None,
            turn_id: "test-turn-id",
        })
        .instrument(zeroclaw_log::attribution_span!(
            &crate::agent::AgentAttribution("trimtest")
        ))
        .await
        .expect("loop should succeed");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut trim_event = None;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .is_some_and(|m| m.starts_with("History trimmed:"))
                    {
                        trim_event = Some(value);
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        let value = trim_event.expect("trim record must be emitted when history exceeds budget");
        assert_eq!(
            value["zeroclaw"]["model"], "claude-opus-4-8",
            "trim record must carry model attribution, got: {value}"
        );
        assert_eq!(
            value["zeroclaw"]["model_provider"], "anthropic.personal",
            "trim record must carry model_provider attribution, got: {value}"
        );
        assert_eq!(
            value["zeroclaw"]["agent_alias"], "trimtest",
            "trim record must inherit agent_alias from the agent attribution span, got: {value}"
        );
    }
    //
    // The post-turn skill-review fork (`crate::skills::review::maybe_run_skill_review`)
    // runs AFTER the parent turn's `TOOL_LOOP_COST_TRACKING_CONTEXT.scope(...)`
    // has exited, so `agent::loop_::run` re-scopes it under the SAME
    // `cost_tracking_context` as the parent turn. These tests pin that wiring:
    // the fork's provider calls must be recorded against — and bounded by — the
    // same tracker/budget as the parent.

    fn review_test_pricing() -> crate::agent::cost::ModelProviderPricing {
        use std::collections::HashMap;
        let mut model_pricing: HashMap<String, f64> = HashMap::new();
        model_pricing.insert("mock-model.input".to_string(), 3.0);
        model_pricing.insert("mock-model.output".to_string(), 15.0);
        let mut pricing: crate::agent::cost::ModelProviderPricing = HashMap::new();
        pricing.insert("mock-provider".to_string(), model_pricing);
        pricing
    }

    fn review_test_history() -> Vec<ChatMessage> {
        // One completed tool call so `should_trigger` (threshold 1) fires.
        vec![
            ChatMessage::system("test"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("..."),
            ChatMessage {
                role: "tool".to_string(),
                content: "ok".to_string(),
            },
        ]
    }

    #[tokio::test]
    async fn skill_review_fork_records_cost_usage_under_parent_scope() {
        use super::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
        use crate::cost::CostTracker;
        use crate::observability::noop::NoopObserver;

        // Fork's single provider turn returns "Nothing to save." with usage —
        // no tool calls, so the fork ends after one recorded provider call.
        let model_provider = ScriptedModelProvider {
            responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
                text: Some("Nothing to save.".to_string()),
                tool_calls: Vec::new(),
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(800),
                    output_tokens: Some(120),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            }]))),
            capabilities: ProviderCapabilities::default(),
        };
        let observer = NoopObserver;
        let workspace = tempfile::TempDir::new().unwrap();
        let cost_config = zeroclaw_config::schema::CostConfig {
            enabled: true,
            ..zeroclaw_config::schema::CostConfig::default()
        };
        let tracker = Arc::new(CostTracker::new(cost_config, workspace.path()).unwrap());
        let ctx =
            ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(review_test_pricing()));

        let review_config = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 0,
            nudge_interval_iterations: 1,
            max_review_iterations: 2,
        };

        TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                crate::skills::review::maybe_run_skill_review(
                    workspace.path().to_path_buf(),
                    review_config,
                    false,
                    review_test_history(),
                    Vec::new(),
                    &model_provider,
                    "mock-provider",
                    "mock-model",
                    &observer,
                    &zeroclaw_config::schema::MultimodalConfig::default(),
                    &zeroclaw_config::schema::PacingConfig::default(),
                    0,
                    0,
                    None,
                    None, // agent_alias — no parent alias in the review-fork test fixture
                ),
            )
            .await;

        let summary = tracker.get_summary().unwrap();
        assert_eq!(
            summary.request_count, 1,
            "review fork must record its provider call under the parent cost scope"
        );
        assert_eq!(summary.total_tokens, 920);
        assert!(summary.session_cost_usd > 0.0);
    }

    #[tokio::test]
    async fn skill_review_fork_respects_budget_under_parent_scope() {
        use super::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
        use crate::cost::CostTracker;
        use crate::observability::noop::NoopObserver;

        // Provider that WOULD record usage if reached — but the pre-exceeded
        // budget must block the call before it happens.
        let model_provider = ScriptedModelProvider {
            responses: Arc::new(Mutex::new(VecDeque::from([ChatResponse {
                text: Some("should not be reached".to_string()),
                tool_calls: Vec::new(),
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(800),
                    output_tokens: Some(120),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            }]))),
            capabilities: ProviderCapabilities::default(),
        };
        let observer = NoopObserver;
        let workspace = tempfile::TempDir::new().unwrap();
        let cost_config = zeroclaw_config::schema::CostConfig {
            enabled: true,
            daily_limit_usd: 0.001, // very low limit
            ..zeroclaw_config::schema::CostConfig::default()
        };
        let tracker = Arc::new(CostTracker::new(cost_config, workspace.path()).unwrap());
        // Record usage that already exceeds the daily limit.
        tracker
            .record_usage(crate::cost::types::TokenUsage::new(
                "mock-model",
                100_000,
                50_000,
                0,
                1.0,
                1.0,
                0.0,
            ))
            .unwrap();
        let before = tracker.get_summary().unwrap().request_count;

        let ctx =
            ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(review_test_pricing()));
        let review_config = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 0,
            nudge_interval_iterations: 1,
            max_review_iterations: 2,
        };

        TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                crate::skills::review::maybe_run_skill_review(
                    workspace.path().to_path_buf(),
                    review_config,
                    false,
                    review_test_history(),
                    Vec::new(),
                    &model_provider,
                    "mock-provider",
                    "mock-model",
                    &observer,
                    &zeroclaw_config::schema::MultimodalConfig::default(),
                    &zeroclaw_config::schema::PacingConfig::default(),
                    0,
                    0,
                    None,
                    None, // agent_alias — no parent alias in the review-fork test fixture
                ),
            )
            .await;

        let after = tracker.get_summary().unwrap().request_count;
        assert_eq!(
            after, before,
            "budget-exceeded fork must not make a recorded provider call"
        );
    }

    // ── apply_policy_tool_filter coverage ─────────────────────
    //
    // The dispatch-site filter must consult both the parent agent's
    // SecurityPolicy.allowed_tools / .excluded_tools AND the
    // caller-supplied allowed_tools list, with both gates composing
    // by intersection. A tool name absent from either falls out.

    use zeroclaw_api::tool::Tool as TestTool;
    use zeroclaw_config::policy::SecurityPolicy as TestPolicy;

    struct NamedMockTool {
        the_name: &'static str,
    }

    #[async_trait]
    impl TestTool for NamedMockTool {
        fn name(&self) -> &str {
            self.the_name
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    fn mock_tool(name: &'static str) -> Box<dyn TestTool> {
        Box::new(NamedMockTool { the_name: name })
    }

    fn mock_tool_arc(name: &'static str) -> std::sync::Arc<dyn TestTool> {
        std::sync::Arc::new(NamedMockTool { the_name: name })
    }

    fn tool_names(tools: &[Box<dyn TestTool>]) -> Vec<&str> {
        tools.iter().map(|t| t.name()).collect()
    }

    #[test]
    fn apply_policy_tool_filter_no_gates_keeps_everything() {
        let mut tools = vec![
            mock_tool("shell"),
            mock_tool("spawn_subagent"),
            mock_tool("memory_recall"),
        ];
        super::apply_policy_tool_filter(&mut tools, None, None);
        assert_eq!(
            tool_names(&tools),
            vec!["shell", "spawn_subagent", "memory_recall"]
        );
    }

    #[test]
    fn apply_policy_tool_filter_policy_allowlist_restricts() {
        let mut tools = vec![
            mock_tool("shell"),
            mock_tool("spawn_subagent"),
            mock_tool("memory_recall"),
        ];
        let policy = TestPolicy {
            allowed_tools: Some(vec!["shell".into(), "memory_recall".into()]),
            ..TestPolicy::default()
        };

        super::apply_policy_tool_filter(&mut tools, Some(&policy), None);
        assert_eq!(tool_names(&tools), vec!["shell", "memory_recall"]);
    }

    #[test]
    fn apply_policy_tool_filter_policy_excluded_subtracts_from_unrestricted() {
        let mut tools = vec![mock_tool("shell"), mock_tool("spawn_subagent")];
        let policy = TestPolicy {
            excluded_tools: Some(vec!["spawn_subagent".into()]),
            ..TestPolicy::default()
        };

        super::apply_policy_tool_filter(&mut tools, Some(&policy), None);
        assert_eq!(tool_names(&tools), vec!["shell"]);
    }

    #[test]
    fn apply_policy_tool_filter_caller_filter_alone_restricts() {
        let mut tools = vec![
            mock_tool("shell"),
            mock_tool("spawn_subagent"),
            mock_tool("memory_recall"),
        ];
        let caller = vec!["memory_recall".to_string()];

        super::apply_policy_tool_filter(&mut tools, None, Some(&caller));
        assert_eq!(tool_names(&tools), vec!["memory_recall"]);
    }

    #[test]
    fn apply_policy_tool_filter_policy_and_caller_intersect() {
        let mut tools = vec![
            mock_tool("shell"),
            mock_tool("spawn_subagent"),
            mock_tool("memory_recall"),
        ];
        let policy = TestPolicy {
            allowed_tools: Some(vec!["shell".into(), "memory_recall".into()]),
            ..TestPolicy::default()
        };
        let caller = vec!["shell".to_string(), "spawn_subagent".to_string()];

        super::apply_policy_tool_filter(&mut tools, Some(&policy), Some(&caller));
        // Only `shell` survives — it's the intersection of the policy
        // allowlist {shell, memory_recall} and the caller filter
        // {shell, spawn_subagent}.
        assert_eq!(tool_names(&tools), vec!["shell"]);
    }

    #[test]
    fn apply_policy_tool_filter_policy_deny_all_drops_everything() {
        let mut tools = vec![mock_tool("shell"), mock_tool("spawn_subagent")];
        let policy = TestPolicy {
            allowed_tools: Some(vec![]),
            ..TestPolicy::default()
        };

        super::apply_policy_tool_filter(&mut tools, Some(&policy), None);
        assert!(
            tools.is_empty(),
            "Some(vec![]) on policy must deny every tool"
        );
    }

    // ── capture_llm_messages tests ────────────────────────────────

    #[cfg(feature = "observability-otel")]
    #[test]
    fn capture_llm_messages_splits_system_scrubs_and_maps_output() {
        // scrub_credentials catches key=value; scrub_secret_patterns catches bare token
        // prefixes (ghp_, sk-, xoxb-). capture composes both via scrub_for_export, so
        // assert each field == scrub_for_export(raw) (robust regardless of exact regex).
        let sys_raw = "You are helpful. api_key=SUPERSECRETVALUE123";
        let user_raw = "deploy token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let messages = vec![
            ChatMessage::system(sys_raw),
            ChatMessage::user(user_raw),
            ChatMessage::assistant("earlier reply"),
        ];
        let tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: r#"{"cmd":"echo api_key=ANOTHERSECRET99"}"#.into(),
            extra_content: None,
        }];

        let snap = super::capture_llm_messages(&messages, Some("final answer"), &tool_calls)
            .expect("Some under observability-otel");

        // System split out and routed through the composed scrubber.
        assert_eq!(
            snap.system_instructions.as_deref(),
            Some(super::scrub_for_export(sys_raw).as_str())
        );
        assert_ne!(snap.system_instructions.as_deref(), Some(sys_raw)); // proves scrubbing ran

        // input excludes system, preserves order; bare ghp_ token must be scrubbed.
        assert_eq!(snap.input.len(), 2);
        assert!(snap.input.iter().all(|m| m.role != "system"));
        assert_eq!(snap.input[0].role, "user");
        assert_eq!(snap.input[0].content, super::scrub_for_export(user_raw));
        assert_ne!(snap.input[0].content, user_raw); // proves the bare-prefix scrubber fired

        // output text + scrubbed tool-call arguments.
        assert_eq!(snap.output_text.as_deref(), Some("final answer"));
        assert_eq!(snap.output_tool_calls.len(), 1);
        assert_eq!(snap.output_tool_calls[0].name, "shell");
        assert_eq!(
            snap.output_tool_calls[0].arguments_json,
            super::scrub_for_export(r#"{"cmd":"echo api_key=ANOTHERSECRET99"}"#)
        );
        assert!(
            !snap.output_tool_calls[0]
                .arguments_json
                .contains("ANOTHERSECRET99")
        );
    }

    #[cfg(feature = "observability-otel")]
    #[test]
    fn capture_llm_messages_elides_image_data_uris() {
        let raw = "see [IMAGE:data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB] here";
        let messages = vec![ChatMessage::user(raw)];
        let snap = super::capture_llm_messages(&messages, None, &[]).expect("Some");
        let content = &snap.input[0].content;
        assert!(
            !content.contains("base64,iVBOR"),
            "image bytes not elided: {content}"
        );
        assert!(
            content.contains("[IMAGE:<image data elided>]"),
            "placeholder missing: {content}"
        );
    }

    #[cfg(feature = "observability-otel")]
    #[test]
    fn capture_llm_messages_empty_output_and_no_system() {
        let messages = vec![ChatMessage::user("hi")];
        let snap = super::capture_llm_messages(&messages, Some(""), &[]).expect("Some");
        assert_eq!(snap.system_instructions, None);
        assert_eq!(snap.output_text, None); // empty string captured as None
        assert!(snap.output_tool_calls.is_empty());
        assert_eq!(snap.input.len(), 1);
    }

    #[test]
    fn eager_mcp_policy_allows_only_names_that_pass_policy_and_caller_gates() {
        let policy = TestPolicy {
            allowed_tools: Some(vec!["fs__read_file".into(), "slack__post".into()]),
            excluded_tools: Some(vec!["slack__post".into()]),
            ..TestPolicy::default()
        };
        let caller = vec!["fs__read_file".to_string(), "github__search".to_string()];
        let access_policy = super::mcp_tool_access_policy(&policy, Some(&caller));

        assert!(
            super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
            "name admitted by both policy and caller gates must be registered eagerly"
        );
        assert!(
            !super::eager_mcp_tool_allowed("slack__post", access_policy.as_ref()),
            "policy excluded_tools must block eager MCP registration"
        );
        // `github__search` is in the caller list AND its `__` prefix triggers
        // the risk-profile MCP auto-admit, so both independent gates pass it.
        assert!(
            super::eager_mcp_tool_allowed("github__search", access_policy.as_ref()),
            "name auto-admitted by risk-profile MCP exception and listed by \
             the caller must be registered eagerly"
        );
    }

    /// PR #7547 review (Audacity88 / singlerider) — second-round blocking:
    /// the MCP `<server>__<tool>` auto-admit exception must apply only to
    /// the risk-profile gate. A caller-supplied per-run `allowed_tools`
    /// list (cron job, narrowed delegate invocation, …) must still narrow
    /// the visible set strictly, even when the risk-profile auto-admit
    /// would otherwise pass an MCP name.
    ///
    /// Concrete scenario from the review: a cron job narrows
    /// `allowed_tools = ["cron_add"]`. The risk profile is broader and
    /// includes `cron_add` (so the cron tool itself survives both gates),
    /// but the risk-profile MCP exception would happily admit
    /// `filesystem__write_file` — the caller list does not include it,
    /// so eager MCP registration must reject it.
    #[test]
    fn eager_mcp_policy_caller_per_run_list_does_not_leak_mcp_auto_admit() {
        // Risk profile is permissive enough that the MCP auto-admit
        // fires on its own gate (a non-empty list activates the `__`
        // exception). It also covers `cron_add` so the per-run narrowing
        // can pin that tool through both gates.
        let policy = TestPolicy {
            allowed_tools: Some(vec!["cron_add".into(), "fs__read_file".into()]),
            ..TestPolicy::default()
        };
        // Caller-supplied per-run list narrows to a single non-MCP tool.
        let caller = vec!["cron_add".to_string()];
        let access_policy = super::mcp_tool_access_policy(&policy, Some(&caller));

        assert!(
            super::eager_mcp_tool_allowed("cron_add", access_policy.as_ref()),
            "explicitly-narrowed tool must be admitted by both gates"
        );
        assert!(
            !super::eager_mcp_tool_allowed("filesystem__write_file", access_policy.as_ref()),
            "MCP wrapper outside the caller-supplied per-run list must be \
             rejected — the per-run gate does not honor the risk-profile \
             MCP auto-admit exception (PR #7547 review regression)"
        );
        assert!(
            !super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
            "even risk-profile-listed MCP names are rejected when the \
             caller-supplied per-run list does not include them"
        );
    }

    /// Same scenario as above, but proving the deferred (`tool_search`)
    /// path also honors the per-run narrowing. `mcp_allowed_tool_count`
    /// is what guards the `tool_search` registration: if it returns 0
    /// the deferred-MCP `tool_search` tool is not even added to the
    /// agent's tool list, so the LLM cannot ask for the MCP wrapper.
    #[test]
    fn deferred_mcp_caller_per_run_list_does_not_leak_mcp_auto_admit() {
        let policy = TestPolicy {
            allowed_tools: Some(vec!["cron_add".into(), "fs__read_file".into()]),
            ..TestPolicy::default()
        };
        let caller = vec!["cron_add".to_string()];
        let access_policy = super::mcp_tool_access_policy(&policy, Some(&caller));

        // The two stubs are MCP-shaped wrappers that the risk-profile
        // auto-admit would otherwise pass, but the caller-supplied
        // per-run list does not include either of them.
        assert_eq!(
            super::mcp_allowed_tool_count(
                ["filesystem__write_file", "github__search"],
                access_policy.as_ref()
            ),
            0,
            "deferred MCP `tool_search` must not be registered when the \
             caller-supplied per-run list admits no MCP stub (PR #7547 \
             review regression)"
        );
    }

    #[test]
    fn eager_mcp_policy_uses_security_policy_without_caller_gate_on_process_message() {
        let policy = TestPolicy {
            allowed_tools: Some(vec!["fs__read_file".into()]),
            ..TestPolicy::default()
        };
        let access_policy = super::mcp_tool_access_policy(&policy, None);

        assert!(
            super::eager_mcp_tool_allowed("fs__read_file", access_policy.as_ref()),
            "process_message eager MCP should use the agent SecurityPolicy allowlist"
        );
        // github__search contains "__" → auto-admitted even though not in allowed_tools
        assert!(
            super::eager_mcp_tool_allowed("github__search", access_policy.as_ref()),
            "runtime-discovered MCP tools are auto-admitted (subject only to excluded_tools)"
        );
    }

    #[test]
    fn deferred_mcp_allowed_count_honors_deny_all_policy() {
        let policy = TestPolicy {
            allowed_tools: Some(vec![]),
            ..TestPolicy::default()
        };
        let access_policy = super::mcp_tool_access_policy(&policy, None);

        assert_eq!(
            super::mcp_allowed_tool_count(
                ["fs__read_file", "github__search"],
                access_policy.as_ref()
            ),
            0,
            "deferred MCP must not register tool_search when policy admits no MCP stubs"
        );
    }

    #[test]
    fn pinned_section_survives_deferred_loading_reassignment() {
        // Regression for the loop_.rs clobber bug (PR #8508 review): the
        // pinned-resources block must reach the prompt even under
        // `deferred_loading = true`, where the deferred branch reassigns
        // `deferred_section` with `=`. The fix appends pins AFTER that branch
        // via `append_pinned_mcp_section`; this test pins that ordering.
        let pinned = "## Pinned MCP Resources\n\n\
            <mcp-resource server=\"docs\" uri=\"docs__file:///handbook.md\" \
            mime=\"text/plain\" trust=\"untrusted-external\">\nhandbook body\n</mcp-resource>\n";

        // Emulate the production statement order for the deferred path.
        // (build_pinned_resources_section result is captured first in `run()`)
        let pinned_section = pinned.to_string();
        // deferred_loading == true branch reassigns the section (as
        // build_deferred_tools_section_filtered does), dropping prior content.
        let mut deferred_section = "## Deferred MCP Tools\n\n- mcp__example".to_string();
        // The fix: append pins AFTER the branch.
        super::append_pinned_mcp_section(&mut deferred_section, &pinned_section);

        assert!(
            deferred_section.contains("## Pinned MCP Resources"),
            "pinned section must survive the deferred-loading reassignment, got: {deferred_section}"
        );
        assert!(
            deferred_section.contains("trust=\"untrusted-external\""),
            "provenance-wrapped pinned content must reach the prompt under deferred_loading"
        );
        assert!(
            deferred_section.contains("## Deferred MCP Tools"),
            "the deferred tools section must also remain present"
        );
    }

    #[test]
    fn append_pinned_mcp_section_is_noop_for_empty() {
        let mut section = "## Deferred MCP Tools".to_string();
        super::append_pinned_mcp_section(&mut section, "");
        assert_eq!(
            section, "## Deferred MCP Tools",
            "empty pinned section must not alter the accumulator"
        );
    }

    #[test]
    fn register_eager_mcp_tool_filters_tools_and_delegate_handle_together() {
        let policy = TestPolicy {
            allowed_tools: Some(vec!["fs__read_file".into()]),
            excluded_tools: Some(vec!["slack__post".into()]),
            ..TestPolicy::default()
        };
        let access_policy = super::mcp_tool_access_policy(&policy, None);
        let delegate_handle: crate::tools::DelegateParentToolsHandle =
            std::sync::Arc::new(parking_lot::RwLock::new(Vec::new()));
        let mut tools: Vec<Box<dyn TestTool>> = Vec::new();

        assert!(super::register_eager_mcp_tool_if_allowed(
            mock_tool_arc("fs__read_file"),
            &mut tools,
            Some(&delegate_handle),
            access_policy.as_ref(),
        ));
        // github__search contains "__" → auto-admitted
        assert!(super::register_eager_mcp_tool_if_allowed(
            mock_tool_arc("github__search"),
            &mut tools,
            Some(&delegate_handle),
            access_policy.as_ref(),
        ));
        // slack__post is explicitly excluded → denied
        assert!(!super::register_eager_mcp_tool_if_allowed(
            mock_tool_arc("slack__post"),
            &mut tools,
            Some(&delegate_handle),
            access_policy.as_ref(),
        ));

        assert_eq!(tool_names(&tools), vec!["fs__read_file", "github__search"]);
        let delegate_names: Vec<String> = delegate_handle
            .read()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        assert_eq!(delegate_names, vec!["fs__read_file", "github__search"]);
    }

    // ── agent_provider_composite regression ───────────────────────────────

    #[test]
    fn agent_provider_composite_returns_dotted_ref_not_bare_family() {
        use zeroclaw_config::providers::ModelProviderRef;
        use zeroclaw_config::schema::{
            AliasedAgentConfig, ModelProviderConfig, OpenAIModelProviderConfig,
        };

        let alias = "qwertfoozp";

        let mut config = zeroclaw_config::schema::Config::default();
        config.providers.models.openai.insert(
            alias.to_string(),
            OpenAIModelProviderConfig {
                base: ModelProviderConfig {
                    requires_openai_auth: true,
                    ..Default::default()
                },
            },
        );
        config.agents.insert(
            "my_agent".to_string(),
            AliasedAgentConfig {
                model_provider: ModelProviderRef::new(format!("openai.{alias}")),
                ..Default::default()
            },
        );

        let result = super::agent_provider_composite(&config, "my_agent");

        // Must be the full dotted ref so the alias-aware factory path is taken.
        assert_eq!(
            result.as_deref(),
            Some("openai.qwertfoozp"),
            "agent_provider_composite must return the dotted composite ref"
        );
        // Explicitly assert it is NOT the bare family — this is the regression
        // this test protects against.
        assert_ne!(
            result.as_deref(),
            Some("openai"),
            "bare family name would bypass the alias-aware factory path and drop \
             requires_openai_auth from the config, routing to the wrong provider"
        );
    }

    #[test]
    fn direct_turn_agent_config_resolves_runtime_profile_tunables() {
        use zeroclaw_config::providers::RuntimeProfileRef;
        use zeroclaw_config::schema::{AliasedAgentConfig, RuntimeProfileConfig};

        let mut config = zeroclaw_config::schema::Config::default();
        config.runtime_profiles.insert(
            "long".to_string(),
            RuntimeProfileConfig {
                max_tool_iterations: 25,
                ..Default::default()
            },
        );
        config.agents.insert(
            "default".to_string(),
            AliasedAgentConfig {
                runtime_profile: RuntimeProfileRef::new("long"),
                ..Default::default()
            },
        );

        let agent = super::resolved_agent_for_turn(&config, "default")
            .expect("configured agent should resolve for direct turns");

        assert_eq!(
            agent.resolved.max_tool_iterations, 25,
            "direct agent turns must honor runtime_profiles.*.max_tool_iterations \
             instead of the skipped AliasedAgentConfig::resolved default"
        );
    }

    #[tokio::test]
    async fn runtime_entrypoints_resolve_runtime_profile_tunables_before_provider_setup() {
        use zeroclaw_config::providers::RuntimeProfileRef;
        use zeroclaw_config::schema::{AliasedAgentConfig, RuntimeProfileConfig};

        let mut config = zeroclaw_config::schema::Config::default();
        config.runtime_profiles.insert(
            "entrypoint-profile".to_string(),
            RuntimeProfileConfig {
                max_tool_iterations: 3,
                ..Default::default()
            },
        );
        config.agents.insert(
            "entrypoint-profile-agent".to_string(),
            AliasedAgentConfig {
                runtime_profile: RuntimeProfileRef::new("entrypoint-profile"),
                ..Default::default()
            },
        );

        let seen = Arc::new(Mutex::new(Vec::<(String, usize)>::new()));
        let seen_for_hook = Arc::clone(&seen);
        {
            let mut hook = RESOLVED_AGENT_FOR_TURN_TEST_HOOK
                .lock()
                .expect("resolved-agent test hook lock should not be poisoned");
            *hook = Some(Arc::new(move |alias, max_tool_iterations| {
                seen_for_hook
                    .lock()
                    .expect("seen lock should not be poisoned")
                    .push((alias.to_string(), max_tool_iterations));
            }));
        }

        let _ = super::run(
            config.clone(),
            "entrypoint-profile-agent",
            Some("hello".to_string()),
            None,
            None,
            None,
            Vec::new(),
            false,
            None,
            None,
            super::AgentRunOverrides::default(),
        )
        .await;
        let _ =
            super::process_message(config, "entrypoint-profile-agent", "hello", Some("session"))
                .await;

        {
            let mut hook = RESOLVED_AGENT_FOR_TURN_TEST_HOOK
                .lock()
                .expect("resolved-agent test hook lock should not be poisoned");
            *hook = None;
        }

        let seen = seen.lock().expect("seen lock should not be poisoned");
        let matching = seen
            .iter()
            .filter(|(alias, max_tool_iterations)| {
                alias == "entrypoint-profile-agent" && *max_tool_iterations == 3
            })
            .count();

        assert_eq!(
            matching, 2,
            "run and process_message must both resolve runtime_profiles.*.max_tool_iterations \
             before provider setup; observed {seen:?}"
        );
    }

    /// Characterization of the unified `process_message` built-in filter
    /// (replaces the deleted `filter_channel_builtin_tools` safe-defaults tests,
    /// #6959). `process_message` now routes its real eager registry through
    /// `ScopedToolRegistry::assemble` with `caller_allowed: None` - the plain
    /// `allowed_tools`/`excluded_tools` policy filter. At non-Full autonomy a
    /// canonical read-only default (`web_search_tool`) that is NOT in a
    /// restrictive `allowed_tools` is now DROPPED, where the removed admit
    /// retained it. This positively pins the narrowing this path lands: on the
    /// gateway-chat and peer paths `allowed_tools` is honored strictly.
    #[tokio::test]
    async fn process_message_seam_narrows_safe_defaults_outside_allowed_tools() {
        let config = zeroclaw_config::schema::Config::default();
        let security = Arc::new(TestPolicy {
            workspace_dir: std::env::temp_dir(),
            ..TestPolicy::default()
        });
        let risk = zeroclaw_config::schema::RiskProfileConfig::default();
        let mem: Arc<dyn zeroclaw_memory::Memory> =
            Arc::new(zeroclaw_memory::NoneMemory::new("test"));

        // Build the real eager built-in registry, as process_message does.
        let built = crate::tools::all_tools(
            Arc::new(config.clone()),
            &security,
            &risk,
            "test",
            mem,
            None,
            None,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &security.workspace_dir,
            &config.agents,
            None,
            &config,
            None,
            false,
            None,
        );

        let before = tool_names(&built.tools);
        assert!(
            before.contains(&"web_search_tool"),
            "precondition: web_search_tool in the eager registry, got {before:?}"
        );
        assert!(
            before.contains(&"shell"),
            "precondition: shell in the eager registry, got {before:?}"
        );

        // Restrict allowed_tools to shell only, at non-Full autonomy - the exact
        // scenario where the removed admit diverged from the plain filter.
        let policy = Arc::new(TestPolicy {
            workspace_dir: std::env::temp_dir(),
            allowed_tools: Some(vec!["shell".into()]),
            autonomy: crate::security::AutonomyLevel::Supervised,
            ..TestPolicy::default()
        });

        let assembled = crate::tools::scoped::ScopedToolRegistry::assemble(
            crate::tools::scoped::ScopedAssembly {
                config: &config,
                agent_alias: "test",
                security: &policy,
                built,
                skills: &[],
                runtime: Arc::new(crate::platform::NativeRuntime::new()),
                caller_allowed: None, // process_message has no caller allowlist
                connect_mcp: false,   // exercise the filter without MCP fixtures
                connect_peripherals: false,
                exclude_memory: false,
                emit_assembly_logs: false,
            },
        )
        .await;

        let filtered: Vec<&str> = assembled.registry.iter().map(|t| t.name()).collect();
        assert!(
            !filtered.contains(&"web_search_tool"),
            "unified filter must DROP a read-only default outside allowed_tools \
             (the removed safe-defaults admit retained it), got {filtered:?}"
        );
        assert!(
            filtered.contains(&"shell"),
            "shell in allowed_tools must survive, got {filtered:?}"
        );
    }

    // ── Observer metadata regression tests ──

    #[derive(Default)]
    struct CapturingObserver {
        events: parking_lot::Mutex<Vec<ObserverEvent>>,
    }

    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            self.events.lock().push(event.clone());
        }
        fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}
        fn name(&self) -> &str {
            "capturing"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn flush(&self) {}
    }

    fn assert_all_events_share_turn_id(
        events: &[ObserverEvent],
        expected_alias: Option<&str>,
        expected_channel: Option<&str>,
    ) {
        // Regression guard for PR #7771: every lifecycle observer event that
        // carries the `(channel, agent_alias, turn_id)` correlation triple
        // MUST populate all three. These six variants back OTel parent-child
        // span linkage and per-agent attribution; a `None` in any field
        // silently breaks trace correlation. The original `run()` entry point
        // emitted `AgentStart`/`AgentEnd` with `agent_alias: None` /
        // `turn_id: None` because it recorded events outside the turn engine.
        //
        // This checks each event individually (no skipping
        // `AgentStart`/`AgentEnd` aliases) and forbids `None` outright rather
        // than only checking consistency among the non-`None` subset — a
        // single `None` field is a failure, not something to be filtered out.
        let mut turn_ids: Vec<String> = Vec::new();
        for event in events {
            let (variant, channel, agent_alias, turn_id) = match event {
                ObserverEvent::AgentStart {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("AgentStart", channel, agent_alias, turn_id),
                ObserverEvent::AgentEnd {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("AgentEnd", channel, agent_alias, turn_id),
                ObserverEvent::LlmRequest {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("LlmRequest", channel, agent_alias, turn_id),
                ObserverEvent::LlmResponse {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("LlmResponse", channel, agent_alias, turn_id),
                ObserverEvent::ToolCallStart {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("ToolCallStart", channel, agent_alias, turn_id),
                ObserverEvent::ToolCall {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("ToolCall", channel, agent_alias, turn_id),
                _ => continue,
            };
            assert!(
                channel.is_some(),
                "{variant} observer event must carry channel, got None: {event:?}"
            );
            assert!(
                agent_alias.is_some(),
                "{variant} observer event must carry agent_alias, got None: {event:?}"
            );
            assert!(
                turn_id.is_some(),
                "{variant} observer event must carry turn_id, got None: {event:?}"
            );
            turn_ids.push(turn_id.clone().expect("checked Some above"));
        }

        assert!(!turn_ids.is_empty(), "expected turn events with turn_id");
        let first = &turn_ids[0];
        assert!(
            turn_ids.iter().all(|id| id == first),
            "all turn_ids should be consistent"
        );

        if let Some(alias) = expected_alias {
            for e in events {
                let agent_alias = match e {
                    ObserverEvent::AgentStart { agent_alias, .. }
                    | ObserverEvent::AgentEnd { agent_alias, .. }
                    | ObserverEvent::LlmRequest { agent_alias, .. }
                    | ObserverEvent::LlmResponse { agent_alias, .. }
                    | ObserverEvent::ToolCallStart { agent_alias, .. }
                    | ObserverEvent::ToolCall { agent_alias, .. } => agent_alias,
                    _ => continue,
                };
                assert_eq!(
                    agent_alias.as_deref(),
                    Some(alias),
                    "agent_alias should be consistent"
                );
            }
        }

        if let Some(channel) = expected_channel {
            for e in events {
                let ch = match e {
                    ObserverEvent::AgentStart { channel: ch, .. }
                    | ObserverEvent::LlmRequest { channel: ch, .. }
                    | ObserverEvent::LlmResponse { channel: ch, .. }
                    | ObserverEvent::ToolCallStart { channel: ch, .. }
                    | ObserverEvent::ToolCall { channel: ch, .. }
                    | ObserverEvent::AgentEnd { channel: ch, .. } => ch,
                    _ => continue,
                };
                assert_eq!(ch.as_deref(), Some(channel), "channel should be consistent");
            }
        }
    }

    #[tokio::test]
    async fn run_tool_call_loop_events_share_consistent_turn_id_channel_and_alias() {
        use super::run_tool_call_loop;

        let turn_id = uuid::Uuid::new_v4().to_string();
        let invocations = Arc::new(AtomicUsize::new(0));
        let model_provider = ScriptedModelProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"X"}}
</tool_call>"#,
            "done",
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut history = vec![ChatMessage::system("test"), ChatMessage::user("hello")];

        let result = run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: &model_provider,
                    provider_name: "mock-provider",
                    model: "mock-model",
                    temperature: Some(0.0),
                },
                tools_registry: &tools_registry,
                observer: observer.as_ref(),
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                max_tool_iterations: 10,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_token_budget: 0,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: IngressContext::internal(),
            agent_alias: Some("test-agent"),
            turn_id: &turn_id,
        })
        .await
        .expect("tool loop should succeed");

        assert_eq!(result, "done");

        let events = capturing.events.lock();
        assert_all_events_share_turn_id(&events, Some("test-agent"), Some("cli"));
    }

    /// `read_capped_line` returns a full line and [`CappedLine::Line`]
    /// when the input is under the cap. EOF with no bytes returns
    /// [`CappedLine::Eof`]. Regression guard for the happy path.
    #[test]
    fn read_capped_line_returns_full_line_under_cap() {
        let mut cursor = std::io::Cursor::new(b"hello\n".to_vec());
        match read_capped_line(&mut cursor, 1024).unwrap() {
            CappedLine::Line(line) => assert_eq!(line, "hello"),
            other => panic!("expected Line, got {other:?}"),
        }

        // EOF without any bytes.
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        assert!(matches!(
            read_capped_line(&mut cursor, 1024).unwrap(),
            CappedLine::Eof
        ));

        // EOF with bytes but no trailing newline.
        let mut cursor = std::io::Cursor::new(b"no newline at eof".to_vec());
        match read_capped_line(&mut cursor, 1024).unwrap() {
            CappedLine::Line(line) => assert_eq!(line, "no newline at eof"),
            other => panic!("expected Line, got {other:?}"),
        }
    }

    /// `read_capped_line` returns [`CappedLine::Truncated`] for a line
    /// that exceeds the cap, regardless of whether a trailing newline is
    /// present. This is the property that prevents a
    /// `head -c 10G | zeroclaw chat` pipe from blowing up RSS.
    #[test]
    fn read_capped_line_truncates_at_cap() {
        let cap = 64usize;

        // A line that exceeds the cap and ends with a newline.
        let mut input = vec![b'a'; cap + 100];
        input.push(b'\n');
        assert!(matches!(
            read_capped_line(&mut std::io::Cursor::new(input), cap).unwrap(),
            CappedLine::Truncated
        ));

        // A line that exceeds the cap with no newline.
        let input = vec![b'a'; cap + 100];
        assert!(matches!(
            read_capped_line(&mut std::io::Cursor::new(input), cap).unwrap(),
            CappedLine::Truncated
        ));
    }

    /// `read_capped_line` correctly handles a cap that lands mid-UTF-8
    /// codepoint. The important invariant is that the call does not panic
    /// and reports [`CappedLine::Truncated`] instead of returning a
    /// partial, invalid string.
    #[test]
    fn read_capped_line_truncates_inside_multibyte_chars_safely() {
        // "🦀" is 4 bytes; cap of 5 will land mid-codepoint.
        let cap = 5usize;
        let input = "🦀".repeat(10);
        let bytes = input.as_bytes();
        assert!(matches!(
            read_capped_line(&mut std::io::Cursor::new(bytes.to_vec()), cap).unwrap(),
            CappedLine::Truncated
        ));
    }

    /// `read_capped_line` returns the full input unchanged when the
    /// input is exactly the cap. This pins the off-by-one boundary:
    /// the +1 headroom in the `take(cap + 1)` wrapper means a buffer
    /// that fills exactly to `cap` bytes is NOT considered truncated.
    #[test]
    fn read_capped_line_at_exact_cap_is_not_truncated() {
        let cap = 8usize;
        let input: Vec<u8> = vec![b'x'; cap];
        match read_capped_line(&mut std::io::Cursor::new(input), cap).unwrap() {
            CappedLine::Line(line) => {
                assert_eq!(line.len(), cap);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    /// Regression for #8463 (Audacity88 review): after truncating an
    /// oversized line, the rest of the line is drained so the next call
    /// starts at the next line instead of chunking the same oversized
    /// line into repeated prompts.
    #[test]
    fn read_capped_line_drains_truncated_line_remainder() {
        let cap = 8usize;
        // One oversized line + one normal line.
        let oversized_line = vec![b'a'; cap * 3];
        let second_line = b"short\n";
        let mut input: Vec<u8> = Vec::new();
        input.extend_from_slice(&oversized_line);
        input.push(b'\n');
        input.extend_from_slice(second_line);

        let mut cursor = std::io::Cursor::new(input);

        // First call: should cap and drain the rest of the line.
        assert!(matches!(
            read_capped_line(&mut cursor, cap).unwrap(),
            CappedLine::Truncated
        ));

        // Second call: should see the second line, not more 'a's.
        match read_capped_line(&mut cursor, cap).unwrap() {
            CappedLine::Line(line) => assert_eq!(line, "short"),
            other => panic!("expected Line, got {other:?}"),
        }
    }

    /// After truncation, the next line is still readable even when it
    /// is close to the cap (boundary condition on the drain).
    #[test]
    fn read_capped_line_drain_does_not_eat_next_line() {
        let cap = 8usize;
        // Oversized line (no trailing \n in mid-data), then a line
        // right up to the cap.
        let oversized = vec![b'x'; cap * 2];
        let second: Vec<u8> = vec![b'y'; cap]; // exactly 'cap' bytes, no \n
        let mut input: Vec<u8> = Vec::new();
        input.extend_from_slice(&oversized);
        input.push(b'\n');
        input.extend_from_slice(&second);
        // The second line has no trailing \n — it is the last line
        // before EOF.

        let mut cursor = std::io::Cursor::new(input);

        assert!(matches!(
            read_capped_line(&mut cursor, cap).unwrap(),
            CappedLine::Truncated
        ));

        match read_capped_line(&mut cursor, cap).unwrap() {
            CappedLine::Line(line) => {
                assert_eq!(line.len(), second.len());
                assert_eq!(line.as_bytes(), &second);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    /// Regression for #8463: the bounded drain must not allocate an
    /// unbounded buffer. We verify this indirectly by ensuring that a
    /// line much larger than the cap is discarded and the next line is
    /// still readable.
    #[test]
    fn read_capped_line_bounded_drain_preserves_next_line() {
        let cap = 64usize;
        let oversized = vec![b'z'; cap * 100];
        let next = b"next-line\n";
        let mut input: Vec<u8> = Vec::new();
        input.extend_from_slice(&oversized);
        input.push(b'\n');
        input.extend_from_slice(next);

        let mut cursor = std::io::Cursor::new(input);

        assert!(matches!(
            read_capped_line(&mut cursor, cap).unwrap(),
            CappedLine::Truncated
        ));

        match read_capped_line(&mut cursor, cap).unwrap() {
            CappedLine::Line(line) => assert_eq!(line, "next-line"),
            other => panic!("expected Line, got {other:?}"),
        }
    }
}
