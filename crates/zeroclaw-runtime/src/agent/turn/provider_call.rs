//! The provider call step: request announcement, budget enforcement, and the
//! streaming/non-streaming chat dispatch.

use super::context::TurnCtx;
use super::events::{ProgressEvent, StreamDelta, send_progress};
use super::outcome::{
    StreamInterruptedAfterOutput, StreamPreExecutedToolsWithoutFinalResponse,
    StreamSemanticEmptyCompletion, ToolLoopCancelled, is_tool_loop_cancelled,
};
use super::redact::scrub_credentials;
use super::stream_consume::consume_provider_streaming_response;
use crate::agent::cost::{check_tool_loop_budget, record_rejected_tool_loop_cost_usage};
use crate::cost::types::BudgetCheck;
use crate::observability::ObserverEvent;
use crate::tools::ToolSpec;
use anyhow::Result;
use std::time::{Duration, Instant};
use zeroclaw_providers::{
    AccountedChatResponse, ChatMessage, ChatRequest, ChatResponse, ModelProvider, ProviderDispatch,
};

pub(crate) struct ProviderCallOutcome {
    pub(crate) chat_result: Result<ChatResponse>,
    pub(crate) rejected_attempt_usage: Option<zeroclaw_providers::traits::TokenUsage>,
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
    if let Some(tx) = ctx.on_delta {
        let phase = if iteration == 0 {
            "\u{1f914} Thinking...\n".to_string()
        } else {
            format!("\u{1f914} Thinking (round {})...\n", iteration + 1)
        };
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

    let chat_result = if should_consume_provider_stream {
        // Attribution is opened by ProviderDispatch::from_ref(...).stream_chat
        // inside `consume_provider_streaming_response`; the caller does not
        // wrap a second attribution_span! here.
        let stream_future = consume_provider_streaming_response(
            active_model_provider,
            prepared_messages,
            request_tools,
            active_model,
            ctx.temperature,
            ctx.cancellation_token,
            ctx.on_delta,
            ctx.event_tx,
            ctx.strict_tool_parsing,
        );
        match stream_future.await {
            Ok(streamed) => {
                streamed_live_deltas = streamed.forwarded_live_deltas;
                streamed_protocol_suppressed = streamed.suppressed_protocol;
                streamed_visible_text = streamed.forwarded_visible_text;
                let reasoning_content = if streamed.reasoning_content.is_empty() {
                    None
                } else {
                    Some(streamed.reasoning_content)
                };
                Ok(AccountedChatResponse {
                    response: zeroclaw_providers::ChatResponse {
                        text: Some(streamed.response_text),
                        tool_calls: streamed.tool_calls,
                        usage: streamed.usage,
                        reasoning_content,
                    },
                    rejected_attempt_usage: None,
                })
            }
            Err(stream_err)
                if stream_err
                    .downcast_ref::<StreamPreExecutedToolsWithoutFinalResponse>()
                    .is_some() =>
            {
                if let Some(usage) = stream_err
                    .downcast_ref::<StreamPreExecutedToolsWithoutFinalResponse>()
                    .and_then(|error| error.usage.as_ref())
                {
                    record_rejected_tool_loop_cost_usage(ctx.provider_name, ctx.model, usage);
                }
                Err(stream_err)
            }
            Err(stream_err)
                if is_tool_loop_cancelled(&stream_err)
                    || stream_err
                        .downcast_ref::<StreamInterruptedAfterOutput>()
                        .is_some() =>
            {
                Err(stream_err)
            }
            Err(stream_err) => {
                let discarded_usage = stream_err
                    .downcast_ref::<StreamSemanticEmptyCompletion>()
                    .and_then(|error| error.usage.as_ref());
                if let Some(usage) = discarded_usage {
                    record_rejected_tool_loop_cost_usage(ctx.provider_name, ctx.model, usage);
                }
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
                {
                    let dispatcher = ProviderDispatch::from_ref(active_model_provider);
                    let chat_future = dispatcher.chat_accounted(
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
                    );
                    if let Some(token) = ctx.cancellation_token {
                        tokio::select! {
                            () = token.cancelled() => Err(ToolLoopCancelled.into()),
                            result = chat_future => result,
                        }
                    } else {
                        chat_future.await
                    }
                }
            }
        }
    } else {
        // Non-streaming path: wrap with optional per-step timeout from
        // pacing config to catch hung model responses.
        let dispatcher = ProviderDispatch::from_ref(active_model_provider);
        let chat_future = dispatcher.chat_accounted(
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
        );

        match ctx.pacing.step_timeout_secs {
            Some(step_secs) if step_secs > 0 => {
                let step_timeout = Duration::from_secs(step_secs);
                if let Some(token) = ctx.cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                        result = tokio::time::timeout(step_timeout, chat_future) => {
                            match result {
                                Ok(inner) => inner,
                                Err(_) => anyhow::bail!(
                                    "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                                ),
                            }
                        },
                    }
                } else {
                    match tokio::time::timeout(step_timeout, chat_future).await {
                        Ok(inner) => inner,
                        Err(_) => anyhow::bail!(
                            "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                        ),
                    }
                }
            }
            _ => {
                if let Some(token) = ctx.cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                        result = chat_future => result,
                    }
                } else {
                    chat_future.await
                }
            }
        }
    };

    let (chat_result, rejected_attempt_usage) = match chat_result {
        Ok(accounted) => (Ok(accounted.response), accounted.rejected_attempt_usage),
        Err(error) => (Err(error), None),
    };

    Ok(ProviderCallOutcome {
        chat_result,
        rejected_attempt_usage,
        streamed_live_deltas,
        streamed_protocol_suppressed,
        streamed_visible_text,
    })
}

#[cfg(test)]
mod payload_capture_tests {
    use super::super::context::TurnCtx;
    use super::announce_llm_request;
    use crate::observability::NoopObserver;
    use async_trait::async_trait;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_config::schema::PacingConfig;
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
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing,
            strict_tool_parsing: false,
            channel: None,
            agent_alias: None,
            turn_id: "trace-req-test",
        }
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
    use futures_util::stream::BoxStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::StreamEvent;
    use zeroclaw_config::schema::PacingConfig;
    use zeroclaw_providers::ModelProvider;
    use zeroclaw_providers::traits::{StreamOptions, StreamResult, TokenUsage};

    struct EmptyStreamThenTextProvider {
        non_stream_calls: AtomicUsize,
    }

    struct PreExecutedToolThenEmptyProvider {
        non_stream_calls: AtomicUsize,
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
        assert_eq!(recorded.input_tokens, 10);
        assert_eq!(recorded.output_tokens, 5);
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
}
