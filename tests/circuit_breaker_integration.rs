//! Integration tests for circuit breaker behavior.
//!
//! Tests circuit breaker opening, closing, and interaction with ReliableProvider.

use std::time::Duration;
use zeroclaw::providers::health::ProviderHealthTracker;

#[test]
fn circuit_breaker_opens_after_failures() {
    let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

    // Record failures up to threshold
    tracker.record_failure("test-provider", "error 1");
    tracker.record_failure("test-provider", "error 2");

    // Should still be allowed before threshold
    assert!(tracker.should_try("test-provider").is_ok());

    // Third failure should open circuit
    tracker.record_failure("test-provider", "error 3");

    // Circuit should now be open
    let result = tracker.should_try("test-provider");
    assert!(result.is_err(), "Circuit should be open after threshold");

    if let Err((remaining, state)) = result {
        assert!(remaining.as_secs() > 0 && remaining.as_secs() <= 60);
        assert_eq!(state.failure_count, 3);
    }
}

#[test]
fn circuit_breaker_closes_after_timeout() {
    let tracker = ProviderHealthTracker::new(3, Duration::from_millis(100), 100);

    // Open circuit
    tracker.record_failure("test-provider", "error 1");
    tracker.record_failure("test-provider", "error 2");
    tracker.record_failure("test-provider", "error 3");

    // Verify circuit is open
    assert!(tracker.should_try("test-provider").is_err());

    // Wait for cooldown
    std::thread::sleep(Duration::from_millis(120));

    // Circuit should be closed (timeout expired)
    assert!(
        tracker.should_try("test-provider").is_ok(),
        "Circuit should close after cooldown period"
    );
}

#[test]
fn circuit_breaker_resets_on_success() {
    let tracker = ProviderHealthTracker::new(3, Duration::from_secs(60), 100);

    // Record failures below threshold
    tracker.record_failure("test-provider", "error 1");
    tracker.record_failure("test-provider", "error 2");

    let state = tracker.get_state("test-provider");
    assert_eq!(state.failure_count, 2);

    // Success should reset counter
    tracker.record_success("test-provider");

    let state = tracker.get_state("test-provider");
    assert_eq!(state.failure_count, 0, "Success should reset failure count");
    assert_eq!(state.last_error, None, "Success should clear last error");

    // Should still be allowed
    assert!(tracker.should_try("test-provider").is_ok());
}
