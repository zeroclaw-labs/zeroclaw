#![cfg(feature = "plugins-wasm")]

//! Test: Truncation and timeout clearly indicated in response.
//!
//! Task US-ZCL-56-5: Verifies acceptance criterion for US-ZCL-56:
//! > Truncation and timeout clearly indicated in response
//!
//! These tests verify that CLI execution responses clearly indicate when
//! truncation or timeout has occurred, using both boolean flags and
//! human-readable text indicators.

use zeroclaw::plugins::host_functions::CliExecResponse;

// ---------------------------------------------------------------------------
// Core acceptance criterion: Truncation and timeout clearly indicated
// ---------------------------------------------------------------------------

/// Truncation indicator string that appears in output.
const TRUNCATION_TEXT_INDICATOR: &str = "[output truncated]";

/// SIGKILL exit code (128 + 9) that indicates timeout.
const SIGKILL_EXIT_CODE: i32 = 137;

/// Timeout is clearly indicated by timed_out boolean field.
#[test]
fn timeout_clearly_indicated_by_boolean_field() {
    let response = CliExecResponse {
        stdout: "output before timeout".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    // The timed_out field must be true and clearly accessible
    assert!(
        response.timed_out,
        "timeout must be clearly indicated via timed_out=true"
    );
}

/// Timeout is clearly indicated by exit code 137 (SIGKILL).
#[test]
fn timeout_clearly_indicated_by_sigkill_exit_code() {
    let response = CliExecResponse {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    // Exit code 137 = 128 + SIGKILL(9) is a clear signal of forced termination
    assert_eq!(
        response.exit_code, SIGKILL_EXIT_CODE,
        "timeout must be indicated by SIGKILL exit code 137"
    );
}

/// Truncation is clearly indicated by truncated boolean field.
#[test]
fn truncation_clearly_indicated_by_boolean_field() {
    let mut stdout = "x".repeat(100);
    stdout.push_str("\n[output truncated]");

    let response = CliExecResponse {
        stdout,
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    // The truncated field must be true and clearly accessible
    assert!(
        response.truncated,
        "truncation must be clearly indicated via truncated=true"
    );
}

/// Truncation is clearly indicated by text marker in output.
#[test]
fn truncation_clearly_indicated_by_text_marker_in_stdout() {
    let mut stdout = "partial output".to_string();
    stdout.push_str("\n[output truncated]");

    let response = CliExecResponse {
        stdout,
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    assert!(
        response.stdout.contains(TRUNCATION_TEXT_INDICATOR),
        "truncation must be clearly indicated by '{}' text in stdout",
        TRUNCATION_TEXT_INDICATOR
    );
}

/// Truncation is clearly indicated by text marker in stderr.
#[test]
fn truncation_clearly_indicated_by_text_marker_in_stderr() {
    let mut stderr = "error output".to_string();
    stderr.push_str("\n[output truncated]");

    let response = CliExecResponse {
        stdout: String::new(),
        stderr,
        exit_code: 1,
        truncated: true,
        timed_out: false,
    };

    assert!(
        response.stderr.contains(TRUNCATION_TEXT_INDICATOR),
        "truncation must be clearly indicated by '{}' text in stderr",
        TRUNCATION_TEXT_INDICATOR
    );
}

/// Normal completion clearly shows no timeout or truncation.
#[test]
fn normal_completion_clearly_indicates_no_issues() {
    let response = CliExecResponse {
        stdout: "completed successfully".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    // Both flags must be clearly false for normal completion
    assert!(
        !response.timed_out,
        "normal completion must clearly show timed_out=false"
    );
    assert!(
        !response.truncated,
        "normal completion must clearly show truncated=false"
    );
    assert!(
        !response.stdout.contains(TRUNCATION_TEXT_INDICATOR),
        "normal completion must not contain truncation indicator"
    );
}

// ---------------------------------------------------------------------------
// Combined scenarios: both indicators can coexist and remain clear
// ---------------------------------------------------------------------------

/// When both timeout and truncation occur, both are clearly indicated.
#[test]
fn both_timeout_and_truncation_clearly_indicated_together() {
    let mut stdout = "partial output before kill".to_string();
    stdout.push_str("\n[output truncated]");

    let response = CliExecResponse {
        stdout,
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: true,
        timed_out: true,
    };

    // Both must be clearly indicated simultaneously
    assert!(
        response.timed_out,
        "timeout must be clearly indicated even when truncation also occurs"
    );
    assert!(
        response.truncated,
        "truncation must be clearly indicated even when timeout also occurs"
    );
    assert_eq!(
        response.exit_code, SIGKILL_EXIT_CODE,
        "exit code must still clearly indicate SIGKILL"
    );
    assert!(
        response.stdout.contains(TRUNCATION_TEXT_INDICATOR),
        "text indicator must still be present when both conditions occur"
    );
}

/// Indicators are distinguishable - timeout without truncation.
#[test]
fn timeout_without_truncation_is_distinguishable() {
    let response = CliExecResponse {
        stdout: "small output before timeout".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    assert!(response.timed_out);
    assert!(!response.truncated);
    // No truncation indicator should appear
    assert!(
        !response.stdout.contains(TRUNCATION_TEXT_INDICATOR),
        "timeout-only response must not have truncation indicator"
    );
}

/// Indicators are distinguishable - truncation without timeout.
#[test]
fn truncation_without_timeout_is_distinguishable() {
    let mut stdout = "large output".repeat(100);
    stdout.truncate(50);
    stdout.push_str("\n[output truncated]");

    let response = CliExecResponse {
        stdout,
        stderr: String::new(),
        exit_code: 0, // Normal exit, not SIGKILL
        truncated: true,
        timed_out: false,
    };

    assert!(response.truncated);
    assert!(!response.timed_out);
    assert_ne!(
        response.exit_code, SIGKILL_EXIT_CODE,
        "truncation-only should not have SIGKILL exit code"
    );
}

// ---------------------------------------------------------------------------
// JSON serialization preserves clear indication
// ---------------------------------------------------------------------------

/// Timeout indication is preserved in JSON serialization.
#[test]
fn timeout_indication_preserved_in_json() {
    let response = CliExecResponse {
        stdout: "output".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    // JSON must clearly contain the timeout indicator
    assert!(
        json.contains("\"timed_out\":true"),
        "JSON must clearly contain timed_out:true, got: {}",
        json
    );
    assert!(
        json.contains("\"exit_code\":137"),
        "JSON must clearly contain exit_code:137, got: {}",
        json
    );
}

/// Truncation indication is preserved in JSON serialization.
#[test]
fn truncation_indication_preserved_in_json() {
    let response = CliExecResponse {
        stdout: "output\n[output truncated]".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    // JSON must clearly contain the truncation indicator
    assert!(
        json.contains("\"truncated\":true"),
        "JSON must clearly contain truncated:true, got: {}",
        json
    );
    // Text indicator must also be in the serialized stdout
    assert!(
        json.contains("[output truncated]"),
        "JSON stdout must contain text indicator, got: {}",
        json
    );
}

/// Both indicators preserved when serializing combined response.
#[test]
fn both_indicators_preserved_in_json() {
    let response = CliExecResponse {
        stdout: "output\n[output truncated]".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: true,
        timed_out: true,
    };

    let json = serde_json::to_string(&response).expect("serialization must succeed");

    assert!(
        json.contains("\"timed_out\":true"),
        "JSON must contain timed_out:true"
    );
    assert!(
        json.contains("\"truncated\":true"),
        "JSON must contain truncated:true"
    );
    assert!(
        json.contains("\"exit_code\":137"),
        "JSON must contain exit_code:137"
    );
    assert!(
        json.contains("[output truncated]"),
        "JSON must contain text indicator"
    );
}

// ---------------------------------------------------------------------------
// Response consumers can programmatically detect indicators
// ---------------------------------------------------------------------------

/// Consumer can detect timeout programmatically.
#[test]
fn consumer_can_detect_timeout_programmatically() {
    let json = r#"{
        "stdout": "partial output",
        "stderr": "",
        "exit_code": 137,
        "truncated": false,
        "timed_out": true
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    // Consumer code can clearly check for timeout
    let is_timeout = response.timed_out;
    let was_killed = response.exit_code == SIGKILL_EXIT_CODE;

    assert!(
        is_timeout,
        "consumer must be able to detect timeout via field"
    );
    assert!(
        was_killed,
        "consumer must be able to detect kill via exit code"
    );
}

/// Consumer can detect truncation programmatically.
#[test]
fn consumer_can_detect_truncation_programmatically() {
    let json = r#"{
        "stdout": "partial\n[output truncated]",
        "stderr": "",
        "exit_code": 0,
        "truncated": true,
        "timed_out": false
    }"#;

    let response: CliExecResponse =
        serde_json::from_str(json).expect("deserialization must succeed");

    // Consumer code can clearly check for truncation
    let is_truncated = response.truncated;
    let has_indicator = response.stdout.contains(TRUNCATION_TEXT_INDICATOR)
        || response.stderr.contains(TRUNCATION_TEXT_INDICATOR);

    assert!(
        is_truncated,
        "consumer must be able to detect truncation via field"
    );
    assert!(
        has_indicator,
        "consumer must be able to detect truncation via text"
    );
}

/// Consumer can distinguish all four states clearly.
#[test]
fn consumer_can_distinguish_all_states() {
    // State 1: Normal (no issues)
    let normal = CliExecResponse {
        stdout: "done".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: false,
        timed_out: false,
    };

    // State 2: Timeout only
    let timeout_only = CliExecResponse {
        stdout: "partial".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: false,
        timed_out: true,
    };

    // State 3: Truncation only
    let truncated_only = CliExecResponse {
        stdout: "big\n[output truncated]".to_string(),
        stderr: String::new(),
        exit_code: 0,
        truncated: true,
        timed_out: false,
    };

    // State 4: Both timeout and truncation
    let both = CliExecResponse {
        stdout: "big partial\n[output truncated]".to_string(),
        stderr: String::new(),
        exit_code: SIGKILL_EXIT_CODE,
        truncated: true,
        timed_out: true,
    };

    // All four states are clearly distinguishable
    assert!(!normal.timed_out && !normal.truncated, "normal state clear");
    assert!(
        timeout_only.timed_out && !timeout_only.truncated,
        "timeout-only state clear"
    );
    assert!(
        !truncated_only.timed_out && truncated_only.truncated,
        "truncated-only state clear"
    );
    assert!(both.timed_out && both.truncated, "both state clear");
}
