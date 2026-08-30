//! The provider call step: request announcement, budget enforcement, and the
//! streaming/non-streaming chat dispatch.

use super::context::TurnCtx;
use super::events::{ProgressEvent, StreamDelta, send_progress, thinking_status_text};
use super::outcome::{
    StreamCancelledAfterOutput, StreamCancelledWithUsage, StreamErrorWithUsage,
    StreamInterruptedAfterOutput, StreamPreExecutedToolsWithoutFinalResponse,
    StreamSemanticEmptyCompletion, ToolLoopCancelled, is_tool_loop_cancelled,
};
use super::redact::scrub_credentials;
use super::stream_consume::consume_provider_streaming_response;
use crate::agent::cost::check_tool_loop_budget;
use crate::cost::types::BudgetCheck;
use crate::observability::ObserverEvent;
use crate::tools::ToolSpec;
use anyhow::Result;
use std::time::{Duration, Instant};
use zeroclaw_config::schema::StreamReasoningMode;
use zeroclaw_providers::dispatch::{AcceptedRoute, AccountedAttempt, with_exact_dispatch_route};
use zeroclaw_providers::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, ProviderDispatch};

pub(crate) struct ProviderCallOutcome {
    pub(crate) chat_result: Result<ChatResponse>,
    /// Every physical leaf, in first-poll order. Runtime decides semantic
    /// acceptance once, then settles this immutable report once.
    pub(crate) attempts: Vec<AccountedAttempt>,
    pub(crate) accepted_route: Option<AcceptedRoute>,
    pub(crate) streamed_live_deltas: bool,
    pub(crate) streamed_protocol_suppressed: bool,
    pub(crate) streamed_visible_text: String,
}

pub(crate) async fn announce_llm_request(
    ctx: &TurnCtx<'_>,
    request_messages: &[ChatMessage],
    active_model_provider: &dyn ModelProvider,
    active_model_provider_name: &str,
    active_model: &str,
    iteration: usize,
) -> Instant {
    // ── Progress: LLM thinking ────────────────────────────
    send_progress(ctx.on_delta, ProgressEvent::WaitingOnModel).await;
    if ctx.draft_reasoning == StreamReasoningMode::Status
        && let Some(tx) = ctx.on_delta
    {
        let phase = thinking_status_text(iteration);
        let _ = tx.send(StreamDelta::Status(phase)).await;
    }

    ctx.observer.record_event(&ObserverEvent::LlmRequest {
        model_provider: active_model_provider_name.to_string(),
        model: active_model.to_string(),
        messages_count: request_messages.len(),
        channel: Some(ctx.channel_name.to_string()),
        agent_alias: ctx.agent_alias.map(|s| s.to_string()),
        parent_agent_alias: ctx.parent_agent_alias.map(|s| s.to_string()),
        turn_id: Some(ctx.turn_id.to_string()),
    });
    {
        let _provider_guard = ::zeroclaw_log::attribution_span!(active_model_provider).entered();
        let mut attrs = ::serde_json::json!({
            "iteration": iteration + 1,
            "messages_count": request_messages.len(),
            "model": active_model,
            "trace_id": ctx.turn_id,
        });
        // Opt-in request payload capture (observability.log_llm_request_payload,
        // default off). When enabled, attach the scrubbed + truncated message
        // history; when off (or no writer installed) `attrs` is unchanged.
        if let Some((policy, truncate_bytes)) = ::zeroclaw_log::llm_request_payload_policy()
            && policy.captures_payload()
            && let ::serde_json::Value::Object(map) = &mut attrs
        {
            let rendered: Vec<::serde_json::Value> = request_messages
                .iter()
                .map(|m| {
                    ::serde_json::json!({"role": m.role.as_str(), "content": m.content.as_str()})
                })
                .collect();
            let serialized = ::serde_json::to_string(&rendered).unwrap_or_default();
            let scrubbed = scrub_credentials(&serialized);
            if let Some(capture) =
                ::zeroclaw_log::capture_llm_request(policy, truncate_bytes, &scrubbed)
            {
                map.insert(
                    "request_messages".to_string(),
                    ::serde_json::Value::String(capture.text),
                );
                if capture.truncated {
                    map.insert("request_messages_truncated".to_string(), true.into());
                    map.insert(
                        "request_messages_original_bytes".to_string(),
                        capture.original_bytes.into(),
                    );
                }
            }
        }
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_attrs(attrs),
            "llm_request"
        );
    }

    let llm_started_at = Instant::now();

    // Fire void hook before LLM call
    if let Some(hooks) = ctx.hooks {
        hooks.fire_llm_input(request_messages, active_model).await;
    }

    llm_started_at
}

/// Budget enforcement — block if limit exceeded (no-op when not scoped).
pub(crate) fn enforce_tool_loop_budget() -> Result<()> {
    if let Some(BudgetCheck::Exceeded {
        current_usd,
        limit_usd,
        period,
    }) = check_tool_loop_budget()
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "current_usd": current_usd,
                    "limit_usd": limit_usd,
                    "period": format!("{period:?}"),
                })),
            "tool-call loop budget exceeded"
        );
        anyhow::bail!(
            "Budget exceeded: ${:.4} of ${:.2} {:?} limit. Cannot make further API calls until the budget resets.",
            current_usd,
            limit_usd,
            period
        );
    }
    Ok(())
}

/// One provider call: streaming via `consume_provider_streaming_response`
/// with non-streaming fallback, or plain non-streaming chat with optional
/// per-step timeout and cancel select. See [`ProviderCallOutcome`] for the
/// cancel asymmetry this function must preserve.
pub(crate) async fn call_provider(
    ctx: &TurnCtx<'_>,
    active_model_provider: &dyn ModelProvider,
    active_model: &str,
    prepared_messages: &[ChatMessage],
    request_tools: Option<&[ToolSpec]>,
    should_consume_provider_stream: bool,
    iteration: usize,
) -> Result<ProviderCallOutcome> {
    let mut streamed_live_deltas = false;
    let mut streamed_protocol_suppressed = false;
    let mut streamed_visible_text = String::new();

    let (chat_result, accounting) = if should_consume_provider_stream {
        // The stream is lazily consumed inside this call-scoped owner. Its
        // permitted non-stream recovery therefore shares the same Reliable
        // attempt ledger instead of opening a second scope.
        let scope = zeroclaw_providers::dispatch::AccountedChatScope::new();
        let (result, live_deltas, protocol_suppressed, visible_text) = scope
            .scope(Box::pin(zeroclaw_providers::reliable::scope_provider_fallback(Box::pin(async {
                    match consume_provider_streaming_response(
                        active_model_provider,
                        prepared_messages,
                        request_tools,
                        active_model,
                        ctx.temperature,
                        ctx.cancellation_token,
                        ctx.on_delta,
                        ctx.event_tx,
                        ctx.strict_tool_parsing,
                        ctx.draft_reasoning,
                    )
                    .await
                    {
                        Ok(streamed) => {
                            let reasoning_content = (!streamed.reasoning_content.is_empty())
                                .then_some(streamed.reasoning_content);
                            (
                                Ok(ChatResponse {
                                    text: Some(streamed.response_text),
                                    tool_calls: streamed.tool_calls,
                                    usage: streamed.usage,
                                    reasoning_content,
                                }),
                                streamed.forwarded_live_deltas,
                                streamed.suppressed_protocol,
                                streamed.forwarded_visible_text,
                            )
                        }
                        Err(stream_err)
                            if stream_err
                                .downcast_ref::<StreamPreExecutedToolsWithoutFinalResponse>()
                                .is_some()
                                || is_tool_loop_cancelled(&stream_err)
                                || stream_err
                                    .downcast_ref::<StreamInterruptedAfterOutput>()
                                    .is_some() =>
                        {
                            if let Some(usage) = stream_err
                                .downcast_ref::<StreamPreExecutedToolsWithoutFinalResponse>()
                                .and_then(|error| error.usage.clone())
                                .or_else(|| {
                                    stream_err
                                        .downcast_ref::<StreamInterruptedAfterOutput>()
                                        .and_then(|error| error.usage.clone())
                                })
                                .or_else(|| {
                                    stream_err
                                        .downcast_ref::<StreamCancelledAfterOutput>()
                                        .and_then(|error| error.usage.clone())
                                })
                                .or_else(|| {
                                    stream_err
                                        .downcast_ref::<StreamCancelledWithUsage>()
                                        .and_then(|error| error.usage.clone())
                                })
                            {
                                scope.record_stream_interruption_usage(usage);
                            }
                            (Err(stream_err), false, false, String::new())
                        }
                        Err(stream_err) => {
                            if let Some(usage) = stream_err
                                .downcast_ref::<StreamSemanticEmptyCompletion>()
                                .and_then(|error| error.usage.clone())
                            {
                                scope.record_stream_semantic_rejection_usage(usage);
                            } else if let Some(usage) = stream_err
                                .downcast_ref::<StreamErrorWithUsage>()
                                .and_then(|error| error.usage.clone())
                            {
                                scope.record_stream_interruption_usage(usage);
                            }
                            if stream_err
                                .downcast_ref::<StreamSemanticEmptyCompletion>()
                                .is_some()
                            {
                                scope.mark_stream_recovery_semantic_empty();
                            }
                            scope.record_stream_recovery_failure(&stream_err);
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                                    .with_category(::zeroclaw_log::EventCategory::Provider)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({
                                        "model": active_model,
                                        "iteration": iteration + 1,
                                        "error": scrub_credentials(&stream_err.to_string()),
                                        "trace_id": ctx.turn_id,
                                    })),
                                "llm_stream_fallback: provider stream failed, falling back to non-streaming chat"
                            );
                            scope.clear_provisional_provider_route();
                            let dispatcher = ProviderDispatch::from_ref(active_model_provider);
                            let recovery = with_exact_dispatch_route(
                                ctx.provider_name.to_string(),
                                active_model.to_string(),
                                dispatcher.chat(
                                ChatRequest {
                                    messages: prepared_messages,
                                    tools: request_tools,
                                    thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                                        .try_with(Clone::clone)
                                        .ok()
                                        .flatten(),
                                },
                                active_model,
                                ctx.temperature,
                                ),
                            );
                            let result = if let Some(token) = ctx.cancellation_token {
                                tokio::select! {
                                    biased;
                                    result = recovery => result,
                                    () = token.cancelled() => Err(ToolLoopCancelled.into()),
                                }
                            } else {
                                recovery.await
                            };
                            (result, false, false, String::new())
                        }
                    }
                }))))
            .await;
        if result.is_ok() {
            scope.mark_logical_success();
        }
        let accounting = scope.take();
        streamed_live_deltas = live_deltas;
        streamed_protocol_suppressed = protocol_suppressed;
        streamed_visible_text = visible_text;
        (result, accounting)
    } else {
        // Non-streaming path: wrap with optional per-step timeout from
        // pacing config to catch hung model responses.
        let dispatcher = ProviderDispatch::from_ref(active_model_provider);
        let scope = zeroclaw_providers::dispatch::AccountedChatScope::new();
        let chat_future = scope.scope(Box::pin(with_exact_dispatch_route(
            ctx.provider_name.to_string(),
            active_model.to_string(),
            dispatcher.chat(
                ChatRequest {
                    messages: prepared_messages,
                    tools: request_tools,
                    thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                        .try_with(Clone::clone)
                        .ok()
                        .flatten(),
                },
                active_model,
                ctx.temperature,
            ),
        )));

        let result = match ctx.pacing.step_timeout_secs {
            Some(step_secs) if step_secs > 0 => {
                let step_timeout = Duration::from_secs(step_secs);
                if let Some(token) = ctx.cancellation_token {
                    tokio::select! {
                        biased;
                        result = tokio::time::timeout(step_timeout, chat_future) => {
                            match result {
                                Ok(inner) => inner,
                                Err(_) => Err(anyhow::Error::msg(format!("LLM inference step timed out after {step_secs}s (step_timeout_secs)"))),
                            }
                        },
                        () = token.cancelled() => Err(ToolLoopCancelled.into()),
                    }
                } else {
                    match tokio::time::timeout(step_timeout, chat_future).await {
                        Ok(inner) => inner,
                        Err(_) => Err(anyhow::Error::msg(format!(
                            "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                        ))),
                    }
                }
            }
            _ => {
                if let Some(token) = ctx.cancellation_token {
                    tokio::select! {
                        biased;
                        result = chat_future => result,
                        () = token.cancelled() => Err(ToolLoopCancelled.into()),
                    }
                } else {
                    chat_future.await
                }
            }
        };
        if result.is_ok() {
            scope.mark_logical_success();
        }
        (result, scope.take())
    };
    let (attempts, _, accepted_route) = accounting.into_attempts_and_parts();

    Ok(ProviderCallOutcome {
        chat_result,
        attempts,
        accepted_route,
        streamed_live_deltas,
        streamed_protocol_suppressed,
        streamed_visible_text,
    })
}

#[cfg(test)]
mod payload_capture_tests {
    use super::super::context::TurnCtx;
    use super::super::events::{ProgressEvent, StreamDelta, thinking_status_text};
    use super::announce_llm_request;
    use crate::observability::NoopObserver;
    use async_trait::async_trait;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_config::schema::{PacingConfig, StreamReasoningMode};
    use zeroclaw_log::LogConfig;
    use zeroclaw_providers::{ChatMessage, ModelProvider};

    /// Minimal provider stub. Only `chat_with_system` is required by
    /// `ModelProvider`; `announce_llm_request` never calls it (it only opens
    /// `attribution_span!` over the provider), so a trivial reply is fine.
    struct StubProvider;

    #[async_trait]
    impl ModelProvider for StubProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    impl Attributable for StubProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "stub-provider"
        }
    }

    fn test_ctx<'a>(observer: &'a NoopObserver, pacing: &'a PacingConfig) -> TurnCtx<'a> {
        test_ctx_with_delta(observer, pacing, None, StreamReasoningMode::Status)
    }

    fn test_ctx_with_delta<'a>(
        observer: &'a NoopObserver,
        pacing: &'a PacingConfig,
        on_delta: Option<&'a tokio::sync::mpsc::Sender<StreamDelta>>,
        draft_reasoning: StreamReasoningMode,
    ) -> TurnCtx<'a> {
        TurnCtx {
            parent_agent_alias: None,
            observer,
            provider_name: "stub",
            model: "stub-model",
            temperature: None,
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning,
            agent_alias: None,
            turn_id: "trace-req-test",
        }
    }

    #[tokio::test]
    async fn announce_llm_request_only_emits_thinking_status_in_status_mode() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let provider = StubProvider;
        let history = vec![ChatMessage::user("hello")];

        for mode in [StreamReasoningMode::Off, StreamReasoningMode::Full] {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamDelta>(4);
            let ctx = test_ctx_with_delta(&observer, &pacing, Some(&tx), mode);
            let _ = announce_llm_request(&ctx, &history, &provider, "stub", "stub-model", 0).await;
            drop(tx);
            assert!(matches!(
                rx.recv().await,
                Some(StreamDelta::Lifecycle(ProgressEvent::WaitingOnModel))
            ));
            assert!(
                rx.recv().await.is_none(),
                "{mode:?} must not emit static thinking"
            );
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamDelta>(4);
        let ctx = test_ctx_with_delta(&observer, &pacing, Some(&tx), StreamReasoningMode::Status);
        let _ = announce_llm_request(&ctx, &history, &provider, "stub", "stub-model", 3).await;
        drop(tx);
        assert!(matches!(
            rx.recv().await,
            Some(StreamDelta::Lifecycle(ProgressEvent::WaitingOnModel))
        ));
        assert!(matches!(
            rx.recv().await,
            Some(StreamDelta::Status(text)) if text == thinking_status_text(3)
        ));
    }

    async fn next_llm_request(
        rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    let ours = value
                        .get("attributes")
                        .and_then(|a| a.get("trace_id"))
                        .and_then(|v| v.as_str())
                        == Some("trace-req-test");
                    if ours && value.get("message").and_then(|v| v.as_str()) == Some("llm_request")
                    {
                        return value;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        panic!("did not observe an llm_request broadcast record within the deadline");
    }

    fn install_writer(payload_mode: &str) {
        let cfg = LogConfig {
            log_llm_request_payload: payload_mode.into(),
            log_tool_io_truncate_bytes: 40,
            log_persistence: "none".into(),
            ..LogConfig::default()
        };
        zeroclaw_log::init_from_config(&cfg, std::path::Path::new("/"));
    }

    // The raw credential embedded in one message. The rendering-layer scrubber
    // (`redact::scrub_credentials`) matches the `api_key: <value>` pattern and
    // redacts the value, preserving only its first 4 chars. The unique secret
    // tail below must NOT survive into the captured payload.
    const SECRET_TAIL: &str = "ABCDEF1234567890SECRET";

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn llm_request_payload_redacts_truncates_and_off_omits() {
        // Serialize against writer::tests and the broadcast-hook tests for the
        // whole test: we drive `record!` -> LogCaptureLayer -> broadcast hook,
        // and a parallel `clear_broadcast_hook` would otherwise drop our event.
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();

        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();

        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let provider = StubProvider;
        let history = vec![
            ChatMessage::system("You are a helpful assistant."),
            ChatMessage::user(format!("deploy with api_key: sk-{SECRET_TAIL} please")),
        ];

        // ---- ON: redacted + truncate cap 40 ----
        install_writer("redacted");
        while rx.try_recv().is_ok() {}

        let ctx = test_ctx(&observer, &pacing);
        let _ = announce_llm_request(&ctx, &history, &provider, "stub", "stub-model", 0).await;
        let on_record = next_llm_request(&mut rx).await;

        let attrs = on_record
            .get("attributes")
            .expect("llm_request record carries attributes");
        let request_messages = attrs
            .get("request_messages")
            .and_then(|v| v.as_str())
            .expect("request_messages present and a String when capture is on");
        assert!(
            !request_messages.contains(SECRET_TAIL),
            "captured payload must not contain the raw secret; got: {request_messages}"
        );
        assert_eq!(
            attrs
                .get("request_messages_truncated")
                .and_then(|v| v.as_bool()),
            Some(true),
            "payload exceeds the 40-byte cap so it must be flagged truncated"
        );
        let original_bytes = attrs
            .get("request_messages_original_bytes")
            .and_then(|v| v.as_u64())
            .expect("request_messages_original_bytes is a number");
        assert!(
            original_bytes > 40,
            "original payload byte length must exceed the cap; got {original_bytes}"
        );
        assert!(
            attrs.get("messages_count").is_some(),
            "messages_count is always present"
        );

        // ---- OFF: payload omitted entirely ----
        install_writer("off");
        while rx.try_recv().is_ok() {}

        let ctx = test_ctx(&observer, &pacing);
        let _ = announce_llm_request(&ctx, &history, &provider, "stub", "stub-model", 0).await;
        let off_record = next_llm_request(&mut rx).await;

        let off_attrs = off_record
            .get("attributes")
            .expect("llm_request record carries attributes");
        assert!(
            off_attrs.get("request_messages").is_none(),
            "request_messages must be absent when the policy is off"
        );
        assert!(
            off_attrs.get("request_messages_truncated").is_none(),
            "no truncation metadata when capture is off"
        );
        assert!(
            off_attrs.get("messages_count").is_some(),
            "messages_count is present regardless of payload policy"
        );

        zeroclaw_log::clear_broadcast_hook();
    }
}

#[cfg(test)]
mod streaming_fallback_tests {
    use super::super::context::TurnCtx;
    use super::*;
    use crate::agent::cost::{
        TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext,
        TurnUsage,
    };
    use crate::observability::NoopObserver;
    use async_trait::async_trait;
    use axum::{Router, http::StatusCode, routing::post};
    use futures_util::stream::BoxStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::StreamEvent;
    use zeroclaw_config::schema::PacingConfig;
    use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleModelProvider};
    use zeroclaw_providers::reliable::ReliableModelProvider;
    use zeroclaw_providers::traits::{StreamOptions, StreamResult, TokenUsage};
    use zeroclaw_providers::{
        ModelProvider, ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
    };

    struct EmptyStreamThenTextProvider {
        non_stream_calls: AtomicUsize,
    }

    struct PreExecutedToolThenEmptyProvider {
        non_stream_calls: AtomicUsize,
    }

    struct EmptyThenPendingProvider {
        calls: AtomicUsize,
    }

    struct StreamFailureNoReplayProvider {
        non_stream_calls: Arc<AtomicUsize>,
    }

    struct VisibleThenServerStreamFailureProvider {
        non_stream_calls: Arc<AtomicUsize>,
    }

    impl Attributable for EmptyStreamThenTextProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "EmptyStreamThenTextProvider"
        }
    }

    impl Attributable for PreExecutedToolThenEmptyProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "PreExecutedToolThenEmptyProvider"
        }
    }

    impl Attributable for EmptyThenPendingProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "EmptyThenPendingProvider"
        }
    }

    impl Attributable for StreamFailureNoReplayProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "StreamFailureNoReplayProvider"
        }
    }

    impl Attributable for VisibleThenServerStreamFailureProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "VisibleThenServerStreamFailureProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for EmptyStreamThenTextProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            self.non_stream_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ChatResponse {
                text: Some("fallback response".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::Usage(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cached_input_tokens: None,
                })),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for PreExecutedToolThenEmptyProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            self.non_stream_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ChatResponse {
                text: Some("must not be requested".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::PreExecutedToolCall {
                    name: "provider_tool".to_string(),
                    args: "{}".to_string(),
                }),
                Ok(StreamEvent::PreExecutedToolResult {
                    name: "provider_tool".to_string(),
                    output: "completed".to_string(),
                }),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for EmptyThenPendingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Ok(ChatResponse {
                    text: None,
                    tool_calls: Vec::new(),
                    usage: Some(TokenUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(5),
                        cached_input_tokens: None,
                    }),
                    reasoning_content: None,
                });
            }
            std::future::pending::<Result<ChatResponse>>().await
        }
    }

    #[async_trait]
    impl ModelProvider for StreamFailureNoReplayProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            self.non_stream_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ChatResponse {
                text: Some("must not replay".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            Box::pin(futures_util::stream::iter(vec![Err(
                zeroclaw_providers::traits::StreamError::ModelProvider(
                    "error sending request for url (http://127.0.0.1:9/v1/messages): \
                     client error (Connect): connection refused"
                        .to_string(),
                ),
            )]))
        }
    }

    #[async_trait]
    impl ModelProvider for VisibleThenServerStreamFailureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            self.non_stream_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ChatResponse {
                text: Some("must not replay".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(
                    zeroclaw_api::model_provider::StreamChunk::delta("visible"),
                )),
                Err(zeroclaw_providers::traits::StreamError::ModelProvider(
                    "503 Service Unavailable".to_string(),
                )),
            ]))
        }
    }

    #[tokio::test]
    async fn completed_empty_stream_uses_one_non_streaming_fallback() {
        let provider = EmptyStreamThenTextProvider {
            non_stream_calls: AtomicUsize::new(0),
        };
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let cost_context = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = std::sync::Arc::new(parking_lot::Mutex::new(TurnUsage::default()));
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: Some(0.0),
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        };

        let outcome = TOOL_LOOP_TURN_USAGE
            .scope(
                Some(std::sync::Arc::clone(&turn_usage)),
                TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    Some(cost_context),
                    call_provider(
                        &ctx,
                        &provider,
                        "test-model",
                        &[ChatMessage::user("go")],
                        None,
                        true,
                        0,
                    ),
                ),
            )
            .await
            .expect("stream failure is recovered by one non-streaming request");
        let response = outcome.chat_result.expect("fallback response succeeds");

        assert_eq!(response.text.as_deref(), Some("fallback response"));
        assert_eq!(provider.non_stream_calls.load(Ordering::Relaxed), 1);
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 0);
        assert_eq!(recorded.output_tokens, 0);
        assert_eq!(outcome.attempts.len(), 2);
        assert!(matches!(
            outcome.attempts[0].outcome(),
            zeroclaw_providers::dispatch::AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(usage),
            } if usage.input_tokens == Some(10) && usage.output_tokens == Some(5)
        ));
    }

    #[tokio::test]
    async fn stream_failure_without_fallback_keeps_typed_terminal_cause() {
        let non_stream_calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".to_string(),
                Box::new(StreamFailureNoReplayProvider {
                    non_stream_calls: Arc::clone(&non_stream_calls),
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: Some(0.0),
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        };

        let error = call_provider(
            &ctx,
            &provider,
            "test-model",
            &[ChatMessage::user("go")],
            None,
            true,
            0,
        )
        .await
        .expect("stream fallback remains a provider-call outcome")
        .chat_result
        .expect_err("the only stream candidate must fail");

        assert_eq!(non_stream_calls.load(Ordering::Relaxed), 0);
        assert!(
            error
                .to_string()
                .contains("All model providers/models failed after 0 failure event(s)")
        );
        let terminal = error
            .chain()
            .find_map(|source| source.downcast_ref::<ReliableProviderTerminalFailure>())
            .expect("recovery error must preserve a typed terminal cause");
        assert_eq!(
            terminal.kind(),
            ReliableProviderTerminalFailureKind::Connection
        );
        assert_eq!(terminal.endpoint(), Some("http://127.0.0.1:9/v1/messages"));
    }

    #[tokio::test]
    async fn visible_stream_failure_preserves_partial_text_and_typed_server_cause_without_replay() {
        let non_stream_calls = Arc::new(AtomicUsize::new(0));
        let provider = VisibleThenServerStreamFailureProvider {
            non_stream_calls: Arc::clone(&non_stream_calls),
        };
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: Some(0.0),
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: Some(&event_tx),
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        };

        let error = call_provider(
            &ctx,
            &provider,
            "test-model",
            &[ChatMessage::user("go")],
            None,
            true,
            0,
        )
        .await
        .expect("stream interruption remains a provider-call outcome")
        .chat_result
        .expect_err("visible stream failure must remain terminal");

        match event_rx
            .recv()
            .await
            .expect("visible chunk must be delivered")
        {
            zeroclaw_api::agent::TurnEvent::Chunk { delta } => assert_eq!(delta, "visible"),
            other => panic!("expected visible chunk, got {other:?}"),
        }
        let interrupted = error
            .downcast_ref::<StreamInterruptedAfterOutput>()
            .expect("visible output must preserve its typed interruption outcome");
        assert_eq!(interrupted.partial_text, "visible");
        assert_eq!(
            interrupted.to_string(),
            "model_provider stream error: ModelProvider error: 503 Service Unavailable"
        );

        let terminal = error
            .chain()
            .find_map(|source| source.downcast_ref::<ReliableProviderTerminalFailure>())
            .expect("stream interruption must expose its typed provider cause");
        assert_eq!(
            terminal.kind(),
            ReliableProviderTerminalFailureKind::ProviderServer
        );
        assert_eq!(
            crate::agent::terminal_completion_error_message(&error, None),
            Some(crate::i18n::get_required_cli_string(
                "cli-agent-error-provider-server"
            ))
        );
        assert_eq!(non_stream_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn compatible_stream_failures_recover_to_typed_terminal_kinds_without_replay() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let cases = [
            (
                StatusCode::UNAUTHORIZED,
                ReliableProviderTerminalFailureKind::Authentication,
            ),
            (
                StatusCode::NOT_FOUND,
                ReliableProviderTerminalFailureKind::ModelNotFound,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                ReliableProviderTerminalFailureKind::RateLimited,
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                ReliableProviderTerminalFailureKind::ProviderServer,
            ),
        ];

        for (status, expected_kind) in cases {
            let request_count = Arc::new(AtomicUsize::new(0));
            let request_count_for_route = Arc::clone(&request_count);
            let app = Router::new().route(
                "/chat/completions",
                post(move || {
                    let request_count = Arc::clone(&request_count_for_route);
                    async move {
                        request_count.fetch_add(1, Ordering::Relaxed);
                        (status, "upstream failure")
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind compatible test server");
            let addr = listener.local_addr().expect("read compatible test address");
            let server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve compatible test response");
            });
            let compatible = OpenAiCompatibleModelProvider::builder("test")
                .display_name("Test Compatible")
                .base_url(&format!("http://{addr}"))
                .credential(None)
                .auth_style(AuthStyle::Bearer)
                .build();
            let provider = ReliableModelProvider::new(
                "test",
                vec![(
                    "primary".to_string(),
                    Box::new(compatible) as Box<dyn ModelProvider>,
                )],
                0,
                1,
            );
            let ctx = TurnCtx {
                observer: &observer,
                provider_name: "test-provider",
                model: "test-model",
                temperature: Some(0.0),
                approval: None,
                channel_name: "test",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                event_tx: None,
                hooks: None,
                dedup_exempt_tools: &[],
                pacing: &pacing,
                strict_tool_parsing: false,
                channel: None,
                draft_reasoning: StreamReasoningMode::Status,
                turn_id: "test-turn",
                agent_alias: None,
                parent_agent_alias: None,
            };

            let error = call_provider(
                &ctx,
                &provider,
                "test-model",
                &[ChatMessage::user("go")],
                None,
                true,
                0,
            )
            .await
            .expect("stream failure is returned as a provider-call outcome")
            .chat_result
            .expect_err("the compatible stream failure must remain terminal");
            let terminal = error
                .chain()
                .find_map(|source| source.downcast_ref::<ReliableProviderTerminalFailure>())
                .expect("recovery error must preserve a typed terminal cause");

            assert_eq!(
                terminal.kind(),
                expected_kind,
                "{status} must retain its compatible streaming classification"
            );
            assert_eq!(request_count.load(Ordering::Relaxed), 1, "{status}");
            server.abort();
        }
    }

    #[tokio::test]
    async fn pre_executed_tool_empty_stream_never_replays_request() {
        let provider = PreExecutedToolThenEmptyProvider {
            non_stream_calls: AtomicUsize::new(0),
        };
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: Some(0.0),
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        };

        let error = call_provider(
            &ctx,
            &provider,
            "test-model",
            &[ChatMessage::user("go")],
            None,
            true,
            0,
        )
        .await
        .expect("dispatch returns the provider outcome")
        .chat_result
        .expect_err("provider-executed tool work without final text must fail");

        assert!(error.to_string().contains("provider-executed tools"));
        assert_eq!(
            provider.non_stream_calls.load(Ordering::Relaxed),
            0,
            "replaying after provider-executed tool work could repeat side effects"
        );
    }

    #[tokio::test]
    async fn non_stream_cancellation_keeps_prior_rejected_reliable_attempt() {
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".to_string(),
                Box::new(EmptyThenPendingProvider {
                    calls: AtomicUsize::new(0),
                }) as Box<dyn ModelProvider>,
            )],
            1,
            0,
        );
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let cancel_after_retry = cancellation.clone();
        let _cancel = zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_after_retry.cancel();
        });
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "requested-provider",
            model: "requested-model",
            temperature: Some(0.0),
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: Some(&cancellation),
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        };

        let outcome = call_provider(
            &ctx,
            &provider,
            "requested-model",
            &[ChatMessage::user("go")],
            None,
            false,
            0,
        )
        .await
        .expect("cancellation remains a provider-call outcome");

        assert!(is_tool_loop_cancelled(
            &outcome.chat_result.expect_err("provider call is cancelled")
        ));
        assert_eq!(outcome.attempts.len(), 1);
        let attempt = &outcome.attempts[0];
        assert_eq!(attempt.provider_ref(), "primary");
        assert_eq!(attempt.model(), "requested-model");
        assert!(matches!(
            attempt.outcome(),
            zeroclaw_providers::dispatch::AttemptUsageOutcome::Complete(usage)
                if usage.input_tokens == Some(10) && usage.output_tokens == Some(5)
        ));
    }

    #[tokio::test]
    async fn non_stream_timeout_keeps_prior_rejected_reliable_attempt() {
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".to_string(),
                Box::new(EmptyThenPendingProvider {
                    calls: AtomicUsize::new(0),
                }) as Box<dyn ModelProvider>,
            )],
            1,
            0,
        );
        let observer = NoopObserver;
        let pacing = PacingConfig {
            step_timeout_secs: Some(1),
            ..PacingConfig::default()
        };
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "requested-provider",
            model: "requested-model",
            temperature: Some(0.0),
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        };

        let outcome = call_provider(
            &ctx,
            &provider,
            "requested-model",
            &[ChatMessage::user("go")],
            None,
            false,
            0,
        )
        .await
        .expect("timeout remains a provider-call outcome");

        assert!(
            outcome
                .chat_result
                .expect_err("provider call times out")
                .to_string()
                .contains("step_timeout_secs")
        );
        assert_eq!(outcome.attempts.len(), 2);
        let attempt = &outcome.attempts[0];
        assert_eq!(attempt.provider_ref(), "primary");
        assert_eq!(attempt.model(), "requested-model");
        assert!(matches!(
            attempt.outcome(),
            zeroclaw_providers::dispatch::AttemptUsageOutcome::Complete(usage)
                if usage.input_tokens == Some(10) && usage.output_tokens == Some(5)
        ));
    }
}
