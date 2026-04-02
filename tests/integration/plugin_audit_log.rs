//! Integration test: every plugin tool execution creates an audit log entry.
//!
//! Verifies acceptance criterion for US-ZCL-14:
//! > Every plugin tool execution creates an audit log entry.
//!
//! Uses the echo plugin to execute a tool via WasmTool (with an AuditLogger
//! attached), then reads the audit log file and asserts an entry was written
//! with event_type = "plugin_execution".

use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const ECHO_WASM: &str = "tests/plugins/artifacts/echo_plugin.wasm";

fn echo_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(ECHO_WASM)
}

/// Create an AuditLogger writing to a temp directory.
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

#[tokio::test]
async fn plugin_execution_creates_audit_log_entry() {
    let wasm_path = echo_wasm_path();
    assert!(
        wasm_path.is_file(),
        "echo_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    // Build Extism plugin
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));
    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    // Create WasmTool with audit logger
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

    // Execute
    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({"hello": "world"}))
        .await
        .expect("execute should not return Err");

    assert!(result.success, "echo tool should succeed");

    // Verify audit log entry was created
    let events = read_audit_events(tmp.path());
    assert!(
        !events.is_empty(),
        "audit log should contain at least one entry after plugin execution"
    );

    let entry = &events[0];
    assert_eq!(
        entry["event_type"].as_str(),
        Some("plugin_execution"),
        "audit entry event_type must be plugin_execution"
    );
}

#[tokio::test]
async fn failed_plugin_execution_also_creates_audit_entry() {
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

    // Even failed executions must produce an audit entry
    let events = read_audit_events(tmp.path());
    assert!(
        !events.is_empty(),
        "audit log should contain an entry even for failed plugin executions"
    );

    let entry = &events[0];
    assert_eq!(
        entry["event_type"].as_str(),
        Some("plugin_execution"),
        "audit entry event_type must be plugin_execution"
    );
    assert_eq!(
        entry["result"]["success"].as_bool(),
        Some(false),
        "audit entry should record failure"
    );
}

#[tokio::test]
async fn multiple_executions_create_multiple_audit_entries() {
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

    use zeroclaw::tools::traits::Tool;
    for i in 0..3 {
        let _ = tool
            .execute(serde_json::json!({"iteration": i}))
            .await
            .expect("execute should not return Err");
    }

    let events = read_audit_events(tmp.path());
    assert_eq!(
        events.len(),
        3,
        "each plugin execution should produce exactly one audit entry"
    );

    // All entries should be plugin_execution type
    for (i, entry) in events.iter().enumerate() {
        assert_eq!(
            entry["event_type"].as_str(),
            Some("plugin_execution"),
            "entry {} should be plugin_execution",
            i
        );
    }
}
