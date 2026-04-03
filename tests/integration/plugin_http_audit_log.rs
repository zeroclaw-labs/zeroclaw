#![cfg(feature = "plugins-wasm")]

//! Integration test: HTTP requests made by plugin are logged (URL and method only).
//!
//! Verifies acceptance criterion for US-ZCL-14:
//! > HTTP requests made by plugin are logged (URL and method only).
//!
//! Uses the multi-tool plugin's `tool_http_get` to make an HTTP request,
//! then reads the audit log and asserts the entry contains an `http_requests`
//! array with the correct URL and method.

use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
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
async fn http_request_logged_with_url_and_method() {
    let wasm_path = wasm_path();
    if !wasm_path.is_file() {
        eprintln!("skipping: multi_tool_plugin.wasm not found");
        return;
    }

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(10))
        .with_allowed_hosts(["example.com"].iter().map(|s| s.to_string()));

    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "http_get_tool".into(),
        "fetches a URL".into(),
        "multi_tool_plugin".into(),
        "1.0.0".into(),
        "tool_http_get".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({"url": "http://example.com"}))
        .await
        .expect("execute should not return Err");

    assert!(
        result.success,
        "tool_http_get should succeed, got: {:?}",
        result.error
    );

    let events = read_audit_events(tmp.path());
    assert!(
        !events.is_empty(),
        "audit log should contain at least one entry"
    );

    let entry = &events[0];

    // The action must contain http_requests
    let http_requests = entry["action"]["http_requests"]
        .as_array()
        .expect("action.http_requests should be an array");

    assert_eq!(
        http_requests.len(),
        1,
        "should have exactly one HTTP request logged"
    );

    let req = &http_requests[0];
    assert_eq!(
        req["method"].as_str(),
        Some("GET"),
        "HTTP method should be GET"
    );
    assert_eq!(
        req["url"].as_str(),
        Some("http://example.com"),
        "HTTP URL should match the request"
    );
}

#[tokio::test]
async fn http_requests_not_logged_for_non_http_tools() {
    let wasm_path = wasm_path();
    if !wasm_path.is_file() {
        eprintln!("skipping: multi_tool_plugin.wasm not found");
        return;
    }

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    // Use the non-HTTP tool_add function
    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "add_tool".into(),
        "adds numbers".into(),
        "multi_tool_plugin".into(),
        "1.0.0".into(),
        "tool_add".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    use zeroclaw::tools::traits::Tool;
    let result = tool
        .execute(serde_json::json!({"a": 1, "b": 2}))
        .await
        .expect("execute should not return Err");

    assert!(result.success, "tool_add should succeed");

    let events = read_audit_events(tmp.path());
    assert!(!events.is_empty(), "audit log should have an entry");

    let entry = &events[0];
    // Non-HTTP tools should NOT have http_requests in the audit log
    assert!(
        entry["action"]["http_requests"].is_null(),
        "non-HTTP tool should not have http_requests, got: {}",
        entry["action"]
    );
}

#[tokio::test]
async fn http_request_logs_only_url_and_method() {
    let wasm_path = wasm_path();
    if !wasm_path.is_file() {
        eprintln!("skipping: multi_tool_plugin.wasm not found");
        return;
    }

    let tmp = TempDir::new().expect("failed to create temp dir");
    let logger = Arc::new(make_audit_logger(tmp.path()));

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(10))
        .with_allowed_hosts(["example.com"].iter().map(|s| s.to_string()));

    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");
    let plugin = Arc::new(Mutex::new(plugin));

    let tool = zeroclaw::plugins::wasm_tool::WasmTool::new(
        "http_get_tool".into(),
        "fetches a URL".into(),
        "multi_tool_plugin".into(),
        "1.0.0".into(),
        "tool_http_get".into(),
        serde_json::json!({"type": "object"}),
        plugin,
    )
    .with_audit_logger(Arc::clone(&logger));

    use zeroclaw::tools::traits::Tool;
    let _ = tool
        .execute(serde_json::json!({"url": "http://example.com"}))
        .await
        .expect("execute should not return Err");

    let events = read_audit_events(tmp.path());
    let entry = &events[0];
    let req = &entry["action"]["http_requests"][0];

    // Verify only url and method are present (no body, headers, etc.)
    let req_obj = req
        .as_object()
        .expect("http request entry should be an object");
    let keys: Vec<&String> = req_obj.keys().collect();
    assert!(
        keys.contains(&&"url".to_string()),
        "entry must contain 'url'"
    );
    assert!(
        keys.contains(&&"method".to_string()),
        "entry must contain 'method'"
    );
    assert_eq!(
        keys.len(),
        2,
        "entry should contain exactly url and method, got: {:?}",
        keys
    );
}
