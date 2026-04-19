#![cfg(feature = "plugins-wasm")]

//! Test: Command and arguments logged (sensitive args redacted).
//!
//! Task US-ZCL-57-2: Verifies acceptance criterion for US-ZCL-57:
//! > Command and arguments logged (sensitive args redacted)
//!
//! These tests verify that when a plugin executes a CLI command via
//! `zeroclaw_cli_exec`, the audit log records the command name and
//! all arguments, with sensitive values (passwords, tokens, API keys)
//! automatically redacted.

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

/// Parse the redacted_input field from an audit event.
fn parse_redacted_input(event: &serde_json::Value) -> serde_json::Value {
    let redacted_str = event["action"]["redacted_input"]
        .as_str()
        .expect("redacted_input should be a string");
    serde_json::from_str(redacted_str).expect("redacted_input should be valid JSON")
}

// ---------------------------------------------------------------------------
// Core acceptance criterion: Command and arguments logged
// ---------------------------------------------------------------------------

/// AC: Command name is logged in the audit entry.
#[test]
fn cli_audit_entry_includes_command() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "command-test-plugin",
        "git",
        &["status".to_string(), "--short".to_string()],
        Some("/home/user/repo".to_string()),
        0,
        15,
        128,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let event = &events[0];
    assert_eq!(
        event["action"]["command"].as_str(),
        Some("git"),
        "command name must be recorded in action.command"
    );
}

/// AC: Arguments are logged in the redacted_input field.
#[test]
fn cli_audit_entry_includes_arguments() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "args-test-plugin",
        "npm",
        &[
            "install".to_string(),
            "--save-dev".to_string(),
            "typescript".to_string(),
        ],
        None,
        0,
        3000,
        512,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "should have exactly one audit entry");

    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    assert_eq!(args.len(), 3, "all three arguments should be logged");
    assert_eq!(args[0].as_str(), Some("install"));
    assert_eq!(args[1].as_str(), Some("--save-dev"));
    assert_eq!(args[2].as_str(), Some("typescript"));
}

// ---------------------------------------------------------------------------
// Sensitive argument redaction: --key=value patterns
// ---------------------------------------------------------------------------

/// AC: Password arguments with --password=value format are redacted.
#[test]
fn cli_audit_entry_redacts_password_flag() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "password-redact-plugin",
        "mysql",
        &[
            "-u".to_string(),
            "root".to_string(),
            "--password=supersecret123".to_string(),
            "mydb".to_string(),
        ],
        None,
        0,
        50,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    // The password value should be redacted, but the flag preserved
    assert_eq!(args[2].as_str(), Some("--password=[REDACTED]"));
    // Non-sensitive args should remain
    assert_eq!(args[0].as_str(), Some("-u"));
    assert_eq!(args[1].as_str(), Some("root"));
    assert_eq!(args[3].as_str(), Some("mydb"));
}

/// AC: Token arguments with --token=value format are redacted.
#[test]
fn cli_audit_entry_redacts_token_flag() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "token-redact-plugin",
        "gh",
        &[
            "auth".to_string(),
            "login".to_string(),
            "--token=ghp_abc123def456ghi789jkl".to_string(),
        ],
        None,
        0,
        100,
        64,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    assert_eq!(args[2].as_str(), Some("--token=[REDACTED]"));
}

/// AC: API key arguments with --api-key=value format are redacted.
#[test]
fn cli_audit_entry_redacts_api_key_flag() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "apikey-redact-plugin",
        "curl",
        &[
            "-X".to_string(),
            "POST".to_string(),
            "--api-key=sk-1234567890abcdef".to_string(),
            "https://api.example.com".to_string(),
        ],
        None,
        0,
        200,
        1024,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    assert_eq!(args[2].as_str(), Some("--api-key=[REDACTED]"));
    // Other args preserved
    assert_eq!(args[0].as_str(), Some("-X"));
    assert_eq!(args[1].as_str(), Some("POST"));
    assert_eq!(args[3].as_str(), Some("https://api.example.com"));
}

// ---------------------------------------------------------------------------
// Sensitive argument redaction: Environment variable assignments
// ---------------------------------------------------------------------------

/// AC: Environment variable assignments with sensitive names are redacted.
#[test]
fn cli_audit_entry_redacts_env_var_secrets() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "env-redact-plugin",
        "env",
        &[
            "HOME=/home/user".to_string(), // safe, should not be redacted
            "API_KEY=sk_live_abc123".to_string(),
            "PASSWORD=hunter2".to_string(),
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI".to_string(),
        ],
        None,
        0,
        10,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    // HOME is not sensitive, should be preserved
    assert_eq!(args[0].as_str(), Some("HOME=/home/user"));
    // Sensitive env vars should have values redacted
    assert_eq!(args[1].as_str(), Some("API_KEY=[REDACTED]"));
    assert_eq!(args[2].as_str(), Some("PASSWORD=[REDACTED]"));
    assert_eq!(args[3].as_str(), Some("AWS_SECRET_ACCESS_KEY=[REDACTED]"));
}

// ---------------------------------------------------------------------------
// Sensitive argument redaction: API key patterns in values
// ---------------------------------------------------------------------------

/// AC: Standalone API keys (like sk-..., ghp_...) are fully redacted.
#[test]
fn cli_audit_entry_redacts_standalone_api_keys() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "standalone-key-plugin",
        "somecommand",
        &[
            "sk-abc123def456ghi789jkl0".to_string(), // OpenAI-style key
            "ghp_abc123def456ghi789".to_string(),    // GitHub PAT
            "AKIAIOSFODNN7EXAMPLE".to_string(),      // AWS access key
            "normal-argument".to_string(),           // Safe
        ],
        None,
        0,
        5,
        0,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    // All API key patterns should be fully redacted
    assert_eq!(args[0].as_str(), Some("[REDACTED]"));
    assert_eq!(args[1].as_str(), Some("[REDACTED]"));
    assert_eq!(args[2].as_str(), Some("[REDACTED]"));
    // Normal argument preserved
    assert_eq!(args[3].as_str(), Some("normal-argument"));
}

/// AC: Bearer tokens in header values are redacted.
#[test]
fn cli_audit_entry_redacts_bearer_tokens() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "bearer-token-plugin",
        "curl",
        &[
            "-H".to_string(),
            "Authorization: Bearer sk_live_abc123def456".to_string(),
            "https://api.stripe.com/v1/charges".to_string(),
        ],
        None,
        0,
        300,
        2048,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    // Bearer token should be detected and redacted
    assert!(
        args[1].as_str().is_some_and(|s| s.contains("[REDACTED]")),
        "Bearer token header value should be redacted, got: {:?}",
        args[1]
    );
}

// ---------------------------------------------------------------------------
// Safe arguments are preserved
// ---------------------------------------------------------------------------

/// AC: Non-sensitive arguments are logged without modification.
#[test]
fn cli_audit_entry_preserves_safe_arguments() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "safe-args-plugin",
        "ls",
        &[
            "-la".to_string(),
            "--color=auto".to_string(),
            "/home/user/projects".to_string(),
            "file with spaces.txt".to_string(),
        ],
        Some("/tmp".to_string()),
        0,
        8,
        256,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    // All safe arguments should be preserved exactly
    assert_eq!(args[0].as_str(), Some("-la"));
    assert_eq!(args[1].as_str(), Some("--color=auto"));
    assert_eq!(args[2].as_str(), Some("/home/user/projects"));
    assert_eq!(args[3].as_str(), Some("file with spaces.txt"));
}

/// AC: Short flags without sensitive meaning are preserved.
#[test]
fn cli_audit_entry_preserves_safe_short_flags() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "short-flags-plugin",
        "git",
        &[
            "commit".to_string(),
            "-m".to_string(),
            "Add new feature".to_string(),
            "-a".to_string(),
        ],
        None,
        0,
        50,
        100,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    assert_eq!(args[0].as_str(), Some("commit"));
    assert_eq!(args[1].as_str(), Some("-m"));
    assert_eq!(args[2].as_str(), Some("Add new feature"));
    assert_eq!(args[3].as_str(), Some("-a"));
}

// ---------------------------------------------------------------------------
// Working directory is logged
// ---------------------------------------------------------------------------

/// AC: Working directory is included in the audit log.
#[test]
fn cli_audit_entry_includes_working_dir() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "workdir-test-plugin",
        "make",
        &["build".to_string()],
        Some("/home/user/myproject".to_string()),
        0,
        5000,
        10240,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);

    assert_eq!(
        redacted_input["working_dir"].as_str(),
        Some("/home/user/myproject"),
        "working_dir should be recorded in redacted_input"
    );
}

/// AC: Null working directory is handled gracefully.
#[test]
fn cli_audit_entry_handles_null_working_dir() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "null-workdir-plugin",
        "echo",
        &["hello".to_string()],
        None, // No working directory specified
        0,
        1,
        6,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);

    assert!(
        redacted_input["working_dir"].is_null(),
        "working_dir should be null when not specified"
    );
}

// ---------------------------------------------------------------------------
// Multiple commands with mixed sensitivity
// ---------------------------------------------------------------------------

/// AC: Multiple commands with mixed sensitive/safe args are logged correctly.
#[test]
fn cli_audit_entry_handles_mixed_commands() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    // First command: has sensitive args
    let entry1 = CliAuditEntry::new(
        "mixed-plugin",
        "docker",
        &[
            "login".to_string(),
            "--password=mysecretpassword".to_string(),
            "registry.example.com".to_string(),
        ],
        None,
        0,
        100,
        50,
        false,
        false,
    );

    // Second command: all safe args
    let entry2 = CliAuditEntry::new(
        "mixed-plugin",
        "docker",
        &[
            "build".to_string(),
            "-t".to_string(),
            "myapp:latest".to_string(),
            ".".to_string(),
        ],
        Some("/home/user/app".to_string()),
        0,
        30000,
        51200,
        false,
        false,
    );

    logger.log_cli(&entry1).expect("log_cli 1");
    logger.log_cli(&entry2).expect("log_cli 2");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 2, "should have two audit entries");

    // First command: password redacted
    let redacted1 = parse_redacted_input(&events[0]);
    let args1 = redacted1["args"].as_array().unwrap();
    assert_eq!(args1[0].as_str(), Some("login"));
    assert_eq!(args1[1].as_str(), Some("--password=[REDACTED]"));
    assert_eq!(args1[2].as_str(), Some("registry.example.com"));

    // Second command: all args preserved
    let redacted2 = parse_redacted_input(&events[1]);
    let args2 = redacted2["args"].as_array().unwrap();
    assert_eq!(args2[0].as_str(), Some("build"));
    assert_eq!(args2[1].as_str(), Some("-t"));
    assert_eq!(args2[2].as_str(), Some("myapp:latest"));
    assert_eq!(args2[3].as_str(), Some("."));
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// AC: Empty argument list is handled correctly.
#[test]
fn cli_audit_entry_handles_empty_args() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let entry = CliAuditEntry::new(
        "empty-args-plugin",
        "pwd",
        &[], // No arguments
        None,
        0,
        1,
        32,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    assert!(args.is_empty(), "empty args should result in empty array");
}

/// AC: Command with many arguments is logged completely.
#[test]
fn cli_audit_entry_handles_many_arguments() {
    let tmp = TempDir::new().expect("temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let many_args: Vec<String> = (0..50).map(|i| format!("arg{}", i)).collect();

    let entry = CliAuditEntry::new(
        "many-args-plugin",
        "somecommand",
        &many_args,
        None,
        0,
        500,
        1024,
        false,
        false,
    );

    logger.log_cli(&entry).expect("log_cli should succeed");

    let events = read_audit_events(tmp.path());
    let redacted_input = parse_redacted_input(&events[0]);
    let args = redacted_input["args"]
        .as_array()
        .expect("args should be an array");

    assert_eq!(args.len(), 50, "all 50 arguments should be logged");
    for (i, arg) in args.iter().enumerate() {
        assert_eq!(
            arg.as_str(),
            Some(format!("arg{}", i).as_str()),
            "argument {} should match",
            i
        );
    }
}
