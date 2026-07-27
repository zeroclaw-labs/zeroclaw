//! Pre-iteration history maintenance: orphaned tool-message removal and
//! system-message normalization. No preemptive token-budget trimming runs
//! here; context trimming is reactive and turn-bounded (see
//! `trim_to_recent_turns`).

use crate::agent::history::normalize_system_messages;
use zeroclaw_providers::ChatMessage;

pub(crate) fn preflight_history_maintenance(history: &mut Vec<ChatMessage>) {
    // Repair both halves of an incomplete native-tool exchange before the
    // provider sees restored or externally modified history. An interrupted
    // turn can leave either an assistant tool_call without its result or a
    // tool result whose assistant dispatch was dropped.
    let stripped_calls =
        crate::agent::history_pruner::strip_orphaned_tool_calls_from_assistants(history);
    let pruned_in_loop = crate::agent::history_pruner::remove_orphaned_tool_messages(history);
    if stripped_calls > 0 || !pruned_in_loop.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "assistant_messages_repaired": stripped_calls,
                    "removed": pruned_in_loop.removed,
                    "orphan_tool_call_ids": pruned_in_loop.orphan_tool_call_ids,
                })),
            "history pairing repair fired inside run_tool_call_loop: \
             orphaned assistant tool_use blocks and/or tool_results were stripped from \
             the live history. If this fires mid-conversation the model loses \
             the in-flight tool work and acts like it just woke up."
        );
    }
    normalize_system_messages(history);
}
