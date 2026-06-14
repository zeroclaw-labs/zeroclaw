//! `ProviderDispatch` — single source of truth for `attribution_span!`
//! on the [`ModelProvider`] surface.
//!
//! Every direct call to a `ModelProvider` method in the workspace goes
//! through this helper so the resulting `LogEvent` carries the inner
//! provider's alias-bound attribution without the call site naming any
//! of it. Each wrapping method opens
//! `attribution_span!(&*self.inner)` (and, for the methods that take a
//! model string, an additional `scope!(model: …)`) around the
//! underlying call.
//!
//! Adding a new `ModelProvider` method: add the wrapping method here
//! and extend the `scripts/ci/rust_quality_gate.sh` grep gate's
//! protected method list. The dispatch module is the only place in the
//! workspace that opens `attribution_span!` for a `ModelProvider`.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt as _};
use zeroclaw_api::model_provider::{
    ChatRequest, ChatResponse, ModelProvider, StreamEvent, StreamOptions, StreamResult,
};

/// Wraps a model provider so every call opens the correct
/// `attribution_span!` automatically. See the module docs for the
/// rationale and the CI gate that enforces routing through this type.
pub struct ProviderDispatch {
    inner: Arc<dyn ModelProvider>,
}

impl ProviderDispatch {
    /// Wrap an `Arc<dyn ModelProvider>` so its method calls open
    /// `attribution_span!(&*inner)` automatically.
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>) -> Self {
        Self { inner }
    }

    /// Open `attribution_span!(&*self.inner)` + `scope!(model: model)`
    /// around the inner provider's `chat` call.
    pub async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        // Enter the attribution span first so the scope! macro's
        // info_span! constructs with the attribution span as its parent.
        // Without this nesting, the attribution span and the scope
        // span would be siblings (both children of the caller's span),
        // and the layer's leaf→root walk from the scope span would
        // skip the attribution contribution entirely.
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat(request, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `stream_chat` with
    /// `attribution_span!` and a `scope!(model: …)`. Each `poll_next`
    /// of the returned stream re-enters the `model_scope` span; the
    /// attribution span is `model_scope`'s parent (set at construction
    /// time while the attribution span was entered), so the layer's
    /// leaf→root walk reaches the attribution contribution on every
    /// per-chunk `record!` from inside the provider's stream body.
    pub fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        let attribution = zeroclaw_log::attribution_span!(&*self.inner);
        // Enter the attribution span synchronously so the model_scope
        // info_span! constructs with attribution as its parent. Drop
        // the guard before returning; the attribution span lives on
        // via model_scope's parent pointer.
        let _attribution_enter = attribution.enter();
        let model_scope = zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            model = %model,
        );
        let inner_stream = self.inner.stream_chat(request, model, temperature, options);
        drop(_attribution_enter);
        // Manually re-enter `model_scope` on every poll. `tracing`
        // does not impl `Stream` for `Instrumented<S>` (only for
        // `Future`s), so we use the `poll_fn` adapter to drive the
        // inner stream while holding the scope guard for the duration
        // of each poll. The guard never crosses an await — it is
        // dropped before `poll_next` returns — so this stays `Send`.
        let mut inner_stream = inner_stream;
        stream::poll_fn(move |cx| {
            let _enter = model_scope.enter();
            inner_stream.as_mut().poll_next(cx)
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{
        ChatRequest, ChatResponse, ModelProvider, StreamChunk, StreamEvent, StreamOptions,
        StreamResult,
    };

    struct FakeAnthropic {
        alias: String,
    }

    impl Attributable for FakeAnthropic {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic))
        }
        fn alias(&self) -> &str {
            &self.alias
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for FakeAnthropic {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            zeroclaw_log::record!(
                INFO,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                "fake-anthropic chat called"
            );
            Ok(ChatResponse {
                text: Some(String::new()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn dispatch_chat_attaches_inner_provider_attribution() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let fake: Arc<dyn ModelProvider> = Arc::new(FakeAnthropic {
            alias: "test-alias".into(),
        });
        let dispatch = ProviderDispatch::new(fake);
        let request = ChatRequest {
            messages: &[],
            tools: None,
            thinking: None,
        };
        let _ = dispatch.chat(request, "claude-sonnet-4-6", None).await;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while !found && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("fake-anthropic chat called"))
                        .unwrap_or(false)
                    {
                        let zc = value.get("zeroclaw").expect("zeroclaw block present");
                        assert_eq!(
                            zc.get("model_provider").and_then(|v| v.as_str()),
                            Some("anthropic.test-alias"),
                            "expected composite model_provider; got: {zc:?}"
                        );
                        assert_eq!(
                            zc.get("model_provider_type").and_then(|v| v.as_str()),
                            Some("anthropic"),
                        );
                        assert_eq!(
                            zc.get("model_provider_alias").and_then(|v| v.as_str()),
                            Some("test-alias"),
                        );
                        assert_eq!(
                            zc.get("model").and_then(|v| v.as_str()),
                            Some("claude-sonnet-4-6"),
                        );
                        found = true;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        assert!(found, "did not capture the fake-anthropic event");
        zeroclaw_log::clear_broadcast_hook();
    }

    struct StreamingFake {
        alias: String,
    }

    impl Attributable for StreamingFake {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic))
        }
        fn alias(&self) -> &str {
            &self.alias
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for StreamingFake {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("not used in stream test")
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<'static, StreamResult<StreamEvent>> {
            // Emit a record! from inside the stream body on each poll
            // so the test can verify the span survives stream re-entry.
            // We use `stream::iter` with eagerly-evaluated items;
            // alternatively a manual stream could fire from inside
            // `poll_next`. Either way the layer's scope walk must see
            // the dispatcher-installed spans on the resulting event.
            futures_util::stream::unfold(0u8, |state| async move {
                match state {
                    0 => {
                        zeroclaw_log::record!(
                            INFO,
                            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note,),
                            "streaming-fake chunk"
                        );
                        Some((Ok(StreamEvent::TextDelta(StreamChunk::delta("hi"))), 1u8))
                    }
                    1 => Some((Ok(StreamEvent::Final), 2u8)),
                    _ => None,
                }
            })
            .boxed()
        }
    }

    #[tokio::test]
    async fn dispatch_stream_chunk_records_carry_attribution() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let fake: Arc<dyn ModelProvider> = Arc::new(StreamingFake {
            alias: "stream-alias".into(),
        });
        let dispatch = ProviderDispatch::new(fake);
        let request = ChatRequest {
            messages: &[],
            tools: None,
            thinking: None,
        };
        let mut stream =
            dispatch.stream_chat(request, "claude-sonnet-4-6", None, StreamOptions::default());
        while stream.next().await.is_some() {}

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while !found && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("streaming-fake chunk"))
                        .unwrap_or(false)
                    {
                        let zc = value.get("zeroclaw").expect("zeroclaw block present");
                        assert_eq!(
                            zc.get("model_provider_alias").and_then(|v| v.as_str()),
                            Some("stream-alias"),
                            "stream chunk record not attributed; zc: {zc:?}",
                        );
                        assert_eq!(
                            zc.get("model_provider_type").and_then(|v| v.as_str()),
                            Some("anthropic"),
                        );
                        assert_eq!(
                            zc.get("model").and_then(|v| v.as_str()),
                            Some("claude-sonnet-4-6"),
                        );
                        found = true;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        assert!(found, "stream chunk record was not attributed");
        zeroclaw_log::clear_broadcast_hook();
    }
}
