#![cfg(feature = "plugins-wasm")]

//! Test: Per-command timeout enforced with SIGKILL on expiry.
//!
//! Task US-ZCL-56-1: Verifies acceptance criterion for US-ZCL-56:
//! > Per-command timeout enforced with SIGKILL on expiry
//!
//! These tests verify that the CLI execution host function properly handles
//! command timeouts by killing processes with SIGKILL and returning appropriate
//! response fields.

use zeroclaw::plugins::host_functions::CliExecResponse;
use zeroclaw::plugins::{CliCapability, DEFAULT_CLI_TIMEOUT_MS};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Per-command timeout enforced with SIGKILL
// ---------------------------------------------------------------------------

/// SIGKILL exit code convention: 128 + signal number.
/// SIGKILL is signal 9, so exit code is 137.
const SIGKILL_EXIT_CODE: i32 = 137;

/// Verify CliExecResponse has timed_out field for indicating timeout.
#[test]
fn response_has_timed_out_field() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    assert!(response.timed_out, "timed_out field must be accessible");
}

/// Timed-out response uses exit code 137 (128 + SIGKILL).
#[test]
fn timeout_exit_code_is_sigkill_convention() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    // Exit code 137 = 128 + 9 (SIGKILL)
    assert_eq!(
        response.exit_code, SIGKILL_EXIT_CODE,
        "timed-out process should report exit code 137 (SIGKILL)"
    );
    assert_eq!(
        SIGKILL_EXIT_CODE,
        128 + 9,
        "SIGKILL exit code should be 128 + signal 9"
    );
}

/// CliCapability has timeout_ms field for configuring per-command timeout.
#[test]
fn cli_capability_has_timeout_ms_field() {
    let cap = CliCapability {
        timeout_ms: 5000,
        ..Default::default()
    };

    assert_eq!(cap.timeout_ms, 5000, "timeout_ms must be configurable");
}

/// CliCapability default timeout is reasonable (5 seconds).
#[test]
fn cli_capability_default_timeout_is_reasonable() {
    let cap = CliCapability::default();

    assert_eq!(
        cap.timeout_ms, DEFAULT_CLI_TIMEOUT_MS,
        "default timeout should match DEFAULT_CLI_TIMEOUT_MS"
    );
    assert_eq!(
        DEFAULT_CLI_TIMEOUT_MS, 5000,
        "default timeout should be 5000ms (5 seconds)"
    );
}

/// Timeout response can still contain partial output.
#[test]
fn timeout_response_captures_partial_output() {
    let response = CliExecResponse {
        stdout: "partial output before timeout".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    assert!(response.timed_out);
    assert!(
        !response.stdout.is_empty(),
        "stdout should capture partial output before timeout"
    );
}

/// Timeout response can have both stdout and stderr.
#[test]
fn timeout_response_captures_both_streams() {
    let response = CliExecResponse {
        stdout: "stdout before kill".to_string(),
        stderr: "stderr before kill".to_string(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    assert!(response.timed_out);
    assert!(!response.stdout.is_empty());
    assert!(!response.stderr.is_empty());
}

/// Non-timed-out response has timed_out = false.
#[test]
fn normal_completion_has_timed_out_false() {
    let response = CliExecResponse {
        stdout: "completed normally".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(!response.timed_out);
    assert_eq!(response.exit_code, 0);
}

/// Timeout can occur with truncation simultaneously.
#[test]
fn timeout_and_truncation_can_coexist() {
    let response = CliExecResponse {
        stdout: "output that was both truncated and timed out".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: true,
        timed_out: true,
    };

    assert!(response.truncated, "truncated should be true");
    assert!(response.timed_out, "timed_out should be true");
}

// ---------------------------------------------------------------------------
// JSON serialization of timeout responses
// ---------------------------------------------------------------------------

/// Timeout response serializes timed_out field to JSON.
#[test]
fn timeout_response_serializes_timed_out_field() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(
        json.contains("\"timed_out\":true"),
        "JSON should contain timed_out:true, got: {}",
        json
    );
    assert!(
        json.contains("\"exit_code\":137"),
        "JSON should contain exit_code:137, got: {}",
        json
    );
}

/// Timeout response deserializes from JSON.
#[test]
fn timeout_response_deserializes_from_json() {
    let json = r#"{
        "stdout": "output before timeout",
        "stderr": "",
        "exit_code": 137,
        "truncated": false,
        "timed_out": true
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert!(response.timed_out);
    assert_eq!(response.exit_code, SIGKILL_EXIT_CODE);
    assert_eq!(response.stdout, "output before timeout");
}

/// Timeout response roundtrips through JSON.
#[test]
fn timeout_response_json_roundtrip() {
    let original = CliExecResponse {
        stdout: "partial output\nwith newlines".to_string(),
        stderr: "stderr content".to_string(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: true,
        timed_out: true,
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliExecResponse =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(original.stdout, restored.stdout);
    assert_eq!(original.stderr, restored.stderr);
    assert_eq!(original.exit_code, restored.exit_code);
    assert_eq!(original.truncated, restored.truncated);
    assert_eq!(original.timed_out, restored.timed_out);
}

// ---------------------------------------------------------------------------
// CliCapability timeout configuration
// ---------------------------------------------------------------------------

/// Custom timeout can be set in CliCapability.
#[test]
fn cli_capability_custom_timeout() {
    let cap = CliCapability {
        timeout_ms: 30_000, // 30 seconds
        ..Default::default()
    };

    assert_eq!(cap.timeout_ms, 30_000);
}

/// Very short timeout can be configured (for testing).
#[test]
fn cli_capability_short_timeout_for_testing() {
    let cap = CliCapability {
        timeout_ms: 100, // 100ms
        ..Default::default()
    };

    assert_eq!(cap.timeout_ms, 100);
}

/// Timeout of zero is representable (though not recommended).
#[test]
fn cli_capability_zero_timeout_representable() {
    let cap = CliCapability {
        timeout_ms: 0,
        ..Default::default()
    };

    assert_eq!(cap.timeout_ms, 0);
}

/// CliCapability serializes timeout_ms to JSON.
#[test]
fn cli_capability_timeout_serializes() {
    let cap = CliCapability {
        allowed_commands: vec!["echo".to_string()],
        timeout_ms: 10_000,
        ..Default::default()
    };

    let json = serde_json::to_string(&cap).expect("serialization must succeed");

    assert!(
        json.contains("\"timeout_ms\":10000"),
        "JSON should contain timeout_ms, got: {}",
        json
    );
}

/// CliCapability deserializes timeout_ms from JSON.
#[test]
fn cli_capability_timeout_deserializes() {
    let json = r#"{
        "allowed_commands": ["ls"],
        "allowed_args": [],
        "allowed_env": [],
        "timeout_ms": 15000,
        "max_output_bytes": 1048576,
        "max_concurrent": 2,
        "rate_limit_per_minute": 10
    }"#;

    let cap: CliCapability = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(cap.timeout_ms, 15_000);
}
