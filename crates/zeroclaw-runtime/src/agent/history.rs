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
    Regex::new(r#"/[^\s<>'"`\]\)]+?\.(?i:png|jpe?g|webp|gif|bmp)"#).expect("valid image path regex")
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

/// Rewrite real local image file paths in tool output into `[IMAGE:...]`
/// markers so the multimodal pipeline can normalize them before the next
/// provider call. This targets shell/skill outputs that print filesystem
/// paths directly rather than returning explicit media markers.
pub fn canonicalize_tool_result_media_markers(output: &str) -> String {
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

/// Truncate a tool message's content, preserving JSON structure when the
/// message stores `tool_call_id` alongside `content` (native tool-call
/// format). Without this, `truncate_tool_result` destroys the JSON envelope
/// and downstream providers receive a `null` `call_id` (#5425).
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

/// Aggressively trim old tool result messages in history to recover from
/// context overflow. Keeps the last `protect_last_n` messages untouched.
/// Returns total characters saved.
pub fn fast_trim_tool_results(
    history: &mut [zeroclaw_providers::ChatMessage],
    protect_last_n: usize,
) -> usize {
    let trim_to = 2000;
    let mut saved = 0;
    let cutoff = history.len().saturating_sub(protect_last_n);
    for msg in &mut history[..cutoff] {
        if msg.role == "tool" && msg.content.len() > trim_to {
            let original_len = msg.content.len();
            msg.content = truncate_tool_message(&msg.content, trim_to);
            saved += original_len - msg.content.len();
        }
    }
    saved
}

/// Emergency: drop oldest non-system, non-recent messages from history.
/// Tool groups (assistant + consecutive tool messages) are dropped
/// atomically to preserve tool_use/tool_result pairing. See #4810.
/// Returns number of messages dropped.
pub fn emergency_history_trim(
    history: &mut Vec<zeroclaw_providers::ChatMessage>,
    keep_recent: usize,
) -> usize {
    let mut dropped = 0;
    let target_drop = history.len() / 3;
    let mut i = 0;
    while dropped < target_drop && i < history.len().saturating_sub(keep_recent) {
        if history[i].role == "system" {
            i += 1;
        } else if history[i].role == "assistant" {
            // Count following tool messages — drop as atomic group
            let mut tool_count = 0;
            while i + 1 + tool_count < history.len().saturating_sub(keep_recent)
                && history[i + 1 + tool_count].role == "tool"
            {
                tool_count += 1;
            }
            for _ in 0..=tool_count {
                history.remove(i);
                dropped += 1;
            }
        } else {
            history.remove(i);
            dropped += 1;
        }
    }
    dropped += remove_orphaned_tool_messages(history);
    dropped
}

/// Estimate token count for a message history using ~4 chars/token heuristic.
/// Includes a small overhead per message for role/framing tokens.
pub fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .map(|m| {
            // ~4 chars per token + ~4 framing tokens per message (role, delimiters)
            m.content.len().div_ceil(4) + 4
        })
        .sum()
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
/// Preserves the system prompt (first message if role=system) and the most recent messages.
pub fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    // Nothing to trim if within limit
    let has_system = history.first().is_some_and(|m| m.role == "system");
    let non_system_count = if has_system {
        history.len() - 1
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return;
    }

    let start = if has_system { 1 } else { 0 };
    let to_remove = non_system_count - max_history;
    history.drain(start..start + to_remove);
    remove_orphaned_tool_messages(history);
    normalize_system_messages(history);
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
    // file is deleted. See #5813.
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
    fn canonicalize_tool_result_media_markers_wraps_existing_local_image_path() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("generated.png");
        std::fs::write(&image, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();

        let input = format!("Image generated successfully.\nFile: {}", image.display());
        let output = canonicalize_tool_result_media_markers(&input);

        assert!(output.contains("[IMAGE:"));
        assert!(output.contains(&format!("[IMAGE:{}]", image.display())));
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
