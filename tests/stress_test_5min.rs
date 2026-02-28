//! Stress tests for circuit breaker under sustained load.
//!
//! Tests circuit breaker behavior over extended time periods with varying
//! failure patterns.

use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroclaw::providers::health::ProviderHealthTracker;

#[test]
#[ignore] // Run with: cargo test --release -- --ignored --test-threads=1
fn stress_test_1_minute_time_based_failures() {
    let health = Arc::new(ProviderHealthTracker::new(3, Duration::from_secs(5), 100));
    let start = Instant::now();
    let test_duration = Duration::from_secs(60);

    let mut total_attempts = 0;
    let mut successful_calls = 0;
    let mut circuit_blocks = 0;
    let mut provider_failures = 0;

    println!("Starting 1-minute stress test with time-based failures...");

    while start.elapsed() < test_duration {
        total_attempts += 1;

        // Check circuit breaker
        if health.should_try("stress-provider").is_err() {
            circuit_blocks += 1;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Simulate time-based failure window: fail during seconds 10-20 and 40-50
        let elapsed_secs = start.elapsed().as_secs();
        let should_fail = (10..20).contains(&elapsed_secs) || (40..50).contains(&elapsed_secs);

        if should_fail {
            health.record_failure("stress-provider", "Time-based failure window");
            provider_failures += 1;
        } else {
            health.record_success("stress-provider");
            successful_calls += 1;
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    println!("1-minute stress test completed:");
    println!("  Total attempts: {}", total_attempts);
    println!("  Successful calls: {}", successful_calls);
    println!("  Provider failures: {}", provider_failures);
    println!("  Circuit blocks: {}", circuit_blocks);

    assert!(
        total_attempts > 100,
        "Should have many attempts in 1 minute"
    );
    assert!(successful_calls > 0, "Should have some successful calls");
    assert!(
        circuit_blocks > 0,
        "Circuit should have blocked some attempts"
    );
}

#[test]
#[ignore] // Run with: cargo test --release -- --ignored --test-threads=1
fn stress_test_5_minute_sustained_load() {
    let health = Arc::new(ProviderHealthTracker::new(5, Duration::from_secs(10), 100));
    let start = Instant::now();
    let test_duration = Duration::from_secs(300); // 5 minutes

    let mut total_attempts = 0;
    let mut successful_calls = 0;
    let mut circuit_blocks = 0;
    let mut provider_failures = 0;

    println!("Starting 5-minute sustained load test...");

    while start.elapsed() < test_duration {
        total_attempts += 1;

        // Check circuit breaker
        if health.should_try("sustained-provider").is_err() {
            circuit_blocks += 1;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Simulate periodic failure bursts: fail every 60 seconds for 5 seconds
        let elapsed_secs = start.elapsed().as_secs();
        let cycle_position = elapsed_secs % 60;
        let should_fail = cycle_position >= 55; // Fail in last 5 seconds of each minute

        if should_fail {
            health.record_failure("sustained-provider", "Periodic failure burst");
            provider_failures += 1;
        } else {
            health.record_success("sustained-provider");
            successful_calls += 1;
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    println!("5-minute stress test completed:");
    println!("  Total attempts: {}", total_attempts);
    println!("  Successful calls: {}", successful_calls);
    println!("  Provider failures: {}", provider_failures);
    println!("  Circuit blocks: {}", circuit_blocks);

    assert!(
        total_attempts > 1000,
        "Should have many attempts in 5 minutes"
    );
    assert!(successful_calls > 0, "Should have some successful calls");
    assert!(
        provider_failures > 0,
        "Should have some provider failures during bursts"
    );
    assert!(
        circuit_blocks > 0,
        "Circuit should have blocked attempts during failure bursts"
    );

    // Success rate should be high (>80%) since we only fail 5s per minute
    let success_rate = (successful_calls as f64) / (total_attempts as f64) * 100.0;
    println!("  Success rate: {:.2}%", success_rate);
    assert!(
        success_rate > 80.0,
        "Success rate should be high with periodic failures"
    );
}
