//! Failed-turn media degradation, shared by the ACP restore and live-session paths.

use zeroclaw_api::model_provider::ConversationMessage;

/// Whether this typed message opens a turn for failed-turn span purposes.
///
/// Tool results are stored separately from user messages, so a selector that
/// walks back to the last user message naturally finds the turn opener.
pub fn is_turn_opening_user_message(message: &ConversationMessage) -> bool {
    matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == "user"
    )
}

/// Remove explicit attachments from a failed-turn provider projection.
/// Literal message text never grants media capability and survives unchanged.
pub fn degrade_media_in_message(message: &mut ConversationMessage) -> usize {
    let omitted = crate::i18n::get_required_cli_string("turn-failed-attachment-omitted");
    match message {
        ConversationMessage::Chat(chat) => {
            let count = chat.attachments.len();
            if count > 0 {
                chat.attachments.clear();
                chat.append_note(&omitted);
            }
            count
        }
        ConversationMessage::ToolResults(results) => results
            .iter_mut()
            .map(|result| {
                let count = result.attachments.len();
                if count > 0 {
                    result.attachments.clear();
                    if !result.content.is_empty() {
                        result.content.push_str("\n\n");
                    }
                    result.content.push_str(&omitted);
                }
                count
            })
            .sum(),
        _ => 0,
    }
}

/// Omit structured attachments in this message span with an explanatory note.
///
/// A turn that ended in a non-retryable failure may have had its attachments
/// rejected by the provider. That rejection is durable knowledge about those
/// attachments: resending them on the next request reproduces the same
/// rejection, so one bad attachment would poison every later prompt on the
/// session. An omission note is added so the
/// model still sees that something was attached; surrounding text survives.
///
/// Returns the number of attachment references degraded. Callers own the span
/// policy (which messages belong to the failed turn); this helper only
/// performs the projection.
pub fn degrade_media_in_messages(messages: &mut [ConversationMessage]) -> usize {
    messages.iter_mut().map(degrade_media_in_message).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::{
        media::{AttachmentKind, AttachmentReference},
        model_provider::{ChatMessage, ToolResultMessage},
    };
    fn attachment() -> AttachmentReference {
        AttachmentReference {
            kind: AttachmentKind::Image,
            target: "/real.png".into(),
        }
    }
    #[test]
    fn degrades_structured_native_result_without_corrupting_envelope() {
        let mut message = ConversationMessage::Chat(
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: "a".into(),
                tool_name: "".into(),
                success: None,
                content: "[IMAGE:/example]".into(),
                attachments: Vec::new(),
            })
            .with_attachments(vec![attachment()]),
        );
        assert_eq!(degrade_media_in_message(&mut message), 1);
        let ConversationMessage::Chat(chat) = message else {
            panic!("chat")
        };
        let value = serde_json::to_value(&chat).unwrap();
        assert_eq!(value["tool_result"]["tool_call_id"], "a");
        assert!(
            value["content"]
                .as_str()
                .unwrap()
                .starts_with("[IMAGE:/example]")
        );
        assert!(chat.attachments.is_empty());
    }
    #[test]
    fn degrades_typed_tool_results() {
        let mut message = ConversationMessage::ToolResults(vec![ToolResultMessage {
            success: None,
            tool_call_id: "a".into(),
            tool_name: "screenshot".into(),
            content: "saved".into(),
            attachments: vec![attachment()],
        }]);
        assert_eq!(degrade_media_in_message(&mut message), 1);
        assert_eq!(degrade_media_in_message(&mut message), 0);
    }
    #[test]
    fn literal_markers_are_never_interpreted() {
        let mut message = ConversationMessage::Chat(ChatMessage::user("[IMAGE:/example]"));
        assert_eq!(degrade_media_in_message(&mut message), 0);
        let ConversationMessage::Chat(chat) = message else {
            panic!("chat")
        };
        assert_eq!(chat.content, "[IMAGE:/example]");
    }
}
