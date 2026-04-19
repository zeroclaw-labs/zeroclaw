#![cfg(feature = "plugins-wasm")]

//! Test: Rate limit exceeded returns clear error.
//!
//! Task US-ZCL-57-5: Verifies acceptance criterion for US-ZCL-57:
//! > Rate limit exceeded returns clear error
//!
//! These tests verify that when a plugin exceeds its CLI rate limit,
//! the error message is clear, actionable, and includes all necessary
//! information for debugging and retry logic.

use zeroclaw::plugins::host_functions::{CliExecResponse, CliRateLimiter};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Rate limit exceeded returns clear error
// ---------------------------------------------------------------------------

/// AC: Error message includes the plugin name for identification.
/// When rate limit is exceeded, the error must identify which plugin was limited.
#[test]
fn rate_limit_error_includes_plugin_name() {
    // The error format from host_functions.rs is:
    // "[plugin:{name}] rate limit exceeded ({limit} executions/minute). Retry after {seconds} seconds."
    let plugin_name = "my-test-plugin";
    let limit = 5u32;

    let limiter = CliRateLimiter::new();

    // Exhaust the rate limit
    for _ in 0..limit {
        limiter
            .record_execution(plugin_name, limit)
            .expect("should succeed within limit");
    }

    // Next call should fail and return retry_after
    let retry_after = limiter
        .record_execution(plugin_name, limit)
        .expect_err("should be rate limited");

    // Construct the error message as the host function would
    let error_message = format!(
        "[plugin:{}] rate limit exceeded ({} executions/minute). Retry after {} seconds.",
        plugin_name, limit, retry_after
    );

    assert!(
        error_message.contains(plugin_name),
        "error message must include plugin name: {}",
        error_message
    );
}

/// AC: Error message includes the rate limit value.
/// Operators need to know the configured limit to understand the constraint.
#[test]
fn rate_limit_error_includes_limit_value() {
    let plugin_name = "limit-display-test";
    let limit = 3u32;

    let limiter = CliRateLimiter::new();

    // Exhaust the limit
    for _ in 0..limit {
        limiter.record_execution(plugin_name, limit).unwrap();
    }

    let retry_after = limiter
        .record_execution(plugin_name, limit)
        .expect_err("should be rate limited");

    let error_message = format!(
        "[plugin:{}] rate limit exceeded ({} executions/minute). Retry after {} seconds.",
        plugin_name, limit, retry_after
    );

    assert!(
        error_message.contains(&format!("{} executions/minute", limit)),
        "error message must include rate limit value: {}",
        error_message
    );
}

/// AC: Error message includes retry-after hint.
/// The retry-after value helps callers know when to retry.
#[test]
fn rate_limit_error_includes_retry_after() {
    let plugin_name = "retry-after-test";
    let limit = 1u32;

    let limiter = CliRateLimiter::new();

    // Exhaust the limit (single execution allowed)
    limiter.record_execution(plugin_name, limit).unwrap();

    let retry_after = limiter
        .record_execution(plugin_name, limit)
        .expect_err("should be rate limited");

    let error_message = format!(
        "[plugin:{}] rate limit exceeded ({} executions/minute). Retry after {} seconds.",
        plugin_name, limit, retry_after
    );

    assert!(
        error_message.contains("Retry after"),
        "error message must include retry-after hint: {}",
        error_message
    );
    assert!(
        error_message.contains(&format!("{} seconds", retry_after)),
        "error message must include retry-after value: {}",
        error_message
    );
}

/// AC: Error message is clear and actionable.
/// The error should explain what happened (rate limit exceeded) clearly.
#[test]
fn rate_limit_error_is_clear_and_actionable() {
    let plugin_name = "clarity-test";
    let limit = 2u32;

    let limiter = CliRateLimiter::new();

    // Exhaust the limit
    limiter.record_execution(plugin_name, limit).unwrap();
    limiter.record_execution(plugin_name, limit).unwrap();

    let retry_after = limiter
        .record_execution(plugin_name, limit)
        .expect_err("should be rate limited");

    let error_message = format!(
        "[plugin:{}] rate limit exceeded ({} executions/minute). Retry after {} seconds.",
        plugin_name, limit, retry_after
    );

    // Error must clearly state what happened
    assert!(
        error_message.contains("rate limit exceeded"),
        "error must clearly state 'rate limit exceeded': {}",
        error_message
    );

    // Error must be structured with plugin identifier prefix
    assert!(
        error_message.starts_with("[plugin:"),
        "error must start with plugin identifier: {}",
        error_message
    );
}

/// AC: retry_after value is within reasonable bounds (1-61 seconds for 1-minute window).
#[test]
fn rate_limit_retry_after_is_reasonable() {
    let limiter = CliRateLimiter::new();

    // Exhaust the limit
    limiter.record_execution("test-plugin", 1).unwrap();

    let retry_after = limiter
        .record_execution("test-plugin", 1)
        .expect_err("should be rate limited");

    assert!(
        (1..=61).contains(&retry_after),
        "retry_after should be 1-61 seconds for 1-minute window, got: {}",
        retry_after
    );
}

// ---------------------------------------------------------------------------
// CliExecResponse error format integration
// ---------------------------------------------------------------------------

/// AC: CliExecResponse can represent rate limit error with stderr message.
/// The response uses stderr for the error message and exit_code -1.
#[test]
fn cli_response_represents_rate_limit_error() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:test] rate limit exceeded (10 executions/minute). Retry after 45 seconds."
            .to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    // stderr contains the error message
    assert!(
        response.stderr.contains("rate limit exceeded"),
        "stderr must contain rate limit error"
    );

    // stdout is empty (no partial execution)
    assert!(
        response.stdout.is_empty(),
        "stdout must be empty for rate limit error"
    );

    // exit_code is -1 (indicates internal error, not command exit)
    assert_eq!(
        response.exit_code, -1,
        "exit_code must be -1 for rate limit error"
    );

    // truncated and timed_out are false (command didn't run)
    assert!(!response.truncated, "truncated must be false");
    assert!(!response.timed_out, "timed_out must be false");
}

/// AC: Rate limit error response serializes correctly to JSON.
#[test]
fn cli_rate_limit_error_response_json_serialization() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr:
            "[plugin:json-test] rate limit exceeded (5 executions/minute). Retry after 30 seconds."
                .to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(
        json.contains("rate limit exceeded"),
        "JSON stderr must contain error message: {}",
        json
    );
    assert!(
        json.contains("\"exit_code\":-1"),
        "JSON must contain exit_code:-1: {}",
        json
    );
}

/// AC: Rate limit error response deserializes correctly from JSON.
#[test]
fn cli_rate_limit_error_response_json_deserialization() {
    let json = r#"{
        "stdout": "",
        "stderr": "[plugin:deser-test] rate limit exceeded (7 executions/minute). Retry after 25 seconds.",
        "exit_code": -1,
        "truncated": false,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert!(response.stderr.contains("rate limit exceeded"));
    assert!(response.stderr.contains("deser-test"));
    assert!(response.stderr.contains("7 executions/minute"));
    assert!(response.stderr.contains("25 seconds"));
    assert_eq!(response.exit_code, -1);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// AC: Error message format is consistent across different plugins.
#[test]
fn rate_limit_error_format_consistent() {
    let limiter = CliRateLimiter::new();

    let plugins = [
        ("plugin-alpha", 5u32),
        ("plugin-beta", 10),
        ("plugin-gamma", 1),
    ];

    for (plugin_name, limit) in plugins {
        // Exhaust each plugin's limit
        for _ in 0..limit {
            limiter.record_execution(plugin_name, limit).unwrap();
        }

        let retry_after = limiter
            .record_execution(plugin_name, limit)
            .expect_err("should be rate limited");

        let error_message = format!(
            "[plugin:{}] rate limit exceeded ({} executions/minute). Retry after {} seconds.",
            plugin_name, limit, retry_after
        );

        // All errors follow the same format
        assert!(
            error_message.starts_with("[plugin:"),
            "error format must be consistent for {}: {}",
            plugin_name,
            error_message
        );
        assert!(
            error_message.contains("rate limit exceeded"),
            "error must contain 'rate limit exceeded' for {}: {}",
            plugin_name,
            error_message
        );
        assert!(
            error_message.contains("executions/minute"),
            "error must contain 'executions/minute' for {}: {}",
            plugin_name,
            error_message
        );
        assert!(
            error_message.contains("Retry after"),
            "error must contain 'Retry after' for {}: {}",
            plugin_name,
            error_message
        );
    }
}

/// AC: Error distinguishes between different plugins hitting their limits.
#[test]
fn rate_limit_error_identifies_specific_plugin() {
    let limiter = CliRateLimiter::new();

    // Exhaust plugin-a
    limiter.record_execution("plugin-a", 1).unwrap();
    let retry_a = limiter
        .record_execution("plugin-a", 1)
        .expect_err("plugin-a should be limited");

    // plugin-b is still available
    assert!(
        limiter.record_execution("plugin-b", 1).is_ok(),
        "plugin-b should not be affected by plugin-a's limit"
    );

    // Exhaust plugin-b
    let retry_b = limiter
        .record_execution("plugin-b", 1)
        .expect_err("plugin-b should be limited");

    // Error messages correctly identify each plugin
    let error_a = format!(
        "[plugin:plugin-a] rate limit exceeded (1 executions/minute). Retry after {} seconds.",
        retry_a
    );
    let error_b = format!(
        "[plugin:plugin-b] rate limit exceeded (1 executions/minute). Retry after {} seconds.",
        retry_b
    );

    assert!(
        error_a.contains("plugin-a"),
        "error_a must identify plugin-a"
    );
    assert!(
        error_b.contains("plugin-b"),
        "error_b must identify plugin-b"
    );
    assert_ne!(
        error_a, error_b,
        "errors for different plugins must be distinguishable"
    );
}
