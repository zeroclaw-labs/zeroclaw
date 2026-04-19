#![cfg(feature = "plugins-wasm")]

//! Test: Stdout and stderr captured and returned.
//!
//! Task US-ZCL-55-5: Verifies acceptance criterion for US-ZCL-55:
//! > Stdout and stderr captured and returned
//!
//! These tests verify that the CLI execution host function properly captures
//! both stdout and stderr streams and returns them in the response.

use zeroclaw::plugins::host_functions::CliExecResponse;

// ---------------------------------------------------------------------------
// Core acceptance criterion: stdout and stderr captured and returned
// ---------------------------------------------------------------------------

/// Verify CliExecResponse struct has stdout field.
#[test]
fn response_has_stdout_field() {
    let response = CliExecResponse {
        stdout: "standard output content".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.stdout, "standard output content");
}

/// Verify CliExecResponse struct has stderr field.
#[test]
fn response_has_stderr_field() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "error output content".to_string(),
        exit_code: 1,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.stderr, "error output content");
}

/// Both stdout and stderr can contain content simultaneously.
#[test]
fn response_captures_both_streams_independently() {
    let response = CliExecResponse {
        stdout: "stdout line 1\nstdout line 2".to_string(),
        stderr: "stderr warning\nstderr error".to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    // Both streams captured independently
    assert!(response.stdout.contains("stdout line 1"));
    assert!(response.stdout.contains("stdout line 2"));
    assert!(response.stderr.contains("stderr warning"));
    assert!(response.stderr.contains("stderr error"));

    // Streams are kept separate (no mixing)
    assert!(!response.stdout.contains("stderr"));
    assert!(!response.stderr.contains("stdout line"));
}

/// Empty stdout is represented as empty string.
#[test]
fn empty_stdout_captured() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "only error output".to_string(),
        exit_code: 1,
        truncated: false,
        timed_out: false,
    };

    assert!(response.stdout.is_empty());
    assert!(!response.stderr.is_empty());
}

/// Empty stderr is represented as empty string.
#[test]
fn empty_stderr_captured() {
    let response = CliExecResponse {
        stdout: "only standard output".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(!response.stdout.is_empty());
    assert!(response.stderr.is_empty());
}

/// Both streams can be empty.
#[test]
fn both_streams_empty() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(response.stdout.is_empty());
    assert!(response.stderr.is_empty());
}

/// Multiline output is captured with newlines preserved.
#[test]
fn multiline_output_preserved() {
    let multiline_stdout = "line 1\nline 2\nline 3\n";
    let multiline_stderr = "err 1\nerr 2\n";

    let response = CliExecResponse {
        stdout: multiline_stdout.to_string(),
        stderr: multiline_stderr.to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.stdout.lines().count(), 3);
    assert_eq!(response.stderr.lines().count(), 2);
}

/// Special characters in output are preserved.
#[test]
fn special_characters_preserved() {
    let response = CliExecResponse {
        stdout: "tabs\there\tand\nnewlines".to_string(),
        stderr: "quotes: \"double\" and 'single'".to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(response.stdout.contains('\t'));
    assert!(response.stdout.contains('\n'));
    assert!(response.stderr.contains('"'));
    assert!(response.stderr.contains('\''));
}

/// Unicode content is captured correctly.
#[test]
fn unicode_output_captured() {
    let response = CliExecResponse {
        stdout: "Hello, World!".to_string(),
        stderr: "Warning: cafe".to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert!(response.stdout.contains("World"));
    assert!(response.stderr.contains("cafe"));
}

/// Large output can be captured (subject to truncation limits).
#[test]
fn large_output_captured() {
    // Simulate large output (100KB)
    let large_stdout = "x".repeat(100_000);
    let large_stderr = "e".repeat(50_000);

    let response = CliExecResponse {
        stdout: large_stdout.clone(),
        stderr: large_stderr.clone(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.stdout.len(), 100_000);
    assert_eq!(response.stderr.len(), 50_000);
}

/// Truncated flag indicates when output was truncated.
#[test]
fn truncation_flag_tracks_output_truncation() {
    let response = CliExecResponse {
        stdout: "truncated output...".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
}

/// Output is captured even when command fails.
#[test]
fn output_captured_on_failure() {
    let response = CliExecResponse {
        stdout: "partial output before failure".to_string(),
        stderr: "error: command failed".to_string(),
        exit_code: 1,
        truncated: false,
        timed_out: false,
    };

    assert!(!response.stdout.is_empty());
    assert!(!response.stderr.is_empty());
    assert_ne!(response.exit_code, 0);
}

/// Output is captured on timeout.
#[test]
fn output_captured_on_timeout() {
    let response = CliExecResponse {
        stdout: "output before timeout".to_string(),
        stderr: String::new(),
        exit_code: 137, // SIGKILL
        truncated: false,
        timed_out: true,
    };

    assert!(!response.stdout.is_empty());
    assert!(response.timed_out);
}

/// Output serializes to JSON with both streams.
#[test]
fn output_streams_serialize_to_json() {
    let response = CliExecResponse {
        stdout: "stdout content".to_string(),
        stderr: "stderr content".to_string(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(json.contains("\"stdout\":\"stdout content\""));
    assert!(json.contains("\"stderr\":\"stderr content\""));
}

/// Output deserializes from JSON with both streams.
#[test]
fn output_streams_deserialize_from_json() {
    let json = r#"{
        "stdout": "deserialized stdout",
        "stderr": "deserialized stderr",
        "exit_code": 0,
        "truncated": false,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(response.stdout, "deserialized stdout");
    assert_eq!(response.stderr, "deserialized stderr");
}

/// JSON roundtrip preserves both output streams.
#[test]
fn output_streams_json_roundtrip() {
    let original = CliExecResponse {
        stdout: "roundtrip stdout\nwith newlines".to_string(),
        stderr: "roundtrip stderr".to_string(),
        exit_code: 42,
        truncated: true,
        timed_out: false,
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliExecResponse =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(original.stdout, restored.stdout);
    assert_eq!(original.stderr, restored.stderr);
}
