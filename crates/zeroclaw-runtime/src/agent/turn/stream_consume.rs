//! Streaming provider-response consumption for the turn loop.

use super::events::{DraftEvent, StreamDelta};
use super::outcome::{
    StreamCancelledAfterOutput, StreamCancelledWithUsage, StreamErrorWithUsage,
    StreamInterruptedAfterOutput, StreamPreExecutedToolsWithoutFinalResponse,
    StreamSemanticEmptyCompletion,
};
use super::stream_guard::{StreamTerminalMarkerStripper, StreamTextGuard, StreamThinkTagStripper};
use anyhow::Result;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::model_provider::StreamEvent;
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
) -> Result<StreamedChatOutcome> {
    let mut provider_stream = ProviderDispatch::from_ref(model_provider).stream_chat(
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
            if !visible.is_empty() {
                if event_tx.is_some() || delta_sender.is_some() {
                    outcome.forwarded_visible_text.push_str(&visible);
                }
                if let Some(tx) = event_tx {
                    outcome.forwarded_live_deltas = true;
                    forward_visible!(@count $count_visible, visible);
                    let _ = tx
                        .send(TurnEvent::Chunk {
                            delta: visible.clone(),
                        })
                        .await;
                }
                if let Some(tx) = delta_sender {
                    outcome.forwarded_live_deltas = true;
                    if tx.send(StreamDelta::Text(visible)).await.is_err() {
                        delta_sender = None;
                    }
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
                        return Err(StreamCancelledWithUsage::new(outcome.usage).into());
                    }
                    return Err(StreamCancelledAfterOutput::with_usage(
                        forwarded_text,
                        outcome.usage,
                    )
                    .into());
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
                let message = format!("model_provider stream error: {err}");
                if visible_event_output {
                    // Persist only what the consumer actually saw
                    // (`forwarded_text`), never the raw accumulated text —
                    // that includes guard-withheld protocol fragments and
                    // suppression-buffered output nobody received.
                    return Err(StreamInterruptedAfterOutput {
                        partial_text: forwarded_text,
                        message,
                        usage: outcome.usage,
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
                if let Some(tx) = event_tx {
                    visible_event_output = true;
                    let _ = tx
                        .send(TurnEvent::ToolCall {
                            id,
                            name,
                            args: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
                        })
                        .await;
                }
            }
            StreamEvent::PreExecutedToolResult { name, output } => {
                outcome.saw_pre_executed_tool_activity = true;
                let id = pre_executed_ids
                    .get_mut(&name)
                    .and_then(|ids| ids.pop_front())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                if let Some(tx) = event_tx {
                    visible_event_output = true;
                    let _ = tx
                        .send(TurnEvent::ToolResult {
                            id,
                            name,
                            output,
                            artifact: None,
                        })
                        .await;
                }
            }
            StreamEvent::TextDelta(chunk) => {
                if let Some(reasoning) = chunk.reasoning.as_deref()
                    && !reasoning.is_empty()
                {
                    outcome.reasoning_content.push_str(reasoning);
                    // Thinking is surfaced as its own TurnEvent variant; it
                    // must never reach the Chunk/draft text surfaces.
                    if let Some(tx) = event_tx {
                        visible_event_output = true;
                        let _ = tx
                            .send(TurnEvent::Thinking {
                                delta: reasoning.to_string(),
                            })
                            .await;
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
            }
            .into());
        }
        return Err(StreamSemanticEmptyCompletion {
            usage: outcome.usage,
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

    struct EmptyStreamProvider;

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
            Box::pin(futures_util::stream::iter(vec![Ok(StreamEvent::Final)]))
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
        )
        .await
        .expect_err("a semantically empty stream must not complete successfully");

        assert_eq!(
            err.to_string(),
            "provider stream completed without final text or tool calls"
        );
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
        )
        .await
        .expect("stream consume should succeed");

        // Critical: strict mode should also strip terminal markers
        assert_eq!(
            outcome.response_text, "Summary",
            "terminal marker <eom> should be stripped even in strict_tool_parsing mode"
        );
    }

    /// Provider that emits a single marker-only text delta (`<eom>`) and then
    /// a stream error, with no text ever produced. Used to pin the strict-mode
    /// fallback eligibility: a marker-only delta yields empty stripped text,
    /// which must NOT count as visible output.
    struct MarkerOnlyThenErrorProvider;

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
            let events: Vec<StreamResult<StreamEvent>> = vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta("<eom>"))),
                Err(::zeroclaw_api::model_provider::StreamError::ModelProvider(
                    "provider exploded after marker-only delta".into(),
                )),
            ];
            Box::pin(futures_util::stream::iter(events))
        }
    }

    /// Strict-mode marker-only deltas must not count as visible output: if a
    /// marker-only delta (stripped to empty text) is forwarded as a visible
    /// Chunk, a later provider error turns the failure into
    /// `StreamInterruptedAfterOutput`, which disables the non-streaming
    /// fallback even though the user received no text. The error must instead
    /// surface as a plain error with an empty visible prefix, keeping the
    /// pre-output fallback eligible.
    #[tokio::test]
    async fn strict_marker_only_delta_does_not_disable_fallback_on_provider_error() {
        let provider = MarkerOnlyThenErrorProvider;

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

        // The failure must be a plain error, NOT StreamInterruptedAfterOutput
        // (which would disable the non-streaming fallback despite no visible
        // text). Asserting the error type is not the interruption type is the
        // observable fallback-eligibility signal.
        let err = result.expect_err("provider error must surface");
        assert!(
            err.downcast_ref::<crate::agent::turn::outcome::StreamInterruptedAfterOutput>()
                .is_none(),
            "a marker-only delta followed by a provider error must stay eligible for the \
             non-streaming fallback; got StreamInterruptedAfterOutput instead"
        );
        assert!(
            err.to_string().contains("provider exploded"),
            "the underlying provider error must be surfaced: {err}"
        );
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
