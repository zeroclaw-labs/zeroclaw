//! Token-bucket rate limiter for per-key request throttling.
//!
//! Supports per-key limiting by user ID, channel, IP, or workspace with
//! configurable requests-per-window, burst capacity, and window duration.
//! Uses atomics and `parking_lot::Mutex` — no external crates required.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Returned when a request is rate-limited.
#[derive(Debug, Clone)]
pub struct RateLimited {
    /// Suggested duration to wait before retrying.
    pub retry_after: Duration,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rate limited — retry after {:.1}s",
            self.retry_after.as_secs_f64()
        )
    }
}

impl std::error::Error for RateLimited {}

/// Configuration for a [`TokenBucketRateLimiter`].
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Maximum tokens (requests) per refill window.
    pub requests_per_window: u32,
    /// Burst capacity — tokens available immediately above steady state.
    pub burst: u32,
    /// Duration of the refill window.
    pub window: Duration,
    /// Maximum distinct keys tracked (prevents unbounded memory growth).
    pub max_keys: usize,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            requests_per_window: 60,
            burst: 10,
            window: Duration::from_secs(60),
            max_keys: 10_000,
        }
    }
}

/// Per-key state for the token bucket.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// A token-bucket rate limiter with per-key tracking.
///
/// Each key gets an independent bucket. Tokens refill at a steady rate
/// determined by `requests_per_window / window`. Burst allows a short
/// spike above the steady-state rate.
#[derive(Debug)]
pub struct TokenBucketRateLimiter {
    config: RateLimiterConfig,
    /// Refill rate: tokens per second.
    refill_rate: f64,
    /// Maximum tokens a bucket can hold (requests_per_window + burst).
    capacity: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl TokenBucketRateLimiter {
    /// Create a new rate limiter with the given configuration.
    /// Create a new rate limiter with the given configuration.
    ///
    /// # Panics
    ///
    /// Panics if `config.window` is zero — a zero-length refill window would
    /// silently fail open (infinite refill rate).
    pub fn new(config: RateLimiterConfig) -> Self {
        assert!(
            !config.window.is_zero(),
            "RateLimiterConfig.window must be non-zero (zero window fails open)"
        );
        let refill_rate = config.requests_per_window as f64 / config.window.as_secs_f64();
        let capacity = (config.requests_per_window + config.burst) as f64;
        Self {
            config,
            refill_rate,
            capacity,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether a request for `key` is allowed.
    ///
    /// Returns `Ok(())` if a token was consumed, or `Err(RateLimited)` with
    /// a `retry_after` hint if the bucket is empty.
    pub fn check(&self, key: &str) -> Result<(), RateLimited> {
        if self.config.requests_per_window == 0 && self.config.burst == 0 {
            // Zero capacity means always blocked — fail-fast.
            return Err(RateLimited {
                retry_after: self.config.window,
            });
        }

        let now = Instant::now();
        let mut buckets = self.buckets.lock();

        // Evict oldest key if at capacity and key is new.
        if !buckets.contains_key(key) && buckets.len() >= self.config.max_keys.max(1) {
            let evict_key = buckets
                .iter()
                .min_by_key(|(_, b)| b.last_refill)
                .map(|(k, _)| k.clone());
            if let Some(evict_key) = evict_key {
                buckets.remove(&evict_key);
            }
        }

        let bucket = buckets.entry(key.to_owned()).or_insert_with(|| Bucket {
            tokens: self.capacity,
            last_refill: now,
        });

        // Refill tokens based on elapsed time.
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_rate).min(self.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            // Time until one token is available.
            let deficit = 1.0 - bucket.tokens;
            let retry_secs = if self.refill_rate > 0.0 {
                deficit / self.refill_rate
            } else {
                self.config.window.as_secs_f64()
            };
            Err(RateLimited {
                retry_after: Duration::from_secs_f64(retry_secs),
            })
        }
    }

    /// Returns the number of keys currently tracked.
    #[cfg(test)]
    fn key_count(&self) -> usize {
        self.buckets.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_capacity() {
        let limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 3,
            burst: 0,
            window: Duration::from_secs(60),
            max_keys: 100,
        });

        // capacity = 3 + 0 = 3 tokens
        assert!(limiter.check("user_a").is_ok());
        assert!(limiter.check("user_a").is_ok());
        assert!(limiter.check("user_a").is_ok());
        // 4th should be blocked
        let err = limiter.check("user_a").unwrap_err();
        assert!(err.retry_after.as_secs_f64() > 0.0);
    }

    #[test]
    fn burst_adds_extra_capacity() {
        let limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 2,
            burst: 3,
            window: Duration::from_secs(60),
            max_keys: 100,
        });

        // capacity = 2 + 3 = 5 tokens
        for _ in 0..5 {
            assert!(limiter.check("user_a").is_ok());
        }
        assert!(limiter.check("user_a").is_err());
    }

    #[test]
    fn independent_keys() {
        let limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 1,
            burst: 0,
            window: Duration::from_secs(60),
            max_keys: 100,
        });

        assert!(limiter.check("user_a").is_ok());
        assert!(limiter.check("user_a").is_err());
        // Different key should still have tokens
        assert!(limiter.check("user_b").is_ok());
    }

    #[test]
    fn evicts_oldest_when_at_max_keys() {
        let limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 10,
            burst: 0,
            window: Duration::from_secs(60),
            max_keys: 2,
        });

        limiter.check("key_1").unwrap();
        limiter.check("key_2").unwrap();
        assert_eq!(limiter.key_count(), 2);

        // Adding a third key should evict one
        limiter.check("key_3").unwrap();
        assert_eq!(limiter.key_count(), 2);
    }

    #[test]
    fn retry_after_is_positive() {
        let limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 1,
            burst: 0,
            window: Duration::from_secs(10),
            max_keys: 100,
        });

        limiter.check("user_a").unwrap();
        let err = limiter.check("user_a").unwrap_err();
        // Refill rate is 1/10 = 0.1 tokens/s, deficit ~1.0, so retry ~10s
        assert!(err.retry_after.as_secs() >= 1);
        assert!(err.retry_after.as_secs() <= 11);
    }

    #[test]
    fn zero_capacity_always_blocks() {
        let limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 0,
            burst: 0,
            window: Duration::from_secs(60),
            max_keys: 100,
        });

        assert!(limiter.check("user_a").is_err());
    }


    #[test]
    #[should_panic(expected = "window must be non-zero")]
    fn zero_window_panics() {
        let _limiter = TokenBucketRateLimiter::new(RateLimiterConfig {
            requests_per_window: 10,
            burst: 5,
            window: Duration::ZERO,
            max_keys: 100,
        });
    }
}
