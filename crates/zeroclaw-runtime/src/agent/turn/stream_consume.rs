//! Streaming provider-response consumption for the turn loop.

use super::events::{DraftEvent, StreamDelta};
use super::outcome::{
    StreamCancelledAfterOutput, StreamCancelledWithUsage, StreamErrorWithUsage,
    StreamInterruptedAfterOutput, StreamPreExecutedToolsCause,
    StreamPreExecutedToolsWithoutFinalResponse, StreamSemanticEmptyCompletion,
    StreamTerminalCompletion,
};
use super::stream_guard::{StreamTerminalMarkerStripper, StreamTextGuard, StreamThinkTagStripper};
use anyhow::Result;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::model_provider::StreamEvent;
use zeroclaw_config::schema::StreamReasoningMode;
use zeroclaw_providers::{ChatMessage, ChatRequest, ModelProvider, ProviderDispatch, ToolCall};

#[derive(Debug, Default)]
pub(crate) struct StreamedChatOutcome {
    pub(crate) response_text: String,
    pub(crate) reasoning_content: String,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) forwarded_live_deltas: bool,
    /// Visible text already delivered live on the draft/event sinks. The loop
    /// re-sends only `display_text` beyond this prefix, so narration is neither
    /// duplicated nor truncated when a tool call cuts the live stream short.
    pub(crate) forwarded_visible_text: String,
    pub(crate) suppressed_protocol: bool,
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
    pub(crate) saw_pre_executed_tool_activity: bool,
}

pub(crate) async fn consume_provider_streaming_response(
    model_provider: &dyn ModelProvider,
    messages: &[ChatMessage],
    request_tools: Option<&[crate::tools::ToolSpec]>,
    model: &str,
    temperature: Option<f64>,
    cancellation_token: Option<&CancellationToken>,
    on_delta: Option<&tokio::sync::mpsc::Sender<DraftEvent>>,
    event_tx: Option<&tokio::sync::mpsc::Sender<TurnEvent>>,
    strict_tool_parsing: bool,
    draft_reasoning: StreamReasoningMode,
) -> Result<StreamedChatOutcome> {
    let mut provider_stream = ProviderDispatch::from_ref(model_provider)
        .stream_chat_terminal_aware(
            ChatRequest {
                messages,
                tools: request_tools,
                thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                    .try_with(Clone::clone)
                    .ok()
                    .flatten(),
            },
            model,
            temperature,
            zeroclaw_providers::traits::StreamOptions::new(true),
        );
    let mut outcome = StreamedChatOutcome::default();
    let mut delta_sender = on_delta;
    let mut think_stripper = StreamThinkTagStripper::default();
    let mut marker_stripper = StreamTerminalMarkerStripper::new();
    let mut text_guard = StreamTextGuard::new(request_tools);
    // Correlates PreExecutedToolCall events with their later results so both
    // TurnEvents share a stable id (FIFO per tool name).
    let mut pre_executed_ids: std::collections::HashMap<
        String,
        std::collections::VecDeque<String>,
    > = std::collections::HashMap::new();
    // Tracks event_tx-visible output only (Chunk/Thinking/pre-executed tool
    // events). Draft (`on_delta`) forwards don't count: drafts are mutable
    // surfaces, so a non-streaming retry after a stream error overwrites
    // rather than duplicates.
    let mut visible_event_output = false;
    let mut forwarded_text = String::new();
    // Whitespace before the first meaningful chunk is not immutable output.
    // Keep it until a later chunk makes it part of visible text; otherwise a
    // whitespace-only stream would suppress the permitted pre-output retry.
    let mut pending_leading_whitespace = String::new();
    let mut saw_meaningful_visible_text = false;

    macro_rules! forward_visible {
        ($text:expr, $count_visible:tt) => {{
            let visible = $text;
            // Empty visible text is not output: a strict-mode marker-only delta
            // (or a trailing flush that stripped a marker) yields an empty
            // string here, and forwarding it would record an empty Chunk as
            // visible output. That would make a later provider error take the
            // StreamInterruptedAfterOutput path and disable the non-streaming
            // fallback even though the user saw no text. Guard so empty text is
            // never counted as visible output.
            if visible.trim().is_empty() && !saw_meaningful_visible_text {
                pending_leading_whitespace.push_str(&visible);
            } else if !visible.is_empty() {
                let visible = if saw_meaningful_visible_text {
                    visible
                } else {
                    let mut prefixed = std::mem::take(&mut pending_leading_whitespace);
                    prefixed.push_str(&visible);
                    prefixed
                };
                let mut delivered_live = false;
                if let Some(tx) = event_tx {
                    if tx
                        .send(TurnEvent::Chunk {
                            delta: visible.clone(),
                        })
                        .await
                        .is_ok()
                    {
                        outcome.forwarded_live_deltas = true;
                        delivered_live = true;
                        forward_visible!(@count $count_visible, visible);
                    }
                }
                if let Some(tx) = delta_sender {
                    if tx.send(StreamDelta::Text(visible.clone())).await.is_ok() {
                        outcome.forwarded_live_deltas = true;
                        delivered_live = true;
                    } else {
                        delta_sender = None;
                    }
                }
                // This buffer is used only to avoid duplicating successfully
                // delivered live text on a later successful final response.
                // It may include mutable drafts; interruption persistence
                // below uses `forwarded_text`, which records only successful
                // immutable Chunk events.
                if delivered_live {
                    outcome.forwarded_visible_text.push_str(&visible);
                }
                if !visible.trim().is_empty() {
                    saw_meaningful_visible_text = true;
                }
            }
        }};
        (@count true, $visible:ident) => {{
            visible_event_output = true;
            forwarded_text.push_str(&$visible);
        }};
        (@count false, $visible:ident) => {{}};
    }

    loop {
        let next_chunk = if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => {
                    // Cancel after visible streamed text: persist-worthy,
                    // exactly like the pre-consolidation engine's
                    // committed-partial-on-cancel.
                    if forwarded_text.is_empty() {
                        return Err(StreamCancelledWithUsage::new(outcome.usage.clone()).into());
                    }
                    return Err(
                        StreamCancelledAfterOutput::with_usage(
                            forwarded_text,
                            outcome.usage.clone(),
                        )
                        .into(),
                    );
                }
                chunk = provider_stream.next() => chunk,
            }
        } else {
            provider_stream.next().await
        };

        let Some(event_result) = next_chunk else {
            break;
        };

        let event = match event_result {
            Ok(event) => event,
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Provider)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "model_provider stream emitted an error event"
                );
                if let Some(failure) =
                    zeroclaw_api::model_provider::semantic_empty_terminal_failure(&err)
                {
                    let usage = outcome.usage.clone().or_else(|| failure.usage.clone());
                    if visible_event_output {
                        let failure = if failure.has_pre_executed_tool_activity() {
                            zeroclaw_api::model_provider::SemanticEmptyTerminalFailure::with_pre_executed_tool_activity(usage)
                        } else {
                            zeroclaw_api::model_provider::SemanticEmptyTerminalFailure::with_no_replay(usage)
                        };
                        return Err(StreamInterruptedAfterOutput::semantic_empty(
                            forwarded_text,
                            failure,
                        )
                        .into());
                    }
                    if failure.has_pre_executed_tool_activity() {
                        return Err(StreamPreExecutedToolsWithoutFinalResponse {
                            usage,
                            cause: None,
                        }
                        .into());
                    }
                    return Err(StreamSemanticEmptyCompletion {
                        usage,
                        replayable: failure.is_replayable() && outcome.tool_calls.is_empty(),
                    }
                    .into());
                }
                if let Some(failure) =
                    zeroclaw_api::model_provider::terminal_completion_failure(&err).cloned()
                {
                    if visible_event_output {
                        let failure = zeroclaw_api::model_provider::TerminalCompletionFailure::new(
                            failure.reason,
                            outcome.usage.clone().or(failure.usage),
                        );
                        return Err(StreamInterruptedAfterOutput::terminal(
                            forwarded_text,
                            failure,
                        )
                        .into());
                    }
                    if outcome.saw_pre_executed_tool_activity {
                        return Err(StreamPreExecutedToolsWithoutFinalResponse {
                            usage: outcome.usage.clone().or(failure.usage.clone()),
                            cause: Some(StreamPreExecutedToolsCause::Terminal(failure)),
                        }
                        .into());
                    }
                    let mut policy = zeroclaw_providers::terminal_completion_context(&err)
                        .map(zeroclaw_providers::TerminalCompletionContext::policy)
                        .unwrap_or_else(|| {
                            zeroclaw_providers::default_terminal_policy(failure.reason)
                        });
                    if !outcome.tool_calls.is_empty() {
                        policy = zeroclaw_providers::TerminalCompletionPolicy::new(
                            zeroclaw_providers::TerminalRecoveryDisposition::NoReplay,
                            policy.usage_chargeability(),
                        );
                    }
                    return Err(StreamTerminalCompletion { failure, policy }.into());
                }

                let message = format!("model_provider stream error: {err}");
                let provider_error = anyhow::Error::msg(message.clone());
                if visible_event_output {
                    // Preserve only the immutable prefix that the caller
                    // actually received, even when a client tool call also
                    // makes replay ineligible.
                    if outcome.saw_pre_executed_tool_activity {
                        return Err(StreamInterruptedAfterOutput::semantic_empty(
                            forwarded_text,
                            zeroclaw_api::model_provider::SemanticEmptyTerminalFailure::with_pre_executed_tool_activity(outcome.usage),
                        )
                        .into());
                    }
                    return Err(StreamInterruptedAfterOutput::reliable_provider(
                        forwarded_text,
                        message,
                        outcome.usage,
                        zeroclaw_providers::ReliableProviderTerminalFailure::from_error(
                            &provider_error,
                        ),
                    )
                    .into());
                }
                if !outcome.tool_calls.is_empty() {
                    // A client tool call has already advanced the turn. This
                    // defense protects direct or legacy providers that report
                    // a generic stream error instead of a typed no-replay
                    // terminal failure after emitting that call.
                    return Err(StreamTerminalCompletion {
                        failure: zeroclaw_api::model_provider::TerminalCompletionFailure::new(
                            zeroclaw_api::model_provider::TerminalCompletionError::InvalidTerminalReason,
                            outcome.usage,
                        ),
                        policy: zeroclaw_providers::TerminalCompletionPolicy::new(
                            zeroclaw_providers::TerminalRecoveryDisposition::NoReplay,
                            zeroclaw_providers::TerminalUsageChargeability::Billable,
                        ),
                    }
                    .into());
                }
                if outcome.saw_pre_executed_tool_activity && !forwarded_text.is_empty() {
                    return Err(StreamInterruptedAfterOutput::transport(
                        forwarded_text,
                        message,
                        outcome.usage,
                    )
                    .into());
                }
                if outcome.saw_pre_executed_tool_activity {
                    return Err(StreamPreExecutedToolsWithoutFinalResponse {
                        usage: outcome.usage,
                        cause: Some(StreamPreExecutedToolsCause::Reliable(
                            zeroclaw_providers::ReliableProviderTerminalFailure::from_error(
                                &provider_error,
                            ),
                        )),
                    }
                    .into());
                }
                return Err(StreamErrorWithUsage {
                    message,
                    usage: outcome.usage,
                }
                .into());
            }
        };
        match event {
            StreamEvent::Final => break,
            StreamEvent::Usage(usage) => {
                outcome.usage = Some(usage);
            }
            StreamEvent::ToolCall(tool_call) => {
                outcome.tool_calls.push(tool_call);
            }
            // Transient, human-readable thinking progress. Surfaced via
            // TurnEvent::Thinking (draft forwarding stays gated by the
            // visibility policy). It never enters reasoning_content: the
            // durable signed replay payload arrives separately via
            // StreamEvent::ReasoningFinalized.
            StreamEvent::ThinkingDelta(delta) => {
                if delta.is_empty() {
                    continue;
                }
                if draft_reasoning == StreamReasoningMode::Full
                    && let Some(tx) = on_delta
                {
                    let _ = tx.send(StreamDelta::Reasoning(delta.clone())).await;
                }
                if let Some(tx) = event_tx
                    && tx.send(TurnEvent::Thinking { delta }).await.is_ok()
                {
                    visible_event_output = true;
                }
            }
            // Durable replay-only finalized reasoning: appended to
            // reasoning_content for history reconstruction and never
            // surfaced as user-visible progress.
            StreamEvent::ReasoningFinalized(payload) => {
                outcome.reasoning_content.push_str(&payload);
            }
            // Pre-executed tool events are for observability only: they are
            // relayed as TurnEvents but do not affect the agent's tool
            // dispatch loop.
            StreamEvent::PreExecutedToolCall { name, args } => {
                outcome.saw_pre_executed_tool_activity = true;
                let id = Uuid::new_v4().to_string();
                pre_executed_ids
                    .entry(name.clone())
                    .or_default()
                    .push_back(id.clone());
                if let Some(tx) = event_tx
                    && tx
                        .send(TurnEvent::ToolCall {
                            id,
                            name,
                            args: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
                        })
                        .await
                        .is_ok()
                {
                    visible_event_output = true;
                }
            }
            StreamEvent::PreExecutedToolResult { name, output } => {
                outcome.saw_pre_executed_tool_activity = true;
                let id = pre_executed_ids
                    .get_mut(&name)
                    .and_then(|ids| ids.pop_front())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                if let Some(tx) = event_tx
                    && tx
                        .send(TurnEvent::ToolResult {
                            id,
                            name,
                            output,
                            artifact: None,
                        })
                        .await
                        .is_ok()
                {
                    visible_event_output = true;
                }
            }
            StreamEvent::TextDelta(chunk) => {
                if let Some(reasoning) = chunk.reasoning.as_deref()
                    && !reasoning.is_empty()
                {
                    // Legacy readable-reasoning path (providers that stream
                    // unsigned reasoning text in the chunk): accumulated for
                    // history and surfaced per the visibility policy. The
                    // signed Anthropic replay payload never travels here —
                    // providers must use StreamEvent::ReasoningFinalized for
                    // that.
                    outcome.reasoning_content.push_str(reasoning);
                    if draft_reasoning == StreamReasoningMode::Full
                        && let Some(tx) = on_delta
                    {
                        let _ = tx.send(StreamDelta::Reasoning(reasoning.to_string())).await;
                    }
                    // Thinking is surfaced as its own TurnEvent variant; it
                    // must never reach the Chunk/draft text surfaces.
                    if let Some(tx) = event_tx
                        && tx
                            .send(TurnEvent::Thinking {
                                delta: reasoning.to_string(),
                            })
                            .await
                            .is_ok()
                    {
                        visible_event_output = true;
                    }
                }

                if chunk.delta.is_empty() {
                    continue;
                }

                let sanitized_delta = think_stripper.push(&chunk.delta);
                if sanitized_delta.is_empty() {
                    continue;
                }

                // First pass through the marker stripper to strip terminal markers
                let stripped = marker_stripper.push(&sanitized_delta);

                // Append the stripped text to response_text (single accumulation path)
                outcome.response_text.push_str(&stripped);

                if strict_tool_parsing {
                    forward_visible!(stripped, true);
                    continue;
                }

                let Some(forward_text) = text_guard.push(&stripped) else {
                    continue;
                };

                forward_visible!(forward_text, true);
            }
        }
    }

    // Process trailing delta from think stripper through marker stripper
    let trailing_delta = think_stripper.finish();
    if !trailing_delta.is_empty() {
        let trailing_stripped = marker_stripper.push(&trailing_delta);
        outcome.response_text.push_str(&trailing_stripped);
        if strict_tool_parsing {
            forward_visible!(trailing_stripped, false);
        } else if let Some(forward_text) = text_guard.push(&trailing_stripped) {
            forward_visible!(forward_text, false);
        }
    }

    // Flush any remaining terminal markers held by the marker stripper
    let remaining = marker_stripper.finish();
    if !remaining.is_empty() {
        outcome.response_text.push_str(&remaining);
        if strict_tool_parsing {
            forward_visible!(remaining, false);
        } else if let Some(forward_text) = text_guard.push(&remaining) {
            forward_visible!(forward_text, false);
        }
    }

    if let Some(forward_text) = text_guard.finish() {
        forward_visible!(forward_text, false);
    }
    // Keep the leading-whitespace buffer intentionally unflushed: without a
    // later meaningful character it was never immutable user-visible output.
    let _ = saw_meaningful_visible_text;
    // Final forward may null delta_sender on send failure; mark it read.
    let _ = delta_sender;
    outcome.suppressed_protocol = text_guard.suppressed_protocol;

    if outcome.response_text.trim().is_empty() && outcome.tool_calls.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "has_reasoning": !outcome.reasoning_content.trim().is_empty(),
                    "protocol_suppressed": outcome.suppressed_protocol,
                })),
            "model_provider stream completed without final text or tool calls"
        );
        if outcome.saw_pre_executed_tool_activity {
            return Err(StreamPreExecutedToolsWithoutFinalResponse {
                usage: outcome.usage,
                cause: None,
            }
            .into());
        }
        return Err(StreamSemanticEmptyCompletion {
            usage: outcome.usage,
            // Reasoning delivered through TurnEvent is already immutable
            // user-visible progress. A semantic-empty terminal result after
            // that delivery must not replay the completed provider request.
            replayable: !visible_event_output,
        }
        .into());
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use zeroclaw_api::model_provider::StreamChunk;
    use zeroclaw_providers::ToolCall;
    use zeroclaw_providers::traits::{
        ChatResponse, ProviderCapabilities, StreamOptions, StreamResult, TokenUsage,
    };

    struct ToolThenTextProvider;
    struct ToolThenGenericErrorProvider;
    struct SemanticEmptyAfterTextProvider;

    struct EmptyStreamProvider;
    struct ProviderToolThenVisibleFailureProvider;
    struct ProviderToolThenTerminalFailureProvider;
    struct TerminalAfterTextProvider;
    struct ReasoningProvider;
    struct CancelAfterUsageProvider {
        cancellation: CancellationToken,
        cancel_after_visible_output: bool,
    }

    impl ::zeroclaw_api::attribution::Attributable for ToolThenTextProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ToolThenTextProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ToolThenGenericErrorProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ToolThenGenericErrorProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for SemanticEmptyAfterTextProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "SemanticEmptyAfterTextProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for EmptyStreamProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "EmptyStreamProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ProviderToolThenVisibleFailureProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ProviderToolThenVisibleFailureProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ProviderToolThenTerminalFailureProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ProviderToolThenTerminalFailureProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for TerminalAfterTextProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "TerminalAfterTextProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ReasoningProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ReasoningProvider"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for CancelAfterUsageProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "CancelAfterUsageProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for ToolThenTextProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
                extended_thinking: false,
            }
        }

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
            anyhow::bail!("unused")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            let tool_call = ToolCall {
                id: "call_1".to_string(),
                name: "noop".to_string(),
                arguments: "{}".to_string(),
                extra_content: None,
            };
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta("Let me "))),
                Ok(StreamEvent::ToolCall(tool_call)),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    "check the count.",
                ))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for ToolThenGenericErrorProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
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
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "call_1".to_string(),
                    name: "noop".to_string(),
                    arguments: "{}".to_string(),
                    extra_content: None,
                })),
                Err(zeroclaw_api::model_provider::StreamError::ModelProvider(
                    "stream broke".to_string(),
                )),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for SemanticEmptyAfterTextProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
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
                Ok(StreamEvent::TextDelta(StreamChunk::delta("visible"))),
                Err(zeroclaw_api::model_provider::StreamError::SemanticEmpty(
                    zeroclaw_api::model_provider::SemanticEmptyTerminalFailure::new(None),
                )),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for EmptyStreamProvider {
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
            anyhow::bail!("unused")
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
                Ok(StreamEvent::ThinkingDelta("internal".to_string())),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for ProviderToolThenVisibleFailureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
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
                Ok(StreamEvent::PreExecutedToolCall {
                    name: "search".to_string(),
                    args: "{}".to_string(),
                }),
                Ok(StreamEvent::TextDelta(StreamChunk::delta(
                    "visible partial",
                ))),
                Err(zeroclaw_api::model_provider::StreamError::ModelProvider(
                    "upstream failed".to_string(),
                )),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for ProviderToolThenTerminalFailureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
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
                    name: "search".to_string(),
                    args: "{}".to_string(),
                }),
                Err(
                    zeroclaw_api::model_provider::StreamError::TerminalCompletion(
                        zeroclaw_api::model_provider::TerminalCompletionFailure::from(
                            zeroclaw_api::model_provider::TerminalCompletionError::OutputTokenLimit,
                        ),
                    ),
                ),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for TerminalAfterTextProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("unused")
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
                Ok(StreamEvent::TextDelta(StreamChunk::reasoning("internal"))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("draft text"))),
                Err(
                    zeroclaw_api::model_provider::StreamError::TerminalCompletion(
                        zeroclaw_api::model_provider::TerminalCompletionFailure::new(
                            zeroclaw_api::model_provider::TerminalCompletionError::OutputTokenLimit,
                            None,
                        ),
                    ),
                ),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for ReasoningProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: false,
                prompt_caching: false,
                extended_thinking: true,
            }
        }

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
            anyhow::bail!("unused")
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
                Ok(StreamEvent::TextDelta(StreamChunk {
                    delta: "answer<eom>".to_string(),
                    reasoning: Some("private <eom>".to_string()),
                    is_final: false,
                    token_count: 0,
                })),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    /// Emits the post-repair Anthropic pair: a durable signed replay payload
    /// plus a transient readable delta, in that order, then text.
    struct ThinkingSplitProvider;

    impl ::zeroclaw_api::attribution::Attributable for ThinkingSplitProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ThinkingSplitProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for ThinkingSplitProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: false,
                prompt_caching: false,
                extended_thinking: true,
            }
        }

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
            anyhow::bail!("unused")
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
                Ok(StreamEvent::ReasoningFinalized(
                    r#"{"thinking":"secret reasoning","signature":"SIG"}"#.to_string(),
                )),
                Ok(StreamEvent::ThinkingDelta("readable progress".to_string())),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("answer"))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    #[async_trait]
    impl ModelProvider for CancelAfterUsageProvider {
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
            anyhow::bail!("unused")
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
            let cancellation = self.cancellation.clone();
            let cancel_after_visible_output = self.cancel_after_visible_output;
            Box::pin(futures_util::stream::unfold(0_u8, move |state| {
                let cancellation = cancellation.clone();
                async move {
                    match state {
                        0 => Some((
                            Ok(StreamEvent::Usage(TokenUsage {
                                input_tokens: Some(10),
                                output_tokens: Some(5),
                                cached_input_tokens: None,
                            })),
                            1,
                        )),
                        1 => {
                            if cancel_after_visible_output {
                                return Some((
                                    Ok(StreamEvent::TextDelta(StreamChunk::delta("visible"))),
                                    2,
                                ));
                            }
                            cancellation.cancel();
                            std::future::pending::<()>().await;
                            None
                        }
                        2 => {
                            std::future::pending::<()>().await;
                            None
                        }
                        _ => None,
                    }
                }
            }))
        }
    }

    #[tokio::test]
    async fn forwards_text_deltas_emitted_after_a_native_tool_call() {
        let provider = ToolThenTextProvider;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");
        drop(event_tx);

        let mut forwarded = String::new();
        while let Some(event) = event_rx.recv().await {
            if let TurnEvent::Chunk { delta } = event {
                forwarded.push_str(&delta);
            }
        }

        assert_eq!(outcome.tool_calls.len(), 1);
        assert!(
            forwarded.contains("check the count."),
            "narration emitted after the native tool call must be forwarded live; forwarded={forwarded:?}"
        );
    }

    #[tokio::test]
    async fn rejects_completed_stream_without_text_or_tool_calls() {
        let err = consume_provider_streaming_response(
            &EmptyStreamProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("a semantically empty stream must not complete successfully");

        assert!(
            err.downcast_ref::<StreamSemanticEmptyCompletion>()
                .is_some()
        );
    }

    #[tokio::test]
    async fn reasoning_delivered_to_event_sink_makes_empty_terminal_no_replay() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        let err = consume_provider_streaming_response(
            &EmptyStreamProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("a reasoning-only terminal stream must fail");

        let semantic_empty = err
            .downcast_ref::<StreamSemanticEmptyCompletion>()
            .expect("failure stays typed");
        assert!(
            !semantic_empty.replayable,
            "a delivered immutable reasoning event prevents replay"
        );
        assert!(matches!(
            event_rx.recv().await,
            Some(TurnEvent::Thinking { delta }) if delta == "internal"
        ));
    }

    #[tokio::test]
    async fn failed_thinking_event_send_keeps_empty_terminal_replayable() {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(1);
        drop(event_rx);
        let error = consume_provider_streaming_response(
            &EmptyStreamProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("a reasoning-only terminal stream must fail");

        let semantic_empty = error
            .downcast_ref::<StreamSemanticEmptyCompletion>()
            .expect("failure stays typed");
        assert!(
            semantic_empty.replayable,
            "a failed immutable thinking send cannot suppress recovery"
        );
    }

    #[tokio::test]
    async fn generic_error_after_client_tool_call_is_no_replay_terminal_failure() {
        let error = consume_provider_streaming_response(
            &ToolThenGenericErrorProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("client tool followed by a generic error must not replay");

        let terminal = error
            .downcast_ref::<StreamTerminalCompletion>()
            .expect("client-tool boundary must use typed terminal failure");
        assert_eq!(
            terminal.failure.reason,
            zeroclaw_api::model_provider::TerminalCompletionError::InvalidTerminalReason
        );
        assert_eq!(
            terminal.policy.recovery(),
            zeroclaw_providers::TerminalRecoveryDisposition::NoReplay
        );
    }

    #[tokio::test]
    async fn semantic_empty_after_visible_chunk_preserves_no_replay_partial() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        let error = consume_provider_streaming_response(
            &SemanticEmptyAfterTextProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("visible text must prevent semantic-empty replay");

        let interrupted = error
            .downcast_ref::<StreamInterruptedAfterOutput>()
            .expect("immutable output must retain its partial prefix");
        assert_eq!(interrupted.partial_text, "visible");
        assert!(
            zeroclaw_api::model_provider::semantic_empty_terminal_failure(&error)
                .is_some_and(|failure| !failure.is_replayable())
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(TurnEvent::Chunk { delta }) if delta == "visible"
        ));
    }

    #[tokio::test]
    async fn provider_tool_failure_after_visible_text_preserves_partial_output() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        let error = consume_provider_streaming_response(
            &ProviderToolThenVisibleFailureProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("provider-tool stream failure must remain non-replayable");

        let interrupted = error
            .downcast_ref::<StreamInterruptedAfterOutput>()
            .expect("visible partial output must use the interruption outcome");
        assert_eq!(interrupted.partial_text, "visible partial");
        assert_eq!(
            interrupted.usage().and_then(|usage| usage.output_tokens),
            Some(5)
        );
        assert!(
            zeroclaw_api::model_provider::semantic_empty_terminal_failure(&error)
                .is_some_and(|failure| failure.has_pre_executed_tool_activity()),
            "the partial-output wrapper must retain the no-replay semantic-empty cause"
        );

        let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            TurnEvent::Chunk { delta } if delta == "visible partial"
        )));
    }

    #[tokio::test]
    async fn provider_tool_terminal_failure_preserves_typed_delivery_cause_without_replay() {
        let error = consume_provider_streaming_response(
            &ProviderToolThenTerminalFailureProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("provider-tool terminal failure must surface");

        assert!(
            error
                .downcast_ref::<StreamPreExecutedToolsWithoutFinalResponse>()
                .is_some(),
            "provider work must keep the no-replay outcome"
        );
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<zeroclaw_api::model_provider::TerminalCompletionFailure>()
                .is_some_and(|failure| {
                    failure.reason
                        == zeroclaw_api::model_provider::TerminalCompletionError::OutputTokenLimit
                })
        }));
        assert_eq!(
            crate::agent::turn::outcome::terminal_completion_error_message(&error, None),
            Some(
                "The provider reached its output token limit before completing the response."
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn draft_only_text_does_not_disable_terminal_recovery() {
        let (draft_tx, mut draft_rx) = tokio::sync::mpsc::channel(4);
        let error = consume_provider_streaming_response(
            &TerminalAfterTextProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&draft_tx),
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("the terminal failure must surface");

        assert!(
            error
                .downcast_ref::<StreamInterruptedAfterOutput>()
                .is_none(),
            "a mutable draft is not an immutable persisted prefix"
        );
        assert!(error.downcast_ref::<StreamTerminalCompletion>().is_some());
        assert!(matches!(
            draft_rx.try_recv(),
            Ok(StreamDelta::Text(text)) if text == "draft text"
        ));
    }

    #[tokio::test]
    async fn failed_event_send_does_not_create_a_persisted_prefix() {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(1);
        drop(event_rx);
        let error = consume_provider_streaming_response(
            &TerminalAfterTextProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("the terminal failure must surface");

        assert!(
            error
                .downcast_ref::<StreamInterruptedAfterOutput>()
                .is_none(),
            "a failed immutable send cannot become a persisted prefix"
        );
        assert!(error.downcast_ref::<StreamTerminalCompletion>().is_some());
    }

    #[tokio::test]
    async fn cancellation_after_reported_usage_preserves_rejected_usage() {
        let cancellation = CancellationToken::new();
        let provider = CancelAfterUsageProvider {
            cancellation: cancellation.clone(),
            cancel_after_visible_output: false,
        };

        let error = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            Some(&cancellation),
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("cancellation must interrupt the stream");

        let cancelled = error
            .downcast_ref::<StreamCancelledWithUsage>()
            .expect("pre-output cancellation retains its typed outcome");
        let usage = cancelled
            .usage
            .as_ref()
            .expect("reported usage must survive cancellation");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(5));
    }

    #[tokio::test]
    async fn cancellation_after_visible_output_preserves_usage_and_partial_text() {
        let cancellation = CancellationToken::new();
        let provider = CancelAfterUsageProvider {
            cancellation: cancellation.clone(),
            cancel_after_visible_output: true,
        };
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        let cancel_after_chunk = cancellation.clone();
        let observed_chunk = zeroclaw_spawn::spawn!(async move {
            let event = event_rx.recv().await;
            cancel_after_chunk.cancel();
            event
        });

        let error = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            Some(&cancellation),
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("cancellation must interrupt the stream");

        let cancelled = error
            .downcast_ref::<StreamCancelledAfterOutput>()
            .expect("visible cancellation keeps its typed partial outcome");
        assert_eq!(cancelled.partial_text, "visible");
        let usage = cancelled
            .usage
            .as_ref()
            .expect("reported usage must survive visible cancellation");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(5));
        match observed_chunk.await.expect("chunk observer task must join") {
            Some(TurnEvent::Chunk { delta }) => assert_eq!(delta, "visible"),
            other => panic!("expected one visible text chunk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn thinking_lifetime_split_keeps_signatures_out_of_visible_events() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);
        let outcome = consume_provider_streaming_response(
            &ThinkingSplitProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&event_tx),
            false,
            StreamReasoningMode::Full,
        )
        .await
        .expect("stream consume should succeed");
        drop(event_tx);

        // Durable payload retained byte-for-byte for replay.
        assert_eq!(
            outcome.reasoning_content,
            r#"{"thinking":"secret reasoning","signature":"SIG"}"#
        );
        assert_eq!(outcome.response_text, "answer");

        let mut thinking_events = Vec::new();
        while let Some(event) = event_rx.recv().await {
            if let TurnEvent::Thinking { delta } = event {
                thinking_events.push(delta);
            }
        }
        assert_eq!(
            thinking_events,
            vec!["readable progress"],
            "only the transient delta is user-visible thinking"
        );
        assert!(
            !thinking_events
                .iter()
                .any(|delta| delta.contains("SIG") || delta.contains("signature")),
            "the signed replay payload must never reach TurnEvent::Thinking"
        );
    }

    #[tokio::test]
    async fn thinking_deltas_forward_to_drafts_only_in_full_mode() {
        // Full mode: the readable delta is forwarded to drafts.
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);
        consume_provider_streaming_response(
            &ThinkingSplitProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&delta_tx),
            None,
            false,
            StreamReasoningMode::Full,
        )
        .await
        .expect("stream consume should succeed");
        drop(delta_tx);
        let deltas: Vec<_> = std::iter::from_fn(|| delta_rx.try_recv().ok()).collect();
        assert!(deltas.iter().any(
            |delta| matches!(delta, StreamDelta::Reasoning(text) if text == "readable progress")
        ));

        // Status mode (default): no reasoning in drafts.
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);
        consume_provider_streaming_response(
            &ThinkingSplitProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&delta_tx),
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");
        drop(delta_tx);
        while let Some(delta) = delta_rx.recv().await {
            assert!(
                !matches!(delta, StreamDelta::Reasoning(_)),
                "status mode must not expose reasoning deltas"
            );
        }
    }

    #[tokio::test]
    async fn reasoning_status_mode_keeps_reasoning_out_of_draft_deltas() {
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);
        let outcome = consume_provider_streaming_response(
            &ReasoningProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&delta_tx),
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");
        drop(delta_tx);

        assert_eq!(outcome.reasoning_content, "private <eom>");
        while let Some(delta) = delta_rx.recv().await {
            assert!(
                !matches!(delta, StreamDelta::Reasoning(_)),
                "status mode must not expose raw reasoning"
            );
        }
    }

    #[tokio::test]
    async fn reasoning_full_mode_keeps_raw_reasoning_separate_from_terminal_markers() {
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);
        let outcome = consume_provider_streaming_response(
            &ReasoningProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&delta_tx),
            None,
            false,
            StreamReasoningMode::Full,
        )
        .await
        .expect("stream consume should succeed");
        drop(delta_tx);

        let deltas: Vec<_> = std::iter::from_fn(|| delta_rx.try_recv().ok()).collect();
        assert_eq!(outcome.reasoning_content, "private <eom>");
        assert_eq!(outcome.response_text, "answer");
        assert!(deltas.iter().any(|delta| matches!(
            delta,
            StreamDelta::Reasoning(text) if text == "private <eom>"
        )));
        assert!(
            deltas
                .iter()
                .any(|delta| matches!(delta, StreamDelta::Text(text) if text == "answer"))
        );
    }

    #[tokio::test]
    async fn reasoning_off_mode_emits_no_reasoning_draft_delta() {
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(8);
        consume_provider_streaming_response(
            &ReasoningProvider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            Some(&delta_tx),
            None,
            false,
            StreamReasoningMode::Off,
        )
        .await
        .expect("stream consume should succeed");
        drop(delta_tx);

        while let Some(delta) = delta_rx.recv().await {
            assert!(
                !matches!(delta, StreamDelta::Reasoning(_)),
                "off mode must not expose raw reasoning"
            );
        }
    }

    struct MarkerTestProvider {
        text_sequence: Vec<&'static str>,
    }

    impl MarkerTestProvider {
        fn with_text_sequence(texts: Vec<&'static str>) -> Self {
            Self {
                text_sequence: texts,
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for MarkerTestProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MarkerTestProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for MarkerTestProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
                extended_thinking: false,
            }
        }

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
            anyhow::bail!("unused")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            let events: Vec<StreamResult<StreamEvent>> = self
                .text_sequence
                .iter()
                .map(|text| Ok(StreamEvent::TextDelta(StreamChunk::delta(*text))))
                .chain(std::iter::once(Ok(StreamEvent::Final)))
                .collect();
            Box::pin(futures_util::stream::iter(events))
        }
    }

    #[tokio::test]
    async fn strips_terminal_marker_same_chunk() {
        let provider = MarkerTestProvider::with_text_sequence(vec!["Summary<eom>"]);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");

        // Critical: response_text should NOT contain the marker
        assert_eq!(
            outcome.response_text, "Summary",
            "terminal marker <eom> should be stripped from response_text"
        );
    }

    #[tokio::test]
    async fn strips_terminal_marker_in_strict_mode() {
        let provider = MarkerTestProvider::with_text_sequence(vec!["Summary<eom>"]);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            true, // strict_tool_parsing = true
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");

        // Critical: strict mode should also strip terminal markers
        assert_eq!(
            outcome.response_text, "Summary",
            "terminal marker <eom> should be stripped even in strict_tool_parsing mode"
        );
    }

    /// Provider that emits a marker-only text delta followed by a typed
    /// terminal failure. The marker is protocol metadata, not user-visible
    /// partial output, so the failure must retain pre-output recovery.
    struct MarkerOnlyThenErrorProvider {
        text: &'static str,
    }

    impl ::zeroclaw_api::attribution::Attributable for MarkerOnlyThenErrorProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MarkerOnlyThenErrorProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for MarkerOnlyThenErrorProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
                extended_thinking: false,
            }
        }

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
            anyhow::bail!("unused")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            let events: Vec<StreamResult<StreamEvent>> =
                vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(self.text))),
                Err(::zeroclaw_api::model_provider::StreamError::TerminalCompletion(
                    zeroclaw_api::model_provider::TerminalCompletionFailure::new(
                        zeroclaw_api::model_provider::TerminalCompletionError::OutputTokenLimit,
                        None,
                    ),
                )),
            ];
            Box::pin(futures_util::stream::iter(events))
        }
    }

    /// Marker-only deltas must not count as visible output: otherwise a later
    /// terminal failure becomes `StreamInterruptedAfterOutput`, suppressing
    /// an allowed fallback even though the user received no text.
    #[tokio::test]
    async fn marker_only_delta_keeps_terminal_recovery_pre_output() {
        let provider = MarkerOnlyThenErrorProvider { text: "<eom>" };

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let result = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,      // on_delta (draft sink)
            Some(&tx), // event_tx
            true,      // strict_tool_parsing = true
            StreamReasoningMode::Status,
        )
        .await;

        // The event sink must have seen zero Chunks (the marker-only delta was
        // stripped to empty and must not be forwarded).
        drop(tx);
        let mut chunks = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            chunks.push(ev);
        }
        assert!(
            chunks.is_empty(),
            "no Chunk event may be forwarded for a marker-only delta: {chunks:?}"
        );

        let err = result.expect_err("terminal failure must surface");
        assert!(
            err.downcast_ref::<crate::agent::turn::outcome::StreamInterruptedAfterOutput>()
                .is_none(),
            "a marker-only terminal failure must stay eligible for fallback; \
             got StreamInterruptedAfterOutput instead"
        );
        let terminal = err
            .downcast_ref::<StreamTerminalCompletion>()
            .expect("marker-only terminal failure must preserve its typed cause");
        assert_eq!(
            terminal.failure.reason,
            zeroclaw_api::model_provider::TerminalCompletionError::OutputTokenLimit
        );
        assert_eq!(
            terminal.policy.recovery(),
            zeroclaw_providers::TerminalRecoveryDisposition::NextCandidate
        );
    }

    #[tokio::test]
    async fn whitespace_before_terminal_marker_does_not_disable_pre_output_recovery() {
        let provider = MarkerOnlyThenErrorProvider { text: " <eom>" };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let result = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&tx),
            true,
            StreamReasoningMode::Status,
        )
        .await;

        drop(tx);
        assert!(
            rx.try_recv().is_err(),
            "whitespace before a stripped terminal marker is not a visible Chunk"
        );
        let err = result.expect_err("terminal failure must surface");
        let terminal = err
            .downcast_ref::<StreamTerminalCompletion>()
            .expect("blank output must remain eligible for pre-output recovery");
        assert_eq!(
            terminal.policy.recovery(),
            zeroclaw_providers::TerminalRecoveryDisposition::NextCandidate
        );
    }

    #[tokio::test]
    async fn whitespace_only_stream_emits_no_chunk_and_is_replayable_semantic_empty() {
        let provider = MarkerTestProvider::with_text_sequence(vec![" \n\t"]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let err = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&tx),
            true,
            StreamReasoningMode::Status,
        )
        .await
        .expect_err("whitespace-only terminal output must fail semantically");

        drop(tx);
        assert!(
            rx.try_recv().is_err(),
            "whitespace-only output must not become an immutable Chunk"
        );
        let semantic_empty = err
            .downcast_ref::<StreamSemanticEmptyCompletion>()
            .expect("whitespace-only output must use the semantic-empty outcome");
        assert!(semantic_empty.replayable);
    }

    #[tokio::test]
    async fn leading_whitespace_is_released_with_later_meaningful_text() {
        let provider = MarkerTestProvider::with_text_sequence(vec!["  ", "answer"]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            Some(&tx),
            true,
            StreamReasoningMode::Status,
        )
        .await
        .expect("meaningful text after whitespace must complete");

        drop(tx);
        let event = rx.recv().await.expect("meaningful text must be forwarded");
        assert!(matches!(event, TurnEvent::Chunk { ref delta } if delta == "  answer"));
        assert!(
            rx.try_recv().is_err(),
            "leading whitespace must not become a separate Chunk"
        );
        assert_eq!(outcome.forwarded_visible_text, "  answer");
    }

    #[tokio::test]
    async fn strips_fragmented_terminal_marker() {
        let provider = MarkerTestProvider::with_text_sequence(vec!["Summary<", "eom>"]);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");

        assert_eq!(
            outcome.response_text, "Summary",
            "fragmented terminal marker across chunks should be stripped"
        );
    }

    #[tokio::test]
    async fn strips_stacked_terminal_markers() {
        let provider = MarkerTestProvider::with_text_sequence(vec!["Summary<eom><|eom|>"]);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");

        assert_eq!(
            outcome.response_text, "Summary",
            "stacked terminal markers should be stripped"
        );
    }

    #[tokio::test]
    async fn preserves_inline_marker_but_strips_terminal() {
        let provider = MarkerTestProvider::with_text_sequence(vec!["Text <eom> inline<eom>"]);

        let outcome = consume_provider_streaming_response(
            &provider,
            &[ChatMessage::user("go")],
            None,
            "mock-model",
            Some(0.0),
            None,
            None,
            None,
            false,
            StreamReasoningMode::Status,
        )
        .await
        .expect("stream consume should succeed");

        // The inline <eom> should be preserved, terminal <eom> stripped
        assert_eq!(
            outcome.response_text, "Text <eom> inline",
            "inline marker should be preserved but terminal marker stripped"
        );
    }

    /// Provider that emits one text delta ending in a terminal marker, then
    /// delays before `Final` so the test can observe whether the answer
    /// streamed live or was buffered until completion.
    struct LiveMarkerTimingProvider;

    impl ::zeroclaw_api::attribution::Attributable for LiveMarkerTimingProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "LiveMarkerTimingProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for LiveMarkerTimingProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
                extended_thinking: false,
            }
        }

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
            anyhow::bail!("unused")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            // Emit the delta immediately, then pause before Final so the
            // consumer's forwarding timing is observable: if the stripper
            // buffers the whole chunk, no Chunk event is sent until the
            // (delayed) finish path and the test times out.
            Box::pin(futures_util::stream::unfold(0u8, |state| async move {
                match state {
                    0 => Some((
                        Ok(StreamEvent::TextDelta(StreamChunk::delta(
                            "A large answer<eom>",
                        ))),
                        1,
                    )),
                    _ => {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        Some((Ok(StreamEvent::Final), 2))
                    }
                }
            }))
        }
    }

    #[tokio::test]
    async fn streams_single_marker_chunk_live_before_final() {
        let provider = LiveMarkerTimingProvider;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);

        let handle = zeroclaw_spawn::spawn!(async move {
            consume_provider_streaming_response(
                &provider,
                &[ChatMessage::user("go")],
                None,
                "mock-model",
                Some(0.0),
                None,
                None,
                Some(&event_tx),
                false,
                StreamReasoningMode::Status,
            )
            .await
        });

        // The answer must be forwarded as a live Chunk before the provider's
        // Final (which is delayed 500ms). The old stripper held the whole
        // chunk until `finish()` after Final, so nothing arrived in time.
        let chunk = tokio::time::timeout(std::time::Duration::from_millis(200), event_rx.recv())
            .await
            .expect("the answer must stream live before the provider's Final event")
            .expect("event channel remains open");

        match chunk {
            TurnEvent::Chunk { delta } => assert_eq!(
                delta, "A large answer",
                "the live chunk must be the stripped answer, without the terminal marker"
            ),
            other => panic!("expected the stripped answer as a live Chunk, got {other:?}"),
        }

        let outcome = handle
            .await
            .expect("consume task completes")
            .expect("stream consume should succeed");
        assert_eq!(
            outcome.response_text, "A large answer",
            "the final accumulated response strips the terminal marker"
        );
    }
}
