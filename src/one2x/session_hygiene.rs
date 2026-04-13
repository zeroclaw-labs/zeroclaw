//! Session file hygiene: tool result trimming + truncation + repair.
//!
//! Solves the session bloat → context explosion → 504 timeout cascade:
//! 1. Large tool results trimmed before persist (prevents JSONL bloat)
//! 2. Session file truncated after compaction (syncs disk with RAM)
//! 3. Broken messages repaired on session load (self-healing)
//!
//! ## Upstream hooks (channels/mod.rs, all cfg-gated)
//!
//! - `trim_tool_result_for_session(&msg)` before append_sender_turn for tool msgs
//! - `truncate_session_file(path, n)` after compress_if_needed succeeds
//! - `repair_session_messages(&mut msgs)` during session hydration on startup

use zeroclaw_api::provider::ChatMessage;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Tool results larger than this are trimmed before session persistence.
const MAX_TOOL_RESULT_SESSION_CHARS: usize = 2_000;

/// Session files larger than this trigger truncation after compaction.
const MAX_SESSION_FILE_BYTES: u64 = 500 * 1024;

/// Trim a tool result before persisting to session JSONL.
/// Prevents session bloat from huge JSON blobs (e.g. Medeo 20 recipes).
pub fn trim_tool_result_for_session(msg: &ChatMessage) -> ChatMessage {
    if msg.role != "tool" || msg.content.len() <= MAX_TOOL_RESULT_SESSION_CHARS {
        return msg.clone();
    }
    let keep_head = MAX_TOOL_RESULT_SESSION_CHARS * 2 / 3;
    let keep_tail = MAX_TOOL_RESULT_SESSION_CHARS / 3;
    let omitted = msg.content.len() - keep_head - keep_tail;
    let mut eh = keep_head;
    while eh > 0 && !msg.content.is_char_boundary(eh) {
        eh -= 1;
    }
    let mut st = msg.content.len() - keep_tail;
    while st < msg.content.len() && !msg.content.is_char_boundary(st) {
        st += 1;
    }
    tracing::debug!(
        original = msg.content.len(),
        "Trimmed large tool result for session persistence"
    );
    ChatMessage {
        role: msg.role.clone(),
        content: format!(
            "{}... [{} chars omitted] ...{}",
            &msg.content[..eh],
            omitted,
            &msg.content[st..]
        ),
    }
}

/// Repair session messages during hydration: remove orphaned tool results,
/// empty messages, and broken tool_use/tool_result pairs.
pub fn repair_session_messages(msgs: &mut Vec<ChatMessage>) {
    let before = msgs.len();
    msgs.retain(|m| !m.content.trim().is_empty());
    while !msgs.is_empty() && msgs[0].role == "tool" {
        msgs.remove(0);
    }
    let mut i = 0;
    while i < msgs.len() {
        if msgs[i].content.contains("[CONTEXT SUMMARY") {
            while i + 1 < msgs.len() && msgs[i + 1].role == "tool" {
                msgs.remove(i + 1);
            }
        }
        i += 1;
    }
    let removed = before - msgs.len();
    if removed > 0 {
        tracing::info!(removed, "Repaired session: removed broken messages");
    }
}

/// Ensure every tool message has a matching assistant tool_call.
/// Removes orphaned tool results that would cause Bedrock/Anthropic 400 errors.
/// Called before every LLM request — prevents errors instead of recovering after.
/// Pattern from Claude Code's `ensureToolResultPairing`.
pub fn ensure_tool_result_pairing(history: &mut Vec<ChatMessage>) {
    let before = history.len();

    // Remove orphaned tool results at the start (after system prompt)
    while history.len() > 1 && history[1].role == "tool" {
        history.remove(1);
    }

    // Remove orphaned tool results after [CONTEXT SUMMARY] markers
    let mut i = 0;
    while i < history.len() {
        if history[i].content.contains("[CONTEXT SUMMARY") {
            while i + 1 < history.len() && history[i + 1].role == "tool" {
                history.remove(i + 1);
            }
        }
        i += 1;
    }

    // Remove trailing orphan tool messages
    while history.len() >= 2 && history.last().map_or(false, |m| m.role == "tool") {
        history.pop();
    }

    let removed = before - history.len();
    if removed > 0 {
        tracing::debug!(removed, "ensure_tool_result_pairing: removed orphaned tool messages");
    }
}

/// Max characters allowed for a single tool result before every LLM call.
/// Prevents a single huge result from eating half the context window.
/// Matches openclaw's pre-LLM tool result guard (50% of context → ~20K chars conservatively).
const MAX_TOOL_RESULT_PRE_LLM_CHARS: usize = 20_000;

/// Full mid-history tool pairing repair.
///
/// Extends `ensure_tool_result_pairing` to scan the *entire* history:
/// 1. Remove any `tool` message not preceded by `assistant` or another `tool`.
/// 2. For native-mode `assistant` messages with JSON `tool_calls`, find any
///    `id` that has no matching `tool_call_id` in subsequent `tool` messages
///    and insert a synthetic error result so the provider sees a complete pair.
///
/// Pattern from openclaw `repairToolUseResultPairing` — covers mid-history
/// orphans that `ensure_tool_result_pairing` misses (start/end/summary only).
pub fn repair_full_tool_pairing(history: &mut Vec<ChatMessage>) {
    // Pass 1: remove orphaned tool messages mid-history
    // A tool message is valid only when preceded by assistant or another tool.
    let before = history.len();
    let mut i = 1; // index 0 is typically system prompt, skip it
    while i < history.len() {
        if history[i].role == "tool" {
            let prev_role = history[i - 1].role.as_str();
            if prev_role != "assistant" && prev_role != "tool" {
                tracing::debug!(
                    index = i,
                    "repair_full_tool_pairing: removing mid-history orphan tool message"
                );
                history.remove(i);
                // Don't advance i — next element shifted into position i
                continue;
            }
        }
        i += 1;
    }

    // Pass 2: for native-mode assistant messages, insert synthetic tool results
    // for any tool_call id that has no matching tool_call_id in the following tool messages.
    let mut i = 0;
    while i < history.len() {
        if history[i].role == "assistant" {
            // Try to parse as native-mode JSON with tool_calls
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&history[i].content) {
                if let Some(tool_calls) = val.get("tool_calls").and_then(|v| v.as_array()) {
                    let ids: Vec<String> = tool_calls
                        .iter()
                        .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .collect();

                    if !ids.is_empty() {
                        // Collect tool_call_ids from subsequent tool messages
                        let mut existing_ids = HashSet::new();
                        let mut j = i + 1;
                        while j < history.len() && history[j].role == "tool" {
                            if let Ok(tv) = serde_json::from_str::<serde_json::Value>(&history[j].content) {
                                if let Some(tcid) = tv.get("tool_call_id").and_then(|v| v.as_str()) {
                                    existing_ids.insert(tcid.to_string());
                                }
                            }
                            j += 1;
                        }

                        // Insert synthetic results for missing ids (after last tool message)
                        let insert_at = j;
                        let mut offset = 0;
                        for id in &ids {
                            if !existing_ids.contains(id) {
                                let synthetic_content = serde_json::json!({
                                    "tool_call_id": id,
                                    "content": "[one2x] missing tool result in session history; inserted synthetic error result."
                                })
                                .to_string();
                                tracing::warn!(
                                    tool_call_id = id,
                                    "repair_full_tool_pairing: inserting synthetic tool result for missing id"
                                );
                                history.insert(
                                    insert_at + offset,
                                    ChatMessage {
                                        role: "tool".to_string(),
                                        content: synthetic_content,
                                    },
                                );
                                offset += 1;
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }

    let after = history.len();
    let removed = before.saturating_sub(after);
    let added = after.saturating_sub(before);
    if removed > 0 || added > 0 {
        tracing::info!(
            removed,
            added,
            "repair_full_tool_pairing: repaired mid-history tool pairing"
        );
    }
}

/// Cap large tool results before every LLM call.
///
/// Prevents a single huge tool result from consuming >50% of the context window.
/// Called unconditionally before every LLM request — not just on budget breach.
/// Pattern from openclaw pre-LLM tool result guard.
pub fn limit_tool_result_sizes(history: &mut Vec<ChatMessage>) {
    let mut capped = 0usize;
    for msg in history.iter_mut() {
        if msg.role != "tool" || msg.content.len() <= MAX_TOOL_RESULT_PRE_LLM_CHARS {
            continue;
        }
        // Try to cap cleanly at a char boundary
        let mut end = MAX_TOOL_RESULT_PRE_LLM_CHARS;
        while end > 0 && !msg.content.is_char_boundary(end) {
            end -= 1;
        }
        let omitted = msg.content.len() - end;
        msg.content = format!("{}... [{} chars truncated before LLM call]", &msg.content[..end], omitted);
        capped += 1;
    }
    if capped > 0 {
        tracing::debug!(capped, "limit_tool_result_sizes: capped oversized tool results");
    }
}

/// After compaction, rewrite the session JSONL to only contain recent messages.
pub fn truncate_session_file(session_path: &Path, keep_last_n: usize) -> std::io::Result<bool> {
    if !session_path.exists() {
        return Ok(false);
    }

    let metadata = fs::metadata(session_path)?;
    if metadata.len() < MAX_SESSION_FILE_BYTES {
        return Ok(false);
    }

    let file = fs::File::open(session_path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<Vec<_>, _>>()?;

    if lines.len() <= keep_last_n {
        return Ok(false);
    }

    // Keep only the last N lines
    let keep_from = lines.len() - keep_last_n;
    let kept_lines = &lines[keep_from..];

    // Write a compaction marker + kept lines
    let tmp_path = session_path.with_extension("jsonl.tmp");
    {
        let mut tmp = fs::File::create(&tmp_path)?;
        writeln!(
            tmp,
            r#"{{"_compacted":true,"dropped":{},"kept":{},"timestamp":"{}"}}"#,
            keep_from,
            keep_last_n,
            chrono::Utc::now().to_rfc3339()
        )?;
        for line in kept_lines {
            writeln!(tmp, "{}", line)?;
        }
        tmp.flush()?;
    }

    // Atomic replace
    fs::rename(&tmp_path, session_path)?;

    tracing::info!(
        path = %session_path.display(),
        dropped = keep_from,
        kept = keep_last_n,
        "Session file truncated after compaction"
    );

    Ok(true)
}

/// Enforce a disk budget across all session files in a directory.
/// Deletes oldest session files when total size exceeds `max_bytes`.
pub fn enforce_session_disk_budget(sessions_dir: &Path, max_bytes: u64) -> std::io::Result<u64> {
    if !sessions_dir.exists() {
        return Ok(0);
    }

    let mut entries: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total_bytes: u64 = 0;

    for entry in fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(true, |e| e != "jsonl") {
            continue;
        }
        let meta = entry.metadata()?;
        let size = meta.len();
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        total_bytes += size;
        entries.push((path, size, modified));
    }

    if total_bytes <= max_bytes {
        return Ok(0);
    }

    // Sort by modification time (oldest first)
    entries.sort_by_key(|e| e.2);

    let mut freed: u64 = 0;
    for (path, size, _) in &entries {
        if total_bytes - freed <= max_bytes {
            break;
        }
        if let Err(e) = fs::remove_file(path) {
            tracing::warn!(path = %path.display(), error = %e, "Failed to remove old session");
        } else {
            freed += size;
            tracing::info!(path = %path.display(), size, "Removed old session file (disk budget)");
        }
    }

    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn trim_small_tool_result_unchanged() {
        let m = msg("tool", "ok");
        assert_eq!(trim_tool_result_for_session(&m).content, "ok");
    }

    #[test]
    fn trim_large_tool_result() {
        let m = msg("tool", &"x".repeat(5000));
        let t = trim_tool_result_for_session(&m);
        assert!(t.content.len() < 2500);
        assert!(t.content.contains("chars omitted"));
    }

    #[test]
    fn trim_non_tool_unchanged() {
        let m = msg("user", &"x".repeat(5000));
        assert_eq!(trim_tool_result_for_session(&m).content.len(), 5000);
    }

    #[test]
    fn repair_removes_orphaned_tool_at_start() {
        let mut msgs = vec![msg("tool", "orphan"), msg("user", "hi"), msg("assistant", "hello")];
        repair_session_messages(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn repair_removes_empty_messages() {
        let mut msgs = vec![msg("user", "hi"), msg("assistant", "  "), msg("assistant", "ok")];
        repair_session_messages(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].content, "ok");
    }

    #[test]
    fn repair_removes_orphan_after_summary() {
        let mut msgs = vec![
            msg("assistant", "[CONTEXT SUMMARY -- compressed]"),
            msg("tool", "orphan result"),
            msg("user", "next"),
        ];
        repair_session_messages(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].role, "user");
    }

    #[test]
    fn truncate_small_file_skipped() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for i in 0..5 {
            writeln!(f, r#"{{"role":"user","content":"msg {}"}}"#, i).unwrap();
        }
        assert!(!truncate_session_file(&path, 3).unwrap());
    }

    #[test]
    fn enforce_disk_budget_under_limit() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("small.jsonl");
        fs::write(&path, "tiny").unwrap();
        let freed = enforce_session_disk_budget(tmp.path(), 1_000_000).unwrap();
        assert_eq!(freed, 0);
    }
}
