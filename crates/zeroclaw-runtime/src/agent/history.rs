use crate::agent::history_pruner::remove_orphaned_tool_messages;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use zeroclaw_providers::ChatMessage;
use zeroclaw_providers::multimodal::AttachmentDisposition;
use zeroclaw_providers::multimodal::attachment_dispositions;
use zeroclaw_providers::multimodal::message_image_summary;

/// Default complete-turn retention limit. Prefer passing the config-driven
/// value via `run_tool_call_loop`; this constant is only used when callers omit
/// the parameter. The name is retained for config compatibility.
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

/// Returns the largest UTF-8 character boundary at or before `index`.
///
/// This compatibility wrapper preserves the previously exported helper while
/// directing new callers to the standard-library implementation.
#[deprecated(since = "0.8.4", note = "use str::floor_char_boundary instead")]
pub fn floor_char_boundary(s: &str, index: usize) -> usize {
    // Keep downstream callers source-compatible without retaining duplicate boundary logic.
    s.floor_char_boundary(index)
}

/// Output plus byte-accurate measurements from one tool-result truncation.
pub(crate) struct ToolResultTruncation {
    pub(crate) output: String,
    pub(crate) original_bytes: usize,
    pub(crate) retained_bytes: usize,
    pub(crate) elided_bytes: usize,
}

impl ToolResultTruncation {
    pub(crate) fn was_truncated(&self) -> bool {
        self.elided_bytes > 0
    }
}

/// Truncate a tool result and report the loss without retaining a second copy.
pub(crate) fn truncate_tool_result_with_metadata(
    output: &str,
    max_chars: usize,
) -> ToolResultTruncation {
    let original_bytes = output.len();
    if max_chars == 0 || output.len() <= max_chars {
        return ToolResultTruncation {
            output: output.to_string(),
            original_bytes,
            retained_bytes: original_bytes,
            elided_bytes: 0,
        };
    }
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars.saturating_sub(head_len);
    let head_end = output.floor_char_boundary(head_len);
    // ceil_char_boundary: find smallest byte index >= i on a char boundary
    let tail_start_raw = output.len().saturating_sub(tail_len);
    let tail_start = if tail_start_raw >= output.len() {
        output.len()
    } else {
        let mut pos = tail_start_raw;
        while pos < output.len() && !output.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    };

    // Guard against overlap when max_chars is very small
    if head_end >= tail_start {
        let retained_bytes = output.floor_char_boundary(max_chars);
        return ToolResultTruncation {
            output: output[..retained_bytes].to_string(),
            original_bytes,
            retained_bytes,
            elided_bytes: original_bytes.saturating_sub(retained_bytes),
        };
    }
    let elided_bytes = tail_start - head_end;
    let retained_bytes = original_bytes - elided_bytes;
    ToolResultTruncation {
        output: format!(
            "{}\n\n[... {} characters truncated ...]\n\n{}",
            &output[..head_end],
            elided_bytes,
            &output[tail_start..]
        ),
        original_bytes,
        retained_bytes,
        elided_bytes,
    }
}

pub fn truncate_tool_result(output: &str, max_chars: usize) -> String {
    truncate_tool_result_with_metadata(output, max_chars).output
}

/// Fixed per-image charge in the history estimate, applied to the images
/// preparation dispatches from structured user and tool-result attachments. Approximates the
/// standard-tier Anthropic maximum (1,568 tokens for an image
/// at the 1568px downscale). High-resolution tiers and some models bill more
/// (Anthropic high-res up to 4,784; GPT-4o-mini base 2,833; Qwen-VL ~4k per
/// A4 page): this is a heuristic for trimming, not a ceiling, and
/// provider-reported usage corrects it after the first successful response.
pub const IMAGE_TOKEN_ESTIMATE: usize = 1_600;

/// Estimate the token cost of a single message: the ~4 chars/token heuristic
/// plus ~4 framing tokens (role, delimiters). Images follow preparation
/// ([`message_image_summary`]): a tool-result carrier is charged for the
/// attachments it declared, with any attachment-looking syntax in its body counted as text,
/// and a user message is charged for its loadable structured image attachments.
/// The per-image charge applies only when preparation
/// dispatches the images ([`AttachmentDisposition::Normalized`]); stale
/// tool-result carriers are priced as their bytes (the same formula as
/// literal text; see the arm below), and system or assistant content
/// stays literal text. A message without loadable attachments keeps the
/// plain-text formula. Single-sourced so the history
/// and system-floor estimates stay in lock-step.
fn estimate_message_tokens(message: &ChatMessage, disposition: AttachmentDisposition) -> usize {
    let text_estimate = message.context_text_bytes().div_ceil(4) + 4;
    if disposition == AttachmentDisposition::Literal {
        return text_estimate;
    }
    // Attachment fields are independent of the message text.
    let summary = message_image_summary(message);
    match disposition {
        AttachmentDisposition::Normalized => {
            summary.text_bytes.div_ceil(4) + summary.image_refs * IMAGE_TOKEN_ESTIMATE + 4
        }
        // Stripped attachments are not sent; only the text is charged.
        AttachmentDisposition::Stripped => text_estimate,
        // Unreachable after the guard; keeps the arm total.
        AttachmentDisposition::Literal => text_estimate,
    }
}

/// Estimate token count for a message history using the ~4 chars/token
/// heuristic plus ~4 framing tokens per message. Images are charged per
/// image only where preparation dispatches them: structured attachments on
/// user turns and on the tool-result carriers of
/// the current user turn. Stale tool-result carriers are priced as their
/// bytes, and system or assistant content is priced
/// as text. Trim
/// probes estimate history suffixes that always retain the newest turn, so
/// the current turn's tool-result carriers carry the same disposition in
/// every probe as in the full history.
pub fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    let dispositions = attachment_dispositions(history);
    history
        .iter()
        .zip(dispositions)
        .map(|(message, disposition)| estimate_message_tokens(message, disposition))
        .sum()
}

/// Estimate the token cost of native tool definitions serialized into the
/// provider request, using the same ~4 chars/token heuristic as messages.
/// The OpenAI and compatible adapters serialize these schemas into the chat
/// request, so providers that include them in `input_tokens` report a
/// population of messages plus tool schemas, not messages alone.
pub fn estimate_tool_schema_tokens(specs: &[crate::tools::ToolSpec]) -> usize {
    specs
        .iter()
        .map(|spec| {
            let parameters_len =
                serde_json::to_string(&*spec.parameters).map_or(0, |serialized| serialized.len());
            (spec.name.len() + spec.description.len() + parameters_len).div_ceil(4) + 4
        })
        .sum()
}

pub fn estimate_system_floor_tokens(history: &[ChatMessage]) -> usize {
    // System content is always dispatched verbatim, so the floor always uses
    // the literal-text formula.
    history
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| estimate_message_tokens(m, AttachmentDisposition::Literal))
        .sum()
}

#[must_use]
pub fn context_floor_remediation(system_floor: usize, budget: usize) -> String {
    let floor_s = system_floor.to_string();
    let budget_s = budget.to_string();
    crate::i18n::get_required_cli_string_with_args(
        "history-trim-floor-exceeds-budget",
        &[("floor", floor_s.as_str()), ("budget", budget_s.as_str())],
    )
}

pub fn normalize_system_messages(history: &mut Vec<ChatMessage>) {
    let mut saw_system = false;
    let mut system_content = String::new();
    let mut non_system = Vec::with_capacity(history.len());

    for message in history.drain(..) {
        if message.role == "system" {
            saw_system = true;
            if !message.content.is_empty() {
                if !system_content.is_empty() {
                    system_content.push_str("\n\n");
                }
                system_content.push_str(&message.content);
            }
        } else {
            non_system.push(message);
        }
    }

    if saw_system && !system_content.is_empty() {
        history.push(ChatMessage::system(system_content));
    }
    history.extend(non_system);
}

pub fn append_or_merge_system_message(history: &mut Vec<ChatMessage>, content: impl Into<String>) {
    let content = content.into();
    if content.is_empty() {
        normalize_system_messages(history);
        return;
    }

    if let Some(system_message) = history.iter_mut().find(|message| message.role == "system") {
        if !system_message.content.is_empty() {
            system_message.content.push_str("\n\n");
        }
        system_message.content.push_str(&content);
    } else {
        history.insert(0, ChatMessage::system(content));
    }
    normalize_system_messages(history);
}

pub fn trim_history(
    history: &mut Vec<ChatMessage>,
    max_history_turns: usize,
    history_has_trim_breadcrumb: &mut bool,
) {
    let result = crate::agent::history_trim::trim_to_recent_turn_count(
        std::mem::take(history),
        max_history_turns,
        *history_has_trim_breadcrumb,
    );
    let crate::agent::history_trim::TurnCountTrimResult {
        history: trimmed_history,
        dropped_messages,
        dropped_turns,
        kept_turns,
        trimmed,
    } = result;
    let messages_before = trimmed_history.len().saturating_add(dropped_messages);
    *history = trimmed_history;
    if trimmed {
        remove_orphaned_tool_messages(history);
        normalize_system_messages(history);
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "messages_before": messages_before,
                    "messages_after": history.len(),
                    "dropped_messages": dropped_messages,
                    "dropped_turns": dropped_turns,
                    "kept_turns": kept_turns,
                    "max_history_turns": max_history_turns,
                })),
            "trim_history: dropped oldest whole turns"
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveSessionState {
    pub version: u32,
    pub history: Vec<ChatMessage>,
    #[serde(default)]
    pub history_has_trim_breadcrumb: bool,
}

impl InteractiveSessionState {
    pub fn from_history_with_crumb(history: &[ChatMessage], has_crumb: bool) -> Self {
        Self {
            version: 2,
            history: history.to_vec(),
            history_has_trim_breadcrumb: has_crumb,
        }
    }
}

pub fn load_interactive_session_history(
    path: &Path,
    system_prompt: &str,
) -> Result<Vec<ChatMessage>> {
    let (history, _) = load_interactive_session_history_with_crumb(path, system_prompt)?;
    Ok(history)
}

/// Canonical breadcrumb text used for trim markers. This is the English
/// string that has been stable across all locales; it is the locale-
/// independent anchor for legacy migration. Injected breadcrumbs are always
/// produced via `get_required_cli_string("history-trim-breadcrumb")`, which
/// currently resolves to this same English text in every locale. Checking
/// the canonical value makes restore independent of the current process
/// locale.
pub const HISTORY_TRIM_BREADCRUMB_CANONICAL: &str =
    "[earlier turns omitted to fit the context window]";

pub fn is_history_trim_breadcrumb_text(text: &str) -> bool {
    if text == HISTORY_TRIM_BREADCRUMB_CANONICAL {
        return true;
    }
    // Best-effort cross-locale coverage: if a future translation changes the
    // breadcrumb in a non-English locale, a legacy v1 file trimmed in that
    // locale will still be recognised after a restart with a different locale.
    // v2 files carry an explicit flag and never reach this path.
    text == crate::i18n::get_required_cli_string("history-trim-breadcrumb")
}

/// Load interactive history plus the persisted breadcrumb provenance. Legacy
/// files without the flag are migrated by inspecting the restored history:
/// if the first non-system message equals the breadcrumb string, the flag is
/// recovered as true. v2 records carry an explicit
/// `history_has_trim_breadcrumb` (true or false) so a genuine user message
/// that collides with the breadcrumb text keeps its real turn-boundary role.
/// Legacy v1 migration is locale-independent: it checks the canonical
/// English breadcrumb plus the current-locale string, covering both the
/// stable historical value and any future translated variant. A v1 file whose
/// first user turn genuinely equals the breadcrumb text cannot be
/// distinguished from a synthetic marker on its one-time migration; after the
/// next persist the file becomes v2 with the (mis)classified flag, which is
/// the documented one-time limitation for unmarked legacy state.
pub fn load_interactive_session_history_with_crumb(
    path: &Path,
    system_prompt: &str,
) -> Result<(Vec<ChatMessage>, bool)> {
    if !path.exists() {
        return Ok((vec![ChatMessage::system(system_prompt)], false));
    }

    let raw = std::fs::read_to_string(path)?;
    let mut state: InteractiveSessionState = serde_json::from_str(&raw)?;
    if state.history.is_empty() {
        state.history.push(ChatMessage::system(system_prompt));
    } else if state.history.first().map(|msg| msg.role.as_str()) != Some("system") {
        state.history.insert(0, ChatMessage::system(system_prompt));
    }
    normalize_system_messages(&mut state.history);
    if state.history.first().map(|msg| msg.role.as_str()) != Some("system") {
        state.history.insert(0, ChatMessage::system(system_prompt));
    }

    remove_orphaned_tool_messages(&mut state.history);

    // Migration: only legacy v1 files lack explicit provenance. v2 records
    // carry an explicit `history_has_trim_breadcrumb` (true or false) — a
    // fresh v2 session where the user legitimately sent the breadcrumb text
    // must not be reclassified as synthetic. Infer only when the persisted
    // version is < 2 and the flag is false.
    let mut has_crumb = state.history_has_trim_breadcrumb;
    if !has_crumb && state.version < 2 {
        let leading_system = state
            .history
            .iter()
            .take_while(|m| m.role == "system")
            .count();
        if let Some(first) = state.history.get(leading_system)
            && first.role == "user"
            && is_history_trim_breadcrumb_text(&first.content)
        {
            has_crumb = true;
        }
    }

    Ok((state.history, has_crumb))
}

pub fn save_interactive_session_history(path: &Path, history: &[ChatMessage]) -> Result<()> {
    save_interactive_session_history_with_crumb(path, history, false)
}

pub fn save_interactive_session_history_with_crumb(
    path: &Path,
    history: &[ChatMessage],
    has_crumb: bool,
) -> Result<()> {
    let lease = zeroclaw_infra::session_events::cli_state::CliStateGuard::acquire_legacy(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_string_pretty(&InteractiveSessionState::from_history_with_crumb(
        history, has_crumb,
    ))?;
    std::fs::write(lease.path(), payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the exported compatibility wrapper retains the legacy UTF-8 boundary contract.
    #[allow(deprecated)]
    #[test]
    fn floor_char_boundary_compatibility_wrapper_delegates_to_std() {
        let text = "abc😀def";

        assert_eq!(floor_char_boundary(text, 5), 3);
        assert_eq!(floor_char_boundary(text, usize::MAX), text.len());
    }

    #[test]
    fn estimate_system_floor_counts_only_system_messages() {
        let history = vec![
            ChatMessage::system("You are helpful."), // 16 chars -> 4 + 4 = 8
            ChatMessage::user("What is Rust?"),      // counted by history, not floor
            ChatMessage::assistant("A language."),   // counted by history, not floor
        ];
        // Floor = system message only; conversation turns are prunable.
        assert_eq!(estimate_system_floor_tokens(&history), 8);
        assert!(estimate_system_floor_tokens(&history) < estimate_history_tokens(&history));
    }

    #[test]
    fn estimate_system_floor_empty_and_no_system() {
        assert_eq!(estimate_system_floor_tokens(&[]), 0);
        let history = vec![ChatMessage::user("hi"), ChatMessage::assistant("yo")];
        assert_eq!(estimate_system_floor_tokens(&history), 0);
    }

    #[test]
    fn image_path_attachment_is_charged_per_image_not_per_byte() {
        // The short path must not reduce the image's estimated token cost.
        let message = ChatMessage::user("").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: "/tmp/a.png".to_string(),
            },
        ]);

        assert_eq!(
            estimate_history_tokens(&[message]),
            IMAGE_TOKEN_ESTIMATE + 4
        );
    }

    #[test]
    fn image_data_uri_attachment_is_charged_per_image_not_per_byte() {
        // ~600 KB of base64 would price at ~150k tokens under the text
        // heuristic, against ~1.5k billed.
        let payload = format!("data:image/png;base64,{}", "A".repeat(600_000));
        let message = ChatMessage::user("").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: payload,
            },
        ]);

        assert_eq!(
            estimate_history_tokens(&[message]),
            IMAGE_TOKEN_ESTIMATE + 4
        );
    }

    #[test]
    fn path_and_data_uri_forms_estimate_identically() {
        // Invariant 2: the same image costs the same however it is
        // referenced, so the raw-history estimate bounds the prepared
        // payload from above.
        let via_path = ChatMessage::user("").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: "/tmp/scene.png".to_string(),
            },
        ]);
        let payload = format!("data:image/png;base64,{}", "B".repeat(600_000));
        let via_data_uri = ChatMessage::user("").with_attachments(vec![
            zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: payload,
            },
        ]);

        let path_estimate = estimate_history_tokens(&[via_path]);
        assert_eq!(path_estimate, estimate_history_tokens(&[via_data_uri]));
        assert_eq!(path_estimate, IMAGE_TOKEN_ESTIMATE + 4);
    }

    #[test]
    fn placeholder_marker_stays_text() {
        // Literal marker syntax has no attachment semantics.
        for placeholder in ["[IMAGE:...]", "structured image attachments"] {
            let message = ChatMessage::user(placeholder);

            assert_eq!(
                estimate_history_tokens(&[message]),
                placeholder.len().div_ceil(4) + 4
            );
        }
    }

    #[test]
    fn system_and_assistant_markers_stay_text() {
        // System and assistant content is dispatched verbatim, so a 16 KB data
        // URI marker must estimate as its full text, not as one image.
        let payload = format!("data:image/png;base64,{}", "A".repeat(16_000));
        let system_marker = ChatMessage::system(format!("[IMAGE:{payload}]"));
        let system_len = system_marker.content.len();
        assert_eq!(
            estimate_history_tokens(&[system_marker]),
            system_len.div_ceil(4) + 4
        );

        let assistant_marker = ChatMessage::assistant("[IMAGE:/tmp/a.png]");
        let assistant_len = assistant_marker.content.len();
        assert_eq!(
            estimate_history_tokens(&[assistant_marker]),
            assistant_len.div_ceil(4) + 4
        );

        // Twenty short markers in one system message must stay far under the
        // default 32,000-token floor warning they used to trip.
        let twenty = (0..20)
            .map(|index| format!("[IMAGE:/tmp/s-{index}.png]"))
            .collect::<Vec<_>>()
            .join("\n");
        let system_history = vec![ChatMessage::system(twenty)];
        let floor = estimate_system_floor_tokens(&system_history);
        assert_eq!(floor, system_history[0].content.len().div_ceil(4) + 4);
        assert!(floor < 32_000);
    }

    #[test]
    fn placeholder_with_padding_is_byte_identical_to_master() {
        // Parsing trims the padding around a placeholder, but a message with
        // no loadable references keeps the plain-text formula.
        let padded = "    [IMAGE:...]    ";
        let message = ChatMessage::user(padded);

        assert_eq!(
            estimate_history_tokens(&[message]),
            padded.len().div_ceil(4) + 4
        );
    }

    #[test]
    fn estimate_tool_schema_tokens_counts_name_description_and_parameters() {
        let spec = crate::tools::ToolSpec::new(
            "search",
            "search the corpus",
            serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        );
        let tokens = estimate_tool_schema_tokens(&[spec]);
        assert!(tokens > 0, "a tool schema must contribute tokens");
        assert!(
            estimate_tool_schema_tokens(&[]) == 0,
            "no tools means no schema tokens"
        );
    }

    #[test]
    fn context_floor_remediation_names_budget_floor_and_runtime_profile_surface() {
        let msg = context_floor_remediation(2000, 100);
        // Names the resolved budget N the runtime actually used ...
        assert!(
            msg.contains("100"),
            "remediation must name the resolved budget: {msg}"
        );
        // ... and the measured system floor ...
        assert!(
            msg.contains("2000"),
            "remediation must name the system floor: {msg}"
        );
        // ... points at the config surface an operator can change ...
        assert!(
            msg.contains("[runtime_profiles"),
            "remediation must point at the runtime-profile surface: {msg}"
        );
        // ... and never at the inert agent-inline knob
        assert!(
            !msg.contains("agent.max_context_tokens"),
            "remediation must not reference the inert agent.max_context_tokens: {msg}"
        );
    }

    #[test]
    fn truncate_tool_result_treats_literal_media_syntax_as_plain_text() {
        let path = format!("/tmp/{}.png", "a".repeat(200));
        let marker = format!("[IMAGE:{path}]");
        let output = format!("prefix-text {marker} trailing-text padding-padding");

        let truncated = truncate_tool_result(&output, 80);

        assert!(
            truncated.contains("[... ") && truncated.contains("characters truncated ...]"),
            "expected truncation marker in output, got: {truncated}"
        );
        assert!(truncated.starts_with(&output[..80 * 2 / 3]));
        assert!(truncated.ends_with(&output[output.len() - (80 - 80 * 2 / 3)..]));
    }

    #[test]
    fn truncate_tool_result_retains_plain_text_tail_inside_literal_media_syntax() {
        // Marker placed near the end so tail_start (~max_chars / 3 from the
        // end) lands inside it.
        let path = format!("/tmp/{}.png", "b".repeat(200));
        let marker = format!("[IMAGE:{path}]");
        let output = format!("{} preamble-content-line {marker} ending", "x".repeat(400));

        let truncated = truncate_tool_result(&output, 90);

        assert!(truncated.ends_with(&output[output.len() - 30..]));
    }

    #[test]
    fn truncate_tool_result_keeps_complete_marker_in_head() {
        let marker = "[IMAGE:/tmp/short.png]";
        let output = format!("{marker} {}", "y".repeat(500));

        let truncated = truncate_tool_result(&output, 200);

        assert!(
            truncated.starts_with(marker),
            "expected head to retain full marker, got: {truncated}"
        );
    }

    #[test]
    fn truncation_metadata_reports_byte_accurate_loss() {
        let output = format!("{}{}", "é".repeat(40), "z".repeat(80));

        let result = truncate_tool_result_with_metadata(&output, 60);

        assert!(result.was_truncated());
        assert_eq!(result.original_bytes, output.len());
        assert_eq!(result.retained_bytes + result.elided_bytes, output.len());
        assert!(result.retained_bytes <= 60);
        assert!(result.output.is_char_boundary(result.output.len()));
    }

    #[test]
    fn truncation_metadata_reports_unchanged_results() {
        let result = truncate_tool_result_with_metadata("small result", 100);

        assert!(!result.was_truncated());
        assert_eq!(result.original_bytes, 12);
        assert_eq!(result.retained_bytes, 12);
        assert_eq!(result.elided_bytes, 0);
        assert_eq!(result.output, "small result");
    }
}
