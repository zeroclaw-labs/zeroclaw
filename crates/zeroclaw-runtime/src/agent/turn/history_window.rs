//! Pre-iteration history maintenance: orphaned tool-message removal and
//! system-message normalization. No preemptive token-budget trimming runs
//! here; context trimming is reactive and turn-bounded (see
//! `trim_to_recent_turns`).

use crate::agent::history::normalize_system_messages;
use zeroclaw_providers::ChatMessage;

pub(crate) fn preflight_history_maintenance(history: &mut Vec<ChatMessage>) {
    // Remove orphaned tool-role messages whose assistant (tool_calls)
    // counterpart was dropped by turn-boundary trimming or session history
    // reloading. Without this, model_providers like MiniMax reject the
    // request with "tool result's tool id not found" (bug #7727).
    //
    // Only the orphaned tool-role half is removed here. Repairing the other
    // half — an assistant tool_call whose result is missing — is deliberately
    // left to the provider adapters, which have the pairing context this flat,
    // shared pass lacks: the OpenAI-compatible adapter borrows the preceding
    // call id for an id-less result, and the Anthropic adapter backfills an
    // interrupted call. Stripping the assistant call here — on flat provider
    // JSON that cannot tell a native `AssistantToolCalls` from a model-authored
    // answer that merely contains a `tool_calls` array — would defeat both and
    // could rewrite an ordinary assistant answer. Restored ACP transcripts are
    // instead repaired earlier, while still typed `ConversationMessage` data
    // (see `repair_incomplete_tool_calls` on the ACP `session/load` path).
    let pruned_in_loop = crate::agent::history_pruner::remove_orphaned_tool_messages(history);
    if !pruned_in_loop.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "removed": pruned_in_loop.removed,
                    "orphan_tool_call_ids": pruned_in_loop.orphan_tool_call_ids,
                })),
            "remove_orphaned_tool_messages fired inside run_tool_call_loop: \
             assistant tool_use blocks and/or tool_results were stripped from \
             the live history. If this fires mid-conversation the model loses \
             the in-flight tool work and acts like it just woke up."
        );
    }
    normalize_system_messages(history);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    // An assistant native call followed by an id-less tool result is a shape the
    // OpenAI-compatible adapter repairs by borrowing the preceding call id. The
    // shared preflight must leave both halves in place so that fallback stays
    // reachable — it must not strip the assistant call on this flat history.
    #[test]
    fn preflight_keeps_call_and_idless_result_for_adapter_fallback() {
        let assistant =
            r#"{"content":null,"tool_calls":[{"id":"fc_123","name":"shell","arguments":"{}"}]}"#;
        let idless_result = r#"{"content":"pwd output"}"#;
        let mut history = vec![
            msg("user", "run pwd"),
            msg("assistant", assistant),
            msg("tool", idless_result),
        ];

        preflight_history_maintenance(&mut history);

        assert_eq!(
            history.len(),
            3,
            "neither the assistant call nor the id-less result may be removed: {history:?}"
        );
        assert!(
            history[1].content.contains("fc_123"),
            "assistant native call must survive for the adapter fallback: {}",
            history[1].content
        );
        assert_eq!(history[2].content, idless_result);
    }

    // Ordinary model-authored assistant JSON that merely contains a `tool_calls`
    // array (no tool results at all) must pass through untouched — the shared
    // preflight can no longer rewrite or delete it based on JSON shape.
    #[test]
    fn preflight_leaves_ordinary_assistant_json_untouched() {
        let assistant_json =
            r#"{"answer":"here","tool_calls":[{"id":"x","name":"noop","arguments":"{}"}]}"#;
        let mut history = vec![msg("user", "hi"), msg("assistant", assistant_json)];

        preflight_history_maintenance(&mut history);

        assert_eq!(history.len(), 2, "no message may be dropped: {history:?}");
        assert_eq!(
            history[1].content, assistant_json,
            "ordinary assistant JSON must not be rewritten"
        );
    }
}
