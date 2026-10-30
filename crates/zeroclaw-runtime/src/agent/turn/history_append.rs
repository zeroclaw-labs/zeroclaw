//! One canonical tool round for all provider protocols. Runtime messages carry
//! typed calls and results; durable replay groups results by tool round.

use zeroclaw_providers::{ChatMessage, ToolCall};

use super::results_collect::ToolRoundResult;

pub(crate) fn append_tool_round_to_history(
    history: &mut Vec<ChatMessage>,
    assistant_history_content: ChatMessage,
    native_tool_calls: &[ToolCall],
    individual_results: &[ToolRoundResult],
) {
    history.push(assistant_history_content);
    for (idx, result) in individual_results.iter().enumerate() {
        let resolved_id = result
            .tool_call_id
            .as_deref()
            .or_else(|| native_tool_calls.get(idx).map(|call| call.id.as_str()));
        history.push(ChatMessage::tool_result(
            zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: resolved_id.unwrap_or_default().to_owned(),
                tool_name: result.tool_name.clone(),
                success: Some(result.success),
                content: result.output.clone(),
                attachments: result.attachments.clone(),
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::media::{AttachmentKind, AttachmentReference};

    fn image(target: &str) -> AttachmentReference {
        AttachmentReference {
            target: target.to_string(),
            kind: AttachmentKind::Image,
        }
    }

    fn result(
        id: Option<&str>,
        output: &str,
        attachments: Vec<AttachmentReference>,
    ) -> ToolRoundResult {
        ToolRoundResult {
            tool_call_id: id.map(str::to_string),
            tool_name: "test_tool".into(),
            success: true,
            output: output.to_string(),
            attachments,
        }
    }

    #[test]
    fn tool_result_preserves_attachments_as_structured_fields() {
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            ChatMessage::assistant("assistant text"),
            &[],
            &[result(
                Some("call-1"),
                "File: /tmp/shot.png",
                vec![image("/tmp/shot.png")],
            )],
        );

        assert_eq!(history.len(), 2);
        assert_eq!(history[1].role, "tool");
        let value = serde_json::to_value(&history[1]).unwrap();
        assert_eq!(value["tool_result"]["tool_call_id"], "call-1");
        assert_eq!(value["content"], "File: /tmp/shot.png");
        assert_eq!(history[1].attachments, vec![image("/tmp/shot.png")]);
    }

    #[test]
    fn text_only_tool_result_has_no_attachments() {
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            ChatMessage::assistant("assistant text"),
            &[],
            &[result(Some("call-1"), "plain output", Vec::new())],
        );

        let value = serde_json::to_value(&history[1]).unwrap();
        assert_eq!(value["content"], "plain output");
        assert!(history[1].attachments.is_empty());
    }

    #[test]
    fn id_fallback_pairs_result_with_native_call_id() {
        let calls = vec![ToolCall {
            id: "native-id-1".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
            extra_content: None,
        }];
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            ChatMessage::assistant("assistant text"),
            &calls,
            &[result(None, "output", Vec::new())],
        );

        let value = serde_json::to_value(&history[1]).unwrap();
        assert_eq!(value["tool_result"]["tool_call_id"], "native-id-1");
    }
}
