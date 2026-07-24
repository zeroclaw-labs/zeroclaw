//! Streaming provider-response consumption for the turn loop.

use super::events::{DraftEvent, StreamDelta};
use super::outcome::{StreamCancelledAfterOutput, StreamInterruptedAfterOutput, ToolLoopCancelled};
use super::stream_guard::{StreamTextGuard, StreamThinkTagStripper};
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
    let mut text_guard = StreamTextGuard::new(request_tools);
    let mut think_stripper = StreamThinkTagStripper::default();
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
                        return Err(ToolLoopCancelled.into());
                    }
                    return Err(StreamCancelledAfterOutput::new(forwarded_text).into());
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
                    }
                    .into());
                }
                return Err(anyhow::Error::msg(message));
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
                let id = pre_executed_ids
                    .get_mut(&name)
                    .and_then(|ids| ids.pop_front())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                if let Some(tx) = event_tx {
                    visible_event_output = true;
                    let _ = tx.send(TurnEvent::ToolResult { id, name, output }).await;
                }
            }
            StreamEvent::TextDelta(chunk) => {
                if let Some(reasoning) = chunk.reasoning.as_deref()
                    && !reasoning.is_empty()
                {
                    outcome.reasoning_content.push_str(reasoning);
                    match draft_reasoning {
                        StreamReasoningMode::Off => {}
                        StreamReasoningMode::Status => {}
                        StreamReasoningMode::Full => {
                            if let Some(tx) = on_delta {
                                let _ =
                                    tx.send(StreamDelta::Reasoning(reasoning.to_string())).await;
                            }
                        }
                    }
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

                outcome.response_text.push_str(&sanitized_delta);

                if strict_tool_parsing {
                    forward_visible!(sanitized_delta, true);
                    continue;
                }

                let Some(forward_text) = text_guard.push(&sanitized_delta) else {
                    continue;
                };

                forward_visible!(forward_text, true);
            }
        }
    }

    let trailing_delta = think_stripper.finish();
    if !trailing_delta.is_empty() {
        outcome.response_text.push_str(&trailing_delta);
        if strict_tool_parsing {
            forward_visible!(trailing_delta, false);
        } else if let Some(forward_text) = text_guard.push(&trailing_delta) {
            forward_visible!(forward_text, false);
        }
    }

    if let Some(forward_text) = text_guard.finish() {
        forward_visible!(forward_text, false);
    }
    // Final forward may null delta_sender on send failure; mark it read.
    let _ = delta_sender;
    outcome.suppressed_protocol = text_guard.suppressed_protocol;

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
        ChatResponse, ProviderCapabilities, StreamOptions, StreamResult,
    };

    struct ToolThenTextProvider;
    struct ReasoningProvider;

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
                Ok(StreamEvent::TextDelta(StreamChunk::reasoning(
                    "private reasoning",
                ))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("final"))),
                Ok(StreamEvent::Final),
            ]))
        }
    }

    struct MultiReasoningProvider;

    impl ::zeroclaw_api::attribution::Attributable for MultiReasoningProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "MultiReasoningProvider"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for MultiReasoningProvider {
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
                Ok(StreamEvent::TextDelta(StreamChunk::reasoning("first"))),
                Ok(StreamEvent::TextDelta(StreamChunk::reasoning(" second"))),
                Ok(StreamEvent::TextDelta(StreamChunk::delta("final"))),
                Ok(StreamEvent::Final),
            ]))
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
    async fn reasoning_status_mode_captures_reasoning_without_draft_delta() {
        let provider = MultiReasoningProvider;
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let outcome = consume_provider_streaming_response(
            &provider,
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

        while let Ok(delta) = delta_rx.try_recv() {
            match delta {
                StreamDelta::Text(text) => assert_eq!(text, "final"),
                StreamDelta::Status(text) => assert!(!text.contains("first second")),
                StreamDelta::Reasoning(text) => {
                    panic!("status mode must not emit raw reasoning: {text}")
                }
                StreamDelta::ToolStart { .. } | StreamDelta::ToolComplete { .. } => {}
            }
        }

        assert_eq!(outcome.reasoning_content, "first second");
    }

    #[tokio::test]
    async fn reasoning_full_mode_emits_raw_reasoning_delta() {
        let provider = ReasoningProvider;
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(16);

        let outcome = consume_provider_streaming_response(
            &provider,
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

        let mut deltas = Vec::new();
        while let Some(delta) = delta_rx.recv().await {
            deltas.push(delta);
        }

        assert_eq!(outcome.reasoning_content, "private reasoning");
        assert!(deltas.iter().any(|delta| matches!(
            delta,
            StreamDelta::Reasoning(text) if text == "private reasoning"
        )));
    }
}
