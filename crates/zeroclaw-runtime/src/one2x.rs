//! One2X hooks for the runtime crate.

pub mod session_hygiene {
    use std::collections::HashSet;
    use zeroclaw_api::provider::ChatMessage;

    const MAX_TOOL_RESULT_PRE_LLM_CHARS: usize = 20_000;

    pub fn repair_full_tool_pairing(history: &mut Vec<ChatMessage>) {
        let before = history.len();
        let mut i = 1;
        while i < history.len() {
            if history[i].role == "tool" {
                let prev_role = history[i - 1].role.as_str();
                if prev_role != "assistant" && prev_role != "tool" {
                    tracing::debug!(
                        index = i,
                        "repair_full_tool_pairing: removing mid-history orphan tool message"
                    );
                    history.remove(i);
                    continue;
                }
            }
            i += 1;
        }

        let mut i = 0;
        while i < history.len() {
            if history[i].role == "assistant" {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&history[i].content) {
                    if let Some(tool_calls) = val.get("tool_calls").and_then(|v| v.as_array()) {
                        let ids: Vec<String> = tool_calls
                            .iter()
                            .filter_map(|tc| {
                                tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())
                            })
                            .collect();

                        if !ids.is_empty() {
                            let mut existing_ids = HashSet::new();
                            let mut j = i + 1;
                            while j < history.len() && history[j].role == "tool" {
                                if let Ok(tv) =
                                    serde_json::from_str::<serde_json::Value>(&history[j].content)
                                {
                                    if let Some(tcid) =
                                        tv.get("tool_call_id").and_then(|v| v.as_str())
                                    {
                                        existing_ids.insert(tcid.to_string());
                                    }
                                }
                                j += 1;
                            }

                            let insert_at = j;
                            let mut offset = 0;
                            for id in &ids {
                                if !existing_ids.contains(id) {
                                    let synthetic_content = serde_json::json!({
                                        "tool_call_id": id,
                                        "content": "[one2x] missing tool result; inserted synthetic error result."
                                    })
                                    .to_string();
                                    tracing::warn!(
                                        tool_call_id = id,
                                        "repair_full_tool_pairing: inserting synthetic tool result"
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

    pub fn limit_tool_result_sizes(history: &mut Vec<ChatMessage>) {
        let mut capped = 0usize;
        for msg in history.iter_mut() {
            if msg.role != "tool" || msg.content.len() <= MAX_TOOL_RESULT_PRE_LLM_CHARS {
                continue;
            }
            let mut end = MAX_TOOL_RESULT_PRE_LLM_CHARS;
            while end > 0 && !msg.content.is_char_boundary(end) {
                end -= 1;
            }
            let omitted = msg.content.len() - end;
            msg.content = format!(
                "{}... [{} chars truncated before LLM call]",
                &msg.content[..end],
                omitted
            );
            capped += 1;
        }
        if capped > 0 {
            tracing::debug!(capped, "limit_tool_result_sizes: capped oversized tool results");
        }
    }
}

pub mod agent_hooks {
    use zeroclaw_api::provider::ChatMessage;

    pub const STEP_TIMEOUT_MAX_RETRIES: u32 = 2;

    const PLANNING_PHRASES: &[&str] = &[
        "i will ",
        "i'll ",
        "i would ",
        "i'd recommend ",
        "here's my plan",
        "here is my plan",
        "let me outline",
        "the steps are",
        "the approach would be",
        "i'm going to ",
        "my plan is to",
        "first, i'll ",
        "step 1:",
        "i can help you by",
        "here's what i'll do",
        "here's what we need to do",
        "i propose ",
        "the strategy is",
    ];

    const EXECUTION_INDICATORS: &[&str] = &[
        "```",
        "tool_use",
        "tool_call",
        "i ran ",
        "i executed ",
        "the result is",
        "here's the output",
        "the output shows",
        "done.",
        "completed.",
        "created successfully",
        "updated successfully",
        "error:",
        "warning:",
    ];

    const NUDGE_MESSAGE: &str = "\
Do not describe what you will do — execute it now. \
Use your tools to take the first concrete action immediately. \
If multiple steps are needed, execute the first step now and report the result.";

    pub fn check_planning_without_execution(messages: &mut Vec<ChatMessage>) -> bool {
        let last = match messages.last() {
            Some(m) if m.role == "assistant" => m,
            _ => return false,
        };
        let content_lower = last.content.to_lowercase();
        for indicator in EXECUTION_INDICATORS {
            if content_lower.contains(indicator) {
                return false;
            }
        }
        let has_planning = PLANNING_PHRASES
            .iter()
            .any(|phrase| content_lower.contains(phrase));
        if !has_planning {
            return false;
        }
        if last.content.len() < 100 {
            return false;
        }
        tracing::info!("Detected planning-without-execution, injecting execution nudge");
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: NUDGE_MESSAGE.to_string(),
        });
        true
    }
}
