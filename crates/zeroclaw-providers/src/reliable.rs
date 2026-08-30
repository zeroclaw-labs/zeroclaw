use super::ModelProvider;
use super::dispatch::{
    AcceptedRoute, AccountedCallReport, ProviderDispatch, current_dispatch_billable_usage,
    mark_current_dispatch_composite, stream_as_dispatch_composite,
    stream_with_exact_dispatch_route, with_exact_dispatch_route,
};
use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamEvent, StreamOptions, StreamResult,
    TokenUsage,
};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use parking_lot::Mutex as ParkingMutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Info about a model_provider fallback that occurred during a request.
#[derive(Debug, Clone)]
pub struct ProviderFallbackInfo {
    /// ModelProvider family that was originally requested.
    pub requested_provider: String,
    /// Model that was originally requested.
    pub requested_model: String,
    /// ModelProvider family that actually served the request.
    pub actual_provider: String,
    /// Model that actually served the request.
    pub actual_model: String,
}

/// Fallback metadata for a caller that needs exact configured-candidate
/// provenance in addition to the stable provider-family display fields.
#[derive(Debug, Clone)]
pub struct ProviderFallbackAttribution {
    /// Stable provider/model record for channel and direct-agent notices.
    pub fallback: ProviderFallbackInfo,
    /// Exact configured candidate that was requested before fallback.
    pub requested_candidate: String,
    /// Exact configured candidate that served the recovered request.
    pub actual_candidate: String,
}

tokio::task_local! {
    static PROVIDER_FALLBACK: RefCell<Option<ProviderFallbackAttribution>>;
}

tokio::task_local! {
    static PROVIDER_CONTEXT_TRUNCATED: RefCell<bool>;
}

tokio::task_local! {
    static RELIABLE_CALL_ACCOUNTING: Arc<ParkingMutex<ReliableCallAccounting>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReliableEntryId {
    model_slot: usize,
    entry_index: usize,
}

/// Call-scoped outcome retained independently of the provider result.
///
/// In particular, callers must extract it before propagating an error: a
/// rejected attempt can have been billed even when Reliable eventually fails.
#[derive(Debug, Default)]
pub(crate) struct ReliableCallAccounting {
    accepted_route: Option<AcceptedRoute>,
    stream_resume_after: Option<ReliableEntryId>,
    stream_recovery_semantic_empty: bool,
    stream_recovery_failure: Option<ProviderErrorDiagnostic>,
}

impl ReliableCallAccounting {
    /// Transfer the selected stream's provisional physical attempt to the
    /// runtime semantic classifier. It becomes a rejected report only when
    /// that classifier rejects the completed stream response.
    #[doc(hidden)]
    fn into_report(mut self) -> AccountedCallReport {
        AccountedCallReport::new(self.accepted_route.take())
    }
}

/// An opaque per-call accounting scope for the landed dispatch seam.
///
/// The runtime retains this handle across cancellation so a dropped provider
/// future cannot erase reports from attempts that had already completed.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReliableCallAccountingScope {
    accounting: Arc<ParkingMutex<ReliableCallAccounting>>,
}

impl ReliableCallAccountingScope {
    pub(crate) async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        RELIABLE_CALL_ACCOUNTING
            .scope(self.accounting.clone(), future)
            .await
    }

    pub(crate) fn take(&self) -> AccountedCallReport {
        std::mem::take(&mut *self.accounting.lock()).into_report()
    }
}

#[cfg(test)]
async fn scope_reliable_call_accounting<F: std::future::Future>(
    future: F,
) -> (F::Output, AccountedCallReport) {
    let scope = ReliableCallAccountingScope::default();
    let output = scope.scope(future).await;
    (output, scope.take())
}

fn accounted_rejected_attempt_usage() -> Option<TokenUsage> {
    current_dispatch_billable_usage()
}

fn is_stream_recovery_skip(model_slot: usize, entry_index: usize) -> bool {
    RELIABLE_CALL_ACCOUNTING
        .try_with(|accounting| {
            accounting.lock().stream_resume_after.is_some_and(|failed| {
                model_slot == failed.model_slot && entry_index == failed.entry_index
            })
        })
        .unwrap_or(false)
}

/// Preserve Reliable's exact-entry recovery policy, but only once the selected
/// stream has actually been polled.  Billing ownership belongs to dispatch;
/// this is continuation state, not an attempt record.
fn activate_stream_recovery_after_first_poll(model_slot: usize, entry_index: usize) {
    let _ = RELIABLE_CALL_ACCOUNTING.try_with(|accounting| {
        accounting.lock().stream_resume_after = Some(ReliableEntryId {
            model_slot,
            entry_index,
        });
    });
}

fn stream_with_recovery_identity<T>(
    stream: stream::BoxStream<'static, StreamResult<T>>,
    model_slot: usize,
    entry_index: usize,
) -> stream::BoxStream<'static, StreamResult<T>>
where
    T: Send + 'static,
{
    let mut stream = stream;
    let mut started = false;
    stream::poll_fn(move |cx| {
        if !started {
            started = true;
            activate_stream_recovery_after_first_poll(model_slot, entry_index);
        }
        stream.as_mut().poll_next(cx)
    })
    .boxed()
}

/// A later direct/primary recovery supersedes a stream's provisional fallback
/// candidate. Presentation still waits for runtime semantic acceptance.
pub(crate) fn clear_provisional_provider_route() {
    let _ = RELIABLE_CALL_ACCOUNTING.try_with(|accounting| accounting.lock().accepted_route = None);
}

fn record_accepted_attempt(
    entry: &ReliableModelProviderEntry,
    model: &str,
    fallback: Option<ProviderFallbackAttribution>,
) {
    let route = AcceptedRoute::new(
        entry.cooldown_key.clone(),
        entry.served_model(model).to_string(),
        fallback,
    );
    let _ = RELIABLE_CALL_ACCOUNTING
        .try_with(|accounting| accounting.lock().accepted_route = Some(route));
}

fn has_reliable_call_accounting() -> bool {
    RELIABLE_CALL_ACCOUNTING.try_with(|_| ()).is_ok()
}

pub(crate) fn mark_stream_recovery_semantic_empty() {
    let _ = RELIABLE_CALL_ACCOUNTING.try_with(|accounting| {
        accounting.lock().stream_recovery_semantic_empty = true;
    });
}

/// Preserve the classified stream failure while runtime recovers through the
/// remaining candidates without replaying the failed stream entry.
pub(crate) fn record_stream_recovery_failure(error: &anyhow::Error) {
    let _ = RELIABLE_CALL_ACCOUNTING.try_with(|accounting| {
        accounting.lock().stream_recovery_failure = Some(provider_error_diagnostic(error));
    });
}

fn stream_recovery_was_semantic_empty() -> bool {
    RELIABLE_CALL_ACCOUNTING
        .try_with(|accounting| accounting.lock().stream_recovery_semantic_empty)
        .unwrap_or(false)
}

fn stream_recovery_failure_diagnostic() -> Option<ProviderErrorDiagnostic> {
    RELIABLE_CALL_ACCOUNTING
        .try_with(|accounting| accounting.lock().stream_recovery_failure.clone())
        .ok()
        .flatten()
}

/// Take (consume) the last model_provider fallback info, if any.
/// Must be called within a `scope_provider_fallback` scope.
pub fn take_last_provider_fallback() -> Option<ProviderFallbackInfo> {
    PROVIDER_FALLBACK
        .try_with(|cell| cell.borrow_mut().take())
        .ok()
        .flatten()
        .map(|attribution| attribution.fallback)
}

/// Take fallback metadata including the exact configured candidates.
pub fn take_last_provider_fallback_attribution() -> Option<ProviderFallbackAttribution> {
    PROVIDER_FALLBACK
        .try_with(|cell| cell.borrow_mut().take())
        .ok()
        .flatten()
}

/// Take whether Reliable shortened the provider-visible transcript while
/// recovering the current request from a context-window error.
///
/// This is intentionally distinct from fallback attribution: retrying the
/// same candidate must not render a fallback notice, but the caller must not
/// cache that response under the untrimmed request transcript.
pub fn take_last_provider_context_truncation() -> bool {
    PROVIDER_CONTEXT_TRUNCATED
        .try_with(|cell| std::mem::take(&mut *cell.borrow_mut()))
        .unwrap_or(false)
}

fn record_provider_context_truncation() {
    let _ = PROVIDER_CONTEXT_TRUNCATED.try_with(|cell| *cell.borrow_mut() = true);
}

/// Record the fallback that served the current successful provider request, or
/// clear stale attribution when the primary served it.
///
/// A fallback scope can span an agentic tool loop, which issues several model
/// requests. The caller-visible result must describe the request that produced
/// the final response, not an earlier request that only produced a tool call.
fn record_successful_provider_fallback(record: Option<&ProviderFallbackRecord>) {
    if let Some(record) = record {
        record.record();
    } else {
        let _ = PROVIDER_FALLBACK.try_with(|cell| *cell.borrow_mut() = None);
    }
}

/// Commit the route of a response the runtime has accepted semantically.
/// A primary/direct accepted response intentionally clears an earlier fallback
/// candidate in the same outer delivery scope.
pub(crate) fn commit_accepted_provider_route(route: Option<ProviderFallbackAttribution>) {
    let _ = PROVIDER_FALLBACK.try_with(|cell| *cell.borrow_mut() = route);
}

/// Run the given future within a provider-fallback scope.
/// Both `record_provider_fallback` (inside ReliableModelProvider) and
/// `take_last_provider_fallback` (post-loop channel code) must execute
/// within this scope for the data to be visible.
pub async fn scope_provider_fallback<F: std::future::Future>(future: F) -> F::Output {
    PROVIDER_FALLBACK
        .scope(
            RefCell::new(None),
            PROVIDER_CONTEXT_TRUNCATED.scope(RefCell::new(false), future),
        )
        .await
}

/// Record a model_provider fallback event.
fn record_provider_fallback(
    requested_provider: &str,
    requested_model: &str,
    actual_provider: &str,
    actual_model: &str,
    requested_candidate: &str,
    actual_candidate: &str,
) -> ProviderFallbackInfo {
    let fallback = ProviderFallbackInfo {
        requested_provider: requested_provider.to_string(),
        requested_model: requested_model.to_string(),
        actual_provider: actual_provider.to_string(),
        actual_model: actual_model.to_string(),
    };
    let _ = PROVIDER_FALLBACK.try_with(|cell| {
        *cell.borrow_mut() = Some(ProviderFallbackAttribution {
            fallback: fallback.clone(),
            requested_candidate: requested_candidate.to_string(),
            actual_candidate: actual_candidate.to_string(),
        });
    });
    // An accounted runtime call owns presentation timing. Legacy direct trait
    // callers still receive the historical immediate recovery record.
    if !has_reliable_call_accounting() {
        commit_accepted_provider_route(Some(ProviderFallbackAttribution {
            fallback: fallback.clone(),
            requested_candidate: requested_candidate.to_string(),
            actual_candidate: actual_candidate.to_string(),
        }));
    }
    fallback
}

struct ProviderFallbackRecord {
    requested_provider: String,
    requested_model: String,
    actual_provider: String,
    actual_model: String,
    requested_candidate: String,
    actual_candidate: String,
}

impl ProviderFallbackRecord {
    fn new_if_true_fallback(
        requested_provider: &str,
        requested_model: &str,
        actual_provider: &str,
        actual_model: &str,
        used_later_candidate: bool,
        requested_candidate: &str,
        actual_candidate: &str,
    ) -> Option<Self> {
        if !used_later_candidate && requested_model == actual_model {
            return None;
        }

        Some(Self {
            requested_provider: requested_provider.to_string(),
            requested_model: requested_model.to_string(),
            actual_provider: actual_provider.to_string(),
            actual_model: actual_model.to_string(),
            requested_candidate: requested_candidate.to_string(),
            actual_candidate: actual_candidate.to_string(),
        })
    }

    fn record(&self) {
        record_provider_fallback(
            &self.requested_provider,
            &self.requested_model,
            &self.actual_provider,
            &self.actual_model,
            &self.requested_candidate,
            &self.actual_candidate,
        );
    }

    fn info(&self) -> ProviderFallbackInfo {
        ProviderFallbackInfo {
            requested_provider: self.requested_provider.clone(),
            requested_model: self.requested_model.clone(),
            actual_provider: self.actual_provider.clone(),
            actual_model: self.actual_model.clone(),
        }
    }

    fn attribution(&self) -> ProviderFallbackAttribution {
        ProviderFallbackAttribution {
            fallback: self.info(),
            requested_candidate: self.requested_candidate.clone(),
            actual_candidate: self.actual_candidate.clone(),
        }
    }
}

fn stream_with_success_recording<T, IsFinal>(
    stream: stream::BoxStream<'static, StreamResult<T>>,
    fallback_record: Option<ProviderFallbackRecord>,
    accepted_route: AcceptedRoute,
    is_final: IsFinal,
) -> stream::BoxStream<'static, StreamResult<T>>
where
    T: Send + 'static,
    IsFinal: Fn(&T) -> bool + Send + 'static,
{
    stream::unfold(
        (
            stream,
            fallback_record,
            accepted_route,
            false,
            false,
            is_final,
        ),
        |(mut stream, fallback_record, accepted_route, saw_error, recorded, is_final)| async move {
            match stream.next().await {
                Some(event) => {
                    let mut saw_error = saw_error;
                    let mut recorded = recorded;
                    match &event {
                        Ok(value) if !saw_error && !recorded && is_final(value) => {
                            record_successful_provider_fallback(fallback_record.as_ref());
                            record_accepted_route(accepted_route.clone());
                            recorded = true;
                        }
                        Err(_) => {
                            saw_error = true;
                        }
                        Ok(_) => {}
                    }
                    Some((
                        event,
                        (
                            stream,
                            fallback_record,
                            accepted_route,
                            saw_error,
                            recorded,
                            is_final,
                        ),
                    ))
                }
                None => None,
            }
        },
    )
    .boxed()
}

fn record_accepted_route(route: AcceptedRoute) {
    let _ = RELIABLE_CALL_ACCOUNTING
        .try_with(|accounting| accounting.lock().accepted_route = Some(route));
}

pub fn transient_error_hint(err: &anyhow::Error) -> Option<&'static str> {
    let msg = err.to_string();
    // 503 / service unavailable / high demand (Gemini, OpenAI, etc.)
    if msg.contains("503")
        || msg.to_ascii_lowercase().contains("unavailable")
        || msg.to_ascii_lowercase().contains("high demand")
        || msg.to_ascii_lowercase().contains("overloaded")
    {
        return Some(
            "I'm temporarily unable to reach my AI backend — please try again in a moment.",
        );
    }
    // 429 / quota / rate limit
    if msg.contains("429")
        || msg.to_ascii_lowercase().contains("rate limit")
        || msg.to_ascii_lowercase().contains("quota")
    {
        return Some("I've hit a usage limit — please try again shortly.");
    }
    None
}

/// Check if an error is non-retryable (client errors that won't resolve with retries).
pub fn is_non_retryable(err: &anyhow::Error) -> bool {
    // Context window errors are NOT non-retryable — they can be recovered
    // by truncating conversation history, so let the retry loop handle them.
    if is_context_window_exceeded(err) {
        return false;
    }

    // Tool schema validation errors are NOT non-retryable — the model_provider's
    // built-in fallback in compatible.rs can recover by switching to
    // prompt-guided tool instructions.
    if is_tool_schema_error(err) {
        return false;
    }

    // 4xx errors are generally non-retryable (bad request, auth failure, etc.),
    // except 429 (rate-limit — transient) and 408 (timeout — worth retrying).
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && let Some(status) = reqwest_err.status()
    {
        let code = status.as_u16();
        return status.is_client_error() && code != 429 && code != 408;
    }
    // Fallback: parse status codes from stringified errors (some model_providers
    // embed codes in error messages rather than returning typed HTTP errors).
    let msg = err.to_string();
    for word in msg.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = word.parse::<u16>()
            && (400..500).contains(&code)
        {
            return code != 429 && code != 408;
        }
    }

    // Heuristic: detect auth/model failures by keyword when no HTTP status
    // is available (e.g. gRPC or custom transport errors).
    let msg_lower = msg.to_lowercase();
    let auth_failure_hints = [
        "invalid api key",
        "incorrect api key",
        "missing api key",
        "api key not set",
        "authentication failed",
        "auth failed",
        "unauthorized",
        "forbidden",
        "permission denied",
        "access denied",
        "invalid token",
    ];

    if auth_failure_hints
        .iter()
        .any(|hint| msg_lower.contains(hint))
    {
        return true;
    }

    has_model_not_found_hint(&msg_lower)
}

/// Check if an error indicates an authentication/authorization failure.
/// Used by channels to evict cached model_providers whose OAuth tokens may have
/// expired so the next request triggers a fresh credential resolution.
pub fn is_auth_error(err: &anyhow::Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && let Some(status) = reqwest_err.status()
    {
        let code = status.as_u16();
        return code == 401 || code == 403;
    }

    let msg_lower = err.to_string().to_lowercase();
    let hints = [
        "401 unauthorized",
        "403 forbidden",
        "invalid api key",
        "incorrect api key",
        "authentication failed",
        "auth failed",
        "unauthorized",
        "invalid token",
        "token expired",
        "access_token",
    ];

    hints.iter().any(|hint| msg_lower.contains(hint))
}

fn is_missing_credential_error(err: &anyhow::Error) -> bool {
    let lower = err.to_string().to_lowercase();
    [
        "missing api key",
        "api key not set",
        "api key is required",
        "missing access token",
        "token not set",
        "anthropic credentials not set",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
}

pub fn is_tool_schema_error(err: &anyhow::Error) -> bool {
    let lower = err.to_string().to_lowercase();
    let hints = [
        "tool call validation failed",
        "was not in request",
        "not found in tool list",
        "invalid_tool_call",
    ];
    hints.iter().any(|hint| lower.contains(hint))
}

pub fn is_context_window_exceeded(err: &anyhow::Error) -> bool {
    let hints = [
        "exceeds the context window",
        "exceeds the available context size",
        "context window of this model",
        "maximum context length",
        "context length exceeded",
        "too many tokens",
        "token limit exceeded",
        "prompt is too long",
        "input is too long",
        "prompt exceeds max length",
    ];

    err.chain().any(|cause| {
        let lower = cause.to_string().to_lowercase();
        hints.iter().any(|hint| lower.contains(hint))
    })
}

/// Check if an error is a rate-limit (429) error.
fn is_rate_limited(err: &anyhow::Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>()
        && let Some(status) = reqwest_err.status()
    {
        return status.as_u16() == 429;
    }
    let msg = err.to_string();
    msg.contains("429")
        && (msg.contains("Too Many") || msg.contains("rate") || msg.contains("limit"))
}

fn is_non_retryable_rate_limit(err: &anyhow::Error) -> bool {
    if !is_rate_limited(err) {
        return false;
    }

    let msg = err.to_string();
    let lower = msg.to_lowercase();

    let business_hints = [
        "plan does not include",
        "doesn't include",
        "not include",
        "insufficient balance",
        "insufficient_balance",
        "insufficient quota",
        "insufficient_quota",
        "quota exhausted",
        "out of credits",
        "no available package",
        "package not active",
        "purchase package",
        "model not available for your plan",
    ];

    if business_hints.iter().any(|hint| lower.contains(hint)) {
        return true;
    }

    // Known model_provider business codes observed for 429 where retry is futile.
    for token in lower.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = token.parse::<u16>()
            && matches!(code, 1113 | 1311)
        {
            return true;
        }
    }

    false
}

/// Try to extract a Retry-After value (in milliseconds) from an error message.
/// Looks for patterns like `Retry-After: 5` or `retry_after: 2.5` in the error string.
fn parse_retry_after_ms(err: &anyhow::Error) -> Option<u64> {
    let msg = err.to_string();
    let lower = msg.to_lowercase();

    // Look for "retry-after: <number>" or "retry_after: <number>"
    for prefix in &[
        "retry-after:",
        "retry_after:",
        "retry-after ",
        "retry_after ",
    ] {
        if let Some(pos) = lower.find(prefix) {
            let after = &msg[pos + prefix.len()..];
            let num_str: String = after
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(secs) = num_str.parse::<f64>()
                && secs.is_finite()
                && secs >= 0.0
            {
                let millis = Duration::from_secs_f64(secs).as_millis();
                if let Ok(value) = u64::try_from(millis) {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn failure_reason(rate_limited: bool, non_retryable: bool) -> &'static str {
    if rate_limited && non_retryable {
        "rate_limited_non_retryable"
    } else if rate_limited {
        "rate_limited"
    } else if non_retryable {
        "non_retryable"
    } else {
        "retryable"
    }
}

fn compact_error_detail(err: &anyhow::Error) -> String {
    super::sanitize_api_error(&format!("{err:#}"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderErrorDiagnostic {
    kind: &'static str,
    phase: &'static str,
    hint: &'static str,
    endpoint: Option<String>,
}

/// A terminal Reliable failure that can be rendered safely at a user-facing
/// delivery boundary without exposing retry-attempt diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReliableProviderTerminalFailureKind {
    ContextWindow,
    CredentialsMissing,
    Authentication,
    RateLimited,
    ProviderServer,
    ModelNotFound,
    ClientRequest,
    Connection,
    Timeout,
    Other,
}

impl ReliableProviderTerminalFailureKind {
    fn from_diagnostic_kind(kind: &str) -> Self {
        match kind {
            "context_window" => Self::ContextWindow,
            "credentials_missing" => Self::CredentialsMissing,
            "auth" => Self::Authentication,
            "rate_limited" => Self::RateLimited,
            "provider_server" => Self::ProviderServer,
            "model_not_found" => Self::ModelNotFound,
            "client_error" => Self::ClientRequest,
            "connect" | "connect_timeout" | "dns" => Self::Connection,
            "timeout" => Self::Timeout,
            _ => Self::Other,
        }
    }
}

/// The typed terminal presentation cause for a Reliable provider failure.
///
/// `Display` intentionally remains the full diagnostic summary used by logs.
/// User-facing delivery must select a localized message from [`Self::kind`]
/// instead of exposing the retry envelope.
#[derive(Debug)]
pub struct ReliableProviderTerminalFailure {
    kind: ReliableProviderTerminalFailureKind,
    provider: Option<String>,
    endpoint: Option<String>,
    diagnostic: String,
    terminal_cause: Option<anyhow::Error>,
}

impl ReliableProviderTerminalFailure {
    pub fn new(
        kind: ReliableProviderTerminalFailureKind,
        endpoint: Option<String>,
        diagnostic: String,
    ) -> Self {
        Self {
            kind,
            provider: None,
            endpoint,
            diagnostic,
            terminal_cause: None,
        }
    }

    /// Classify a provider error into a safe terminal presentation cause.
    pub fn from_error(error: &anyhow::Error) -> Self {
        let diagnostic = provider_error_diagnostic(error);
        Self::new(
            ReliableProviderTerminalFailureKind::from_diagnostic_kind(diagnostic.kind),
            diagnostic.endpoint,
            format!(
                "provider error: kind={}; phase={}; hint={}",
                diagnostic.kind, diagnostic.phase, diagnostic.hint
            ),
        )
    }

    fn with_cause(
        provider: Option<&str>,
        diagnostic: ProviderErrorDiagnostic,
        failure_aggregate: String,
        terminal_cause: anyhow::Error,
    ) -> Self {
        Self {
            kind: ReliableProviderTerminalFailureKind::from_diagnostic_kind(diagnostic.kind),
            provider: provider
                .filter(|provider| !provider.is_empty())
                .map(str::to_owned),
            endpoint: diagnostic.endpoint,
            diagnostic: failure_aggregate,
            terminal_cause: Some(terminal_cause),
        }
    }

    pub fn kind(&self) -> ReliableProviderTerminalFailureKind {
        self.kind
    }

    /// Attach the configured provider identity used for safe user-facing text.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        let provider = provider.into();
        self.provider = (!provider.is_empty()).then_some(provider);
        self
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    pub fn endpoint_is_local(&self) -> bool {
        self.endpoint.as_deref().is_some_and(|endpoint| {
            reqwest::Url::parse(endpoint)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .is_some_and(|host| {
                    host.eq_ignore_ascii_case("localhost")
                        || host
                            .parse::<IpAddr>()
                            .is_ok_and(|address| address.is_loopback())
                })
        })
    }
}

impl std::fmt::Display for ReliableProviderTerminalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl std::error::Error for ReliableProviderTerminalFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.terminal_cause
            .as_ref()
            .map(|cause| cause.as_ref() as &(dyn std::error::Error + 'static))
    }
}

fn sanitized_url_endpoint(mut url: reqwest::Url) -> String {
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    super::sanitize_api_error(url.as_ref())
}

fn endpoint_from_error_text(text: &str) -> Option<String> {
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    let raw = text[start..]
        .split(|c: char| c.is_whitespace() || matches!(c, ')' | ',' | ';' | '"'))
        .next()
        .unwrap_or("");
    let url = reqwest::Url::parse(raw)
        .or_else(|_| reqwest::Url::parse(raw.trim_end_matches([':', '.'])))
        .ok()?;
    Some(sanitized_url_endpoint(url))
}

fn http_status_from_error_text(text: &str) -> Option<u16> {
    for prefix in [
        "model_provider stream error: modelprovider error:",
        "modelprovider error:",
    ] {
        if let Some(after_prefix) = text.strip_prefix(prefix).map(str::trim_start)
            && let Some(code) = after_prefix
                .get(..3)
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|code| (400..600).contains(code))
                .filter(|_| {
                    after_prefix
                        .as_bytes()
                        .get(3)
                        .is_some_and(u8::is_ascii_whitespace)
                })
        {
            return Some(code);
        }
    }

    for marker in ["api error (", "http "] {
        let mut remainder = text;
        while let Some(start) = remainder.find(marker) {
            let after_marker = &remainder[start + marker.len()..];
            if let Some(code) = after_marker
                .get(..3)
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|code| (400..600).contains(code))
            {
                return Some(code);
            }
            remainder = after_marker;
        }
    }
    None
}

fn http_status_diagnostic(code: u16, endpoint: Option<String>) -> ProviderErrorDiagnostic {
    let (kind, hint) = if matches!(code, 401 | 403) {
        ("auth", "check provider credentials")
    } else if code == 429 {
        ("rate_limited", "wait, change key/quota, or switch provider")
    } else if (500..600).contains(&code) {
        (
            "provider_server",
            "provider returned a server error; retry or switch provider",
        )
    } else if code == 404 {
        (
            "model_not_found",
            "check the configured model id for this provider",
        )
    } else if (400..500).contains(&code) {
        (
            "client_error",
            "provider rejected the request; check config, model, or request shape",
        )
    } else {
        ("http_error", "inspect provider response or switch provider")
    };
    ProviderErrorDiagnostic {
        kind,
        phase: "http_response",
        hint,
        endpoint,
    }
}

fn http_status_is_authoritative(code: u16) -> bool {
    matches!(code, 401 | 403 | 404 | 429) || (500..600).contains(&code)
}

fn has_model_not_found_hint(message: &str) -> bool {
    message.split(": ").any(|segment| {
        let segment = segment.trim_start();

        [
            "model not found",
            "unknown model",
            "unsupported model",
            "invalid model",
        ]
        .iter()
        .any(|hint| segment.starts_with(hint))
            || ["model ", "requested model ", "the requested model "]
                .iter()
                .find_map(|prefix| segment.strip_prefix(prefix))
                .is_some_and(|model_detail| {
                    [
                        " not found",
                        " does not exist",
                        " is unknown",
                        " is unsupported",
                        " is not supported",
                        " is invalid",
                    ]
                    .iter()
                    .any(|hint| model_detail.contains(hint))
                        || model_detail == "unknown"
                })
    })
}

fn provider_error_diagnostic(err: &anyhow::Error) -> ProviderErrorDiagnostic {
    let error_detail = compact_error_detail(err);
    let lower = error_detail.to_lowercase();
    let endpoint = err
        .downcast_ref::<reqwest::Error>()
        .and_then(|reqwest_err| reqwest_err.url().cloned().map(sanitized_url_endpoint))
        .or_else(|| endpoint_from_error_text(&error_detail));
    let structured_status = err
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .map(|status| status.as_u16());
    let text_status = http_status_from_error_text(&lower);

    let http_status = structured_status.or(text_status);

    if let Some(status) = http_status.filter(|status| http_status_is_authoritative(*status)) {
        return http_status_diagnostic(status, endpoint);
    }

    if is_context_window_exceeded(err) {
        return ProviderErrorDiagnostic {
            kind: "context_window",
            phase: "request_validation",
            hint: "reduce context or use a larger-context model",
            endpoint,
        };
    }

    if is_missing_credential_error(err) {
        return ProviderErrorDiagnostic {
            kind: "credentials_missing",
            phase: "configuration",
            hint: "configure provider credentials",
            endpoint,
        };
    }

    if is_auth_error(err) {
        return ProviderErrorDiagnostic {
            kind: "auth",
            phase: "http_response",
            hint: "check provider credentials",
            endpoint,
        };
    }

    if is_rate_limited(err) {
        return ProviderErrorDiagnostic {
            kind: "rate_limited",
            phase: "http_response",
            hint: "wait, change key/quota, or switch provider",
            endpoint,
        };
    }

    if let Some(status) = http_status {
        return http_status_diagnostic(status, endpoint);
    }

    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>() {
        if reqwest_err.is_timeout() && reqwest_err.is_connect() {
            return ProviderErrorDiagnostic {
                kind: "connect_timeout",
                phase: "tls_or_connect",
                hint: "connection reached the host but timed out during connect/TLS; check VPN, firewall, routing, or switch provider",
                endpoint,
            };
        }

        if reqwest_err.is_timeout() {
            return ProviderErrorDiagnostic {
                kind: "timeout",
                phase: "request",
                hint: "provider request timed out; retry or switch provider",
                endpoint,
            };
        }

        if reqwest_err.is_connect() {
            return ProviderErrorDiagnostic {
                kind: "connect",
                phase: "connect",
                hint: "could not open provider connection; check network, VPN, or firewall",
                endpoint,
            };
        }
    }

    if (lower.contains("client error (connect)") && lower.contains("timed out"))
        || lower.contains("ssl connection timeout")
        || (lower.contains("tls") && lower.contains("timeout"))
    {
        return ProviderErrorDiagnostic {
            kind: "connect_timeout",
            phase: "tls_or_connect",
            hint: "connection reached the host but timed out during connect/TLS; check VPN, firewall, routing, or switch provider",
            endpoint,
        };
    }

    if lower.contains("client error (connect)") || lower.contains("connection refused") {
        return ProviderErrorDiagnostic {
            kind: "connect",
            phase: "connect",
            hint: "could not open provider connection; check network, VPN, or firewall",
            endpoint,
        };
    }

    if lower.contains("timed out") || lower.contains("timeout") {
        return ProviderErrorDiagnostic {
            kind: "timeout",
            phase: "request",
            hint: "provider request timed out; retry or switch provider",
            endpoint,
        };
    }

    if lower.contains("dns") || lower.contains("resolve") {
        return ProviderErrorDiagnostic {
            kind: "dns",
            phase: "dns",
            hint: "DNS resolution failed; check network or provider host",
            endpoint,
        };
    }

    if has_model_not_found_hint(&lower) {
        return ProviderErrorDiagnostic {
            kind: "model_not_found",
            phase: "http_response",
            hint: "check the configured model id for this provider",
            endpoint,
        };
    }

    ProviderErrorDiagnostic {
        kind: "provider_error",
        phase: "unknown",
        hint: "inspect provider error or switch provider",
        endpoint,
    }
}

fn provider_failure_attrs(
    provider_name: &str,
    model: &str,
    error_detail: &str,
    diagnostic: &ProviderErrorDiagnostic,
) -> serde_json::Value {
    serde_json::json!({
        "model_provider": provider_name,
        "model": model,
        "error": error_detail,
        "error_kind": diagnostic.kind,
        "error_phase": diagnostic.phase,
        "endpoint": diagnostic.endpoint.as_deref(),
        "hint": diagnostic.hint,
    })
}

fn provider_retry_attrs(
    provider_name: &str,
    model: &str,
    attempt: u32,
    backoff_ms: u64,
    reason: &str,
    error_detail: &str,
    diagnostic: &ProviderErrorDiagnostic,
) -> serde_json::Value {
    serde_json::json!({
        "model_provider": provider_name,
        "model": model,
        "attempt": attempt,
        "backoff_ms": backoff_ms,
        "reason": reason,
        "error": error_detail,
        "error_kind": diagnostic.kind,
        "error_phase": diagnostic.phase,
        "endpoint": diagnostic.endpoint.as_deref(),
        "hint": diagnostic.hint,
    })
}

fn provider_exhausted_attrs(
    provider_name: &str,
    model: &str,
    last_error_detail: Option<&str>,
    last_diagnostic: Option<&ProviderErrorDiagnostic>,
) -> serde_json::Value {
    serde_json::json!({
        "model_provider": provider_name,
        "model": model,
        "error": last_error_detail,
        "error_kind": last_diagnostic.map(|diagnostic| diagnostic.kind),
        "error_phase": last_diagnostic.map(|diagnostic| diagnostic.phase),
        "endpoint": last_diagnostic.and_then(|diagnostic| diagnostic.endpoint.as_deref()),
        "hint": last_diagnostic.map(|diagnostic| diagnostic.hint),
    })
}

fn is_context_turn_boundary(message: &ChatMessage) -> bool {
    message.role == "user"
        && !crate::multimodal::is_prompt_tool_result_message(message)
        && !message.is_pruned_context_separator()
}

fn context_truncation_limit(messages: &[ChatMessage]) -> &'static str {
    if messages.iter().any(is_context_turn_boundary) {
        "only one complete user turn remains"
    } else {
        "history contains no complete user turn"
    }
}

/// Truncate conversation history at a user-turn boundary near the oldest half.
/// Returns the number of non-system messages dropped while keeping at least the
/// most recent complete turn and preserving tool calls with all of their
/// results.
fn truncate_for_context(messages: &mut Vec<ChatMessage>) -> usize {
    let non_system: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role != "system")
        .map(|(i, _)| i)
        .collect();

    let turn_boundaries: Vec<usize> = non_system
        .iter()
        .enumerate()
        .filter_map(|(position, &message_index)| {
            is_context_turn_boundary(&messages[message_index]).then_some(position)
        })
        .collect();
    if turn_boundaries.len() <= 1 {
        return 0;
    }

    let target_drop = non_system.len() / 2;
    let Some(&last_boundary) = turn_boundaries.last() else {
        return 0;
    };
    let first_kept_position = turn_boundaries
        .iter()
        .copied()
        .skip(1)
        .find(|&position| position >= target_drop)
        .unwrap_or(last_boundary);
    let first_kept_index = non_system[first_kept_position];
    let mut original_index = 0usize;
    messages.retain(|message| {
        let keep = message.role == "system" || original_index >= first_kept_index;
        original_index += 1;
        keep
    });

    first_kept_position
}

const MAX_RETAINED_FAILURE_EVENTS: usize = 8;
const MAX_FAILURE_AGGREGATE_BYTES: usize = 2_048;

#[derive(Clone, Debug, Default)]
struct FailureEvents {
    total: usize,
    retained: Vec<String>,
}

impl FailureEvents {
    fn push(&mut self, event: String) {
        self.total += 1;
        if self.retained.len() < MAX_RETAINED_FAILURE_EVENTS {
            self.retained.push(event);
        }
    }

    fn next_index(&self) -> usize {
        self.total + 1
    }
}

fn push_failure(
    failures: &mut FailureEvents,
    attempt: u32,
    max_attempts: u32,
    reason: &'static str,
    diagnostic: Option<&ProviderErrorDiagnostic>,
) {
    // This aggregate can cross into model-visible tool results and durable
    // background results. Keep it to fields controlled by ZeroClaw; the
    // provider response detail is retained in the structured attempt logs.
    let mut failure = format!(
        "event {} (retry {attempt}/{max_attempts}): {reason}",
        failures.next_index()
    );
    if let Some(diagnostic) = diagnostic {
        failure.push_str(&format!(
            "; kind={}; phase={}; hint={}",
            diagnostic.kind, diagnostic.phase, diagnostic.hint
        ));
    }
    failures.push(failure);
}

fn omitted_failure_marker(count: usize) -> String {
    format!("[{count} additional failure event(s) omitted]")
}

fn format_failure_aggregate(header: String, failures: &FailureEvents) -> String {
    let all_omitted_marker = omitted_failure_marker(failures.total);
    let minimum_suffix_len = if failures.total > 0 {
        1 + all_omitted_marker.len()
    } else {
        0
    };
    let mut output = if header.len() + minimum_suffix_len <= MAX_FAILURE_AGGREGATE_BYTES {
        header
    } else {
        format!(
            "Model provider failure after {} failure event(s). Events:",
            failures.total
        )
    };
    let mut retained_count = 0;

    for failure in &failures.retained {
        let candidate_retained_count = retained_count + 1;
        let omitted_after = failures.total - candidate_retained_count;
        let reserved_marker_len = if omitted_after > 0 {
            1 + omitted_failure_marker(omitted_after).len()
        } else {
            0
        };
        if output.len() + 1 + failure.len() + reserved_marker_len > MAX_FAILURE_AGGREGATE_BYTES {
            break;
        }
        output.push('\n');
        output.push_str(failure);
        retained_count = candidate_retained_count;
    }

    let omitted = failures.total - retained_count;
    if omitted > 0 {
        output.push('\n');
        output.push_str(&omitted_failure_marker(omitted));
    }
    debug_assert!(output.len() <= MAX_FAILURE_AGGREGATE_BYTES);
    output
}

fn failure_aggregate(failures: &FailureEvents) -> String {
    format_failure_aggregate(
        format!(
            "All model providers/models failed after {} failure event(s). Events:",
            failures.total
        ),
        failures,
    )
}

fn context_failure_aggregate(message: &str, failures: &FailureEvents) -> String {
    format_failure_aggregate(
        format!(
            "{message} Failed after {} failure event(s). Events:",
            failures.total
        ),
        failures,
    )
}

fn is_empty_completion(resp: &ChatResponse) -> bool {
    resp.is_semantically_empty_terminal()
}

fn is_empty_text_completion(text: &str) -> bool {
    zeroclaw_api::model_provider::strip_think_tags(text).is_empty()
}

fn is_semantic_empty_completion_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion>())
}

/// Extract billing metadata a Reliable terminal error preserves alongside its
/// actual cause. The caller still returns the original error unchanged.
pub(crate) fn terminal_error_usage(error: &anyhow::Error) -> Option<TokenUsage> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<ReliableRejectedCompletionUsage>()
            .map(|rejected| rejected.usage.clone())
    })
}

/// A Reliable chat request exhausted its candidates after receiving rejected
/// semantic completions. The provider-reported usage is retained so the turn
/// loop can account for work that was billed even though no response was
/// accepted.
#[derive(Debug)]
pub struct ReliableRejectedCompletionUsage {
    pub usage: TokenUsage,
    failures: FailureEvents,
    terminal_cause: Option<anyhow::Error>,
}

impl ReliableRejectedCompletionUsage {
    fn new(usage: TokenUsage, failures: FailureEvents) -> Self {
        Self {
            usage,
            failures,
            terminal_cause: None,
        }
    }

    fn with_terminal_cause(
        usage: TokenUsage,
        failures: FailureEvents,
        cause: anyhow::Error,
    ) -> Self {
        Self {
            usage,
            failures,
            terminal_cause: Some(cause),
        }
    }
}

impl std::fmt::Display for ReliableRejectedCompletionUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", failure_aggregate(&self.failures))
    }
}

impl std::error::Error for ReliableRejectedCompletionUsage {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.terminal_cause
            .as_ref()
            .map(|cause| cause.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// The final candidate attempt completed successfully at the transport layer
/// but supplied neither usable text nor a native tool call. This remains typed
/// independently from optional rejected-attempt usage so delivery layers can
/// classify the actual terminal failure without guessing from accounting data.
#[derive(Debug)]
pub struct ReliableSemanticEmptyCompletion {
    failures: FailureEvents,
    rejected_usage: Option<ReliableRejectedCompletionUsage>,
    terminal_cause: zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion,
}

impl std::fmt::Display for ReliableSemanticEmptyCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", failure_aggregate(&self.failures))
    }
}

impl std::error::Error for ReliableSemanticEmptyCompletion {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.rejected_usage
            .as_ref()
            .map(|usage| usage as &(dyn std::error::Error + 'static))
            .or(Some(&self.terminal_cause))
    }
}

fn reliable_terminal_error(
    failures: FailureEvents,
    rejected_attempt_usage: Option<TokenUsage>,
    final_cause_is_semantic_empty: bool,
) -> anyhow::Error {
    let rejected_attempt_usage = rejected_attempt_usage.or_else(accounted_rejected_attempt_usage);
    if final_cause_is_semantic_empty {
        let terminal_cause = zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion;
        return anyhow::Error::new(ReliableSemanticEmptyCompletion {
            failures: failures.clone(),
            rejected_usage: rejected_attempt_usage.map(|usage| {
                ReliableRejectedCompletionUsage::with_terminal_cause(
                    usage,
                    failures,
                    anyhow::Error::new(
                        zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion,
                    ),
                )
            }),
            terminal_cause,
        });
    }

    match rejected_attempt_usage {
        Some(usage) => anyhow::Error::new(ReliableRejectedCompletionUsage::new(usage, failures)),
        None => anyhow::Error::msg(failure_aggregate(&failures)),
    }
}

fn reliable_terminal_error_with_cause(
    provider: Option<&str>,
    failures: FailureEvents,
    rejected_attempt_usage: Option<TokenUsage>,
    final_cause_is_semantic_empty: bool,
    final_cause: Option<anyhow::Error>,
) -> anyhow::Error {
    let rejected_attempt_usage = rejected_attempt_usage.or_else(accounted_rejected_attempt_usage);
    if !final_cause_is_semantic_empty && let Some(cause) = final_cause {
        let terminal_failure = anyhow::Error::new(ReliableProviderTerminalFailure::with_cause(
            provider,
            provider_error_diagnostic(&cause),
            failure_aggregate(&failures),
            cause,
        ));
        if let Some(usage) = rejected_attempt_usage {
            return anyhow::Error::new(ReliableRejectedCompletionUsage::with_terminal_cause(
                usage,
                failures,
                terminal_failure,
            ));
        }
        return terminal_failure;
    }
    if !final_cause_is_semantic_empty && let Some(diagnostic) = stream_recovery_failure_diagnostic()
    {
        let terminal_failure = anyhow::Error::new(
            ReliableProviderTerminalFailure::new(
                ReliableProviderTerminalFailureKind::from_diagnostic_kind(diagnostic.kind),
                diagnostic.endpoint,
                failure_aggregate(&failures),
            )
            .with_provider(provider.unwrap_or_default()),
        );
        if let Some(usage) = rejected_attempt_usage {
            return anyhow::Error::new(ReliableRejectedCompletionUsage::with_terminal_cause(
                usage,
                failures,
                terminal_failure,
            ));
        }
        return terminal_failure;
    }
    reliable_terminal_error(
        failures,
        rejected_attempt_usage,
        final_cause_is_semantic_empty,
    )
}

pub(crate) fn accumulate_usage(total: &mut Option<TokenUsage>, usage: Option<&TokenUsage>) {
    let Some(usage) = usage else {
        return;
    };
    let accumulated = total.get_or_insert_with(TokenUsage::default);
    for (target, value) in [
        (&mut accumulated.input_tokens, usage.input_tokens),
        (&mut accumulated.output_tokens, usage.output_tokens),
        (
            &mut accumulated.cached_input_tokens,
            usage.cached_input_tokens,
        ),
    ] {
        if let Some(value) = value {
            *target = Some(target.unwrap_or(0).saturating_add(value));
        }
    }
}

fn combine_response_usage(response: &mut ChatResponse, prior_attempts: Option<TokenUsage>) {
    let Some(prior_attempts) = prior_attempts else {
        return;
    };
    let mut combined = Some(prior_attempts);
    accumulate_usage(&mut combined, response.usage.as_ref());
    response.usage = combined;
}

enum ReliableModelProviderEntryProvider {
    Direct(Box<dyn ModelProvider>),
    Pinned(crate::model_pin::ModelPinnedProvider),
}

impl ReliableModelProviderEntryProvider {
    fn as_model_provider(&self) -> &dyn ModelProvider {
        match self {
            Self::Direct(provider) => provider.as_ref(),
            Self::Pinned(provider) => provider,
        }
    }

    fn served_model<'a>(&'a self, requested_model: &'a str) -> &'a str {
        match self {
            Self::Direct(_) => requested_model,
            Self::Pinned(provider) => provider.pinned_model(),
        }
    }
}

pub(crate) struct ReliableModelProviderEntry {
    display_name: String,
    /// Exact configured candidate identity used for fallback attribution.
    ///
    /// This deliberately differs from `display_name`: two aliases can share a
    /// provider family and model while still being distinct fallback candidates.
    candidate_name: String,
    cooldown_key: String,
    provider: ReliableModelProviderEntryProvider,
}

impl ReliableModelProviderEntry {
    pub(crate) fn new(
        display_name: impl Into<String>,
        cooldown_key: impl Into<String>,
        provider: Box<dyn ModelProvider>,
    ) -> Self {
        let display_name = display_name.into();
        Self {
            candidate_name: display_name.clone(),
            display_name,
            cooldown_key: cooldown_key.into(),
            provider: ReliableModelProviderEntryProvider::Direct(provider),
        }
    }

    pub(crate) fn new_with_candidate(
        display_name: impl Into<String>,
        cooldown_key: impl Into<String>,
        candidate_name: impl Into<String>,
        provider: Box<dyn ModelProvider>,
    ) -> Self {
        Self {
            display_name: display_name.into(),
            candidate_name: candidate_name.into(),
            cooldown_key: cooldown_key.into(),
            provider: ReliableModelProviderEntryProvider::Direct(provider),
        }
    }

    /// Build an entry that serves `pinned_model` regardless of the requested
    /// model. The [`crate::model_pin::ModelPinnedProvider`] wrapper is the
    /// source of truth for the pinned model; this entry reads it from the
    /// wrapper at use-time.
    pub(crate) fn new_pinned(
        display_name: impl Into<String>,
        cooldown_key: impl Into<String>,
        alias: &str,
        pinned_model: &str,
        inner: Box<dyn ModelProvider>,
    ) -> Self {
        let cooldown_key = cooldown_key.into();
        Self {
            display_name: display_name.into(),
            candidate_name: cooldown_key.clone(),
            cooldown_key,
            provider: ReliableModelProviderEntryProvider::Pinned(
                crate::model_pin::ModelPinnedProvider::builder(alias)
                    .pinned_model(pinned_model)
                    .inner(inner)
                    .build(),
            ),
        }
    }

    /// Model this entry serves for `requested_model`: the pinned model when
    /// the entry is model-pinned, otherwise the requested model unchanged.
    fn served_model<'a>(&'a self, requested_model: &'a str) -> &'a str {
        self.provider.served_model(requested_model)
    }

    fn candidate_name(&self) -> &str {
        &self.candidate_name
    }

    fn provider(&self) -> &dyn ModelProvider {
        self.provider.as_model_provider()
    }
}

/// ModelProvider wrapper with retry + auth-key rotation. The model_provider Vec exists
/// for tests to exercise multi-provider failover; production wiring always
/// passes a single primary. Per-model failover chains are also test-only —
/// the schema no longer surfaces them.
pub struct ReliableModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    model_providers: Vec<ReliableModelProviderEntry>,
    max_retries: u32,
    base_backoff_ms: u64,
    /// Extra API keys for rotation (index tracks round-robin position).
    api_keys: Vec<String>,
    key_index: AtomicUsize,
    /// Per-model failover chains. Test-only: model_name → [alt1, alt2, ...].
    model_fallbacks: HashMap<String, Vec<String>>,
    /// Transient provider cooldowns after retryable rate limits.
    /// Source of truth: live provider 429 / Retry-After evidence observed by
    /// this wrapper. It is intentionally in-memory and per wrapper instance.
    rate_limit_cooldowns: Mutex<HashMap<String, Instant>>,
}

impl ReliableModelProvider {
    pub fn new(
        alias: &str,
        model_providers: Vec<(String, Box<dyn ModelProvider>)>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        let model_providers = model_providers
            .into_iter()
            .map(|(display_name, provider)| {
                ReliableModelProviderEntry::new(display_name.clone(), display_name, provider)
            })
            .collect();

        Self::new_with_entries(alias, model_providers, max_retries, base_backoff_ms)
    }

    pub(crate) fn new_with_entries(
        alias: &str,
        model_providers: Vec<ReliableModelProviderEntry>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        Self {
            alias: alias.to_string(),
            model_providers,
            max_retries,
            base_backoff_ms: base_backoff_ms.max(50),
            api_keys: Vec::new(),
            key_index: AtomicUsize::new(0),
            model_fallbacks: HashMap::new(),
            rate_limit_cooldowns: Mutex::new(HashMap::new()),
        }
    }
    /// Set additional API keys for round-robin rotation on rate-limit errors.
    pub fn with_api_keys(mut self, keys: Vec<String>) -> Self {
        self.api_keys = keys;
        self
    }

    #[cfg(test)]
    pub fn with_model_fallbacks(mut self, fallbacks: HashMap<String, Vec<String>>) -> Self {
        self.model_fallbacks = fallbacks;
        self
    }

    /// Build the list of models to try: [original, alt1, alt2, ...]
    fn model_chain<'a>(&'a self, model: &'a str) -> Vec<&'a str> {
        let mut chain = vec![model];
        if let Some(fallbacks) = self.model_fallbacks.get(model) {
            chain.extend(fallbacks.iter().map(|s| s.as_str()));
        }
        chain
    }

    /// Advance to the next API key and return it, or None if no extra keys configured.
    fn rotate_key(&self) -> Option<&str> {
        if self.api_keys.is_empty() {
            return None;
        }
        let idx = self.key_index.fetch_add(1, Ordering::Relaxed) % self.api_keys.len();
        Some(&self.api_keys[idx])
    }

    /// Compute backoff duration, respecting Retry-After if present.
    fn compute_backoff(&self, base: u64, err: &anyhow::Error) -> u64 {
        if let Some(retry_after) = parse_retry_after_ms(err) {
            // Use Retry-After but cap at 30s to avoid indefinite waits
            retry_after.min(30_000).max(base)
        } else {
            base
        }
    }

    fn configured_provider_identity(&self) -> Option<&str> {
        self.model_providers
            .first()
            .map(ReliableModelProviderEntry::candidate_name)
            .filter(|provider| !provider.is_empty())
    }

    /// Default cooldown after a retryable 429 when Retry-After is absent.
    const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(10);

    /// Returns whether a cooldown is active and prunes expired cooldowns.
    fn provider_cooldown_active(&self, cooldown_key: &str) -> bool {
        let now = Instant::now();
        let mut cooldowns = self
            .rate_limit_cooldowns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        match cooldowns.get(cooldown_key).copied() {
            Some(deadline) if now < deadline => true,
            Some(_) => {
                cooldowns.remove(cooldown_key);
                false
            }
            None => false,
        }
    }

    fn provider_should_skip_for_cooldown(&self, entry: &ReliableModelProviderEntry) -> bool {
        self.model_providers.len() > 1 && self.provider_cooldown_active(&entry.cooldown_key)
    }

    fn record_cooldown_skip_failure(failures: &mut FailureEvents, max_attempts: u32) {
        let diagnostic = ProviderErrorDiagnostic {
            kind: "rate_limited",
            phase: "cooldown",
            hint: "wait for provider cooldown or switch provider",
            endpoint: None,
        };
        push_failure(
            failures,
            0,
            max_attempts,
            "rate_limit_cooldown",
            Some(&diagnostic),
        );
    }

    fn log_cooldown_skip(&self, provider_name: &str, model: &str) {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "model_provider": provider_name,
                    "model": model,
                })
            ),
            "Skipping model_provider during rate-limit cooldown"
        );
    }

    fn set_rate_limit_cooldown(&self, cooldown_key: &str, err: &anyhow::Error) -> Duration {
        let cooldown = parse_retry_after_ms(err)
            .map(|ms| Duration::from_millis(ms.min(60_000)))
            .unwrap_or(Self::RATE_LIMIT_COOLDOWN);

        let mut cooldowns = self
            .rate_limit_cooldowns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cooldowns.insert(cooldown_key.to_string(), Instant::now() + cooldown);
        cooldown
    }

    fn cool_down_rate_limited_provider(
        &self,
        entry: &ReliableModelProviderEntry,
        model: &str,
        err: &anyhow::Error,
    ) -> Duration {
        let cooldown = self.set_rate_limit_cooldown(&entry.cooldown_key, err);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "model_provider": entry.display_name,
                    "model": model,
                    "cooldown_ms": cooldown.as_millis(),
                })
            ),
            "ModelProvider rate-limited; trying next provider"
        );
        cooldown
    }

    /// Shared tail of the empty-completion retry path used by every chat method:
    /// record the empty attempt, warn, sleep the current backoff, then double it
    /// (capped). The caller owns the response-shape check and either retries
    /// or records its final failed attempt. See [`is_empty_completion`].
    async fn backoff_after_empty_completion(
        &self,
        failures: &mut FailureEvents,
        provider_name: &str,
        model: &str,
        attempt: u32,
        backoff_ms: &mut u64,
    ) {
        self.record_empty_completion_failure(failures, provider_name, model, attempt, true);
        tokio::time::sleep(Duration::from_millis(*backoff_ms)).await;
        *backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
    }

    /// Record an invalid but HTTP-successful provider response. The retry
    /// loops use this as an ordinary failure so that exhaustion advances to
    /// fallback instead of returning a successful blank turn.
    fn record_empty_completion_failure(
        &self,
        failures: &mut FailureEvents,
        provider_name: &str,
        model: &str,
        attempt: u32,
        retrying: bool,
    ) {
        push_failure(
            failures,
            attempt + 1,
            self.max_retries + 1,
            "empty_response",
            None,
        );
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "model_provider": provider_name,
                    "model": model,
                    "attempt": attempt + 1,
                    "retrying": retrying,
                })),
            if retrying {
                "Empty completion; retrying"
            } else {
                "Empty completion; retries exhausted"
            }
        );
    }
}

#[async_trait]
impl ModelProvider for ReliableModelProvider {
    fn has_stable_request_identity(&self, model: &str) -> bool {
        if self.model_providers.len() != 1
            || self
                .model_fallbacks
                .get(model)
                .is_some_and(|fallbacks| !fallbacks.is_empty())
        {
            return false;
        }

        self.model_providers
            .first()
            .is_some_and(|entry| entry.provider().has_stable_request_identity(model))
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        for entry in &self.model_providers {
            let provider_name = entry.display_name.as_str();
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"model_provider": provider_name})),
                "Warming up model_provider connection pool"
            );
            if ProviderDispatch::from_ref(entry.provider())
                .warmup()
                .await
                .is_err()
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"model_provider": provider_name})),
                    "Warmup failed (non-fatal)"
                );
            }
        }
        Ok(())
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        mark_current_dispatch_composite();
        let models = self.model_chain(model);
        let mut failures = FailureEvents::default();
        let mut final_cause_is_semantic_empty = stream_recovery_was_semantic_empty();
        let mut final_cause = None;
        let mut final_cause_provider = None;

        // Outer: model fallback chain. Middle: model_provider priority. Inner: retries.
        // Each iteration: attempt one (model_provider, model) call. On success, return
        // immediately. On non-retryable error, break to next model_provider. On
        // retryable error, sleep with exponential backoff and retry.
        for (model_slot, current_model) in models.iter().enumerate() {
            for (entry_index, entry) in self.model_providers.iter().enumerate() {
                let provider_name = entry.display_name.as_str();
                let served_model = entry.served_model(current_model);
                if self.provider_should_skip_for_cooldown(entry) {
                    self.log_cooldown_skip(provider_name, served_model);
                    Self::record_cooldown_skip_failure(&mut failures, self.max_retries + 1);
                    continue;
                }

                let mut backoff_ms = self.base_backoff_ms;
                let mut last_error_detail: Option<String> = None;
                let mut last_diagnostic: Option<ProviderErrorDiagnostic> = None;

                for attempt in 0..=self.max_retries {
                    match with_exact_dispatch_route(
                        entry.cooldown_key.clone(),
                        entry.served_model(current_model).to_string(),
                        ProviderDispatch::from_ref(entry.provider()).chat_with_system(
                            system_prompt,
                            message,
                            current_model,
                            temperature,
                        ),
                    )
                    .await
                    {
                        Ok(resp) => {
                            if is_empty_text_completion(&resp) {
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            if attempt > 0
                                || served_model != model
                                || model_slot != 0
                                || entry_index != 0
                                || self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    != Some(provider_name)
                            {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "attempt": attempt, "original_model": model})), "ModelProvider recovered (failover/retry)");
                                let primary = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.candidate_name())
                                    .unwrap_or("");
                                let primary_provider = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    .unwrap_or("");
                                let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                                    primary_provider,
                                    model,
                                    provider_name,
                                    served_model,
                                    model_slot != 0 || entry_index != 0,
                                    primary,
                                    entry.candidate_name(),
                                );
                                record_successful_provider_fallback(fallback_record.as_ref());
                                record_accepted_attempt(
                                    entry,
                                    current_model,
                                    fallback_record
                                        .as_ref()
                                        .map(ProviderFallbackRecord::attribution),
                                );
                            } else {
                                record_successful_provider_fallback(None);
                                record_accepted_attempt(entry, current_model, None);
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            if is_semantic_empty_completion_error(&e) {
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            final_cause_is_semantic_empty = false;
                            // Context window exceeded: no history to truncate
                            // in chat_with_system, bail immediately.
                            if is_context_window_exceeded(&e) {
                                let diagnostic = provider_error_diagnostic(&e);
                                push_failure(
                                    &mut failures,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    "context_window",
                                    Some(&diagnostic),
                                );
                                let context_error = context_failure_aggregate(
                                    "Request exceeds model context window.",
                                    &failures,
                                );
                                return Err(reliable_terminal_error_with_cause(
                                    Some(entry.candidate_name()),
                                    failures,
                                    None,
                                    false,
                                    Some(e),
                                )
                                .context(context_error));
                            }

                            let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                            let non_retryable = is_non_retryable(&e) || non_retryable_rate_limit;
                            let rate_limited = is_rate_limited(&e);
                            let failure_reason = failure_reason(rate_limited, non_retryable);
                            let error_detail = compact_error_detail(&e);
                            let diagnostic = provider_error_diagnostic(&e);
                            last_error_detail = Some(error_detail.clone());
                            last_diagnostic = Some(diagnostic.clone());

                            push_failure(
                                &mut failures,
                                attempt + 1,
                                self.max_retries + 1,
                                failure_reason,
                                Some(&diagnostic),
                            );

                            // Rate-limit with rotatable keys: cycle to the next API key
                            // so the retry hits a different quota bucket.
                            if rate_limited
                                && !non_retryable_rate_limit
                                && let Some(new_key) = self.rotate_key()
                            {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "error": error_detail})), &format!("Rate limited; key rotation selected key ending ...{} \
                                     but cannot apply (ModelProvider trait has no set_api_key). \
                                     Retrying with original key.", &new_key[new_key.len().saturating_sub(4)..]));
                            }

                            if non_retryable {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_failure_attrs(
                                            provider_name,
                                            served_model,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "Non-retryable error, moving on"
                                );
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if rate_limited && self.model_providers.len() > 1 {
                                self.cool_down_rate_limited_provider(entry, served_model, &e);
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if attempt < self.max_retries {
                                let wait = self.compute_backoff(backoff_ms, &e);
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_retry_attrs(
                                            provider_name,
                                            served_model,
                                            attempt + 1,
                                            wait,
                                            failure_reason,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "ModelProvider call failed, retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(wait)).await;
                                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                            }
                            final_cause = Some(e);
                            final_cause_provider = Some(entry.candidate_name().to_string());
                        }
                    }
                }

                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(provider_exhausted_attrs(
                            provider_name,
                            served_model,
                            last_error_detail.as_deref(),
                            last_diagnostic.as_ref(),
                        )),
                    "Exhausted retries, trying next model_provider/model"
                );
            }

            if *current_model != model {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"original_model": model, "fallback_model": *current_model})), "Model fallback exhausted all model_providers, trying next fallback model");
            }
        }

        Err(reliable_terminal_error_with_cause(
            final_cause_provider
                .as_deref()
                .or_else(|| self.configured_provider_identity()),
            failures,
            None,
            final_cause_is_semantic_empty,
            final_cause,
        ))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        mark_current_dispatch_composite();
        let models = self.model_chain(model);
        let mut failures = FailureEvents::default();
        let mut final_cause_is_semantic_empty = stream_recovery_was_semantic_empty();
        let mut final_cause = None;
        let mut final_cause_provider = None;
        let mut effective_messages = messages.to_vec();
        let mut context_truncated = false;

        for (model_slot, current_model) in models.iter().enumerate() {
            for (entry_index, entry) in self.model_providers.iter().enumerate() {
                let provider_name = entry.display_name.as_str();
                let served_model = entry.served_model(current_model);
                if self.provider_should_skip_for_cooldown(entry) {
                    self.log_cooldown_skip(provider_name, served_model);
                    Self::record_cooldown_skip_failure(&mut failures, self.max_retries + 1);
                    continue;
                }

                let mut backoff_ms = self.base_backoff_ms;
                let mut last_error_detail: Option<String> = None;
                let mut last_diagnostic: Option<ProviderErrorDiagnostic> = None;

                for attempt in 0..=self.max_retries {
                    match with_exact_dispatch_route(
                        entry.cooldown_key.clone(),
                        entry.served_model(current_model).to_string(),
                        ProviderDispatch::from_ref(entry.provider()).chat_with_history(
                            &effective_messages,
                            current_model,
                            temperature,
                        ),
                    )
                    .await
                    {
                        Ok(resp) => {
                            if is_empty_text_completion(&resp) {
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            if attempt > 0
                                || served_model != model
                                || model_slot != 0
                                || entry_index != 0
                                || context_truncated
                                || self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    != Some(provider_name)
                            {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "attempt": attempt, "original_model": model, "context_truncated": context_truncated})), "ModelProvider recovered (failover/retry)");
                                let primary = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.candidate_name())
                                    .unwrap_or("");
                                let primary_provider = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    .unwrap_or("");
                                let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                                    primary_provider,
                                    model,
                                    provider_name,
                                    served_model,
                                    model_slot != 0 || entry_index != 0,
                                    primary,
                                    entry.candidate_name(),
                                );
                                record_successful_provider_fallback(fallback_record.as_ref());
                                record_accepted_attempt(
                                    entry,
                                    current_model,
                                    fallback_record
                                        .as_ref()
                                        .map(ProviderFallbackRecord::attribution),
                                );
                            } else {
                                record_successful_provider_fallback(None);
                                record_accepted_attempt(entry, current_model, None);
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            if is_semantic_empty_completion_error(&e) {
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }
                            final_cause_is_semantic_empty = false;
                            // Context window exceeded: truncate history and retry
                            if is_context_window_exceeded(&e) && !context_truncated {
                                let diagnostic = provider_error_diagnostic(&e);
                                push_failure(
                                    &mut failures,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    "context_window",
                                    Some(&diagnostic),
                                );
                                let dropped = truncate_for_context(&mut effective_messages);
                                if dropped > 0 {
                                    record_provider_context_truncation();
                                    context_truncated = true;
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "dropped": dropped, "remaining": effective_messages.len()})), "Context window exceeded; truncated history and retrying");
                                    continue; // Retry with truncated messages (counts as an attempt)
                                }
                                // No complete older turn can be removed safely.
                                let truncation_limit =
                                    context_truncation_limit(&effective_messages);
                                let context_error = context_failure_aggregate(
                                    &format!(
                                        "Request exceeds model context window and cannot be reduced without \
                                         breaking message/tool pairing ({truncation_limit}). Try using a model \
                                         with a larger context window, reducing the number of tools/skills, or \
                                         enabling compact_context in config."
                                    ),
                                    &failures,
                                );
                                return Err(reliable_terminal_error_with_cause(
                                    Some(entry.candidate_name()),
                                    failures,
                                    None,
                                    false,
                                    Some(e),
                                )
                                .context(context_error));
                            }

                            let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                            let non_retryable = is_non_retryable(&e) || non_retryable_rate_limit;
                            let rate_limited = is_rate_limited(&e);
                            let failure_reason = failure_reason(rate_limited, non_retryable);
                            let error_detail = compact_error_detail(&e);
                            let diagnostic = provider_error_diagnostic(&e);
                            last_error_detail = Some(error_detail.clone());
                            last_diagnostic = Some(diagnostic.clone());

                            push_failure(
                                &mut failures,
                                attempt + 1,
                                self.max_retries + 1,
                                failure_reason,
                                Some(&diagnostic),
                            );

                            if rate_limited
                                && !non_retryable_rate_limit
                                && let Some(new_key) = self.rotate_key()
                            {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "error": error_detail})), &format!("Rate limited; key rotation selected key ending ...{} \
                                     but cannot apply (ModelProvider trait has no set_api_key). \
                                     Retrying with original key.", &new_key[new_key.len().saturating_sub(4)..]));
                            }

                            if non_retryable {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_failure_attrs(
                                            provider_name,
                                            served_model,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "Non-retryable error, moving on"
                                );
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if rate_limited && self.model_providers.len() > 1 {
                                self.cool_down_rate_limited_provider(entry, served_model, &e);
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if attempt < self.max_retries {
                                let wait = self.compute_backoff(backoff_ms, &e);
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_retry_attrs(
                                            provider_name,
                                            served_model,
                                            attempt + 1,
                                            wait,
                                            failure_reason,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "ModelProvider call failed, retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(wait)).await;
                                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                            }
                            final_cause = Some(e);
                            final_cause_provider = Some(entry.candidate_name().to_string());
                        }
                    }
                }

                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(provider_exhausted_attrs(
                            provider_name,
                            served_model,
                            last_error_detail.as_deref(),
                            last_diagnostic.as_ref(),
                        )),
                    "Exhausted retries, trying next model_provider/model"
                );
            }
        }

        Err(reliable_terminal_error_with_cause(
            final_cause_provider
                .as_deref()
                .or_else(|| self.configured_provider_identity()),
            failures,
            None,
            final_cause_is_semantic_empty,
            final_cause,
        ))
    }

    fn capabilities(&self) -> crate::traits::ProviderCapabilities {
        let mut capabilities = self
            .model_providers
            .first()
            .map(|entry| entry.provider().capabilities())
            .unwrap_or_default();
        // A request may advance past the primary after a retryable failure.
        // Report vision only when every reachable provider can accept images;
        // otherwise the turn engine must select a dedicated vision route before
        // dispatch instead of admitting an image that a fallback could reject.
        capabilities.vision = !self.model_providers.is_empty()
            && self
                .model_providers
                .iter()
                .all(|entry| entry.provider().supports_vision());
        capabilities.native_tool_calling = !self.model_providers.is_empty()
            && self
                .model_providers
                .iter()
                .all(|entry| entry.provider().supports_native_tools());
        capabilities
    }

    fn capabilities_for_model(&self, model: &str) -> crate::traits::ProviderCapabilities {
        let mut capabilities = self
            .model_providers
            .first()
            .map(|entry| entry.provider().capabilities_for_model(model))
            .unwrap_or_default();
        capabilities.vision = !self.model_providers.is_empty()
            && self
                .model_providers
                .iter()
                .all(|entry| entry.provider().capabilities_for_model(model).vision);
        capabilities.native_tool_calling = !self.model_providers.is_empty()
            && self.model_providers.iter().all(|entry| {
                entry
                    .provider()
                    .capabilities_for_model(model)
                    .native_tool_calling
            });
        capabilities
    }

    fn has_mixed_native_tool_support_for_model(&self, model: &str) -> bool {
        let mut has_native = false;
        let mut has_text_only = false;

        for entry in &self.model_providers {
            let provider = entry.provider();
            if provider.has_mixed_native_tool_support_for_model(model) {
                return true;
            }
            if provider.capabilities_for_model(model).native_tool_calling {
                has_native = true;
            } else {
                has_text_only = true;
            }
            if has_native && has_text_only {
                return true;
            }
        }

        false
    }

    fn supports_native_tools(&self) -> bool {
        // The turn loop selects one tool protocol before Reliable chooses a
        // candidate. A native request is therefore safe only when every
        // candidate the request may reach accepts native tool specifications.
        !self.model_providers.is_empty()
            && self
                .model_providers
                .iter()
                .all(|entry| entry.provider().supports_native_tools())
    }

    fn supports_vision(&self) -> bool {
        self.capabilities().vision
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        mark_current_dispatch_composite();
        let models = self.model_chain(model);
        let mut failures = FailureEvents::default();
        let mut final_cause_is_semantic_empty = stream_recovery_was_semantic_empty();
        let mut effective_messages = messages.to_vec();
        let mut context_truncated = false;
        let mut rejected_attempt_usage = None;
        let mut final_cause = None;
        let mut final_cause_provider = None;

        for (model_slot, current_model) in models.iter().enumerate() {
            for (entry_index, entry) in self.model_providers.iter().enumerate() {
                if is_stream_recovery_skip(model_slot, entry_index) {
                    final_cause_provider = Some(entry.candidate_name().to_string());
                    continue;
                }
                let provider_name = entry.display_name.as_str();
                let served_model = entry.served_model(current_model);
                if self.provider_should_skip_for_cooldown(entry) {
                    self.log_cooldown_skip(provider_name, served_model);
                    Self::record_cooldown_skip_failure(&mut failures, self.max_retries + 1);
                    continue;
                }

                let mut backoff_ms = self.base_backoff_ms;
                let mut last_error_detail: Option<String> = None;
                let mut last_diagnostic: Option<ProviderErrorDiagnostic> = None;

                for attempt in 0..=self.max_retries {
                    match with_exact_dispatch_route(
                        entry.cooldown_key.clone(),
                        entry.served_model(current_model).to_string(),
                        ProviderDispatch::from_ref(entry.provider()).chat_with_tools(
                            &effective_messages,
                            tools,
                            current_model,
                            temperature,
                        ),
                    )
                    .await
                    {
                        Ok(mut resp) => {
                            if is_empty_completion(&resp) {
                                if let Some(usage) = resp.usage.clone()
                                    && !has_reliable_call_accounting()
                                {
                                    accumulate_usage(&mut rejected_attempt_usage, Some(&usage));
                                }
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            if let Some(usage) = rejected_attempt_usage.take()
                                && !has_reliable_call_accounting()
                            {
                                combine_response_usage(&mut resp, Some(usage));
                            }
                            if attempt > 0
                                || served_model != model
                                || model_slot != 0
                                || entry_index != 0
                                || context_truncated
                                || self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    != Some(provider_name)
                            {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "attempt": attempt, "original_model": model, "context_truncated": context_truncated})), "ModelProvider recovered (failover/retry)");
                                let primary = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.candidate_name())
                                    .unwrap_or("");
                                let primary_provider = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    .unwrap_or("");
                                let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                                    primary_provider,
                                    model,
                                    provider_name,
                                    served_model,
                                    model_slot != 0 || entry_index != 0,
                                    primary,
                                    entry.candidate_name(),
                                );
                                record_successful_provider_fallback(fallback_record.as_ref());
                                record_accepted_attempt(
                                    entry,
                                    current_model,
                                    fallback_record
                                        .as_ref()
                                        .map(ProviderFallbackRecord::attribution),
                                );
                            } else {
                                record_successful_provider_fallback(None);
                                record_accepted_attempt(entry, current_model, None);
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            if is_semantic_empty_completion_error(&e) {
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            final_cause_is_semantic_empty = false;
                            // Context window exceeded: truncate history and retry
                            if is_context_window_exceeded(&e) && !context_truncated {
                                let diagnostic = provider_error_diagnostic(&e);
                                push_failure(
                                    &mut failures,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    "context_window",
                                    Some(&diagnostic),
                                );
                                let dropped = truncate_for_context(&mut effective_messages);
                                if dropped > 0 {
                                    record_provider_context_truncation();
                                    context_truncated = true;
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "dropped": dropped, "remaining": effective_messages.len()})), "Context window exceeded; truncated history and retrying");
                                    continue; // Retry with truncated messages (counts as an attempt)
                                }
                                // No complete older turn can be removed safely.
                                let truncation_limit =
                                    context_truncation_limit(&effective_messages);
                                let context_error = context_failure_aggregate(
                                    &format!(
                                        "Request exceeds model context window and cannot be reduced without \
                                         breaking message/tool pairing ({truncation_limit}). Try using a model \
                                         with a larger context window, reducing the number of tools/skills, or \
                                         enabling compact_context in config."
                                    ),
                                    &failures,
                                );
                                return Err(reliable_terminal_error_with_cause(
                                    Some(entry.candidate_name()),
                                    failures,
                                    rejected_attempt_usage,
                                    false,
                                    Some(e),
                                )
                                .context(context_error));
                            }

                            let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                            let non_retryable = is_non_retryable(&e) || non_retryable_rate_limit;
                            let rate_limited = is_rate_limited(&e);
                            let failure_reason = failure_reason(rate_limited, non_retryable);
                            let error_detail = compact_error_detail(&e);
                            let diagnostic = provider_error_diagnostic(&e);
                            last_error_detail = Some(error_detail.clone());
                            last_diagnostic = Some(diagnostic.clone());

                            push_failure(
                                &mut failures,
                                attempt + 1,
                                self.max_retries + 1,
                                failure_reason,
                                Some(&diagnostic),
                            );

                            if rate_limited
                                && !non_retryable_rate_limit
                                && let Some(new_key) = self.rotate_key()
                            {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "error": error_detail})), &format!("Rate limited; key rotation selected key ending ...{} \
                                     but cannot apply (ModelProvider trait has no set_api_key). \
                                     Retrying with original key.", &new_key[new_key.len().saturating_sub(4)..]));
                            }

                            if non_retryable {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_failure_attrs(
                                            provider_name,
                                            served_model,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "Non-retryable error, moving on"
                                );
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if rate_limited && self.model_providers.len() > 1 {
                                self.cool_down_rate_limited_provider(entry, served_model, &e);
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if attempt < self.max_retries {
                                let wait = self.compute_backoff(backoff_ms, &e);
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_retry_attrs(
                                            provider_name,
                                            served_model,
                                            attempt + 1,
                                            wait,
                                            failure_reason,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "ModelProvider call failed, retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(wait)).await;
                                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                            }
                            final_cause = Some(e);
                            final_cause_provider = Some(entry.candidate_name().to_string());
                        }
                    }
                }

                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(provider_exhausted_attrs(
                            provider_name,
                            served_model,
                            last_error_detail.as_deref(),
                            last_diagnostic.as_ref(),
                        )),
                    "Exhausted retries, trying next model_provider/model"
                );
            }
        }

        Err(reliable_terminal_error_with_cause(
            final_cause_provider
                .as_deref()
                .or_else(|| self.configured_provider_identity()),
            failures,
            rejected_attempt_usage,
            final_cause_is_semantic_empty,
            final_cause,
        ))
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        mark_current_dispatch_composite();
        let models = self.model_chain(model);
        let mut failures = FailureEvents::default();
        let mut final_cause_is_semantic_empty = stream_recovery_was_semantic_empty();
        let mut effective_messages = request.messages.to_vec();
        let mut context_truncated = false;
        let mut rejected_attempt_usage = None;
        let mut final_cause = None;
        let mut final_cause_provider = None;

        for (model_slot, current_model) in models.iter().enumerate() {
            for (entry_index, entry) in self.model_providers.iter().enumerate() {
                if is_stream_recovery_skip(model_slot, entry_index) {
                    final_cause_provider = Some(entry.candidate_name().to_string());
                    continue;
                }
                let provider_name = entry.display_name.as_str();
                let served_model = entry.served_model(current_model);
                if self.provider_should_skip_for_cooldown(entry) {
                    self.log_cooldown_skip(provider_name, served_model);
                    Self::record_cooldown_skip_failure(&mut failures, self.max_retries + 1);
                    continue;
                }

                let mut backoff_ms = self.base_backoff_ms;
                let mut last_error_detail: Option<String> = None;
                let mut last_diagnostic: Option<ProviderErrorDiagnostic> = None;

                for attempt in 0..=self.max_retries {
                    let req = ChatRequest {
                        messages: &effective_messages,
                        tools: request.tools,
                        thinking: request.thinking,
                    };
                    match with_exact_dispatch_route(
                        entry.cooldown_key.clone(),
                        entry.served_model(current_model).to_string(),
                        ProviderDispatch::from_ref(entry.provider()).chat(
                            req,
                            current_model,
                            temperature,
                        ),
                    )
                    .await
                    {
                        Ok(mut resp) => {
                            if is_empty_completion(&resp) {
                                if let Some(usage) = resp.usage.clone()
                                    && !has_reliable_call_accounting()
                                {
                                    accumulate_usage(&mut rejected_attempt_usage, Some(&usage));
                                }
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            if let Some(usage) = rejected_attempt_usage.take()
                                && !has_reliable_call_accounting()
                            {
                                combine_response_usage(&mut resp, Some(usage));
                            }
                            if attempt > 0
                                || served_model != model
                                || model_slot != 0
                                || entry_index != 0
                                || context_truncated
                                || self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    != Some(provider_name)
                            {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "attempt": attempt, "original_model": model, "context_truncated": context_truncated})), "ModelProvider recovered (failover/retry)");
                                let primary = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.candidate_name())
                                    .unwrap_or("");
                                let primary_provider = self
                                    .model_providers
                                    .first()
                                    .map(|entry| entry.display_name.as_str())
                                    .unwrap_or("");
                                let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                                    primary_provider,
                                    model,
                                    provider_name,
                                    served_model,
                                    model_slot != 0 || entry_index != 0,
                                    primary,
                                    entry.candidate_name(),
                                );
                                record_successful_provider_fallback(fallback_record.as_ref());
                                record_accepted_attempt(
                                    entry,
                                    current_model,
                                    fallback_record
                                        .as_ref()
                                        .map(ProviderFallbackRecord::attribution),
                                );
                            } else {
                                record_successful_provider_fallback(None);
                                record_accepted_attempt(entry, current_model, None);
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            if is_semantic_empty_completion_error(&e) {
                                if attempt < self.max_retries {
                                    self.backoff_after_empty_completion(
                                        &mut failures,
                                        provider_name,
                                        served_model,
                                        attempt,
                                        &mut backoff_ms,
                                    )
                                    .await;
                                    continue;
                                }
                                self.record_empty_completion_failure(
                                    &mut failures,
                                    provider_name,
                                    served_model,
                                    attempt,
                                    false,
                                );
                                final_cause_is_semantic_empty = true;
                                break;
                            }
                            final_cause_is_semantic_empty = false;
                            // Context window exceeded: truncate history and retry
                            if is_context_window_exceeded(&e) && !context_truncated {
                                let diagnostic = provider_error_diagnostic(&e);
                                push_failure(
                                    &mut failures,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    "context_window",
                                    Some(&diagnostic),
                                );
                                let dropped = truncate_for_context(&mut effective_messages);
                                if dropped > 0 {
                                    record_provider_context_truncation();
                                    context_truncated = true;
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "model": served_model, "dropped": dropped, "remaining": effective_messages.len()})), "Context window exceeded; truncated history and retrying");
                                    continue; // Retry with truncated messages (counts as an attempt)
                                }
                                // No complete older turn can be removed safely.
                                let truncation_limit =
                                    context_truncation_limit(&effective_messages);
                                let context_error = context_failure_aggregate(
                                    &format!(
                                        "Request exceeds model context window and cannot be reduced without \
                                         breaking message/tool pairing ({truncation_limit}). Try using a model \
                                         with a larger context window, reducing the number of tools/skills, or \
                                         enabling compact_context in config."
                                    ),
                                    &failures,
                                );
                                return Err(reliable_terminal_error_with_cause(
                                    Some(entry.candidate_name()),
                                    failures,
                                    rejected_attempt_usage,
                                    false,
                                    Some(e),
                                )
                                .context(context_error));
                            }

                            let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                            let non_retryable = is_non_retryable(&e) || non_retryable_rate_limit;
                            let rate_limited = is_rate_limited(&e);
                            let failure_reason = failure_reason(rate_limited, non_retryable);
                            let error_detail = compact_error_detail(&e);
                            let diagnostic = provider_error_diagnostic(&e);
                            last_error_detail = Some(error_detail.clone());
                            last_diagnostic = Some(diagnostic.clone());

                            push_failure(
                                &mut failures,
                                attempt + 1,
                                self.max_retries + 1,
                                failure_reason,
                                Some(&diagnostic),
                            );

                            if rate_limited
                                && !non_retryable_rate_limit
                                && let Some(new_key) = self.rotate_key()
                            {
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": provider_name, "error": error_detail})), &format!("Rate limited; key rotation selected key ending ...{} \
                                     but cannot apply (ModelProvider trait has no set_api_key). \
                                     Retrying with original key.", &new_key[new_key.len().saturating_sub(4)..]));
                            }

                            if non_retryable {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_failure_attrs(
                                            provider_name,
                                            served_model,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "Non-retryable error, moving on"
                                );
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if rate_limited && self.model_providers.len() > 1 {
                                self.cool_down_rate_limited_provider(entry, served_model, &e);
                                final_cause = Some(e);
                                final_cause_provider = Some(entry.candidate_name().to_string());
                                break;
                            }

                            if attempt < self.max_retries {
                                let wait = self.compute_backoff(backoff_ms, &e);
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        provider_retry_attrs(
                                            provider_name,
                                            served_model,
                                            attempt + 1,
                                            wait,
                                            failure_reason,
                                            &error_detail,
                                            &diagnostic,
                                        )
                                    ),
                                    "ModelProvider call failed, retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(wait)).await;
                                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                            }
                            final_cause = Some(e);
                            final_cause_provider = Some(entry.candidate_name().to_string());
                        }
                    }
                }

                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(provider_exhausted_attrs(
                            provider_name,
                            served_model,
                            last_error_detail.as_deref(),
                            last_diagnostic.as_ref(),
                        )),
                    "Exhausted retries, trying next model_provider/model"
                );
            }

            if *current_model != model {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"original_model": model, "fallback_model": *current_model})), "Model fallback exhausted all model_providers, trying next fallback model");
            }
        }

        Err(reliable_terminal_error_with_cause(
            final_cause_provider
                .as_deref()
                .or_else(|| self.configured_provider_identity()),
            failures,
            rejected_attempt_usage,
            final_cause_is_semantic_empty,
            final_cause,
        ))
    }

    fn supports_streaming(&self) -> bool {
        self.model_providers
            .iter()
            .any(|entry| entry.provider().supports_streaming())
    }

    fn supports_streaming_tool_events(&self) -> bool {
        self.model_providers
            .iter()
            .any(|entry| entry.provider().supports_streaming_tool_events())
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        mark_current_dispatch_composite();
        let needs_tool_events = request.tools.is_some_and(|tools| !tools.is_empty());

        for (entry_index, entry) in self.model_providers.iter().enumerate() {
            let provider_name = entry.display_name.as_str();
            let model_provider = entry.provider();
            if !model_provider.supports_streaming() || !options.enabled {
                continue;
            }

            if needs_tool_events && !model_provider.supports_streaming_tool_events() {
                continue;
            }

            if self.provider_should_skip_for_cooldown(entry) {
                self.log_cooldown_skip(provider_name, entry.served_model(model));
                continue;
            }

            let current_model = self
                .model_chain(model)
                .first()
                .copied()
                .unwrap_or(model)
                .to_string();
            let served_model = entry.served_model(&current_model).to_string();
            let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                self.model_providers
                    .first()
                    .map(|entry| entry.display_name.as_str())
                    .unwrap_or(""),
                model,
                provider_name,
                &served_model,
                entry_index != 0,
                self.model_providers
                    .first()
                    .map(|entry| entry.candidate_name())
                    .unwrap_or(""),
                entry.candidate_name(),
            );
            let req = ChatRequest {
                messages: request.messages,
                tools: request.tools,
                thinking: request.thinking,
            };
            let stream = stream_with_exact_dispatch_route(
                entry.cooldown_key.clone(),
                served_model.clone(),
                ProviderDispatch::from_ref(model_provider).stream_chat(
                    req,
                    &current_model,
                    temperature,
                    options,
                ),
            );
            let stream = stream_with_recovery_identity(stream, 0, entry_index);
            let accepted_route = AcceptedRoute::new(
                entry.cooldown_key.clone(),
                served_model.clone(),
                fallback_record
                    .as_ref()
                    .map(ProviderFallbackRecord::attribution),
            );
            // Usage is billing metadata, not stream acceptance. A provider can
            // report usage and then fail before Final; recording the fallback
            // at Usage would leak a route that never produced an accepted
            // completion to legacy direct callers.
            return stream_with_success_recording(
                stream,
                fallback_record,
                accepted_route,
                |event| matches!(event, StreamEvent::Final),
            );
        }

        let message = if needs_tool_events {
            "No model_provider supports streaming tool events".to_string()
        } else {
            "No model_provider supports streaming".to_string()
        };
        stream_as_dispatch_composite(
            stream::once(async move { Err(super::traits::StreamError::ModelProvider(message)) })
                .boxed(),
        )
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        mark_current_dispatch_composite();
        // Try each model_provider/model combination for streaming
        // For streaming, we use the first model_provider that supports it and has streaming enabled
        for (provider_index, entry) in self.model_providers.iter().enumerate() {
            let provider_name = entry.display_name.as_str();
            let model_provider = entry.provider();
            if !model_provider.supports_streaming() || !options.enabled {
                continue;
            }

            if self.provider_should_skip_for_cooldown(entry) {
                self.log_cooldown_skip(provider_name, entry.served_model(model));
                continue;
            }

            // Clone model_provider data for the stream
            // Try the first model in the chain for streaming
            let current_model = match self.model_chain(model).first() {
                Some(m) => (*m).to_string(),
                None => model.to_string(),
            };
            let served_model = entry.served_model(&current_model).to_string();
            let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                self.model_providers
                    .first()
                    .map(|entry| entry.display_name.as_str())
                    .unwrap_or(""),
                model,
                provider_name,
                &served_model,
                provider_index != 0,
                self.model_providers
                    .first()
                    .map(|entry| entry.candidate_name())
                    .unwrap_or(""),
                entry.candidate_name(),
            );

            // For streaming, we attempt once and propagate errors
            // The caller can retry the entire request if needed
            let stream = stream_with_exact_dispatch_route(
                entry.cooldown_key.clone(),
                served_model.clone(),
                ProviderDispatch::from_ref(model_provider).stream_chat_with_system(
                    system_prompt,
                    message,
                    &current_model,
                    temperature,
                    options,
                ),
            );
            let accepted_route = AcceptedRoute::new(
                entry.cooldown_key.clone(),
                served_model.clone(),
                fallback_record
                    .as_ref()
                    .map(ProviderFallbackRecord::attribution),
            );

            return stream_with_success_recording(
                stream,
                fallback_record,
                accepted_route,
                |chunk| chunk.is_final,
            );
        }

        // No streaming support available
        stream_as_dispatch_composite(
            stream::once(async move {
                Err(super::traits::StreamError::ModelProvider(
                    "No model_provider supports streaming".to_string(),
                ))
            })
            .boxed(),
        )
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        mark_current_dispatch_composite();
        // Try each model_provider/model combination for streaming with history.
        // Mirrors stream_chat_with_system but delegates to the underlying
        // model_provider's stream_chat_with_history, preserving the full conversation.
        for (provider_index, entry) in self.model_providers.iter().enumerate() {
            let provider_name = entry.display_name.as_str();
            let model_provider = entry.provider();
            if !model_provider.supports_streaming() || !options.enabled {
                continue;
            }

            if self.provider_should_skip_for_cooldown(entry) {
                self.log_cooldown_skip(provider_name, entry.served_model(model));
                continue;
            }

            let current_model = match self.model_chain(model).first() {
                Some(m) => (*m).to_string(),
                None => model.to_string(),
            };
            let served_model = entry.served_model(&current_model).to_string();
            let fallback_record = ProviderFallbackRecord::new_if_true_fallback(
                self.model_providers
                    .first()
                    .map(|entry| entry.display_name.as_str())
                    .unwrap_or(""),
                model,
                provider_name,
                &served_model,
                provider_index != 0,
                self.model_providers
                    .first()
                    .map(|entry| entry.candidate_name())
                    .unwrap_or(""),
                entry.candidate_name(),
            );

            let stream = stream_with_exact_dispatch_route(
                entry.cooldown_key.clone(),
                served_model.clone(),
                ProviderDispatch::from_ref(model_provider).stream_chat_with_history(
                    messages,
                    &current_model,
                    temperature,
                    options,
                ),
            );
            let accepted_route = AcceptedRoute::new(
                entry.cooldown_key.clone(),
                served_model.clone(),
                fallback_record
                    .as_ref()
                    .map(ProviderFallbackRecord::attribution),
            );

            return stream_with_success_recording(
                stream,
                fallback_record,
                accepted_route,
                |chunk| chunk.is_final,
            );
        }

        // No streaming support available
        stream_as_dispatch_composite(
            stream::once(async move {
                Err(super::traits::StreamError::ModelProvider(
                    "No model_provider supports streaming".to_string(),
                ))
            })
            .boxed(),
        )
    }
}

impl ::zeroclaw_api::attribution::Attributable for ReliableModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        match self.model_providers.first() {
            Some(entry) => ::zeroclaw_api::attribution::Attributable::role(entry.provider()),
            None => ::zeroclaw_api::attribution::Role::System,
        }
    }

    fn alias(&self) -> &str {
        // Delegate to the primary inner provider for the same reason
        // as `role()`. Falls back to the wrapper's own configured alias
        // when no inner provider is registered.
        match self.model_providers.first() {
            Some(entry) => ::zeroclaw_api::attribution::Attributable::alias(entry.provider()),
            None => &self.alias,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::AnthropicModelProvider;
    use crate::router::{Route, RouterModelProvider};
    use futures_util::StreamExt;
    use std::sync::Arc;
    use zeroclaw_api::tool::ToolSpec;

    fn drain_captured_events(
        rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
    ) -> Vec<serde_json::Value> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn assert_captured_model(
        events: &[serde_json::Value],
        message: &str,
        provider: &str,
        served_model: &str,
    ) {
        let matching: Vec<_> = events
            .iter()
            .filter(|event| {
                event.get("message").and_then(|value| value.as_str()) == Some(message)
                    && event
                        .get("attributes")
                        .and_then(|attrs| attrs.get("model_provider"))
                        .and_then(|value| value.as_str())
                        == Some(provider)
            })
            .collect();
        assert!(
            !matching.is_empty(),
            "missing structured log for {provider}: {message}"
        );

        for event in matching {
            let attrs = event
                .get("attributes")
                .expect("structured log must include attributes");
            assert_eq!(
                attrs.get("model").and_then(|value| value.as_str()),
                Some(served_model),
                "wrong model attribution in {message}: {attrs}"
            );
        }
    }

    struct MockModelProvider {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response: &'static str,
        error: &'static str,
    }

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        fn has_stable_request_identity(&self, _model: &str) -> bool {
            true
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(self.error);
            }
            Ok(self.response.to_string())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(self.error);
            }
            Ok(self.response.to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    /// Mock that records which model was used for each call.
    struct ModelAwareMock {
        calls: Arc<AtomicUsize>,
        models_seen: parking_lot::Mutex<Vec<String>>,
        fail_models: Vec<&'static str>,
        response: &'static str,
    }

    #[async_trait]
    impl ModelProvider for ModelAwareMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.models_seen.lock().push(model.to_string());
            if self.fail_models.contains(&model) {
                anyhow::bail!("500 model {} unavailable", model);
            }
            Ok(self.response.to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ModelAwareMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ModelAwareMock"
        }
    }

    // ── Existing tests (preserved) ──

    #[tokio::test]
    async fn succeeds_without_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "boom",
                }),
            )],
            2,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    response: "recovered",
                    error: "temporary",
                }),
            )],
            2,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn falls_back_after_retries_exhausted() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary down",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from fallback",
                        error: "fallback down",
                    }),
                ),
            ],
            1,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "from fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn request_identity_is_unstable_when_provider_failover_is_possible() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: calls.clone(),
                        fail_until_attempt: 0,
                        response: "primary",
                        error: "unused",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls,
                        fail_until_attempt: 0,
                        response: "fallback",
                        error: "unused",
                    }),
                ),
            ],
            0,
            1,
        );

        assert!(!model_provider.has_stable_request_identity("model"));
    }

    #[test]
    fn request_identity_is_unstable_when_model_fallback_is_configured() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "primary",
                    error: "unused",
                }),
            )],
            0,
            1,
        )
        .with_model_fallbacks(HashMap::from([(
            "model".to_string(),
            vec!["fallback-model".to_string()],
        )]));

        assert!(!model_provider.has_stable_request_identity("model"));
    }

    #[test]
    fn single_provider_request_identity_remains_stable() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "primary",
                    error: "unused",
                }),
            )],
            0,
            1,
        );

        assert!(model_provider.has_stable_request_identity("model"));
    }

    #[test]
    fn pinned_request_identity_is_stable_only_without_a_model_remap() {
        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![ReliableModelProviderEntry::new_pinned(
                "primary",
                "primary.default",
                "primary.default",
                "pinned-model",
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "primary",
                    error: "unused",
                }),
            )],
            0,
            1,
        );

        assert!(model_provider.has_stable_request_identity("pinned-model"));
        assert!(!model_provider.has_stable_request_identity("requested-model"));
    }

    /// A `fallback_models` downgrade uses model-PINNED entries on one
    /// provider: the model swap happens inside `ModelPinnedProvider`, so the
    /// failover loop must read the entry's pinned model to record the
    /// downgrade. Regression test for the case where the requested model's
    /// entry fails and a sibling entry pinned to another model serves the
    /// turn: the recorded fallback must name the SERVED model.
    #[tokio::test]
    async fn pinned_model_fallback_records_served_model() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![
                ReliableModelProviderEntry::new_pinned(
                    "openai",
                    "openai.mock",
                    "mock",
                    "model-primary",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary model down",
                    }),
                ),
                ReliableModelProviderEntry::new_pinned(
                    "openai",
                    "openai.mock",
                    "mock",
                    "model-served",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from pinned fallback",
                        error: "unused",
                    }),
                ),
            ],
            0,
            1,
        );

        let (result, fallback) = scope_provider_fallback(async {
            let result = model_provider
                .simple_chat("hello", "model-primary", Some(0.0))
                .await;
            (result, take_last_provider_fallback())
        })
        .await;

        assert_eq!(result.unwrap(), "from pinned fallback");
        let fallback = fallback.expect("pinned-model downgrade must be recorded");
        assert_eq!(fallback.requested_model, "model-primary");
        assert_eq!(
            fallback.actual_model, "model-served",
            "the record must carry the model the pinned entry actually served"
        );
        assert_eq!(fallback.requested_provider, fallback.actual_provider);
    }

    /// The requested model served by its own pinned entry (attempt 0, first
    /// entry) is not a fallback — nothing may be recorded.
    #[tokio::test]
    async fn pinned_primary_success_records_nothing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![ReliableModelProviderEntry::new_pinned(
                "openai",
                "openai.mock",
                "mock",
                "model-primary",
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "unused",
                }),
            )],
            0,
            1,
        );

        let (result, fallback) = scope_provider_fallback(async {
            let result = model_provider
                .simple_chat("hello", "model-primary", Some(0.0))
                .await;
            (result, take_last_provider_fallback())
        })
        .await;

        assert_eq!(result.unwrap(), "ok");
        assert!(
            fallback.is_none(),
            "primary pinned entry serving the requested model is not a fallback"
        );
    }

    /// A pinned entry's physical model belongs in operator diagnostics while
    /// the terminal aggregate remains bounded and free of route identity.
    #[tokio::test]
    async fn pinned_empty_completion_logs_served_model_but_aggregate_stays_safe() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![ReliableModelProviderEntry::new_pinned(
                "gemini",
                "gemini.mock",
                "mock",
                "gemini-3-flash",
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: usize::MAX,
                    response: "never",
                }),
            )],
            1,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let err = model_provider
            .chat(request, "deepseek-v4-flash", Some(0.0))
            .await
            .expect_err("a pinned entry stuck on empty completions must not succeed");
        let events = drain_captured_events(&mut rx);
        zeroclaw_log::clear_broadcast_hook();

        let text = err.to_string();
        assert!(text.contains("event 1 (retry 1/2): empty_response"));
        assert!(
            !text.contains("gemini"),
            "aggregate leaked provider identity: {text}"
        );
        assert!(
            !text.contains("gemini-3-flash") && !text.contains("deepseek-v4-flash"),
            "aggregate leaked model identity: {text}"
        );
        assert_captured_model(
            &events,
            "Empty completion; retrying",
            "gemini",
            "gemini-3-flash",
        );
        assert_captured_model(
            &events,
            "Empty completion; retries exhausted",
            "gemini",
            "gemini-3-flash",
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pinned_retry_and_exhaustion_logs_use_served_model() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![ReliableModelProviderEntry::new_pinned(
                "gemini",
                "gemini.mock",
                "mock",
                "gemini-3-flash",
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "temporary provider outage",
                }),
            )],
            1,
            1,
        );

        let err = model_provider
            .chat_with_system(None, "hello", "deepseek-v4-flash", Some(0.0))
            .await
            .expect_err("a pinned entry stuck on a retryable error must not succeed");
        let events = drain_captured_events(&mut rx);
        zeroclaw_log::clear_broadcast_hook();

        let text = err.to_string();
        assert!(
            !text.contains("gemini")
                && !text.contains("gemini-3-flash")
                && !text.contains("deepseek-v4-flash")
                && !text.contains("temporary provider outage"),
            "aggregate leaked physical attempt details: {text}"
        );
        assert_captured_model(
            &events,
            "ModelProvider call failed, retrying",
            "gemini",
            "gemini-3-flash",
        );
        assert_captured_model(
            &events,
            "Exhausted retries, trying next model_provider/model",
            "gemini",
            "gemini-3-flash",
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pinned_non_retryable_log_uses_served_model() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![ReliableModelProviderEntry::new_pinned(
                "gemini",
                "gemini.mock",
                "mock",
                "gemini-3-flash",
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "HTTP 400 invalid request",
                }),
            )],
            2,
            1,
        );

        let err = model_provider
            .chat_with_system(None, "hello", "deepseek-v4-flash", Some(0.0))
            .await
            .expect_err("a pinned entry with a non-retryable error must fail");
        let events = drain_captured_events(&mut rx);
        zeroclaw_log::clear_broadcast_hook();

        let text = err.to_string();
        assert!(!text.contains("HTTP 400 invalid request"));
        assert!(!text.contains("gemini-3-flash"));
        assert_captured_model(
            &events,
            "Non-retryable error, moving on",
            "gemini",
            "gemini-3-flash",
        );
        assert_captured_model(
            &events,
            "Exhausted retries, trying next model_provider/model",
            "gemini",
            "gemini-3-flash",
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pinned_context_truncation_log_uses_served_model() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![ReliableModelProviderEntry::new_pinned(
                "gemini",
                "gemini.mock",
                "mock",
                "gemini-3-flash",
                Box::new(ContextOverflowMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    post_context_error: None,
                    message_counts: parking_lot::Mutex::new(Vec::new()),
                }),
            )],
            1,
            1,
        );
        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("old request"),
            ChatMessage::assistant("old response"),
            ChatMessage::user("current request"),
        ];

        let result = model_provider
            .chat_with_history(&messages, "deepseek-v4-flash", Some(0.0))
            .await
            .expect("the truncated retry should recover");
        let events = drain_captured_events(&mut rx);
        zeroclaw_log::clear_broadcast_hook();

        assert_eq!(result, "recovered after truncation");
        assert_captured_model(
            &events,
            "Context window exceeded; truncated history and retrying",
            "gemini",
            "gemini-3-flash",
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pinned_rate_limit_and_cooldown_logs_use_each_served_model() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let rate_limited_calls = Arc::new(AtomicUsize::new(0));
        let skipped_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "mock",
            vec![
                ReliableModelProviderEntry::new_pinned(
                    "gemini",
                    "gemini.mock",
                    "mock",
                    "gemini-3-flash",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&rate_limited_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "HTTP 429 Too Many Requests, Retry-After: 30",
                    }),
                ),
                ReliableModelProviderEntry::new_pinned(
                    "gemini",
                    "gemini.mock",
                    "mock",
                    "gemini-2-flash",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&skipped_calls),
                        fail_until_attempt: 0,
                        response: "should be skipped",
                        error: "unused",
                    }),
                ),
                ReliableModelProviderEntry::new_pinned(
                    "anthropic",
                    "anthropic.mock",
                    "mock",
                    "claude-fallback",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "recovered",
                        error: "unused",
                    }),
                ),
            ],
            1,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "deepseek-v4-flash", Some(0.0))
            .await
            .expect("the downstream fallback should recover");
        let events = drain_captured_events(&mut rx);
        zeroclaw_log::clear_broadcast_hook();

        assert_eq!(result, "recovered");
        assert_captured_model(
            &events,
            "ModelProvider rate-limited; trying next provider",
            "gemini",
            "gemini-3-flash",
        );
        assert_captured_model(
            &events,
            "Skipping model_provider during rate-limit cooldown",
            "gemini",
            "gemini-2-flash",
        );
        assert_eq!(rate_limited_calls.load(Ordering::SeqCst), 1);
        assert_eq!(skipped_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    /// Returns an empty completion (blank `chat_with_system` text, which the
    /// default `chat`/`chat_with_tools`/`chat_with_history` impls surface as a
    /// blank `ChatResponse`) for the first `empty_until_attempt` calls, then a
    /// non-empty response. Counts total calls so tests can assert re-rolls.
    struct EmptyThenTextMock {
        calls: Arc<AtomicUsize>,
        empty_until_attempt: usize,
        response: &'static str,
    }

    struct UsageEmptyThenTextMock {
        calls: Arc<AtomicUsize>,
    }

    struct UsagePersistentEmptyMock {
        calls: Arc<AtomicUsize>,
    }

    struct ContextWindowErrorMock;

    struct TerminalUsageErrorMock;

    #[derive(Debug)]
    struct ContextWindowTypedError;

    impl std::fmt::Display for ContextWindowTypedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("input exceeds the context window")
        }
    }

    impl std::error::Error for ContextWindowTypedError {}

    #[derive(Debug)]
    struct TerminalProviderTypedError;

    impl std::fmt::Display for TerminalProviderTypedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("typed terminal provider failure")
        }
    }

    impl std::error::Error for TerminalProviderTypedError {}

    fn terminal_usage_error() -> anyhow::Error {
        anyhow::Error::new(ReliableRejectedCompletionUsage::with_terminal_cause(
            TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cached_input_tokens: None,
            },
            FailureEvents::default(),
            anyhow::Error::new(TerminalProviderTypedError),
        ))
    }

    struct ThinkOnlyThenTextMock {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for EmptyThenTextMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.empty_until_attempt {
                Ok(String::new())
            } else {
                Ok(self.response.to_string())
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for EmptyThenTextMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "EmptyThenTextMock"
        }
    }

    #[async_trait]
    impl ModelProvider for UsageEmptyThenTextMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: (attempt > 0).then(|| "recovered".to_string()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            })
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: (attempt > 0).then(|| "recovered".to_string()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for UsageEmptyThenTextMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "UsageEmptyThenTextMock"
        }
    }

    #[async_trait]
    impl ModelProvider for UsagePersistentEmptyMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: None,
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            })
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: None,
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for UsagePersistentEmptyMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "UsagePersistentEmptyMock"
        }
    }

    #[async_trait]
    impl ModelProvider for ContextWindowErrorMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow::Error::new(ContextWindowTypedError))
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow::Error::new(ContextWindowTypedError))
        }
    }

    #[async_trait]
    impl ModelProvider for TerminalUsageErrorMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Err(terminal_usage_error())
        }

        async fn chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Err(terminal_usage_error())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Err(terminal_usage_error())
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Err(terminal_usage_error())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for TerminalUsageErrorMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "TerminalUsageErrorMock"
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ContextWindowErrorMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ContextWindowErrorMock"
        }
    }

    #[async_trait]
    impl ModelProvider for ThinkOnlyThenTextMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(if attempt == 0 {
                "<think>internal reasoning</think>".to_string()
            } else {
                "recovered".to_string()
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ThinkOnlyThenTextMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ThinkOnlyThenTextMock"
        }
    }

    fn persistent_empty_reliable(calls: Arc<AtomicUsize>) -> ReliableModelProvider {
        ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls,
                    empty_until_attempt: usize::MAX,
                    response: "never",
                }),
            )],
            0,
            1,
        )
    }

    fn semantic_empty_then_provider_error_reliable() -> ReliableModelProvider {
        ReliableModelProvider::new(
            "test",
            vec![
                (
                    "empty".into(),
                    Box::new(EmptyThenTextMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        empty_until_attempt: usize::MAX,
                        response: "never",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "failure".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "upstream terminal provider failure",
                    }),
                ),
            ],
            0,
            1,
        )
    }

    #[tokio::test]
    async fn chat_retries_empty_completion_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: 1,
                    response: "recovered",
                }),
            )],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("recovered"));
        // One empty completion + one successful re-roll.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_retry_preserves_usage_from_rejected_empty_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(UsageEmptyThenTextMock {
                    calls: Arc::clone(&calls),
                }),
            )],
            1,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let response = model_provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await
            .expect("second attempt succeeds");

        let usage = response.usage.expect("combined usage is retained");
        assert_eq!(usage.input_tokens, Some(20));
        assert_eq!(usage.output_tokens, Some(10));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn accounted_chat_keeps_rejected_usage_out_of_accepted_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(UsageEmptyThenTextMock {
                    calls: Arc::clone(&calls),
                }),
            )],
            1,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let accounted = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await
            .expect("second attempt succeeds");

        let accepted = accounted.response.usage.expect("accepted usage");
        assert_eq!(accepted.input_tokens, Some(10));
        assert_eq!(accepted.output_tokens, Some(5));
        let rejected = accounted
            .rejected_attempt_usage
            .expect("rejected usage sidecar");
        assert_eq!(rejected.input_tokens, Some(10));
        assert_eq!(rejected.output_tokens, Some(5));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn accounted_chat_with_tools_keeps_rejected_usage_out_of_accepted_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(UsageEmptyThenTextMock {
                    calls: Arc::clone(&calls),
                }),
            )],
            1,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let accounted = ProviderDispatch::from_ref(&model_provider)
            .chat_with_tools_accounted(&messages, &[], "test", Some(0.0))
            .await
            .expect("second attempt succeeds");

        let accepted = accounted.response.usage.expect("accepted usage");
        assert_eq!(accepted.input_tokens, Some(10));
        assert_eq!(accepted.output_tokens, Some(5));
        let rejected = accounted
            .rejected_attempt_usage
            .expect("rejected usage sidecar");
        assert_eq!(rejected.input_tokens, Some(10));
        assert_eq!(rejected.output_tokens, Some(5));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn accounted_primary_success_exposes_configured_provider_ref_and_served_model() {
        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![ReliableModelProviderEntry::new_pinned(
                "primary-display",
                "openai.primary",
                "primary-alias",
                "served-model",
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "unused",
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "requested-model",
                Some(0.0),
            )
            .await;

        assert!(outcome.result.is_ok());
        let route = outcome
            .accounting
            .accepted_route()
            .cloned()
            .expect("accepted Reliable call must retain its configured route");
        assert_eq!(route.provider_ref(), "openai.primary");
        assert_eq!(route.model(), "served-model");
        assert!(
            route.fallback().is_some(),
            "pinned model is a visible recovery"
        );
    }

    #[tokio::test]
    async fn accounted_failover_keeps_rejected_and_accepted_configured_routes_distinct() {
        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new_pinned(
                    "primary-display",
                    "openai.primary",
                    "primary-alias",
                    "model-a",
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                ReliableModelProviderEntry::new_pinned(
                    "backup-display",
                    "anthropic.backup",
                    "backup-alias",
                    "model-b",
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response: "recovered",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "requested-model",
                Some(0.0),
            )
            .await;

        assert_eq!(
            outcome
                .result
                .expect("backup response succeeds")
                .response
                .text
                .as_deref(),
            Some("recovered")
        );
        assert_eq!(outcome.accounting.rejected_attempts().len(), 1);
        let rejected = &outcome.accounting.rejected_attempts()[0];
        assert_eq!(rejected.provider_ref(), "openai.primary");
        assert_eq!(rejected.model(), "model-a");
        let accepted = outcome
            .accounting
            .accepted_route()
            .cloned()
            .expect("fallback must retain its actual configured route");
        assert_eq!(accepted.provider_ref(), "anthropic.backup");
        assert_eq!(accepted.model(), "model-b");
    }

    /// Physical attempts must retain their served model even when two pinned
    /// entries share the same configured provider alias. The route string is
    /// insufficient identity for billing a model downgrade.
    #[tokio::test]
    async fn accounted_same_configured_provider_keeps_pinned_models_distinct() {
        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new_pinned(
                    "shared-display",
                    "openai.shared",
                    "shared-alias",
                    "model-a",
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                ReliableModelProviderEntry::new_pinned(
                    "shared-display",
                    "openai.shared",
                    "shared-alias",
                    "model-b",
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response: "recovered",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "requested-model",
                Some(0.0),
            )
            .await;

        assert!(outcome.result.is_ok());
        assert_eq!(outcome.accounting.rejected_attempts().len(), 1);
        let rejected = &outcome.accounting.rejected_attempts()[0];
        assert_eq!(rejected.provider_ref(), "openai.shared");
        assert_eq!(rejected.model(), "model-a");
        let accepted = outcome
            .accounting
            .accepted_route()
            .cloned()
            .expect("fallback must retain its actual pinned model");
        assert_eq!(accepted.provider_ref(), "openai.shared");
        assert_eq!(accepted.model(), "model-b");
    }

    #[tokio::test]
    async fn accounted_report_excludes_provider_error_and_response_content() {
        const ERROR_SENTINEL: &str = "S9470_PROVIDER_ERROR_BODY";
        const RESPONSE_SENTINEL: &str = "S9470_PROVIDER_RESPONSE_BODY";
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "unused",
                        error: ERROR_SENTINEL,
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response: RESPONSE_SENTINEL,
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("request text must not enter the report")];
        let outcome = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "model",
                Some(0.0),
            )
            .await;

        assert_eq!(
            outcome
                .result
                .expect("fallback response succeeds")
                .response
                .text
                .as_deref(),
            Some(RESPONSE_SENTINEL)
        );
        let report = format!("{:?}", outcome.accounting);
        for secret in [
            ERROR_SENTINEL,
            RESPONSE_SENTINEL,
            "request text must not enter the report",
        ] {
            assert!(
                !report.contains(secret),
                "accounting report leaked {secret}: {report}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_accounted_calls_keep_attempt_reports_task_local() {
        let make_provider = |provider_ref: &str, served_model: &str| {
            ReliableModelProvider::new_with_entries(
                "test",
                vec![ReliableModelProviderEntry::new_pinned(
                    provider_ref,
                    provider_ref,
                    provider_ref,
                    served_model,
                    Box::new(UsageEmptyThenTextMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                )],
                1,
                1,
            )
        };
        let first = make_provider("openai.first", "model-first");
        let second = make_provider("anthropic.second", "model-second");
        let messages = vec![ChatMessage::user("hello")];
        let first_dispatch = ProviderDispatch::from_ref(&first);
        let second_dispatch = ProviderDispatch::from_ref(&second);

        let (first, second) = tokio::join!(
            first_dispatch.chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "requested-first",
                Some(0.0),
            ),
            second_dispatch.chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "requested-second",
                Some(0.0),
            ),
        );

        for (outcome, provider_ref, model) in [
            (first, "openai.first", "model-first"),
            (second, "anthropic.second", "model-second"),
        ] {
            assert!(outcome.result.is_ok());
            assert_eq!(outcome.accounting.rejected_attempts().len(), 1);
            let rejected = &outcome.accounting.rejected_attempts()[0];
            assert_eq!(rejected.provider_ref(), provider_ref);
            assert_eq!(rejected.model(), model);
            let accepted = outcome
                .accounting
                .accepted_route()
                .cloned()
                .expect("each task keeps its own accepted route");
            assert_eq!(accepted.provider_ref(), provider_ref);
            assert_eq!(accepted.model(), model);
        }
    }

    #[tokio::test]
    async fn accounted_chat_outcome_retains_rejected_attempts_on_error() {
        let messages = vec![ChatMessage::user("hello")];
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "empty".into(),
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "failure".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "terminal provider failure",
                    }),
                ),
            ],
            0,
            1,
        );

        let outcome = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await;

        assert!(outcome.result.is_err());
        assert_eq!(outcome.accounting.rejected_attempts().len(), 1);
        let rejected = &outcome.accounting.rejected_attempts()[0];
        assert_eq!(rejected.provider_ref(), "empty");
        assert_eq!(rejected.model(), "test");
        assert_eq!(rejected.usage().input_tokens, Some(10));
        assert_eq!(rejected.usage().output_tokens, Some(5));
    }

    #[tokio::test]
    async fn terminal_error_usage_is_accounted_at_the_dispatch_boundary() {
        let messages = vec![ChatMessage::user("hello")];
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "configured.primary".into(),
                Box::new(TerminalUsageErrorMock) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );

        let outcome = ProviderDispatch::from_ref(&provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "served-model",
                Some(0.0),
            )
            .await;
        assert_terminal_usage_accounting(
            outcome.result.map(|response| response.response),
            outcome.accounting,
        );
    }

    #[tokio::test]
    async fn nested_reliable_terminal_usage_is_not_double_counted() {
        let messages = vec![ChatMessage::user("hello")];
        let inner = ReliableModelProvider::new(
            "inner",
            vec![
                (
                    "inner.first".into(),
                    Box::new(TerminalUsageErrorMock) as Box<dyn ModelProvider>,
                ),
                (
                    "inner.second".into(),
                    Box::new(TerminalUsageErrorMock) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let outer = ReliableModelProvider::new(
            "outer",
            vec![(
                "outer.wrapper".into(),
                Box::new(inner) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );

        let outcome = ProviderDispatch::from_ref(&outer)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "served-model",
                Some(0.0),
            )
            .await;

        assert!(outcome.result.is_err());
        let rejected = outcome.accounting.rejected_attempts();
        assert_eq!(rejected.len(), 2);
        assert_eq!(rejected[0].provider_ref(), "inner.first");
        assert_eq!(rejected[1].provider_ref(), "inner.second");
        for attempt in rejected {
            assert_eq!(attempt.model(), "served-model");
            assert_eq!(attempt.usage().input_tokens, Some(10));
            assert_eq!(attempt.usage().output_tokens, Some(5));
        }
    }

    #[tokio::test]
    async fn reliable_then_router_reports_the_router_selected_leaf() {
        let router = RouterModelProvider::new(
            "router",
            vec![(
                "configured.leaf".to_string(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "unused",
                }) as Box<dyn ModelProvider>,
            )],
            vec![(
                "fast".to_string(),
                Route {
                    provider_name: "configured.leaf".to_string(),
                    model: "served-model".to_string(),
                },
            )],
            "default-model".to_string(),
        );
        let reliable = ReliableModelProvider::new(
            "reliable",
            vec![(
                "outer-wrapper".into(),
                Box::new(router) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&reliable)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "hint:fast",
                None,
            )
            .await;

        assert!(outcome.result.is_ok());
        assert_eq!(outcome.accounting.attempts().len(), 1);
        let leaf = &outcome.accounting.attempts()[0];
        assert_eq!(
            (leaf.provider_ref(), leaf.model()),
            ("configured.leaf", "served-model")
        );
        assert!(matches!(
            leaf.outcome(),
            crate::dispatch::AttemptUsageOutcome::Missing
        ));
        assert_eq!(
            outcome
                .accounting
                .accepted_route()
                .map(|route| (route.provider_ref(), route.model())),
            Some(("configured.leaf", "served-model"))
        );
    }

    #[tokio::test]
    async fn router_then_reliable_reports_the_reliable_physical_leaf_on_error() {
        let reliable = ReliableModelProvider::new(
            "inner-reliable",
            vec![(
                "inner.actual".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "expected failure",
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let router = RouterModelProvider::new(
            "router",
            vec![(
                "router-child".to_string(),
                Box::new(reliable) as Box<dyn ModelProvider>,
            )],
            vec![(
                "fast".to_string(),
                Route {
                    provider_name: "router-child".to_string(),
                    model: "served-model".to_string(),
                },
            )],
            "default-model".to_string(),
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&router)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "hint:fast",
                None,
            )
            .await;

        assert!(outcome.result.is_err());
        assert_eq!(outcome.accounting.attempts().len(), 1);
        let leaf = &outcome.accounting.attempts()[0];
        assert_eq!(
            (leaf.provider_ref(), leaf.model()),
            ("inner.actual", "served-model")
        );
        assert!(matches!(
            leaf.outcome(),
            crate::dispatch::AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
        assert!(outcome.accounting.accepted_route().is_none());
    }

    #[tokio::test]
    async fn reliable_then_router_failure_reports_the_router_physical_leaf() {
        let router = RouterModelProvider::new(
            "router",
            vec![(
                "configured.leaf".to_string(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "expected failure",
                }) as Box<dyn ModelProvider>,
            )],
            vec![(
                "fast".to_string(),
                Route {
                    provider_name: "configured.leaf".to_string(),
                    model: "served-model".to_string(),
                },
            )],
            "default-model".to_string(),
        );
        let reliable = ReliableModelProvider::new(
            "reliable",
            vec![(
                "outer-wrapper".into(),
                Box::new(router) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&reliable)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "hint:fast",
                None,
            )
            .await;

        assert!(outcome.result.is_err());
        assert_eq!(outcome.accounting.attempts().len(), 1);
        assert_eq!(
            (
                outcome.accounting.attempts()[0].provider_ref(),
                outcome.accounting.attempts()[0].model(),
            ),
            ("configured.leaf", "served-model")
        );
        assert!(outcome.accounting.accepted_route().is_none());
    }

    #[tokio::test]
    async fn router_then_reliable_success_reports_the_reliable_physical_leaf() {
        let reliable = ReliableModelProvider::new(
            "inner-reliable",
            vec![(
                "inner.actual".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "unused",
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let router = RouterModelProvider::new(
            "router",
            vec![(
                "router-child".to_string(),
                Box::new(reliable) as Box<dyn ModelProvider>,
            )],
            vec![(
                "fast".to_string(),
                Route {
                    provider_name: "router-child".to_string(),
                    model: "served-model".to_string(),
                },
            )],
            "default-model".to_string(),
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&router)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "hint:fast",
                None,
            )
            .await;

        assert!(outcome.result.is_ok());
        assert_eq!(outcome.accounting.attempts().len(), 1);
        assert_eq!(
            (
                outcome.accounting.attempts()[0].provider_ref(),
                outcome.accounting.attempts()[0].model(),
            ),
            ("inner.actual", "served-model")
        );
        assert_eq!(
            outcome
                .accounting
                .accepted_route()
                .map(|route| (route.provider_ref(), route.model())),
            Some(("inner.actual", "served-model"))
        );
    }

    fn assert_terminal_usage_accounting<T: std::fmt::Debug>(
        result: anyhow::Result<T>,
        accounting: AccountedCallReport,
    ) {
        let error = result.expect_err("the terminal mock must fail");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TerminalProviderTypedError>()),
            "the final typed cause must remain downcastable: {error:#}"
        );
        assert_eq!(accounting.rejected_attempts().len(), 1);
        let rejected = &accounting.rejected_attempts()[0];
        assert_eq!(rejected.provider_ref(), "configured.primary");
        assert_eq!(rejected.model(), "served-model");
        assert_eq!(rejected.usage().input_tokens, Some(10));
        assert_eq!(rejected.usage().output_tokens, Some(5));
    }

    #[tokio::test]
    async fn chat_exhausted_empty_completions_retain_rejected_usage() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(UsagePersistentEmptyMock {
                    calls: Arc::clone(&calls),
                }),
            )],
            1,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let error = model_provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await
            .expect_err("persistent semantic-empty responses must fail");

        let rejected = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableRejectedCompletionUsage>())
            .expect("rejected usage must survive exhaustion");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>()),
            "the final semantic-empty cause must remain typed alongside usage"
        );
        assert_eq!(rejected.usage.input_tokens, Some(20));
        assert_eq!(rejected.usage.output_tokens, Some(10));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn exhausted_semantic_empty_reliable_call_has_no_accepted_route() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(UsagePersistentEmptyMock {
                    calls: Arc::new(AtomicUsize::new(0)),
                }) as Box<dyn ModelProvider>,
            )],
            1,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&model_provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await;

        assert!(outcome.result.is_err());
        assert!(outcome.accounting.accepted_route().is_none());
        assert_eq!(outcome.accounting.rejected_attempts().len(), 2);
    }

    #[tokio::test]
    async fn chat_preserves_rejected_usage_on_untruncatable_context_error() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "empty".into(),
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                ("context".into(), Box::new(ContextWindowErrorMock)),
            ],
            0,
            1,
        );
        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("hello"),
        ];

        let error = model_provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await
            .expect_err("untruncatable context error must fail");

        assert!(
            error
                .to_string()
                .contains("without breaking message/tool pairing")
        );
        let rejected = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableRejectedCompletionUsage>())
            .expect("rejected usage must survive the early context exit");
        assert!(
            !error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>()),
            "a later context failure supersedes an earlier semantic-empty attempt"
        );
        assert_eq!(rejected.usage.input_tokens, Some(10));
        assert_eq!(rejected.usage.output_tokens, Some(5));
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ContextWindowTypedError>()),
            "the untruncatable context cause must remain downcastable: {error:#}"
        );
    }

    #[tokio::test]
    async fn chat_with_tools_preserves_rejected_usage_on_untruncatable_context_error() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "empty".into(),
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                ("context".into(), Box::new(ContextWindowErrorMock)),
            ],
            0,
            1,
        );
        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("hello"),
        ];
        let tools = vec![serde_json::json!({"name": "noop"})];

        let error = model_provider
            .chat_with_tools(&messages, &tools, "test", Some(0.0))
            .await
            .expect_err("untruncatable context error must fail");

        assert!(
            error
                .to_string()
                .contains("without breaking message/tool pairing")
        );
        let rejected = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableRejectedCompletionUsage>())
            .expect("rejected usage must survive the early context exit");
        assert!(
            !error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>()),
            "a later context failure supersedes an earlier semantic-empty attempt"
        );
        assert_eq!(rejected.usage.input_tokens, Some(10));
        assert_eq!(rejected.usage.output_tokens, Some(5));
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ContextWindowTypedError>()),
            "the untruncatable context cause must remain downcastable: {error:#}"
        );
    }

    #[tokio::test]
    async fn chat_with_tools_retries_empty_completion_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: 1,
                    response: "recovered",
                }),
            )],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let result = model_provider
            .chat_with_tools(&messages, &[], "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("recovered"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_with_tools_retry_preserves_usage_from_rejected_empty_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(UsageEmptyThenTextMock {
                    calls: Arc::clone(&calls),
                }),
            )],
            1,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let response = model_provider
            .chat_with_tools(&messages, &[], "test", Some(0.0))
            .await
            .expect("second attempt succeeds");

        let usage = response.usage.expect("combined usage is retained");
        assert_eq!(usage.input_tokens, Some(20));
        assert_eq!(usage.output_tokens, Some(10));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_with_history_retries_empty_string_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: 1,
                    response: "recovered",
                }),
            )],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let result = model_provider
            .chat_with_history(&messages, "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_with_system_retries_think_only_text_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(ThinkOnlyThenTextMock {
                    calls: Arc::clone(&calls),
                }),
            )],
            1,
            1,
        );
        let result = model_provider
            .chat_with_system(None, "hello", "test", Some(0.0))
            .await
            .expect("think-only text must retry through the Reliable path");

        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_with_system_falls_back_after_think_only_text() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: 0,
                        response: "<think>internal reasoning</think>",
                        error: "unused",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from fallback",
                        error: "unused",
                    }),
                ),
            ],
            0,
            1,
        );

        let result = model_provider
            .chat_with_system(None, "hello", "test", Some(0.0))
            .await
            .expect("think-only text must advance to provider fallback");

        assert_eq!(result, "from fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn chat_with_system_retries_empty_string_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: 1,
                    response: "recovered",
                }),
            )],
            3,
            1,
        );

        // `simple_chat` routes through `ReliableModelProvider::chat_with_system`,
        // the path subagent delegation uses.
        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_persistent_empty_returns_aggregated_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: usize::MAX, // always empty
                    response: "never",
                }),
            )],
            2,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let err = model_provider
            .chat(request, "test", Some(0.0))
            .await
            .expect_err("an exhausted empty completion must not be returned as success");
        assert!(
            err.to_string()
                .contains("All model providers/models failed")
        );
        assert!(err.to_string().contains("empty_response"));
        // Initial attempt + max_retries (2) re-rolls = 3 calls.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn every_reliable_chat_entrypoint_rejects_persistent_empty_completion() {
        let messages = vec![ChatMessage::user("hello")];

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = persistent_empty_reliable(Arc::clone(&calls));
        let error = provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await
            .expect_err("semantic-empty chat result must fail");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = persistent_empty_reliable(Arc::clone(&calls));
        let error = provider
            .chat_with_tools(&messages, &[], "test", Some(0.0))
            .await
            .expect_err("semantic-empty tool result must fail");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = persistent_empty_reliable(Arc::clone(&calls));
        let error = provider
            .chat_with_history(&messages, "test", Some(0.0))
            .await
            .expect_err("semantic-empty history result must fail");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = persistent_empty_reliable(Arc::clone(&calls));
        let error = provider
            .chat_with_system(None, "hello", "test", Some(0.0))
            .await
            .expect_err("semantic-empty system result must fail");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn every_reliable_chat_entrypoint_uses_the_later_provider_failure_cause() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![serde_json::json!({"name": "noop"})];

        let error = semantic_empty_then_provider_error_reliable()
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
            )
            .await
            .expect_err("the later provider failure must fail chat");
        assert_later_provider_failure_supersedes_semantic_empty(&error);

        let error = semantic_empty_then_provider_error_reliable()
            .chat_with_tools(&messages, &tools, "test", Some(0.0))
            .await
            .expect_err("the later provider failure must fail chat_with_tools");
        assert_later_provider_failure_supersedes_semantic_empty(&error);

        let error = semantic_empty_then_provider_error_reliable()
            .chat_with_history(&messages, "test", Some(0.0))
            .await
            .expect_err("the later provider failure must fail chat_with_history");
        assert_later_provider_failure_supersedes_semantic_empty(&error);

        let error = semantic_empty_then_provider_error_reliable()
            .chat_with_system(None, "hello", "test", Some(0.0))
            .await
            .expect_err("the later provider failure must fail chat_with_system");
        assert_later_provider_failure_supersedes_semantic_empty(&error);
    }

    fn assert_later_provider_failure_supersedes_semantic_empty(error: &anyhow::Error) {
        assert!(
            error
                .to_string()
                .contains("event 2 (retry 1/1): retryable; kind=provider_error"),
            "the final provider failure classification must be retained: {error:#}"
        );
        assert!(
            !error
                .to_string()
                .contains("upstream terminal provider failure"),
            "provider-controlled failure text must not reach the terminal summary: {error:#}"
        );
        assert!(
            !error
                .chain()
                .any(|cause| cause.is::<ReliableSemanticEmptyCompletion>()),
            "a later provider failure must supersede the semantic-empty marker: {error:#}"
        );
    }

    #[tokio::test]
    async fn later_provider_failure_keeps_rejected_usage_without_a_semantic_empty_marker() {
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![serde_json::json!({"name": "noop"})];

        let error = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "empty".into(),
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "failure".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "upstream terminal provider failure",
                    }),
                ),
            ],
            0,
            1,
        )
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "test",
            Some(0.0),
        )
        .await
        .expect_err("the later provider failure must fail chat");
        assert_later_provider_failure_supersedes_semantic_empty(&error);
        assert_rejected_usage_survives(&error);

        let error = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "empty".into(),
                    Box::new(UsagePersistentEmptyMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "failure".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "upstream terminal provider failure",
                    }),
                ),
            ],
            0,
            1,
        )
        .chat_with_tools(&messages, &tools, "test", Some(0.0))
        .await
        .expect_err("the later provider failure must fail chat_with_tools");
        assert_later_provider_failure_supersedes_semantic_empty(&error);
        assert_rejected_usage_survives(&error);
    }

    fn assert_rejected_usage_survives(error: &anyhow::Error) {
        let rejected = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableRejectedCompletionUsage>())
            .expect("rejected usage must survive the later provider failure");
        assert_eq!(rejected.usage.input_tokens, Some(10));
        assert_eq!(rejected.usage.output_tokens, Some(5));
    }

    #[tokio::test]
    async fn chat_persistent_empty_falls_back_to_next_provider() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(EmptyThenTextMock {
                        calls: Arc::clone(&primary_calls),
                        empty_until_attempt: usize::MAX,
                        response: "never",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(EmptyThenTextMock {
                        calls: Arc::clone(&fallback_calls),
                        empty_until_attempt: 0,
                        response: "recovered by fallback",
                    }),
                ),
            ],
            1,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "test", Some(0.0))
            .await
            .expect("fallback should recover an exhausted empty completion");

        assert_eq!(result.text.as_deref(), Some("recovered by fallback"));
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn chat_nonempty_response_is_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(EmptyThenTextMock {
                    calls: Arc::clone(&calls),
                    empty_until_attempt: 0, // never empty
                    response: "direct",
                }),
            )],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("direct"));
        // A non-empty response must not trigger any re-roll.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returns_aggregated_error_when_all_providers_fail() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "p1".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p1 error",
                    }),
                ),
                (
                    "p2".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p2 error",
                    }),
                ),
            ],
            0,
            1,
        );

        let err = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .expect_err("all model_providers should fail");
        let msg = err.to_string();
        assert!(msg.contains("All model providers/models failed after 2 failure event(s)"));
        assert!(msg.contains("event 1 (retry 1/1): retryable"));
        assert!(msg.contains("event 2 (retry 1/1): retryable"));
        assert!(!msg.contains("p1 error"));
        assert!(!msg.contains("p2 error"));
        assert!(msg.contains("retryable"));
    }

    #[tokio::test]
    async fn excessive_failure_events_are_bounded_without_provider_details() {
        const PROVIDER_COUNT: usize = 64;
        let providers: Vec<(String, Box<dyn ModelProvider>)> = (0..PROVIDER_COUNT)
            .map(|index| {
                (
                    format!("sensitive-provider-identity-{index}"),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "sensitive provider response body: secret-token",
                    }) as Box<dyn ModelProvider>,
                )
            })
            .collect();
        let model_provider = ReliableModelProvider::new("test", providers, 0, 1);

        let err = model_provider
            .simple_chat("hello", "sensitive-model-identity", Some(0.0))
            .await
            .expect_err("all model providers should fail");
        let msg = err.to_string();

        assert!(
            msg.len() <= MAX_FAILURE_AGGREGATE_BYTES,
            "aggregate was {} bytes",
            msg.len()
        );
        assert!(msg.contains("after 64 failure event(s)"));
        assert!(msg.contains("event 8 (retry 1/1): retryable"));
        assert!(!msg.contains("event 9 (retry 1/1): retryable"));
        assert!(msg.contains(&format!(
            "[{} additional failure event(s) omitted]",
            PROVIDER_COUNT - MAX_RETAINED_FAILURE_EVENTS
        )));
        assert!(!msg.contains("sensitive-provider-identity"));
        assert!(!msg.contains("sensitive-model-identity"));
        assert!(!msg.contains("sensitive provider response body"));
        assert!(!msg.contains("secret-token"));
    }

    #[test]
    fn non_retryable_detects_common_patterns() {
        assert!(is_non_retryable(&anyhow::Error::msg("400 Bad Request")));
        assert!(is_non_retryable(&anyhow::Error::msg("401 Unauthorized")));
        assert!(is_non_retryable(&anyhow::Error::msg("403 Forbidden")));
        assert!(is_non_retryable(&anyhow::Error::msg("404 Not Found")));
        assert!(is_non_retryable(&anyhow::Error::msg(
            "invalid api key provided"
        )));
        assert!(is_non_retryable(&anyhow::Error::msg(
            "authentication failed"
        )));
        assert!(is_non_retryable(&anyhow::Error::msg(
            "model glm-4.7 not found"
        )));
        assert!(is_non_retryable(&anyhow::Error::msg(
            "unsupported model: glm-4.7"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "429 Too Many Requests"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "408 Request Timeout"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "500 Internal Server Error"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg("502 Bad Gateway")));
        assert!(!is_non_retryable(&anyhow::Error::msg("timeout")));
        assert!(!is_non_retryable(&anyhow::Error::msg("connection reset")));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "model overloaded, try again later"
        )));
        // Context window errors are now recoverable (not non-retryable)
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "OpenAI Codex stream error: Your input exceeds the context window of this model."
        )));
    }

    #[test]
    fn auth_error_detects_common_patterns() {
        assert!(is_auth_error(&anyhow::Error::msg("401 Unauthorized")));
        assert!(is_auth_error(&anyhow::Error::msg("403 Forbidden")));
        assert!(is_auth_error(&anyhow::Error::msg("invalid api key")));
        assert!(is_auth_error(&anyhow::Error::msg("authentication failed")));
        assert!(is_auth_error(&anyhow::Error::msg("token expired")));
        assert!(!is_auth_error(&anyhow::Error::msg("400 Bad Request")));
        assert!(!is_auth_error(&anyhow::Error::msg("429 Too Many Requests")));
        assert!(!is_auth_error(&anyhow::Error::msg("timeout")));
        assert!(!is_auth_error(&anyhow::Error::msg("connection reset")));
    }

    #[test]
    fn provider_error_diagnostic_identifies_connect_timeout_endpoint() {
        let err = anyhow::Error::msg(
            "error sending request for url (https://api.deepseek.com/chat/completions): \
             client error (Connect): operation timed out",
        );

        let diagnostic = provider_error_diagnostic(&err);

        assert_eq!(diagnostic.kind, "connect_timeout");
        assert_eq!(diagnostic.phase, "tls_or_connect");
        assert_eq!(
            diagnostic.endpoint.as_deref(),
            Some("https://api.deepseek.com/chat/completions")
        );
        assert!(diagnostic.hint.contains("VPN"));
    }

    #[test]
    fn endpoint_from_error_text_strips_url_userinfo() {
        let endpoint = endpoint_from_error_text(
            "error sending request for url \
             (https://user:hunter2@inference.host/v1?token=hunter2#debug): timed out",
        );

        assert_eq!(endpoint.as_deref(), Some("https://inference.host/v1"));
    }

    #[test]
    fn sanitized_url_endpoint_scrubs_secret_like_path_segments() {
        let endpoint = sanitized_url_endpoint(
            reqwest::Url::parse(
                "https://user:hunter2@inference.host/v1/sk-secretvalue123/chat?token=hunter2#debug",
            )
            .expect("test URL parses"),
        );

        assert_eq!(endpoint, "https://inference.host/v1/[REDACTED]/chat");
        assert!(!endpoint.contains("secretvalue123"));
        assert!(!endpoint.contains("hunter2"));
    }

    #[test]
    fn endpoint_from_error_text_drops_unparseable_urls() {
        let endpoint = endpoint_from_error_text("error sending request to https://:not-a-url");

        assert_eq!(endpoint, None);
    }

    #[test]
    fn endpoint_from_error_text_preserves_ipv6_host_brackets() {
        let bare = endpoint_from_error_text("error sending request for url (http://[::1]): failed");
        let with_port = endpoint_from_error_text(
            "error sending request for url (http://[::1]:8080/v1): failed",
        );

        assert_eq!(bare.as_deref(), Some("http://[::1]/"));
        assert_eq!(with_port.as_deref(), Some("http://[::1]:8080/v1"));
    }

    #[test]
    fn provider_error_diagnostic_classifies_text_error_branches() {
        let cases = [
            (
                "input exceeds the context window of this model",
                "context_window",
                "request_validation",
                "larger-context model",
            ),
            (
                "missing api key for configured provider",
                "credentials_missing",
                "configuration",
                "configure provider credentials",
            ),
            (
                "Anthropic credentials not set. Run `zeroclaw quickstart` or `zeroclaw config set` to configure.",
                "credentials_missing",
                "configuration",
                "configure provider credentials",
            ),
            (
                "API error (401): missing API key",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "API error (403): API key not set",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "401 Unauthorized: invalid api key",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "429 Too Many Requests",
                "rate_limited",
                "http_response",
                "quota",
            ),
            (
                "client error (Connect): operation timed out",
                "connect_timeout",
                "tls_or_connect",
                "VPN",
            ),
            (
                "request timed out while waiting for provider",
                "timeout",
                "request",
                "timed out",
            ),
            ("dns resolve failed for provider host", "dns", "dns", "DNS"),
            (
                "model gpt-missing does not exist",
                "model_not_found",
                "http_response",
                "model id",
            ),
            (
                "compatible API error (503 Service Unavailable): overload",
                "provider_server",
                "http_response",
                "server error",
            ),
            (
                "compatible API error (400 Bad Request): malformed request",
                "client_error",
                "http_response",
                "request shape",
            ),
            (
                "compatible API error (400 Bad Request): input exceeds the context window of this model",
                "context_window",
                "request_validation",
                "larger-context model",
            ),
            (
                "HTTP 503 Service Unavailable",
                "provider_server",
                "http_response",
                "server error",
            ),
            (
                "HTTP 404 Not Found",
                "model_not_found",
                "http_response",
                "model id",
            ),
            (
                "HTTP 400 Bad Request",
                "client_error",
                "http_response",
                "request shape",
            ),
            (
                "HTTP request failed: HTTP 503 Service Unavailable",
                "provider_server",
                "http_response",
                "server error",
            ),
            (
                "compatible API error (401 Unauthorized): invalid credentials",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "compatible API error (403 Forbidden): invalid credentials",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "compatible API error (429 Too Many Requests): retry later",
                "rate_limited",
                "http_response",
                "quota",
            ),
            (
                "ModelProvider error: 401 Unauthorized: invalid credentials",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "ModelProvider error: 403 Forbidden: invalid credentials",
                "auth",
                "http_response",
                "credentials",
            ),
            (
                "ModelProvider error: 404 Not Found: unknown model",
                "model_not_found",
                "http_response",
                "model id",
            ),
            (
                "ModelProvider error: 429 Too Many Requests: retry later",
                "rate_limited",
                "http_response",
                "quota",
            ),
            (
                "ModelProvider error: 503 Service Unavailable: overload",
                "provider_server",
                "http_response",
                "server error",
            ),
            (
                "model_provider stream error: ModelProvider error: 404 Not Found: unknown model",
                "model_not_found",
                "http_response",
                "model id",
            ),
            (
                "model_provider stream error: JSON parse error: invalid type: string \"503 Service Unavailable\", expected a sequence at line 1 column 36",
                "provider_error",
                "unknown",
                "inspect provider error",
            ),
            (
                "provider returned an opaque transport error",
                "provider_error",
                "unknown",
                "inspect provider error",
            ),
        ];

        for (message, expected_kind, expected_phase, expected_hint) in cases {
            let diagnostic = provider_error_diagnostic(&anyhow::Error::msg(message));

            assert_eq!(diagnostic.kind, expected_kind, "{message}");
            assert_eq!(diagnostic.phase, expected_phase, "{message}");
            assert!(diagnostic.hint.contains(expected_hint), "{message}");
        }
    }

    #[test]
    fn provider_error_diagnostic_prioritizes_wrapped_structured_http_status() {
        for (status, expected_kind) in [
            (reqwest::StatusCode::UNAUTHORIZED, "auth"),
            (reqwest::StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        ] {
            let response = reqwest::Response::from(
                axum::http::Response::builder()
                    .status(status)
                    .body(reqwest::Body::default())
                    .expect("test response should build"),
            );
            let error = anyhow::Error::new(
                response
                    .error_for_status()
                    .expect_err("error status should produce an error"),
            )
            .context("missing API key");

            let diagnostic = provider_error_diagnostic(&error);

            assert_eq!(diagnostic.kind, expected_kind, "{status}");
            assert_eq!(diagnostic.phase, "http_response", "{status}");
        }

        let response = reqwest::Response::from(
            axum::http::Response::builder()
                .status(reqwest::StatusCode::BAD_REQUEST)
                .body(reqwest::Body::default())
                .expect("test response should build"),
        );
        let error = anyhow::Error::new(
            response
                .error_for_status()
                .expect_err("error status should produce an error"),
        )
        .context("input exceeds the context window of this model");

        let diagnostic = provider_error_diagnostic(&error);

        assert_eq!(diagnostic.kind, "context_window");
        assert_eq!(diagnostic.phase, "request_validation");
    }

    #[test]
    fn terminal_provider_failure_keeps_attempt_diagnostics_out_of_presentation_type() {
        let mut failures = FailureEvents::default();
        failures.push("event 1 (retry 1/1): retryable; provider detail".to_string());
        let error = reliable_terminal_error_with_cause(
            Some("custom.truefoundry"),
            failures,
            None,
            false,
            Some(anyhow::Error::msg(
                "error sending request for url (http://127.0.0.1:11434/v1/chat/completions): \
                 client error (Connect): connection refused",
            )),
        );

        assert!(error.to_string().contains("event 1 (retry 1/1)"));
        let failure = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableProviderTerminalFailure>())
            .expect("terminal provider failure must retain its typed presentation cause");
        assert_eq!(
            failure.kind(),
            ReliableProviderTerminalFailureKind::Connection
        );
        assert_eq!(failure.provider(), Some("custom.truefoundry"));
        assert_eq!(
            failure.endpoint(),
            Some("http://127.0.0.1:11434/v1/chat/completions")
        );
        assert!(failure.endpoint_is_local());
        assert!(error.chain().any(|cause| {
            cause
                .to_string()
                .contains("client error (Connect): connection refused")
        }));
    }

    #[test]
    fn terminal_provider_failure_preserves_rejected_usage_accounting() {
        let mut failures = FailureEvents::default();
        failures.push("event 1 (retry 1/1): retryable; provider detail".to_string());
        let error = reliable_terminal_error_with_cause(
            None,
            failures,
            Some(TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cached_input_tokens: None,
            }),
            false,
            Some(anyhow::Error::msg(
                "compatible API error (503 Service Unavailable)",
            )),
        );

        assert_rejected_usage_survives(&error);
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<ReliableProviderTerminalFailure>()
                .is_some()
        }));
    }

    #[tokio::test]
    async fn anthropic_missing_credentials_reach_typed_terminal_failure() {
        let model_provider = ReliableModelProvider::new(
            "anthropic",
            vec![(
                "anthropic".into(),
                Box::new(AnthropicModelProvider::builder("anthropic").build()),
            )],
            0,
            1,
        );

        let error = model_provider
            .simple_chat("hello", "claude-sonnet-4-5", Some(0.0))
            .await
            .expect_err("missing Anthropic credentials should fail");
        let failure = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableProviderTerminalFailure>())
            .expect("Reliable must retain the typed Anthropic failure");

        assert_eq!(
            failure.kind(),
            ReliableProviderTerminalFailureKind::CredentialsMissing
        );
    }

    #[test]
    fn failure_summary_contains_only_safe_diagnostic_fields() {
        let diagnostic = ProviderErrorDiagnostic {
            kind: "connect_timeout",
            phase: "tls_or_connect",
            hint: "check network, VPN, or firewall",
            endpoint: Some("https://api.deepseek.com/chat/completions".to_string()),
        };
        let mut failures = FailureEvents::default();

        push_failure(&mut failures, 1, 3, "retryable", Some(&diagnostic));

        let summary = failure_aggregate(&failures);
        assert!(summary.contains("event 1 (retry 1/3): retryable"));
        assert!(summary.contains("kind=connect_timeout"));
        assert!(summary.contains("phase=tls_or_connect"));
        assert!(summary.contains("hint=check network, VPN, or firewall"));
        assert!(!summary.contains("https://api.deepseek.com/chat/completions"));
        assert!(!summary.contains("operation timed out"));
    }

    #[test]
    fn failure_aggregate_enforces_byte_limit_when_an_event_does_not_fit() {
        let mut failures = FailureEvents::default();
        failures.push("provider-controlled-body".repeat(MAX_FAILURE_AGGREGATE_BYTES));

        let summary = failure_aggregate(&failures);

        assert!(summary.len() <= MAX_FAILURE_AGGREGATE_BYTES);
        assert!(summary.contains("after 1 failure event(s)"));
        assert!(summary.contains("[1 additional failure event(s) omitted]"));
        assert!(!summary.contains("provider-controlled-body"));
    }

    #[tokio::test]
    async fn context_window_error_aborts_retries_and_model_fallbacks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut model_fallbacks = std::collections::HashMap::new();
        model_fallbacks.insert(
            "gpt-5.3-codex".to_string(),
            vec!["gpt-5.2-codex".to_string()],
        );

        let model_provider = ReliableModelProvider::new("test", vec![(
                "openai-codex".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "OpenAI Codex stream error: Your input exceeds the context window of this model. Please adjust your input and try again.",
                }),
            )],
            4,
            1,
        )
        .with_model_fallbacks(model_fallbacks);

        let err = model_provider
            .simple_chat("hello", "gpt-5.3-codex", Some(0.0))
            .await
            .expect_err("context window overflow should fail fast");
        let msg = err.to_string();

        assert!(msg.contains("context window"));
        // chat_with_system has no history to truncate, so it bails immediately
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn aggregated_error_marks_non_retryable_model_mismatch_without_provider_text() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "custom".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "unsupported model: glm-4.7",
                }),
            )],
            3,
            1,
        );

        let err = model_provider
            .simple_chat("hello", "glm-4.7", Some(0.0))
            .await
            .expect_err("model_provider should fail");
        let msg = err.to_string();

        assert!(msg.contains("All model providers/models failed after 1 failure event(s)"));
        assert!(msg.contains("non_retryable"));
        assert!(msg.contains("kind=model_not_found"));
        assert!(!msg.contains("unsupported model: glm-4.7"));
        // Non-retryable errors should not consume retry budget.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn skips_retries_on_non_retryable_error() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "401 Unauthorized",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from fallback",
                        error: "fallback err",
                    }),
                ),
            ],
            3,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "from fallback");
        // Primary should have been called only once (no retries)
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn chat_with_history_retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    response: "history ok",
                    error: "temporary",
                }),
            )],
            2,
            1,
        );

        let messages = vec![ChatMessage::system("system"), ChatMessage::user("hello")];
        let result = model_provider
            .chat_with_history(&messages, "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "history ok");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_with_history_falls_back() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary down",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "fallback ok",
                        error: "fallback err",
                    }),
                ),
            ],
            1,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let result = model_provider
            .chat_with_history(&messages, "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "fallback ok");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    // ── New tests: model failover ──

    #[tokio::test]
    async fn model_failover_tries_fallback_model() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(ModelAwareMock {
            calls: Arc::clone(&calls),
            models_seen: parking_lot::Mutex::new(Vec::new()),
            fail_models: vec!["claude-opus"],
            response: "ok from sonnet",
        });

        let mut fallbacks = HashMap::new();
        fallbacks.insert("claude-opus".to_string(), vec!["claude-sonnet".to_string()]);

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "anthropic".into(),
                Box::new(mock.clone()) as Box<dyn ModelProvider>,
            )],
            0, // no retries — force immediate model failover
            1,
        )
        .with_model_fallbacks(fallbacks);

        let result = model_provider
            .simple_chat("hello", "claude-opus", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "ok from sonnet");

        let seen = mock.models_seen.lock();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], "claude-opus");
        assert_eq!(seen[1], "claude-sonnet");
    }

    #[tokio::test]
    async fn model_failover_all_models_fail() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(ModelAwareMock {
            calls: Arc::clone(&calls),
            models_seen: parking_lot::Mutex::new(Vec::new()),
            fail_models: vec!["model-a", "model-b", "model-c"],
            response: "never",
        });

        let mut fallbacks = HashMap::new();
        fallbacks.insert(
            "model-a".to_string(),
            vec!["model-b".to_string(), "model-c".to_string()],
        );

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "p1".into(),
                Box::new(mock.clone()) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        )
        .with_model_fallbacks(fallbacks);

        let err = model_provider
            .simple_chat("hello", "model-a", Some(0.0))
            .await
            .expect_err("all models should fail");
        assert!(
            err.to_string()
                .contains("All model providers/models failed after 3 failure event(s)")
        );

        let seen = mock.models_seen.lock();
        assert_eq!(seen.len(), 3);
    }

    #[tokio::test]
    async fn no_model_fallbacks_behaves_like_before() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "boom",
                }),
            )],
            2,
            1,
        );
        // No model_fallbacks set — should work exactly as before
        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // ── New tests: auth rotation ──

    #[tokio::test]
    async fn auth_rotation_cycles_keys() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "p".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "",
                }),
            )],
            0,
            1,
        )
        .with_api_keys(vec!["key-a".into(), "key-b".into(), "key-c".into()]);

        // Rotate 5 times, verify round-robin
        let keys: Vec<&str> = (0..5)
            .map(|_| model_provider.rotate_key().unwrap())
            .collect();
        assert_eq!(keys, vec!["key-a", "key-b", "key-c", "key-a", "key-b"]);
    }

    #[tokio::test]
    async fn auth_rotation_returns_none_when_empty() {
        let model_provider = ReliableModelProvider::new("test", vec![], 0, 1);
        assert!(model_provider.rotate_key().is_none());
    }

    // ── New tests: Retry-After parsing ──

    #[test]
    fn parse_retry_after_integer() {
        let err = anyhow::Error::msg("429 Too Many Requests, Retry-After: 5");
        assert_eq!(parse_retry_after_ms(&err), Some(5000));
    }

    #[test]
    fn parse_retry_after_float() {
        let err = anyhow::Error::msg("Rate limited. retry_after: 2.5 seconds");
        assert_eq!(parse_retry_after_ms(&err), Some(2500));
    }

    #[test]
    fn parse_retry_after_missing() {
        let err = anyhow::Error::msg("500 Internal Server Error");
        assert_eq!(parse_retry_after_ms(&err), None);
    }

    #[test]
    fn rate_limited_detection() {
        assert!(is_rate_limited(&anyhow::Error::msg(
            "429 Too Many Requests"
        )));
        assert!(is_rate_limited(&anyhow::Error::msg(
            "HTTP 429 rate limit exceeded"
        )));
        assert!(!is_rate_limited(&anyhow::Error::msg("401 Unauthorized")));
        assert!(!is_rate_limited(&anyhow::Error::msg(
            "500 Internal Server Error"
        )));
    }

    #[test]
    fn non_retryable_rate_limit_detects_plan_restricted_model() {
        let err = anyhow::Error::msg(
            "API error (429 Too Many Requests): {\"code\":1311,\"message\":\"the current account plan does not include glm-5\"}",
        );
        assert!(
            is_non_retryable_rate_limit(&err),
            "plan-restricted 429 should skip retries"
        );
    }

    #[test]
    fn non_retryable_rate_limit_detects_insufficient_balance() {
        let err = anyhow::Error::msg(
            "API error (429 Too Many Requests): {\"code\":1113,\"message\":\"insufficient balance\"}",
        );
        assert!(
            is_non_retryable_rate_limit(&err),
            "insufficient-balance 429 should skip retries"
        );
    }

    #[test]
    fn non_retryable_rate_limit_does_not_flag_generic_429() {
        let err = anyhow::Error::msg("429 Too Many Requests: rate limit exceeded");
        assert!(
            !is_non_retryable_rate_limit(&err),
            "generic rate-limit 429 should remain retryable"
        );
    }

    #[test]
    fn compute_backoff_uses_retry_after() {
        let model_provider = ReliableModelProvider::new("test", vec![], 0, 500);
        let err = anyhow::Error::msg("429 Retry-After: 3");
        assert_eq!(model_provider.compute_backoff(500, &err), 3_000);
    }

    #[test]
    fn compute_backoff_caps_at_30s() {
        let model_provider = ReliableModelProvider::new("test", vec![], 0, 500);
        let err = anyhow::Error::msg("429 Retry-After: 120");
        assert_eq!(model_provider.compute_backoff(500, &err), 30_000);
    }

    #[test]
    fn compute_backoff_falls_back_to_base() {
        let model_provider = ReliableModelProvider::new("test", vec![], 0, 500);
        let err = anyhow::Error::msg("500 Server Error");
        assert_eq!(model_provider.compute_backoff(500, &err), 500);
    }

    // ── §2.1 API auth error (401/403) tests ──────────────────

    #[test]
    fn non_retryable_detects_401() {
        let err = anyhow::Error::msg("API error (401 Unauthorized): invalid api key");
        assert!(
            is_non_retryable(&err),
            "401 errors must be detected as non-retryable"
        );
    }

    #[test]
    fn non_retryable_detects_403() {
        let err = anyhow::Error::msg("API error (403 Forbidden): access denied");
        assert!(
            is_non_retryable(&err),
            "403 errors must be detected as non-retryable"
        );
    }

    #[test]
    fn non_retryable_detects_404() {
        let err = anyhow::Error::msg("API error (404 Not Found): model not found");
        assert!(
            is_non_retryable(&err),
            "404 errors must be detected as non-retryable"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_429() {
        let err = anyhow::Error::msg("429 Too Many Requests");
        assert!(
            !is_non_retryable(&err),
            "429 must NOT be treated as non-retryable (it is retryable with backoff)"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_408() {
        let err = anyhow::Error::msg("408 Request Timeout");
        assert!(
            !is_non_retryable(&err),
            "408 must NOT be treated as non-retryable (it is retryable)"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_500() {
        let err = anyhow::Error::msg("500 Internal Server Error");
        assert!(
            !is_non_retryable(&err),
            "500 must NOT be treated as non-retryable (server errors are retryable)"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_502() {
        let err = anyhow::Error::msg("502 Bad Gateway");
        assert!(
            !is_non_retryable(&err),
            "502 must NOT be treated as non-retryable"
        );
    }

    // ── §2.2 Rate limit Retry-After edge cases ───────────────

    #[test]
    fn parse_retry_after_zero() {
        let err = anyhow::Error::msg("429 Too Many Requests, Retry-After: 0");
        assert_eq!(
            parse_retry_after_ms(&err),
            Some(0),
            "Retry-After: 0 should parse as 0ms"
        );
    }

    #[test]
    fn parse_retry_after_with_underscore_separator() {
        let err = anyhow::Error::msg("rate limited, retry_after: 10");
        assert_eq!(
            parse_retry_after_ms(&err),
            Some(10_000),
            "retry_after with underscore must be parsed"
        );
    }

    #[test]
    fn parse_retry_after_space_separator() {
        let err = anyhow::Error::msg("Retry-After 7");
        assert_eq!(
            parse_retry_after_ms(&err),
            Some(7000),
            "Retry-After with space separator must be parsed"
        );
    }

    #[test]
    fn rate_limited_false_for_generic_error() {
        let err = anyhow::Error::msg("Connection refused");
        assert!(
            !is_rate_limited(&err),
            "generic errors must not be flagged as rate-limited"
        );
    }

    // ── §2.3 Malformed API response error classification ─────

    #[tokio::test]
    async fn non_retryable_skips_retries_for_401() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "API error (401 Unauthorized): invalid key",
                }),
            )],
            5,
            1,
        );

        let result = model_provider.simple_chat("hello", "test", Some(0.0)).await;
        assert!(result.is_err(), "401 should fail without retries");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "must not retry on 401 — should be exactly 1 call"
        );
    }

    #[tokio::test]
    async fn non_retryable_rate_limit_skips_retries_for_plan_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "API error (429 Too Many Requests): {\"code\":1311,\"message\":\"plan does not include glm-5\"}",
                }),
            )],
            5,
            1,
        );

        let result = model_provider.simple_chat("hello", "test", Some(0.0)).await;
        assert!(
            result.is_err(),
            "plan-restricted 429 should fail quickly without retrying"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "must not retry non-retryable 429 business errors"
        );
    }

    #[test]
    fn cooldown_state_expires_and_cleans_itself() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "boom",
                }),
            )],
            0,
            1,
        );
        let err = anyhow::Error::msg("429 Too Many Requests, Retry-After: 0");

        let cooldown = model_provider.set_rate_limit_cooldown("primary", &err);

        assert_eq!(cooldown, Duration::ZERO);
        assert!(
            !model_provider.provider_cooldown_active("primary"),
            "zero-length cooldown should expire and be removed on read"
        );
    }

    #[tokio::test]
    async fn retryable_rate_limit_cools_down_provider_and_uses_fallback() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "HTTP 429 Too Many Requests, Retry-After: 30",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from fallback",
                        error: "fallback down",
                    }),
                ),
            ],
            5,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();

        assert_eq!(result, "from fallback");
        assert_eq!(
            primary_calls.load(Ordering::SeqCst),
            1,
            "retryable 429 should not spend every retry on the cooled-down provider"
        );
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert!(
            model_provider.provider_cooldown_active("primary"),
            "primary provider should remain cooled down after Retry-After"
        );
    }

    #[tokio::test]
    async fn cooldown_skip_uses_safe_terminal_summary() {
        let skipped_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new(
                    "provider-alias-should-not-escape",
                    "primary-cooldown-key",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&skipped_calls),
                        fail_until_attempt: 0,
                        response: "should be skipped",
                        error: "unreachable",
                    }),
                ),
                ReliableModelProviderEntry::new(
                    "fallback-alias-should-not-escape",
                    "fallback-cooldown-key",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: usize::MAX,
                        response: "unreachable",
                        error: "fallback provider response marker",
                    }),
                ),
            ],
            0,
            1,
        );
        let cooldown_error = anyhow::Error::msg("429 Too Many Requests, Retry-After: 30");
        model_provider.set_rate_limit_cooldown("primary-cooldown-key", &cooldown_error);

        let error = model_provider
            .simple_chat("hello", "model-id-should-not-escape", Some(0.0))
            .await
            .expect_err("the non-cooled fallback should fail");
        let message = error.to_string();

        assert_eq!(skipped_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert!(message.contains("event 1 (retry 0/1): rate_limit_cooldown"));
        assert!(message.contains("kind=rate_limited"));
        assert!(message.contains("phase=cooldown"));
        assert!(message.contains("event 2 (retry 1/1): retryable"));
        assert!(!message.contains("provider-alias-should-not-escape"));
        assert!(!message.contains("fallback-alias-should-not-escape"));
        assert!(!message.contains("model-id-should-not-escape"));
        assert!(!message.contains("fallback provider response marker"));
    }

    #[tokio::test]
    async fn cooldown_skipped_candidate_creates_no_canonical_attempt() {
        let skipped_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new(
                    "skipped-display",
                    "primary-cooldown-key",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&skipped_calls),
                        fail_until_attempt: 0,
                        response: "never",
                        error: "unused",
                    }),
                ),
                ReliableModelProviderEntry::new(
                    "physical-fallback",
                    "fallback-cooldown-key",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "expected fallback failure",
                    }),
                ),
            ],
            0,
            1,
        );
        provider.set_rate_limit_cooldown(
            "primary-cooldown-key",
            &anyhow::Error::msg("429 Too Many Requests, Retry-After: 30"),
        );
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&provider)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "served-model",
                None,
            )
            .await;

        assert!(outcome.result.is_err());
        assert_eq!(skipped_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.accounting.attempts().len(), 1);
        assert_eq!(
            outcome.accounting.attempts()[0].provider_ref(),
            "fallback-cooldown-key"
        );
    }

    #[tokio::test]
    async fn retryable_rate_limit_cools_down_shared_provider_identity() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let shared_model_fallback_calls = Arc::new(AtomicUsize::new(0));
        let downstream_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new(
                    "primary",
                    "openai.work",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "HTTP 429 Too Many Requests, Retry-After: 30",
                    }),
                ),
                ReliableModelProviderEntry::new(
                    "primary",
                    "openai.work",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&shared_model_fallback_calls),
                        fail_until_attempt: 0,
                        response: "should be skipped",
                        error: "shared down",
                    }),
                ),
                ReliableModelProviderEntry::new(
                    "downstream",
                    "anthropic.work",
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&downstream_calls),
                        fail_until_attempt: 0,
                        response: "downstream fallback",
                        error: "downstream down",
                    }),
                ),
            ],
            5,
            1,
        );

        let result = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .unwrap();

        assert_eq!(result, "downstream fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            shared_model_fallback_calls.load(Ordering::SeqCst),
            0,
            "entries sharing a cooldown key should be skipped as one provider"
        );
        assert_eq!(downstream_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retryable_rate_limit_cools_down_provider_for_history_chat() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "HTTP 429 Too Many Requests, Retry-After: 30",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "history fallback",
                        error: "fallback down",
                    }),
                ),
            ],
            5,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let result = model_provider
            .chat_with_history(&messages, "test", Some(0.0))
            .await
            .unwrap();

        assert_eq!(result, "history fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    // Arc<ModelAwareMock> ModelProvider impl provided by blanket impl in zeroclaw-types.

    /// Mock model_provider that implements `chat()` with native tool support.
    struct NativeToolMock {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response_text: &'static str,
        tool_calls: Vec<super::super::traits::ToolCall>,
        error: &'static str,
    }

    #[async_trait]
    impl ModelProvider for NativeToolMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(self.response_text.to_string())
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(self.error);
            }
            Ok(ChatResponse {
                text: Some(self.response_text.to_string()),
                tool_calls: self.tool_calls.clone(),
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for NativeToolMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "NativeToolMock"
        }
    }

    #[tokio::test]
    async fn chat_delegates_to_inner_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_call = super::super::traits::ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"date"}"#.to_string(),
            extra_content: None,
        };
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(NativeToolMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response_text: "ok",
                    tool_calls: vec![tool_call.clone()],
                    error: "boom",
                }) as Box<dyn ModelProvider>,
            )],
            2,
            1,
        );

        let messages = vec![ChatMessage::user("what time is it?")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "test-model", Some(0.0))
            .await
            .unwrap();

        assert_eq!(result.text.as_deref(), Some("ok"));
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "shell");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn chat_retries_and_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_call = super::super::traits::ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"date"}"#.to_string(),
            extra_content: None,
        };
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(NativeToolMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 2,
                    response_text: "recovered",
                    tool_calls: vec![tool_call],
                    error: "temporary failure",
                }) as Box<dyn ModelProvider>,
            )],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("test")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "test-model", Some(0.0))
            .await
            .unwrap();

        assert_eq!(result.text.as_deref(), Some("recovered"));
        assert!(
            calls.load(Ordering::SeqCst) > 1,
            "should have retried at least once"
        );
    }

    #[tokio::test]
    async fn chat_preserves_native_tools_support() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(NativeToolMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response_text: "ok",
                    tool_calls: vec![],
                    error: "boom",
                }) as Box<dyn ModelProvider>,
            )],
            2,
            1,
        );

        assert!(
            model_provider.supports_native_tools(),
            "ReliableModelProvider must propagate supports_native_tools from inner model_provider"
        );
    }

    #[test]
    fn mixed_chain_disables_native_tools_before_dispatch() {
        let provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "native-primary".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response_text: "unused",
                        tool_calls: vec![],
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "text-fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response: "unused",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );

        assert!(
            !provider.supports_native_tools(),
            "a text-only fallback must select prompt-guided tools before dispatch"
        );
        assert!(
            !provider.capabilities().native_tool_calling,
            "Reliable capabilities must not advertise native tools that a fallback cannot accept"
        );
        assert!(
            !provider
                .capabilities_for_model("any-model")
                .native_tool_calling,
            "model-specific Reliable capabilities must not advertise native tools that a fallback cannot accept"
        );
        assert!(
            provider.has_mixed_native_tool_support_for_model("any-model"),
            "the strict-mode preflight must distinguish a mixed chain from a homogeneous text-only chain"
        );
    }

    // ── Gap 2-4: Parity tests for chat() ────────────────────────

    #[test]
    fn homogeneous_chains_do_not_report_mixed_native_tool_support() {
        let native_provider = ReliableModelProvider::new(
            "native",
            vec![
                (
                    "native-primary".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response_text: "unused",
                        tool_calls: vec![],
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "native-fallback".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response_text: "unused",
                        tool_calls: vec![],
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let text_provider = ReliableModelProvider::new(
            "text",
            vec![
                (
                    "text-primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response: "unused",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "text-fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: 0,
                        response: "unused",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );

        assert!(native_provider.supports_native_tools());
        assert!(
            !native_provider.has_mixed_native_tool_support_for_model("any-model"),
            "an all-native chain is homogeneous"
        );
        assert!(!text_provider.supports_native_tools());
        assert!(
            !text_provider.has_mixed_native_tool_support_for_model("any-model"),
            "an all-text chain is homogeneous and remains valid in strict mode"
        );
    }

    #[tokio::test]
    async fn chat_returns_aggregated_error_when_all_providers_fail() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "p1".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response_text: "never",
                        tool_calls: vec![],
                        error: "p1 chat error",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "p2".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response_text: "never",
                        tool_calls: vec![],
                        error: "p2 chat error",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let err = model_provider
            .chat(request, "test", Some(0.0))
            .await
            .expect_err("all model_providers should fail");
        let msg = err.to_string();
        assert!(msg.contains("All model providers/models failed after 2 failure event(s)"));
        assert!(msg.contains("event 1 (retry 1/1): retryable"));
        assert!(msg.contains("event 2 (retry 1/1): retryable"));
        assert!(!msg.contains("p1 chat error"));
        assert!(!msg.contains("p2 chat error"));
        assert!(msg.contains("retryable"));
    }

    /// Mock that records model names and can fail specific models,
    /// implementing `chat()` for native tool calling parity tests.
    struct NativeModelAwareMock {
        calls: Arc<AtomicUsize>,
        models_seen: parking_lot::Mutex<Vec<String>>,
        fail_models: Vec<&'static str>,
        response_text: &'static str,
    }

    #[async_trait]
    impl ModelProvider for NativeModelAwareMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(self.response_text.to_string())
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.models_seen.lock().push(model.to_string());
            if self.fail_models.contains(&model) {
                anyhow::bail!("500 model {} unavailable", model);
            }
            Ok(ChatResponse {
                text: Some(self.response_text.to_string()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for NativeModelAwareMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "NativeModelAwareMock"
        }
    }

    // Arc<NativeModelAwareMock> ModelProvider impl provided by blanket impl in zeroclaw-types.

    #[tokio::test]
    async fn chat_tries_model_failover_on_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(NativeModelAwareMock {
            calls: Arc::clone(&calls),
            models_seen: parking_lot::Mutex::new(Vec::new()),
            fail_models: vec!["claude-opus"],
            response_text: "ok from sonnet",
        });

        let mut fallbacks = HashMap::new();
        fallbacks.insert("claude-opus".to_string(), vec!["claude-sonnet".to_string()]);

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "anthropic".into(),
                Box::new(mock.clone()) as Box<dyn ModelProvider>,
            )],
            0, // no retries — force immediate model failover
            1,
        )
        .with_model_fallbacks(fallbacks);

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "claude-opus", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("ok from sonnet"));

        let seen = mock.models_seen.lock();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], "claude-opus");
        assert_eq!(seen[1], "claude-sonnet");
    }

    #[tokio::test]
    async fn chat_skips_non_retryable_errors() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response_text: "never",
                        tool_calls: vec![],
                        error: "401 Unauthorized",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response_text: "from fallback",
                        tool_calls: vec![],
                        error: "fallback err",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let result = model_provider
            .chat(request, "test", Some(0.0))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("from fallback"));
        // Primary should have been called only once (no retries)
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn terminal_provider_failure_names_the_final_fallback_candidate() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "401 Unauthorized",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "401 Unauthorized",
                    }),
                ),
            ],
            0,
            1,
        );

        let error = model_provider
            .simple_chat("hello", "test", Some(0.0))
            .await
            .expect_err("all configured candidates should fail");
        let failure = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ReliableProviderTerminalFailure>())
            .expect("terminal provider failure must retain its typed cause");

        assert_eq!(failure.provider(), Some("fallback"));
        assert_eq!(
            failure.kind(),
            ReliableProviderTerminalFailureKind::Authentication
        );
    }

    // ── Context window truncation tests ─────────────────────────

    #[test]
    fn context_window_error_is_not_non_retryable() {
        // Context window errors should be recoverable via truncation
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "exceeds the context window"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "maximum context length exceeded"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "too many tokens in the request"
        )));
        assert!(!is_non_retryable(&anyhow::Error::msg(
            "token limit exceeded"
        )));
    }

    #[test]
    fn is_context_window_exceeded_detects_llamacpp() {
        assert!(is_context_window_exceeded(&anyhow::Error::msg(
            "request (8968 tokens) exceeds the available context size (8448 tokens), try increasing it"
        )));
    }

    #[test]
    fn is_context_window_exceeded_detects_nested_provider_cause() {
        let err = anyhow::Error::msg("maximum context length exceeded")
            .context("provider request failed after retry recovery");

        assert!(!err.to_string().contains("maximum context length"));
        assert!(is_context_window_exceeded(&err));
    }

    #[test]
    fn truncate_for_context_drops_oldest_non_system() {
        let mut messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("msg1"),
            ChatMessage::assistant("resp1"),
            ChatMessage::user("msg2"),
            ChatMessage::assistant("resp2"),
            ChatMessage::user("msg3"),
        ];

        let dropped = truncate_for_context(&mut messages);

        // 5 non-system messages, drop oldest half = 2
        assert_eq!(dropped, 2);
        // System message preserved
        assert_eq!(messages[0].role, "system");
        // Remaining messages should be the newer ones
        assert_eq!(messages.len(), 4); // system + 3 remaining non-system
        // The last message should still be the most recent user message
        assert_eq!(messages.last().unwrap().content, "msg3");
    }

    #[test]
    fn truncate_for_context_preserves_system_and_last_message() {
        // Only one non-system message: nothing to drop
        let mut messages = vec![ChatMessage::system("sys"), ChatMessage::user("only")];
        let dropped = truncate_for_context(&mut messages);
        assert_eq!(dropped, 0);
        assert_eq!(messages.len(), 2);

        // No system message, only one user message
        let mut messages = vec![ChatMessage::user("only")];
        let dropped = truncate_for_context(&mut messages);
        assert_eq!(dropped, 0);
        assert_eq!(messages.len(), 1);
    }

    fn native_tool_call(ids: &[&str]) -> ChatMessage {
        let tool_calls = ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "name": "shell",
                    "arguments": "{}",
                })
            })
            .collect::<Vec<_>>();
        ChatMessage::assistant(
            serde_json::json!({
                "content": "",
                "tool_calls": tool_calls,
            })
            .to_string(),
        )
    }

    fn native_tool_result(id: &str) -> ChatMessage {
        ChatMessage::tool(
            serde_json::json!({
                "tool_call_id": id,
                "content": format!("result for {id}"),
            })
            .to_string(),
        )
    }

    fn context_overflow_native_tool_history() -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old request"),
            ChatMessage::assistant("old response"),
            ChatMessage::user("run both tools"),
            native_tool_call(&["call_1", "call_2"]),
            native_tool_result("call_1"),
            native_tool_result("call_2"),
            ChatMessage::assistant("tool summary"),
            ChatMessage::user("recent request"),
            ChatMessage::assistant("recent response"),
            ChatMessage::user("current question"),
        ]
    }

    #[test]
    fn truncate_for_context_drops_complete_parallel_native_tool_turn() {
        let mut messages = context_overflow_native_tool_history();

        let dropped = truncate_for_context(&mut messages);

        assert_eq!(dropped, 7);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].content, "recent request");
        assert_eq!(messages[2].content, "recent response");
        assert_eq!(messages[3].content, "current question");
        assert!(messages.iter().all(|message| message.role != "tool"));
    }

    #[test]
    fn truncate_for_context_retains_complete_latest_parallel_native_tool_turn() {
        let mut messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old request"),
            ChatMessage::assistant("old response"),
            ChatMessage::user("run both tools"),
            native_tool_call(&["call_1", "call_2"]),
            native_tool_result("call_1"),
            native_tool_result("call_2"),
        ];

        let dropped = truncate_for_context(&mut messages);

        assert_eq!(dropped, 2);
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[1].content, "run both tools");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[4].role, "tool");
    }

    #[test]
    fn truncate_for_context_does_not_split_only_native_tool_turn() {
        let mut messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("current request"),
            native_tool_call(&["call_1", "call_2"]),
            native_tool_result("call_1"),
            native_tool_result("call_2"),
        ];
        let original = messages.clone();

        let dropped = truncate_for_context(&mut messages);

        assert_eq!(dropped, 0);
        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
    }

    #[test]
    fn truncate_for_context_treats_prompt_tool_results_as_part_of_turn() {
        let mut messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old request"),
            ChatMessage::assistant("old response"),
            ChatMessage::user("run a prompt tool"),
            ChatMessage::assistant("<tool_call>...</tool_call>"),
            ChatMessage::user("[Tool results]\n<tool_result>ok</tool_result>"),
            ChatMessage::assistant("tool summary"),
            ChatMessage::user("current question"),
        ];

        let dropped = truncate_for_context(&mut messages);

        assert_eq!(dropped, 6);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].content, "current question");
    }

    struct NativeContextOverflowMock {
        calls: Arc<AtomicUsize>,
        histories: Arc<parking_lot::Mutex<Vec<Vec<ChatMessage>>>>,
    }

    #[async_trait]
    impl ModelProvider for NativeContextOverflowMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.histories.lock().push(request.messages.to_vec());
            if attempt == 1 {
                anyhow::bail!("maximum context length exceeded");
            }
            Ok(ChatResponse {
                text: Some("recovered".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for NativeContextOverflowMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "NativeContextOverflowMock"
        }
    }

    #[tokio::test]
    async fn chat_context_overflow_retry_sends_complete_native_tool_turns() {
        let calls = Arc::new(AtomicUsize::new(0));
        let histories = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mock = NativeContextOverflowMock {
            calls: Arc::clone(&calls),
            histories: Arc::clone(&histories),
        };
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![("local".into(), Box::new(mock) as Box<dyn ModelProvider>)],
            2,
            1,
        );
        let messages = context_overflow_native_tool_history();
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };

        let response = model_provider
            .chat(request, "local-model", Some(0.0))
            .await
            .unwrap();

        assert_eq!(response.text.as_deref(), Some("recovered"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let histories = histories.lock();
        assert_eq!(histories.len(), 2);
        assert_eq!(histories[1][1].content, "recent request");
        assert!(histories[1].iter().all(|message| message.role != "tool"));
    }

    /// Mock that fails with context error on first N calls, then succeeds.
    /// Tracks the number of messages received on each call.
    struct ContextOverflowMock {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        post_context_error: Option<&'static str>,
        message_counts: parking_lot::Mutex<Vec<usize>>,
    }

    impl ContextOverflowMock {
        fn record_attempt(&self, message_count: usize) -> anyhow::Result<()> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            self.message_counts.lock().push(message_count);
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(
                    "request (8968 tokens) exceeds the available context size (8448 tokens), try increasing it"
                );
            }
            if let Some(error) = self.post_context_error {
                anyhow::bail!(error);
            }
            Ok(())
        }
    }

    #[async_trait]
    impl ModelProvider for ContextOverflowMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.record_attempt(0)?;
            Ok("recovered after truncation".to_string())
        }

        async fn chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.record_attempt(messages.len())?;
            Ok("recovered after truncation".to_string())
        }

        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.record_attempt(messages.len())?;
            Ok(ChatResponse {
                text: Some("recovered after truncation".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.record_attempt(request.messages.len())?;
            Ok(ChatResponse {
                text: Some("recovered after truncation".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ContextOverflowMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ContextOverflowMock"
        }
    }

    fn all_non_stream_context_overflow_provider(calls: Arc<AtomicUsize>) -> ReliableModelProvider {
        ReliableModelProvider::new(
            "test",
            vec![(
                "local".into(),
                Box::new(ContextOverflowMock {
                    calls,
                    fail_until_attempt: usize::MAX,
                    post_context_error: None,
                    message_counts: parking_lot::Mutex::new(Vec::new()),
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        )
    }

    fn assert_single_safe_context_failure(err: anyhow::Error, calls: &AtomicUsize) {
        let msg = err.to_string();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(msg.contains("after 1 failure event(s)"), "{msg}");
        assert!(msg.contains("event 1 (retry 1/1): context_window"), "{msg}");
        assert!(!msg.contains("8968 tokens"));
        assert!(!msg.contains("8448 tokens"));
    }

    #[tokio::test]
    async fn chat_with_history_truncates_on_context_overflow() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = ContextOverflowMock {
            calls: Arc::clone(&calls),
            fail_until_attempt: 1, // fail first call, succeed after truncation
            post_context_error: None,
            message_counts: parking_lot::Mutex::new(Vec::new()),
        };

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![("local".into(), Box::new(mock) as Box<dyn ModelProvider>)],
            3,
            1,
        );

        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("old message 1"),
            ChatMessage::assistant("old response 1"),
            ChatMessage::user("old message 2"),
            ChatMessage::assistant("old response 2"),
            ChatMessage::user("current question"),
        ];

        let (result, fallback, context_truncated) = scope_provider_fallback(async {
            let result = model_provider
                .chat_with_history(&messages, "local-model", Some(0.0))
                .await
                .unwrap();
            (
                result,
                take_last_provider_fallback(),
                take_last_provider_context_truncation(),
            )
        })
        .await;
        assert_eq!(result, "recovered after truncation");
        // Should have been called twice: once with full messages, once with truncated
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            fallback.is_none(),
            "same-candidate context recovery is not user-visible fallback attribution"
        );
        assert!(
            context_truncated,
            "the internal provenance signal must remain available for cache safety"
        );
    }

    #[tokio::test]
    async fn all_non_stream_context_overflows_with_zero_retries_report_one_attempt() {
        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("old message"),
            ChatMessage::assistant("old response"),
            ChatMessage::user("current question"),
        ];

        let system_calls = Arc::new(AtomicUsize::new(0));
        let err = all_non_stream_context_overflow_provider(Arc::clone(&system_calls))
            .chat_with_system(None, "hello", "local-model", Some(0.0))
            .await
            .expect_err("the only system-chat call should overflow");
        assert_single_safe_context_failure(err, &system_calls);

        let history_calls = Arc::new(AtomicUsize::new(0));
        let err = all_non_stream_context_overflow_provider(Arc::clone(&history_calls))
            .chat_with_history(&messages, "local-model", Some(0.0))
            .await
            .expect_err("the only history-chat call should overflow");
        assert_single_safe_context_failure(err, &history_calls);

        let tool_calls = Arc::new(AtomicUsize::new(0));
        let err = all_non_stream_context_overflow_provider(Arc::clone(&tool_calls))
            .chat_with_tools(&messages, &[], "local-model", Some(0.0))
            .await
            .expect_err("the only tool-chat call should overflow");
        assert_single_safe_context_failure(err, &tool_calls);

        let chat_calls = Arc::new(AtomicUsize::new(0));
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            thinking: None,
        };
        let err = all_non_stream_context_overflow_provider(Arc::clone(&chat_calls))
            .chat(request, "local-model", Some(0.0))
            .await
            .expect_err("the only structured-chat call should overflow");
        assert_single_safe_context_failure(err, &chat_calls);
    }

    #[tokio::test]
    async fn context_truncation_then_failure_reports_both_events_in_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = ContextOverflowMock {
            calls: Arc::clone(&calls),
            fail_until_attempt: 1,
            post_context_error: Some("sensitive final provider response body: secret-token"),
            message_counts: parking_lot::Mutex::new(Vec::new()),
        };
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![("local".into(), Box::new(mock) as Box<dyn ModelProvider>)],
            1,
            1,
        );
        let messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("old message"),
            ChatMessage::assistant("old response"),
            ChatMessage::user("current question"),
        ];

        let err = model_provider
            .chat_with_history(&messages, "local-model", Some(0.0))
            .await
            .expect_err("both provider calls should overflow");
        let msg = err.to_string();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(msg.contains("after 2 failure event(s)"), "{msg}");
        let first = msg
            .find("event 1 (retry 1/2): context_window")
            .expect("first overflow event should be retained");
        let second = msg
            .find("event 2 (retry 2/2): retryable")
            .expect("post-truncation failure should be retained");
        assert!(first < second, "events were reordered: {msg}");
        assert!(msg.contains("kind=provider_error"));
        assert!(!msg.contains("8968 tokens"));
        assert!(!msg.contains("8448 tokens"));
        assert!(!msg.contains("sensitive final provider response body"));
        assert!(!msg.contains("secret-token"));
    }

    #[tokio::test]
    async fn context_overflow_with_no_history_to_truncate_bails_immediately() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = ContextOverflowMock {
            calls: Arc::clone(&calls),
            fail_until_attempt: 999, // always fail
            post_context_error: None,
            message_counts: parking_lot::Mutex::new(Vec::new()),
        };

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![("local".into(), Box::new(mock) as Box<dyn ModelProvider>)],
            3,
            1,
        );

        // Only system + one user message — nothing to truncate
        let messages = vec![
            ChatMessage::system("huge system prompt that exceeds context window"),
            ChatMessage::user("hello"),
        ];

        let result = model_provider
            .chat_with_history(&messages, "local-model", Some(0.0))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("without breaking message/tool pairing"),
            "Should bail with actionable message, got: {err_msg}"
        );
        // Should only be called once — no useless retries
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "Should not retry when truncation is impossible"
        );
    }

    // ── Tool schema error detection tests ───────────────────────────────

    #[test]
    fn tool_schema_error_detects_groq_validation_failure() {
        let msg = r#"Groq API error (400 Bad Request): {"error":{"message":"tool call validation failed: attempted to call tool 'memory_recall' which was not in request"}}"#;
        let err = anyhow::Error::msg(msg.to_string());
        assert!(is_tool_schema_error(&err));
    }

    #[test]
    fn tool_schema_error_detects_not_in_request() {
        let err = anyhow::Error::msg("tool 'search' was not in request");
        assert!(is_tool_schema_error(&err));
    }

    #[test]
    fn tool_schema_error_detects_not_found_in_tool_list() {
        let err = anyhow::Error::msg("function 'foo' not found in tool list");
        assert!(is_tool_schema_error(&err));
    }

    #[test]
    fn tool_schema_error_detects_invalid_tool_call() {
        let err = anyhow::Error::msg("invalid_tool_call: no matching function");
        assert!(is_tool_schema_error(&err));
    }

    #[test]
    fn tool_schema_error_ignores_unrelated_errors() {
        let err = anyhow::Error::msg("invalid api key");
        assert!(!is_tool_schema_error(&err));

        let err = anyhow::Error::msg("model not found");
        assert!(!is_tool_schema_error(&err));
    }

    #[test]
    fn non_retryable_returns_false_for_tool_schema_400() {
        // A 400 error with tool schema validation text should NOT be non-retryable.
        let msg = "400 Bad Request: tool call validation failed: attempted to call tool 'x' which was not in request";
        let err = anyhow::Error::msg(msg.to_string());
        assert!(!is_non_retryable(&err));
    }

    #[test]
    fn non_retryable_returns_true_for_other_400_errors() {
        // A regular 400 error (e.g. invalid API key) should still be non-retryable.
        let err = anyhow::Error::msg("400 Bad Request: invalid api key provided");
        assert!(is_non_retryable(&err));
    }

    #[test]
    fn malformed_stream_parser_error_is_not_treated_as_a_model_failure() {
        for payload in [
            "503 Service Unavailable",
            "model mystery is unknown",
            "unknown model",
        ] {
            let err = anyhow::Error::msg(format!(
                "model_provider stream error: JSON parse error: invalid type: string \"{payload}\", expected a sequence at line 1 column 36"
            ));

            assert!(!is_non_retryable(&err), "{payload}");
            assert_eq!(
                provider_error_diagnostic(&err).kind,
                "provider_error",
                "{payload}"
            );
        }
    }

    #[test]
    fn model_first_failure_phrases_remain_non_retryable_and_classified() {
        for message in [
            "model \"missing\" not found",
            "model \"mystery\" is unknown",
            "model unknown",
            "the requested model 'mystery' is unknown",
            "model \"legacy\" is unsupported",
            "model \"legacy\" is not supported",
            "model \"bad\" is invalid",
            "model \"gone\" does not exist",
        ] {
            let err = anyhow::Error::msg(message);

            assert!(is_non_retryable(&err), "{message}");
            assert_eq!(
                provider_error_diagnostic(&err).kind,
                "model_not_found",
                "{message}"
            );
        }
    }

    struct StreamingToolEventMock {
        stream_calls: Arc<AtomicUsize>,
        non_stream_calls: Arc<AtomicUsize>,
        supports_tool_events: bool,
    }

    impl StreamingToolEventMock {
        fn new(supports_tool_events: bool) -> Self {
            Self {
                stream_calls: Arc::new(AtomicUsize::new(0)),
                non_stream_calls: Arc::new(AtomicUsize::new(0)),
                supports_tool_events,
            }
        }
    }

    #[async_trait]
    impl ModelProvider for StreamingToolEventMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.non_stream_calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            self.supports_tool_events
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            stream::iter(vec![
                Ok(StreamEvent::ToolCall(super::super::traits::ToolCall {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    arguments: r#"{"command":"date"}"#.to_string(),
                    extra_content: None,
                })),
                Ok(StreamEvent::Final),
            ])
            .boxed()
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for StreamingToolEventMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamingToolEventMock"
        }
    }

    // Arc<StreamingToolEventMock> ModelProvider impl provided by blanket impl in zeroclaw-types.

    #[derive(Clone, Copy)]
    enum StreamingRecordMode {
        Success,
        Error,
        UsageThenError,
    }

    struct StreamingRecordMock {
        stream_calls: Arc<AtomicUsize>,
        supports: bool,
        mode: StreamingRecordMode,
    }

    struct StreamErrorNoChatReplayMock {
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    struct StreamThenChatErrorMock;

    impl ::zeroclaw_api::attribution::Attributable for StreamThenChatErrorMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "StreamThenChatErrorMock"
        }
    }

    #[async_trait]
    impl ModelProvider for StreamThenChatErrorMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("expected recovery failure")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("expected recovery failure")
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
        ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
            stream::iter(vec![Err(StreamingRecordMock::stream_error())]).boxed()
        }
    }

    impl StreamingRecordMock {
        fn success(stream_calls: Arc<AtomicUsize>) -> Self {
            Self {
                stream_calls,
                supports: true,
                mode: StreamingRecordMode::Success,
            }
        }

        fn unsupported(stream_calls: Arc<AtomicUsize>) -> Self {
            Self {
                stream_calls,
                supports: false,
                mode: StreamingRecordMode::Success,
            }
        }

        fn error(stream_calls: Arc<AtomicUsize>) -> Self {
            Self {
                stream_calls,
                supports: true,
                mode: StreamingRecordMode::Error,
            }
        }

        fn usage_then_error(stream_calls: Arc<AtomicUsize>) -> Self {
            Self {
                stream_calls,
                supports: true,
                mode: StreamingRecordMode::UsageThenError,
            }
        }

        fn stream_error() -> crate::traits::StreamError {
            crate::traits::StreamError::ModelProvider("stream failed".to_string())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for StreamErrorNoChatReplayMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "StreamErrorNoChatReplayMock"
        }
    }

    #[async_trait]
    impl ModelProvider for StreamingRecordMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn supports_streaming(&self) -> bool {
            self.supports
        }

        fn supports_streaming_tool_events(&self) -> bool {
            self.supports
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                StreamingRecordMode::Success => stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta("streamed"))),
                    Ok(StreamEvent::Final),
                ])
                .boxed(),
                StreamingRecordMode::Error => stream::iter(vec![Err(Self::stream_error())]).boxed(),
                StreamingRecordMode::UsageThenError => stream::iter(vec![
                    Ok(StreamEvent::Usage(TokenUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(5),
                        cached_input_tokens: None,
                    })),
                    Err(Self::stream_error()),
                ])
                .boxed(),
            }
        }

        fn stream_chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                StreamingRecordMode::Success => stream::iter(vec![
                    Ok(StreamChunk::delta("streamed")),
                    Ok(StreamChunk::final_chunk()),
                ])
                .boxed(),
                StreamingRecordMode::Error => stream::iter(vec![Err(Self::stream_error())]).boxed(),
                StreamingRecordMode::UsageThenError => {
                    stream::iter(vec![Err(Self::stream_error())]).boxed()
                }
            }
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                StreamingRecordMode::Success => stream::iter(vec![
                    Ok(StreamChunk::delta("streamed")),
                    Ok(StreamChunk::final_chunk()),
                ])
                .boxed(),
                StreamingRecordMode::Error => stream::iter(vec![Err(Self::stream_error())]).boxed(),
                StreamingRecordMode::UsageThenError => {
                    stream::iter(vec![Err(Self::stream_error())]).boxed()
                }
            }
        }
    }

    #[async_trait]
    impl ModelProvider for StreamErrorNoChatReplayMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("must not replay".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
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

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            stream::iter(vec![Err(StreamingRecordMock::stream_error())]).boxed()
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for StreamingRecordMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamingRecordMock"
        }
    }

    fn reliable_with_streaming_pinned_fallback(
        primary_calls: Arc<AtomicUsize>,
        fallback: StreamingRecordMock,
    ) -> ReliableModelProvider {
        ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new(
                    "primary",
                    "primary.key",
                    Box::new(StreamingRecordMock::unsupported(primary_calls))
                        as Box<dyn ModelProvider>,
                ),
                ReliableModelProviderEntry::new_pinned(
                    "fallback",
                    "fallback.key",
                    "fallback-alias",
                    "model-served",
                    Box::new(fallback) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        )
    }

    fn assert_streaming_fallback_record(fallback: ProviderFallbackInfo) {
        assert_eq!(fallback.requested_provider, "primary");
        assert_eq!(fallback.requested_model, "model-requested");
        assert_eq!(fallback.actual_provider, "fallback");
        assert_eq!(fallback.actual_model, "model-served");
    }

    #[tokio::test]
    async fn stream_chat_prefers_provider_with_tool_event_support() {
        let primary = Arc::new(StreamingToolEventMock::new(false));
        let fallback = Arc::new(StreamingToolEventMock::new(true));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(Arc::clone(&primary)) as Box<dyn ModelProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(Arc::clone(&fallback)) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![ToolSpec::new(
            "shell",
            "run shell",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                }
            }),
        )];
        let mut stream = model_provider.stream_chat(
            ChatRequest {
                messages: &messages,
                tools: Some(&tools),
                thinking: None,
            },
            "model",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        assert!(stream.next().await.is_none());

        match first {
            StreamEvent::ToolCall(call) => assert_eq!(call.name, "shell"),
            other => panic!("expected tool-call event, got {other:?}"),
        }
        assert!(matches!(second, StreamEvent::Final));
        assert_eq!(primary.stream_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback.stream_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_chat_records_pinned_fallback_on_success() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = reliable_with_streaming_pinned_fallback(
            Arc::clone(&primary_calls),
            StreamingRecordMock::success(Arc::clone(&fallback_calls)),
        );

        let messages = vec![ChatMessage::user("hello")];
        let fallback = scope_provider_fallback(async {
            let mut stream = model_provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "model-requested",
                Some(0.0),
                StreamOptions::new(true),
            );

            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                StreamEvent::TextDelta(_)
            ));
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                StreamEvent::Final
            ));
            take_last_provider_fallback()
        })
        .await
        .expect("successful fallback stream must record fallback info");

        assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_streaming_fallback_record(fallback);
    }

    #[tokio::test]
    async fn stream_chat_errors_when_no_provider_supports_tool_events() {
        let primary = Arc::new(StreamingToolEventMock::new(false));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(Arc::clone(&primary)) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![ToolSpec::new(
            "shell",
            "run shell",
            serde_json::json!({"type": "object"}),
        )];
        let mut stream = model_provider.stream_chat(
            ChatRequest {
                messages: &messages,
                tools: Some(&tools),
                thinking: None,
            },
            "model",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap();
        let err = first.expect_err("stream should fail without tool-event support");
        assert!(
            err.to_string()
                .contains("No model_provider supports streaming tool events"),
            "unexpected stream error: {err}"
        );
        assert!(stream.next().await.is_none());
        assert_eq!(primary.stream_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stream_chat_error_does_not_record_stale_fallback_info() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = reliable_with_streaming_pinned_fallback(
            Arc::clone(&primary_calls),
            StreamingRecordMock::error(Arc::clone(&fallback_calls)),
        );

        let messages = vec![ChatMessage::user("hello")];
        let fallback = scope_provider_fallback(async {
            let mut stream = model_provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "model-requested",
                Some(0.0),
                StreamOptions::new(true),
            );

            let first = stream.next().await.unwrap();
            assert!(first.is_err(), "stream must surface the provider error");
            assert!(stream.next().await.is_none());
            take_last_provider_fallback()
        })
        .await;

        assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert!(
            fallback.is_none(),
            "failed streams must not leave successful fallback info behind"
        );
    }

    #[tokio::test]
    async fn stream_chat_usage_then_error_does_not_record_stale_fallback_info() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = reliable_with_streaming_pinned_fallback(
            Arc::clone(&primary_calls),
            StreamingRecordMock::usage_then_error(Arc::clone(&fallback_calls)),
        );

        let messages = vec![ChatMessage::user("hello")];
        let fallback = scope_provider_fallback(async {
            let mut stream = model_provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "model-requested",
                Some(0.0),
                StreamOptions::new(true),
            );

            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                StreamEvent::Usage(_)
            ));
            assert!(stream.next().await.unwrap().is_err());
            assert!(stream.next().await.is_none());
            take_last_provider_fallback()
        })
        .await;

        assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert!(
            fallback.is_none(),
            "usage before a stream error must remain provisional"
        );
    }

    #[tokio::test]
    async fn stream_recovery_continues_after_the_selected_entry_without_replay() {
        let backup_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "stream-failure".into(),
                    Box::new(StreamingRecordMock::error(Arc::new(AtomicUsize::new(0))))
                        as Box<dyn ModelProvider>,
                ),
                (
                    "backup".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&backup_calls),
                        fail_until_attempt: 0,
                        response: "backup response",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];

        let (response, _) = scope_reliable_call_accounting(async {
            let mut stream = model_provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
                StreamOptions::new(true),
            );
            assert!(stream.next().await.unwrap().is_err());
            model_provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "test",
                    Some(0.0),
                )
                .await
        })
        .await;

        assert_eq!(response.unwrap().text.as_deref(), Some("backup response"));
        assert_eq!(backup_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_non_stream_recovery_is_a_second_canonical_leaf() {
        let provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "stream-physical".into(),
                    Box::new(StreamThenChatErrorMock) as Box<dyn ModelProvider>,
                ),
                (
                    "recovery-physical".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "expected recovery failure",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let scope = crate::dispatch::AccountedChatScope::new();
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
                assert!(stream.next().await.expect("stream error event").is_err());
                assert!(
                    ProviderDispatch::from_ref(&provider)
                        .chat(
                            ChatRequest {
                                messages: &messages,
                                tools: None,
                                thinking: None,
                            },
                            "served-model",
                            None,
                        )
                        .await
                        .is_err()
                );
            })
            .await;

        let report = scope.take();
        assert_eq!(report.attempts().len(), 2);
        assert_eq!(
            report
                .attempts()
                .iter()
                .map(|attempt| attempt.provider_ref())
                .collect::<Vec<_>>(),
            vec!["stream-physical", "recovery-physical"]
        );
        assert!(report.attempts().iter().all(|attempt| matches!(
            attempt.outcome(),
            crate::dispatch::AttemptUsageOutcome::OutcomeUnknown { observed: None }
        )));
    }

    #[tokio::test]
    async fn same_candidate_reliable_recovery_skip_creates_no_second_leaf() {
        let chat_calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "physical".into(),
                Box::new(StreamErrorNoChatReplayMock {
                    stream_calls: Arc::new(AtomicUsize::new(0)),
                    chat_calls: Arc::clone(&chat_calls),
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let scope = crate::dispatch::AccountedChatScope::new();
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
                assert!(stream.next().await.expect("stream error event").is_err());
                assert!(
                    ProviderDispatch::from_ref(&provider)
                        .chat(
                            ChatRequest {
                                messages: &messages,
                                tools: None,
                                thinking: None,
                            },
                            "served-model",
                            None,
                        )
                        .await
                        .is_err()
                );
            })
            .await;

        let report = scope.take();
        assert_eq!(chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.attempts().len(), 1);
        assert_eq!(report.attempts()[0].provider_ref(), "physical");
    }

    #[tokio::test]
    async fn unpolled_reliable_stream_creates_no_accounted_attempt() {
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(StreamingRecordMock::success(Arc::new(AtomicUsize::new(0))))
                    as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let (_, accounting) = scope_reliable_call_accounting(async {
            let stream = provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "served-model",
                Some(0.0),
                StreamOptions::new(true),
            );
            drop(stream);
        })
        .await;

        let (rejected, accepted) = accounting.into_parts();
        assert!(rejected.is_empty());
        assert!(accepted.is_none());
    }

    #[tokio::test]
    async fn no_eligible_reliable_stream_candidate_creates_no_wrapper_attempt() {
        let provider = ReliableModelProvider::new(
            "test",
            vec![(
                "non-streaming".into(),
                Box::new(MockModelProvider {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_until_attempt: 0,
                    response: "unused",
                    error: "unused",
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let scope = crate::dispatch::AccountedChatScope::new();

        scope
            .scope(async {
                let mut stream = ProviderDispatch::from_ref(&provider).stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "served-model",
                    Some(0.0),
                    StreamOptions::new(true),
                );
                assert!(stream.next().await.expect("synthetic error event").is_err());
            })
            .await;

        assert!(scope.take().attempts().is_empty());
    }

    #[tokio::test]
    async fn nested_reliable_stream_reports_only_the_inner_physical_leaf() {
        let inner = ReliableModelProvider::new(
            "inner",
            vec![(
                "inner.actual".into(),
                Box::new(StreamingRecordMock::success(Arc::new(AtomicUsize::new(0))))
                    as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let outer = ReliableModelProvider::new(
            "outer",
            vec![(
                "outer.wrapper".into(),
                Box::new(inner) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let scope = crate::dispatch::AccountedChatScope::new();

        scope
            .scope(async {
                let mut stream = ProviderDispatch::from_ref(&outer).stream_chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "served-model",
                    None,
                    StreamOptions::new(true),
                );
                let mut saw_final = false;
                while let Some(event) = stream.next().await {
                    saw_final |=
                        matches!(event.expect("successful nested stream"), StreamEvent::Final);
                }
                assert!(saw_final, "the physical leaf must reach Final");
            })
            .await;

        let report = scope.take();
        assert_eq!(report.attempts().len(), 1);
        let leaf = &report.attempts()[0];
        assert_eq!(
            (leaf.provider_ref(), leaf.model()),
            ("inner.actual", "served-model")
        );
        assert!(matches!(
            leaf.outcome(),
            crate::dispatch::AttemptUsageOutcome::Missing
        ));
        let accepted = report
            .accepted_route()
            .expect("Final marks the inner physical route accepted");
        assert_eq!(
            (accepted.provider_ref(), accepted.model()),
            ("inner.actual", "served-model")
        );
    }

    #[tokio::test]
    async fn semantic_stream_recovery_skips_only_failed_entry_and_uses_later_candidate() {
        let earlier_chat_calls = Arc::new(AtomicUsize::new(0));
        let failed_stream_calls = Arc::new(AtomicUsize::new(0));
        let failed_chat_calls = Arc::new(AtomicUsize::new(0));
        let later_chat_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "earlier-no-stream".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&earlier_chat_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "earlier failure",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "duplicate-display".into(),
                    Box::new(StreamErrorNoChatReplayMock {
                        stream_calls: Arc::clone(&failed_stream_calls),
                        chat_calls: Arc::clone(&failed_chat_calls),
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "duplicate-display".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&later_chat_calls),
                        fail_until_attempt: 0,
                        response: "later response",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];

        let (response, _) = scope_reliable_call_accounting(async {
            let mut stream = model_provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "test",
                Some(0.0),
                StreamOptions::new(true),
            );
            assert!(stream.next().await.unwrap().is_err());
            mark_stream_recovery_semantic_empty();
            model_provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "test",
                    Some(0.0),
                )
                .await
        })
        .await;

        assert_eq!(response.unwrap().text.as_deref(), Some("later response"));
        assert_eq!(earlier_chat_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(later_chat_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn router_preserves_exact_stream_recovery_identity() {
        let earlier_chat_calls = Arc::new(AtomicUsize::new(0));
        let failed_stream_calls = Arc::new(AtomicUsize::new(0));
        let failed_chat_calls = Arc::new(AtomicUsize::new(0));
        let later_chat_calls = Arc::new(AtomicUsize::new(0));
        let reliable = ReliableModelProvider::new(
            "inner",
            vec![
                (
                    "duplicate-display".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&earlier_chat_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "earlier failure",
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "duplicate-display".into(),
                    Box::new(StreamErrorNoChatReplayMock {
                        stream_calls: Arc::clone(&failed_stream_calls),
                        chat_calls: Arc::clone(&failed_chat_calls),
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "duplicate-display".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&later_chat_calls),
                        fail_until_attempt: 0,
                        response: "later response",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let router = RouterModelProvider::new(
            "router",
            vec![(
                "reliable".to_string(),
                Box::new(reliable) as Box<dyn ModelProvider>,
            )],
            vec![(
                "route".to_string(),
                Route {
                    provider_name: "reliable".to_string(),
                    model: "inner-model".to_string(),
                },
            )],
            "inner-model".to_string(),
        );
        let messages = vec![ChatMessage::user("hello")];

        let (response, _) = scope_reliable_call_accounting(async {
            let mut stream = router.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "hint:route",
                Some(0.0),
                StreamOptions::new(true),
            );
            assert!(stream.next().await.unwrap().is_err());
            router
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: None,
                        thinking: None,
                    },
                    "hint:route",
                    Some(0.0),
                )
                .await
        })
        .await;

        assert_eq!(response.unwrap().text.as_deref(), Some("later response"));
        assert_eq!(earlier_chat_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(later_chat_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_recovery_keeps_earlier_tool_stream_ineligible_entry_eligible() {
        let earlier = Arc::new(StreamingToolEventMock::new(false));
        let failed_stream_calls = Arc::new(AtomicUsize::new(0));
        let failed_chat_calls = Arc::new(AtomicUsize::new(0));
        let later_chat_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "earlier-no-tool-events".into(),
                    Box::new(Arc::clone(&earlier)) as Box<dyn ModelProvider>,
                ),
                (
                    "failed-tool-stream".into(),
                    Box::new(StreamErrorNoChatReplayMock {
                        stream_calls: Arc::clone(&failed_stream_calls),
                        chat_calls: Arc::clone(&failed_chat_calls),
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "later".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&later_chat_calls),
                        fail_until_attempt: 0,
                        response: "later response",
                        error: "unused",
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![ToolSpec::new(
            "shell",
            "run shell",
            serde_json::json!({"type": "object"}),
        )];

        let (response, _) = scope_reliable_call_accounting(async {
            let mut stream = model_provider.stream_chat(
                ChatRequest {
                    messages: &messages,
                    tools: Some(&tools),
                    thinking: None,
                },
                "test",
                Some(0.0),
                StreamOptions::new(true),
            );
            assert!(stream.next().await.unwrap().is_err());
            model_provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: Some(&tools),
                        thinking: None,
                    },
                    "test",
                    Some(0.0),
                )
                .await
        })
        .await;

        assert_eq!(response.unwrap().text.as_deref(), Some("ok"));
        assert_eq!(earlier.stream_calls.load(Ordering::SeqCst), 0);
        assert_eq!(earlier.non_stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(later_chat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stream_chat_with_system_records_pinned_fallback_on_success() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = reliable_with_streaming_pinned_fallback(
            Arc::clone(&primary_calls),
            StreamingRecordMock::success(Arc::clone(&fallback_calls)),
        );

        let fallback = scope_provider_fallback(async {
            let mut stream = model_provider.stream_chat_with_system(
                Some("system"),
                "hello",
                "model-requested",
                Some(0.0),
                StreamOptions::new(true),
            );

            assert_eq!(stream.next().await.unwrap().unwrap().delta, "streamed");
            assert!(stream.next().await.unwrap().unwrap().is_final);
            take_last_provider_fallback()
        })
        .await
        .expect("successful fallback stream must record fallback info");

        assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_streaming_fallback_record(fallback);
    }

    // ── stream_chat_with_history failover tests ──────────────────────

    /// Mock model_provider that supports streaming via stream_chat_with_history.
    struct StreamingHistoryMock {
        stream_calls: Arc<AtomicUsize>,
        supports: bool,
    }

    #[async_trait]
    impl ModelProvider for StreamingHistoryMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn supports_streaming(&self) -> bool {
            self.supports
        }

        fn stream_chat_with_history(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            // Echo the number of messages as the delta to verify history was passed through
            let msg_count = messages.len().to_string();
            stream::iter(vec![
                Ok(StreamChunk::delta(msg_count)),
                Ok(StreamChunk::final_chunk()),
            ])
            .boxed()
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for StreamingHistoryMock {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamingHistoryMock"
        }
    }

    #[tokio::test]
    async fn stream_chat_with_history_delegates_to_streaming_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "primary".into(),
                Box::new(StreamingHistoryMock {
                    stream_calls: Arc::clone(&calls),
                    supports: true,
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );

        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("msg1"),
            ChatMessage::assistant("resp1"),
            ChatMessage::user("msg2"),
        ];
        let mut stream = model_provider.stream_chat_with_history(
            &messages,
            "model",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(
            first.delta, "4",
            "should pass all 4 messages to model_provider"
        );
        let second = stream.next().await.unwrap().unwrap();
        assert!(second.is_final);
        assert!(stream.next().await.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_chat_with_history_skips_non_streaming_providers() {
        let non_streaming_calls = Arc::new(AtomicUsize::new(0));
        let streaming_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "non-streaming".into(),
                    Box::new(StreamingHistoryMock {
                        stream_calls: Arc::clone(&non_streaming_calls),
                        supports: false,
                    }) as Box<dyn ModelProvider>,
                ),
                (
                    "streaming".into(),
                    Box::new(StreamingHistoryMock {
                        stream_calls: Arc::clone(&streaming_calls),
                        supports: true,
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let mut stream = model_provider.stream_chat_with_history(
            &messages,
            "model",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.delta, "1");
        assert_eq!(
            non_streaming_calls.load(Ordering::SeqCst),
            0,
            "non-streaming model_provider should be skipped"
        );
        assert_eq!(
            streaming_calls.load(Ordering::SeqCst),
            1,
            "streaming model_provider should be used"
        );
    }

    #[tokio::test]
    async fn stream_chat_with_history_skips_cooled_down_provider() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let model_provider = ReliableModelProvider::new_with_entries(
            "test",
            vec![
                ReliableModelProviderEntry::new(
                    "primary",
                    "openai.work",
                    Box::new(StreamingHistoryMock {
                        stream_calls: Arc::clone(&primary_calls),
                        supports: true,
                    }) as Box<dyn ModelProvider>,
                ),
                ReliableModelProviderEntry::new(
                    "fallback",
                    "anthropic.work",
                    Box::new(StreamingHistoryMock {
                        stream_calls: Arc::clone(&fallback_calls),
                        supports: true,
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        );
        let err = anyhow::Error::msg("429 Too Many Requests, Retry-After: 30");
        model_provider.set_rate_limit_cooldown("openai.work", &err);

        let messages = vec![ChatMessage::user("hello")];
        let mut stream = model_provider.stream_chat_with_history(
            &messages,
            "model",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.delta, "1");
        assert_eq!(
            primary_calls.load(Ordering::SeqCst),
            0,
            "cooled-down streaming provider should be skipped"
        );
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_chat_with_history_records_pinned_fallback_on_success() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let model_provider = reliable_with_streaming_pinned_fallback(
            Arc::clone(&primary_calls),
            StreamingRecordMock::success(Arc::clone(&fallback_calls)),
        );

        let messages = vec![ChatMessage::user("hello")];
        let fallback = scope_provider_fallback(async {
            let mut stream = model_provider.stream_chat_with_history(
                &messages,
                "model-requested",
                Some(0.0),
                StreamOptions::new(true),
            );

            assert_eq!(stream.next().await.unwrap().unwrap().delta, "streamed");
            assert!(stream.next().await.unwrap().unwrap().is_final);
            take_last_provider_fallback()
        })
        .await
        .expect("successful fallback stream must record fallback info");

        assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        assert_streaming_fallback_record(fallback);
    }

    #[tokio::test]
    async fn stream_chat_with_history_errors_when_no_provider_supports_streaming() {
        let model_provider = ReliableModelProvider::new(
            "test",
            vec![(
                "non-streaming".into(),
                Box::new(StreamingHistoryMock {
                    stream_calls: Arc::new(AtomicUsize::new(0)),
                    supports: false,
                }) as Box<dyn ModelProvider>,
            )],
            0,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let mut stream = model_provider.stream_chat_with_history(
            &messages,
            "model",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap();
        let err = first.expect_err("should fail when no model_provider supports streaming");
        assert!(
            err.to_string()
                .contains("No model_provider supports streaming"),
            "unexpected error: {err}"
        );
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn fallback_records_provider_fallback_info() {
        scope_provider_fallback(async {
            let model_provider = ReliableModelProvider::new(
                "test",
                vec![
                    (
                        "broken".into(),
                        Box::new(MockModelProvider {
                            calls: Arc::new(AtomicUsize::new(0)),
                            fail_until_attempt: 99, // always fail
                            response: "unused",
                            error: "401 Unauthorized",
                        }),
                    ),
                    (
                        "working".into(),
                        Box::new(MockModelProvider {
                            calls: Arc::new(AtomicUsize::new(0)),
                            fail_until_attempt: 0,
                            response: "hello from working",
                            error: "unused",
                        }),
                    ),
                ],
                2,
                1,
            );

            let resp = model_provider
                .simple_chat("hi", "test-model", Some(0.0))
                .await
                .unwrap();
            assert_eq!(resp, "hello from working");

            let fb = take_last_provider_fallback();
            assert!(fb.is_some(), "fallback info should be recorded");
            let fb = fb.unwrap();
            assert_eq!(fb.requested_provider, "broken");
            assert_eq!(fb.actual_provider, "working");
            assert_eq!(fb.actual_model, "test-model");

            // Second take should be None.
            assert!(take_last_provider_fallback().is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn later_primary_success_clears_stale_fallback_attribution() {
        scope_provider_fallback(async {
            let primary_calls = Arc::new(AtomicUsize::new(0));
            let backup_calls = Arc::new(AtomicUsize::new(0));
            let model_provider = ReliableModelProvider::new(
                "test",
                vec![
                    (
                        "primary".into(),
                        Box::new(MockModelProvider {
                            calls: Arc::clone(&primary_calls),
                            fail_until_attempt: 1,
                            response: "final primary response",
                            error: "503 Service Unavailable",
                        }),
                    ),
                    (
                        "backup".into(),
                        Box::new(MockModelProvider {
                            calls: Arc::clone(&backup_calls),
                            fail_until_attempt: 0,
                            response: "tool-call response",
                            error: "unused",
                        }),
                    ),
                ],
                0,
                1,
            );

            assert_eq!(
                model_provider
                    .simple_chat("first", "test-model", Some(0.0))
                    .await
                    .expect("fallback recovers the first request"),
                "tool-call response"
            );
            assert_eq!(
                model_provider
                    .simple_chat("second", "test-model", Some(0.0))
                    .await
                    .expect("primary recovers for the second request"),
                "final primary response"
            );
            assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
            assert_eq!(backup_calls.load(Ordering::SeqCst), 1);
            assert!(
                take_last_provider_fallback().is_none(),
                "the final primary request must clear fallback attribution"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn retry_on_same_candidate_does_not_record_fallback_info() {
        scope_provider_fallback(async {
            let calls = Arc::new(AtomicUsize::new(0));
            let model_provider = ReliableModelProvider::new(
                "test",
                vec![(
                    "primary".into(),
                    Box::new(MockModelProvider {
                        calls: Arc::clone(&calls),
                        fail_until_attempt: 1,
                        response: "recovered on retry",
                        error: "503 Service Unavailable",
                    }),
                )],
                1,
                1,
            );

            let response = model_provider
                .simple_chat("hi", "test-model", Some(0.0))
                .await
                .expect("same candidate retry recovers");

            assert_eq!(response, "recovered on retry");
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(
                take_last_provider_fallback().is_none(),
                "retrying the exact candidate must not be reported as fallback"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn later_duplicate_candidate_records_fallback_info() {
        scope_provider_fallback(async {
            let model_provider = ReliableModelProvider::new(
                "test",
                vec![
                    (
                        "duplicate".into(),
                        Box::new(MockModelProvider {
                            calls: Arc::new(AtomicUsize::new(0)),
                            fail_until_attempt: usize::MAX,
                            response: "unused",
                            error: "503 Service Unavailable",
                        }),
                    ),
                    (
                        "duplicate".into(),
                        Box::new(MockModelProvider {
                            calls: Arc::new(AtomicUsize::new(0)),
                            fail_until_attempt: 0,
                            response: "recovered on later duplicate",
                            error: "unused",
                        }),
                    ),
                ],
                0,
                1,
            );

            let response = model_provider
                .simple_chat("hi", "test-model", Some(0.0))
                .await
                .expect("later duplicate candidate recovers");

            assert_eq!(response, "recovered on later duplicate");
            let fallback = take_last_provider_fallback()
                .expect("later configured candidate must be reported as fallback");
            assert_eq!(fallback.requested_provider, "duplicate");
            assert_eq!(fallback.actual_provider, "duplicate");
            assert_eq!(fallback.requested_model, "test-model");
            assert_eq!(fallback.actual_model, "test-model");
        })
        .await;
    }

    // Vision must be safe for every provider the request can reach. Unlike
    // native tools, a fallback cannot recover after receiving an unsupported
    // image payload, so mixed chains report non-vision at the outer gate.
    #[test]
    fn supports_vision_requires_every_fallback_to_support_images() {
        struct VisionMock(bool);

        #[async_trait]
        impl ModelProvider for VisionMock {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                Ok(String::new())
            }

            fn supports_vision(&self) -> bool {
                self.0
            }
        }
        impl ::zeroclaw_api::attribution::Attributable for VisionMock {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "VisionMock"
            }
        }

        let provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(VisionMock(false)) as Box<dyn ModelProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(VisionMock(true)) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            0,
        );

        assert!(
            !provider.supports_vision(),
            "ReliableModelProvider with non-vision primary must report supports_vision()=false even when a fallback supports vision"
        );

        let provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(VisionMock(true)) as Box<dyn ModelProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(VisionMock(false)) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            0,
        );

        assert!(
            !provider.supports_vision(),
            "a text-only fallback makes the effective chain non-vision even when the primary supports images"
        );

        let provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(VisionMock(true)) as Box<dyn ModelProvider>,
                ),
                (
                    "fallback".into(),
                    Box::new(VisionMock(true)) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            0,
        );
        assert!(provider.supports_vision());
    }

    #[tokio::test]
    async fn model_capability_rejects_images_before_text_only_fallback_dispatch() {
        struct VisionDispatchMock {
            vision: bool,
            fail: bool,
            calls: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for VisionDispatchMock {
            fn capabilities(&self) -> crate::traits::ProviderCapabilities {
                crate::traits::ProviderCapabilities {
                    vision: self.vision,
                    ..Default::default()
                }
            }

            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail {
                    anyhow::bail!("503 unavailable");
                }
                Ok("fallback".to_string())
            }
        }
        impl ::zeroclaw_api::attribution::Attributable for VisionDispatchMock {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "VisionDispatchMock"
            }
        }

        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableModelProvider::new(
            "test",
            vec![
                (
                    "primary".into(),
                    Box::new(VisionDispatchMock {
                        vision: true,
                        fail: true,
                        calls: Arc::clone(&primary_calls),
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(VisionDispatchMock {
                        vision: false,
                        fail: false,
                        calls: Arc::clone(&fallback_calls),
                    }),
                ),
            ],
            0,
            1,
        );

        assert!(
            !provider.capabilities_for_model("requested-model").vision,
            "the pre-dispatch gate must account for the text-only fallback"
        );
        assert_eq!(
            provider
                .simple_chat("hello", "requested-model", None)
                .await
                .expect("fallback succeeds"),
            "fallback"
        );
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fallback_calls.load(Ordering::SeqCst),
            1,
            "the text-only provider is an actual reachable dispatch target"
        );
    }

    #[test]
    fn capabilities_vision_matches_supports_vision_on_final_wrapped_reliable() {
        // Regression: the final wrapped ReliableModelProvider must report the SAME
        // `vision` on `capabilities().vision` and `supports_vision()`. Wrap the
        // config `vision` decorator forcing vision ON over a non-vision inner; the
        // outer surface must reflect it on BOTH accessors. Before `capabilities()`
        // delegated to the primary, the outer returned the trait default
        // (vision=false) and disagreed with the delegated `supports_vision()`.
        struct PlainMock;
        #[async_trait]
        impl ModelProvider for PlainMock {
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
        impl ::zeroclaw_api::attribution::Attributable for PlainMock {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "PlainMock"
            }
        }

        let inner = crate::vision_override::VisionOverrideProvider::new(
            Box::new(PlainMock) as Box<dyn ModelProvider>,
            true,
        );
        let provider = ReliableModelProvider::new(
            "test",
            vec![("primary".into(), Box::new(inner) as Box<dyn ModelProvider>)],
            0,
            0,
        );
        assert!(provider.supports_vision());
        assert!(
            provider.capabilities().vision,
            "outer capabilities().vision must match the delegated supports_vision()"
        );
        assert_eq!(provider.capabilities().vision, provider.supports_vision());
    }

    #[tokio::test]
    async fn reliable_wrapper_exposes_inner_provider_attribution() {
        use crate::ProviderDispatch;
        use std::sync::Arc;
        use zeroclaw_api::attribution::Attributable;

        let inner_mock = MockModelProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            fail_until_attempt: 0,
            response: "ok",
            error: "",
        };
        let inner_role = inner_mock.role();
        let inner_alias = inner_mock.alias().to_string();

        let reliable = ReliableModelProvider::new(
            "wrapped-alias",
            vec![("primary".into(), Box::new(inner_mock))],
            0,
            0,
        );
        // The wrapper must report the inner provider's role/alias,
        // not its own.
        assert_eq!(reliable.role(), inner_role, "wrapper must delegate role()",);
        assert_eq!(
            reliable.alias(),
            inner_alias,
            "wrapper must delegate alias()",
        );

        // End-to-end through ProviderDispatch: the captured event
        // must report the inner provider's `model_provider_type`,
        // never `reliable`.
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let reliable: Arc<dyn ModelProvider> = Arc::new(reliable);
        let dispatch = ProviderDispatch::new(reliable);
        let req = ChatRequest {
            messages: &[],
            tools: None,
            thinking: None,
        };
        let _ = dispatch.chat(req, "m", None).await;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found_type: Option<String> = None;
        while found_type.is_none() && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if let Some(zc) = value.get("zeroclaw")
                        && let Some(t) = zc.get("model_provider_type").and_then(|v| v.as_str())
                    {
                        found_type = Some(t.to_string());
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        assert_ne!(
            found_type.as_deref(),
            Some("reliable"),
            "ReliableModelProvider must not surface as model_provider_type=reliable",
        );
        zeroclaw_log::clear_broadcast_hook();
    }
}
