//! Integration test: audit log entries on plugin calls contain correct fields and redact sensitive input.
//!
//! Verifies task US-ZCL-14-8:
//! > Call a WasmTool with an AuditLogger configured, verify an audit event was logged
//! > with correct plugin name, tool name, duration, and success status. Verify sensitive
//! > input values are redacted in the log entry.

use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const ECHO_WASM: &str = "tests/plugins/artifacts/echo_plugin.wasm";

fn echo_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(ECHO_WASM)
}

fn make_audit_logger(dir: &std::path::Path) -> zeroclaw::security::audit::AuditLogger {
    let config = zeroclaw::config::AuditConfig {
        enabled: true,
        log_path: "audit.jsonl".to_string(),
        max_size_mb: 10,
        sign_events: false,
    };
    zeroclaw::security::audit::AuditLogger::new(config, dir.to_path_buf())
        .expect("failed to create AuditLogger")
}

fn read_audit_events(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let log_path = dir.join("audit.jsonl");
    let contents = std::fs::read_to_string(&log_path).unwrap_or_default();
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("invalid JSON in audit log"))
        .collect()
}

/// Successful plugin execution with sensitive input: verify all audit fields and redaction.
#[tokio::test]
async fn successful_call_logs_correct_fields_and_redacts_sensitive_input() {
    let wasm_path = echo_wasm_path();
    assert!(
        wasm_path.is_file(),
        "echo_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));
    let plugin = extism::Plugin::new(&manifest, [], true)
        .expect("failed to instantiate echo plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "echo_tool".into(),
        "echoes input".into(),
        "echo_plugin".into(),
        "3.1.0".into(),
        "tool_echo".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    let secret_api_key = "sk-live-very-secret-key-9999";
    let secret_password = "p4ssw0rd-ultra-secure-long";
    let safe_region = "ap-southeast-2";

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({
            "api_key": secret_api_key,
            "password": secret_password,
            "region": safe_region,
        }))
        .await
        .expect("execute should not return Err");

    assert!(result.success, "echo tool should succeed");

    // --- Verify audit log entry ---
    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "exactly one audit entry expected");

    let entry = &events[0];

    // Event type
    assert_eq!(
        entry["event_type"].as_str(),
        Some("plugin_execution"),
        "event_type must be plugin_execution"
    );

    // Plugin name, version, tool name in action.command
    let command = entry["action"]["command"]
        .as_str()
        .expect("action.command must be a string");
    assert_eq!(
        command, "echo_plugin@3.1.0:echo_tool",
        "action.command must have format plugin@version:tool"
    );

    // Duration
    let duration = entry["result"]["duration_ms"]
        .as_u64()
        .expect("result.duration_ms must be present");
    assert!(
        duration < 30_000,
        "duration_ms should be reasonable, got: {duration}"
    );

    // Success status
    assert_eq!(
        entry["result"]["success"].as_bool(),
        Some(true),
        "result.success must be true for successful execution"
    );

    // --- Verify sensitive input redaction ---
    let redacted_input_str = entry["action"]["redacted_input"]
        .as_str()
        .expect("action.redacted_input must be present");

    // Raw secrets must NOT appear
    assert!(
        !redacted_input_str.contains(secret_api_key),
        "redacted_input must not contain raw api_key"
    );
    assert!(
        !redacted_input_str.contains(secret_password),
        "redacted_input must not contain raw password"
    );

    // Redacted forms must appear
    let redacted_key = zeroclaw::security::redact(secret_api_key);
    let redacted_pass = zeroclaw::security::redact(secret_password);
    assert!(
        redacted_input_str.contains(&redacted_key),
        "redacted_input should contain redacted api_key '{redacted_key}', got: {redacted_input_str}"
    );
    assert!(
        redacted_input_str.contains(&redacted_pass),
        "redacted_input should contain redacted password '{redacted_pass}', got: {redacted_input_str}"
    );

    // Non-sensitive values preserved
    assert!(
        redacted_input_str.contains(safe_region),
        "redacted_input should contain non-sensitive value '{safe_region}'"
    );

    // Raw secrets must not appear anywhere in the full log line
    let raw_log = std::fs::read_to_string(tmp.path().join("audit.jsonl"))
        .expect("should read audit log");
    assert!(
        !raw_log.contains(secret_api_key),
        "raw audit log must not contain full api_key"
    );
    assert!(
        !raw_log.contains(secret_password),
        "raw audit log must not contain full password"
    );
}

/// Failed plugin execution: verify audit entry records failure status and still has all fields.
#[tokio::test]
async fn failed_call_logs_failure_status_and_correct_fields() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    // Minimal WASM with no exports — any call will fail
    let wasm_bytes: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // \0asm
        0x01, 0x00, 0x00, 0x00, // version 1
    ];
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let plugin = extism::Plugin::new(&manifest, [], true).expect("minimal wasm should load");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "fail_tool".into(),
        "always fails".into(),
        "fail_plugin".into(),
        "0.0.1".into(),
        "missing_export".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    let secret_token = "ghp_abcdef1234567890XYZ";

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({
            "token": secret_token,
            "action": "deploy",
        }))
        .await
        .expect("execute should not return Err even on failure");

    assert!(!result.success, "call to nonexistent export should fail");

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 1, "exactly one audit entry expected");

    let entry = &events[0];

    // Event type
    assert_eq!(
        entry["event_type"].as_str(),
        Some("plugin_execution"),
    );

    // Plugin name, version, tool name
    assert_eq!(
        entry["action"]["command"].as_str(),
        Some("fail_plugin@0.0.1:fail_tool"),
    );

    // Failure status
    assert_eq!(
        entry["result"]["success"].as_bool(),
        Some(false),
        "result.success must be false for failed execution"
    );

    // Duration still present
    assert!(
        entry["result"]["duration_ms"].as_u64().is_some(),
        "duration_ms must be present even for failures"
    );

    // Error message present
    assert!(
        entry["result"]["error"].as_str().is_some(),
        "result.error should be present for failed execution"
    );

    // Sensitive input redacted
    let redacted_input_str = entry["action"]["redacted_input"]
        .as_str()
        .expect("action.redacted_input must be present");
    assert!(
        !redacted_input_str.contains(secret_token),
        "redacted_input must not contain raw token"
    );
    let redacted_tok = zeroclaw::security::redact(secret_token);
    assert!(
        redacted_input_str.contains(&redacted_tok),
        "redacted_input should contain redacted token '{redacted_tok}'"
    );

    // Non-sensitive value preserved
    assert!(
        redacted_input_str.contains("deploy"),
        "non-sensitive 'action' field should be preserved"
    );

    // Full log line must not contain raw secret
    let raw_log = std::fs::read_to_string(tmp.path().join("audit.jsonl"))
        .expect("should read audit log");
    assert!(
        !raw_log.contains(secret_token),
        "raw audit log must not contain full token"
    );
}
