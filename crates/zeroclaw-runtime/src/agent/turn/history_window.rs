//! Pre-iteration history maintenance: preemptive token-budget trimming,
//! orphaned tool-message removal, and system-message normalization.

use crate::agent::history::{
    estimate_history_tokens, fast_trim_tool_results, normalize_system_messages,
};
use zeroclaw_providers::ChatMessage;

pub(crate) fn preflight_history_maintenance(
    history: &mut Vec<ChatMessage>,
    context_token_budget: usize,
    iteration: usize,
) {
    // Preemptive context management: trim history before it overflows
    if context_token_budget > 0 {
        let estimated = estimate_history_tokens(history);
        if estimated > context_token_budget {
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete).with_category(::zeroclaw_log::EventCategory::Agent).with_attrs(::serde_json::json!({"estimated": estimated, "budget": context_token_budget, "iteration": iteration + 1})), "Preemptive context trim: estimated tokens exceed budget");
            let chars_saved = fast_trim_tool_results(history, 4);
            if chars_saved > 0 {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_attrs(::serde_json::json!({"chars_saved": chars_saved})),
                    "Preemptive fast-trim applied"
                );
            }
            // If still over budget, use the history pruner for deeper cleanup
            let recheck = estimate_history_tokens(history);
            if recheck > context_token_budget {
                let stats = crate::agent::history_pruner::prune_history(
                    history,
                    &crate::agent::history_pruner::HistoryPrunerConfig {
                        enabled: true,
                        max_tokens: context_token_budget,
                        keep_recent: 4,
                        collapse_tool_results: true,
                    },
                );
                if stats.dropped_messages > 0 || stats.collapsed_pairs > 0 {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete).with_category(::zeroclaw_log::EventCategory::Agent).with_attrs(::serde_json::json!({"collapsed": stats.collapsed_pairs, "dropped": stats.dropped_messages})), "Preemptive history prune applied");
                }
            }
        }
    }

    // Remove orphaned tool-role messages whose assistant (tool_calls)
    // counterpart was dropped by proactive trimming, context compression,
    // or session history reloading.  Without this, model_providers like MiniMax
    // reject the request with "tool result's tool id not found" (bug #5743).
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
