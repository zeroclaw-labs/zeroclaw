//! `ProviderDispatch` — single source of truth for `attribution_span!`
//! on the [`ModelProvider`] surface.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use futures_util::stream::{self, StreamExt as _};
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, ChatResponse, ModelInfo, ModelProvider, StreamEvent, StreamOptions,
    StreamResult,
};

mod accounting;

/// Why a provider supplied usage observation cannot be billed as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidUsageReason {
    MissingInput,
    MissingOutput,
    CachedInputExceedsInput,
    TotalOverflow,
}

/// Completeness of one physical provider attempt's usage observation.
#[derive(Debug, Clone)]
pub enum AttemptUsageOutcome {
    Complete(crate::traits::TokenUsage),
    Missing,
    Invalid {
        observed: crate::traits::TokenUsage,
        reason: InvalidUsageReason,
    },
    OutcomeUnknown {
        observed: Option<crate::traits::TokenUsage>,
    },
}

/// Derived completeness of all physical leaves in one call report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallAccountingState {
    Complete,
    Missing,
    Invalid,
    OutcomeUnknown,
}

impl AttemptUsageOutcome {
    fn observed_usage(&self) -> Option<crate::traits::TokenUsage> {
        match self {
            Self::Complete(usage) => Some(usage.clone()),
            Self::OutcomeUnknown { observed } => observed.clone(),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }
}

/// One actual physical provider leaf, in first-poll order.
#[derive(Debug, Clone)]
pub struct AccountedAttempt {
    provider_ref: String,
    model: String,
    outcome: AttemptUsageOutcome,
}

impl AccountedAttempt {
    pub(crate) fn new(provider_ref: String, model: String, outcome: AttemptUsageOutcome) -> Self {
        Self {
            provider_ref,
            model,
            outcome,
        }
    }

    #[must_use]
    pub fn provider_ref(&self) -> &str {
        &self.provider_ref
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn outcome(&self) -> &AttemptUsageOutcome {
        &self.outcome
    }
}

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

    pub(crate) fn with_identity(mut self, provider_ref: String, model: String) -> Self {
        self.provider_ref = provider_ref;
        self.model = model;
        self
    }
}

/// Final immutable accounting projection for one dispatch-scoped call.
#[derive(Debug, Default)]
pub struct AccountedCallReport {
    attempts: Vec<AccountedAttempt>,
    rejected_attempts: Vec<RejectedAttempt>,
    accepted_route: Option<AcceptedRoute>,
}

impl AccountedCallReport {
    pub(crate) fn new(accepted_route: Option<AcceptedRoute>) -> Self {
        Self {
            attempts: Vec::new(),
            rejected_attempts: Vec::new(),
            accepted_route,
        }
    }

    pub(crate) fn with_attempts(
        mut self,
        attempts: Vec<AccountedAttempt>,
        successful_route: Option<(String, String)>,
    ) -> Self {
        self.accepted_route = successful_route.map(|(provider_ref, model)| {
            if let Some(route) = self.accepted_route.take() {
                route.with_identity(provider_ref, model)
            } else {
                AcceptedRoute::new(provider_ref, model, None)
            }
        });
        // This is a compatibility view retained for the landed accounting seam.
        // The ordered physical-attempt report above is canonical; on a
        // transport-successful call its final leaf is the accepted response,
        // while terminal failures have no accepted route and expose every
        // completed billable leaf here.
        let rejected_len = attempts
            .len()
            .saturating_sub(usize::from(self.accepted_route.is_some()));
        self.rejected_attempts = attempts[..rejected_len]
            .iter()
            .filter_map(|attempt| match attempt.outcome() {
                AttemptUsageOutcome::Complete(usage)
                | AttemptUsageOutcome::OutcomeUnknown {
                    observed: Some(usage),
                } => Some(RejectedAttempt::new(
                    attempt.provider_ref().to_string(),
                    attempt.model().to_string(),
                    usage.clone(),
                )),
                _ => None,
            })
            .collect();
        self.attempts = attempts;
        self
    }

    /// Actual physical provider leaves, in first-poll order.
    #[must_use]
    pub fn attempts(&self) -> &[AccountedAttempt] {
        &self.attempts
    }

    /// Conservative aggregate completeness derived from physical leaves.
    #[must_use]
    pub fn accounting_state(&self) -> CallAccountingState {
        self.attempts
            .iter()
            .fold(CallAccountingState::Complete, |state, attempt| {
                let next = match attempt.outcome() {
                    AttemptUsageOutcome::OutcomeUnknown { .. } => {
                        CallAccountingState::OutcomeUnknown
                    }
                    AttemptUsageOutcome::Invalid { .. } => CallAccountingState::Invalid,
                    AttemptUsageOutcome::Missing => CallAccountingState::Missing,
                    AttemptUsageOutcome::Complete(_) => CallAccountingState::Complete,
                };
                match (state, next) {
                    (CallAccountingState::OutcomeUnknown, _)
                    | (_, CallAccountingState::OutcomeUnknown) => {
                        CallAccountingState::OutcomeUnknown
                    }
                    (CallAccountingState::Invalid, _) | (_, CallAccountingState::Invalid) => {
                        CallAccountingState::Invalid
                    }
                    (CallAccountingState::Missing, _) | (_, CallAccountingState::Missing) => {
                        CallAccountingState::Missing
                    }
                    _ => CallAccountingState::Complete,
                }
            })
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

    /// Consume this scope's compatibility projection at the runtime semantic boundary.
    #[must_use]
    pub fn into_parts(self) -> (Vec<RejectedAttempt>, Option<AcceptedRoute>) {
        (self.rejected_attempts, self.accepted_route)
    }

    #[must_use]
    pub fn into_attempts_and_parts(
        self,
    ) -> (
        Vec<AccountedAttempt>,
        Vec<RejectedAttempt>,
        Option<AcceptedRoute>,
    ) {
        (self.attempts, self.rejected_attempts, self.accepted_route)
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
    collector: accounting::CallAccountingCollector,
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
            collector: accounting::CallAccountingCollector::default(),
            inner: crate::reliable::ReliableCallAccountingScope::default(),
        }
    }

    /// Run one provider future inside this scope's task-local collector.
    ///
    /// The scope owns reports independently of whether `future` completes,
    /// times out, or is cancelled; call [`Self::take`] afterwards exactly once.
    pub async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        self.collector.scope(self.inner.scope(future)).await
    }

    /// Mark the completed logical operation as accepted after its caller has
    /// applied semantic validation. Physical leaf finalization remains owned
    /// by dispatch adapters.
    pub fn mark_logical_success(&self) {
        self.collector.mark_logical_success();
    }

    #[must_use]
    /// Consume the current report and reset this scope for no further reports.
    pub fn take(&self) -> AccountedCallReport {
        let (attempts, successful_route) = self.collector.close();
        self.inner.take().with_attempts(attempts, successful_route)
    }

    /// Preserve a semantic-empty stream cause across the exact-entry recovery walk.
    pub fn mark_stream_recovery_semantic_empty(&self) {
        crate::reliable::mark_stream_recovery_semantic_empty();
    }

    /// Preserve a stream failure's safe classification if recovery exhausts
    /// later candidates without replaying the failed stream entry.
    pub fn record_stream_recovery_failure(&self, error: &anyhow::Error) {
        crate::reliable::record_stream_recovery_failure(error);
    }

    /// Clear a provisional route before an in-scope recovery replaces it.
    ///
    /// This has no presentation effect until [`commit_accepted_provider_route`]
    /// runs after runtime semantic acceptance.
    pub fn clear_provisional_provider_route(&self) {
        crate::reliable::clear_provisional_provider_route()
    }

    /// Preserve a stream consumer's observed lower-bound usage on its already
    /// started physical leaf. Never creates an attempt.
    pub fn record_stream_interruption_usage(&self, usage: crate::traits::TokenUsage) {
        accounting::record_stream_interruption_usage(usage);
    }

    /// Record usage for a terminal stream response rejected by runtime semantic
    /// validation. It preserves the physical leaf and changes only its usage
    /// completeness to an interrupted lower bound.
    pub fn record_stream_semantic_rejection_usage(&self, usage: crate::traits::TokenUsage) {
        accounting::record_stream_semantic_rejection_usage(usage);
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

/// Mark the currently executing dispatch provider as a routing/decorator
/// composite.  Provider implementations call this immediately before they
/// dispatch an inner provider.
pub(crate) fn mark_current_dispatch_composite() {
    accounting::mark_current_composite();
}

/// Mark a decorator/composite only when its returned stream is actually
/// polled. Construction is not a physical provider attempt.
pub(crate) fn stream_as_dispatch_composite<T>(
    stream: stream::BoxStream<'static, T>,
) -> stream::BoxStream<'static, T>
where
    T: Send + 'static,
{
    let mut stream = stream;
    let mut marked = false;
    stream::poll_fn(move |cx| {
        if !marked {
            marked = true;
            mark_current_dispatch_composite();
        }
        stream.as_mut().poll_next(cx)
    })
    .boxed()
}

pub(crate) fn current_dispatch_billable_usage() -> Option<crate::traits::TokenUsage> {
    accounting::current_billable_usage()
}

/// Apply the configured physical route selected by a composite to its next
/// dispatched child.  The override exists only during polling and is never
/// persisted as wrapper state.
#[doc(hidden)]
pub fn with_exact_dispatch_route<F>(
    provider_ref: String,
    model: String,
    future: F,
) -> impl Future<Output = F::Output>
where
    F: Future,
{
    accounting::exact_route_future(provider_ref, model, future)
}

pub(crate) fn stream_with_exact_dispatch_route<T>(
    provider_ref: String,
    model: String,
    stream: stream::BoxStream<'static, T>,
) -> stream::BoxStream<'static, T>
where
    T: Send + 'static,
{
    accounting::exact_route_stream(provider_ref, model, stream)
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

fn provider_reference(provider: &dyn ModelProvider) -> String {
    zeroclaw_api::attribution::Attributable::alias(provider).to_string()
}

fn accounted_chat_call<'a, F>(
    provider: &'a dyn ModelProvider,
    model: &'a str,
    future: F,
) -> impl Future<Output = anyhow::Result<ChatResponse>> + 'a
where
    F: Future<Output = anyhow::Result<ChatResponse>> + 'a,
{
    let provider_ref = provider_reference(provider);
    let model = model.to_string();
    let mut future = Box::pin(future);
    let mut attempt = accounting::AttemptState::unstarted(provider_ref, model);
    futures_util::future::poll_fn(move |cx| {
        attempt.start();
        let mut poll = || Pin::as_mut(&mut future).poll(cx);
        let result = match attempt.lease() {
            Some(lease) => lease.poll_scope(poll),
            None => poll(),
        };
        if let Poll::Ready(result) = &result
            && let Some(lease) = attempt.lease()
        {
            match result {
                Ok(response) => lease.finish_response(response),
                Err(error) => {
                    lease.finish_error_with_usage(crate::reliable::terminal_error_usage(error))
                }
            }
        }
        result
    })
}

fn accounted_string_call<'a, F>(
    provider: &'a dyn ModelProvider,
    model: &'a str,
    future: F,
) -> impl Future<Output = anyhow::Result<String>> + 'a
where
    F: Future<Output = anyhow::Result<String>> + 'a,
{
    let provider_ref = provider_reference(provider);
    let model = model.to_string();
    let mut future = Box::pin(future);
    let mut attempt = accounting::AttemptState::unstarted(provider_ref, model);
    futures_util::future::poll_fn(move |cx| {
        attempt.start();
        let mut poll = || Pin::as_mut(&mut future).poll(cx);
        let result = match attempt.lease() {
            Some(lease) => lease.poll_scope(poll),
            None => poll(),
        };
        if let Poll::Ready(result) = &result
            && let Some(lease) = attempt.lease()
        {
            match result {
                Ok(_) => lease.finish_missing_response(),
                Err(error) => {
                    lease.finish_error_with_usage(crate::reliable::terminal_error_usage(error))
                }
            }
        }
        result
    })
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
        accounted_chat_call(&*self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat(request, model, temperature)
            )
            .await
        })
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
        if result.is_ok() {
            scope.mark_logical_success();
        }
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
        let provider_ref = provider_reference(&*self.inner);
        let model_name = model.to_string();
        let inner_stream = self.inner.stream_chat(request, model, temperature, options);
        drop(_attribution_enter);
        let mut inner_stream = inner_stream;
        let mut attempt = accounting::AttemptState::unstarted(provider_ref, model_name);
        stream::poll_fn(move |cx| {
            attempt.start();
            let _enter = model_scope.enter();
            let mut poll = || inner_stream.as_mut().poll_next(cx);
            let result = match attempt.lease() {
                Some(lease) => lease.poll_scope(poll),
                None => poll(),
            };
            if let Some(lease) = attempt.lease() {
                match &result {
                    Poll::Ready(Some(Ok(StreamEvent::Usage(usage)))) => {
                        lease.observe_stream_usage(usage.clone());
                    }
                    Poll::Ready(Some(Ok(StreamEvent::Final))) => lease.finish_stream(),
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => lease.set_unknown(),
                    Poll::Pending | Poll::Ready(Some(Ok(_))) => {}
                }
            }
            result
        })
        .boxed()
    }

    /// Dispatch a legacy chunk stream through the same first-poll accounting
    /// boundary as structured streams.
    pub fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<crate::traits::StreamChunk>> {
        let provider_ref = provider_reference(&*self.inner);
        let model_name = model.to_string();
        let mut inner_stream =
            self.inner
                .stream_chat_with_system(system_prompt, message, model, temperature, options);
        let mut attempt = accounting::AttemptState::unstarted(provider_ref, model_name);
        stream::poll_fn(move |cx| {
            attempt.start();
            let mut poll = || inner_stream.as_mut().poll_next(cx);
            let result = match attempt.lease() {
                Some(lease) => lease.poll_scope(poll),
                None => poll(),
            };
            if let Some(lease) = attempt.lease() {
                match &result {
                    Poll::Ready(Some(Ok(chunk))) if chunk.is_final => lease.finish_stream(),
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => lease.set_unknown(),
                    Poll::Pending | Poll::Ready(Some(Ok(_))) => {}
                }
            }
            result
        })
        .boxed()
    }

    pub fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<crate::traits::StreamChunk>> {
        let provider_ref = provider_reference(&*self.inner);
        let model_name = model.to_string();
        let mut inner_stream =
            self.inner
                .stream_chat_with_history(messages, model, temperature, options);
        let mut attempt = accounting::AttemptState::unstarted(provider_ref, model_name);
        stream::poll_fn(move |cx| {
            attempt.start();
            let mut poll = || inner_stream.as_mut().poll_next(cx);
            let result = match attempt.lease() {
                Some(lease) => lease.poll_scope(poll),
                None => poll(),
            };
            if let Some(lease) = attempt.lease() {
                match &result {
                    Poll::Ready(Some(Ok(chunk))) if chunk.is_final => lease.finish_stream(),
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => lease.set_unknown(),
                    Poll::Pending | Poll::Ready(Some(Ok(_))) => {}
                }
            }
            result
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
        accounted_string_call(&*self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => (*self.inner).simple_chat(message, model, temperature)
            )
            .await
        })
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
        accounted_string_call(&*self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_system(system_prompt, message, model, temperature)
            )
            .await
        })
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
        accounted_string_call(&*self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_history(messages, model, temperature)
            )
            .await
        })
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
        accounted_chat_call(&*self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_tools(messages, tools, model, temperature)
            )
            .await
        })
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
        if result.is_ok() {
            scope.mark_logical_success();
        }
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
        accounted_chat_call(self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat(request, model, temperature)
            )
            .await
        })
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
        if result.is_ok() {
            scope.mark_logical_success();
        }
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
        if result.is_ok() {
            scope.mark_logical_success();
        }
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
        let provider_ref = provider_reference(self.inner);
        let model_name = model.to_string();
        let inner_stream = self.inner.stream_chat(request, model, temperature, options);
        drop(_attribution_enter);
        let mut inner_stream = inner_stream;
        let mut attempt = accounting::AttemptState::unstarted(provider_ref, model_name);
        stream::poll_fn(move |cx| {
            attempt.start();
            let _enter = model_scope.enter();
            let mut poll = || inner_stream.as_mut().poll_next(cx);
            let result = match attempt.lease() {
                Some(lease) => lease.poll_scope(poll),
                None => poll(),
            };
            if let Some(lease) = attempt.lease() {
                match &result {
                    Poll::Ready(Some(Ok(StreamEvent::Usage(usage)))) => {
                        lease.observe_stream_usage(usage.clone());
                    }
                    Poll::Ready(Some(Ok(StreamEvent::Final))) => lease.finish_stream(),
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => lease.set_unknown(),
                    Poll::Pending | Poll::Ready(Some(Ok(_))) => {}
                }
            }
            result
        })
        .boxed()
    }

    pub fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<crate::traits::StreamChunk>> {
        let provider_ref = provider_reference(self.inner);
        let model_name = model.to_string();
        let mut inner_stream =
            self.inner
                .stream_chat_with_system(system_prompt, message, model, temperature, options);
        let mut attempt = accounting::AttemptState::unstarted(provider_ref, model_name);
        stream::poll_fn(move |cx| {
            attempt.start();
            let mut poll = || inner_stream.as_mut().poll_next(cx);
            let result = match attempt.lease() {
                Some(lease) => lease.poll_scope(poll),
                None => poll(),
            };
            if let Some(lease) = attempt.lease() {
                match &result {
                    Poll::Ready(Some(Ok(chunk))) if chunk.is_final => lease.finish_stream(),
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => lease.set_unknown(),
                    Poll::Pending | Poll::Ready(Some(Ok(_))) => {}
                }
            }
            result
        })
        .boxed()
    }

    pub fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<crate::traits::StreamChunk>> {
        let provider_ref = provider_reference(self.inner);
        let model_name = model.to_string();
        let mut inner_stream =
            self.inner
                .stream_chat_with_history(messages, model, temperature, options);
        let mut attempt = accounting::AttemptState::unstarted(provider_ref, model_name);
        stream::poll_fn(move |cx| {
            attempt.start();
            let mut poll = || inner_stream.as_mut().poll_next(cx);
            let result = match attempt.lease() {
                Some(lease) => lease.poll_scope(poll),
                None => poll(),
            };
            if let Some(lease) = attempt.lease() {
                match &result {
                    Poll::Ready(Some(Ok(chunk))) if chunk.is_final => lease.finish_stream(),
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => lease.set_unknown(),
                    Poll::Pending | Poll::Ready(Some(Ok(_))) => {}
                }
            }
            result
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
        accounted_string_call(self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.simple_chat(message, model, temperature)
            )
            .await
        })
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
        accounted_string_call(self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_system(system_prompt, message, model, temperature)
            )
            .await
        })
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
        accounted_string_call(self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_history(messages, model, temperature)
            )
            .await
        })
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
        accounted_chat_call(self.inner, model, async move {
            zeroclaw_log::scope!(
                model: model,
                => self.inner.chat_with_tools(messages, tools, model, temperature)
            )
            .await
        })
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
        if result.is_ok() {
            scope.mark_logical_success();
        }
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

    struct UsageFake {
        usage: Option<crate::traits::TokenUsage>,
    }

    impl Attributable for UsageFake {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "configured.usage"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for UsageFake {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: self.usage.clone(),
                reasoning_content: None,
            })
        }
    }

    struct FailingOrPendingFake {
        pending: bool,
    }

    impl Attributable for FailingOrPendingFake {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "configured.failing"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for FailingOrPendingFake {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if self.pending {
                futures_util::future::pending::<()>().await;
                unreachable!("the pending provider future cannot complete")
            }
            anyhow::bail!("expected direct failure")
        }
    }

    #[tokio::test]
    async fn direct_error_and_after_poll_future_drop_are_unknown_attempts() {
        let messages = vec![ChatMessage::user("hello")];
        for pending in [false, true] {
            let provider = FailingOrPendingFake { pending };
            let scope = AccountedChatScope::new();
            scope
                .scope(async {
                    let dispatch = ProviderDispatch::from_ref(&provider);
                    let call = dispatch.chat(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        "served-model",
                        None,
                    );
                    if pending {
                        assert!(
                            tokio::time::timeout(std::time::Duration::from_millis(1), call)
                                .await
                                .is_err()
                        );
                    } else {
                        assert!(call.await.is_err());
                    }
                })
                .await;
            let report = scope.take();
            assert_eq!(report.attempts().len(), 1);
            assert!(matches!(
                report.attempts()[0].outcome(),
                AttemptUsageOutcome::OutcomeUnknown { observed: None }
            ));
        }
    }

    #[tokio::test]
    async fn legacy_system_chat_is_a_missing_usage_physical_attempt() {
        let provider = UsageFake { usage: None };
        let scope = AccountedChatScope::new();
        let result = scope
            .scope(async {
                ProviderDispatch::from_ref(&provider)
                    .chat_with_system(None, "hello", "served-model", None)
                    .await
            })
            .await;
        assert_eq!(result.expect("legacy call succeeds"), "ok");

        let report = scope.take();
        assert_eq!(report.attempts().len(), 1);
        assert_eq!(report.attempts()[0].provider_ref(), "configured.usage");
        assert_eq!(report.attempts()[0].model(), "served-model");
        assert!(matches!(
            report.attempts()[0].outcome(),
            AttemptUsageOutcome::Missing
        ));
    }

    #[tokio::test]
    async fn direct_dispatch_classifies_complete_zero_invalid_and_missing_usage() {
        let cases = [
            (
                Some(crate::traits::TokenUsage {
                    input_tokens: Some(3),
                    output_tokens: Some(2),
                    cached_input_tokens: None,
                }),
                "complete",
            ),
            (
                Some(crate::traits::TokenUsage {
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cached_input_tokens: Some(0),
                }),
                "zero",
            ),
            (
                Some(crate::traits::TokenUsage {
                    input_tokens: None,
                    output_tokens: Some(2),
                    cached_input_tokens: None,
                }),
                "invalid",
            ),
            (None, "missing"),
        ];
        let messages = vec![ChatMessage::user("hello")];
        for (usage, expected) in cases {
            let provider = UsageFake { usage };
            let outcome = ProviderDispatch::from_ref(&provider)
                .chat_accounted_outcome(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "model",
                    None,
                )
                .await;
            assert!(outcome.result.is_ok());
            assert_eq!(outcome.accounting.attempts().len(), 1);
            let outcome = outcome.accounting.attempts()[0].outcome();
            match expected {
                "complete" | "zero" => assert!(matches!(outcome, AttemptUsageOutcome::Complete(_))),
                "invalid" => assert!(matches!(outcome, AttemptUsageOutcome::Invalid { .. })),
                "missing" => assert!(matches!(outcome, AttemptUsageOutcome::Missing)),
                _ => unreachable!(),
            }
        }
    }

    #[tokio::test]
    async fn accounted_dispatch_creates_one_missing_leaf_on_first_poll() {
        let fake: Arc<dyn ModelProvider> = Arc::new(FakeAnthropic {
            alias: "configured.fake".to_string(),
        });
        let dispatch = ProviderDispatch::new(fake);
        let scope = AccountedChatScope::new();
        let request = ChatRequest {
            messages: &[ChatMessage::user("hello")],
            tools: None,
            thinking: None,
        };

        let result = scope.scope(dispatch.chat(request, "model", None)).await;
        assert!(result.is_ok());
        let report = scope.take();
        assert_eq!(report.attempts().len(), 1);
        let attempt = &report.attempts()[0];
        assert_eq!(attempt.provider_ref(), "configured.fake");
        assert_eq!(attempt.model(), "model");
        assert!(matches!(attempt.outcome(), AttemptUsageOutcome::Missing));
    }

    #[tokio::test]
    async fn unpolled_direct_stream_creates_no_attempt() {
        let fake: Arc<dyn ModelProvider> = Arc::new(FakeAnthropic {
            alias: "configured.fake".to_string(),
        });
        let dispatch = ProviderDispatch::new(fake);
        let scope = AccountedChatScope::new();
        let messages = vec![ChatMessage::user("hello")];

        scope
            .scope(async {
                let stream = dispatch.stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "model",
                    None,
                    StreamOptions::new(true),
                );
                drop(stream);
            })
            .await;

        assert!(scope.take().attempts().is_empty());
    }

    #[tokio::test]
    async fn pre_poll_cancellation_creates_no_direct_attempt() {
        let fake: Arc<dyn ModelProvider> = Arc::new(FakeAnthropic {
            alias: "configured.fake".to_string(),
        });
        let dispatch = ProviderDispatch::new(fake);
        let scope = AccountedChatScope::new();
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let messages = vec![ChatMessage::user("hello")];

        scope
            .scope(async {
                let call = dispatch.chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "model",
                    None,
                );
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {}
                    _ = call => panic!("the cancelled branch must win before the provider is polled"),
                }
            })
            .await;

        assert!(scope.take().attempts().is_empty());
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

    #[derive(Clone, Copy)]
    enum AccountingStreamMode {
        Final,
        Error,
        Eof,
        Pending,
    }

    struct AccountingStreamFake {
        mode: AccountingStreamMode,
    }

    impl Attributable for AccountingStreamFake {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "configured.stream"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for AccountingStreamFake {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> futures_util::stream::BoxStream<'static, StreamResult<StreamEvent>> {
            match self.mode {
                AccountingStreamMode::Final => stream::iter(vec![Ok(StreamEvent::Final)]).boxed(),
                AccountingStreamMode::Error => {
                    stream::iter(vec![Err(crate::traits::StreamError::ModelProvider(
                        "expected stream failure".to_string(),
                    ))])
                    .boxed()
                }
                AccountingStreamMode::Eof => stream::empty().boxed(),
                AccountingStreamMode::Pending => stream::pending().boxed(),
            }
        }
    }

    #[tokio::test]
    async fn direct_stream_final_error_and_eof_have_distinct_closed_outcomes() {
        let messages = vec![ChatMessage::user("hello")];
        for (mode, expected_final) in [
            (AccountingStreamMode::Final, true),
            (AccountingStreamMode::Error, false),
            (AccountingStreamMode::Eof, false),
        ] {
            let scope = AccountedChatScope::new();
            let provider = AccountingStreamFake { mode };
            scope
                .scope(async {
                    let mut stream = ProviderDispatch::from_ref(&provider).stream_chat(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        "served-model",
                        None,
                        StreamOptions::new(true),
                    );
                    while stream.next().await.is_some() {}
                })
                .await;

            let report = scope.take();
            assert_eq!(report.attempts().len(), 1);
            let leaf = &report.attempts()[0];
            assert_eq!(
                (leaf.provider_ref(), leaf.model()),
                ("configured.stream", "served-model")
            );
            if expected_final {
                assert!(matches!(leaf.outcome(), AttemptUsageOutcome::Missing));
                assert!(report.accepted_route().is_some());
            } else {
                assert!(matches!(
                    leaf.outcome(),
                    AttemptUsageOutcome::OutcomeUnknown { observed: None }
                ));
                assert!(report.accepted_route().is_none());
            }
        }
    }

    #[tokio::test]
    async fn after_first_poll_stream_drop_is_reported_as_unknown() {
        let scope = AccountedChatScope::new();
        let provider = AccountingStreamFake {
            mode: AccountingStreamMode::Pending,
        };
        let messages = vec![ChatMessage::user("hello")];
        scope
            .scope(async {
                let mut stream = ProviderDispatch::from_ref(&provider).stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "served-model",
                    None,
                    StreamOptions::new(true),
                );
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(1), stream.next())
                        .await
                        .is_err()
                );
                drop(stream);
            })
            .await;

        let report = scope.take();
        assert_eq!(report.attempts().len(), 1);
        assert!(matches!(
            report.attempts()[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
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
