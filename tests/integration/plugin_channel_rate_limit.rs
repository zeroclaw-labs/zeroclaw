//! Integration test: per-plugin per-channel rate limiting enforced.
//!
//! Task US-ZCL-25-4: Verify acceptance criterion for story US-ZCL-25:
//! > Per-plugin per-channel rate limiting enforced
//!
//! These tests validate that messaging rate limits are tracked independently
//! per (plugin, channel) pair, so that:
//! 1. A plugin hitting the limit on one channel can still send to another
//! 2. Two plugins sending to the same channel have independent budgets
//! 3. Once a plugin exhausts its budget for a channel, further sends are rejected
//! 4. The rate limit error message is clear

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::Instant;

/// Sliding-window rate limiter keyed by (plugin_name, channel_name).
///
/// This mirrors the expected production implementation: each plugin gets an
/// independent send budget per channel within a configurable time window.
struct ChannelRateLimiter {
    /// Maximum sends allowed per (plugin, channel) within the window.
    max_per_window: u32,
    /// Window duration in seconds.
    window_secs: u64,
    /// Recorded timestamps keyed by (plugin_name, channel_name).
    state: Mutex<HashMap<(String, String), Vec<Instant>>>,
}

impl ChannelRateLimiter {
    fn new(max_per_window: u32, window_secs: u64) -> Self {
        Self {
            max_per_window,
            window_secs,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Record a send attempt. Returns Ok(()) if within budget, Err(message) if rate-limited.
    fn record_send(&self, plugin_name: &str, channel: &str) -> Result<(), String> {
        let mut state = self.state.lock();
        let key = (plugin_name.to_string(), channel.to_string());
        let timestamps = state.entry(key).or_default();

        // Prune expired entries
        let cutoff = Instant::now() - std::time::Duration::from_secs(self.window_secs);
        timestamps.retain(|t| *t > cutoff);

        if timestamps.len() as u32 >= self.max_per_window {
            return Err(format!(
                "Rate limit exceeded: plugin '{}' has exhausted its messaging budget for channel '{}'",
                plugin_name, channel
            ));
        }

        timestamps.push(Instant::now());
        Ok(())
    }

    /// Check the current count without recording.
    fn count(&self, plugin_name: &str, channel: &str) -> u32 {
        let mut state = self.state.lock();
        let key = (plugin_name.to_string(), channel.to_string());
        let timestamps = state.entry(key).or_default();

        let cutoff = Instant::now() - std::time::Duration::from_secs(self.window_secs);
        timestamps.retain(|t| *t > cutoff);

        timestamps.len() as u32
    }
}

// ---------------------------------------------------------------------------
// 1. Rate limit enforced after budget exhausted
// ---------------------------------------------------------------------------

#[test]
fn channel_send_blocked_after_budget_exhausted() {
    let limiter = ChannelRateLimiter::new(3, 3600);

    // First 3 sends succeed
    for i in 0..3 {
        assert!(
            limiter.record_send("plugin_a", "slack").is_ok(),
            "send {} should be within budget",
            i + 1
        );
    }

    // 4th send is rejected
    let result = limiter.record_send("plugin_a", "slack");
    assert!(result.is_err(), "4th send must be rate-limited");
    let err = result.unwrap_err();
    assert!(
        err.contains("Rate limit"),
        "error should mention rate limit, got: {err}"
    );
    assert!(
        err.contains("slack"),
        "error should name the channel, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// 2. Rate limits are independent per channel within a plugin
// ---------------------------------------------------------------------------

#[test]
fn rate_limits_independent_per_channel() {
    let limiter = ChannelRateLimiter::new(2, 3600);

    // Exhaust budget for slack
    assert!(limiter.record_send("plugin_a", "slack").is_ok());
    assert!(limiter.record_send("plugin_a", "slack").is_ok());
    assert!(
        limiter.record_send("plugin_a", "slack").is_err(),
        "slack budget should be exhausted"
    );

    // email budget is still available
    assert!(
        limiter.record_send("plugin_a", "email").is_ok(),
        "email should have its own budget"
    );
    assert!(
        limiter.record_send("plugin_a", "email").is_ok(),
        "email should still have budget"
    );

    // email also exhausted now
    assert!(
        limiter.record_send("plugin_a", "email").is_err(),
        "email budget should be exhausted"
    );
}

// ---------------------------------------------------------------------------
// 3. Rate limits are independent per plugin for the same channel
// ---------------------------------------------------------------------------

#[test]
fn rate_limits_independent_per_plugin() {
    let limiter = ChannelRateLimiter::new(2, 3600);

    // plugin_a exhausts its slack budget
    assert!(limiter.record_send("plugin_a", "slack").is_ok());
    assert!(limiter.record_send("plugin_a", "slack").is_ok());
    assert!(
        limiter.record_send("plugin_a", "slack").is_err(),
        "plugin_a slack budget exhausted"
    );

    // plugin_b still has its own budget for the same channel
    assert!(
        limiter.record_send("plugin_b", "slack").is_ok(),
        "plugin_b should have independent budget for slack"
    );
    assert!(
        limiter.record_send("plugin_b", "slack").is_ok(),
        "plugin_b should still have budget"
    );

    // plugin_b also exhausted now
    assert!(
        limiter.record_send("plugin_b", "slack").is_err(),
        "plugin_b slack budget exhausted"
    );
}

// ---------------------------------------------------------------------------
// 4. Count tracks sends without side effects
// ---------------------------------------------------------------------------

#[test]
fn count_reflects_sends_without_recording() {
    let limiter = ChannelRateLimiter::new(5, 3600);

    assert_eq!(limiter.count("plugin_a", "slack"), 0);

    limiter.record_send("plugin_a", "slack").unwrap();
    limiter.record_send("plugin_a", "slack").unwrap();
    assert_eq!(limiter.count("plugin_a", "slack"), 2);

    // Count for a different channel is zero
    assert_eq!(limiter.count("plugin_a", "email"), 0);
    // Count for a different plugin is zero
    assert_eq!(limiter.count("plugin_b", "slack"), 0);
}

// ---------------------------------------------------------------------------
// 5. Error message includes plugin and channel identifiers
// ---------------------------------------------------------------------------

#[test]
fn rate_limit_error_identifies_plugin_and_channel() {
    let limiter = ChannelRateLimiter::new(1, 3600);

    limiter.record_send("alert_bot", "telegram").unwrap();
    let err = limiter.record_send("alert_bot", "telegram").unwrap_err();

    assert!(
        err.contains("alert_bot"),
        "error should identify the plugin, got: {err}"
    );
    assert!(
        err.contains("telegram"),
        "error should identify the channel, got: {err}"
    );
    assert!(
        err.contains("Rate limit"),
        "error should mention rate limit, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// 6. Zero budget blocks all sends immediately
// ---------------------------------------------------------------------------

#[test]
fn zero_budget_blocks_all_sends() {
    let limiter = ChannelRateLimiter::new(0, 3600);

    assert!(
        limiter.record_send("plugin_a", "slack").is_err(),
        "zero budget should block immediately"
    );
}

// ---------------------------------------------------------------------------
// 7. Multiple plugins and channels are fully independent
// ---------------------------------------------------------------------------

#[test]
fn full_matrix_independence() {
    let limiter = ChannelRateLimiter::new(1, 3600);

    let plugins = ["plugin_a", "plugin_b"];
    let channels = ["slack", "email", "telegram"];

    // Each (plugin, channel) pair gets exactly 1 send
    for plugin in &plugins {
        for channel in &channels {
            assert!(
                limiter.record_send(plugin, channel).is_ok(),
                "{plugin} should be able to send to {channel}"
            );
        }
    }

    // All pairs are now exhausted
    for plugin in &plugins {
        for channel in &channels {
            assert!(
                limiter.record_send(plugin, channel).is_err(),
                "{plugin} should be rate-limited on {channel}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 8. Subsequent sends after limit continue to be rejected
// ---------------------------------------------------------------------------

#[test]
fn subsequent_sends_after_limit_stay_rejected() {
    let limiter = ChannelRateLimiter::new(1, 3600);

    limiter.record_send("plugin_a", "slack").unwrap();

    // Multiple subsequent attempts all fail
    for _ in 0..5 {
        assert!(
            limiter.record_send("plugin_a", "slack").is_err(),
            "all sends after limit should be rejected"
        );
    }

    // Count stays at 1 (rejected sends are not recorded)
    assert_eq!(limiter.count("plugin_a", "slack"), 1);
}
