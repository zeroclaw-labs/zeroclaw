#![cfg(feature = "plugins-wasm")]

//! Test: Exit code and duration logged.
//!
//! Task US-ZCL-57-3: Verifies acceptance criterion for US-ZCL-57:
//! > Exit code and duration logged
//!
//! These tests verify that when a plugin executes a CLI command via
//! `zeroclaw_cli_exec`, the audit log records both the exit code and
//! execution duration in milliseconds.

use std::sync::Arc;
use tempfile::TempDir;
use zeroclaw::config::AuditConfig;
use zeroclaw::security::audit::{AuditLogger, CliAuditEntry};

/// Create an AuditLogger writing to a temp directory.
fn make_audit_logger(dir: &std::path::Path) -> AuditLogger {
    let config = AuditConfig {
        enabled: true,
        log_path: "audit.jsonl".to_string(),
        max_size_mb: 10,
        sign_events: false,
    };
    AuditLogger::new(config, dir.to_path_buf()).expect("failed to create AuditLogger")
}

/// Read all audit events from the log file.
fn read_audit_events(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let log_path = dir.join("audit.jsonl");
    let contents = std::fs::read_to_string(&log_path).unwrap_or_default();
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("invalid JSON in audit log"))
        .collect()
}

// ---------------------------------------------------------------------------
// Core acceptance criterion: Exit code and duration logged
// ---------------------------------------------------------------------------

/// AC: Exit code is logged in the audit entry result field.
#[test]
fn cli_audit_entry_includes_exit_code() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "exit-code-test-plugin",
        "echo",
        &["hello".to_string()],
        None,
        0, // exit_code
        100,
        6,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["exit_code"].as_i64(),
        Some(0),
        "exit_code must be recorded in result.exit_code"
    );
}

/// AC: Duration in milliseconds is logged in the audit entry result field.
#[test]
fn cli_audit_entry_includes_duration_ms() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "duration-test-plugin",
        "sleep",
        &["0.1".to_string()],
        None,
        0,
        150, // duration_ms
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["duration_ms"].as_u64(),
        Some(150),
        "duration_ms must be recorded in result.duration_ms"
    );
}

/// AC: Non-zero exit codes are logged correctly.
#[test]
fn cli_audit_entry_logs_non_zero_exit_code() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "nonzero-exit-plugin",
        "false", // standard command that exits with code 1
        &[],
        None,
        1, // exit_code = 1
        25,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["exit_code"].as_i64(),
        Some(1),
        "non-zero exit_code must be recorded correctly"
    );
}

/// AC: High exit codes (e.g., 127 for command not found) are logged.
#[test]
fn cli_audit_entry_logs_high_exit_codes() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "high-exit-code-plugin",
        "nonexistent-command",
        &[],
        None,
        127, // typical "command not found" exit code
        10,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["exit_code"].as_i64(),
        Some(127),
        "exit_code 127 must be recorded for command not found"
    );
}

/// AC: Very short durations (sub-millisecond rounded to 0) are logged.
#[test]
fn cli_audit_entry_logs_zero_duration() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "zero-duration-plugin",
        "true", // instant command
        &[],
        None,
        0,
        0, // very fast, duration rounds to 0
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["duration_ms"].as_u64(),
        Some(0),
        "zero duration must be recorded correctly"
    );
}

/// AC: Long-running commands have their duration logged accurately.
#[test]
fn cli_audit_entry_logs_long_duration() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "long-duration-plugin",
        "make",
        &["build".to_string()],
        Some("/home/user/project".to_string()),
        0,
        300000, // 5 minutes in milliseconds
        10240,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["duration_ms"].as_u64(),
        Some(300000),
        "long duration (5 minutes) must be recorded correctly"
    );
}

/// AC: Exit code and duration are both logged together.
#[test]
fn cli_audit_entry_logs_exit_code_and_duration_together() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "combined-test-plugin",
        "git",
        &["status".to_string()],
        Some("/repo".to_string()),
        0,
        42, // 42ms
        256,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    let result = &event["result"];

    // Both fields must be present
    assert!(
        result["exit_code"].is_number(),
        "exit_code must be present in result"
    );
    assert!(
        result["duration_ms"].is_number(),
        "duration_ms must be present in result"
    );

    // And have correct values
    assert_eq!(result["exit_code"].as_i64(), Some(0));
    assert_eq!(result["duration_ms"].as_u64(), Some(42));
}

/// AC: Failed command logs both non-zero exit code and duration.
#[test]
fn cli_audit_entry_logs_failed_command_with_duration() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "failed-with-duration-plugin",
        "grep",
        &["nonexistent".to_string(), "file.txt".to_string()],
        None,
        2, // grep exits with 2 on error
        75,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["exit_code"].as_i64(),
        Some(2),
        "failed command exit code must be logged"
    );
    assert_eq!(
        event["result"]["duration_ms"].as_u64(),
        Some(75),
        "duration must still be logged for failed commands"
    );
}

/// AC: Timed-out command logs exit code and duration.
#[test]
fn cli_audit_entry_logs_timed_out_with_exit_and_duration() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "timeout-plugin",
        "sleep",
        &["3600".to_string()],
        None,
        -1,    // typical killed exit code
        30000, // 30 seconds timeout
        0,
        false,
        true, // timed_out = true
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["exit_code"].as_i64(),
        Some(-1),
        "timed-out command exit code must be logged"
    );
    assert_eq!(
        event["result"]["duration_ms"].as_u64(),
        Some(30000),
        "timeout duration must be logged"
    );
}

/// AC: Multiple executions each have independent exit codes and durations.
#[test]
fn cli_audit_entry_logs_multiple_executions_independently() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry1 = CliAuditEntry::new(
        "multi-plugin",
        "echo",
        &["first".to_string()],
        None,
        0,
        10,
        5,
        false,
        false,
    );
    let entry2 = CliAuditEntry::new("multi-plugin", "false", &[], None, 1, 20, 0, false, false);
    let entry3 = CliAuditEntry::new(
        "multi-plugin",
        "sleep",
        &["0.5".to_string()],
        None,
        0,
        500,
        0,
        false,
        false,
    );

    logger.log_cli(&entry1).expect("log_cli 1");
    logger.log_cli(&entry2).expect("log_cli 2");
    logger.log_cli(&entry3).expect("log_cli 3");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 3, "should have three audit entries");

    // First entry
    assert_eq!(events[0]["result"]["exit_code"].as_i64(), Some(0));
    assert_eq!(events[0]["result"]["duration_ms"].as_u64(), Some(10));

    // Second entry (failed)
    assert_eq!(events[1]["result"]["exit_code"].as_i64(), Some(1));
    assert_eq!(events[1]["result"]["duration_ms"].as_u64(), Some(20));

    // Third entry
    assert_eq!(events[2]["result"]["exit_code"].as_i64(), Some(0));
    assert_eq!(events[2]["result"]["duration_ms"].as_u64(), Some(500));
}

/// AC: Negative exit codes (from signals) are logged correctly.
#[test]
fn cli_audit_entry_logs_negative_exit_code() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "signal-plugin",
        "killed-process",
        &[],
        None,
        -9, // SIGKILL
        5000,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["result"]["exit_code"].as_i64(),
        Some(-9),
        "negative exit code (signal) must be logged"
    );
}

/// AC: Result field contains success indicator derived from exit code.
#[test]
fn cli_audit_entry_result_includes_success_flag() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    // Success case
    let success_entry = CliAuditEntry::new(
        "success-plugin",
        "echo",
        &["ok".to_string()],
        None,
        0,
        10,
        3,
        false,
        false,
    );

    // Failure case
    let failure_entry =
        CliAuditEntry::new("failure-plugin", "false", &[], None, 1, 5, 0, false, false);

    logger.log_cli(&success_entry).expect("log_cli success");
    logger.log_cli(&failure_entry).expect("log_cli failure");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 2, "should have two audit entries");

    // Success: exit_code=0 implies success=true
    assert_eq!(
        events[0]["result"]["success"].as_bool(),
        Some(true),
        "exit_code 0 should result in success=true"
    );

    // Failure: exit_code=1 implies success=false
    assert_eq!(
        events[1]["result"]["success"].as_bool(),
        Some(false),
        "non-zero exit_code should result in success=false"
    );
}
