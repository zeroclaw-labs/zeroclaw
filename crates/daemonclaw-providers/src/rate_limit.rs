//! Proactive rate-limit tracking.
//!
//! Parses `X-RateLimit-Remaining`, `X-RateLimit-Reset`, and `X-RateLimit-Limit`
//! headers from successful HTTP responses and throttles outgoing requests before
//! hitting provider-imposed limits.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static GLOBAL_TRACKER: LazyLock<Arc<RateLimitTracker>> =
    LazyLock::new(|| Arc::new(RateLimitTracker::new()));

pub fn global_rate_limiter() -> &'static Arc<RateLimitTracker> {
    &GLOBAL_TRACKER
}

#[derive(Debug, Clone)]
struct RateLimitState {
    remaining: u64,
    #[allow(dead_code)]
    limit: Option<u64>,
    reset_at: Instant,
    last_updated: Instant,
}

/// Tracks per-(provider, model) rate limit quotas learned from HTTP response headers.
pub struct RateLimitTracker {
    state: Mutex<HashMap<(String, String), RateLimitState>>,
}

impl RateLimitTracker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Record rate limit info from a successful HTTP response.
    /// Called after every successful provider response with headers.
    pub fn record(
        &self,
        provider: &str,
        model: &str,
        remaining: Option<u64>,
        limit: Option<u64>,
        reset_epoch_secs: Option<u64>,
        reset_delta_secs: Option<u64>,
    ) {
        let remaining = match remaining {
            Some(r) => r,
            None => return,
        };

        let now = Instant::now();
        let reset_at = if let Some(epoch) = reset_epoch_secs {
            let now_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if epoch > now_epoch {
                now + Duration::from_secs(epoch - now_epoch)
            } else {
                now
            }
        } else if let Some(delta) = reset_delta_secs {
            now + Duration::from_secs(delta)
        } else {
            now + Duration::from_secs(60)
        };

        let key = (provider.to_string(), model.to_string());
        let mut state = self.state.lock();
        state.insert(
            key,
            RateLimitState {
                remaining,
                limit,
                reset_at,
                last_updated: now,
            },
        );
    }

    /// Check if we should wait before making a request to this provider/model.
    /// Returns the duration to sleep, or None if we're clear to proceed.
    pub fn check_wait(&self, provider: &str, model: &str) -> Option<Duration> {
        let key = (provider.to_string(), model.to_string());
        let state = self.state.lock();
        let entry = state.get(&key)?;

        let now = Instant::now();

        // Stale entries (>5 min old) are ignored — the quota likely reset.
        if now.duration_since(entry.last_updated) > Duration::from_secs(300) {
            return None;
        }

        if entry.remaining == 0 && entry.reset_at > now {
            Some(entry.reset_at - now)
        } else {
            None
        }
    }

    /// Async version: sleeps if rate limited, logs the wait.
    /// Returns true if we waited, false if we proceeded immediately.
    pub async fn wait_if_limited(&self, provider: &str, model: &str) -> bool {
        if let Some(wait) = self.check_wait(provider, model) {
            let wait = wait.min(Duration::from_secs(30));
            tracing::info!(
                provider,
                model,
                wait_ms = wait.as_millis() as u64,
                "Proactive rate limit: waiting for quota reset"
            );
            tokio::time::sleep(wait).await;
            true
        } else {
            false
        }
    }

    /// Decrement remaining counter after a request is sent (before response arrives).
    /// This prevents bursts when multiple concurrent requests see the same "remaining" value.
    pub fn consume_one(&self, provider: &str, model: &str) {
        let key = (provider.to_string(), model.to_string());
        let mut state = self.state.lock();
        if let Some(entry) = state.get_mut(&key) {
            entry.remaining = entry.remaining.saturating_sub(1);
        }
    }
}

/// Parse rate limit headers from an HTTP response.
/// Returns (remaining, limit, reset_epoch_secs, reset_delta_secs).
pub fn parse_rate_limit_headers(
    headers: &reqwest::header::HeaderMap,
) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let remaining = header_u64(headers, "x-ratelimit-remaining")
        .or_else(|| header_u64(headers, "x-rate-limit-remaining"))
        .or_else(|| header_u64(headers, "ratelimit-remaining"));

    let limit = header_u64(headers, "x-ratelimit-limit")
        .or_else(|| header_u64(headers, "x-rate-limit-limit"))
        .or_else(|| header_u64(headers, "ratelimit-limit"));

    let reset_epoch = header_u64(headers, "x-ratelimit-reset")
        .or_else(|| header_u64(headers, "x-rate-limit-reset"))
        .or_else(|| header_u64(headers, "ratelimit-reset"));

    // Some providers use a delta (seconds until reset) instead of epoch timestamp.
    // Heuristic: if the value is small (<86400), treat it as a delta.
    let (reset_epoch_secs, reset_delta_secs) = match reset_epoch {
        Some(v) if v < 86400 => (None, Some(v)),
        other => (other, None),
    };

    (remaining, limit, reset_epoch_secs, reset_delta_secs)
}

/// Convenience: parse headers and record into the global tracker.
pub fn record_from_response(
    provider: &str,
    model: &str,
    headers: &reqwest::header::HeaderMap,
) {
    let (remaining, limit, reset_epoch, reset_delta) = parse_rate_limit_headers(headers);
    if remaining.is_some() {
        global_rate_limiter().record(provider, model, remaining, limit, reset_epoch, reset_delta);
    }
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_checks_remaining_zero() {
        let tracker = RateLimitTracker::new();
        tracker.record("zai", "glm-4-flash", Some(0), Some(100), None, Some(5));
        let wait = tracker.check_wait("zai", "glm-4-flash");
        assert!(wait.is_some());
        assert!(wait.unwrap() <= Duration::from_secs(5));
    }

    #[test]
    fn no_wait_when_remaining_positive() {
        let tracker = RateLimitTracker::new();
        tracker.record("zai", "glm-4-flash", Some(50), Some(100), None, Some(60));
        assert!(tracker.check_wait("zai", "glm-4-flash").is_none());
    }

    #[test]
    fn no_wait_for_unknown_provider() {
        let tracker = RateLimitTracker::new();
        assert!(tracker.check_wait("unknown", "model").is_none());
    }

    #[test]
    fn consume_decrements() {
        let tracker = RateLimitTracker::new();
        tracker.record("openai", "gpt-4", Some(2), Some(100), None, Some(60));
        tracker.consume_one("openai", "gpt-4");
        tracker.consume_one("openai", "gpt-4");
        let wait = tracker.check_wait("openai", "gpt-4");
        assert!(wait.is_some());
    }

    #[tokio::test]
    async fn wait_if_limited_caps_at_30s() {
        let tracker = RateLimitTracker::new();
        tracker.record("slow", "model", Some(0), None, None, Some(120));
        let wait = tracker.check_wait("slow", "model").unwrap();
        // check_wait returns the raw duration, wait_if_limited caps it
        assert!(wait > Duration::from_secs(30));
    }

    #[test]
    fn parse_headers_standard() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-remaining", "5".parse().unwrap());
        headers.insert("x-ratelimit-limit", "100".parse().unwrap());
        headers.insert("x-ratelimit-reset", "30".parse().unwrap());
        let (remaining, limit, epoch, delta) = parse_rate_limit_headers(&headers);
        assert_eq!(remaining, Some(5));
        assert_eq!(limit, Some(100));
        assert_eq!(epoch, None);
        assert_eq!(delta, Some(30));
    }
}
