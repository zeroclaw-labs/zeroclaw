//! Generic backoff storage with automatic cleanup.
//!
//! Thread-safe, in-memory, with TTL-based expiration and soonest-to-expire eviction.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Entry in backoff store with deadline and error context.
#[derive(Debug, Clone)]
pub struct BackoffEntry<T> {
    pub deadline: Instant,
    pub error_detail: T,
}

/// Generic backoff store with automatic cleanup.
///
/// Thread-safe via parking_lot::Mutex.
/// Cleanup strategies:
/// - Lazy removal on `get()` if expired
/// - Opportunistic cleanup before eviction
/// - Soonest-to-expire eviction when max_entries reached (evicts the entry with the smallest deadline)
pub struct BackoffStore<K, T> {
    data: Mutex<HashMap<K, BackoffEntry<T>>>,
    max_entries: usize,
}

impl<K, T> BackoffStore<K, T>
where
    K: Eq + Hash + Clone,
    T: Clone,
{
    /// Create new backoff store with capacity limit.
    pub fn new(max_entries: usize) -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
            max_entries: max_entries.max(1), // Clamp to minimum 1
        }
    }

    /// Check if key is in backoff. Returns remaining duration and error detail.
    ///
    /// Lazy cleanup: expired entries removed on check.
    pub fn get(&self, key: &K) -> Option<(Duration, T)> {
        let mut data = self.data.lock();
        let now = Instant::now();

        if let Some(entry) = data.get(key) {
            if now >= entry.deadline {
                // Expired - remove and return None
                data.remove(key);
                None
            } else {
                let remaining = entry.deadline - now;
                Some((remaining, entry.error_detail.clone()))
            }
        } else {
            None
        }
    }

    /// Record backoff for key with duration and error context.
    pub fn set(&self, key: K, duration: Duration, error_detail: T) {
        let mut data = self.data.lock();
        let now = Instant::now();

        // Opportunistic cleanup before eviction
        if data.len() >= self.max_entries {
            data.retain(|_, entry| entry.deadline > now);
        }

        // Soonest-to-expire eviction if still over capacity
        if data.len() >= self.max_entries {
            if let Some(oldest_key) = data
                .iter()
                .min_by_key(|(_, entry)| entry.deadline)
                .map(|(k, _)| k.clone())
            {
                data.remove(&oldest_key);
            }
        }

        data.insert(
            key,
            BackoffEntry {
                deadline: now + duration,
                error_detail,
            },
        );
    }

    /// Clear backoff for key (on success).
    pub fn clear(&self, key: &K) {
        self.data.lock().remove(key);
    }

    /// Clear all backoffs (for testing).
    #[cfg(test)]
    pub fn clear_all(&self) {
        self.data.lock().clear();
    }

    /// Get count of active backoffs (for observability).
    pub fn len(&self) -> usize {
        let mut data = self.data.lock();
        let now = Instant::now();
        data.retain(|_, entry| entry.deadline > now);
        data.len()
    }

    /// Check if store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn backoff_stores_and_retrieves_entry() {
        let store = BackoffStore::new(10);
        let key = "test-key";
        let error = "test error";

        store.set(key.to_string(), Duration::from_secs(5), error.to_string());

        let result = store.get(&key.to_string());
        assert!(result.is_some());

        let (remaining, stored_error) = result.unwrap();
        assert!(remaining.as_secs() > 0 && remaining.as_secs() <= 5);
        assert_eq!(stored_error, error);
    }

    #[test]
    fn backoff_expires_after_duration() {
        let store = BackoffStore::new(10);
        let key = "expire-test";

        store.set(
            key.to_string(),
            Duration::from_millis(50),
            "error".to_string(),
        );
        assert!(store.get(&key.to_string()).is_some());

        thread::sleep(Duration::from_millis(60));
        assert!(store.get(&key.to_string()).is_none());
    }

    #[test]
    fn backoff_clears_on_demand() {
        let store = BackoffStore::new(10);
        let key = "clear-test";

        store.set(
            key.to_string(),
            Duration::from_secs(10),
            "error".to_string(),
        );
        assert!(store.get(&key.to_string()).is_some());

        store.clear(&key.to_string());
        assert!(store.get(&key.to_string()).is_none());
    }

    #[test]
    fn backoff_lru_eviction_at_capacity() {
        let store = BackoffStore::new(2);

        store.set(
            "key1".to_string(),
            Duration::from_secs(10),
            "error1".to_string(),
        );
        store.set(
            "key2".to_string(),
            Duration::from_secs(20),
            "error2".to_string(),
        );
        store.set(
            "key3".to_string(),
            Duration::from_secs(30),
            "error3".to_string(),
        );

        // key1 should be evicted (shortest deadline)
        assert!(store.get(&"key1".to_string()).is_none());
        assert!(store.get(&"key2".to_string()).is_some());
        assert!(store.get(&"key3".to_string()).is_some());
    }

    #[test]
    fn backoff_max_entries_clamped_to_one() {
        let store = BackoffStore::new(0); // Should clamp to 1
        store.set(
            "only-key".to_string(),
            Duration::from_secs(5),
            "error".to_string(),
        );
        assert!(store.get(&"only-key".to_string()).is_some());
    }
}
