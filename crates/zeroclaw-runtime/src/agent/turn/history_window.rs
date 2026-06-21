//! Pre-iteration history maintenance: preemptive token-budget trimming,
//! orphaned tool-message removal, and system-message normalization.

use crate::agent::history::{
    estimate_history_tokens, fast_trim_tool_results, normalize_system_messages,
    trim_tool_results_to,
};
use crate::agent::history_pruner::HistoryPrunerConfig;
use zeroclaw_providers::ChatMessage;

/// Progressively smaller head-extract floors (in characters) applied to old
/// tool results before any whole-turn dropping. Keeping a bounded extract in
/// place preserves what each tool returned for every provider — Anthropic
/// drops synthetic collapse summaries entirely, so in-place extracts are the
/// only content that survives there. Only when even 128-char extracts cannot
/// fit the budget does the deeper pruner run.
const EXTRACT_LADDER_FLOORS: [usize; 2] = [512, 128];

pub(crate) fn preflight_history_maintenance(
    history: &mut Vec<ChatMessage>,
    context_token_budget: usize,
    iteration: usize,
    pruning: &HistoryPrunerConfig,
) {
    // Preemptive context management: trim history before it overflows
    if context_token_budget > 0 {
        let estimated = estimate_history_tokens(history);
        if estimated > context_token_budget {
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"estimated": estimated, "budget": context_token_budget, "iteration": iteration + 1})), "Preemptive context trim: estimated tokens exceed budget");
            let chars_saved = fast_trim_tool_results(history, pruning.keep_recent);
            if chars_saved > 0 {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"chars_saved": chars_saved})),
                    "Preemptive fast-trim applied"
                );
            }
            // Still over budget: shrink old tool results to progressively
            // smaller in-place extracts before dropping anything. Each pass
            // keeps the tool messages (and their pairing) but trims the
            // payload, so the model still sees a bounded record of what every
            // tool returned. This is the only content that survives on
            // Anthropic, which drops synthetic collapse summaries outright.
            for floor in EXTRACT_LADDER_FLOORS {
                if estimate_history_tokens(history) <= context_token_budget {
                    break;
                }
                let saved = trim_tool_results_to(history, pruning.keep_recent, floor);
                if saved > 0 {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(
                                ::serde_json::json!({"chars_saved": saved, "floor": floor})
                            ),
                        "Preemptive extract-ladder trim applied"
                    );
                }
            }

            // If even the smallest extracts cannot fit, use the history pruner
            // for deeper cleanup — dropping whole old turns (and, if the agent
            // opted into it, collapsing them). An over-budget request MUST be
            // trimmed (otherwise the provider rejects it with
            // `context_length_exceeded`), so `enabled` is forced on here
            // regardless of the agent's `history_pruning` config. The user's
            // `collapse_tool_results` and `keep_recent` choices ARE honored.
            // When the user explicitly disabled pruning, the forced override is
            // logged at WARN rather than performed silently, so a
            // "history_pruning = false" config does not hide the fact that
            // context is still being shed under pressure.
            let recheck = estimate_history_tokens(history);
            if recheck > context_token_budget {
                if !pruning.enabled {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"estimated": recheck, "budget": context_token_budget, "collapse_tool_results": pruning.collapse_tool_results, "keep_recent": pruning.keep_recent})), "Emergency context trim overriding history_pruning.enabled=false: request is over the context budget and must be trimmed to avoid a provider context_length_exceeded error. Honoring the configured collapse_tool_results and keep_recent.");
                }
                let stats = crate::agent::history_pruner::prune_history(
                    history,
                    &HistoryPrunerConfig {
                        enabled: true,
                        max_tokens: context_token_budget,
                        keep_recent: pruning.keep_recent,
                        collapse_tool_results: pruning.collapse_tool_results,
                    },
                );
                if stats.dropped_messages > 0 || stats.collapsed_pairs > 0 {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"collapsed": stats.collapsed_pairs, "dropped": stats.dropped_messages})), "Preemptive history prune applied");
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
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
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

    fn assistant_call(id: &str) -> ChatMessage {
        ChatMessage::assistant(format!(
            r#"{{"content":null,"tool_calls":[{{"id":"{id}","name":"shell","arguments":"{{}}"}}]}}"#
        ))
    }

    fn tool_result(id: &str) -> ChatMessage {
        tool_result_sized(id, 4000)
    }

    fn tool_result_sized(id: &str, n: usize) -> ChatMessage {
        ChatMessage::tool(format!(
            r#"{{"tool_call_id":"{id}","content":"{}"}}"#,
            "x".repeat(n)
        ))
    }

    /// Build an over-budget history: system + user + N big tool exchanges + a
    /// recent user/assistant turn. Each exchange is an `assistant(tool_calls)`
    /// followed by its `tool` result.
    fn over_budget_history(pairs: usize) -> Vec<ChatMessage> {
        let mut h = vec![ChatMessage::system("sys"), ChatMessage::user("go")];
        for i in 0..pairs {
            h.push(assistant_call(&format!("t{i}")));
            h.push(tool_result(&format!("t{i}")));
        }
        h.push(ChatMessage::user("recent"));
        h.push(ChatMessage::assistant("recent reply"));
        h
    }

    // An over-budget request must be trimmed even when the agent disabled
    // history pruning — otherwise the provider rejects it with
    // context_length_exceeded. `enabled = false` no longer silently suppresses
    // the emergency trim.
    #[test]
    fn emergency_trim_fires_even_when_pruning_disabled() {
        let mut history = over_budget_history(8);
        let before = history.len();
        let budget = 200; // far below the ~8k-token history
        assert!(estimate_history_tokens(&history) > budget);

        preflight_history_maintenance(
            &mut history,
            budget,
            0,
            &HistoryPrunerConfig {
                enabled: false,
                max_tokens: 8192,
                keep_recent: 4,
                collapse_tool_results: true,
            },
        );

        assert!(
            history.len() < before,
            "over-budget history must shrink despite history_pruning.enabled=false"
        );
        // The recent turn is protected by keep_recent and must survive.
        assert!(history.iter().any(|m| m.content == "recent"));
    }

    // `collapse_tool_results` is honored: when false, the emergency trim drops
    // groups instead of leaving synthetic "[Tool exchange: ...]" collapse
    // summaries.
    #[test]
    fn collapse_tool_results_false_is_honored_in_emergency_trim() {
        let mut history = over_budget_history(8);
        preflight_history_maintenance(
            &mut history,
            200,
            0,
            &HistoryPrunerConfig {
                enabled: false,
                max_tokens: 8192,
                keep_recent: 2,
                collapse_tool_results: false,
            },
        );
        assert!(
            !history
                .iter()
                .any(ChatMessage::is_pruned_tool_exchange_summary),
            "collapse_tool_results=false must not produce collapse summaries"
        );
    }

    // Approach B: when bounded in-place extracts are enough to fit the budget,
    // the trim shrinks tool-result payloads but drops NOTHING and produces no
    // content-free collapse summary — every tool message (and its pairing)
    // survives with a truncated-but-present payload, so the model never loses
    // the record of what a tool returned. This is the anti-amnesia guarantee
    // for Anthropic, which would otherwise drop collapsed summaries entirely.
    #[test]
    fn extract_ladder_preserves_messages_and_pairing_without_collapse() {
        // Six exchanges with 3k-char tool results: too big at the 2000-char
        // floor, but they fit once shrunk to small extracts.
        let mut history = vec![ChatMessage::system("sys"), ChatMessage::user("go")];
        for i in 0..6 {
            history.push(assistant_call(&format!("t{i}")));
            history.push(tool_result_sized(&format!("t{i}"), 3000));
        }
        history.push(ChatMessage::user("recent"));
        history.push(ChatMessage::assistant("recent reply"));
        let before_len = history.len();
        let tool_msgs_before = history.iter().filter(|m| m.role == "tool").count();

        // keep_recent = 2 protects only the trailing user/assistant text turn,
        // leaving all six (large) tool results eligible for in-place extract.
        let budget = 800;
        assert!(estimate_history_tokens(&history) > budget);

        preflight_history_maintenance(
            &mut history,
            budget,
            0,
            &HistoryPrunerConfig {
                enabled: false,
                max_tokens: 8192,
                keep_recent: 2,
                collapse_tool_results: true,
            },
        );

        // Nothing dropped, nothing collapsed: pure in-place extract.
        assert_eq!(
            history.len(),
            before_len,
            "extract ladder must not drop messages when extracts fit the budget"
        );
        assert_eq!(
            history.iter().filter(|m| m.role == "tool").count(),
            tool_msgs_before,
            "every tool result message must survive as a real (truncated) message"
        );
        assert!(
            !history
                .iter()
                .any(ChatMessage::is_pruned_tool_exchange_summary),
            "no content-free collapse summary should be produced"
        );
        // Each surviving (unprotected) tool result still carries its id and a
        // non-empty payload — content preserved, not blanked.
        assert!(
            history
                .iter()
                .filter(|m| m.role == "tool")
                .all(|m| m.content.contains("tool_call_id") && m.content.len() > 20)
        );
        assert!(estimate_history_tokens(&history) <= budget);
    }

    #[test]
    fn pipeline_output_has_no_orphaned_tool_messages_across_configs() {
        for keep_recent in [0usize, 1, 2, 3, 5] {
            for collapse_tool_results in [true, false] {
                let mut history = over_budget_history(8);
                preflight_history_maintenance(
                    &mut history,
                    200,
                    0,
                    &HistoryPrunerConfig {
                        enabled: false,
                        max_tokens: 8192,
                        keep_recent,
                        collapse_tool_results,
                    },
                );
                let mut clone = history.clone();
                let pruned =
                    crate::agent::history_pruner::remove_orphaned_tool_messages(&mut clone);
                assert_eq!(
                    pruned.removed, 0,
                    "orphaned tool messages survived preflight trim \
                     (keep_recent={keep_recent}, collapse={collapse_tool_results}): {:?}",
                    pruned.orphan_tool_call_ids
                );
            }
        }
    }

    // With collapse enabled, the emergency trim collapses old exchanges into
    // synthetic summaries (the historical default behaviour, now config-driven).
    #[test]
    fn collapse_tool_results_true_produces_summaries_in_emergency_trim() {
        let mut history = over_budget_history(8);
        preflight_history_maintenance(
            &mut history,
            200,
            0,
            &HistoryPrunerConfig {
                enabled: false,
                max_tokens: 8192,
                keep_recent: 2,
                collapse_tool_results: true,
            },
        );
        assert!(
            history
                .iter()
                .any(ChatMessage::is_pruned_tool_exchange_summary),
            "collapse_tool_results=true should collapse old exchanges into summaries"
        );
    }
}
