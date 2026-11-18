//! Failed-turn media degradation, shared by the ACP restore and live-session paths.

use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_providers::multimodal;

/// Whether this typed message opens a turn for failed-turn span purposes: a
/// user message that is not the runtime's prompt-mode `[Tool results]` result
/// carrier.
///
/// A prompt-mode tool round appends its results as a user-role message (see
/// `history_append::append_tool_round_to_history`), and typed replay preserves
/// that carrier as an ordinary user chat. A selector that walks back to "the
/// last user message" would otherwise start the failed-turn span at the
/// carrier and miss the user prompt that actually opened the turn — including
/// the attachments that prompt carried. The same prefix rule the whole-turn
/// trimmer uses for flat boundaries applies here.
pub fn is_turn_opening_user_message(message: &ConversationMessage) -> bool {
    matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == "user"
                && !chat
                    .content
                    .starts_with(zeroclaw_api::tool_carrier::TOOL_RESULTS_PREFIX)
    )
}

/// Replace image references in a single message with an omission note.
///
/// The per-message form of [`degrade_media_in_messages`]: callers that carry
/// their own span bookkeeping (for example a provenance-preserving projection
/// of seed rows) apply it row by row, while span-shaped callers keep using the
/// slice form. Returns the number of image references degraded.
///
/// Tool-result carriers degrade only their declared attachments — the count
/// header / array entries — and keep the body verbatim, because a marker in
/// tool body text is text under the attachment-identity contract. A legacy
/// carrier (no declaration) carries no attachments, so nothing degrades and
/// its inline markers survive as text; that is the deliberate behaviour change
/// for pre-upgrade spans.
pub fn degrade_media_in_message(message: &mut ConversationMessage) -> usize {
    let omitted = crate::i18n::get_required_cli_string("turn-failed-attachment-omitted");
    let ConversationMessage::Chat(chat) = message else {
        return 0;
    };
    if let Some(mut parts) = zeroclaw_api::tool_carrier::classify(&chat.role, &chat.content) {
        if !parts.declared || parts.attachments.is_empty() {
            // A legacy carrier declared nothing, so nothing degrades and its
            // inline markers survive as text; a declaration with nothing in
            // it has nothing to drop either. The body is verbatim either way.
            return 0;
        }
        let count = parts.attachments.len();
        parts.text = if parts.text.is_empty() {
            omitted.to_string()
        } else {
            format!("{}\n\n{omitted}", parts.text)
        };
        chat.content =
            zeroclaw_api::tool_carrier::rebuild_carrier(&chat.role, &chat.content, &parts, &[]);
        return count;
    }
    let (cleaned, refs) = multimodal::parse_image_markers(&chat.content);
    if refs.is_empty() {
        return 0;
    }
    chat.content = if cleaned.is_empty() {
        omitted
    } else {
        format!("{cleaned}\n\n{omitted}")
    };
    refs.len()
}

/// Replace image references in this message span with an omission note.
///
/// A turn that ended in a non-retryable failure may have had its attachments
/// rejected by the provider. That rejection is durable knowledge about those
/// attachments: resending them on the next request reproduces the same
/// rejection, so one bad attachment would poison every later prompt on the
/// session. The marker is replaced with a note rather than deleted so the
/// model still sees that something was attached; surrounding text survives.
///
/// Returns the number of image references degraded. Callers own the span
/// policy (which messages belong to the failed turn); this helper only
/// performs the projection.
pub fn degrade_media_in_messages(messages: &mut [ConversationMessage]) -> usize {
    messages.iter_mut().map(degrade_media_in_message).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::model_provider::ChatMessage;

    fn chat(role: &str, content: String) -> ConversationMessage {
        ConversationMessage::Chat(ChatMessage {
            role: role.to_string(),
            content,
        })
    }

    /// A declared native carrier degrades only its attachments array; the
    /// body gains the omission note and otherwise survives verbatim.
    #[test]
    fn degrade_media_in_message_degrades_native_carrier_array_only() {
        let body = "screenshot saved; source mentions [IMAGE:/tmp/example.png] as text";
        let carrier = chat(
            "tool",
            serde_json::json!({
                "tool_call_id": "tc1",
                "content": body,
                "attachments": [{"kind": "image", "target": "/tmp/real.png"}],
            })
            .to_string(),
        );
        let mut message = carrier;
        let degraded = degrade_media_in_message(&mut message);
        assert_eq!(degraded, 1);

        let parsed = match &message {
            ConversationMessage::Chat(chat) => {
                zeroclaw_api::tool_carrier::parse_native_tool_carrier(&chat.content)
            }
            _ => panic!("carrier stays a chat message"),
        }
        .expect("degraded carrier still parses");
        assert!(parsed.declared);
        assert!(parsed.attachments.is_empty());
        assert!(
            parsed.text.contains(body),
            "the body survives verbatim, note appended after it"
        );
        assert!(parsed.text.contains("[IMAGE:/tmp/example.png]"));
        let omitted = crate::i18n::get_required_cli_string("turn-failed-attachment-omitted");
        assert!(parsed.text.contains(omitted.as_str()));
    }

    /// A declared prompt carrier degrades only its count header and marker
    /// lines; the body keeps its inline markers as text.
    #[test]
    fn degrade_media_in_message_degrades_prompt_carrier_header_only() {
        let body = "[Tool attachments: 9]\nnotes mention [IMAGE:/tmp/example.png] inline";
        let carrier = chat(
            "user",
            zeroclaw_api::tool_carrier::render_prompt_tool_carrier(
                body,
                &[zeroclaw_api::media::RenderedMarker {
                    target: "/tmp/real.png".to_string(),
                    kind: zeroclaw_api::media::MarkerKind::Image,
                }],
            ),
        );
        let mut message = carrier;
        let degraded = degrade_media_in_message(&mut message);
        assert_eq!(degraded, 1);

        let parsed = match &message {
            ConversationMessage::Chat(chat) => {
                zeroclaw_api::tool_carrier::parse_prompt_tool_carrier(&chat.content)
            }
            _ => panic!("carrier stays a chat message"),
        }
        .expect("degraded prompt carrier still parses");
        assert!(parsed.declared);
        assert!(parsed.attachments.is_empty());
        let omitted = crate::i18n::get_required_cli_string("turn-failed-attachment-omitted");
        assert_eq!(
            parsed.text,
            format!("{body}\n\n{omitted}"),
            "the body, inline markers included, is untouched apart from the appended note"
        );
    }

    /// Pre-upgrade spans: a legacy carrier's inline tool markers are text
    /// under the attachment-identity contract, so the failed-turn degrade
    /// leaves them alone. This pins the behaviour change from the old
    /// marker-stripping degrade.
    #[test]
    fn degrade_media_in_message_leaves_legacy_carrier_markers_as_text() {
        let legacy_prompt_text =
            format!("[Tool results]\nresult mentions [IMAGE:/tmp/old-inline.png] here");
        let mut message = chat("user", legacy_prompt_text.clone());
        assert_eq!(degrade_media_in_message(&mut message), 0);
        assert!(
            matches!(&message, ConversationMessage::Chat(chat) if chat.content == legacy_prompt_text),
            "a legacy carrier is untouched"
        );

        let legacy_native_text = serde_json::json!({
            "tool_call_id": "tc1",
            "content": "result mentions [IMAGE:/tmp/old-inline.png] here",
        })
        .to_string();
        let mut message = chat("tool", legacy_native_text.clone());
        assert_eq!(degrade_media_in_message(&mut message), 0);
        assert!(
            matches!(&message, ConversationMessage::Chat(chat) if chat.content == legacy_native_text),
            "a legacy native carrier is untouched"
        );
    }

    /// Real user messages still degrade their image markers as before.
    #[test]
    fn degrade_media_in_message_still_degrades_user_markers() {
        let mut message = chat("user", "look at this [IMAGE:/tmp/a.png]".to_string());
        assert_eq!(degrade_media_in_message(&mut message), 1);
        match &message {
            ConversationMessage::Chat(chat) => {
                assert!(!chat.content.contains("[IMAGE:"));
                let omitted =
                    crate::i18n::get_required_cli_string("turn-failed-attachment-omitted");
                assert!(chat.content.contains(omitted.as_str()));
            }
            _ => panic!("user message stays a chat message"),
        }
    }
}
