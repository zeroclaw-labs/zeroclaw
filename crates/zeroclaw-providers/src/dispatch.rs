//! `ProviderDispatch` — single source of truth for `attribution_span!`
//! on the [`ModelProvider`] surface.

use std::sync::Arc;

use futures_util::stream::{self, StreamExt as _};
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, ChatResponse, ModelInfo, ModelProvider, StreamEvent, StreamOptions,
    StreamResult,
};

/// Immutable billing identity for one rejected attempt.
///
/// This is deliberately a data projection of a provider-internal attempt. It
/// carries only the served route and token usage needed by runtime accounting.
#[derive(Debug, Clone)]
pub struct RejectedAttempt {
    provider_ref: String,
    model: String,
    usage: crate::traits::TokenUsage,
}

impl RejectedAttempt {
    pub(crate) fn new(
        provider_ref: String,
        model: String,
        usage: crate::traits::TokenUsage,
    ) -> Self {
        Self {
            provider_ref,
            model,
            usage,
        }
    }

    #[must_use]
    /// Configured provider reference used for rejected-cost attribution.
    pub fn provider_ref(&self) -> &str {
        &self.provider_ref
    }

    #[must_use]
    /// Served model used for rejected-cost attribution.
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    /// Final cumulative usage snapshot for this one physical attempt.
    pub fn usage(&self) -> &crate::traits::TokenUsage {
        &self.usage
    }

    #[must_use]
    /// Return an updated immutable projection for the same physical attempt.
    ///
    /// This replaces a cumulative snapshot; callers must not add the two
    /// snapshots together.
    pub fn with_usage(mut self, usage: crate::traits::TokenUsage) -> Self {
        self.usage = usage;
        self
    }
}

/// Immutable actual route of a transport-successful Reliable completion.
#[derive(Debug, Clone)]
pub struct AcceptedRoute {
    provider_ref: String,
    model: String,
    fallback: Option<crate::reliable::ProviderFallbackAttribution>,
}

impl AcceptedRoute {
    pub(crate) fn new(
        provider_ref: String,
        model: String,
        fallback: Option<crate::reliable::ProviderFallbackAttribution>,
    ) -> Self {
        Self {
            provider_ref,
            model,
            fallback,
        }
    }

    #[must_use]
    /// Configured provider reference that served the accepted candidate.
    pub fn provider_ref(&self) -> &str {
        &self.provider_ref
    }

    #[must_use]
    /// Served model that produced the accepted candidate.
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    /// Presentation-only fallback data, if this accepted route recovered.
    pub fn fallback(&self) -> Option<&crate::reliable::ProviderFallbackInfo> {
        self.fallback
            .as_ref()
            .map(|attribution| &attribution.fallback)
    }

    pub(crate) fn into_fallback_attribution(
        self,
    ) -> Option<crate::reliable::ProviderFallbackAttribution> {
        self.fallback
    }
}

/// Final immutable accounting projection for one dispatch-scoped call.
#[derive(Debug, Default)]
pub struct AccountedCallReport {
    rejected_attempts: Vec<RejectedAttempt>,
    accepted_route: Option<AcceptedRoute>,
    provisional_stream_attempt: Option<RejectedAttempt>,
}

impl AccountedCallReport {
    pub(crate) fn new(
        rejected_attempts: Vec<RejectedAttempt>,
        accepted_route: Option<AcceptedRoute>,
        provisional_stream_attempt: Option<RejectedAttempt>,
    ) -> Self {
        Self {
            rejected_attempts,
            accepted_route,
            provisional_stream_attempt,
        }
    }

    #[must_use]
    /// Rejected attempts, in dispatch order, for cost-only accounting.
    pub fn rejected_attempts(&self) -> &[RejectedAttempt] {
        &self.rejected_attempts
    }

    /// Accepted transport route, which remains provisional until runtime
    /// semantic acceptance commits its presentation fallback.
    #[must_use]
    pub fn accepted_route(&self) -> Option<&AcceptedRoute> {
        self.accepted_route.as_ref()
    }

    /// Consume and reset this scope's final report at the runtime semantic boundary.
    ///
    /// The contained provisional stream attempt is not rejected unless the
    /// runtime later classifies the completed response as unacceptable.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Vec<RejectedAttempt>,
        Option<RejectedAttempt>,
        Option<AcceptedRoute>,
    ) {
        (
            self.rejected_attempts,
            self.provisional_stream_attempt,
            self.accepted_route,
        )
    }

    #[must_use]
    pub(crate) fn rejected_attempt_usage(&self) -> Option<crate::traits::TokenUsage> {
        let mut total = None;
        for attempt in &self.rejected_attempts {
            crate::reliable::accumulate_usage(&mut total, Some(&attempt.usage));
        }
        total
    }
}

/// A successful response plus billed usage from Reliable attempts that were
/// rejected before that response was accepted.
///
/// `response.usage` always describes the accepted attempt. The sidecar is for
/// cost accounting only; it must not be used as context-window usage or
/// successful-response telemetry.
pub struct AccountedChatResponse {
    pub response: ChatResponse,
    /// Aggregate compatibility projection. Runtime accounting uses the opaque
    /// [`AccountedChatScope`] for exact per-attempt routes.
    pub rejected_attempt_usage: Option<crate::traits::TokenUsage>,
}

#[cfg(test)]
pub(crate) struct AccountedChatOutcome {
    pub(crate) result: anyhow::Result<AccountedChatResponse>,
    pub(crate) accounting: AccountedCallReport,
}

/// Opaque, task-local accounting lifetime for the providers/runtime dispatch seam.
///
/// It intentionally owns no route history. Callers run one provider operation
/// inside it and consume its one final accounting projection, including when
/// cancellation drops that operation before it returns a result.
#[doc(hidden)]
pub struct AccountedChatScope {
    inner: crate::reliable::ReliableCallAccountingScope,
}

impl AccountedChatScope {
    /// Start the only lifecycle scope allowed by the providers/runtime
    /// accounting contract. `Default` is intentionally not implemented: it
    /// would add an unaudited second public construction capability.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: crate::reliable::ReliableCallAccountingScope::default(),
        }
    }

    /// Run one provider future inside this scope's task-local collector.
    ///
    /// The scope owns reports independently of whether `future` completes,
    /// times out, or is cancelled; call [`Self::take`] afterwards exactly once.
    pub async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        self.inner.scope(future).await
    }

    #[must_use]
    /// Consume the current report and reset this scope for no further reports.
    pub fn take(&self) -> AccountedCallReport {
        self.inner.take()
    }

    /// Finalize the currently selected stream attempt as rejected.
    ///
    /// Returns `false` when no provisional attempt belongs to this task-local
    /// scope; callers must then account only their own direct-provider usage.
    pub fn record_rejected_stream_usage(&self, usage: crate::traits::TokenUsage) -> bool {
        crate::reliable::record_rejected_stream_usage(usage)
    }

    /// Preserve a semantic-empty stream cause across the exact-entry recovery walk.
    pub fn mark_stream_recovery_semantic_empty(&self) {
        crate::reliable::mark_stream_recovery_semantic_empty();
    }

    /// Clear a provisional route before an in-scope recovery replaces it.
    ///
    /// This has no presentation effect until [`commit_accepted_provider_route`]
    /// runs after runtime semantic acceptance.
    pub fn clear_provisional_provider_route(&self) {
        crate::reliable::clear_provisional_provider_route()
    }
}

/// Commit a semantically accepted route for the existing fallback presenter.
///
/// Passing `None` clears a prior candidate. Call this only after the runtime
/// accepts the response; it mutates task-local presentation state, not billing.
pub fn commit_accepted_provider_route(route: Option<AcceptedRoute>) {
    crate::reliable::commit_accepted_provider_route(
        route.and_then(AcceptedRoute::into_fallback_attribution),
    )
}

/// Wraps a model provider so every call opens the correct
/// `attribution_span!` automatically. See the module docs for the
/// rationale and the CI gate that enforces routing through this type.
pub struct ProviderDispatch {
    inner: Arc<dyn ModelProvider>,
}

pub struct ProviderDispatchRef<'a> {
    inner: &'a dyn ModelProvider,
}

impl ProviderDispatch {
    /// Wrap an `Arc<dyn ModelProvider>` so its method calls open
    /// `attribution_span!(&*inner)` automatically.
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>) -> Self {
        Self { inner }
    }

    /// Wrap a borrowed `&dyn ModelProvider`. Returns a
    /// [`ProviderDispatchRef`] for ergonomic chaining at call sites
    /// that don't hold an `Arc`.
    #[must_use]
    pub fn from_ref(inner: &dyn ModelProvider) -> ProviderDispatchRef<'_> {
        ProviderDispatchRef { inner }
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

    /// Like [`Self::chat`], while retaining rejected Reliable-attempt usage in
    /// a separate accounting sidecar.
    pub async fn chat_accounted(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<AccountedChatResponse> {
        let scope = AccountedChatScope::new();
        let result = scope.scope(self.chat(request, model, temperature)).await;
        let accounting = scope.take();
        let rejected_attempt_usage = accounting.rejected_attempt_usage();
        result.map(|response| AccountedChatResponse {
            response,
            rejected_attempt_usage,
        })
    }

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
        let mut inner_stream = inner_stream;
        stream::poll_fn(move |cx| {
            let _enter = model_scope.enter();
            inner_stream.as_mut().poll_next(cx)
        })
        .boxed()
    }

    pub async fn simple_chat(
        &self,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => (*self.inner).simple_chat(message, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `chat_with_system`.
    pub async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_system(system_prompt, message, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `chat_with_history`.
    pub async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_history(messages, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `chat_with_tools`.
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_tools(messages, tools, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Like [`Self::chat_with_tools`], while retaining rejected
    /// Reliable-attempt usage in a separate accounting sidecar.
    pub async fn chat_with_tools_accounted(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<AccountedChatResponse> {
        let scope = AccountedChatScope::new();
        let result = scope
            .scope(self.chat_with_tools(messages, tools, model, temperature))
            .await;
        let accounting = scope.take();
        let rejected_attempt_usage = accounting.rejected_attempt_usage();
        result.map(|response| AccountedChatResponse {
            response,
            rejected_attempt_usage,
        })
    }

    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        (*self.inner).list_models().instrument(span).await
    }

    /// Wrap the inner provider's `list_models_with_pricing`. Same
    /// `&*self.inner` rationale as `list_models`.
    pub async fn list_models_with_pricing(&self) -> anyhow::Result<Vec<ModelInfo>> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        (*self.inner)
            .list_models_with_pricing()
            .instrument(span)
            .await
    }

    /// Wrap the inner provider's `warmup`. No `model` parameter, so
    /// attribution only.
    pub async fn warmup(&self) -> anyhow::Result<()> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(&*self.inner);
        self.inner.warmup().instrument(span).await
    }
}

impl<'a> ProviderDispatchRef<'a> {
    /// Open `attribution_span!(self.inner)` + `scope!(model: model)`
    /// around the inner provider's `chat` call.
    pub async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
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

    /// Like [`Self::chat`], while retaining rejected Reliable-attempt usage in
    /// a separate accounting sidecar.
    pub async fn chat_accounted(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<AccountedChatResponse> {
        let scope = AccountedChatScope::new();
        let result = scope.scope(self.chat(request, model, temperature)).await;
        let accounting = scope.take();
        let rejected_attempt_usage = accounting.rejected_attempt_usage();
        result.map(|response| AccountedChatResponse {
            response,
            rejected_attempt_usage,
        })
    }

    #[cfg(test)]
    pub(crate) async fn chat_accounted_outcome(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> AccountedChatOutcome {
        let scope = AccountedChatScope::new();
        let result = scope.scope(self.chat(request, model, temperature)).await;
        let accounting = scope.take();
        let rejected_attempt_usage = accounting.rejected_attempt_usage();
        AccountedChatOutcome {
            result: result.map(|response| AccountedChatResponse {
                response,
                rejected_attempt_usage,
            }),
            accounting,
        }
    }

    pub fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        let attribution = zeroclaw_log::attribution_span!(self.inner);
        let _attribution_enter = attribution.enter();
        let model_scope = zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            model = %model,
        );
        let inner_stream = self.inner.stream_chat(request, model, temperature, options);
        drop(_attribution_enter);
        let mut inner_stream = inner_stream;
        stream::poll_fn(move |cx| {
            let _enter = model_scope.enter();
            inner_stream.as_mut().poll_next(cx)
        })
        .boxed()
    }

    /// Wrap the inner provider's `simple_chat`. Dispatched through
    /// `self.inner` so a concrete `simple_chat` override on the inner
    /// provider is honored (rather than the trait default that
    /// delegates to `chat_with_system`).
    pub async fn simple_chat(
        &self,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.simple_chat(message, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `chat_with_system`.
    pub async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_system(system_prompt, message, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `chat_with_history`.
    pub async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_history(messages, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Wrap the inner provider's `chat_with_tools`.
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_tools(messages, tools, model, temperature)
            )
            .await
        }
        .instrument(span)
        .await
    }

    /// Like [`Self::chat_with_tools`], while retaining rejected
    /// Reliable-attempt usage in a separate accounting sidecar.
    pub async fn chat_with_tools_accounted(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<AccountedChatResponse> {
        let scope = AccountedChatScope::new();
        let result = scope
            .scope(self.chat_with_tools(messages, tools, model, temperature))
            .await;
        let accounting = scope.take();
        let rejected_attempt_usage = accounting.rejected_attempt_usage();
        result.map(|response| AccountedChatResponse {
            response,
            rejected_attempt_usage,
        })
    }

    /// Wrap the inner provider's `list_models`. No `model` parameter,
    /// so attribution only.
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        self.inner.list_models().instrument(span).await
    }

    /// Wrap the inner provider's `list_models_with_pricing`.
    pub async fn list_models_with_pricing(&self) -> anyhow::Result<Vec<ModelInfo>> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        self.inner.list_models_with_pricing().instrument(span).await
    }

    /// Wrap the inner provider's `warmup`. No `model` parameter, so
    /// attribution only.
    pub async fn warmup(&self) -> anyhow::Result<()> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self.inner);
        self.inner.warmup().instrument(span).await
    }

    #[must_use]
    pub fn inner(&self) -> &'a dyn ModelProvider {
        self.inner
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

    #[tokio::test]
    async fn dispatch_ref_chat_attaches_inner_provider_attribution() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        // Hold the fake by ownership but pass &dyn to the borrowed
        // dispatcher — exercises the call shape that the runtime's
        // turn helpers use.
        let fake = FakeAnthropic {
            alias: "ref-alias".into(),
        };
        let dispatch = ProviderDispatch::from_ref(&fake as &dyn ModelProvider);
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
                            zc.get("model_provider_alias").and_then(|v| v.as_str()),
                            Some("ref-alias"),
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
        assert!(
            found,
            "did not capture the fake-anthropic event via borrowed dispatcher",
        );
        zeroclaw_log::clear_broadcast_hook();
    }
}
