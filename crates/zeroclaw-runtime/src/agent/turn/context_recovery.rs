//! LLM-failure recording and in-loop context-overflow recovery.

use super::context::TurnCtx;
use super::outcome::is_tool_loop_cancelled;
use crate::agent::history::estimate_history_tokens;
use crate::agent::history_trim::trim_to_recent_turns;
use crate::observability::{Observer, ObserverEvent};
use std::time::Instant;
use zeroclaw_providers::ChatMessage;

/// Record a failed provider call: observer `LlmResponse` (failure) and the
/// `llm_response` failure log line.
pub(crate) fn record_llm_failure(
    ctx: &TurnCtx<'_>,
    llm_started_at: Instant,
    iteration: usize,
    e: &anyhow::Error,
) {
    // User cancellation gets the fixed message the streaming consumers have
    // always seen (and pin), never a raw error string.
    let safe_error = if is_tool_loop_cancelled(e) {
        "request cancelled by user".to_string()
    } else {
        zeroclaw_providers::sanitize_api_error(&e.to_string())
    };
    ctx.observer.record_event(&ObserverEvent::LlmResponse {
        model_provider: ctx.provider_name.to_string(),
        model: ctx.model.to_string(),
        duration: llm_started_at.elapsed(),
        success: false,
        error_message: Some(safe_error.clone()),
        input_tokens: None,
        output_tokens: None,
        channel: Some(ctx.channel_name.to_string()),
        agent_alias: ctx.agent_alias.map(|s| s.to_string()),
        parent_agent_alias: ctx.parent_agent_alias.map(|s| s.to_string()),
        turn_id: Some(ctx.turn_id.to_string()),
        // Error path: no prompt/completion content captured.
        messages: None,
    });
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_category(::zeroclaw_log::EventCategory::Provider)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_duration(u64::try_from(llm_started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
            .with_attrs(::serde_json::json!({
                "model": ctx.model,
                "iteration": iteration + 1,
                "error": safe_error,
                "trace_id": ctx.turn_id,
            })),
        "llm_response"
    );
}

pub(crate) async fn try_recover_context_overflow(
    history: &mut Vec<ChatMessage>,
    e: &anyhow::Error,
    iteration: usize,
    event_tx: Option<&tokio::sync::mpsc::Sender<zeroclaw_api::agent::TurnEvent>>,
    observer: &dyn Observer,
    context_token_budget: usize,
) -> bool {
    if zeroclaw_providers::reliable::is_context_window_exceeded(e) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_attrs(::serde_json::json!({"iteration": iteration + 1})),
            "Context window exceeded, attempting in-loop recovery"
        );

        // One rule: drop oldest whole turns until we are under a budget
        // forced below the current size. Never splits a tool_use/tool_result
        // pair, never silently shrinks a result. Whole turns or nothing.
        let tokens_now = estimate_history_tokens(history);
        let budget = tokens_now.saturating_mul(2) / 3;
        let owned = std::mem::take(history);
        let result = trim_to_recent_turns(owned, budget);
        let trimmed = result.trimmed;
        let dropped_turns = result.dropped_turns;
        let dropped_messages = result.dropped_messages;
        let kept_turns = result.kept_turns;
        let tokens_after = result.tokens_after;
        let mut recovered_history = result.history;
        if trimmed {
            // Insert the same model-visible breadcrumb the turn-boundary path
            // uses, after the leading system messages, so the retried provider
            // call tells the model earlier turns were dropped (never silent to
            // the model, not just to clients).
            let system_count = recovered_history
                .iter()
                .take_while(|m| m.role == "system")
                .count();
            recovered_history.insert(system_count, crate::agent::history_trim::breadcrumb());
        }
        *history = recovered_history;
        if trimmed {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "dropped_turns": dropped_turns,
                        "dropped_messages": dropped_messages,
                        "tokens_before": tokens_now,
                        "tokens_after": tokens_after,
                    })),
                "Context recovery: dropped oldest whole turns, retrying"
            );
            let reason = crate::i18n::get_required_cli_string("history-trim-reason-budget");
            if let Some(tx) = event_tx {
                let _ = tx
                    .send(zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
                        dropped_messages,
                        kept_turns,
                        reason: reason.clone(),
                    })
                    .await;
            }
            observer.record_event(&ObserverEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                reason,
                channel: None,
                agent_alias: None,
                turn_id: None,
            });
            return true;
        }

        let system_floor = crate::agent::history::estimate_system_floor_tokens(history);
        if system_floor >= context_token_budget {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "system_floor": system_floor,
                        "budget": context_token_budget,
                        "error_key": "context_floor_exceeds_budget",
                    })),
                crate::agent::history::context_floor_remediation(
                    system_floor,
                    context_token_budget,
                )
            );
        } else {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "Context overflow unrecoverable: only one turn left, cannot trim further"
            );
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::NoopObserver;
    use zeroclaw_providers::ChatMessage;

    fn overflowing_history() -> Vec<ChatMessage> {
        let big = "x".repeat(4000);
        let mut h = vec![ChatMessage::system("system")];
        for i in 0..6 {
            h.push(ChatMessage::user(format!("turn {i} {big}").as_str()));
            h.push(ChatMessage::assistant(format!("reply {i} {big}").as_str()));
        }
        h
    }

    #[tokio::test]
    async fn recovery_emits_history_trimmed_event_on_trim() {
        let mut history = overflowing_history();
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered =
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), &observer, 32_000).await;

        assert!(recovered, "an overflowing history must trim and recover");
        // The retried history must carry the model-visible breadcrumb after the
        // leading system messages, matching the turn-boundary contract.
        let breadcrumb_text = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
        assert!(
            history.iter().any(|m| m.content == breadcrumb_text),
            "recovery must insert the breadcrumb so the model sees the trim"
        );
        let event = rx.try_recv().expect("recovery must emit a TurnEvent");
        match event {
            zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                reason,
            } => {
                assert!(dropped_messages > 0, "must report dropped messages");
                assert!(kept_turns >= 1, "must keep at least the current turn");
                assert_eq!(
                    reason,
                    crate::i18n::get_required_cli_string("history-trim-reason-budget")
                );
            }
            other => panic!("expected HistoryTrimmed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn floor_exceeds_budget_single_turn_does_not_recover() {
        // the system prompt + tool definitions alone dominate
        // the budget and only one turn exists. Recovery must NOT loop — it
        // returns false (nothing left to drop) so the caller breaks instead of
        // re-running the same turn forever.
        let big = "x".repeat(8000);
        let mut history = vec![
            ChatMessage::system(format!("system {big}").as_str()),
            ChatMessage::user("only turn"),
            ChatMessage::assistant("reply"),
        ];
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered =
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), &observer, 100).await;

        assert!(
            !recovered,
            "single-turn floor overflow must not retry (no #5808 loop)"
        );
        assert!(
            rx.try_recv().is_err(),
            "no trim event when nothing can be dropped"
        );
        // The system floor must dominate the recovery budget — this is what
        // makes the new remediation branch fire.
        assert!(
            crate::agent::history::estimate_system_floor_tokens(&history)
                >= estimate_history_tokens(&history) * 2 / 3,
            "system floor should dominate the recovery budget in the #5808 case"
        );
    }

    #[tokio::test]
    async fn non_overflow_error_is_not_recovered_and_emits_nothing() {
        let mut history = overflowing_history();
        let err = anyhow::Error::msg("some unrelated provider error");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered =
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), &observer, 32_000).await;

        assert!(!recovered, "a non-overflow error must not trigger recovery");
        assert!(rx.try_recv().is_err(), "no event on the non-overflow path");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn floor_exceeds_budget_emits_event_with_resolved_budget_and_remediation() {
        // Serialize against the broadcast-hook tests for the whole test: we drive
        // `record!` -> LogCaptureLayer -> broadcast hook, and a parallel
        // `clear_broadcast_hook` would otherwise drop our event.
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();

        // System prompt + tool definitions dominate; a single turn means nothing
        // can be trimmed, so the floor-dominates-budget remediation branch fires.
        let big = "x".repeat(8000);
        let mut history = vec![
            ChatMessage::system(format!("system {big}").as_str()),
            ChatMessage::user("only turn"),
            ChatMessage::assistant("reply"),
        ];
        let err = anyhow::Error::msg("maximum context length exceeded");
        let observer = NoopObserver;
        let budget = 100usize;

        // Drain any pre-existing broadcast traffic from parallel tests.
        while rx.try_recv().is_ok() {}

        let recovered =
            try_recover_context_overflow(&mut history, &err, 1, None, &observer, budget).await;
        assert!(!recovered, "floor-dominates overflow must not recover");

        // Read the emitted `context_floor_exceeds_budget` record within a 2s
        // deadline, tolerating `Lagged` from parallel broadcast traffic.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let record = loop {
            if std::time::Instant::now() >= deadline {
                panic!("did not observe the context_floor_exceeds_budget record in time");
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("attributes")
                        .and_then(|a| a.get("error_key"))
                        .and_then(|v| v.as_str())
                        == Some("context_floor_exceeds_budget")
                    {
                        break value;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    panic!("broadcast closed before the record arrived")
                }
                Err(_elapsed) => {}
            }
        };

        let attrs = record.get("attributes").expect("record carries attributes");
        // The recorded budget is the RESOLVED budget passed in, not the local
        // 2/3-of-current recovery budget.
        assert_eq!(
            attrs.get("budget").and_then(|v| v.as_u64()),
            Some(budget as u64),
            "emitted budget must be the resolved effective budget"
        );
        assert!(
            attrs.get("system_floor").and_then(|v| v.as_u64()).unwrap() >= budget as u64,
            "system_floor must meet or exceed the resolved budget in this branch"
        );
        // The visible message names the resolved budget and the runtime-profile
        // surface, and never the inert agent.max_context_tokens wording.
        let message = record
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            message.contains("100"),
            "remediation message must name the resolved budget: {message}"
        );
        assert!(
            message.contains("[runtime_profiles"),
            "remediation message must name the runtime-profile surface: {message}"
        );
        assert!(
            !message.contains("agent.max_context_tokens"),
            "remediation message must not reference the inert knob: {message}"
        );

        zeroclaw_log::clear_broadcast_hook();
    }
}
