//! One2X hooks for the channels crate.
//!
//! Contains session hygiene, tool pairing repair, and agent hooks adapted
//! for the workspace crate architecture.

pub mod session_hygiene {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::path::Path;
    use zeroclaw_api::provider::ChatMessage;

    const MAX_TOOL_RESULT_SESSION_CHARS: usize = 2_000;
    const MAX_SESSION_FILE_BYTES: u64 = 500 * 1024;

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
        let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;
        if lines.len() <= keep_last_n {
            return Ok(false);
        }
        let keep_from = lines.len() - keep_last_n;
        let kept_lines = &lines[keep_from..];
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
        fs::rename(&tmp_path, session_path)?;
        tracing::info!(
            path = %session_path.display(),
            dropped = keep_from,
            kept = keep_last_n,
            "Session file truncated after compaction"
        );
        Ok(true)
    }
}

pub mod tool_pairing {
    use zeroclaw_api::provider::ChatMessage;

    const SYNTHETIC_TOOL_RESULT: &str = "[Tool result missing — internal error]";

    fn extract_tool_use_ids(content: &str) -> Vec<String> {
        let trimmed = content.trim();
        let mut ids = Vec::new();
        if trimmed.starts_with('[') {
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
                for v in &arr {
                    if v.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        } else if trimmed.starts_with('{') {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(calls) = obj.get("tool_calls").and_then(|c| c.as_array()) {
                    for call in calls {
                        if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        }
        ids
    }

    fn extract_tool_result_id(content: &str) -> Option<String> {
        let trimmed = content.trim();
        if trimmed.starts_with('{') {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
                return obj
                    .get("tool_call_id")
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string());
            }
        }
        None
    }

    pub fn repair_tool_pairing(history: &mut Vec<ChatMessage>) {
        if history.len() < 2 {
            return;
        }
        let mut repaired = Vec::with_capacity(history.len() + 4);
        let mut i = 0;
        let mut total_injected = 0usize;
        let mut total_removed = 0usize;

        while i < history.len() {
            let msg = &history[i];
            if msg.role != "assistant" {
                if msg.role == "tool"
                    && (repaired.is_empty()
                        || repaired
                            .last()
                            .map_or(true, |m: &ChatMessage| m.role != "assistant"))
                {
                    total_removed += 1;
                    i += 1;
                    continue;
                }
                repaired.push(msg.clone());
                i += 1;
                continue;
            }
            let tool_use_ids = extract_tool_use_ids(&msg.content);
            if tool_use_ids.is_empty() {
                repaired.push(msg.clone());
                i += 1;
                continue;
            }
            repaired.push(msg.clone());
            i += 1;
            let mut matched_ids = std::collections::HashSet::new();
            let mut seen_result_ids = std::collections::HashSet::new();
            let tool_use_id_set: std::collections::HashSet<_> =
                tool_use_ids.iter().cloned().collect();
            while i < history.len() && history[i].role == "tool" {
                if let Some(result_id) = extract_tool_result_id(&history[i].content) {
                    if tool_use_id_set.contains(&result_id)
                        && !seen_result_ids.contains(&result_id)
                    {
                        matched_ids.insert(result_id.clone());
                        seen_result_ids.insert(result_id);
                        repaired.push(history[i].clone());
                    } else {
                        total_removed += 1;
                    }
                } else {
                    repaired.push(history[i].clone());
                }
                i += 1;
            }
            for use_id in &tool_use_ids {
                if !matched_ids.contains(use_id) {
                    let synthetic = serde_json::json!({
                        "tool_call_id": use_id,
                        "content": SYNTHETIC_TOOL_RESULT,
                    });
                    repaired.push(ChatMessage::tool(synthetic.to_string()));
                    total_injected += 1;
                }
            }
        }

        if total_injected > 0 || total_removed > 0 {
            tracing::info!(
                injected = total_injected,
                removed = total_removed,
                "repair_tool_pairing: fixed tool_use/tool_result mismatches"
            );
            *history = repaired;
        }
    }
}

pub mod agent_hooks {
    pub fn detect_fast_approval(user_message: &str) -> Option<&'static str> {
        let trimmed = user_message.trim().to_lowercase();
        let approvals = [
            "ok", "好", "好的", "可以", "行", "执行", "做吧", "开始", "确认", "go", "yes",
            "do it", "proceed", "continue", "sure", "yep", "yeah", "confirmed", "approve",
            "agreed", "lgtm",
        ];
        if approvals.iter().any(|a| trimmed == *a) {
            Some(
                "The user approved. Execute the previously discussed plan immediately. \
                 Do not re-explain the plan — take the first action now.",
            )
        } else {
            None
        }
    }
}
