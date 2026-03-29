//! Integration test: audit entry includes plugin name, version, tool name, and duration.
//!
//! Verifies acceptance criterion for US-ZCL-14:
//! > Audit entry includes plugin name version tool name and duration.
//!
//! Executes an echo plugin tool with an AuditLogger attached and asserts the
//! serialised audit entry contains all four fields.

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
async fn audit_entry_contains_plugin_name_version_tool_name_and_duration() {
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
        "1.2.3".into(),
        "tool_echo".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({"hello": "world"}))
        .await
        .expect("execute should not return Err");

    assert!(result.success, "echo tool should succeed");

    let events = read_audit_events(tmp.path());
    assert!(
        !events.is_empty(),
        "audit log should contain at least one entry"
    );

    let entry = &events[0];

    // Plugin name and version should appear in the action command field
    // Format: "plugin_name@version:tool_name"
    let command = entry["action"]["command"]
        .as_str()
        .expect("action.command must be a string");

    assert!(
        command.contains("echo_plugin"),
        "action.command should contain the plugin name, got: {command}"
    );
    assert!(
        command.contains("1.2.3"),
        "action.command should contain the plugin version, got: {command}"
    );
    assert!(
        command.contains("echo_tool"),
        "action.command should contain the tool name, got: {command}"
    );
    assert_eq!(
        command, "echo_plugin@1.2.3:echo_tool",
        "action.command should have format plugin@version:tool"
    );

    // Duration must be present and non-negative
    let duration = entry["result"]["duration_ms"]
        .as_u64()
        .expect("result.duration_ms must be a number");
    // Duration should be reasonable (under 30 seconds for an echo plugin)
    assert!(
        duration < 30_000,
        "duration_ms should be reasonable, got: {duration}"
    );
}

#[tokio::test]
async fn failed_execution_audit_entry_also_has_all_fields() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

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
        "2.0.0".into(),
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
    assert!(!events.is_empty(), "audit log should contain an entry");

    let entry = &events[0];
    let command = entry["action"]["command"]
        .as_str()
        .expect("action.command must be a string");

    assert_eq!(
        command, "broken_plugin@2.0.0:broken_tool",
        "failed execution should also log plugin@version:tool"
    );

    assert!(
        entry["result"]["duration_ms"].as_u64().is_some(),
        "failed execution should also log duration_ms"
    );
}
