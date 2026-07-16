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

// Matches a local image path that a tool printed as bare text so it can be
// promoted to an `[IMAGE:…]` marker. Three rooted forms are recognized:
//   - POSIX absolute:      `/path/to/a.png`
//   - Windows drive:       `C:\path\a.png` or `C:/path/a.png`
//   - Windows UNC share:   `\\server\share\a.png`
// Only rooted paths are promoted; `is_existing_local_image_path` further
// requires the path to be absolute and to point at a real file, so on
// non-Windows hosts the Windows forms match here but are filtered out there
// (their `is_absolute()` is false), leaving behavior unchanged off-Windows.
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

/// If `boundary` falls inside an `[IMAGE:...]` marker (i.e. between an
/// unclosed `[IMAGE:` and its closing `]`), nudge it onto the nearest
/// complete-marker boundary. The malformed half-marker is dropped into the
/// truncated middle rather than emitted to the regex, which would otherwise
/// silently fail to match and quietly lose the image.
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

/// Truncate a tool result to `max_chars`, keeping head (2/3) + tail (1/3)
/// with a marker in the middle. Returns input unchanged if within limit or
/// `max_chars == 0` (disabled).
///
/// Boundaries are nudged inward when they would split an `[IMAGE:...]`
/// marker, so the multimodal regex never sees a half-marker in the
/// surviving head/tail. This matches the canonicalization step that runs
/// immediately before truncation in `run_tool_call_loop`.
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

/// Collect the inner payloads of every explicit `[IMAGE:…]` marker already
/// present in `output`. A bare path matching one of these must not be promoted
/// into a *second* marker, otherwise the same image would be counted (and
/// inlined) twice. This lets a tool emit both a durable human-readable path
/// line and an explicit marker for the same file (e.g. `image_info`, which
/// keeps a `File: <path>` line so the path survives in history after the image
/// marker is stripped from older turns) without the pipeline double-counting.
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

/// Tools whose output merely *lists* or *quotes* local filesystem paths
/// (search hits, glob matches) rather than presenting an image as visual
/// content. Their incidental image-file paths must NOT be auto-promoted to
/// `[IMAGE:...]` markers: the agent loop counts the current iteration's
/// tool-result markers (`multimodal::count_image_markers`) when deciding
/// whether to switch to a vision provider, so a path echo here falsely
/// triggers vision routing - producing a provider-capability error on a
/// text-only provider. See PR #7345.
///
/// This is a denylist (default-allow): any other tool - including ones that
/// genuinely *generate* or *fetch* an image and print its path (e.g.
/// `image_gen`, `file_download`) - keeps canonicalization, so real
/// tool-produced images still route to a configured vision provider.
fn is_path_listing_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "content_search" | "glob_search"
    )
}

/// Provenance-aware wrapper around [`canonicalize_tool_result_media_markers`].
///
/// Returns the output unchanged for path-listing tools (`is_path_listing_tool`)
/// so their incidental image paths never become routable `[IMAGE:...]` markers;
/// all other tools are canonicalized exactly as before.
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

/// Estimate the token cost of a single message using the ~4 chars/token
/// heuristic plus ~4 framing tokens (role, delimiters). Single-sourced so the
/// history and system-floor estimates stay in lock-step.
fn estimate_message_tokens(message: &ChatMessage) -> usize {
    message.content.len().div_ceil(4) + 4
}

/// Estimate token count for a message history using ~4 chars/token heuristic.
/// Includes a small overhead per message for role/framing tokens.
pub fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    history.iter().map(estimate_message_tokens).sum()
}

/// Estimate the irreducible token floor of a history: the content trimming can
/// never drop. That is every `system` message (system prompt + inlined tool
/// definitions), which whole-turn trimming always keeps.
///
/// When this floor alone meets or exceeds the context budget, no amount of
/// conversation trimming can bring the request under budget — trimming only
/// sheds whole turns, never the protected system content (#5808). Callers use
/// this to detect that condition and surface an actionable remediation hint
/// (raise `[runtime_profiles.<name>] max_context_tokens` or reduce the tool
/// surface) instead of a generic overflow error. Mirrors
/// `estimate_history_tokens`' heuristic exactly.
pub fn estimate_system_floor_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .filter(|m| m.role == "system")
        .map(estimate_message_tokens)
        .sum()
}

/// Actionable one-line remediation for the #5808 floor-exceeds-budget
/// condition. Names the resolved effective budget (`budget`) and the system
/// floor (`system_floor`) the runtime actually measured, and points operators
/// at the config surface they can change (`[runtime_profiles.<name>]
/// max_context_tokens`) rather than the inert `agent.max_context_tokens` knob.
///
/// Every emission site (iteration-0 preemptive trim, the turn-boundary trim,
/// and the in-loop reactive recovery) formats the visible message and its
/// stderr copy through this one function, so the human-facing text and the
/// structured log attrs can never drift and always name the same `N`.
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

/// Trim conversation history to prevent unbounded growth.
///
/// Preserves: the system prompt (if any), the first user message (the framing
/// anchor — losing it is what caused the silent-amnesia bug where models said
/// "the first message I have is 'Continue'"), and the most recent
/// `max_history` messages (minus one slot already taken by the anchor).
///
/// Drops from the middle. Emits a WARN with counts on every fire so silent
/// amnesia is impossible to miss again.
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

    // Self-heal persisted sessions that were written with orphaned
    // tool_result messages (e.g. a crash mid-compaction, or a trim that
    // dropped the assistant tool_use block but left its tool_result).
    // Without this the next API call fails with 400 "unexpected tool_use_id
    // found in tool_result blocks" and the session stays bricked until the
    // file is deleted.
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
        // ... and never at the inert agent-inline knob (#6877).
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
        // routing (PR #7345).
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
        // `image_info` emits a durable `File: <path>` line *and* an explicit
        // `[IMAGE:<path>]` marker for the same file (so the path survives in
        // history once the marker is stripped from older turns). The promoter
        // must not wrap the bare `File:` path into a second marker, which would
        // double-count the image. Order-independent: the bare path appears
        // before the marker here.
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

    /// Regression: when `truncate_tool_result`'s head boundary fell inside an
    /// `[IMAGE:...]` marker, the head ended up containing a half-marker like
    /// `[IMAGE:/very/long/pa` that the multimodal regex would silently fail
    /// to match. The boundary now rewinds to the marker opener so the broken
    /// half is dropped into the truncated middle. See PR #6183 review.
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

    /// Regression: tail boundary previously could land inside an
    /// `[IMAGE:...]` marker, leaving a stray closing `...png]` fragment in
    /// the surviving tail. The boundary now advances past the closing `]`.
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

    /// When a complete `[IMAGE:...]` marker fits naturally inside the
    /// retained head, truncation must not damage it.
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
