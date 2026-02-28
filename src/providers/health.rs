//! Provider health tracking with circuit breaker pattern.
//!
//! Tracks provider failure counts and temporarily blocks providers that exceed
//! failure thresholds (circuit breaker pattern). Uses separate storage for:
//! - Persistent failure state (HashMap with failure counts)
//! - Temporary circuit breaker blocks (BackoffStore with TTL)

use super::backoff::BackoffStore;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Provider health state with failure tracking.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderHealthState {
    pub failure_count: u32,
    pub last_error: Option<String>,
}

/// Thread-safe provider health tracker with circuit breaker.
///
/// Architecture:
/// - `states`: Persistent failure counts per provider (never expires)
/// - `backoff`: Temporary circuit breaker blocks with TTL (auto-expires)
///
/// This separation ensures:
/// - Circuit breaker blocks expire after cooldown (backoff.get() returns None)
/// - Failure history persists for observability (states HashMap)
pub struct ProviderHealthTracker {
    /// Persistent failure state per provider
    states: Arc<Mutex<HashMap<String, ProviderHealthState>>>,
    /// Temporary circuit breaker blocks with TTL
    backoff: Arc<BackoffStore<String, ()>>,
    /// Failure threshold before circuit opens
    failure_threshold: u32,
    /// Circuit breaker cooldown duration
    cooldown: Duration,
}

impl ProviderHealthTracker {
    /// Create new health tracker with circuit breaker settings.
    ///
    /// # Arguments
    /// * `failure_threshold` - Number of consecutive failures before circuit opens
    /// * `cooldown` - Duration to block provider after circuit opens
    /// * `max_tracked_providers` - Maximum number of providers to track (for BackoffStore capacity)
    pub fn new(failure_threshold: u32, cooldown: Duration, max_tracked_providers: usize) -> Self {
        Self {
            states: Arc::new(Mutex::new(HashMap::new())),
            backoff: Arc::new(BackoffStore::new(max_tracked_providers)),
            failure_threshold,
            cooldown,
        }
    }

    /// Check if provider should be tried (circuit closed).
    ///
    /// Returns:
    /// - `Ok(())` if circuit is closed (provider can be tried)
    /// - `Err((remaining, state))` if circuit is open (provider blocked)
    pub fn should_try(&self, provider: &str) -> Result<(), (Duration, ProviderHealthState)> {
        // Check circuit breaker
        if let Some((remaining, ())) = self.backoff.get(&provider.to_string()) {
            // Circuit is open - return remaining time and current state
            let states = self.states.lock();
            let state = states.get(provider).cloned().unwrap_or_default();
            return Err((remaining, state));
        }

        Ok(())
    }

    /// Record successful provider call.
    ///
    /// Resets failure count and clears circuit breaker.
    pub fn record_success(&self, provider: &str) {
        let mut states = self.states.lock();
        if let Some(state) = states.get_mut(provider) {
            if state.failure_count > 0 {
                tracing::info!(
                    provider = provider,
                    previous_failures = state.failure_count,
                    "Provider recovered - resetting failure count"
                );
                state.failure_count = 0;
                state.last_error = None;
            }
        }
        drop(states);

        // Clear circuit breaker
        self.backoff.clear(&provider.to_string());
    }

    /// Record failed provider call.
    ///
    /// Increments failure count. If threshold exceeded, opens circuit breaker.
    pub fn record_failure(&self, provider: &str, error: &str) {
        let mut states = self.states.lock();
        let state = states.entry(provider.to_string()).or_default();

        state.failure_count += 1;
        state.last_error = Some(error.to_string());

        let current_count = state.failure_count;
        drop(states);

        // Open circuit if threshold exceeded
        if current_count >= self.failure_threshold {
            tracing::warn!(
                provider = provider,
                failure_count = current_count,
                threshold = self.failure_threshold,
                cooldown_secs = self.cooldown.as_secs(),
                "Provider failure threshold exceeded - opening circuit breaker"
            );
            self.backoff.set(provider.to_string(), self.cooldown, ());
        }
    }

    /// Get current health state for a provider.
    pub fn get_state(&self, provider: &str) -> ProviderHealthState {
        self.states
            .lock()
            .get(provider)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all tracked provider states (for observability).
    pub fn get_all_states(&self) -> HashMap<String, ProviderHealthState> {
        self.states.lock().clone()
    }

    /// Clear all health tracking (for testing).
    #[cfg(test)]
    pub fn clear_all(&self) {
        self.states.lock().clear();
        self.backoff.clear_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn allows_provider_initially() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);
        assert!(tracker.should_try("test-provider").is_ok());
    }

    #[test]
    fn tracks_failures_below_threshold() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

        tracker.record_failure("test-provider", "error 1");
        assert!(tracker.should_try("test-provider").is_ok());

        tracker.record_failure("test-provider", "error 2");
        assert!(tracker.should_try("test-provider").is_ok());

        let state = tracker.get_state("test-provider");
        assert_eq!(state.failure_count, 2);
        assert_eq!(state.last_error.as_deref(), Some("error 2"));
    }

    #[test]
    fn opens_circuit_at_threshold() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

        tracker.record_failure("test-provider", "error 1");
        tracker.record_failure("test-provider", "error 2");
        tracker.record_failure("test-provider", "error 3");

        // Circuit should be open
        let result = tracker.should_try("test-provider");
        assert!(result.is_err());

        if let Err((remaining, state)) = result {
            assert!(remaining.as_secs() > 0 && remaining.as_secs() <= 60);
            assert_eq!(state.failure_count, 3);
        }
    }

    #[test]
    fn circuit_closes_after_cooldown() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_millis(50), 100);

        // Trigger circuit breaker
        tracker.record_failure("test-provider", "error 1");
        tracker.record_failure("test-provider", "error 2");
        tracker.record_failure("test-provider", "error 3");

        assert!(tracker.should_try("test-provider").is_err());

        // Wait for cooldown
        thread::sleep(Duration::from_millis(60));

        // Circuit should be closed (backoff expired)
        assert!(tracker.should_try("test-provider").is_ok());
    }

    #[test]
    fn success_resets_failure_count() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

        tracker.record_failure("test-provider", "error 1");
        tracker.record_failure("test-provider", "error 2");

        assert_eq!(tracker.get_state("test-provider").failure_count, 2);

        tracker.record_success("test-provider");

        let state = tracker.get_state("test-provider");
        assert_eq!(state.failure_count, 0);
        assert_eq!(state.last_error, None);
    }

    #[test]
    fn success_clears_circuit_breaker() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

        // Trigger circuit breaker
        tracker.record_failure("test-provider", "error 1");
        tracker.record_failure("test-provider", "error 2");
        tracker.record_failure("test-provider", "error 3");

        assert!(tracker.should_try("test-provider").is_err());

        // Success should clear circuit immediately
        tracker.record_success("test-provider");

        assert!(tracker.should_try("test-provider").is_ok());
        assert_eq!(tracker.get_state("test-provider").failure_count, 0);
    }

    #[test]
    fn tracks_multiple_providers_independently() {
        let tracker = ProviderHealthTracker::new(2, Duration::from_secs(60), 100);

        tracker.record_failure("provider-a", "error a1");
        tracker.record_failure("provider-a", "error a2");

        tracker.record_failure("provider-b", "error b1");

        // Provider A should have circuit open
        assert!(tracker.should_try("provider-a").is_err());

        // Provider B should still be allowed
        assert!(tracker.should_try("provider-b").is_ok());

        let state_a = tracker.get_state("provider-a");
        let state_b = tracker.get_state("provider-b");
        assert_eq!(state_a.failure_count, 2);
        assert_eq!(state_b.failure_count, 1);
    }

    #[test]
    fn get_all_states_returns_all_tracked_providers() {
        let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

        tracker.record_failure("provider-1", "error 1");
        tracker.record_failure("provider-2", "error 2");
        tracker.record_failure("provider-2", "error 2 again");

        let states = tracker.get_all_states();
        assert_eq!(states.len(), 2);
        assert_eq!(states.get("provider-1").unwrap().failure_count, 1);
        assert_eq!(states.get("provider-2").unwrap().failure_count, 2);
    }
}
