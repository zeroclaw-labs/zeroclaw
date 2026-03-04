//! Complex stress test with multiple fallback chains.
//!
//! Tests circuit breaker behavior with realistic multi-tier provider fallback
//! chains under sustained load.

use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroclaw::providers::health::ProviderHealthTracker;

/// Simulates a provider with configurable failure pattern
struct ProviderSimulator {
    name: String,
    /// (start_sec, end_sec, failure_count) - fail between these seconds, then succeed
    failure_windows: Vec<(u64, u64, usize)>,
    attempts: std::sync::atomic::AtomicUsize,
}

impl ProviderSimulator {
    fn new(name: &str, failure_windows: Vec<(u64, u64, usize)>) -> Self {
        Self {
            name: name.to_string(),
            failure_windows,
            attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn try_call(
        &self,
        health: &ProviderHealthTracker,
        elapsed_secs: u64,
    ) -> Result<String, String> {
        // Check circuit breaker first
        if let Err((remaining, _)) = health.should_try(&self.name) {
            return Err(format!("Circuit open ({}s remaining)", remaining.as_secs()));
        }

        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // Check if we're in a failure window
        for (start, end, fail_count) in &self.failure_windows {
            if elapsed_secs >= *start
                && elapsed_secs < *end
                && (*fail_count == 0 || attempt % (fail_count + 1) < *fail_count)
            {
                let error = format!("{} failure in window {}-{}s", self.name, start, end);
                health.record_failure(&self.name, &error);
                return Err(error);
            }
        }

        // Success
        health.record_success(&self.name);
        Ok(format!("Success from {}", self.name))
    }

    #[allow(dead_code)]
    fn reset(&self) {
        self.attempts.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
#[ignore] // Run with: cargo test --release -- --ignored --test-threads=1
fn stress_test_complex_multi_chain_fallback() {
    let health = Arc::new(ProviderHealthTracker::new(3, Duration::from_secs(5), 100));
    let start = Instant::now();
    let test_duration = Duration::from_secs(120); // 2 minutes

    // Chain 1: 3-tier fallback (primary → secondary → tertiary)
    // Primary fails 0-30s, secondary fails 30-60s, tertiary is stable
    let chain1_primary = ProviderSimulator::new("chain1-primary", vec![(0, 30, 3)]);
    let chain1_secondary = ProviderSimulator::new("chain1-secondary", vec![(30, 60, 3)]);
    let chain1_tertiary = ProviderSimulator::new("chain1-tertiary", vec![]);

    // Chain 2: 2-tier fallback with periodic failures
    // Primary fails 50-70s, backup is stable
    let chain2_primary = ProviderSimulator::new("chain2-primary", vec![(50, 70, 0)]); // Always fail in window
    let chain2_backup = ProviderSimulator::new("chain2-backup", vec![]);

    let chains = [
        vec![&chain1_primary, &chain1_secondary, &chain1_tertiary],
        vec![&chain2_primary, &chain2_backup],
    ];

    let mut total_requests = 0;
    let mut chain_successes = [0, 0];
    let mut chain_failures = [0, 0];

    println!("Starting 2-minute complex multi-chain stress test...");

    while start.elapsed() < test_duration {
        let elapsed_secs = start.elapsed().as_secs();

        // Alternate between chains
        let chain_idx = total_requests % 2;
        let chain = &chains[chain_idx];

        total_requests += 1;

        // Try providers in fallback order
        let mut success = false;
        for provider in chain {
            match provider.try_call(&health, elapsed_secs) {
                Ok(_) => {
                    chain_successes[chain_idx] += 1;
                    success = true;
                    break;
                }
                Err(_) => continue,
            }
        }

        if !success {
            chain_failures[chain_idx] += 1;
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    println!("Complex multi-chain stress test completed:");
    println!("  Total requests: {}", total_requests);
    println!("  Chain 1 successes: {}", chain_successes[0]);
    println!("  Chain 1 failures: {}", chain_failures[0]);
    println!("  Chain 2 successes: {}", chain_successes[1]);
    println!("  Chain 2 failures: {}", chain_failures[1]);

    // Both chains should have high success rates due to fallback
    let chain1_success_rate =
        (chain_successes[0] as f64) / ((chain_successes[0] + chain_failures[0]) as f64) * 100.0;
    let chain2_success_rate =
        (chain_successes[1] as f64) / ((chain_successes[1] + chain_failures[1]) as f64) * 100.0;

    println!("  Chain 1 success rate: {:.2}%", chain1_success_rate);
    println!("  Chain 2 success rate: {:.2}%", chain2_success_rate);

    assert!(
        total_requests > 500,
        "Should have many requests in 2 minutes"
    );

    assert!(
        chain1_success_rate > 95.0,
        "Chain 1 should have high success rate with 3-tier fallback"
    );

    assert!(
        chain2_success_rate > 95.0,
        "Chain 2 should have high success rate with 2-tier fallback"
    );

    // Overall success rate should be very high
    let total_successes = chain_successes[0] + chain_successes[1];
    let overall_success_rate = (total_successes as f64) / (total_requests as f64) * 100.0;
    println!("  Overall success rate: {:.2}%", overall_success_rate);

    assert!(
        overall_success_rate > 95.0,
        "Overall success rate should be very high with multi-tier fallback chains"
    );
}
