//! In-loop retry with jittered exponential backoff.
//!
//! Classifies LLM call errors and provides a retry policy. Transient errors
//! (rate limits, timeouts, server errors) are retried with increasing delays.
//! Permanent errors (auth, invalid request, context overflow) propagate immediately.

use std::time::Duration;

/// Error classification for retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Rate limited — retry after delay (possibly from Retry-After header).
    RateLimit,
    /// Transient server error (5xx, timeout, connection reset).
    Transient,
    /// Context window exceeded — handled by context recovery, not retry.
    ContextOverflow,
    /// Authentication / authorization failure — may benefit from credential rotation.
    AuthFailure,
    /// Permanent error (invalid request, model not found).
    Permanent,
}

/// Classify an LLM call error for retry decisions.
pub fn classify_error(err: &anyhow::Error) -> ErrorClass {
    let msg = err.to_string().to_ascii_lowercase();

    if daemonclaw_providers::reliable::is_context_window_exceeded(err) {
        return ErrorClass::ContextOverflow;
    }

    if msg.contains("401")
        || msg.contains("403")
        || msg.contains("unauthorized")
        || msg.contains("authentication")
        || msg.contains("invalid api key")
        || msg.contains("invalid_api_key")
    {
        return ErrorClass::AuthFailure;
    }

    if msg.contains("429") || msg.contains("rate limit") || msg.contains("too many requests") {
        return ErrorClass::RateLimit;
    }

    if msg.contains("500")
        || msg.contains("502")
        || msg.contains("503")
        || msg.contains("504")
        || msg.contains("timeout")
        || msg.contains("connection reset")
        || msg.contains("connection refused")
        || msg.contains("broken pipe")
        || msg.contains("eof")
    {
        return ErrorClass::Transient;
    }

    if msg.contains("529") {
        return ErrorClass::Transient;
    }

    ErrorClass::Permanent
}

/// Try to extract a Retry-After delay from an error message.
/// Providers often include "retry after N seconds" or "Retry-After: N" in their errors.
pub fn parse_retry_after(err: &anyhow::Error) -> Option<Duration> {
    let msg = err.to_string();
    let lower = msg.to_ascii_lowercase();

    // "retry-after: 30" or "retry after 30 seconds"
    let patterns: &[&str] = &["retry-after:", "retry after ", "retry_after:"];
    for pat in patterns {
        if let Some(pos) = lower.find(pat) {
            let after = &msg[pos + pat.len()..];
            let num_str: String = after.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(secs) = num_str.parse::<u64>() {
                if secs > 0 && secs <= 300 {
                    return Some(Duration::from_secs(secs));
                }
            }
        }
    }
    None
}

/// Retry policy configuration.
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub jitter_ratio: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(1000),
            max_delay: Duration::from_secs(30),
            jitter_ratio: 0.3,
        }
    }
}

impl RetryPolicy {
    /// Compute the delay for a given retry attempt (1-based).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(20);
        let base_ms = self.base_delay.as_millis() as f64;
        let delay_ms = (base_ms * 2.0f64.powi(exponent as i32)).min(self.max_delay.as_millis() as f64);

        let jitter_ms = delay_ms * self.jitter_ratio * rand_f64();
        Duration::from_millis((delay_ms + jitter_ms) as u64)
    }

    /// Whether a given error class should be retried.
    pub fn should_retry(&self, class: ErrorClass) -> bool {
        matches!(class, ErrorClass::RateLimit | ErrorClass::Transient)
    }
}

/// Sanitize a response string: replace lone surrogates and other invalid
/// sequences that may have survived JSON deserialization (e.g. `\uD800`
/// literals from non-compliant providers). Rust strings are valid UTF-8 by
/// construction, but escaped surrogates can appear as U+FFFD after decoding.
/// This also strips null bytes which can confuse downstream tool parsing.
pub fn sanitize_response(text: &str) -> String {
    text.replace('\u{FFFD}', "")
        .replace('\0', "")
}

/// Simple pseudo-random f64 in [0, 1) using thread-local state.
fn rand_f64() -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let bits = hasher.finish();
    (bits & 0x000F_FFFF_FFFF_FFFF) as f64 / (1u64 << 52) as f64
}
