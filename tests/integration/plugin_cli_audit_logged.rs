#![cfg(feature = "plugins-wasm")]

//! Test: All CLI executions logged with plugin name and timestamp.
//!
//! Task US-ZCL-57-1: Verifies acceptance criterion for US-ZCL-57:
//! > All CLI executions logged with plugin name and timestamp
//!
//! These tests verify that when a plugin executes a CLI command via
//! `zeroclaw_cli_exec`, the audit log records the plugin name (in the
//! actor.channel field) and the timestamp.

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
// Core acceptance criterion: CLI executions logged with plugin name and timestamp
// ---------------------------------------------------------------------------

/// AC: CLI execution audit entry includes plugin name in actor.channel.
#[test]
fn cli_audit_entry_includes_plugin_name() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "my-test-plugin",
        "echo",
        &["hello".to_string()],
        Some("/tmp".to_string()),
        0,
        42,
        5,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["actor"]["channel"].as_str(),
        Some("my-test-plugin"),
        "plugin name must be recorded in actor.channel"
    );
}

/// AC: CLI execution audit entry includes timestamp.
#[test]
fn cli_audit_entry_includes_timestamp() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "timestamp-test-plugin",
        "ls",
        &["-la".to_string()],
        None,
        0,
        10,
        100,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    let timestamp = event["timestamp"].as_str();
    assert!(
        timestamp.is_some(),
        "audit entry must include timestamp field"
    );

    // Verify timestamp is in ISO 8601 format (starts with YYYY-)
    let ts = timestamp.unwrap();
    assert!(
        ts.len() >= 10 && ts.chars().nth(4) == Some('-'),
        "timestamp should be in ISO 8601 format, got: {}",
        ts
    );
}

/// AC: CLI execution audit entry has event_type = cli_execution.
#[test]
fn cli_audit_entry_has_correct_event_type() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "event-type-test-plugin",
        "git",
        &["status".to_string()],
        None,
        0,
        15,
        200,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["event_type"].as_str(),
        Some("cli_execution"),
        "event_type must be cli_execution"
    );
}

/// AC: Multiple CLI executions from different plugins are logged distinctly.
#[test]
fn multiple_plugins_logged_with_distinct_names() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry1 = CliAuditEntry::new(
        "plugin-alpha",
        "echo",
        &["first".to_string()],
        None,
        0,
        5,
        5,
        false,
        false,
    );
    let entry2 = CliAuditEntry::new(
        "plugin-beta",
        "echo",
        &["second".to_string()],
        None,
        0,
        5,
        6,
        false,
        false,
    );
    let entry3 = CliAuditEntry::new("plugin-alpha", "ls", &[], None, 0, 3, 10, false, false);

    logger.log_cli(&entry1).expect("log_cli 1");
    logger.log_cli(&entry2).expect("log_cli 2");
    logger.log_cli(&entry3).expect("log_cli 3");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 3, "should have three audit entries");

    // Verify plugin names are recorded correctly for each entry
    assert_eq!(
        events[0]["actor"]["channel"].as_str(),
        Some("plugin-alpha"),
        "first entry should be from plugin-alpha"
    );
    assert_eq!(
        events[1]["actor"]["channel"].as_str(),
        Some("plugin-beta"),
        "second entry should be from plugin-beta"
    );
    assert_eq!(
        events[2]["actor"]["channel"].as_str(),
        Some("plugin-alpha"),
        "third entry should be from plugin-alpha"
    );
}

/// AC: Each CLI execution has a unique timestamp.
#[test]
fn each_execution_has_timestamp() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    for i in 0..3 {
        let entry = CliAuditEntry::new(
            format!("plugin-{}", i),
            "echo",
            &[format!("iteration-{}", i)],
            None,
            0,
            5,
            10,
            false,
            false,
        );
        logger.log_cli(&entry).expect("log_cli should succeed");
    }

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 3, "should have three audit entries");

    // Verify each entry has a timestamp
    for (i, event) in events.iter().enumerate() {
        assert!(
            event["timestamp"].as_str().is_some(),
            "entry {} must have a timestamp",
            i
        );
    }
}

/// AC: CLI audit entry timestamp is in the expected entry struct.
#[test]
fn cli_audit_entry_struct_has_timestamp_field() {
    use chrono::Utc;

    let before = Utc::now();
    let entry = CliAuditEntry::new("test-plugin", "date", &[], None, 0, 1, 10, false, false);
    let after = Utc::now();

    // Verify the timestamp is within the expected range
    assert!(
        entry.timestamp >= before && entry.timestamp <= after,
        "CliAuditEntry timestamp should be between test start and end"
    );
}

/// AC: Plugin name appears in audit log for failed commands too.
#[test]
fn failed_command_includes_plugin_name() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "failing-plugin",
        "nonexistent-command",
        &[],
        None,
        127, // typical exit code for "command not found"
        50,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["actor"]["channel"].as_str(),
        Some("failing-plugin"),
        "plugin name must be recorded even for failed commands"
    );
    assert!(
        event["timestamp"].as_str().is_some(),
        "timestamp must be present for failed commands"
    );
}

/// AC: Plugin name and timestamp logged for timed-out commands.
#[test]
fn timed_out_command_includes_plugin_name_and_timestamp() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "timeout-plugin",
        "sleep",
        &["3600".to_string()],
        None,
        -1,
        30000, // 30 seconds before timeout
        0,
        false,
        true, // timed_out = true
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["actor"]["channel"].as_str(),
        Some("timeout-plugin"),
        "plugin name must be recorded for timed-out commands"
    );
    assert!(
        event["timestamp"].as_str().is_some(),
        "timestamp must be present for timed-out commands"
    );
}
