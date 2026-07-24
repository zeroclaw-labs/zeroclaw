//! Pre-iteration history maintenance: orphaned tool-message removal and
//! system-message normalization. No preemptive token-budget trimming runs
//! here; context trimming is reactive and turn-bounded (see
//! `trim_to_recent_turns`).

use crate::agent::history::normalize_system_messages;
use zeroclaw_providers::ChatMessage;

pub(crate) fn preflight_history_maintenance(history: &mut Vec<ChatMessage>) {
    // Remove orphaned tool-role messages whose assistant (tool_calls)
    // counterpart was dropped by turn-boundary trimming or session history
    // reloading.  Without this, model_providers like MiniMax reject the
    // request with "tool result's tool id not found" (bug
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
