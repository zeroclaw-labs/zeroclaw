//! Legacy transcript normalization shared by migration and entrance views.
use crate::model_provider::{ChatMessage, ConversationMessage};
const MAX_PERSISTED_TOOL_OUTPUT_BYTES: usize = 16 * 1024;
const SYNTHETIC_INTERRUPTION_ROLE: &str = "__zeroclaw_turn_stream_interrupted__";
pub fn bounded_transcript_messages(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
    messages
        .iter()
        .cloned()
        .map(|message| match message {
            ConversationMessage::ToolResults(mut results) => {
                for result in &mut results {
                    result.content = bounded_tool_output(&result.content);
                }
                ConversationMessage::ToolResults(results)
            }
            other => other,
        })
        .collect()
}

pub fn bounded_tool_output(output: &str) -> String {
    if output.len() <= MAX_PERSISTED_TOOL_OUTPUT_BYTES {
        return output.to_string();
    }
    let mut end = MAX_PERSISTED_TOOL_OUTPUT_BYTES;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &output[..end])
}

/// Produce provider-safe ACP history while preserving client-visible text.
/// Only an immediately adjacent tool-call/result pair is retained, and one
/// result is kept for each unambiguous call id. Duplicate call or result IDs
/// within a batch are rejected; recovery markers stay transcript-only.
pub fn provider_safe_history(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
    let mut repaired = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        match &messages[index] {
            ConversationMessage::Chat(chat)
                if chat.role == "system" || chat.role == SYNTHETIC_INTERRUPTION_ROLE =>
            {
                index += 1;
            }
            ConversationMessage::Chat(_) => {
                repaired.push(messages[index].clone());
                index += 1;
            }
            ConversationMessage::AssistantToolCalls {
                text,
                tool_calls,
                reasoning_content,
            } => {
                let adjacent_results = messages.get(index + 1).and_then(|message| match message {
                    ConversationMessage::ToolResults(results) => Some(results),
                    _ => None,
                });
                let mut paired_calls = Vec::new();
                let mut paired_results = Vec::new();
                if let Some(results) = adjacent_results {
                    let mut call_id_counts = std::collections::HashMap::new();
                    for call in tool_calls {
                        *call_id_counts.entry(call.id.as_str()).or_insert(0usize) += 1;
                    }
                    for call in tool_calls {
                        // Reserving a result is insufficient: even with two
                        // results, duplicated call IDs cannot identify a pair.
                        if call_id_counts.get(call.id.as_str()) != Some(&1) {
                            continue;
                        }
                        let mut matching_results = results
                            .iter()
                            .filter(|result| result.tool_call_id == call.id);
                        if let Some(result) = matching_results.next()
                            && matching_results.next().is_none()
                        {
                            paired_calls.push(call.clone());
                            paired_results.push(result.clone());
                        }
                    }
                }

                if paired_calls.is_empty() {
                    if let Some(text) = text.as_ref().filter(|text| !text.is_empty()) {
                        repaired.push(ConversationMessage::Chat(ChatMessage::assistant(text)));
                    }
                } else {
                    repaired.push(ConversationMessage::AssistantToolCalls {
                        text: text.clone(),
                        tool_calls: paired_calls,
                        reasoning_content: reasoning_content.clone(),
                    });
                    repaired.push(ConversationMessage::ToolResults(paired_results));
                }
                index += if adjacent_results.is_some() { 2 } else { 1 };
            }
            ConversationMessage::ToolResults(_) => {
                index += 1;
            }
        }
    }
    repaired
}
