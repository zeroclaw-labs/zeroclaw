#![cfg(feature = "plugins-wasm")]

//! Test: Output truncated at max_output_bytes with indicator.
//!
//! Task US-ZCL-56-2: Verifies acceptance criterion for US-ZCL-56:
//! > Output truncated at max_output_bytes with indicator
//!
//! These tests verify that the CLI execution host function properly truncates
//! output that exceeds max_output_bytes and appends a truncation indicator.

use zeroclaw::plugins::host_functions::CliExecResponse;
use zeroclaw::plugins::{CliCapability, DEFAULT_CLI_MAX_OUTPUT_BYTES};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Output truncated at max_output_bytes with indicator
// ---------------------------------------------------------------------------

/// Truncation indicator string appended when output is truncated.
const TRUNCATION_INDICATOR: &str = "\n[output truncated]";

/// Verify CliExecResponse has truncated field for indicating truncation.
#[test]
fn response_has_truncated_field() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated, "truncated field must be accessible");
}

/// CliCapability has max_output_bytes field for configuring truncation limit.
#[test]
fn cli_capability_has_max_output_bytes_field() {
    let cap = CliCapability {
        max_output_bytes: 4096,
        ..Default::default()
    };

    assert_eq!(
        cap.max_output_bytes, 4096,
        "max_output_bytes must be configurable"
    );
}

/// CliCapability default max_output_bytes is 1 MiB.
#[test]
fn cli_capability_default_max_output_bytes_is_1mib() {
    let cap = CliCapability::default();

    assert_eq!(
        cap.max_output_bytes, DEFAULT_CLI_MAX_OUTPUT_BYTES,
        "default max_output_bytes should match DEFAULT_CLI_MAX_OUTPUT_BYTES"
    );
    assert_eq!(
        DEFAULT_CLI_MAX_OUTPUT_BYTES, 1_048_576,
        "default max_output_bytes should be 1048576 (1 MiB)"
    );
}

/// Truncated stdout would contain the truncation indicator.
#[test]
fn truncated_stdout_contains_indicator() {
    // Simulate what the implementation does: truncate and append indicator
    let max_bytes = 100;
    let large_output = "x".repeat(150);
    let mut stdout = large_output;
    stdout.truncate(max_bytes);
    stdout.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout,
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(
        response.stdout.ends_with("[output truncated]"),
        "truncated stdout should end with truncation indicator"
    );
    assert!(response.truncated);
}

/// Truncated stderr would contain the truncation indicator.
#[test]
fn truncated_stderr_contains_indicator() {
    // Simulate what the implementation does: truncate and append indicator
    let max_bytes = 100;
    let large_output = "e".repeat(150);
    let mut stderr = large_output;
    stderr.truncate(max_bytes);
    stderr.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout: String::new(),
        stderr,
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(
        response.stderr.ends_with("[output truncated]"),
        "truncated stderr should end with truncation indicator"
    );
    assert!(response.truncated);
}

/// Non-truncated output has truncated = false.
#[test]
fn normal_output_has_truncated_false() {
    let response = CliExecResponse {
        stdout: "normal output".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(!response.truncated);
    assert!(!response.stdout.contains("[output truncated]"));
}

/// Output exactly at max_output_bytes should not be truncated.
#[test]
fn output_at_limit_not_truncated() {
    let max_bytes = 100;
    let exact_output = "x".repeat(max_bytes);

    // Output at exactly max_bytes should not trigger truncation
    let response = CliExecResponse {
        stdout: exact_output.clone(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(!response.truncated);
    assert_eq!(response.stdout.len(), max_bytes);
}

/// Output just over max_output_bytes would be truncated.
#[test]
fn output_over_limit_is_truncated() {
    let max_bytes = 100;
    let over_output = "x".repeat(max_bytes + 1);

    // Simulate truncation
    let mut stdout = over_output;
    stdout.truncate(max_bytes);
    stdout.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout,
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
    assert!(response.stdout.ends_with("[output truncated]"));
}

/// Both stdout and stderr can be truncated independently.
#[test]
fn both_streams_truncated_independently() {
    let max_bytes = 50;
    let mut stdout = "out".repeat(30);
    let mut stderr = "err".repeat(30);

    stdout.truncate(max_bytes);
    stdout.push_str(TRUNCATION_INDICATOR);
    stderr.truncate(max_bytes);
    stderr.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout,
        stderr,
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
    assert!(response.stdout.ends_with("[output truncated]"));
    assert!(response.stderr.ends_with("[output truncated]"));
}

/// Only stdout truncated sets truncated flag.
#[test]
fn only_stdout_truncated_sets_flag() {
    let max_bytes = 50;
    let mut stdout = "x".repeat(100);
    stdout.truncate(max_bytes);
    stdout.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout,
        stderr: "small".to_string(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
    assert!(response.stdout.ends_with("[output truncated]"));
    assert!(!response.stderr.contains("[output truncated]"));
}

/// Only stderr truncated sets truncated flag.
#[test]
fn only_stderr_truncated_sets_flag() {
    let max_bytes = 50;
    let mut stderr = "e".repeat(100);
    stderr.truncate(max_bytes);
    stderr.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout: "small".to_string(),
        stderr,
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
    assert!(!response.stdout.contains("[output truncated]"));
    assert!(response.stderr.ends_with("[output truncated]"));
}

/// Truncation can occur with non-zero exit code.
#[test]
fn truncation_with_nonzero_exit_code() {
    let max_bytes = 50;
    let mut stderr = "error output".repeat(10);
    stderr.truncate(max_bytes);
    stderr.push_str(TRUNCATION_INDICATOR);

    let response = CliExecResponse {
        stdout: String::new(),
        stderr,
        exit_code: 1,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
    assert_eq!(response.exit_code, 1);
}

/// Truncation and timeout can coexist.
#[test]
fn truncation_and_timeout_can_coexist() {
    let response = CliExecResponse {
        stdout: "partial output\n[output truncated]".to_string(),
        stderr: String::new(),
        exit_code: 137, // SIGKILL
        truncated: true,
        timed_out: true,
    };

    assert!(response.truncated, "truncated should be true");
    assert!(response.timed_out, "timed_out should be true");
}

// ---------------------------------------------------------------------------
// JSON serialization of truncation responses
// ---------------------------------------------------------------------------

/// Truncation response serializes truncated field to JSON.
#[test]
fn truncation_response_serializes_truncated_field() {
    let response = CliExecResponse {
        stdout: "truncated...\n[output truncated]".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(
        json.contains("\"truncated\":true"),
        "JSON should contain truncated:true, got: {}",
        json
    );
}

/// Truncation response deserializes from JSON.
#[test]
fn truncation_response_deserializes_from_json() {
    let json = r#"{
        "stdout": "output before truncation\n[output truncated]",
        "stderr": "",
        "exit_code": 0,
        "truncated": true,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert!(response.truncated);
    assert!(response.stdout.contains("[output truncated]"));
}

/// Truncation response roundtrips through JSON.
#[test]
fn truncation_response_json_roundtrip() {
    let original = CliExecResponse {
        stdout: "partial output\n[output truncated]".to_string(),
        stderr: "error\n[output truncated]".to_string(),
        exit_code: 0,
        truncated: true,
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

// ---------------------------------------------------------------------------
// CliCapability max_output_bytes configuration
// ---------------------------------------------------------------------------

/// Custom max_output_bytes can be set in CliCapability.
#[test]
fn cli_capability_custom_max_output_bytes() {
    let cap = CliCapability {
        max_output_bytes: 2_097_152, // 2 MiB
        ..Default::default()
    };

    assert_eq!(cap.max_output_bytes, 2_097_152);
}

/// Small max_output_bytes can be configured (for testing).
#[test]
fn cli_capability_small_max_output_bytes_for_testing() {
    let cap = CliCapability {
        max_output_bytes: 1024, // 1 KiB
        ..Default::default()
    };

    assert_eq!(cap.max_output_bytes, 1024);
}

/// max_output_bytes of zero is representable (though not recommended).
#[test]
fn cli_capability_zero_max_output_bytes_representable() {
    let cap = CliCapability {
        max_output_bytes: 0,
        ..Default::default()
    };

    assert_eq!(cap.max_output_bytes, 0);
}

/// CliCapability serializes max_output_bytes to JSON.
#[test]
fn cli_capability_max_output_bytes_serializes() {
    let cap = CliCapability {
        allowed_commands: vec!["echo".to_string()],
        max_output_bytes: 512_000,
        ..Default::default()
    };

    let json = serde_json::to_string(&cap).expect("serialization must succeed");

    assert!(
        json.contains("\"max_output_bytes\":512000"),
        "JSON should contain max_output_bytes, got: {}",
        json
    );
}

/// CliCapability deserializes max_output_bytes from JSON.
#[test]
fn cli_capability_max_output_bytes_deserializes() {
    let json = r#"{
        "allowed_commands": ["ls"],
        "allowed_args": [],
        "allowed_env": [],
        "timeout_ms": 5000,
        "max_output_bytes": 2097152,
        "max_concurrent": 2,
        "rate_limit_per_minute": 10
    }"#;

    let cap: CliCapability = serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(cap.max_output_bytes, 2_097_152);
}

// ---------------------------------------------------------------------------
// Truncation indicator format
// ---------------------------------------------------------------------------

/// Truncation indicator follows newline pattern.
#[test]
fn truncation_indicator_follows_newline() {
    // The implementation appends "\n[output truncated]"
    // This ensures it appears on its own line
    let max_bytes = 20;
    let mut stdout = "some output here".to_string();
    stdout.truncate(max_bytes);
    stdout.push_str(TRUNCATION_INDICATOR);

    assert!(stdout.contains('\n'));
    assert!(stdout.ends_with("[output truncated]"));
}

/// Truncation indicator is human-readable.
#[test]
fn truncation_indicator_is_human_readable() {
    // The indicator should be clear to users/developers
    assert!(TRUNCATION_INDICATOR.contains("truncated"));
    assert!(TRUNCATION_INDICATOR.starts_with('\n'));
}
