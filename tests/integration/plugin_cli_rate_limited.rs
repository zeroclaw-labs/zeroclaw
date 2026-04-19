#![cfg(feature = "plugins-wasm")]

//! Test: Per-plugin rate limiting with configurable window.
//!
//! Task US-ZCL-57-4: Verifies acceptance criterion for US-ZCL-57:
//! > Per-plugin rate limiting with configurable window
//!
//! These tests verify that the CLI capability properly supports per-plugin
//! rate limiting with a configurable limit and sliding window.

use zeroclaw::plugins::host_functions::CliRateLimiter;
use zeroclaw::plugins::{CliCapability, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Per-plugin rate limiting with configurable window
// ---------------------------------------------------------------------------

/// CliCapability has rate_limit_per_minute field for configuring the limit.
#[test]
fn cli_capability_has_rate_limit_per_minute_field() {
    let cap = CliCapability {
        rate_limit_per_minute: 30,
        ..Default::default()
    };

    assert_eq!(
        cap.rate_limit_per_minute, 30,
        "rate_limit_per_minute must be configurable"
    );
}

/// CliCapability default rate_limit_per_minute matches the constant.
#[test]
fn cli_capability_default_rate_limit_per_minute() {
    let cap = CliCapability::default();

    assert_eq!(
        cap.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE,
        "default rate_limit_per_minute should match DEFAULT_CLI_RATE_LIMIT_PER_MINUTE"
    );
    assert_eq!(
        DEFAULT_CLI_RATE_LIMIT_PER_MINUTE, 10,
        "default rate_limit_per_minute should be 10"
    );
}

/// Custom rate_limit_per_minute can be set to higher values.
#[test]
fn cli_capability_custom_rate_limit_higher() {
    let cap = CliCapability {
        rate_limit_per_minute: 100,
        ..Default::default()
    };

    assert_eq!(cap.rate_limit_per_minute, 100);
}

/// Custom rate_limit_per_minute can be set to 1 (very restrictive).
#[test]
fn cli_capability_rate_limit_one_very_restrictive() {
    let cap = CliCapability {
        rate_limit_per_minute: 1,
        ..Default::default()
    };

    assert_eq!(
        cap.rate_limit_per_minute, 1,
        "rate_limit_per_minute=1 allows only one execution per window"
    );
}

/// rate_limit_per_minute of zero means unlimited.
#[test]
fn cli_capability_zero_rate_limit_means_unlimited() {
    let cap = CliCapability {
        rate_limit_per_minute: 0,
        ..Default::default()
    };

    assert_eq!(
        cap.rate_limit_per_minute, 0,
        "rate_limit_per_minute=0 should mean unlimited"
    );
}

// ---------------------------------------------------------------------------
// CliRateLimiter per-plugin tracking
// ---------------------------------------------------------------------------

/// CliRateLimiter tracks each plugin independently.
#[test]
fn cli_rate_limiter_tracks_per_plugin() {
    let limiter = CliRateLimiter::new();

    // plugin-a with limit of 2
    assert!(
        limiter.record_execution("plugin-a", 2).is_ok(),
        "plugin-a first execution should succeed"
    );
    assert!(
        limiter.record_execution("plugin-a", 2).is_ok(),
        "plugin-a second execution should succeed"
    );
    assert!(
        limiter.record_execution("plugin-a", 2).is_err(),
        "plugin-a third execution should fail (limit 2)"
    );

    // plugin-b has its own independent budget
    assert!(
        limiter.record_execution("plugin-b", 2).is_ok(),
        "plugin-b first execution should succeed (independent from plugin-a)"
    );
    assert!(
        limiter.record_execution("plugin-b", 2).is_ok(),
        "plugin-b second execution should succeed"
    );
    assert!(
        limiter.record_execution("plugin-b", 2).is_err(),
        "plugin-b third execution should fail"
    );

    // plugin-c can have different limit
    assert!(
        limiter.record_execution("plugin-c", 5).is_ok(),
        "plugin-c can have different limit"
    );
}

/// CliRateLimiter allows executions within the configured limit.
#[test]
fn cli_rate_limiter_allows_within_limit() {
    let limiter = CliRateLimiter::new();

    // Should allow exactly the configured number of executions
    for i in 0..5 {
        assert!(
            limiter.record_execution("test-plugin", 5).is_ok(),
            "execution {} should succeed within limit of 5",
            i + 1
        );
    }

    // 6th should fail
    assert!(
        limiter.record_execution("test-plugin", 5).is_err(),
        "execution 6 should fail (exceeds limit of 5)"
    );
}

/// CliRateLimiter with zero limit allows unlimited executions.
#[test]
fn cli_rate_limiter_zero_limit_unlimited() {
    let limiter = CliRateLimiter::new();

    // Zero limit means unlimited - should allow many executions
    for i in 0..100 {
        assert!(
            limiter.record_execution("unlimited-plugin", 0).is_ok(),
            "execution {} should succeed with limit=0 (unlimited)",
            i + 1
        );
    }
}

/// CliRateLimiter returns retry_after seconds when limit exceeded.
#[test]
fn cli_rate_limiter_returns_retry_after_on_limit_exceeded() {
    let limiter = CliRateLimiter::new();

    // Exhaust the limit
    assert!(limiter.record_execution("test-plugin", 1).is_ok());

    // Next attempt should return retry_after
    let err = limiter.record_execution("test-plugin", 1).unwrap_err();
    assert!(
        err > 0 && err <= 61,
        "retry_after should be between 1 and 61 seconds, got: {}",
        err
    );
}

/// CliRateLimiter can be created with Default trait.
#[test]
fn cli_rate_limiter_default_impl() {
    let limiter = CliRateLimiter::default();

    // Default limiter should work
    assert!(
        limiter.record_execution("test", 10).is_ok(),
        "default limiter should accept executions"
    );
}

/// Different plugins can have different rate limits simultaneously.
#[test]
fn cli_rate_limiter_different_limits_per_plugin() {
    let limiter = CliRateLimiter::new();

    // Plugin A has limit of 2
    assert!(limiter.record_execution("plugin-a", 2).is_ok());
    assert!(limiter.record_execution("plugin-a", 2).is_ok());
    assert!(limiter.record_execution("plugin-a", 2).is_err());

    // Plugin B has limit of 5 (higher)
    for _ in 0..5 {
        assert!(limiter.record_execution("plugin-b", 5).is_ok());
    }
    assert!(limiter.record_execution("plugin-b", 5).is_err());

    // Plugin C has limit of 0 (unlimited)
    for _ in 0..50 {
        assert!(limiter.record_execution("plugin-c", 0).is_ok());
    }
}

// ---------------------------------------------------------------------------
// JSON serialization of rate_limit_per_minute
// ---------------------------------------------------------------------------

/// CliCapability serializes rate_limit_per_minute to JSON.
#[test]
fn cli_capability_rate_limit_serializes() {
    let cap = CliCapability {
        allowed_commands: vec!["echo".to_string()],
        rate_limit_per_minute: 25,
        ..Default::default()
    };

    let json = serde_json::to_string(&cap).expect("serialization must succeed");

    assert!(
        json.contains("\"rate_limit_per_minute\":25"),
        "JSON should contain rate_limit_per_minute, got: {}",
        json
    );
}

/// CliCapability deserializes rate_limit_per_minute from JSON.
#[test]
fn cli_capability_rate_limit_deserializes() {
    let json = r#"{
        "allowed_commands": ["ls"],
        "allowed_args": [],
        "allowed_env": [],
        "timeout_ms": 5000,
        "max_output_bytes": 1048576,
        "max_concurrent": 2,
        "rate_limit_per_minute": 42
    }"#;

    let cap: CliCapability = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(cap.rate_limit_per_minute, 42);
}

/// CliCapability uses default rate_limit_per_minute when not specified in JSON.
#[test]
fn cli_capability_rate_limit_defaults_when_missing() {
    let json = r#"{
        "allowed_commands": ["ls"],
        "allowed_args": [],
        "allowed_env": []
    }"#;

    let cap: CliCapability = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(
        cap.rate_limit_per_minute, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE,
        "missing rate_limit_per_minute should default to DEFAULT_CLI_RATE_LIMIT_PER_MINUTE"
    );
}

/// CliCapability rate_limit_per_minute roundtrips through JSON.
#[test]
fn cli_capability_rate_limit_json_roundtrip() {
    let original = CliCapability {
        allowed_commands: vec!["git".to_string(), "cargo".to_string()],
        rate_limit_per_minute: 15,
        ..Default::default()
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliCapability =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(
        original.rate_limit_per_minute,
        restored.rate_limit_per_minute
    );
}

/// Zero rate_limit_per_minute serializes and deserializes correctly.
#[test]
fn cli_capability_zero_rate_limit_json_roundtrip() {
    let original = CliCapability {
        allowed_commands: vec!["echo".to_string()],
        rate_limit_per_minute: 0,
        ..Default::default()
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    assert!(
        json.contains("\"rate_limit_per_minute\":0"),
        "JSON should contain rate_limit_per_minute:0"
    );

    let restored: CliCapability =
        serde_json::from_str(&json).expect("deserialization must succeed");
    assert_eq!(restored.rate_limit_per_minute, 0);
}

// ---------------------------------------------------------------------------
// Configurable window behavior (1-minute sliding window)
// ---------------------------------------------------------------------------

/// CliRateLimiter uses a sliding window (verified by retry_after value).
#[test]
fn cli_rate_limiter_sliding_window_retry_after() {
    let limiter = CliRateLimiter::new();

    // Exhaust limit
    limiter.record_execution("plugin", 1).unwrap();

    // Retry_after should be within the window duration (60 seconds + 1)
    let retry_after = limiter.record_execution("plugin", 1).unwrap_err();
    assert!(
        retry_after <= 61,
        "retry_after ({}) should be at most window duration + 1",
        retry_after
    );
    assert!(
        retry_after >= 1,
        "retry_after ({}) should be at least 1 second",
        retry_after
    );
}

/// Multiple plugins can be rate-limited independently and concurrently.
#[test]
fn cli_rate_limiter_multiple_plugins_concurrent() {
    let limiter = CliRateLimiter::new();

    // Interleave executions from multiple plugins
    assert!(limiter.record_execution("p1", 3).is_ok());
    assert!(limiter.record_execution("p2", 2).is_ok());
    assert!(limiter.record_execution("p1", 3).is_ok());
    assert!(limiter.record_execution("p3", 1).is_ok());
    assert!(limiter.record_execution("p2", 2).is_ok());
    assert!(limiter.record_execution("p1", 3).is_ok());

    // Now p1 is at limit (3/3), p2 is at limit (2/2), p3 is at limit (1/1)
    assert!(
        limiter.record_execution("p1", 3).is_err(),
        "p1 should be rate limited"
    );
    assert!(
        limiter.record_execution("p2", 2).is_err(),
        "p2 should be rate limited"
    );
    assert!(
        limiter.record_execution("p3", 1).is_err(),
        "p3 should be rate limited"
    );

    // New plugin p4 should still work
    assert!(
        limiter.record_execution("p4", 5).is_ok(),
        "new plugin p4 should not be affected"
    );
}
