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

fn extract_tool_call_id(message: &ChatMessage) -> Option<String> {
    message
        .tool_result
        .as_ref()
        .map(|result| result.tool_call_id.clone())
}

fn extract_assistant_tool_call_ids(message: &ChatMessage) -> Option<Vec<String>> {
    (!message.tool_calls.is_empty()).then(|| {
        message
            .tool_calls
            .iter()
            .map(|call| call.id.clone())
            .collect()
    })
}

fn assistant_is_unresolved_dispatch(
    messages: &[ChatMessage],
    prev_idx: usize,
    next_idx: usize,
) -> bool {
    match extract_assistant_tool_call_ids(&messages[prev_idx]) {
        Some(ids) if !ids.is_empty() => {
            let between = &messages[prev_idx + 1..next_idx];
            !ids.iter().all(|id| {
                between.iter().any(|message| {
                    message.role == "tool" && extract_tool_call_id(message).as_ref() == Some(id)
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
        let assistant_tool_call_ids = extract_assistant_tool_call_ids(&messages[i]);
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
                let dominated = match extract_tool_call_id(&messages[i]) {
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
            Some(idx) => match extract_assistant_tool_call_ids(&messages[idx]) {
                None => true,
                Some(ids) => match extract_tool_call_id(&messages[i]) {
                    Some(tool_call_id) => !ids.iter().any(|id| id == &tool_call_id),
                    None => false,
                },
            },
        };
        if is_orphan {
            if let Some(id) = extract_tool_call_id(&messages[i]) {
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

/// Remove unpaired structured calls after a turn stops early. Literal content
/// is never parsed, so JSON text remains ordinary model text.
pub(crate) fn strip_orphaned_tool_calls_from_assistants(messages: &mut Vec<ChatMessage>) -> usize {
    let mut seen_tool_ids = std::collections::HashSet::new();
    let mut stripped = 0usize;
    let mut idx = messages.len();
    while idx > 0 {
        idx -= 1;
        if messages[idx].role == "tool" {
            if let Some(id) = extract_tool_call_id(&messages[idx]) {
                seen_tool_ids.insert(id);
            }
            continue;
        }
        if messages[idx].role != "assistant" || messages[idx].tool_calls.is_empty() {
            continue;
        }
        let orphan_ids: Vec<String> = messages[idx]
            .tool_calls
            .iter()
            .filter(|call| !seen_tool_ids.contains(&call.id))
            .map(|call| call.id.clone())
            .collect();
        if orphan_ids.is_empty() {
            continue;
        }
        messages[idx]
            .tool_calls
            .retain(|call| seen_tool_ids.contains(&call.id));
        if messages[idx].tool_calls.is_empty()
            && messages[idx].content.trim().is_empty()
            && messages[idx].attachments.is_empty()
            && messages[idx].reasoning_content.is_none()
        {
            messages.remove(idx);
        }
        stripped += 1;
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "orphan_ids": orphan_ids })),
            "Stripped unpaired tool_calls from assistant history — likely a \
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
            tool_calls: Vec::new(),
            tool_result: None,
            reasoning_content: None,
            role: role.to_string(),
            content: content.to_string(),

            attachments: Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // strip_orphaned_tool_calls_from_assistants tests
    // -----------------------------------------------------------------------

    #[test]
    fn strip_orphan_tool_calls_drops_tool_calls_when_no_result_follows() {
        let mut messages = vec![
            msg("user", "search for X"),
            ChatMessage::assistant_tool_calls(
                "looking it up",
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_ORPHAN".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
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
    }

    #[test]
    fn strip_orphan_tool_calls_drops_message_when_content_null_all_orphan() {
        let mut messages = vec![
            msg("user", "search for X"),
            ChatMessage::assistant_tool_calls(
                String::new(),
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_ORPHAN".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
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
        let mut messages = vec![
            msg("user", "go"),
            ChatMessage::assistant_tool_calls(
                "",
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_ORPHAN".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        assert_eq!(
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user"]
        );
    }

    #[test]
    fn strip_orphan_tool_calls_retains_paired_calls() {
        let mut messages = vec![
            msg("user", "q"),
            ChatMessage::assistant_tool_calls(
                String::new(),
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_OK".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_OK".into(),
                tool_name: "".into(),
                success: None,
                content: "result".into(),
                attachments: Vec::new(),
            }),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 0, "paired tool_call must not be stripped");
        assert_eq!(messages[1].tool_calls[0].id, "toolu_OK");
    }

    #[test]
    fn strip_orphan_tool_calls_partial_keeps_paired_drops_orphans() {
        // One paired, one orphaned — the paired entry survives, orphan goes.
        let mut messages = vec![
            msg("user", "q"),
            ChatMessage::assistant_tool_calls(
                String::new(),
                vec![
                    zeroclaw_api::model_provider::ToolCall {
                        id: "toolu_OK".into(),
                        name: "a".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                    zeroclaw_api::model_provider::ToolCall {
                        id: "toolu_ORPHAN".into(),
                        name: "b".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                ],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_OK".into(),
                tool_name: "".into(),
                success: None,
                content: "result".into(),
                attachments: Vec::new(),
            }),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        assert_eq!(messages[1].tool_calls.len(), 1);
        assert_eq!(messages[1].tool_calls[0].id, "toolu_OK");
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

    #[test]
    fn stripping_orphan_calls_keeps_reasoning_and_attachments() {
        let mut message = ChatMessage::assistant_tool_calls(
            "",
            vec![zeroclaw_api::model_provider::ToolCall {
                id: "orphan".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            Some("signed reasoning".into()),
        );
        message
            .attachments
            .push(zeroclaw_api::media::AttachmentReference {
                kind: zeroclaw_api::media::AttachmentKind::Image,
                target: "/image.png".into(),
            });
        let mut messages = vec![message];
        assert_eq!(strip_orphaned_tool_calls_from_assistants(&mut messages), 1);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].tool_calls.is_empty());
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("signed reasoning")
        );
        assert_eq!(messages[0].attachments[0].target, "/image.png");
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
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_01HiJXWbhx".into(),
                tool_name: "".into(),
                success: None,
                content: "file listing".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_01AQP25qUz".into(),
                tool_name: "".into(),
                success: None,
                content: "another result".into(),
                attachments: Vec::new(),
            }),
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
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do it"),
            ChatMessage::assistant_tool_calls(
                "checking",
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_abc123".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_abc123".into(),
                tool_name: "".into(),
                success: None,
                content: "ok".into(),
                attachments: Vec::new(),
            }),
            msg("assistant", "done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 0);
        assert_eq!(messages.len(), 5);
    }

    #[test]
    fn multi_tool_call_batch_preserved() {
        // An assistant with 3 tool_calls followed by 3 tool results.
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do all 3"),
            ChatMessage::assistant_tool_calls(
                "running",
                vec![
                    zeroclaw_api::model_provider::ToolCall {
                        id: "toolu_aaa".into(),
                        name: "shell".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                    zeroclaw_api::model_provider::ToolCall {
                        id: "toolu_bbb".into(),
                        name: "shell".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                    zeroclaw_api::model_provider::ToolCall {
                        id: "toolu_ccc".into(),
                        name: "shell".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                ],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_aaa".into(),
                tool_name: "".into(),
                success: None,
                content: "r1".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_bbb".into(),
                tool_name: "".into(),
                success: None,
                content: "r2".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_ccc".into(),
                tool_name: "".into(),
                success: None,
                content: "r3".into(),
                attachments: Vec::new(),
            }),
            msg("assistant", "all done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 0);
        assert_eq!(messages.len(), 7);
    }

    #[test]
    fn mismatched_tool_id_is_removed() {
        // Tool result references a tool_call_id not in the assistant message.
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "go"),
            ChatMessage::assistant_tool_calls(
                "running",
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_aaa".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_aaa".into(),
                tool_name: "".into(),
                success: None,
                content: "ok".into(),
                attachments: Vec::new(),
            }),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_GONE".into(),
                tool_name: "".into(),
                success: None,
                content: "stale".into(),
                attachments: Vec::new(),
            }),
            msg("assistant", "done"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(pruned.removed, 1);
        assert_eq!(messages.len(), 5);
        // The valid tool result stays, the orphan is gone.
        assert_eq!(messages[3].role, "tool");
        assert_eq!(
            messages[3]
                .tool_result
                .as_ref()
                .map(|r| r.tool_call_id.as_str()),
            Some("toolu_aaa")
        );
    }

    #[test]
    fn orphan_tool_in_middle_after_collapsed_pair() {
        // Phase 1 collapsed an assistant+tool pair into a summary, but
        // a subsequent tool message referenced the original tool_call_id.
        let mut messages = vec![
            msg("system", "sys"),
            msg("assistant", "[Tool result: truncated...]"), // collapsed
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_OLD".into(),
                tool_name: "".into(),
                success: None,
                content: "leftover".into(),
                attachments: Vec::new(),
            }),
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
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do something"),
            msg("assistant", "Let me check."),
            ChatMessage::assistant_tool_calls(
                String::new(),
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_LIVE".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_LIVE".into(),
                tool_name: "".into(),
                success: None,
                content: "ok".into(),
                attachments: Vec::new(),
            }),
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
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do something"),
            ChatMessage::assistant_tool_calls(
                String::new(),
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_LOST".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
            ChatMessage::assistant_tool_calls(
                String::new(),
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_DEAD".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_DEAD".into(),
                tool_name: "".into(),
                success: None,
                content: "ok".into(),
                attachments: Vec::new(),
            }),
            msg("assistant", "summary"),
        ];
        let pruned = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(
            pruned.removed, 2,
            "second dispatch + its tool_result must be removed when prior dispatch is unresolved"
        );
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].tool_calls[0].id, "toolu_LOST");
        assert_eq!(messages[3].content, "summary");
    }

    #[test]
    fn tool_without_parseable_id_kept_if_assistant_has_tool_calls() {
        // Conservative: if we can't parse the tool_call_id, keep the
        // message as long as the preceding assistant has tool_calls.
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "go"),
            ChatMessage::assistant_tool_calls(
                "running",
                vec![zeroclaw_api::model_provider::ToolCall {
                    id: "toolu_x".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                None,
            ),
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
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "toolu_01Orphan".into(),
                tool_name: "".into(),
                success: None,
                content: "stale".into(),
                attachments: Vec::new(),
            }),
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
        let mut messages = vec![
            msg("system", "You are a helpful assistant"),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "chatcmpl-tool-92a12a15c14f3b36".into(),
                tool_name: "".into(),
                success: None,
                content: "search results".into(),
                attachments: Vec::new(),
            }),
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
