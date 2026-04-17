//! One2X hooks for the runtime crate.

pub mod compaction;

pub mod session_hygiene {
    use std::collections::HashSet;
    use zeroclaw_api::provider::ChatMessage;

    const MAX_TOOL_RESULT_PRE_LLM_CHARS: usize = 20_000;
    const MAX_TOOL_RESULT_CONTEXT_SHARE: f64 = 0.30;

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

    pub fn micro_compact_old_tool_results(history: &mut Vec<ChatMessage>) {
        const KEEP_RECENT_TURNS: usize = 3;
        const CLEARED_MSG: &str = "[Old tool result cleared — context compacted]";

        let user_turn_indices: Vec<usize> = history
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i)
            .collect();

        if user_turn_indices.len() <= KEEP_RECENT_TURNS {
            return;
        }

        let cutoff_idx = user_turn_indices[user_turn_indices.len() - KEEP_RECENT_TURNS];
        let mut cleared = 0usize;

        for msg in history[..cutoff_idx].iter_mut() {
            if msg.role == "tool" && msg.content.len() > 200 && !msg.content.starts_with(CLEARED_MSG)
            {
                msg.content = CLEARED_MSG.to_string();
                cleared += 1;
            }
        }

        if cleared > 0 {
            tracing::debug!(cleared, cutoff_idx, "micro_compact: cleared old tool results");
        }
    }

    pub fn limit_tool_result_sizes_with_budget(history: &mut Vec<ChatMessage>, context_window: usize) {
        const TAIL_CHARS: usize = 2_000;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let dynamic_cap = ((context_window as f64 * MAX_TOOL_RESULT_CONTEXT_SHARE) as usize * 4)
            .min(MAX_TOOL_RESULT_PRE_LLM_CHARS);

        let mut capped = 0usize;
        for msg in history.iter_mut() {
            if msg.role != "tool" || msg.content.len() <= dynamic_cap {
                continue;
            }
            let total = msg.content.len();
            let head_budget = dynamic_cap.saturating_sub(TAIL_CHARS);
            let omitted = total.saturating_sub(head_budget).saturating_sub(TAIL_CHARS);

            let mut head_end = head_budget;
            while head_end > 0 && !msg.content.is_char_boundary(head_end) {
                head_end -= 1;
            }
            let mut tail_start = total.saturating_sub(TAIL_CHARS);
            while tail_start < total && !msg.content.is_char_boundary(tail_start) {
                tail_start += 1;
            }

            msg.content = format!(
                "{}\n\n... [{} chars omitted] ...\n\n{}",
                &msg.content[..head_end],
                omitted,
                &msg.content[tail_start..]
            );
            capped += 1;
        }
        if capped > 0 {
            tracing::debug!(
                capped,
                "limit_tool_result_sizes: capped oversized tool results (head+tail preserved)"
            );
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
