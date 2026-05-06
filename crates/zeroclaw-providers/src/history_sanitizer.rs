//! Provider-agnostic conversation-history sanitization.
//!
//! Some providers (notably Google Gemini) reject conversation histories whose
//! first non-system turn is anything other than a `user` turn. ZeroClaw can
//! produce such histories when context trimming, session restoration, or
//! native-tool-call serialization leaves an `assistant` turn (often carrying
//! `tool_calls`) at the head of the message list.
//!
//! Permissive providers (Anthropic, GLM) silently accept the malformed shape;
//! strict providers return HTTP 400. See issue #6302 for the full repro.
//!
//! This module enforces the universal invariant: the first non-system message
//! must be a `user` turn. Any leading `assistant` / `tool` turns that precede
//! the first `user` turn are dropped, since without their corresponding
//! `user` predecessor they are not interpretable by any provider.
//!
//! Tool-call/tool-response *pairing* (orphan `tool` messages without a matching
//! `assistant.tool_calls`, empty `tool_calls: []` arrays, etc.) is tracked
//! separately in #6298 and is intentionally out of scope here.

use zeroclaw_api::provider::ChatMessage;

/// Drop leading non-`user`, non-`system` messages so the first non-system
/// turn is always `user`. Returns the number of messages removed.
///
/// Operates in place. System messages keep their position (providers that
/// support a dedicated system slot extract them separately, others forward
/// them inline; both cases are unaffected by this pass).
pub fn enforce_leading_user_turn(messages: &mut Vec<ChatMessage>) -> usize {
    let first_non_system = messages.iter().position(|m| m.role != "system");
    let Some(start) = first_non_system else {
        return 0;
    };

    let mut drop_to = start;
    while drop_to < messages.len() && messages[drop_to].role != "user" {
        drop_to += 1;
    }

    if drop_to == start {
        return 0;
    }

    if drop_to >= messages.len() {
        // No `user` turn anywhere after the system block — leave the messages
        // alone rather than silently producing an empty conversation. The
        // caller will surface the upstream error normally.
        return 0;
    }

    let removed = drop_to - start;
    messages.drain(start..drop_to);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
        }
    }

    #[test]
    fn no_op_when_first_non_system_is_user() {
        let mut messages = vec![
            msg("system", "you are helpful"),
            msg("user", "hi"),
            msg("assistant", "hello"),
        ];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn no_op_when_only_system_messages() {
        let mut messages = vec![msg("system", "a"), msg("system", "b")];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn no_op_when_empty() {
        let mut messages: Vec<ChatMessage> = Vec::new();
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 0);
    }

    #[test]
    fn drops_leading_assistant_with_tool_calls() {
        // Reproduces the exact shape captured for issue #6302.
        let mut messages = vec![
            msg("system", "preamble"),
            msg(
                "assistant",
                r#"{"content":"","tool_calls":[{"id":"c1","name":"x","arguments":"{}"}]}"#,
            ),
            msg("tool", "result"),
            msg("assistant", "interim"),
            msg("user", "respond ok"),
        ];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 3);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, "respond ok");
    }

    #[test]
    fn drops_leading_assistant_when_no_system() {
        let mut messages = vec![
            msg("assistant", "stranded"),
            msg("user", "hello"),
            msg("assistant", "hi"),
        ];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages[0].role, "user");
    }

    #[test]
    fn drops_leading_tool_response() {
        let mut messages = vec![
            msg("system", "preamble"),
            msg("tool", "orphan response"),
            msg("user", "hi"),
        ];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn preserves_messages_when_no_user_turn_exists() {
        // Conservative: don't synthesize an empty conversation. Let the
        // provider return its native error for the caller to surface.
        let mut messages = vec![
            msg("system", "preamble"),
            msg("assistant", "stranded"),
            msg("tool", "stranded too"),
        ];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn keeps_all_system_messages_in_place() {
        let mut messages = vec![
            msg("system", "a"),
            msg("system", "b"),
            msg("assistant", "drop me"),
            msg("user", "real msg"),
        ];
        let removed = enforce_leading_user_turn(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "system");
        assert_eq!(messages[2].role, "user");
    }
}
