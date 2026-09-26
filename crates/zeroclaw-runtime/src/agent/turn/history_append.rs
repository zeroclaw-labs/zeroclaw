//! History append for one tool round: the assistant message plus per-call
//! `role=tool` messages (native) or a `[Tool results]` user message (prompt
//! mode).

use zeroclaw_providers::{ChatMessage, ToolCall};

pub(crate) fn append_tool_round_to_history(
    history: &mut Vec<ChatMessage>,
    assistant_history_content: String,
    native_tool_calls: &[ToolCall],
    individual_results: &[(Option<String>, String)],
    tool_results: &str,
    use_native_tools: bool,
) {
    history.push(ChatMessage::assistant(assistant_history_content));
    if native_tool_calls.is_empty() {
        let all_results_have_ids = use_native_tools
            && !individual_results.is_empty()
            && individual_results
                .iter()
                .all(|(tool_call_id, _)| tool_call_id.is_some());
        if all_results_have_ids {
            for (tool_call_id, result) in individual_results {
                let tool_msg = serde_json::json!({
                    "tool_call_id": tool_call_id,
                    "content": result,
                });
                history.push(ChatMessage::tool(tool_msg.to_string()));
            }
        } else {
            history.push(ChatMessage::user(format!("[Tool results]\n{tool_results}")));
        }
    } else {
        // `zip` would drop trailing results on any length divergence,
        // leaving a native tool_use id with no matching tool_result.
        // Pair on each result's own id instead.
        for (idx, (tool_call_id, result)) in individual_results.iter().enumerate() {
            let resolved_id = tool_call_id
                .clone()
                .or_else(|| native_tool_calls.get(idx).map(|call| call.id.clone()));
            let tool_msg = serde_json::json!({
                "tool_call_id": resolved_id,
                "content": result,
            });
            history.push(ChatMessage::tool(tool_msg.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::append_tool_round_to_history;
    use crate::agent::prompt::redact_session_prompt_tool_exchanges_for_export;
    use zeroclaw_providers::ChatMessage;

    #[test]
    fn json_tool_calls_text_fallback_result_is_redacted_only_in_export_copies() {
        let marker = "session-prompt-private-marker";
        let assistant =
            format!(r#"{{"tool_calls":[{{"name":"session_prompt_list","arguments":{{}}}}]}}"#);
        let result = format!("<tool_result name=\"session_prompt_list\">{marker}</tool_result>");
        let mut history: Vec<ChatMessage> = Vec::new();

        append_tool_round_to_history(
            &mut history,
            assistant,
            &[],
            &[(None, result.clone())],
            &result,
            false,
        );

        assert!(
            history[1].content.contains(marker),
            "the provider's working history keeps the explicit list result"
        );
        let exported = redact_session_prompt_tool_exchanges_for_export(&history);
        assert!(
            exported
                .iter()
                .all(|message| !message.content.contains(marker)),
            "generic exports must not retain the opaque attachment body"
        );
    }
}
