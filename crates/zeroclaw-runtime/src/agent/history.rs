use crate::agent::history_pruner::remove_orphaned_tool_messages;
use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::LazyLock;
use zeroclaw_providers::ChatMessage;

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
/// Prefer passing the config-driven value via `run_tool_call_loop`; this constant is only
/// used when callers omit the parameter.
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

static LOCAL_IMAGE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:[A-Za-z]:[\\/]|\\\\[^\s<>'"`\]\)/\\]+[\\/]|/)[^\s<>'"`\]\)]+?\.(?i:png|jpe?g|webp|gif|bmp)"#,
    )
    .expect("valid image path regex")
});

/// Find the largest byte index `<= i` that is a valid char boundary.
/// MSRV-compatible replacement for `str::floor_char_boundary` (stable in 1.91).
pub fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut pos = i;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// Indicates which side of a truncated string a boundary belongs to when
/// nudging it away from a half-cut `[IMAGE:...]` marker.
#[derive(Clone, Copy)]
enum TruncationSide {
    /// Boundary is the end of the kept head; nudge backward (out of the marker).
    Head,
    /// Boundary is the start of the kept tail; nudge forward (out of the marker).
    Tail,
}

fn nudge_around_image_marker(s: &str, boundary: usize, side: TruncationSide) -> usize {
    const OPEN: &str = "[IMAGE:";
    if boundary == 0 || boundary >= s.len() {
        return boundary;
    }

    // Walk forward to find the most recent `[IMAGE:` whose `[` is strictly
    // before `boundary`. Searching forward (rather than `rfind` on a prefix)
    // correctly handles the case where `boundary` itself splits the literal
    // `[IMAGE:` token.
    let mut search_from = 0usize;
    let mut last_open: Option<usize> = None;
    while let Some(rel) = s[search_from..].find(OPEN) {
        let open_idx = search_from + rel;
        if open_idx >= boundary {
            break;
        }
        last_open = Some(open_idx);
        search_from = open_idx + OPEN.len();
    }
    let Some(open_idx) = last_open else {
        return boundary;
    };

    // First `]` after the opener closes the marker (canonicalize regex
    // forbids `]` inside paths, so this is unambiguous in practice).
    let close_idx = match s[open_idx..].find(']') {
        Some(rel) => open_idx + rel,
        None => return boundary, // malformed input — leave the boundary alone
    };

    if close_idx < boundary {
        return boundary; // marker fully closed before boundary — safe
    }

    match side {
        TruncationSide::Head => open_idx,
        TruncationSide::Tail => (close_idx + 1).min(s.len()),
    }
}

pub fn truncate_tool_result(output: &str, max_chars: usize) -> String {
    if max_chars == 0 || output.len() <= max_chars {
        return output.to_string();
    }
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars.saturating_sub(head_len);
    let head_end = floor_char_boundary(output, head_len);
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

    // Step boundaries away from any `[IMAGE:...]` marker they would bisect.
    // `[IMAGE:` and `]` are pure ASCII, so the adjusted indices land on
    // valid UTF-8 char boundaries.
    let head_end = nudge_around_image_marker(output, head_end, TruncationSide::Head);
    let tail_start = nudge_around_image_marker(output, tail_start, TruncationSide::Tail);

    // Guard against overlap when max_chars is very small
    if head_end >= tail_start {
        return output[..floor_char_boundary(output, max_chars)].to_string();
    }
    let truncated_chars = tail_start - head_end;
    format!(
        "{}\n\n[... {} characters truncated ...]\n\n{}",
        &output[..head_end],
        truncated_chars,
        &output[tail_start..]
    )
}

fn is_existing_local_image_path(path: &str) -> bool {
    let candidate = Path::new(path);
    candidate.is_absolute()
        && candidate.is_file()
        && candidate
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
                )
            })
}

fn existing_marker_payloads(output: &str) -> std::collections::HashSet<&str> {
    const OPEN: &str = "[IMAGE:";
    let mut set = std::collections::HashSet::new();
    let mut from = 0usize;
    while let Some(rel) = output[from..].find(OPEN) {
        let inner_start = from + rel + OPEN.len();
        let Some(rel_end) = output[inner_start..].find(']') else {
            break;
        };
        let inner_end = inner_start + rel_end;
        set.insert(output[inner_start..inner_end].trim());
        from = inner_end + 1;
    }
    set
}

/// Rewrite real local image file paths in tool output into `[IMAGE:...]`
/// markers so the multimodal pipeline can normalize them before the next
/// provider call. This targets shell/skill outputs that print filesystem
/// paths directly rather than returning explicit media markers.
pub fn canonicalize_tool_result_media_markers(output: &str) -> String {
    let existing_markers = existing_marker_payloads(output);
    let mut rewritten = String::with_capacity(output.len());
    let mut cursor = 0usize;
    let mut changed = false;

    for mat in LOCAL_IMAGE_PATH_RE.find_iter(output) {
        let start = mat.start();
        let end = mat.end();
        let path = &output[start..end];

        // Skip paths that are already part of an explicit media marker.
        if output[..start].ends_with("[IMAGE:") {
            continue;
        }

        // Skip a bare path that already appears inside an explicit marker
        // elsewhere in the same output — promoting it would double-count the
        // image (see `existing_marker_payloads`).
        if existing_markers.contains(path) {
            continue;
        }

        if !is_existing_local_image_path(path) {
            continue;
        }

        rewritten.push_str(&output[cursor..start]);
        rewritten.push_str("[IMAGE:");
        rewritten.push_str(path);
        rewritten.push(']');
        cursor = end;
        changed = true;
    }

    if !changed {
        return output.to_string();
    }

    rewritten.push_str(&output[cursor..]);
    rewritten
}

fn is_path_listing_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "content_search" | "glob_search"
    )
}

pub fn canonicalize_tool_result_media_markers_for(tool_name: &str, output: &str) -> String {
    if is_path_listing_tool(tool_name) {
        output.to_string()
    } else {
        canonicalize_tool_result_media_markers(output)
    }
}

/// Truncate a tool message's content, preserving JSON structure when the
/// message stores `tool_call_id` alongside `content` (native tool-call
/// format). Without this, `truncate_tool_result` destroys the JSON envelope
/// and downstream model_providers receive a `null` `call_id`.
pub fn truncate_tool_message(msg_content: &str, max_chars: usize) -> String {
    if max_chars == 0 || msg_content.len() <= max_chars {
        return msg_content.to_string();
    }
    if let Ok(mut obj) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(msg_content)
        && obj.contains_key("tool_call_id")
        && let Some(serde_json::Value::String(inner)) = obj.get("content")
    {
        let truncated = truncate_tool_result(inner, max_chars);
        obj.insert("content".to_string(), serde_json::Value::String(truncated));
        return serde_json::to_string(&obj).unwrap_or_else(|_| msg_content.to_string());
    }
    truncate_tool_result(msg_content, max_chars)
}

/// Conservative token estimate for one `[IMAGE:...]` marker.
///
/// The multimodal pipeline expands a marker into a base64 data URI at
/// send-time without downscaling (`multimodal::normalize_image_reference`), so
/// the provider sees roughly `bytes * 4 / 3` characters of payload. At the
/// shared ~4 chars/token heuristic that is `bytes / 3` tokens — orders of
/// magnitude more than the ~30-character marker string the text heuristic would
/// otherwise count.
///
/// Local files are sized from disk; already-inlined base64 data URIs from their
/// payload length. References we cannot size here (remote URLs, missing files)
/// fall back to a non-trivial constant so the estimate errs high rather than
/// reporting an image-heavy turn as nearly free.
fn estimate_image_marker_tokens(payload: &str) -> usize {
    const IMAGE_BYTES_PER_TOKEN: usize = 3;
    // Best-effort floor for references that cannot be sized locally; keeps an
    // unsizable image from being counted as free without wildly over-trimming.
    const UNSIZABLE_IMAGE_TOKENS: usize = 1_000;

    if let Some(base64_payload) = payload
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map(|(_, data)| data)
    {
        return base64_payload.len().div_ceil(4);
    }

    if payload.starts_with("http://") || payload.starts_with("https://") {
        return UNSIZABLE_IMAGE_TOKENS;
    }

    match std::fs::metadata(payload) {
        Ok(meta) => (meta.len() as usize).div_ceil(IMAGE_BYTES_PER_TOKEN),
        Err(_) => UNSIZABLE_IMAGE_TOKENS,
    }
}

/// Estimate the raw text token cost of a single message using the ~4
/// chars/token heuristic plus ~4 framing tokens (role, delimiters).
fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let (text, _) = zeroclaw_providers::multimodal::parse_image_markers(&message.content);
    text.len().div_ceil(4) + 4
}

/// Estimate one provider-ready message without charging inline image data as
/// both ordinary text and image payload. This must only be used after
/// multimodal preparation has removed stale/failed/capped image references.
fn estimate_prepared_message_tokens(message: &ChatMessage) -> usize {
    let (text, image_refs) = zeroclaw_providers::multimodal::parse_image_markers(&message.content);
    let text_tokens = text.len().div_ceil(4) + 4;
    text_tokens
        + image_refs
            .iter()
            .map(|payload| estimate_image_marker_tokens(payload))
            .sum::<usize>()
}

/// Estimate token count for a message history using ~4 chars/token heuristic.
/// Includes a small overhead per message for role/framing tokens.
pub fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    history.iter().map(estimate_message_tokens).sum()
}

/// Estimate the effective provider-ready history after multimodal preparation.
///
/// Image caps, age trimming, failed references, and stale tool-result images
/// must already have been applied by the caller.
pub fn estimate_prepared_history_tokens(history: &[ChatMessage]) -> usize {
    history.iter().map(estimate_prepared_message_tokens).sum()
}

pub fn estimate_system_floor_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .filter(|m| m.role == "system")
        .map(estimate_message_tokens)
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

/// Diagnostic for a single turn whose prepared multimodal payload alone
/// exceeds the context budget, so no amount of history trimming can make it
/// fit. Surfaced instead of dispatching a request the provider will reject.
#[must_use]
pub fn multimodal_budget_remediation(prepared_tokens: usize, budget: usize) -> String {
    let tokens_s = prepared_tokens.to_string();
    let budget_s = budget.to_string();
    crate::i18n::get_required_cli_string_with_args(
        "history-trim-multimodal-exceeds-budget",
        &[("tokens", tokens_s.as_str()), ("budget", budget_s.as_str())],
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

pub fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    let has_system = history.first().is_some_and(|m| m.role == "system");
    let non_system_count = if has_system {
        history.len() - 1
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return;
    }

    let system_offset = usize::from(has_system);

    // Find the first user message (the framing anchor). If `max_history` is
    // too small to fit both the anchor and any recent context, fall back to
    // the old tail-only behaviour rather than producing a degenerate window.
    let anchor_idx = history
        .iter()
        .enumerate()
        .skip(system_offset)
        .find(|(_, m)| m.role == "user")
        .map(|(i, _)| i);

    let messages_before = history.len();

    let dropped_range = match anchor_idx {
        Some(anchor) if max_history >= 2 => {
            // Reserve one slot for the anchor; keep `max_history - 1` most recent.
            let tail_keep = max_history - 1;
            let tail_start = history.len().saturating_sub(tail_keep);
            // Middle range to drop: (anchor + 1) .. tail_start.
            let drop_start = anchor + 1;
            if tail_start <= drop_start {
                // Anchor is already inside the tail window — nothing in the
                // middle to drop. Fall through to plain head-drop below.
                None
            } else {
                Some(drop_start..tail_start)
            }
        }
        _ => None,
    };

    if let Some(range) = dropped_range {
        history.drain(range);
    } else {
        // No anchor, or `max_history < 2`: original head-drop behaviour.
        let to_remove = non_system_count - max_history;
        history.drain(system_offset..system_offset + to_remove);
    }

    remove_orphaned_tool_messages(history);
    normalize_system_messages(history);

    let dropped = messages_before.saturating_sub(history.len());
    if dropped > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "messages_before": messages_before,
                    "messages_after": history.len(),
                    "dropped": dropped,
                    "max_history": max_history,
                    "kept_anchor": anchor_idx.is_some() && max_history >= 2,
                })),
            "trim_history fired: middle of conversation dropped. Raise \
             [runtime_profiles.<name>] max_history_messages or enable \
             compact_context to avoid silent context loss."
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveSessionState {
    pub version: u32,
    pub history: Vec<ChatMessage>,
}

impl InteractiveSessionState {
    fn from_history(history: &[ChatMessage]) -> Self {
        Self {
            version: 1,
            history: history.to_vec(),
        }
    }
}

pub fn load_interactive_session_history(
    path: &Path,
    system_prompt: &str,
) -> Result<Vec<ChatMessage>> {
    if !path.exists() {
        return Ok(vec![ChatMessage::system(system_prompt)]);
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

    Ok(state.history)
}

pub fn save_interactive_session_history(path: &Path, history: &[ChatMessage]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_string_pretty(&InteractiveSessionState::from_history(history))?;
    std::fs::write(path, payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn prepared_estimate_charges_images_without_affecting_raw_preflight() {
        // Five image markers in one tool result must not be estimated as a few
        // dozen text tokens — the markers expand to un-downscaled base64 at
        // send-time, so each ~1.5 MiB image is ~500K tokens.
        let dir = tempfile::tempdir().unwrap();
        let bytes = 1_500_000usize;
        let mut content = String::from("Here are the slides:\n");
        for i in 0..5 {
            let path = dir.path().join(format!("slide{i}.png"));
            std::fs::write(&path, vec![0u8; bytes]).unwrap();
            content.push_str(&format!("[IMAGE:{}]\n", path.display()));
        }
        let history = vec![ChatMessage::user(&content)];

        let (raw_text, _) = zeroclaw_providers::multimodal::parse_image_markers(&content);
        let text_only = raw_text.len().div_ceil(4) + 4;
        assert_eq!(
            estimate_history_tokens(&history),
            text_only,
            "raw-history preflight must not charge images before preparation applies caps"
        );
        assert!(
            text_only < 1_000,
            "text-only estimate should be small: {text_only}"
        );
        let estimate = estimate_prepared_history_tokens(&history);
        let expected_image_floor = 5 * (bytes / 3);
        assert!(
            estimate >= expected_image_floor,
            "image-heavy turn must be charged for its payload: estimate={estimate}, \
             expected at least {expected_image_floor}"
        );
    }

    #[test]
    fn estimate_image_marker_unsizable_reference_is_not_free() {
        // A missing local file and a remote URL cannot be sized here, but must
        // still cost more than their marker text so the meter errs high.
        assert_eq!(
            estimate_image_marker_tokens("/no/such/file/missing.png"),
            1_000
        );
        assert_eq!(
            estimate_image_marker_tokens("https://example.com/photo.jpg"),
            1_000
        );
    }

    #[test]
    fn estimate_image_marker_sizes_base64_data_uri_from_payload() {
        // 400 base64 chars -> 100 tokens, independent of the filesystem.
        let payload = "A".repeat(400);
        let uri = format!("data:image/png;base64,{payload}");
        assert_eq!(estimate_image_marker_tokens(&uri), 100);
    }

    #[test]
    fn prepared_estimate_does_not_double_count_inline_image_data_as_text() {
        let payload = "A".repeat(400);
        let content = format!("caption [IMAGE:data:image/png;base64,{payload}]");
        let history = vec![ChatMessage::user(&content)];

        assert_eq!(
            estimate_prepared_history_tokens(&history),
            "caption".len().div_ceil(4) + 4 + 100
        );
        assert_eq!(
            estimate_history_tokens(&history),
            "caption".len().div_ceil(4) + 4,
            "raw preflight must leave image enforcement to prepared accounting"
        );
    }

    #[tokio::test]
    async fn prepared_estimate_only_charges_images_that_survive_the_cap() {
        let payload = "A".repeat(400);
        let history: Vec<ChatMessage> = (0..3)
            .map(|index| {
                ChatMessage::user(format!(
                    "caption {index} [IMAGE:data:image/png;base64,{payload}]"
                ))
            })
            .collect();
        let config = zeroclaw_config::schema::MultimodalConfig {
            max_images: 1,
            ..Default::default()
        };

        let prepared =
            zeroclaw_providers::multimodal::prepare_messages_for_provider(&history, &config)
                .await
                .unwrap();
        assert_eq!(
            zeroclaw_providers::multimodal::count_image_markers(&prepared.messages),
            1
        );
        let estimate = estimate_prepared_history_tokens(&prepared.messages);
        assert!(
            estimate < 200,
            "capped images must not remain in effective-payload accounting: {estimate}"
        );
    }

    #[test]
    fn prepared_estimate_charges_each_image_in_a_multi_image_round_once() {
        // A single native-tool round can return several images in one message.
        // Each surviving image must be charged once — its decoded bytes, not the
        // base64 text — and not double-counted as both text and image payload.
        let payload = "A".repeat(400); // 400 base64 chars -> 100 image tokens each
        let message = ChatMessage::user(format!(
            "two screenshots [IMAGE:data:image/png;base64,{payload}] [IMAGE:data:image/png;base64,{payload}]"
        ));
        let (text, refs) = zeroclaw_providers::multimodal::parse_image_markers(&message.content);
        assert_eq!(refs.len(), 2, "two image markers in one round");
        // Charged as the stripped caption text (not the base64 content length,
        // which would double-count) plus each image once.
        assert_eq!(
            estimate_prepared_history_tokens(std::slice::from_ref(&message)),
            text.len().div_ceil(4) + 4 + 100 + 100
        );
    }

    #[test]
    fn estimate_system_floor_empty_and_no_system() {
        assert_eq!(estimate_system_floor_tokens(&[]), 0);
        let history = vec![ChatMessage::user("hi"), ChatMessage::assistant("yo")];
        assert_eq!(estimate_system_floor_tokens(&history), 0);
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
    fn canonicalize_tool_result_media_markers_wraps_existing_local_image_path() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("generated.png");
        std::fs::write(&image, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();

        let input = format!(
            "Image generated successfully.\nFile: {}",
            image.display().to_string()
        );
        let output = canonicalize_tool_result_media_markers(&input);

        assert!(output.contains("[IMAGE:"));
        assert!(output.contains(&format!("[IMAGE:{}]", image.display().to_string())));
    }

    #[test]
    fn canonicalize_tool_result_media_markers_ignores_missing_paths() {
        let input = "File: /tmp/definitely-missing-zeroclaw-image.png";
        let output = canonicalize_tool_result_media_markers(input);
        assert_eq!(output, input);
    }

    #[test]
    fn canonicalize_tool_result_media_markers_preserves_existing_markers() {
        let input = "Already tagged [IMAGE:/tmp/already-tagged.png]";
        let output = canonicalize_tool_result_media_markers(input);
        assert_eq!(output, input);
    }

    #[test]
    fn canonicalize_for_skips_path_listing_tools() {
        // A search/listing tool that surfaces a real image path must be left
        // untouched - promoting it to [IMAGE:...] would falsely trigger vision
        // routing
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("hit.png");
        std::fs::write(&image, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let input = format!("match: {}", image.display());

        for tool in ["content_search", "glob_search", "GLOB_SEARCH"] {
            let output = canonicalize_tool_result_media_markers_for(tool, &input);
            assert_eq!(output, input, "{tool} output must be left untouched");
            assert!(!output.contains("[IMAGE:"));
        }
    }

    #[test]
    fn canonicalize_for_wraps_image_producing_and_fetching_tools() {
        // Default-allow: image_gen (produces) and file_download (fetches) keep
        // canonicalization so a genuinely produced/fetched image still routes.
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("generated.png");
        std::fs::write(&image, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let input = format!("Saved to {}", image.display());
        let expected = format!("[IMAGE:{}]", image.display());

        for tool in ["image_gen", "file_download", "some_future_tool"] {
            let output = canonicalize_tool_result_media_markers_for(tool, &input);
            assert!(
                output.contains(&expected),
                "{tool} output should be canonicalized into a marker"
            );
        }
    }

    #[test]
    fn canonicalize_tool_result_media_markers_dedups_path_already_in_marker() {
        let input = "File: /tmp/pic.png\nFormat: png\n[IMAGE:/tmp/pic.png]";
        let output = canonicalize_tool_result_media_markers(input);
        assert_eq!(
            output, input,
            "bare path duplicating an existing marker must not be promoted"
        );
        assert_eq!(
            output.matches("[IMAGE:").count(),
            1,
            "exactly one image marker expected, got: {output}"
        );
    }

    #[test]
    fn truncate_tool_result_does_not_split_image_marker_at_head_boundary() {
        // 200-byte path → marker length 207 bytes. With max_chars=80 the
        // naive head_end (= 80 * 2 / 3 = 53) falls inside the marker.
        let path = format!("/tmp/{}.png", "a".repeat(200));
        let marker = format!("[IMAGE:{path}]");
        let output = format!("prefix-text {marker} trailing-text padding-padding");

        let truncated = truncate_tool_result(&output, 80);

        assert!(
            truncated.contains("[... ") && truncated.contains("characters truncated ...]"),
            "expected truncation marker in output, got: {truncated}"
        );
        // No half-`[IMAGE:` marker should leak into the surviving content.
        let stripped = truncated.replace(&marker, "");
        assert!(
            !stripped.contains("[IMAGE:"),
            "half-`[IMAGE:` marker leaked into truncated output: {truncated}"
        );
    }

    #[test]
    fn truncate_tool_result_does_not_split_image_marker_at_tail_boundary() {
        // Marker placed near the end so tail_start (~max_chars / 3 from the
        // end) lands inside it.
        let path = format!("/tmp/{}.png", "b".repeat(200));
        let marker = format!("[IMAGE:{path}]");
        let output = format!("{} preamble-content-line {marker} ending", "x".repeat(400));

        let truncated = truncate_tool_result(&output, 90);

        let stripped = truncated.replace(&marker, "");
        assert!(
            !stripped.contains("[IMAGE:") && !stripped.contains(".png]"),
            "half-`[IMAGE:` marker leaked into truncated output: {truncated}"
        );
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
}
