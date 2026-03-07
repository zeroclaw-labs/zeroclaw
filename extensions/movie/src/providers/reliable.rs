use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamOptions, StreamResult,
};
use super::Provider;
use async_trait::async_trait;
use futures_util::{stream, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// ── Error Classification ─────────────────────────────────────────────────
// Errors are split into retryable (transient server/network failures) and
// non-retryable (permanent client errors). This distinction drives whether
// the retry loop continues, falls back to the next provider, or aborts
// immediately — avoiding wasted latency on errors that cannot self-heal.

/// Check if an error is non-retryable (client errors that won't resolve with retries).
fn is_non_retryable(err: &anyhow::Error) -> bool {
    if is_context_window_exceeded(err) {
        return true;
    }

    let msg = err.to_string();
    let msg_lower = msg.to_lowercase();

    // Tool-schema/mapper incompatibility (including vendor 516 wrappers)
    // is deterministic: retries won't fix an unsupported request shape.
    if super::has_native_tool_schema_rejection_hint(&msg_lower) {
        return true;
    }

    // 4xx errors are generally non-retryable (bad request, auth failure, etc.),
    // except 429 (rate-limit — transient) and 408 (timeout — worth retrying).
    if let Some(reqwest_err) = err.downcast_ref::<reqwest::Error>() {
        if let Some(status) = reqwest_err.status() {
            let code = status.as_u16();
            return status.is_client_error() && code != 429 && code != 408;
        }
    }
    // Fallback: parse status codes from stringified errors (some providers
    // embed codes in error messages rather than returning typed HTTP errors).
    for word in msg.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = word.parse::<u16>() {
            if (400..500).contains(&code) {
                return code != 429 && code != 408;
            }
        }
    }

    // Heuristic: detect auth/model failures by keyword when no HTTP status
    // is available (e.g. gRPC or custom transport errors).
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

    msg_lower.contains("model")
        && (msg_lower.contains("not found")
            || msg_lower.contains("unknown")
            || msg_lower.contains("unsupported")
            || msg_lower.contains("does not exist")
            || msg_lower.contains("invalid"))
}

fn is_context_window_exceeded(err: &anyhow::Error) -> bool {
    let lower = err.to_string().to_lowercase();
    let hints = [
        "exceeds the context window",
        "context window of this model",
        "maximum context length",
        "context length exceeded",
        "too many tokens",
        "token limit exceeded",
        "prompt is too long",
        "input is too long",
    ];

    hints.iter().any(|hint| lower.contains(hint))
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

/// Check if a 429 is a business/quota-plan error that retries cannot fix.
///
/// Examples:
/// - plan does not include requested model
/// - insufficient balance / package not active
/// - known provider business codes (e.g. Z.AI: 1311, 1113)
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

    // Known provider business codes observed for 429 where retry is futile.
    for token in lower.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(code) = token.parse::<u16>() {
            if matches!(code, 1113 | 1311) {
                return true;
            }
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
    super::sanitize_api_error(&err.to_string())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_failure(
    failures: &mut Vec<String>,
    provider_name: &str,
    model: &str,
    attempt: u32,
    max_attempts: u32,
    reason: &str,
    error_detail: &str,
) {
    failures.push(format!(
        "provider={provider_name} model={model} attempt {attempt}/{max_attempts}: {reason}; error={error_detail}"
    ));
}

// ── Resilient Provider Wrapper ────────────────────────────────────────────
// Three-level failover strategy: model chain → provider chain → retry loop.
//   Outer loop:  iterate model fallback chain (original model first, then
//                configured alternatives).
//   Middle loop: iterate registered providers in priority order.
//   Inner loop:  retry the same (provider, model) pair with exponential
//                backoff, rotating API keys on rate-limit errors.
// Loop invariant: `failures` accumulates every failed attempt so the final
// error message gives operators a complete diagnostic trail.

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
    /// Provider-scoped model remaps: provider_name → [model_1, model_2, ...]
    provider_model_fallbacks: HashMap<String, Vec<String>>,
    /// Vision support override from config (`None` = defer to provider).
    vision_override: Option<bool>,
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
            provider_model_fallbacks: HashMap::new(),
            vision_override: None,
        }
    }

    /// Set additional API keys for round-robin rotation on rate-limit errors.
    pub fn with_api_keys(mut self, keys: Vec<String>) -> Self {
        self.api_keys = keys;
        self
    }

    /// Set per-model fallback chains.
    pub fn with_model_fallbacks(mut self, fallbacks: HashMap<String, Vec<String>>) -> Self {
        let provider_names: HashSet<&str> = self
            .providers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        self.model_fallbacks.clear();
        self.provider_model_fallbacks.clear();

        for (key, chain) in fallbacks {
            if provider_names.contains(key.as_str()) {
                self.provider_model_fallbacks.insert(key, chain);
            } else {
                self.model_fallbacks.insert(key, chain);
            }
        }

        self
    }

    /// Set vision support override from runtime config.
    pub fn with_vision_override(mut self, vision_override: Option<bool>) -> Self {
        self.vision_override = vision_override;
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

    /// Build provider-specific model candidates for this request.
    ///
    /// Compatibility behavior: keys in `model_fallbacks` that match provider names
    /// are interpreted as provider-scoped remap chains.
    fn provider_model_chain<'a>(
        &'a self,
        model: &'a str,
        provider_name: &str,
        is_primary_provider: bool,
    ) -> Vec<&'a str> {
        let mut chain = Vec::new();

        if is_primary_provider {
            chain.push(model);
        }

        if let Some(remaps) = self.provider_model_fallbacks.get(provider_name) {
            for remapped_model in remaps {
                let remapped_model = remapped_model.as_str();
                if !chain.contains(&remapped_model) {
                    chain.push(remapped_model);
                }
            }
        }

        if chain.is_empty() {
            chain.push(model);
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
            if provider.warmup().await.is_err() {
                tracing::warn!(provider = name, "Warmup failed (non-fatal)");
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

        // Outer: model fallback chain. Middle: provider priority. Inner: retries.
        // Each iteration: attempt one (provider, model) call. On success, return
        // immediately. On non-retryable error, break to next provider. On
        // retryable error, sleep with exponential backoff and retry.
        for current_model in &models {
            for (provider_index, (provider_name, provider)) in self.providers.iter().enumerate() {
                let sent_models =
                    self.provider_model_chain(current_model, provider_name, provider_index == 0);
                for sent_model in sent_models {
                    let mut backoff_ms = self.base_backoff_ms;

                    for attempt in 0..=self.max_retries {
                        match provider
                            .chat_with_system(system_prompt, message, sent_model, temperature)
                            .await
                        {
                            Ok(resp) => {
                                if attempt > 0 || sent_model != model {
                                    tracing::info!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt,
                                        original_model = model,
                                        "Provider recovered (failover/retry)"
                                    );
                                }
                                return Ok(resp);
                            }
                            Err(e) => {
                                let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                                let non_retryable =
                                    is_non_retryable(&e) || non_retryable_rate_limit;
                                let rate_limited = is_rate_limited(&e);
                                let failure_reason = failure_reason(rate_limited, non_retryable);
                                let error_detail = compact_error_detail(&e);

                                push_failure(
                                    &mut failures,
                                    provider_name,
                                    sent_model,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    failure_reason,
                                    &error_detail,
                                );

                                // Rate-limit with rotatable keys: cycle to the next API key
                                // so the retry hits a different quota bucket.
                                if rate_limited && !non_retryable_rate_limit {
                                    if let Some(new_key) = self.rotate_key() {
                                        tracing::warn!(
                                            provider = provider_name,
                                            error = %error_detail,
                                            "Rate limited; key rotation selected key ending ...{} \
                                             but cannot apply (Provider trait has no set_api_key). \
                                             Retrying with original key.",
                                            &new_key[new_key.len().saturating_sub(4)..]
                                        );
                                    }
                                }

                                if non_retryable {
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        error = %error_detail,
                                        "Non-retryable error, moving on"
                                    );

                                    if is_context_window_exceeded(&e) {
                                        anyhow::bail!(
                                            "Request exceeds model context window; retries and fallbacks were skipped. Attempts:\n{}",
                                            failures.join("\n")
                                        );
                                    }

                                    break;
                                }

                                if attempt < self.max_retries {
                                    let wait = self.compute_backoff(backoff_ms, &e);
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt = attempt + 1,
                                        backoff_ms = wait,
                                        reason = failure_reason,
                                        error = %error_detail,
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
                        model = sent_model,
                        "Exhausted retries, trying next provider/model"
                    );
                }
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
            for (provider_index, (provider_name, provider)) in self.providers.iter().enumerate() {
                let sent_models =
                    self.provider_model_chain(current_model, provider_name, provider_index == 0);
                for sent_model in sent_models {
                    let mut backoff_ms = self.base_backoff_ms;

                    for attempt in 0..=self.max_retries {
                        match provider
                            .chat_with_history(messages, sent_model, temperature)
                            .await
                        {
                            Ok(resp) => {
                                if attempt > 0 || sent_model != model {
                                    tracing::info!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt,
                                        original_model = model,
                                        "Provider recovered (failover/retry)"
                                    );
                                }
                                return Ok(resp);
                            }
                            Err(e) => {
                                let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                                let non_retryable =
                                    is_non_retryable(&e) || non_retryable_rate_limit;
                                let rate_limited = is_rate_limited(&e);
                                let failure_reason = failure_reason(rate_limited, non_retryable);
                                let error_detail = compact_error_detail(&e);

                                push_failure(
                                    &mut failures,
                                    provider_name,
                                    sent_model,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    failure_reason,
                                    &error_detail,
                                );

                                if rate_limited && !non_retryable_rate_limit {
                                    if let Some(new_key) = self.rotate_key() {
                                        tracing::warn!(
                                            provider = provider_name,
                                            error = %error_detail,
                                            "Rate limited; key rotation selected key ending ...{} \
                                             but cannot apply (Provider trait has no set_api_key). \
                                             Retrying with original key.",
                                            &new_key[new_key.len().saturating_sub(4)..]
                                        );
                                    }
                                }

                                if non_retryable {
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        error = %error_detail,
                                        "Non-retryable error, moving on"
                                    );

                                    if is_context_window_exceeded(&e) {
                                        anyhow::bail!(
                                            "Request exceeds model context window; retries and fallbacks were skipped. Attempts:\n{}",
                                            failures.join("\n")
                                        );
                                    }

                                    break;
                                }

                                if attempt < self.max_retries {
                                    let wait = self.compute_backoff(backoff_ms, &e);
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt = attempt + 1,
                                        backoff_ms = wait,
                                        reason = failure_reason,
                                        error = %error_detail,
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
                        model = sent_model,
                        "Exhausted retries, trying next provider/model"
                    );
                }
            }
        }

        anyhow::bail!(
            "All providers/models failed. Attempts:\n{}",
            failures.join("\n")
        )
    }

    fn supports_native_tools(&self) -> bool {
        self.providers
            .first()
            .map(|(_, p)| p.supports_native_tools())
            .unwrap_or(false)
    }

    fn supports_vision(&self) -> bool {
        self.vision_override.unwrap_or_else(|| {
            self.providers
                .iter()
                .any(|(_, provider)| provider.supports_vision())
        })
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let models = self.model_chain(model);
        let mut failures = Vec::new();

        for current_model in &models {
            for (provider_index, (provider_name, provider)) in self.providers.iter().enumerate() {
                let sent_models =
                    self.provider_model_chain(current_model, provider_name, provider_index == 0);
                for sent_model in sent_models {
                    let mut backoff_ms = self.base_backoff_ms;

                    for attempt in 0..=self.max_retries {
                        match provider
                            .chat_with_tools(messages, tools, sent_model, temperature)
                            .await
                        {
                            Ok(resp) => {
                                if attempt > 0 || sent_model != model {
                                    tracing::info!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt,
                                        original_model = model,
                                        "Provider recovered (failover/retry)"
                                    );
                                }
                                return Ok(resp);
                            }
                            Err(e) => {
                                let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                                let non_retryable =
                                    is_non_retryable(&e) || non_retryable_rate_limit;
                                let rate_limited = is_rate_limited(&e);
                                let failure_reason = failure_reason(rate_limited, non_retryable);
                                let error_detail = compact_error_detail(&e);

                                push_failure(
                                    &mut failures,
                                    provider_name,
                                    sent_model,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    failure_reason,
                                    &error_detail,
                                );

                                if rate_limited && !non_retryable_rate_limit {
                                    if let Some(new_key) = self.rotate_key() {
                                        tracing::warn!(
                                            provider = provider_name,
                                            error = %error_detail,
                                            "Rate limited; key rotation selected key ending ...{} \
                                             but cannot apply (Provider trait has no set_api_key). \
                                             Retrying with original key.",
                                            &new_key[new_key.len().saturating_sub(4)..]
                                        );
                                    }
                                }

                                if non_retryable {
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        error = %error_detail,
                                        "Non-retryable error, moving on"
                                    );

                                    if is_context_window_exceeded(&e) {
                                        anyhow::bail!(
                                            "Request exceeds model context window; retries and fallbacks were skipped. Attempts:\n{}",
                                            failures.join("\n")
                                        );
                                    }

                                    break;
                                }

                                if attempt < self.max_retries {
                                    let wait = self.compute_backoff(backoff_ms, &e);
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt = attempt + 1,
                                        backoff_ms = wait,
                                        reason = failure_reason,
                                        error = %error_detail,
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
                        model = sent_model,
                        "Exhausted retries, trying next provider/model"
                    );
                }
            }
        }

        anyhow::bail!(
            "All providers/models failed. Attempts:\n{}",
            failures.join("\n")
        )
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let models = self.model_chain(model);
        let mut failures = Vec::new();

        for current_model in &models {
            for (provider_index, (provider_name, provider)) in self.providers.iter().enumerate() {
                let sent_models =
                    self.provider_model_chain(current_model, provider_name, provider_index == 0);
                for sent_model in sent_models {
                    let mut backoff_ms = self.base_backoff_ms;

                    for attempt in 0..=self.max_retries {
                        let req = ChatRequest {
                            messages: request.messages,
                            tools: request.tools,
                        };
                        match provider.chat(req, sent_model, temperature).await {
                            Ok(resp) => {
                                if attempt > 0 || sent_model != model {
                                    tracing::info!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt,
                                        original_model = model,
                                        "Provider recovered (failover/retry)"
                                    );
                                }
                                return Ok(resp);
                            }
                            Err(e) => {
                                let non_retryable_rate_limit = is_non_retryable_rate_limit(&e);
                                let non_retryable =
                                    is_non_retryable(&e) || non_retryable_rate_limit;
                                let rate_limited = is_rate_limited(&e);
                                let failure_reason = failure_reason(rate_limited, non_retryable);
                                let error_detail = compact_error_detail(&e);

                                push_failure(
                                    &mut failures,
                                    provider_name,
                                    sent_model,
                                    attempt + 1,
                                    self.max_retries + 1,
                                    failure_reason,
                                    &error_detail,
                                );

                                if rate_limited && !non_retryable_rate_limit {
                                    if let Some(new_key) = self.rotate_key() {
                                        tracing::warn!(
                                            provider = provider_name,
                                            error = %error_detail,
                                            "Rate limited; key rotation selected key ending ...{} \
                                             but cannot apply (Provider trait has no set_api_key). \
                                             Retrying with original key.",
                                            &new_key[new_key.len().saturating_sub(4)..]
                                        );
                                    }
                                }

                                if non_retryable {
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        error = %error_detail,
                                        "Non-retryable error, moving on"
                                    );

                                    if is_context_window_exceeded(&e) {
                                        anyhow::bail!(
                                            "Request exceeds model context window; retries and fallbacks were skipped. Attempts:\n{}",
                                            failures.join("\n")
                                        );
                                    }

                                    break;
                                }

                                if attempt < self.max_retries {
                                    let wait = self.compute_backoff(backoff_ms, &e);
                                    tracing::warn!(
                                        provider = provider_name,
                                        model = sent_model,
                                        attempt = attempt + 1,
                                        backoff_ms = wait,
                                        reason = failure_reason,
                                        error = %error_detail,
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
                        model = sent_model,
                        "Exhausted retries, trying next provider/model"
                    );
                }
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

    fn supports_streaming(&self) -> bool {
        self.providers.iter().any(|(_, p)| p.supports_streaming())
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        // Try each provider/model combination for streaming
        // For streaming, we use the first provider that supports it and has streaming enabled
        for (provider_index, (provider_name, provider)) in self.providers.iter().enumerate() {
            if !provider.supports_streaming() || !options.enabled {
                continue;
            }

            // Clone provider data for the stream
            let provider_clone = provider_name.clone();

            // Try the first model in the chain for streaming, with provider remap applied.
            let base_model = match self.model_chain(model).first() {
                Some(m) => *m,
                None => model,
            };
            let current_model = self
                .provider_model_chain(base_model, provider_name, provider_index == 0)
                .first()
                .copied()
                .unwrap_or(base_model)
                .to_string();

            // For streaming, we attempt once and propagate errors
            // The caller can retry the entire request if needed
            let stream = provider.stream_chat_with_system(
                system_prompt,
                message,
                &current_model,
                temperature,
                options,
            );

            // Use a channel to bridge the stream with logging
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

            tokio::spawn(async move {
                let mut stream = stream;
                while let Some(chunk) = stream.next().await {
                    if let Err(ref e) = chunk {
                        tracing::warn!(
                            provider = provider_clone,
                            model = current_model,
                            "Streaming error: {e}"
                        );
                    }
                    if tx.send(chunk).await.is_err() {
                        break; // Receiver dropped
                    }
                }
            });

            // Convert channel receiver to stream
            return stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|chunk| (chunk, rx))
            })
            .boxed();
        }

        // No streaming support available
        stream::once(async move {
            Err(super::traits::StreamError::Provider(
                "No provider supports streaming".to_string(),
            ))
        })
        .boxed()
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
        models_seen: parking_lot::Mutex<Vec<String>>,
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
            self.models_seen.lock().push(model.to_string());
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
        assert!(msg.contains("provider=p1 model=test"));
        assert!(msg.contains("provider=p2 model=test"));
        assert!(msg.contains("error=p1 error"));
        assert!(msg.contains("error=p2 error"));
        assert!(msg.contains("retryable"));
    }

    #[test]
    fn non_retryable_detects_common_patterns() {
        assert!(is_non_retryable(&anyhow::anyhow!("400 Bad Request")));
        assert!(is_non_retryable(&anyhow::anyhow!("401 Unauthorized")));
        assert!(is_non_retryable(&anyhow::anyhow!("403 Forbidden")));
        assert!(is_non_retryable(&anyhow::anyhow!("404 Not Found")));
        assert!(is_non_retryable(&anyhow::anyhow!(
            "516 mapper tool schema mismatch: unknown parameter: tools"
        )));
        assert!(is_non_retryable(&anyhow::anyhow!(
            "invalid api key provided"
        )));
        assert!(is_non_retryable(&anyhow::anyhow!("authentication failed")));
        assert!(is_non_retryable(&anyhow::anyhow!(
            "model glm-4.7 not found"
        )));
        assert!(is_non_retryable(&anyhow::anyhow!(
            "unsupported model: glm-4.7"
        )));
        assert!(!is_non_retryable(&anyhow::anyhow!("429 Too Many Requests")));
        assert!(!is_non_retryable(&anyhow::anyhow!("408 Request Timeout")));
        assert!(!is_non_retryable(&anyhow::anyhow!(
            "500 Internal Server Error"
        )));
        assert!(!is_non_retryable(&anyhow::anyhow!("502 Bad Gateway")));
        assert!(!is_non_retryable(&anyhow::anyhow!(
            "516 upstream gateway temporarily unavailable"
        )));
        assert!(!is_non_retryable(&anyhow::anyhow!("timeout")));
        assert!(!is_non_retryable(&anyhow::anyhow!("connection reset")));
        assert!(!is_non_retryable(&anyhow::anyhow!(
            "model overloaded, try again later"
        )));
        assert!(is_non_retryable(&anyhow::anyhow!(
            "OpenAI Codex stream error: Your input exceeds the context window of this model."
        )));
    }

    #[tokio::test]
    async fn context_window_error_aborts_retries_and_model_fallbacks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut model_fallbacks = std::collections::HashMap::new();
        model_fallbacks.insert(
            "gpt-5.3-codex".to_string(),
            vec!["gpt-5.2-codex".to_string()],
        );

        let provider = ReliableProvider::new(
            vec![(
                "openai-codex".into(),
                Box::new(MockProvider {
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

        let err = provider
            .simple_chat("hello", "gpt-5.3-codex", 0.0)
            .await
            .expect_err("context window overflow should fail fast");
        let msg = err.to_string();

        assert!(msg.contains("context window"));
        assert!(msg.contains("skipped"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn aggregated_error_marks_non_retryable_model_mismatch_with_details() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "custom".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "unsupported model: glm-4.7",
                }),
            )],
            3,
            1,
        );

        let err = provider
            .simple_chat("hello", "glm-4.7", 0.0)
            .await
            .expect_err("provider should fail");
        let msg = err.to_string();

        assert!(msg.contains("non_retryable"));
        assert!(msg.contains("error=unsupported model: glm-4.7"));
        // Non-retryable errors should not consume retry budget.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
            models_seen: parking_lot::Mutex::new(Vec::new()),
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

        let seen = mock.models_seen.lock();
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

    #[tokio::test]
    async fn provider_keyed_model_fallbacks_remap_fallback_provider_models() {
        let primary = Arc::new(ModelAwareMock {
            calls: Arc::new(AtomicUsize::new(0)),
            models_seen: parking_lot::Mutex::new(Vec::new()),
            fail_models: vec!["glm-5", "glm-4.7"],
            response: "never",
        });
        let fallback = Arc::new(ModelAwareMock {
            calls: Arc::new(AtomicUsize::new(0)),
            models_seen: parking_lot::Mutex::new(Vec::new()),
            fail_models: vec![],
            response: "ok from remap",
        });

        let mut fallbacks = HashMap::new();
        fallbacks.insert("zai".to_string(), vec!["glm-4.7".to_string()]);
        fallbacks.insert(
            "openrouter".to_string(),
            vec!["anthropic/claude-sonnet-4".to_string()],
        );

        let provider = ReliableProvider::new(
            vec![
                ("zai".into(), Box::new(primary.clone()) as Box<dyn Provider>),
                (
                    "openrouter".into(),
                    Box::new(fallback.clone()) as Box<dyn Provider>,
                ),
            ],
            0,
            1,
        )
        .with_model_fallbacks(fallbacks);

        let result = provider.simple_chat("hello", "glm-5", 0.0).await.unwrap();
        assert_eq!(result, "ok from remap");

        let primary_seen = primary.models_seen.lock();
        assert_eq!(primary_seen.len(), 2);
        assert_eq!(primary_seen[0], "glm-5");
        assert_eq!(primary_seen[1], "glm-4.7");

        let fallback_seen = fallback.models_seen.lock();
        assert_eq!(fallback_seen.len(), 1);
        assert_eq!(fallback_seen[0], "anthropic/claude-sonnet-4");
        assert!(!fallback_seen.iter().any(|m| m == "glm-5"));
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
    fn non_retryable_rate_limit_detects_plan_restricted_model() {
        let err = anyhow::anyhow!(
            "{}",
            "API error (429 Too Many Requests): {\"code\":1311,\"message\":\"the current account plan does not include glm-5\"}"
        );
        assert!(
            is_non_retryable_rate_limit(&err),
            "plan-restricted 429 should skip retries"
        );
    }

    #[test]
    fn non_retryable_rate_limit_detects_insufficient_balance() {
        let err = anyhow::anyhow!(
            "{}",
            "API error (429 Too Many Requests): {\"code\":1113,\"message\":\"insufficient balance\"}"
        );
        assert!(
            is_non_retryable_rate_limit(&err),
            "insufficient-balance 429 should skip retries"
        );
    }

    #[test]
    fn non_retryable_rate_limit_does_not_flag_generic_429() {
        let err = anyhow::anyhow!("429 Too Many Requests: rate limit exceeded");
        assert!(
            !is_non_retryable_rate_limit(&err),
            "generic rate-limit 429 should remain retryable"
        );
    }

    #[test]
    fn compute_backoff_uses_retry_after() {
        let provider = ReliableProvider::new(vec![], 0, 500);
        let err = anyhow::anyhow!("429 Retry-After: 3");
        assert_eq!(provider.compute_backoff(500, &err), 3_000);
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

    // ── §2.1 API auth error (401/403) tests ──────────────────

    #[test]
    fn non_retryable_detects_401() {
        let err = anyhow::anyhow!("API error (401 Unauthorized): invalid api key");
        assert!(
            is_non_retryable(&err),
            "401 errors must be detected as non-retryable"
        );
    }

    #[test]
    fn non_retryable_detects_403() {
        let err = anyhow::anyhow!("API error (403 Forbidden): access denied");
        assert!(
            is_non_retryable(&err),
            "403 errors must be detected as non-retryable"
        );
    }

    #[test]
    fn non_retryable_detects_404() {
        let err = anyhow::anyhow!("API error (404 Not Found): model not found");
        assert!(
            is_non_retryable(&err),
            "404 errors must be detected as non-retryable"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_429() {
        let err = anyhow::anyhow!("429 Too Many Requests");
        assert!(
            !is_non_retryable(&err),
            "429 must NOT be treated as non-retryable (it is retryable with backoff)"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_408() {
        let err = anyhow::anyhow!("408 Request Timeout");
        assert!(
            !is_non_retryable(&err),
            "408 must NOT be treated as non-retryable (it is retryable)"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_500() {
        let err = anyhow::anyhow!("500 Internal Server Error");
        assert!(
            !is_non_retryable(&err),
            "500 must NOT be treated as non-retryable (server errors are retryable)"
        );
    }

    #[test]
    fn non_retryable_does_not_flag_502() {
        let err = anyhow::anyhow!("502 Bad Gateway");
        assert!(
            !is_non_retryable(&err),
            "502 must NOT be treated as non-retryable"
        );
    }

    // ── §2.2 Rate limit Retry-After edge cases ───────────────

    #[test]
    fn parse_retry_after_zero() {
        let err = anyhow::anyhow!("429 Too Many Requests, Retry-After: 0");
        assert_eq!(
            parse_retry_after_ms(&err),
            Some(0),
            "Retry-After: 0 should parse as 0ms"
        );
    }

    #[test]
    fn parse_retry_after_with_underscore_separator() {
        let err = anyhow::anyhow!("rate limited, retry_after: 10");
        assert_eq!(
            parse_retry_after_ms(&err),
            Some(10_000),
            "retry_after with underscore must be parsed"
        );
    }

    #[test]
    fn parse_retry_after_space_separator() {
        let err = anyhow::anyhow!("Retry-After 7");
        assert_eq!(
            parse_retry_after_ms(&err),
            Some(7000),
            "Retry-After with space separator must be parsed"
        );
    }

    #[test]
    fn rate_limited_false_for_generic_error() {
        let err = anyhow::anyhow!("Connection refused");
        assert!(
            !is_rate_limited(&err),
            "generic errors must not be flagged as rate-limited"
        );
    }

    // ── §2.3 Malformed API response error classification ─────

    #[tokio::test]
    async fn non_retryable_skips_retries_for_401() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "API error (401 Unauthorized): invalid key",
                }),
            )],
            5,
            1,
        );

        let result = provider.simple_chat("hello", "test", 0.0).await;
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
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "API error (429 Too Many Requests): {\"code\":1311,\"message\":\"plan does not include glm-5\"}",
                }),
            )],
            5,
            1,
        );

        let result = provider.simple_chat("hello", "test", 0.0).await;
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

    #[tokio::test]
    async fn native_tool_schema_rejection_skips_retries_for_516() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: usize::MAX,
                    response: "never",
                    error: "API error (516 <unknown status code>): mapper validation failed: tool schema mismatch",
                }),
            )],
            5,
            1,
        );

        let result = provider.simple_chat("hello", "test", 0.0).await;
        assert!(
            result.is_err(),
            "516 tool-schema incompatibility should fail quickly without retries"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "tool-schema mismatch must not consume retry budget"
        );
    }

    #[tokio::test]
    async fn generic_516_without_schema_hint_remains_retryable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    response: "recovered",
                    error: "API error (516 <unknown status code>): upstream gateway unavailable",
                }),
            )],
            3,
            1,
        );

        let result = provider.simple_chat("hello", "test", 0.0).await;
        assert_eq!(result.unwrap(), "recovered");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "generic 516 without schema hint should still retry once and recover"
        );
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

    /// Mock provider that implements `chat()` with native tool support.
    struct NativeToolMock {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response_text: &'static str,
        tool_calls: Vec<super::super::traits::ToolCall>,
        error: &'static str,
    }

    #[async_trait]
    impl Provider for NativeToolMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
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
            _temperature: f64,
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
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            })
        }
    }

    #[tokio::test]
    async fn chat_delegates_to_inner_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_call = super::super::traits::ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: r#"{"command":"date"}"#.to_string(),
        };
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(NativeToolMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response_text: "ok",
                    tool_calls: vec![tool_call.clone()],
                    error: "boom",
                }) as Box<dyn Provider>,
            )],
            2,
            1,
        );

        let messages = vec![ChatMessage::user("what time is it?")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = provider.chat(request, "test-model", 0.0).await.unwrap();

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
        };
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(NativeToolMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 2,
                    response_text: "recovered",
                    tool_calls: vec![tool_call],
                    error: "temporary failure",
                }) as Box<dyn Provider>,
            )],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("test")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = provider.chat(request, "test-model", 0.0).await.unwrap();

        assert_eq!(result.text.as_deref(), Some("recovered"));
        assert!(
            calls.load(Ordering::SeqCst) > 1,
            "should have retried at least once"
        );
    }

    #[tokio::test]
    async fn chat_preserves_native_tools_support() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(NativeToolMock {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response_text: "ok",
                    tool_calls: vec![],
                    error: "boom",
                }) as Box<dyn Provider>,
            )],
            2,
            1,
        );

        assert!(
            provider.supports_native_tools(),
            "ReliableProvider must propagate supports_native_tools from inner provider"
        );
    }

    // ── Gap 2-4: Parity tests for chat() ────────────────────────

    /// Gap 2: `chat()` returns an aggregated error when all providers fail,
    /// matching behavior of `returns_aggregated_error_when_all_providers_fail`.
    #[tokio::test]
    async fn chat_returns_aggregated_error_when_all_providers_fail() {
        let provider = ReliableProvider::new(
            vec![
                (
                    "p1".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response_text: "never",
                        tool_calls: vec![],
                        error: "p1 chat error",
                    }) as Box<dyn Provider>,
                ),
                (
                    "p2".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response_text: "never",
                        tool_calls: vec![],
                        error: "p2 chat error",
                    }) as Box<dyn Provider>,
                ),
            ],
            0,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let err = provider
            .chat(request, "test", 0.0)
            .await
            .expect_err("all providers should fail");
        let msg = err.to_string();
        assert!(msg.contains("All providers/models failed"));
        assert!(msg.contains("provider=p1 model=test"));
        assert!(msg.contains("provider=p2 model=test"));
        assert!(msg.contains("error=p1 chat error"));
        assert!(msg.contains("error=p2 chat error"));
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
    impl Provider for NativeModelAwareMock {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
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
            _temperature: f64,
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
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            })
        }
    }

    #[async_trait]
    impl Provider for Arc<NativeModelAwareMock> {
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

        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.as_ref().chat(request, model, temperature).await
        }
    }

    /// Gap 3: `chat()` tries fallback models on failure,
    /// matching behavior of `model_failover_tries_fallback_model`.
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

        let provider = ReliableProvider::new(
            vec![(
                "anthropic".into(),
                Box::new(mock.clone()) as Box<dyn Provider>,
            )],
            0, // no retries — force immediate model failover
            1,
        )
        .with_model_fallbacks(fallbacks);

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = provider.chat(request, "claude-opus", 0.0).await.unwrap();
        assert_eq!(result.text.as_deref(), Some("ok from sonnet"));

        let seen = mock.models_seen.lock();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], "claude-opus");
        assert_eq!(seen[1], "claude-sonnet");
    }

    /// Gap 4: `chat()` skips retries on non-retryable errors (401, 403, etc.),
    /// matching behavior of `skips_retries_on_non_retryable_error`.
    #[tokio::test]
    async fn chat_skips_non_retryable_errors() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response_text: "never",
                        tool_calls: vec![],
                        error: "401 Unauthorized",
                    }) as Box<dyn Provider>,
                ),
                (
                    "fallback".into(),
                    Box::new(NativeToolMock {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response_text: "from fallback",
                        tool_calls: vec![],
                        error: "fallback err",
                    }) as Box<dyn Provider>,
                ),
            ],
            3,
            1,
        );

        let messages = vec![ChatMessage::user("hello")];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let result = provider.chat(request, "test", 0.0).await.unwrap();
        assert_eq!(result.text.as_deref(), Some("from fallback"));
        // Primary should have been called only once (no retries)
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn vision_override_forces_true() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "",
                }) as Box<dyn Provider>,
            )],
            1,
            100,
        )
        .with_vision_override(Some(true));

        // MockProvider default capabilities → vision: false
        // Override should force true
        assert!(provider.supports_vision());
    }

    #[test]
    fn vision_override_forces_false() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "",
                }) as Box<dyn Provider>,
            )],
            1,
            100,
        )
        .with_vision_override(Some(false));

        assert!(!provider.supports_vision());
    }

    #[test]
    fn vision_override_none_defers_to_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "",
                }) as Box<dyn Provider>,
            )],
            1,
            100,
        );
        // No override set → should defer to provider default (false)
        assert!(!provider.supports_vision());
    }
}
