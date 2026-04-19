#![cfg(feature = "plugins-wasm")]

//! Test: Exit code included in response.
//!
//! Task US-ZCL-55-6: Verifies acceptance criterion for US-ZCL-55:
//! > Exit code included in response
//!
//! These tests verify that the CliExecResponse struct includes an exit_code field
//! and that it is properly serialized/deserialized in JSON, ensuring the host
//! function response will include the command's exit code.

use zeroclaw::plugins::host_functions::{CliExecRequest, CliExecResponse};

// ---------------------------------------------------------------------------
// Core acceptance criterion: exit_code field exists and is typed correctly
// ---------------------------------------------------------------------------

/// AC: CliExecResponse includes exit_code field.
/// The response struct must contain the exit code returned by the command.
#[test]
fn response_struct_has_exit_code_field() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    // Verify exit_code field is accessible and has expected type
    let _code: i32 = response.exit_code;
    assert_eq!(response.exit_code, 0, "exit_code must be accessible");
}

/// AC: Exit code can represent success (0).
#[test]
fn exit_code_represents_success() {
    let response = CliExecResponse {
        stdout: "success output".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(response.exit_code, 0, "exit code 0 indicates success");
}

/// AC: Exit code can represent failure (non-zero).
#[test]
fn exit_code_represents_failure() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: "command not found".to_string(),
        exit_code: 127,
        truncated: false,
        timed_out: false,
    };

    assert_eq!(
        response.exit_code, 127,
        "exit code 127 indicates command not found"
    );
}

/// AC: Exit code can represent arbitrary error codes.
#[test]
fn exit_code_arbitrary_values() {
    // Common exit codes
    let codes = [0, 1, 2, 126, 127, 128, 130, 137, 255];

    for code in codes {
        let response = CliExecResponse {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: code,
            truncated: false,
            timed_out: false,
        };

        assert_eq!(
            response.exit_code, code,
            "exit_code must preserve value {}",
            code
        );
    }
}

/// AC: Exit code supports negative values (some systems use negative for signals).
#[test]
fn exit_code_negative_values() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: -9, // Killed by SIGKILL on some systems
        truncated: false,
        timed_out: false,
    };

    assert_eq!(
        response.exit_code, -9,
        "exit_code must support negative values"
    );
}

// ---------------------------------------------------------------------------
// JSON serialization/deserialization
// ---------------------------------------------------------------------------

/// AC: Exit code is included in JSON serialization.
#[test]
fn exit_code_serialized_to_json() {
    let response = CliExecResponse {
        stdout: "output".to_string(),
        stderr: String::new(),
        exit_code: 42,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(
        json.contains("\"exit_code\":42") || json.contains("\"exit_code\": 42"),
        "JSON must contain exit_code field: {}",
        json
    );
}

/// AC: Exit code is properly deserialized from JSON.
#[test]
fn exit_code_deserialized_from_json() {
    let json = r#"{
        "stdout": "hello",
        "stderr": "",
        "exit_code": 99,
        "truncated": false,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    assert_eq!(
        response.exit_code, 99,
        "exit_code must be deserialized correctly"
    );
}

/// AC: Exit code zero roundtrips through JSON.
#[test]
fn exit_code_zero_roundtrip() {
    let original = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliExecResponse =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(
        original.exit_code, restored.exit_code,
        "exit_code must roundtrip through JSON"
    );
}

/// AC: Exit code non-zero roundtrips through JSON.
#[test]
fn exit_code_nonzero_roundtrip() {
    let original = CliExecResponse {
        stdout: "partial output".to_string(),
        stderr: "error details".to_string(),
        exit_code: 1,
        truncated: true,
        timed_out: false,
    };

    let json = serde_json::to_string(&original).expect("serialization must succeed");
    let restored: CliExecResponse =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(
        original.exit_code, restored.exit_code,
        "non-zero exit_code must roundtrip through JSON"
    );
}

// ---------------------------------------------------------------------------
// Request struct validation (related context)
// ---------------------------------------------------------------------------

/// Context: CliExecRequest does not contain exit_code (it's in the response only).
#[test]
fn request_struct_does_not_have_exit_code() {
    // This test documents that exit_code is response-only.
    // If CliExecRequest had an exit_code field, this would fail to compile.
    let request = CliExecRequest {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        working_dir: None,
        env: None,
    };

    // Request should only have command, args, working_dir, env
    assert_eq!(request.command, "echo");
    assert_eq!(request.args, vec!["hello"]);
    assert!(request.working_dir.is_none());
    assert!(request.env.is_none());
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// AC: Exit code 255 (max unsigned byte) handled correctly.
#[test]
fn exit_code_max_byte_value() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 255,
        truncated: false,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");
    let restored: CliExecResponse =
        serde_json::from_str(&json).expect("deserialization must succeed");

    assert_eq!(
        restored.exit_code, 255,
        "exit_code 255 must roundtrip correctly"
    );
}

/// AC: Exit code combined with timeout flag.
/// When a command times out, exit_code might be set to a signal value.
#[test]
fn exit_code_with_timeout() {
    let response = CliExecResponse {
        stdout: "partial".to_string(),
        stderr: String::new(),
        exit_code: 137, // 128 + 9 (SIGKILL)
        truncated: false,
        timed_out: true,
    };

    assert_eq!(response.exit_code, 137);
    assert!(response.timed_out);
}

/// AC: Exit code combined with truncation flag.
/// Exit code should be valid even when output is truncated.
#[test]
fn exit_code_with_truncation() {
    let response = CliExecResponse {
        stdout: "truncated...".to_string(),
        stderr: String::new(),
        exit_code: 0, // Command succeeded, just output was truncated
        truncated: true,
        timed_out: false,
    };

    assert_eq!(response.exit_code, 0);
    assert!(response.truncated);
}
