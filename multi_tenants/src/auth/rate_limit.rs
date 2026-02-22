use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    window: Duration,
    max_attempts: usize,
    entries: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(window_secs: u64, max_attempts: usize) -> Self {
        Self {
            window: Duration::from_secs(window_secs),
            max_attempts,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    /// Records the attempt and evicts expired timestamps for this key.
    pub fn check_and_record(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.entries.lock().expect("rate limiter lock poisoned");
        let timestamps = map.entry(key.to_string()).or_default();

        // Evict entries outside the window
        timestamps.retain(|&t| now.duration_since(t) < self.window);

        if timestamps.len() >= self.max_attempts {
            return false;
        }

        timestamps.push(now);
        true
    }

    /// Remove all entries whose timestamps are entirely outside the window.
    pub fn cleanup(&self) {
        let now = Instant::now();
        let mut map = self.entries.lock().expect("rate limiter lock poisoned");
        map.retain(|_, timestamps| {
            timestamps.retain(|&t| now.duration_since(t) < self.window);
            !timestamps.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_under_limit_allows() {
        let limiter = RateLimiter::new(60, 3);
        assert!(limiter.check_and_record("key1"));
        assert!(limiter.check_and_record("key1"));
        assert!(limiter.check_and_record("key1"));
    }

    #[test]
    fn test_over_limit_blocks() {
        let limiter = RateLimiter::new(60, 2);
        assert!(limiter.check_and_record("key2"));
        assert!(limiter.check_and_record("key2"));
        // 3rd attempt exceeds max
        assert!(!limiter.check_and_record("key2"));
    }

    #[test]
    fn test_window_expires_allows_again() {
        // 1ms window — tiny but enough to expire between sleeps
        let limiter = RateLimiter::new(0, 1); // window_secs=0 → Duration::from_secs(0) = zero
                                              // First attempt: allowed
        assert!(limiter.check_and_record("key3"));
        // Sleep to ensure timestamps are in the past relative to a zero window
        thread::sleep(Duration::from_millis(5));
        // Window has expired; should be allowed again
        assert!(limiter.check_and_record("key3"));
    }

    #[test]
    fn test_cleanup_removes_stale() {
        let limiter = RateLimiter::new(0, 10); // zero-second window
        limiter.check_and_record("key4");
        thread::sleep(Duration::from_millis(5));
        limiter.cleanup();
        // After cleanup the internal map entry should be gone
        let map = limiter.entries.lock().unwrap();
        assert!(
            !map.contains_key("key4") || map["key4"].is_empty(),
            "stale entries must be removed by cleanup"
        );
    }
}
