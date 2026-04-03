#![cfg(feature = "plugins-wasm")]

//! Integration test: input parameters are redacted for sensitive values.
//!
//! Verifies acceptance criterion for US-ZCL-14:
//! > Input parameters are redacted for sensitive values.
//!
//! Executes a plugin tool with sensitive input parameters (e.g. api_key, password)
//! and asserts that the raw values do NOT appear in the audit log, while redacted
//! forms do.

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

#[tokio::test]
async fn sensitive_input_params_are_redacted_in_audit_log() {
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
    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "echo_tool".into(),
        "echoes input".into(),
        "echo_plugin".into(),
        "0.1.0".into(),
        "tool_echo".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    let secret_api_key = "sk-live-super-secret-key-abc123";
    let secret_password = "hunter2-very-long-password";
    let safe_value = "us-east-1";

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({
            "api_key": secret_api_key,
            "password": secret_password,
            "region": safe_value,
        }))
        .await
        .expect("execute should not return Err");

    assert!(result.success, "echo tool should succeed");

    let events = read_audit_events(tmp.path());
    assert!(
        !events.is_empty(),
        "audit log should contain at least one entry"
    );

    let entry = &events[0];

    // The redacted_input field must be present
    let redacted_input_str = entry["action"]["redacted_input"]
        .as_str()
        .expect("action.redacted_input must be present for plugin executions");

    // Raw sensitive values must NOT appear in the redacted input
    assert!(
        !redacted_input_str.contains(secret_api_key),
        "redacted_input must not contain raw api_key value, got: {redacted_input_str}"
    );
    assert!(
        !redacted_input_str.contains(secret_password),
        "redacted_input must not contain raw password value, got: {redacted_input_str}"
    );

    // Redacted forms should appear
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

    // Non-sensitive values should appear as-is
    assert!(
        redacted_input_str.contains(safe_value),
        "redacted_input should contain non-sensitive value '{safe_value}', got: {redacted_input_str}"
    );
}

#[tokio::test]
async fn raw_sensitive_values_absent_from_full_audit_line() {
    let wasm_path = echo_wasm_path();
    if !wasm_path.is_file() {
        eprintln!("skipping: echo_plugin.wasm not found");
        return;
    }

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));
    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "echo_tool".into(),
        "echoes input".into(),
        "echo_plugin".into(),
        "0.1.0".into(),
        "tool_echo".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    let secret_token = "ghp_1234567890abcdefXYZ";

    use zeroclaw::tools::traits::Tool;
    let _ = tool
        .execute(serde_json::json!({
            "auth_token": secret_token,
            "query": "SELECT 1",
        }))
        .await
        .expect("execute should not return Err");

    // Read the raw audit log file — the raw secret must not appear anywhere in the line
    let log_path = tmp.path().join("audit.jsonl");
    let raw_log = std::fs::read_to_string(&log_path).expect("should read audit log");
    assert!(
        !raw_log.contains(secret_token),
        "raw audit log must not contain the full secret token anywhere"
    );
}

/// Unit-level check: `redact_sensitive_params` redacts known sensitive keys.
#[test]
fn redact_sensitive_params_handles_known_keys() {
    use zeroclaw::security::{redact, redact_sensitive_params};

    let input = serde_json::json!({
        "api_key": "sk-live-1234567890",
        "password": "hunter2hunter2",
        "secret": "my-deep-secret-val",
        "token": "tok-abcdefghijk",
        "region": "eu-west-1",
        "count": 42,
    });

    let redacted = redact_sensitive_params(&input);
    let obj = redacted.as_object().expect("should be object");

    // Sensitive keys are redacted
    assert_eq!(
        obj["api_key"].as_str().unwrap(),
        redact("sk-live-1234567890")
    );
    assert_eq!(obj["password"].as_str().unwrap(), redact("hunter2hunter2"));
    assert_eq!(
        obj["secret"].as_str().unwrap(),
        redact("my-deep-secret-val")
    );
    assert_eq!(obj["token"].as_str().unwrap(), redact("tok-abcdefghijk"));

    // Non-sensitive keys are untouched
    assert_eq!(obj["region"].as_str().unwrap(), "eu-west-1");
    assert_eq!(obj["count"].as_u64().unwrap(), 42);
}
