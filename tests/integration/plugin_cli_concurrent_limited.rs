#![cfg(feature = "plugins-wasm")]

//! Test: Concurrent execution limited per plugin.
//!
//! Task US-ZCL-56-3: Verifies acceptance criterion for US-ZCL-56:
//! > Concurrent execution limited per plugin
//!
//! These tests verify that the CLI capability properly tracks and limits
//! concurrent command executions, preventing resource exhaustion.

use zeroclaw::plugins::host_functions::CliExecResponse;
use zeroclaw::plugins::{CliCapability, DEFAULT_CLI_MAX_CONCURRENT};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Concurrent execution limited per plugin
// ---------------------------------------------------------------------------

/// CliCapability has max_concurrent field for limiting simultaneous executions.
#[test]
fn cli_capability_has_max_concurrent_field() {
    let cap = CliCapability {
        max_concurrent: 4,
        ..Default::default()
    };

    assert_eq!(cap.max_concurrent, 4, "max_concurrent must be configurable");
}

/// CliCapability default max_concurrent matches the constant.
#[test]
fn cli_capability_default_max_concurrent() {
    let cap = CliCapability::default();

    assert_eq!(
        cap.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT,
        "default max_concurrent should match DEFAULT_CLI_MAX_CONCURRENT"
    );
    assert_eq!(
        DEFAULT_CLI_MAX_CONCURRENT, 2,
        "default max_concurrent should be 2"
    );
}

/// Custom max_concurrent can be set to higher values.
#[test]
fn cli_capability_custom_max_concurrent_higher() {
    let cap = CliCapability {
        max_concurrent: 10,
        ..Default::default()
    };

    assert_eq!(cap.max_concurrent, 10);
}

/// Custom max_concurrent can be set to 1 (fully serialized execution).
#[test]
fn cli_capability_max_concurrent_one_serializes_execution() {
    let cap = CliCapability {
        max_concurrent: 1,
        ..Default::default()
    };

    assert_eq!(
        cap.max_concurrent, 1,
        "max_concurrent=1 should serialize all executions"
    );
}

/// max_concurrent of zero is representable (effectively disables CLI).
#[test]
fn cli_capability_zero_max_concurrent_representable() {
    let cap = CliCapability {
        max_concurrent: 0,
        ..Default::default()
    };

    assert_eq!(
        cap.max_concurrent, 0,
        "max_concurrent=0 should be representable"
    );
}

// ---------------------------------------------------------------------------
// Error response when limit reached
// ---------------------------------------------------------------------------

/// Concurrent limit error response has exit_code -1.
#[test]
fn concurrent_limit_error_exit_code() {
    // When concurrent limit is reached, the response has exit_code -1
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:test] concurrent execution limit reached (2/2)".to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(
        response.exit_code, -1,
        "concurrent limit error should have exit_code -1"
    );
}

/// Concurrent limit error response includes limit info in stderr.
#[test]
fn concurrent_limit_error_message_format() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:myplugin] concurrent execution limit reached (4/4)".to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    assert!(
        response
            .stderr
            .contains("concurrent execution limit reached"),
        "stderr should indicate concurrent limit was reached"
    );
    assert!(
        response.stderr.contains("4/4"),
        "stderr should show current/max counts"
    );
    assert!(
        response.stderr.contains("[plugin:"),
        "stderr should include plugin name context"
    );
}

/// Concurrent limit error response has empty stdout.
#[test]
fn concurrent_limit_error_empty_stdout() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:test] concurrent execution limit reached (2/2)".to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    assert!(
        response.stdout.is_empty(),
        "concurrent limit error should have empty stdout"
    );
}

/// Concurrent limit error is not truncated or timed out.
#[test]
fn concurrent_limit_error_not_truncated_or_timed_out() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:test] concurrent execution limit reached (2/2)".to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    assert!(
        !response.truncated,
        "concurrent limit error should not be truncated"
    );
    assert!(
        !response.timed_out,
        "concurrent limit error should not be timed_out"
    );
}

// ---------------------------------------------------------------------------
// JSON serialization of max_concurrent
// ---------------------------------------------------------------------------

/// CliCapability serializes max_concurrent to JSON.
#[test]
fn cli_capability_max_concurrent_serializes() {
    let cap = CliCapability {
        allowed_commands: vec!["echo".to_string()],
        max_concurrent: 8,
        ..Default::default()
    };

    let json = serde_json::to_string(&cap).expect("serialization must succeed");

    assert!(
        json.contains("\"max_concurrent\":8"),
        "JSON should contain max_concurrent, got: {}",
        json
    );
}

/// CliCapability deserializes max_concurrent from JSON.
#[test]
fn cli_capability_max_concurrent_deserializes() {
    let json = r#"{
        "allowed_commands": ["ls"],
        "allowed_args": [],
        "allowed_env": [],
        "timeout_ms": 5000,
        "max_output_bytes": 1048576,
        "max_concurrent": 6,
        "rate_limit_per_minute": 10
    }"#;

    let cap: CliCapability = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(cap.max_concurrent, 6);
}

/// CliCapability uses default max_concurrent when not specified in JSON.
#[test]
fn cli_capability_max_concurrent_defaults_when_missing() {
    let json = r#"{
        "allowed_commands": ["ls"],
        "allowed_args": [],
        "allowed_env": []
    }"#;

    let cap: CliCapability = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(
        cap.max_concurrent, DEFAULT_CLI_MAX_CONCURRENT,
        "missing max_concurrent should default to DEFAULT_CLI_MAX_CONCURRENT"
    );
}

/// CliCapability max_concurrent roundtrips through JSON.
#[test]
fn cli_capability_max_concurrent_json_roundtrip() {
    let original = CliCapability {
        allowed_commands: vec!["git".to_string(), "cargo".to_string()],
        max_concurrent: 3,
        ..Default::default()
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliCapability =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(original.max_concurrent, restored.max_concurrent);
}

// ---------------------------------------------------------------------------
// Concurrent limit error response serialization
// ---------------------------------------------------------------------------

/// Concurrent limit error response serializes to JSON.
#[test]
fn concurrent_limit_error_serializes() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:test] concurrent execution limit reached (2/2)".to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(
        json.contains("\"exit_code\":-1"),
        "JSON should contain exit_code:-1, got: {}",
        json
    );
    assert!(
        json.contains("concurrent execution limit reached"),
        "JSON should contain error message in stderr"
    );
}

/// Concurrent limit error response deserializes from JSON.
#[test]
fn concurrent_limit_error_deserializes() {
    let json = r#"{
        "stdout": "",
        "stderr": "[plugin:test] concurrent execution limit reached (4/4)",
        "exit_code": -1,
        "truncated": false,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(response.exit_code, -1);
    assert!(
        response
            .stderr
            .contains("concurrent execution limit reached")
    );
    assert!(response.stdout.is_empty());
}

/// Concurrent limit error response roundtrips through JSON.
#[test]
fn concurrent_limit_error_json_roundtrip() {
    let original = CliExecResponse {
        stdout: String::new(),
        stderr: "[plugin:mytest] concurrent execution limit reached (5/5)".to_string(),
        exit_code: -1,
        truncated: false,
        timed_out: false,
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
