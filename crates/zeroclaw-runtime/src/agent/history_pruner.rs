use zeroclaw_api::model_provider::ChatMessage;

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Orphaned tool-message sanitiser
// ---------------------------------------------------------------------------

/// Outcome of a single `remove_orphaned_tool_messages` pass. The caller
/// is responsible for logging — that's where the agent/channel/session
/// context lives.
#[derive(Debug, Default, Clone)]
pub struct PrunedOrphans {
    /// Total tool / assistant messages removed across both passes.
    pub removed: usize,
    /// `tool_call_id`s that lost their pairing.
    pub orphan_tool_call_ids: Vec<String>,
}

fn assistant_is_unresolved_dispatch(
    messages: &[ChatMessage],
    prev_idx: usize,
    next_idx: usize,
) -> bool {
    match extract_assistant_tool_call_ids(&messages[prev_idx].content) {
        Some(ids) if !ids.is_empty() => {
            let between = &messages[prev_idx + 1..next_idx];
            !ids.iter().all(|id| {
                between.iter().any(|m| {
                    m.role == "tool" && extract_tool_call_id(&m.content).as_ref() == Some(id)
                })
            })
        }
        _ => false,
    }
}

impl PrunedOrphans {
    pub fn is_empty(&self) -> bool {
        self.removed == 0
    }
}

pub fn remove_orphaned_tool_messages(messages: &mut Vec<ChatMessage>) -> PrunedOrphans {
    let mut outcome = PrunedOrphans::default();
    let mut i = 0;
    while i < messages.len() {
        let assistant_tool_call_ids = if messages[i].role == "assistant" {
            extract_assistant_tool_call_ids(&messages[i].content)
        } else {
            None
        };
        if let Some(doomed_ids) = assistant_tool_call_ids
            && i > 0
            && messages[i - 1].role == "assistant"
            && assistant_is_unresolved_dispatch(messages, i - 1, i)
        {
            outcome
                .orphan_tool_call_ids
                .extend(doomed_ids.iter().cloned());
            messages.remove(i);
            outcome.removed += 1;
            while i < messages.len() && messages[i].role == "tool" {
                let dominated = match extract_tool_call_id(&messages[i].content) {
                    Some(id) => doomed_ids.iter().any(|d| d == &id),
                    None => true,
                };
                if dominated {
                    messages.remove(i);
                    outcome.removed += 1;
                } else {
                    break;
                }
            }
        } else {
            i += 1;
        }
    }

    i = 0;
    while i < messages.len() {
        if messages[i].role != "tool" {
            i += 1;
            continue;
        }

        let assistant_idx = (0..i)
            .rev()
            .take_while(|&j| messages[j].role == "assistant" || messages[j].role == "tool")
            .find(|&j| messages[j].role == "assistant");

        let is_orphan = match assistant_idx {
            None => true,
            Some(idx) => match extract_assistant_tool_call_ids(&messages[idx].content) {
                None => true,
                Some(ids) => match extract_tool_call_id(&messages[i].content) {
                    Some(tool_call_id) => !ids.iter().any(|id| id == &tool_call_id),
                    None => false,
                },
            },
        };

        if is_orphan {
            if let Some(id) = extract_tool_call_id(&messages[i].content) {
                outcome.orphan_tool_call_ids.push(id);
            }
            messages.remove(i);
            outcome.removed += 1;
        } else {
            i += 1;
        }
    }
    outcome
}

/// Try to extract a `tool_call_id` from a tool-role message's JSON content.
/// Tool messages are stored as JSON like:
/// `{"content": "...", "tool_call_id": "toolu_01Abc..."}`
fn extract_tool_call_id(content: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    value
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract the list of structured tool-call IDs an assistant message
/// is claiming to have invoked, if any. Returns `None` when the content
/// does not parse as a JSON object with a `tool_calls` array — meaning the
/// assistant has no native tool_use blocks backing any tool_results.
fn extract_assistant_tool_call_ids(content: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let arr = value.get("tool_calls")?.as_array()?;
    let ids: Vec<String> = arr
        .iter()
        .filter_map(|call| call.get("id").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();
    if ids.is_empty() { None } else { Some(ids) }
}

pub(crate) fn strip_orphaned_tool_calls_from_assistants(messages: &mut Vec<ChatMessage>) -> usize {
    let mut seen_tool_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stripped = 0usize;

    let mut idx = messages.len();
    while idx > 0 {
        idx -= 1;

        if messages[idx].role == "tool" {
            if let Some(id) = extract_tool_call_id(&messages[idx].content) {
                seen_tool_ids.insert(id);
            }
            continue;
        }
        if messages[idx].role != "assistant" || !messages[idx].content.contains("tool_calls") {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&messages[idx].content)
        else {
            continue;
        };
        let Some(calls) = value.get("tool_calls").and_then(|v| v.as_array()) else {
            continue;
        };

        let paired_calls: Vec<serde_json::Value> = calls
            .iter()
            .filter(|call| {
                call.get("id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| seen_tool_ids.contains(id))
            })
            .cloned()
            .collect();

        if paired_calls.len() == calls.len() {
            continue; // every tool_call is paired — nothing to do
        }

        let orphan_ids: Vec<String> = calls
            .iter()
            .filter_map(|call| call.get("id").and_then(|v| v.as_str()).map(str::to_owned))
            .filter(|id| !seen_tool_ids.contains(id))
            .collect();

        if paired_calls.is_empty() {
            let salvaged_text = value
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string);
            match salvaged_text {
                Some(text) => messages[idx].content = text,
                None => {
                    messages.remove(idx);
                }
            }
        } else {
            if let serde_json::Value::Object(ref mut map) = value {
                map.insert(
                    "tool_calls".to_string(),
                    serde_json::Value::Array(paired_calls),
                );
            }
            messages[idx].content = value.to_string();
        }
        stripped += 1;

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "orphan_ids": orphan_ids })),
            "Stripped unpaired tool_calls from assistant history message — likely a \
             max_tool_iterations early exit"
        );
    }
    stripped
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // strip_orphaned_tool_calls_from_assistants tests
    // -----------------------------------------------------------------------

    #[test]
    fn strip_orphan_tool_calls_drops_tool_calls_when_no_result_follows() {
        let tool_calls_assistant = r#"{"content":"looking it up","tool_calls":[{"id":"toolu_ORPHAN","name":"search","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("user", "search for X"),
            msg("assistant", tool_calls_assistant),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        assert_eq!(
            messages.len(),
            2,
            "message with salvageable text is retained"
        );
        assert_eq!(
            messages[1].content, "looking it up",
            "survivor must be bare assistant text, not a JSON envelope; got: {}",
            messages[1].content
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&messages[1].content).is_err(),
            "salvaged text must not parse back as a JSON object"
        );
    }

    #[test]
    fn strip_orphan_tool_calls_drops_message_when_content_null_all_orphan() {
        let tool_calls_assistant = r#"{"content":null,"tool_calls":[{"id":"toolu_ORPHAN","name":"search","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("user", "search for X"),
            msg("assistant", tool_calls_assistant),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        assert_eq!(
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user"],
            "the content-null all-orphan assistant must be dropped, leaving no \
             {{\"content\":null}} survivor"
        );
        assert!(
            !messages.iter().any(|m| m.content.contains("content")),
            "no degenerate envelope may survive: {messages:?}"
        );
    }

    #[test]
    fn strip_orphan_tool_calls_drops_message_when_content_empty_all_orphan() {
        // Same as above but with empty-string content rather than null — also
        // degenerate, also dropped.
        let tool_calls_assistant = r#"{"content":"","tool_calls":[{"id":"toolu_ORPHAN","name":"search","arguments":"{}"}]}"#;
        let mut messages = vec![msg("user", "go"), msg("assistant", tool_calls_assistant)];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        assert_eq!(
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user"]
        );
    }

    #[test]
    fn strip_orphan_tool_calls_retains_paired_calls() {
        let tool_calls_assistant =
            r#"{"content":null,"tool_calls":[{"id":"toolu_OK","name":"search","arguments":"{}"}]}"#;
        let tool_result = r#"{"content":"result","tool_call_id":"toolu_OK"}"#;
        let mut messages = vec![
            msg("user", "q"),
            msg("assistant", tool_calls_assistant),
            msg("tool", tool_result),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 0, "paired tool_call must not be stripped");
        assert!(messages[1].content.contains("toolu_OK"));
    }

    #[test]
    fn strip_orphan_tool_calls_partial_keeps_paired_drops_orphans() {
        // One paired, one orphaned — the paired entry survives, orphan goes.
        let tool_calls_assistant = r#"{"content":null,"tool_calls":[{"id":"toolu_OK","name":"a","arguments":"{}"},{"id":"toolu_ORPHAN","name":"b","arguments":"{}"}]}"#;
        let tool_result = r#"{"content":"result","tool_call_id":"toolu_OK"}"#;
        let mut messages = vec![
            msg("user", "q"),
            msg("assistant", tool_calls_assistant),
            msg("tool", tool_result),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        let parsed: serde_json::Value = serde_json::from_str(&messages[1].content).unwrap();
        let calls = parsed.get("tool_calls").and_then(|v| v.as_array()).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("id").and_then(|v| v.as_str()),
            Some("toolu_OK")
        );
        assert!(!messages[1].content.contains("toolu_ORPHAN"));
    }

    #[test]
    fn strip_orphan_tool_calls_no_op_on_plain_assistants() {
        let mut messages = vec![
            msg("user", "hi"),
            msg("assistant", "hello"),
            msg("user", "how are you"),
            msg("assistant", "great"),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 0);
        assert_eq!(messages.len(), 4);
    }

    // -----------------------------------------------------------------------
    // remove_orphaned_tool_messages tests
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_tool_at_start_is_removed() {
        // Simulates the exact bug: session drain removes the assistant
        // message but leaves its tool results at the start.
        let mut messages = vec![
            msg("system", "sys"),
            msg(
                "tool",
                r#"{"content":"file listing","tool_call_id":"toolu_01HiJXWbhx"}"#,
            ),
            msg(
                "tool",
                r#"{"content":"another result","tool_call_id":"toolu_01AQP25qUz"}"#,
            ),
            msg("user", "thanks"),
            msg("assistant", "done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 2);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
    }

    #[test]
    fn valid_tool_pair_preserved() {
        // A properly paired assistant+tool sequence must survive.
        let assistant_with_tools = r#"{"content":"checking","tool_calls":[{"id":"toolu_abc123","name":"shell","arguments":"{}"}]}"#;
        let tool_result = r#"{"content":"ok","tool_call_id":"toolu_abc123"}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do it"),
            msg("assistant", assistant_with_tools),
            msg("tool", tool_result),
            msg("assistant", "done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 0);
        assert_eq!(messages.len(), 5);
    }

    #[test]
    fn multi_tool_call_batch_preserved() {
        // An assistant with 3 tool_calls followed by 3 tool results.
        let assistant_content = r#"{"content":"running","tool_calls":[{"id":"toolu_aaa","name":"shell","arguments":"{}"},{"id":"toolu_bbb","name":"shell","arguments":"{}"},{"id":"toolu_ccc","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do all 3"),
            msg("assistant", assistant_content),
            msg("tool", r#"{"content":"r1","tool_call_id":"toolu_aaa"}"#),
            msg("tool", r#"{"content":"r2","tool_call_id":"toolu_bbb"}"#),
            msg("tool", r#"{"content":"r3","tool_call_id":"toolu_ccc"}"#),
            msg("assistant", "all done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 0);
        assert_eq!(messages.len(), 7);
    }

    #[test]
    fn mismatched_tool_id_is_removed() {
        // Tool result references a tool_call_id not in the assistant message.
        let assistant_content = r#"{"content":"running","tool_calls":[{"id":"toolu_aaa","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "go"),
            msg("assistant", assistant_content),
            msg("tool", r#"{"content":"ok","tool_call_id":"toolu_aaa"}"#),
            msg("tool", r#"{"content":"stale","tool_call_id":"toolu_GONE"}"#),
            msg("assistant", "done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 1);
        assert_eq!(messages.len(), 5);
        // The valid tool result stays, the orphan is gone.
        assert_eq!(messages[3].role, "tool");
        assert!(messages[3].content.contains("toolu_aaa"));
    }

    #[test]
    fn orphan_tool_in_middle_after_collapsed_pair() {
        // Phase 1 collapsed an assistant+tool pair into a summary, but
        // a subsequent tool message referenced the original tool_call_id.
        let mut messages = vec![
            msg("system", "sys"),
            msg("assistant", "[Tool result: truncated...]"), // collapsed
            msg(
                "tool",
                r#"{"content":"leftover","tool_call_id":"toolu_OLD"}"#,
            ),
            msg("user", "next"),
            msg("assistant", "ok"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 1);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn preamble_then_tool_calls_is_kept_intact() {
        // Healthy shape: `[A: "let me check"] [A: tool_calls] [T: result]`.
        // The assistant first emits a brief preamble, then dispatches the
        // tool, then the tool returns. This is the normal flow of a real
        // tool-using turn — Pass 1 must NOT touch it.
        let tool_calls_assistant = r#"{"content":null,"tool_calls":[{"id":"toolu_LIVE","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do something"),
            msg("assistant", "Let me check."),
            msg("assistant", tool_calls_assistant),
            msg("tool", r#"{"content":"ok","tool_call_id":"toolu_LIVE"}"#),
            msg("assistant", "Here are the results."),
        ];
        let before = messages.len();
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(
            pruned.removed, 0,
            "preamble + dispatch + result is a healthy turn, not orphan poisoning"
        );
        assert_eq!(messages.len(), before);
    }

    #[test]
    fn back_to_back_unresolved_tool_calls_strips_later_dispatch() {
        let first_dispatch = r#"{"content":null,"tool_calls":[{"id":"toolu_LOST","name":"shell","arguments":"{}"}]}"#;
        let second_dispatch = r#"{"content":null,"tool_calls":[{"id":"toolu_DEAD","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do something"),
            msg("assistant", first_dispatch),
            msg("assistant", second_dispatch),
            msg("tool", r#"{"content":"ok","tool_call_id":"toolu_DEAD"}"#),
            msg("assistant", "summary"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(
            pruned.removed, 2,
            "second dispatch + its tool_result must be removed when prior dispatch is unresolved"
        );
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].content, first_dispatch);
        assert_eq!(messages[3].content, "summary");
    }

    #[test]
    fn tool_without_parseable_id_kept_if_assistant_has_tool_calls() {
        // Conservative: if we can't parse the tool_call_id, keep the
        // message as long as the preceding assistant has tool_calls.
        let assistant_content = r#"{"content":"running","tool_calls":[{"id":"toolu_x","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "go"),
            msg("assistant", assistant_content),
            msg("tool", "plain text result without json"),
            msg("assistant", "done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 0);
        assert_eq!(messages.len(), 5);
    }

    #[test]
    fn orphan_tool_not_fooled_by_id_in_summary_text() {
        let summary = "[CONTEXT SUMMARY \u{2014} 4 messages compressed]\n\
             Earlier turns invoked shell with tool_calls id toolu_01Orphan \
             and returned ok.";
        let mut messages = vec![
            msg("system", "sys"),
            msg("assistant", summary),
            msg(
                "tool",
                r#"{"tool_call_id":"toolu_01Orphan","content":"stale"}"#,
            ),
            msg("user", "new question"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(
            pruned.removed, 1,
            "orphan must be removed even if its id is mentioned in summary text"
        );
        assert!(!messages.iter().any(|m| m.role == "tool"));
    }

    #[test]
    fn orphan_tool_from_trimmed_channel_history() {
        // Simulates the scenario: channel history was trimmed and the
        // assistant message containing tool_calls was dropped, leaving
        // orphaned tool results with MiniMax-style IDs.
        let tool_result =
            r#"{"content":"search results","tool_call_id":"chatcmpl-tool-92a12a15c14f3b36"}"#;
        let mut messages = vec![
            msg("system", "You are a helpful assistant"),
            msg("tool", tool_result),
            msg("assistant", "Here are the search results"),
            msg("user", "Thanks, now summarize them"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 1, "orphaned tool message should be removed");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }
}
