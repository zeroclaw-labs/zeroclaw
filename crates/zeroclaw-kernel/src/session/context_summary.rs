//! Session summary policy, independent of provider implementations.
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage, strip_think_tags};
use zeroclaw_api::session_events::capture::ContextSnapshot;

/// Derive retention and the model request from the same frozen projection.
pub(super) fn prepare(mut source: ContextSnapshot) -> (ContextSnapshot, Vec<ConversationMessage>) {
    let kept = ContextSnapshot {
        messages: source.messages.iter().filter(|message| {
            matches!(message, ConversationMessage::Chat(chat) if chat.role == "system")
        }).cloned().collect(),
        history_has_trim_breadcrumb: Some(true),
    };
    source.messages.push(ConversationMessage::Chat(ChatMessage::user(
        "Summarize the conversation for continuation. Preserve unresolved requests, decisions, tool outcomes and relevant file references. Return only the summary.",
    )));
    (kept, source.messages)
}

pub(super) fn validate(text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!strip_think_tags(text).is_empty(), "empty_context_summary");
    Ok(())
}
