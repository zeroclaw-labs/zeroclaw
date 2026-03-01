//! End-to-end test for circuit breaker with mock provider workflow.
//!
//! Simulates a bot workflow where primary provider fails and circuit breaker
//! ensures fallback to secondary provider.

use std::sync::Arc;
use std::time::Duration;
use zeroclaw::providers::health::ProviderHealthTracker;

/// Simulates a provider response scenario
struct MockProviderScenario {
    name: String,
    failure_count: usize,
    current_attempt: std::sync::atomic::AtomicUsize,
}

impl MockProviderScenario {
    fn new(name: &str, failure_count: usize) -> Self {
        Self {
            name: name.to_string(),
            failure_count,
            current_attempt: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn try_call(&self, health: &ProviderHealthTracker) -> Result<String, String> {
        // Check circuit breaker
        if let Err((remaining, _)) = health.should_try(&self.name) {
            return Err(format!(
                "Circuit open, {} seconds remaining",
                remaining.as_secs()
            ));
        }

        let attempt = self
            .current_attempt
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        if attempt < self.failure_count {
            let error = format!("Provider {} failed (attempt {})", self.name, attempt + 1);
            health.record_failure(&self.name, &error);
            Err(error)
        } else {
            health.record_success(&self.name);
            Ok(format!("Success from {}", self.name))
        }
    }
}

#[test]
fn e2e_circuit_breaker_enables_fallback() {
    let health = Arc::new(ProviderHealthTracker::new(3, Duration::from_secs(60), 100));

    // Primary provider: will fail 3 times (opens circuit)
    let primary = MockProviderScenario::new("primary", 3);

    // Secondary provider: will succeed immediately
    let secondary = MockProviderScenario::new("secondary", 0);

    // Simulate 5 bot messages with fallback logic
    let mut results = Vec::new();

    for msg_num in 1..=5 {
        let response;

        match primary.try_call(&health) {
            Ok(resp) => response = Some(resp),
            Err(err) => {
                // Primary failed, try secondary
                match secondary.try_call(&health) {
                    Ok(resp) => response = Some(resp),
                    Err(err2) => {
                        response = Some(format!("All providers failed: {}, {}", err, err2));
                    }
                }
            }
        }

        results.push((msg_num, response.unwrap()));
    }

    // Verify results
    assert_eq!(results.len(), 5);

    for (i, result) in results.iter().take(3).enumerate() {
        assert!(
            result.1.contains("Success from secondary"),
            "Message {} should use secondary after primary failure",
            i + 1
        );
    }

    for (i, result) in results.iter().skip(3).enumerate() {
        assert!(
            result.1.contains("Success from secondary") || result.1.contains("Circuit open"),
            "Message {} should skip primary (circuit open) and use secondary",
            i + 4
        );
    }

    // Verify circuit breaker state
    let primary_result = health.should_try("primary");
    assert!(
        primary_result.is_err(),
        "Primary circuit should remain open"
    );

    let secondary_result = health.should_try("secondary");
    assert!(
        secondary_result.is_ok(),
        "Secondary circuit should be closed"
    );
}
