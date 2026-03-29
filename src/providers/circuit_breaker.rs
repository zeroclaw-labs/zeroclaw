//! Circuit breaker for provider failover.
//!
//! Tracks per-provider failure state and short-circuits requests to providers
//! that are currently failing, allowing the fallback chain to skip them
//! immediately instead of wasting retries.
//!
//! State machine:
//! - **Closed** (healthy): requests pass through normally.
//! - **Open** (failing): requests are skipped; transitions to HalfOpen after
//!   `recovery_window` elapses.
//! - **HalfOpen** (probing): one test request is allowed through. Success
//!   closes the circuit; failure reopens it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Circuit breaker state for a single provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    /// Healthy — requests pass through.
    Closed,
    /// Failing — skip this provider until `opened_at + recovery_window`.
    Open { opened_at: Instant },
    /// Recovery probe — allow one request, then decide.
    HalfOpen,
}

/// Per-provider failure tracking.
#[derive(Debug)]
struct ProviderCircuit {
    state: CircuitState,
    consecutive_failures: u32,
}

impl ProviderCircuit {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
        }
    }
}

/// Thread-safe circuit breaker that tracks failure state per provider name.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    inner: Arc<Mutex<HashMap<String, ProviderCircuit>>>,
    failure_threshold: u32,
    recovery_window: Duration,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// - `failure_threshold`: consecutive failures before opening the circuit.
    /// - `recovery_window`: how long a circuit stays open before transitioning
    ///   to half-open.
    pub fn new(failure_threshold: u32, recovery_window: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            failure_threshold: failure_threshold.max(1),
            recovery_window,
        }
    }

    /// Check whether a provider is available for requests.
    ///
    /// Returns `true` if the request should proceed (Closed or HalfOpen),
    /// `false` if the circuit is Open and the provider should be skipped.
    pub fn should_allow(&self, provider: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        let circuit = map
            .entry(provider.to_string())
            .or_insert_with(ProviderCircuit::new);

        match &circuit.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open { opened_at } => {
                if opened_at.elapsed() >= self.recovery_window {
                    // Transition to half-open: allow one probe request.
                    circuit.state = CircuitState::HalfOpen;
                    tracing::info!(
                        provider,
                        "Circuit breaker transitioning to half-open (recovery probe)"
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful request for a provider.
    /// Resets failure count and closes the circuit.
    pub fn record_success(&self, provider: &str) {
        let mut map = self.inner.lock().unwrap();
        let circuit = map
            .entry(provider.to_string())
            .or_insert_with(ProviderCircuit::new);

        let was_half_open = circuit.state == CircuitState::HalfOpen;
        circuit.consecutive_failures = 0;
        circuit.state = CircuitState::Closed;

        if was_half_open {
            tracing::info!(
                provider,
                "Circuit breaker closed (recovery probe succeeded)"
            );
        }
    }

    /// Record a failed request for a provider.
    /// Increments failure count and may open the circuit.
    pub fn record_failure(&self, provider: &str) {
        let mut map = self.inner.lock().unwrap();
        let circuit = map
            .entry(provider.to_string())
            .or_insert_with(ProviderCircuit::new);

        circuit.consecutive_failures += 1;

        match circuit.state {
            CircuitState::HalfOpen => {
                // Probe failed — reopen immediately.
                circuit.state = CircuitState::Open {
                    opened_at: Instant::now(),
                };
                tracing::warn!(provider, "Circuit breaker reopened (recovery probe failed)");
            }
            CircuitState::Closed => {
                if circuit.consecutive_failures >= self.failure_threshold {
                    circuit.state = CircuitState::Open {
                        opened_at: Instant::now(),
                    };
                    tracing::warn!(
                        provider,
                        consecutive_failures = circuit.consecutive_failures,
                        threshold = self.failure_threshold,
                        "Circuit breaker opened"
                    );
                }
            }
            CircuitState::Open { .. } => {
                // Already open, nothing to do.
            }
        }
    }

    /// Get the current state of a provider's circuit (for diagnostics).
    pub fn state(&self, provider: &str) -> CircuitState {
        let map = self.inner.lock().unwrap();
        map.get(provider)
            .map(|c| c.state.clone())
            .unwrap_or(CircuitState::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        assert_eq!(cb.state("test-provider"), CircuitState::Closed);
        assert!(cb.should_allow("test-provider"));
    }

    #[test]
    fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));

        cb.record_failure("p1");
        assert_eq!(cb.state("p1"), CircuitState::Closed);
        assert!(cb.should_allow("p1"));

        cb.record_failure("p1");
        assert_eq!(cb.state("p1"), CircuitState::Closed);

        cb.record_failure("p1");
        assert!(matches!(cb.state("p1"), CircuitState::Open { .. }));
        assert!(!cb.should_allow("p1"));
    }

    #[test]
    fn success_resets_failure_count() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));

        cb.record_failure("p1");
        cb.record_failure("p1");
        // Two failures, one more would open — but success resets.
        cb.record_success("p1");
        assert_eq!(cb.state("p1"), CircuitState::Closed);

        // Now need 3 fresh failures to open.
        cb.record_failure("p1");
        assert_eq!(cb.state("p1"), CircuitState::Closed);
    }

    #[test]
    fn transitions_to_half_open_after_recovery_window() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(0));

        cb.record_failure("p1");
        assert!(matches!(cb.state("p1"), CircuitState::Open { .. }));

        // Recovery window is 0ms, so should_allow transitions to HalfOpen.
        assert!(cb.should_allow("p1"));
        assert_eq!(cb.state("p1"), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_success_closes_circuit() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(0));

        cb.record_failure("p1");
        // Transition to half-open.
        assert!(cb.should_allow("p1"));
        assert_eq!(cb.state("p1"), CircuitState::HalfOpen);

        cb.record_success("p1");
        assert_eq!(cb.state("p1"), CircuitState::Closed);
    }

    #[test]
    fn half_open_failure_reopens_circuit() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(0));

        cb.record_failure("p1");
        assert!(cb.should_allow("p1")); // transitions to HalfOpen
        assert_eq!(cb.state("p1"), CircuitState::HalfOpen);

        cb.record_failure("p1");
        assert!(matches!(cb.state("p1"), CircuitState::Open { .. }));
    }

    #[test]
    fn independent_providers_have_separate_circuits() {
        let cb = CircuitBreaker::new(2, Duration::from_secs(60));

        cb.record_failure("p1");
        cb.record_failure("p1");
        assert!(matches!(cb.state("p1"), CircuitState::Open { .. }));

        // p2 is unaffected.
        assert_eq!(cb.state("p2"), CircuitState::Closed);
        assert!(cb.should_allow("p2"));
    }

    #[test]
    fn threshold_clamped_to_at_least_one() {
        let cb = CircuitBreaker::new(0, Duration::from_secs(60));
        // Threshold of 0 is clamped to 1.
        cb.record_failure("p1");
        assert!(matches!(cb.state("p1"), CircuitState::Open { .. }));
    }

    #[test]
    fn open_circuit_stays_open_before_recovery_window() {
        let cb = CircuitBreaker::new(1, Duration::from_secs(3600));

        cb.record_failure("p1");
        assert!(!cb.should_allow("p1"));
        // Still open — recovery window is 1 hour.
        assert!(!cb.should_allow("p1"));
    }
}
