use super::traits::ChatMessage;
use super::Provider;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Check if an error is non-retryable (client errors that won't resolve with retries).
fn is_non_retryable(err: &anyhow::Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>() {
        if let Some(status) = reqwest_err.status() {
            let code = status.as_u16();
            return status.is_client_error() && code != 429 && code != 408;
        }
    }
    let msg = err.to_string();
    for word in msg.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = word.parse::<u16>() {
            if (400..500).contains(&code) {
                return code != 429 && code != 408;
            }
        }
    }
    false
}

/// Check if an error is a rate-limit (429) error.
fn is_rate_limited(err: &anyhow::Error) -> bool {
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>() {
        if let Some(status) = reqwest_err.status() {
            return status.as_u16() == 429;
        }
    }
    let msg = err.to_string();
    msg.contains("429")
        && (msg.contains("Too Many") || msg.contains("rate") || msg.contains("limit"))
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
            if let Ok(secs) = num_str.parse::<f64>() {
                if secs.is_finite() && secs >= 0.0 {
                    let millis = Duration::from_secs_f64(secs).as_millis();
                    if let Ok(value) = u64::try_from(millis) {
                        return Some(value);
                    }
                }
            }
        }
    }
    None
}

/// Provider wrapper with retry, fallback, auth rotation, and model failover.
pub struct ReliableProvider {
    providers: Vec<(String, Box<dyn Provider>)>,
    max_retries: u32,
    base_backoff_ms: u64,
    /// Extra API keys for rotation (index tracks round-robin position).
    api_keys: Vec<String>,
    key_index: AtomicUsize,
    /// Per-model fallback chains: model_name → [fallback_model_1, fallback_model_2, ...]
    model_fallbacks: HashMap<String, Vec<String>>,
}

impl ReliableProvider {
    pub fn new(
        providers: Vec<(String, Box<dyn Provider>)>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        Self {
            providers,
            max_retries,
            base_backoff_ms: base_backoff_ms.max(50),
            api_keys: Vec::new(),
            key_index: AtomicUsize::new(0),
            model_fallbacks: HashMap::new(),
        }
    }

    /// Set additional API keys for round-robin rotation on rate-limit errors.
    pub fn with_api_keys(mut self, keys: Vec<String>) -> Self {
        self.api_keys = keys;
        self
    }

    /// Set per-model fallback chains.
    pub fn with_model_fallbacks(mut self, fallbacks: HashMap<String, Vec<String>>) -> Self {
        self.model_fallbacks = fallbacks;
        self
    }

    /// Build the list of models to try: [original, fallback1, fallback2, ...]
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
}

#[async_trait]
impl Provider for ReliableProvider {
    async fn warmup(&self) -> anyhow::Result<()> {
        for (name, provider) in &self.providers {
            tracing::info!(provider = name, "Warming up provider connection pool");
            if let Err(e) = provider.warmup().await {
                tracing::warn!(provider = name, "Warmup failed (non-fatal): {e}");
            }
        }
        Ok(())
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let models = self.model_chain(model);
        let mut failures = Vec::new();

        for current_model in &models {
            for (provider_name, provider) in &self.providers {
                let mut backoff_ms = self.base_backoff_ms;

                for attempt in 0..=self.max_retries {
                    match provider
                        .chat_with_system(system_prompt, message, current_model, temperature)
                        .await
                    {
                        Ok(resp) => {
                            if attempt > 0 || *current_model != model {
                                tracing::info!(
                                    provider = provider_name,
                                    model = *current_model,
                                    attempt,
                                    original_model = model,
                                    "Provider recovered (failover/retry)"
                                );
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            let non_retryable = is_non_retryable(&e);
                            let rate_limited = is_rate_limited(&e);

                            failures.push(format!(
                                "{provider_name}/{current_model} attempt {}/{}: {e}",
                                attempt + 1,
                                self.max_retries + 1
                            ));

                            // On rate-limit, try rotating API key
                            if rate_limited {
                                if let Some(new_key) = self.rotate_key() {
                                    tracing::info!(
                                        provider = provider_name,
                                        "Rate limited, rotated API key (key ending ...{})",
                                        &new_key[new_key.len().saturating_sub(4)..]
                                    );
                                }
                            }

                            if non_retryable {
                                tracing::warn!(
                                    provider = provider_name,
                                    model = *current_model,
                                    "Non-retryable error, moving on"
                                );
                                break;
                            }

                            if attempt < self.max_retries {
                                let wait = self.compute_backoff(backoff_ms, &e);
                                tracing::warn!(
                                    provider = provider_name,
                                    model = *current_model,
                                    attempt = attempt + 1,
                                    backoff_ms = wait,
                                    "Provider call failed, retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(wait)).await;
                                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                            }
                        }
                    }
                }

                tracing::warn!(
                    provider = provider_name,
                    model = *current_model,
                    "Exhausted retries, trying next provider/model"
                );
            }

            if *current_model != model {
                tracing::warn!(
                    original_model = model,
                    fallback_model = *current_model,
                    "Model fallback exhausted all providers, trying next fallback model"
                );
            }
        }

        anyhow::bail!(
            "All providers/models failed. Attempts:\n{}",
            failures.join("\n")
        )
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let models = self.model_chain(model);
        let mut failures = Vec::new();

        for current_model in &models {
            for (provider_name, provider) in &self.providers {
                let mut backoff_ms = self.base_backoff_ms;

                for attempt in 0..=self.max_retries {
                    match provider
                        .chat_with_history(messages, current_model, temperature)
                        .await
                    {
                        Ok(resp) => {
                            if attempt > 0 || *current_model != model {
                                tracing::info!(
                                    provider = provider_name,
                                    model = *current_model,
                                    attempt,
                                    original_model = model,
                                    "Provider recovered (failover/retry)"
                                );
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            let non_retryable = is_non_retryable(&e);
                            let rate_limited = is_rate_limited(&e);

                            failures.push(format!(
                                "{provider_name}/{current_model} attempt {}/{}: {e}",
                                attempt + 1,
                                self.max_retries + 1
                            ));

                            if rate_limited {
                                if let Some(new_key) = self.rotate_key() {
                                    tracing::info!(
                                        provider = provider_name,
                                        "Rate limited, rotated API key (key ending ...{})",
                                        &new_key[new_key.len().saturating_sub(4)..]
                                    );
                                }
                            }

                            if non_retryable {
                                tracing::warn!(
                                    provider = provider_name,
                                    model = *current_model,
                                    "Non-retryable error, moving on"
                                );
                                break;
                            }

                            if attempt < self.max_retries {
                                let wait = self.compute_backoff(backoff_ms, &e);
                                tracing::warn!(
                                    provider = provider_name,
                                    model = *current_model,
                                    attempt = attempt + 1,
                                    backoff_ms = wait,
                                    "Provider call failed, retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(wait)).await;
                                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                            }
                        }
                    }
                }

                tracing::warn!(
                    provider = provider_name,
                    model = *current_model,
                    "Exhausted retries, trying next provider/model"
                );
            }
        }

        anyhow::bail!(
            "All providers/models failed. Attempts:\n{}",
            failures.join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response: &'static str,
        error: &'static str,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
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
            _temperature: f64,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(self.error);
            }
            Ok(self.response.to_string())
        }
    }

    /// Mock that records which model was used for each call.
    struct ModelAwareMock {
        calls: Arc<AtomicUsize>,
        models_seen: std::sync::Mutex<Vec<String>>,
        fail_models: Vec<&'static str>,
        response: &'static str,
    }

    #[async_trait]
    impl Provider for ModelAwareMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.models_seen.lock().unwrap().push(model.to_string());
            if self.fail_models.contains(&model) {
                anyhow::bail!("500 model {} unavailable", model);
            }
            Ok(self.response.to_string())
        }
    }

    // ── Existing tests (preserved) ──

    #[tokio::test]
    async fn succeeds_without_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "boom",
                }),
            )],
            2,
            1,
        );

        let result = provider.simple_chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    response: "recovered",
                    error: "temporary",
                }),
            )],
            2,
            1,
        );

        let result = provider.simple_chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn falls_back_after_retries_exhausted() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary down",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockProvider {
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

        let result = provider.simple_chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "from fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returns_aggregated_error_when_all_providers_fail() {
        let provider = ReliableProvider::new(
            vec![
                (
                    "p1".into(),
                    Box::new(MockProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p1 error",
                    }),
                ),
                (
                    "p2".into(),
                    Box::new(MockProvider {
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

        let err = provider
            .simple_chat("hello", "test", 0.0)
            .await
            .expect_err("all providers should fail");
        let msg = err.to_string();
        assert!(msg.contains("All providers/models failed"));
        assert!(msg.contains("p1"));
        assert!(msg.contains("p2"));
    }

    #[test]
    fn non_retryable_detects_common_patterns() {
        assert!(is_non_retryable(&anyhow::anyhow!("400 Bad Request")));
        assert!(is_non_retryable(&anyhow::anyhow!("401 Unauthorized")));
        assert!(is_non_retryable(&anyhow::anyhow!("403 Forbidden")));
        assert!(is_non_retryable(&anyhow::anyhow!("404 Not Found")));
        assert!(!is_non_retryable(&anyhow::anyhow!("429 Too Many Requests")));
        assert!(!is_non_retryable(&anyhow::anyhow!("408 Request Timeout")));
        assert!(!is_non_retryable(&anyhow::anyhow!(
            "500 Internal Server Error"
        )));
        assert!(!is_non_retryable(&anyhow::anyhow!("502 Bad Gateway")));
        assert!(!is_non_retryable(&anyhow::anyhow!("timeout")));
        assert!(!is_non_retryable(&anyhow::anyhow!("connection reset")));
    }

    #[tokio::test]
    async fn skips_retries_on_non_retryable_error() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "401 Unauthorized",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockProvider {
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

        let result = provider.simple_chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "from fallback");
        // Primary should have been called only once (no retries)
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn chat_with_history_retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
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
        let result = provider
            .chat_with_history(&messages, "test", 0.0)
            .await
            .unwrap();
        assert_eq!(result, "history ok");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chat_with_history_falls_back() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary down",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockProvider {
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
        let result = provider
            .chat_with_history(&messages, "test", 0.0)
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
            models_seen: std::sync::Mutex::new(Vec::new()),
            fail_models: vec!["claude-opus"],
            response: "ok from sonnet",
        });

        let mut fallbacks = HashMap::new();
        fallbacks.insert("claude-opus".to_string(), vec!["claude-sonnet".to_string()]);

        let provider = ReliableProvider::new(
            vec![(
                "anthropic".into(),
                Box::new(mock.clone()) as Box<dyn Provider>,
            )],
            0, // no retries — force immediate model failover
            1,
        )
        .with_model_fallbacks(fallbacks);

        let result = provider
            .simple_chat("hello", "claude-opus", 0.0)
            .await
            .unwrap();
        assert_eq!(result, "ok from sonnet");

        let seen = mock.models_seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], "claude-opus");
        assert_eq!(seen[1], "claude-sonnet");
    }

    #[tokio::test]
    async fn model_failover_all_models_fail() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(ModelAwareMock {
            calls: Arc::clone(&calls),
            models_seen: std::sync::Mutex::new(Vec::new()),
            fail_models: vec!["model-a", "model-b", "model-c"],
            response: "never",
        });

        let mut fallbacks = HashMap::new();
        fallbacks.insert(
            "model-a".to_string(),
            vec!["model-b".to_string(), "model-c".to_string()],
        );

        let provider = ReliableProvider::new(
            vec![("p1".into(), Box::new(mock.clone()) as Box<dyn Provider>)],
            0,
            1,
        )
        .with_model_fallbacks(fallbacks);

        let err = provider
            .simple_chat("hello", "model-a", 0.0)
            .await
            .expect_err("all models should fail");
        assert!(err.to_string().contains("All providers/models failed"));

        let seen = mock.models_seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
    }

    #[tokio::test]
    async fn no_model_fallbacks_behaves_like_before() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
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
        let result = provider.simple_chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // ── New tests: auth rotation ──

    #[tokio::test]
    async fn auth_rotation_cycles_keys() {
        let provider = ReliableProvider::new(
            vec![(
                "p".into(),
                Box::new(MockProvider {
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
        let keys: Vec<&str> = (0..5).map(|_| provider.rotate_key().unwrap()).collect();
        assert_eq!(keys, vec!["key-a", "key-b", "key-c", "key-a", "key-b"]);
    }

    #[tokio::test]
    async fn auth_rotation_returns_none_when_empty() {
        let provider = ReliableProvider::new(vec![], 0, 1);
        assert!(provider.rotate_key().is_none());
    }

    // ── New tests: Retry-After parsing ──

    #[test]
    fn parse_retry_after_integer() {
        let err = anyhow::anyhow!("429 Too Many Requests, Retry-After: 5");
        assert_eq!(parse_retry_after_ms(&err), Some(5000));
    }

    #[test]
    fn parse_retry_after_float() {
        let err = anyhow::anyhow!("Rate limited. retry_after: 2.5 seconds");
        assert_eq!(parse_retry_after_ms(&err), Some(2500));
    }

    #[test]
    fn parse_retry_after_missing() {
        let err = anyhow::anyhow!("500 Internal Server Error");
        assert_eq!(parse_retry_after_ms(&err), None);
    }

    #[test]
    fn rate_limited_detection() {
        assert!(is_rate_limited(&anyhow::anyhow!("429 Too Many Requests")));
        assert!(is_rate_limited(&anyhow::anyhow!(
            "HTTP 429 rate limit exceeded"
        )));
        assert!(!is_rate_limited(&anyhow::anyhow!("401 Unauthorized")));
        assert!(!is_rate_limited(&anyhow::anyhow!(
            "500 Internal Server Error"
        )));
    }

    #[test]
    fn compute_backoff_uses_retry_after() {
        let provider = ReliableProvider::new(vec![], 0, 500);
        let err = anyhow::anyhow!("429 Retry-After: 3");
        assert_eq!(provider.compute_backoff(500, &err), 3000);
    }

    #[test]
    fn compute_backoff_caps_at_30s() {
        let provider = ReliableProvider::new(vec![], 0, 500);
        let err = anyhow::anyhow!("429 Retry-After: 120");
        assert_eq!(provider.compute_backoff(500, &err), 30_000);
    }

    #[test]
    fn compute_backoff_falls_back_to_base() {
        let provider = ReliableProvider::new(vec![], 0, 500);
        let err = anyhow::anyhow!("500 Server Error");
        assert_eq!(provider.compute_backoff(500, &err), 500);
    }

    // ── Arc<ModelAwareMock> Provider impl for test ──

    #[async_trait]
    impl Provider for Arc<ModelAwareMock> {
        async fn chat_with_system(
            &self,
            system_prompt: Option<&str>,
            message: &str,
            model: &str,
            temperature: f64,
        ) -> anyhow::Result<String> {
            self.as_ref()
                .chat_with_system(system_prompt, message, model, temperature)
                .await
        }
    }
}
