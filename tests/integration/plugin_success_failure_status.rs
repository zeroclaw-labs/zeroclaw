#![cfg(feature = "plugins-wasm")]

//! Integration test: success and failure status is recorded in audit entries.
//!
//! Verifies acceptance criterion for US-ZCL-14:
//! > Success and failure status is recorded.
//!
//! Executes a plugin tool that succeeds and one that fails, then asserts the
//! audit log records `result.success = true` and `result.success = false`
//! respectively, with an error message present only on failure.

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
async fn successful_execution_records_success_true() {
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

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({"msg": "hello"}))
        .await
        .expect("execute should not return Err");

    assert!(result.success, "echo tool should succeed");

    let events = read_audit_events(tmp.path());
    assert!(!events.is_empty(), "audit log should have an entry");

    let entry = &events[0];
    assert_eq!(
        entry["result"]["success"].as_bool(),
        Some(true),
        "audit entry must record success = true for a successful execution"
    );
    assert!(
        entry["result"]["error"].is_null(),
        "audit entry should have no error field on success"
    );
}

#[tokio::test]
async fn failed_execution_records_success_false_with_error() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    // Minimal valid WASM with no exports — any call will fail
    let wasm_bytes: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // \0asm
        0x01, 0x00, 0x00, 0x00, // version 1
    ];
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let plugin = extism::Plugin::new(&manifest, [], true).expect("minimal wasm should load");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "broken_tool".into(),
        "always fails".into(),
        "broken_plugin".into(),
        "0.0.0".into(),
        "nonexistent".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({}))
        .await
        .expect("execute should not return Err even on failure");

    assert!(!result.success, "call to nonexistent export should fail");

    let events = read_audit_events(tmp.path());
    assert!(!events.is_empty(), "audit log should have an entry");

    let entry = &events[0];
    assert_eq!(
        entry["result"]["success"].as_bool(),
        Some(false),
        "audit entry must record success = false for a failed execution"
    );
    assert!(
        entry["result"]["error"].is_string(),
        "audit entry should include an error message on failure"
    );
}

#[tokio::test]
async fn mixed_executions_record_correct_status_per_entry() {
    let wasm_path = echo_wasm_path();
    assert!(
        wasm_path.is_file(),
        "echo_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    // --- Successful tool ---
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));
    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    let good_tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "echo_tool".into(),
        "echoes input".into(),
        "echo_plugin".into(),
        "0.1.0".into(),
        "tool_echo".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    // --- Failing tool ---
    let wasm_bytes: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let plugin = extism::Plugin::new(&manifest, [], true).expect("minimal wasm should load");
    let plugin = Arc::new(Mutex::new(plugin));

    let bad_tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "broken_tool".into(),
        "always fails".into(),
        "broken_plugin".into(),
        "0.0.0".into(),
        "nonexistent".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    use zeroclaw::tools::traits::Tool;

    // Execute: success, then failure
    let r1 = good_tool
        .execute(serde_json::json!({"msg": "ok"}))
        .await
        .expect("execute should not return Err");
    assert!(r1.success);

    let r2 = bad_tool
        .execute(serde_json::json!({}))
        .await
        .expect("execute should not return Err even on failure");
    assert!(!r2.success);

    let events = read_audit_events(tmp.path());
    assert_eq!(events.len(), 2, "should have exactly two audit entries");

    // First entry: success
    assert_eq!(
        events[0]["result"]["success"].as_bool(),
        Some(true),
        "first audit entry should record success"
    );
    assert!(
        events[0]["result"]["error"].is_null(),
        "successful entry should have no error"
    );

    // Second entry: failure
    assert_eq!(
        events[1]["result"]["success"].as_bool(),
        Some(false),
        "second audit entry should record failure"
    );
    assert!(
        events[1]["result"]["error"].is_string(),
        "failed entry should include an error message"
    );
}
