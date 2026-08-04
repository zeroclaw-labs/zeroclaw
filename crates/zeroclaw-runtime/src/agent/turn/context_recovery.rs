//! LLM-failure recording and in-loop context-overflow recovery.

use super::context::TurnCtx;
use super::events::{ProgressEvent, send_progress};
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
    on_delta: Option<&tokio::sync::mpsc::Sender<super::events::DraftEvent>>,
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
        let mut tokens_after = result.tokens_after;
        let mut recovered_history = result.history;
        if trimmed {
            // Announce compaction only once the trim has actually succeeded.
            // Recognizing the overflow is not enough: a single oversized turn
            // cannot be trimmed, and announcing on recognition would claim
            // work that never happens.
            send_progress(on_delta, ProgressEvent::CompactingContext).await;
            // Insert the same model-visible breadcrumb the turn-boundary path
            // uses, after the leading system messages, so the retried provider
            // call tells the model earlier turns were dropped (never silent to
            // the model, not just to clients).
            let system_count = recovered_history
                .iter()
                .take_while(|m| m.role == "system")
                .count();
            recovered_history.insert(system_count, crate::agent::history_trim::breadcrumb());
            // Recompute from the final recovered history (breadcrumb included)
            // so the reported count matches what the retried call sends.
            tokens_after = crate::agent::history::estimate_history_tokens(&recovered_history);
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
            // This path trims toward `tokens_now * 2 / 3`, not toward
            // `context_token_budget` — a provider can overflow below the
            // configured limit (or with enforcement disabled entirely, where
            // `context_token_budget` is 0). Use a distinct reason so clients
            // do not render this as "trimmed against a Z-token budget" when
            // no configured budget governed the trim. The configured limit is
            // still reported (the configured-budget exposure contract) but only when one is
            // actually set, so a disabled-enforcement recovery does not claim
            // a nonsensical zero-token budget.
            let reason = crate::i18n::get_required_cli_string("history-trim-reason-recovery");
            let reported_token_budget =
                (context_token_budget > 0).then_some(context_token_budget as u64);
            if let Some(tx) = event_tx {
                let _ = tx
                    .send(zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
                        dropped_messages,
                        kept_turns,
                        reason: reason.clone(),
                        token_budget: reported_token_budget,
                        tokens_before: Some(tokens_now as u64),
                        tokens_after: Some(tokens_after as u64),
                        tokens_before_source: Some(
                            zeroclaw_api::agent::TokenCountSource::Estimated,
                        ),
                        tokens_after_source: Some(zeroclaw_api::agent::TokenCountSource::Estimated),
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
                token_budget: reported_token_budget,
                tokens_before: Some(tokens_now as u64),
                tokens_after: Some(tokens_after as u64),
                tokens_before_source: Some(zeroclaw_api::agent::TokenCountSource::Estimated),
                tokens_after_source: Some(zeroclaw_api::agent::TokenCountSource::Estimated),
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

    /// The `CompactingContext` lifecycle state is only reachable through this
    /// recovery path, so it must be exercised with a live draft channel rather
    /// than the `None` sender the other cases use — otherwise the state is
    /// never emitted by any test and only appears in the enumeration tables.
    #[tokio::test]
    async fn recovery_emits_compacting_context_lifecycle_to_the_draft_channel() {
        let mut history = overflowing_history();
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered = try_recover_context_overflow(
            &mut history,
            &err,
            1,
            None,
            Some(&delta_tx),
            &observer,
            32_000,
        )
        .await;

        assert!(recovered, "an overflowing history must trim and recover");
        let delta = delta_rx
            .try_recv()
            .expect("context recovery must emit a lifecycle delta");
        assert!(
            matches!(
                delta,
                super::super::events::StreamDelta::Lifecycle(ProgressEvent::CompactingContext)
            ),
            "context recovery must emit the typed CompactingContext state, got {delta:?}"
        );
    }

    /// An unrelated provider error is not a compaction trigger at all.
    #[tokio::test]
    async fn unrecoverable_error_emits_no_compacting_context_lifecycle() {
        let mut history = vec![ChatMessage::system("system")];
        let err = anyhow::Error::msg("some unrelated provider failure");
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered = try_recover_context_overflow(
            &mut history,
            &err,
            1,
            None,
            Some(&delta_tx),
            &observer,
            32_000,
        )
        .await;

        assert!(!recovered, "a non-overflow error must not report recovery");
        assert!(
            delta_rx.try_recv().is_err(),
            "no lifecycle state may be emitted when compaction never ran"
        );
    }

    /// The case that matters for state accuracy: a genuine context overflow
    /// that cannot be trimmed, because a single oversized turn leaves nothing
    /// to drop. Recognizing the overflow must not announce compaction, or the
    /// user is told the agent is compacting when it provably cannot.
    #[tokio::test]
    async fn overflow_that_cannot_trim_emits_no_compacting_context_lifecycle() {
        // One system message plus a single user turn: `trim_to_recent_turns`
        // always keeps the current turn, so there is no whole turn to drop.
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user(format!("only turn {}", "x".repeat(40_000)).as_str()),
        ];
        let before = history.clone();
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered = try_recover_context_overflow(
            &mut history,
            &err,
            1,
            None,
            Some(&delta_tx),
            &observer,
            32_000,
        )
        .await;

        assert!(
            !recovered,
            "an overflow with a single turn cannot be recovered"
        );
        assert_eq!(
            history.len(),
            before.len(),
            "nothing was trimmable, so history must be unchanged"
        );
        assert!(
            delta_rx.try_recv().is_err(),
            "recognizing an overflow must not announce compaction that cannot happen"
        );
    }

    #[tokio::test]
    async fn recovery_emits_history_trimmed_event_on_trim() {
        let mut history = overflowing_history();
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered =
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), None, &observer, 32_000)
                .await;

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
                token_budget,
                tokens_before,
                tokens_after,
                tokens_before_source,
                tokens_after_source,
            } => {
                assert!(dropped_messages > 0, "must report dropped messages");
                assert!(kept_turns >= 1, "must keep at least the current turn");
                assert_eq!(
                    reason,
                    crate::i18n::get_required_cli_string("history-trim-reason-recovery"),
                    "recovery must use a reason distinct from configured-budget trims: \
                     the trim target is tokens_now * 2/3, not the configured budget"
                );
                assert_eq!(
                    token_budget,
                    Some(32_000),
                    "recovery must report the configured budget when one is set"
                );
                assert!(
                    tokens_before.is_some_and(|before| before > tokens_after.unwrap_or(0)),
                    "pre-trim count must exceed post-trim count"
                );
                assert_eq!(
                    (tokens_before_source, tokens_after_source),
                    (
                        Some(zeroclaw_api::agent::TokenCountSource::Estimated),
                        Some(zeroclaw_api::agent::TokenCountSource::Estimated),
                    ),
                    "estimate-based recovery counts are marked estimated"
                );
                assert_eq!(
                    tokens_after,
                    Some(crate::agent::history::estimate_history_tokens(&history) as u64),
                    "tokens_after must describe the final recovered history (breadcrumb included)"
                );
            }
            other => panic!("expected HistoryTrimmed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recovery_below_configured_budget_does_not_claim_it_governed_the_trim() {
        // A provider can overflow at a token count below the configured
        // limit (e.g. the provider's own window is smaller than our
        // configured budget). The recovery trim target here is
        // tokens_now * 2/3, unrelated to the configured budget, so the
        // emitted reason must not read as a configured-budget trim even
        // though the configured budget is still reported for the contract.
        let mut history = overflowing_history();
        let tokens_now = estimate_history_tokens(&history);
        let configured_budget = tokens_now * 4; // configured limit far above the overflow point
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered = try_recover_context_overflow(
            &mut history,
            &err,
            1,
            Some(&tx),
            None,
            &observer,
            configured_budget,
        )
        .await;

        assert!(recovered, "an overflowing history must trim and recover");
        let event = rx.try_recv().expect("recovery must emit a TurnEvent");
        match event {
            zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
                reason,
                token_budget,
                ..
            } => {
                assert_eq!(
                    reason,
                    crate::i18n::get_required_cli_string("history-trim-reason-recovery"),
                    "a below-configured-limit overflow must not be reported as a \
                     configured-budget trim"
                );
                assert_eq!(
                    token_budget,
                    Some(configured_budget as u64),
                    "the configured limit is still reported for the #9619 contract"
                );
            }
            other => panic!("expected HistoryTrimmed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recovery_with_enforcement_disabled_reports_no_budget() {
        // `context_token_budget == 0` means configured enforcement is
        // disabled. A provider overflow can still trigger recovery in this
        // state; the emitted event must not claim a nonsensical "0-token
        // budget" governed the trim.
        let mut history = overflowing_history();
        let err = anyhow::Error::msg("maximum context length exceeded");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let observer = NoopObserver;

        let recovered =
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), None, &observer, 0)
                .await;

        assert!(recovered, "an overflowing history must trim and recover");
        let event = rx.try_recv().expect("recovery must emit a TurnEvent");
        match event {
            zeroclaw_api::agent::TurnEvent::HistoryTrimmed { token_budget, .. } => {
                assert_eq!(
                    token_budget, None,
                    "disabled enforcement must not report a 0-token budget as governing"
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
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), None, &observer, 100)
                .await;

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
            try_recover_context_overflow(&mut history, &err, 1, Some(&tx), None, &observer, 32_000)
                .await;

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
            try_recover_context_overflow(&mut history, &err, 1, None, None, &observer, budget)
                .await;
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
