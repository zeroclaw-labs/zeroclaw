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

/// True when the assistant at `prev_idx` is itself an unresolved tool-call
/// dispatch: it claims `tool_calls` but the rows between it and `next_idx`
/// do not answer all of them. This is the genuinely poisoned shape where a
/// second dispatch follows a first that never landed — distinct from a
/// healthy `assistant(text preamble)` → `assistant(tool_calls)` turn, where
/// the preamble has no tool_calls and is left untouched.
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

/// Remove `tool`-role messages whose `tool_call_id` has no matching
/// `tool_use` / `tool_calls` entry in a preceding assistant message.
///
/// After any history truncation (drain, remove, prune) the first surviving
/// message(s) may be `tool` results whose assistant request was trimmed away.
/// The Anthropic API (and others) reject these with a 400 error.
pub fn remove_orphaned_tool_messages(messages: &mut Vec<ChatMessage>) -> PrunedOrphans {
    let mut outcome = PrunedOrphans::default();
    // Pass 1: Remove a second `assistant(tool_calls)` (and its immediate
    // tool results) only when the *preceding* assistant is itself
    // problematic in a way that normalization would corrupt:
    //
    //   * a collapsed tool-exchange summary whose merge would orphan this
    //     dispatch's results (the GLM-history case, #7013), or
    //   * an unresolved tool-call dispatch — a first dispatch that never
    //     landed, immediately followed by this one (the poisoned
    //     double-dispatch case).
    //
    // A healthy turn shape `assistant(text preamble)` → `assistant(tool_calls)`
    // → `tool` must NOT be touched: the preamble has no tool_calls and is
    // neither a summary nor an unresolved dispatch, so it is left intact.
    // Nuking the dispatch there produces the "amnesia mid-tool-loop"
    // failure where the model sees the next turn with none of its work.
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

    // Pass 2: Remove remaining orphan tool messages whose tool_call_id
    // is not in the preceding assistant's structured tool_calls array.
    // A substring match on the assistant's *text* is NOT sufficient —
    // compaction summaries are instructed to preserve identifiers, so an
    // id can appear in prose without an actual tool_use block backing it.
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
///
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
        // Genuinely poisoned shape: `[A: tool_calls A]` followed
        // immediately by `[A: tool_calls B]` with no tool result for A
        // sitting between them. The earlier dispatch is unresolved, so
        // the later assistant + its results are removed to restore a
        // well-formed turn.
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
        // What survives: sys, user, first_dispatch (now orphaned), summary.
        // Pass 2 then sweeps any remaining orphan tool messages — there
        // are none after Pass 1, but the orphaned first_dispatch itself
        // (assistant with tool_calls and no responses) stays, because
        // this function only removes *tool*-role orphans in Pass 2,
        // not stranded assistant dispatches.
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

    /// Regression test for issue #5813: a compaction summary preserves
    /// identifiers by design (UUIDs, tokens, tool_call_ids). That means the
    /// summary text may contain the tool_call_id of a tool_result whose
    /// tool_use was dropped. The orphan detector must not be fooled by a
    /// substring match on the summary — it must confirm the id appears in
    /// a structured tool_calls array.
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

    /// Regression test for issue #5743: MiniMax rejects orphaned tool-role
    /// messages whose assistant (with `tool_calls`) was trimmed by the
    /// channel orchestrator's proactive history trimming.
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
