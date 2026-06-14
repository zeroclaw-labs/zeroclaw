//! Sliding-window rate limiter for authentication attempts.
//!
//! Protects pairing and bearer-token validation endpoints against
//! brute-force attacks.  Tracks per-IP attempt timestamps and enforces
//! a lockout period after too many failures within the sliding window.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Maximum auth attempts allowed within the sliding window.
pub const MAX_ATTEMPTS: u32 = 10;
/// Sliding window duration in seconds.
pub const WINDOW_SECS: u64 = 60;
/// Lockout duration in seconds after exceeding [`MAX_ATTEMPTS`].
pub const LOCKOUT_SECS: u64 = 300;
/// How often stale entries are swept from the map.
const SWEEP_INTERVAL_SECS: u64 = 300;

/// Error returned when a client exceeds the auth rate limit.
#[derive(Debug, Clone)]
pub struct RateLimitError {
    /// Seconds until the client may retry.
    pub retry_after_secs: u64,
}

/// Per-IP auth attempt tracker with sliding window and lockout.
#[derive(Debug)]
pub struct AuthRateLimiter {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Key = IP string, value = timestamps of recent attempts.
    attempts: HashMap<String, Vec<Instant>>,
    /// Key = IP string, value = instant when lockout was triggered.
    lockouts: HashMap<String, Instant>,
    last_sweep: Instant,
}

impl AuthRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                attempts: HashMap::new(),
                lockouts: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// Returns `true` if the given IP is a loopback address (exempt from limiting).
    fn is_loopback(key: &str) -> bool {
        matches!(key, "127.0.0.1" | "::1")
            || key
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    }

    /// Check whether the client identified by `key` is allowed to attempt auth.
    ///
    /// Does **not** record a new attempt — call `record_attempt` after
    /// verifying the attempt actually happened (regardless of success/failure).
    pub fn check_rate_limit(&self, key: &str) -> Result<(), RateLimitError> {
        if Self::is_loopback(key) {
            return Ok(());
        }

        let now = Instant::now();
        let mut inner = self.inner.lock();
        Self::maybe_sweep(&mut inner, now);

        // Check active lockout first.
        if let Some(&locked_at) = inner.lockouts.get(key) {
            let elapsed = now.duration_since(locked_at).as_secs();
            if elapsed < LOCKOUT_SECS {
                return Err(RateLimitError {
                    retry_after_secs: LOCKOUT_SECS - elapsed,
                });
            }
            // Lockout expired — remove it and let the attempt through.
            inner.lockouts.remove(key);
            inner.attempts.remove(key);
        }

        // Prune old timestamps for this key.
        let window = Duration::from_secs(WINDOW_SECS);
        if let Some(timestamps) = inner.attempts.get_mut(key) {
            timestamps.retain(|t| now.duration_since(*t) < window);
            if timestamps.len() >= MAX_ATTEMPTS as usize {
                // Trigger lockout.
                inner.lockouts.insert(key.to_owned(), now);
                return Err(RateLimitError {
                    retry_after_secs: LOCKOUT_SECS,
                });
            }
        }

        Ok(())
    }

    /// Record a new authentication attempt for `key`.
    pub fn record_attempt(&self, key: &str) {
        if Self::is_loopback(key) {
            return;
        }

        let now = Instant::now();
        let mut inner = self.inner.lock();
        inner.attempts.entry(key.to_owned()).or_default().push(now);
    }

    /// Check whether `key` is currently locked out, without recording anything.
    pub fn is_locked_out(&self, key: &str) -> bool {
        if Self::is_loopback(key) {
            return false;
        }

        let now = Instant::now();
        let inner = self.inner.lock();
        if let Some(&locked_at) = inner.lockouts.get(key) {
            return now.duration_since(locked_at).as_secs() < LOCKOUT_SECS;
        }
        false
    }

    /// Periodically purge entries older than [`LOCKOUT_SECS`] to bound memory.
    fn maybe_sweep(inner: &mut Inner, now: Instant) {
        if inner.last_sweep.elapsed() < Duration::from_secs(SWEEP_INTERVAL_SECS) {
            return;
        }
        inner.last_sweep = now;

        let lockout_dur = Duration::from_secs(LOCKOUT_SECS);
        let window_dur = Duration::from_secs(WINDOW_SECS);

        inner
            .lockouts
            .retain(|_, locked_at| now.duration_since(*locked_at) < lockout_dur);

        inner.attempts.retain(|_, timestamps| {
            timestamps.retain(|t| now.duration_since(*t) < window_dur);
            !timestamps.is_empty()
        });
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_exempt() {
        let limiter = AuthRateLimiter::new();
        for _ in 0..20 {
            assert!(limiter.check_rate_limit("127.0.0.1").is_ok());
            limiter.record_attempt("127.0.0.1");
        }
        assert!(!limiter.is_locked_out("127.0.0.1"));

        for _ in 0..20 {
            assert!(limiter.check_rate_limit("::1").is_ok());
            limiter.record_attempt("::1");
        }
    }

    #[test]
    fn lockout_after_max_attempts() {
        let limiter = AuthRateLimiter::new();
        let key = "192.168.1.100";

        for _ in 0..MAX_ATTEMPTS {
            assert!(limiter.check_rate_limit(key).is_ok());
            limiter.record_attempt(key);
        }

        // Next check should fail — lockout triggered.
        let err = limiter.check_rate_limit(key).unwrap_err();
        assert!(err.retry_after_secs > 0);
        assert!(limiter.is_locked_out(key));
    }

    #[test]
    fn under_limit_is_ok() {
        let limiter = AuthRateLimiter::new();
        let key = "10.0.0.1";

        for _ in 0..(MAX_ATTEMPTS - 1) {
            assert!(limiter.check_rate_limit(key).is_ok());
            limiter.record_attempt(key);
        }
        // Still under the limit.
        assert!(limiter.check_rate_limit(key).is_ok());
    }
}
