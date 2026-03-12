//! Circuit breaker for wrapping fallible async calls with automatic failure detection
//! and recovery.
//!
//! States:
//! - **Closed** — normal operation; failures are counted.
//! - **Open** — too many failures; calls are rejected immediately.
//! - **HalfOpen** — recovery probe; a limited number of calls are allowed through.

use parking_lot::Mutex;
use std::time::{Duration, Instant};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — calls pass through and failures are counted.
    Closed,
    /// Circuit tripped — all calls are rejected until `recovery_timeout` elapses.
    Open,
    /// Testing recovery — up to `half_open_max_requests` calls pass through.
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::Open => write!(f, "open"),
            Self::HalfOpen => write!(f, "half-open"),
        }
    }
}

/// Returned when a call is rejected because the circuit is open.
#[derive(Debug, Clone)]
pub struct CircuitOpen {
    /// How long until the circuit transitions to half-open.
    pub retry_after: Duration,
}

impl std::fmt::Display for CircuitOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "circuit breaker open — retry after {:.1}s",
            self.retry_after.as_secs_f64()
        )
    }
}

impl std::error::Error for CircuitOpen {}

/// Configuration for a [`CircuitBreaker`].
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before the circuit opens.
    pub failure_threshold: u32,
    /// Duration to stay in the Open state before transitioning to HalfOpen.
    pub recovery_timeout: Duration,
    /// Maximum calls allowed through in the HalfOpen state to probe recovery.
    pub half_open_max_requests: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
            half_open_max_requests: 1,
        }
    }
}

/// Internal mutable state of the circuit breaker.
#[derive(Debug)]
struct Inner {
    state: CircuitState,
    consecutive_failures: u32,
    half_open_successes: u32,
    half_open_in_flight: u32,
    opened_at: Option<Instant>,
}

/// A circuit breaker that can wrap any fallible operation.
///
/// Use [`CircuitBreaker::call`] to execute an async closure through the breaker.
/// The breaker tracks consecutive failures and transitions between states
/// automatically.
#[derive(Debug)]
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(Inner {
                state: CircuitState::Closed,
                consecutive_failures: 0,
                half_open_successes: 0,
                half_open_in_flight: 0,
                opened_at: None,
            }),
        }
    }

    /// Returns the current circuit state.
    pub fn state(&self) -> CircuitState {
        let mut inner = self.inner.lock();
        self.maybe_transition_to_half_open(&mut inner);
        inner.state
    }

    /// Execute `f` through the circuit breaker.
    ///
    /// Returns `Err(anyhow::Error)` wrapping [`CircuitOpen`] if the circuit
    /// is open. Otherwise calls `f` and records the outcome.
    pub async fn call<F, Fut, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        // Acquire permission to call.
        let is_half_open_probe = {
            let mut inner = self.inner.lock();
            self.maybe_transition_to_half_open(&mut inner);

            match inner.state {
                CircuitState::Closed => false,
                CircuitState::Open => {
                    let elapsed = inner
                        .opened_at
                        .map(|t| t.elapsed())
                        .unwrap_or(Duration::ZERO);
                    let remaining = self.config.recovery_timeout.saturating_sub(elapsed);
                    return Err(CircuitOpen {
                        retry_after: remaining,
                    }
                    .into());
                }
                CircuitState::HalfOpen => {
                    if inner.half_open_in_flight + inner.half_open_successes
                        >= self.config.half_open_max_requests
                    {
                        return Err(CircuitOpen {
                            retry_after: Duration::from_secs(1),
                        }
                        .into());
                    }
                    inner.half_open_in_flight += 1;
                    true
                }
            }
        };

        // Execute the call outside the lock.
        let result = f().await;

        // Record outcome.
        let mut inner = self.inner.lock();
        if is_half_open_probe {
            inner.half_open_in_flight = inner.half_open_in_flight.saturating_sub(1);
        }

        match &result {
            Ok(_) => self.record_success(&mut inner, is_half_open_probe),
            Err(_) => self.record_failure(&mut inner),
        }

        result
    }

    fn maybe_transition_to_half_open(&self, inner: &mut Inner) {
        if inner.state == CircuitState::Open {
            if let Some(opened_at) = inner.opened_at {
                if opened_at.elapsed() >= self.config.recovery_timeout {
                    inner.state = CircuitState::HalfOpen;
                    inner.half_open_successes = 0;
                    inner.half_open_in_flight = 0;
                }
            }
        }
    }

    fn record_success(&self, inner: &mut Inner, is_half_open_probe: bool) {
        match inner.state {
            CircuitState::Closed => {
                inner.consecutive_failures = 0;
            }
            CircuitState::HalfOpen => {
                if is_half_open_probe {
                    inner.half_open_successes += 1;
                    if inner.half_open_successes >= self.config.half_open_max_requests {
                        // Recovery confirmed — close the circuit.
                        inner.state = CircuitState::Closed;
                        inner.consecutive_failures = 0;
                        inner.opened_at = None;
                    }
                }
            }
            CircuitState::Open => {}
        }
    }

    fn record_failure(&self, inner: &mut Inner) {
        match inner.state {
            CircuitState::Closed => {
                inner.consecutive_failures += 1;
                if inner.consecutive_failures >= self.config.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(Instant::now());
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open immediately re-opens.
                inner.state = CircuitState::Open;
                inner.opened_at = Some(Instant::now());
                inner.half_open_successes = 0;
            }
            CircuitState::Open => {}
        }
    }
}

// ── Provider wrapper ────────────────────────────────────────────────────
// Wraps any `Box<dyn Provider>` with circuit breaker protection on the
// two primary call paths: `chat_with_system` and `chat`.

use super::traits::{
    ChatRequest, ChatResponse, ProviderCapabilities, StreamChunk, StreamOptions, StreamResult,
    ToolsPayload,
};
use super::Provider;
use crate::tools::ToolSpec;
use async_trait::async_trait;
use futures_util::stream;
use std::sync::Arc;

/// A [`Provider`] decorator that guards calls with a [`CircuitBreaker`].
///
/// When the circuit is open, calls are rejected immediately with a
/// descriptive error instead of hitting the downstream provider.
pub struct CircuitBreakerProvider {
    inner: Box<dyn Provider>,
    breaker: Arc<CircuitBreaker>,
}

impl CircuitBreakerProvider {
    /// Wrap `inner` with a circuit breaker built from `config`.
    pub fn new(inner: Box<dyn Provider>, config: CircuitBreakerConfig) -> Self {
        Self {
            inner,
            breaker: Arc::new(CircuitBreaker::new(config)),
        }
    }

    /// Returns the current circuit state (useful for observability/tests).
    pub fn state(&self) -> CircuitState {
        self.breaker.state()
    }
}

#[async_trait]
impl Provider for CircuitBreakerProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        self.inner.convert_tools(tools)
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let sp = system_prompt.map(String::from);
        let msg = message.to_string();
        let mdl = model.to_string();
        let inner = &self.inner;
        self.breaker
            .call(|| async {
                inner
                    .chat_with_system(sp.as_deref(), &msg, &mdl, temperature)
                    .await
            })
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let messages = request.messages.to_vec();
        let tools: Option<Vec<ToolSpec>> = request.tools.map(|t| t.to_vec());
        let mdl = model.to_string();
        let inner = &self.inner;
        self.breaker
            .call(|| async {
                let req = ChatRequest {
                    messages: &messages,
                    tools: tools.as_deref(),
                };
                inner.chat(req, &mdl, temperature).await
            })
            .await
    }

    fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    fn supports_vision(&self) -> bool {
        self.inner.supports_vision()
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.inner.warmup().await
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        // Circuit breaker does not wrap streaming calls (they have their own
        // error handling). Delegate directly.
        self.inner
            .stream_chat_with_system(system_prompt, message, model, temperature, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_breaker(threshold: u32, recovery_ms: u64, half_open_max: u32) -> CircuitBreaker {
        CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: threshold,
            recovery_timeout: Duration::from_millis(recovery_ms),
            half_open_max_requests: half_open_max,
        })
    }

    #[tokio::test]
    async fn stays_closed_on_success() {
        let cb = make_breaker(3, 1000, 1);
        let result = cb.call(|| async { Ok::<_, anyhow::Error>(42) }).await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn opens_after_threshold_failures() {
        let cb = make_breaker(3, 1000, 1);

        for _ in 0..3 {
            let _ = cb
                .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
                .await;
        }

        assert_eq!(cb.state(), CircuitState::Open);

        // Next call should be rejected with CircuitOpen.
        let err = cb
            .call(|| async { Ok::<_, anyhow::Error>(1) })
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<CircuitOpen>().is_some());
    }

    #[tokio::test]
    async fn resets_failure_count_on_success() {
        let cb = make_breaker(3, 1000, 1);

        // 2 failures, then a success should reset the counter.
        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;
        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;
        let _ = cb.call(|| async { Ok::<_, anyhow::Error>(1) }).await;

        // 2 more failures should not open (counter was reset).
        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;
        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;

        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn transitions_to_half_open_after_timeout() {
        let cb = make_breaker(1, 50, 1);

        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for recovery timeout.
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[tokio::test]
    async fn half_open_success_closes_circuit() {
        let cb = make_breaker(1, 50, 1);

        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Successful probe should close the circuit.
        let _ = cb.call(|| async { Ok::<_, anyhow::Error>(1) }).await;
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn half_open_failure_reopens_circuit() {
        let cb = make_breaker(1, 50, 1);

        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail")) })
            .await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Failure in half-open reopens.
        let _ = cb
            .call(|| async { Err::<i32, _>(anyhow::anyhow!("fail again")) })
            .await;
        assert_eq!(cb.state(), CircuitState::Open);
    }

    // ── CircuitBreakerProvider integration tests ────────────────────────

    use super::super::traits::{ChatRequest, ChatResponse};
    use super::super::Provider;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic mock provider that fails the first N calls then succeeds.
    struct FailNProvider {
        fail_count: AtomicUsize,
        fail_limit: usize,
    }

    impl FailNProvider {
        fn new(fail_limit: usize) -> Self {
            Self {
                fail_count: AtomicUsize::new(0),
                fail_limit,
            }
        }
    }

    #[async_trait]
    impl Provider for FailNProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            let n = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_limit {
                anyhow::bail!("provider error #{}", n + 1);
            }
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let n = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_limit {
                anyhow::bail!("provider error #{}", n + 1);
            }
            Ok(ChatResponse {
                text: Some("ok".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn circuit_breaker_provider_opens_after_failures() {
        let provider = Box::new(FailNProvider::new(100)); // always fails
        let cb_provider = CircuitBreakerProvider::new(
            provider,
            CircuitBreakerConfig {
                failure_threshold: 3,
                recovery_timeout: Duration::from_secs(60),
                half_open_max_requests: 1,
            },
        );

        // First 3 calls fail normally (circuit still closed).
        for i in 1..=3 {
            let err = cb_provider
                .chat_with_system(None, "test", "model", 0.7)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains(&format!("provider error #{i}")),
                "expected provider error, got: {err}"
            );
        }

        // Circuit should now be open — next call rejected immediately.
        assert_eq!(cb_provider.state(), CircuitState::Open);
        let err = cb_provider
            .chat_with_system(None, "test", "model", 0.7)
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<CircuitOpen>().is_some(),
            "expected CircuitOpen error, got: {err}"
        );
    }

    #[tokio::test]
    async fn circuit_breaker_provider_passes_through_on_success() {
        let provider = Box::new(FailNProvider::new(0)); // always succeeds
        let cb_provider = CircuitBreakerProvider::new(
            provider,
            CircuitBreakerConfig {
                failure_threshold: 3,
                recovery_timeout: Duration::from_secs(60),
                half_open_max_requests: 1,
            },
        );

        let result = cb_provider
            .chat_with_system(None, "test", "model", 0.7)
            .await;
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(cb_provider.state(), CircuitState::Closed);
    }
}
