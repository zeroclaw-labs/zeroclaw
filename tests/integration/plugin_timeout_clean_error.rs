//! Integration test: plugin exceeding timeout is terminated cleanly with error response.
//!
//! Verifies the acceptance criterion for US-ZCL-17:
//! > Plugin exceeding timeout is terminated cleanly with error response
//!
//! This exercises the `WasmTool::execute` path end-to-end: a plugin that
//! infinite-loops is killed by the Extism timeout, and the caller receives
//! a structured `ToolResult { success: false, … }` — not a panic or crash.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use zeroclaw::plugins::loader::build_extism_manifest;
use zeroclaw::plugins::wasm_tool::WasmTool;
use zeroclaw::plugins::PluginManifest;
use zeroclaw::tools::traits::Tool;

fn project_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Build a PluginManifest pointing at the bad-actor WASM with a given timeout.
fn manifest_with_timeout(timeout_ms: u64) -> PluginManifest {
    PluginManifest {
        name: "timeout-clean-test".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: "bad_actor_plugin.wasm".to_string(),
        capabilities: vec![],
        permissions: vec![],
        allowed_hosts: vec![],
        allowed_paths: HashMap::new(),
        tools: vec![],
        config: HashMap::new(),
        wasi: true,
        timeout_ms,
        signature: None,
        publisher_key: None,
        host_capabilities: Default::default(),
    }
}

#[tokio::test]
async fn timeout_returns_clean_tool_result_not_panic() {
    let wasm_dir = project_root().join("tests/plugins/artifacts");
    let wasm_path = wasm_dir.join("bad_actor_plugin.wasm");
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let pm = manifest_with_timeout(2_000);
    let loader_manifest = build_extism_manifest(&pm, &wasm_dir, None);

    let plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate plugin");

    let wasm_tool = WasmTool::new(
        "tool_infinite_loop".to_string(),
        "spins forever".to_string(),
        "timeout-clean-test".to_string(),
        "0.1.0".to_string(),
        "tool_infinite_loop".to_string(),
        json!({"type": "object"}),
        Arc::new(Mutex::new(plugin)),
    );

    let start = Instant::now();
    let result = wasm_tool.execute(json!({})).await;
    let elapsed = start.elapsed();

    // Must not panic — we get an Ok(ToolResult), not an Err or unwinding panic.
    let tool_result = result.expect("execute must return Ok(ToolResult), not an Err");

    // The ToolResult must indicate failure, not success.
    assert!(
        !tool_result.success,
        "timed-out plugin must produce success=false, got success=true"
    );

    // The output must mention the timeout classification.
    assert!(
        tool_result.output.contains("timed out"),
        "output should contain 'timed out', got: {}",
        tool_result.output
    );

    // The output should identify the plugin and function.
    assert!(
        tool_result.output.contains("timeout-clean-test")
            && tool_result.output.contains("tool_infinite_loop"),
        "output should identify plugin and function, got: {}",
        tool_result.output
    );

    // The error field must be present with the raw error details.
    let err = tool_result
        .error
        .as_ref()
        .expect("error field must be Some for a timed-out call");
    let err_lower = err.to_lowercase();
    assert!(
        err_lower.contains("timeout") || err_lower.contains("timed out"),
        "error field should mention timeout, got: {}",
        err
    );

    // The call must complete within a reasonable window (timeout + grace).
    let max_allowed = Duration::from_millis(2_000) + Duration::from_secs(2);
    assert!(
        elapsed < max_allowed,
        "call took {:?}, expected it to finish within {:?}",
        elapsed,
        max_allowed
    );
}

#[tokio::test]
async fn plugin_is_still_usable_after_timeout() {
    // Verifies "terminated cleanly" — the plugin instance can be called again
    // after a timeout without crashing or deadlocking.
    let wasm_dir = project_root().join("tests/plugins/artifacts");
    let wasm_path = wasm_dir.join("bad_actor_plugin.wasm");
    assert!(wasm_path.is_file(), "bad_actor_plugin.wasm missing");

    let pm = manifest_with_timeout(1_000);
    let loader_manifest = build_extism_manifest(&pm, &wasm_dir, None);

    let plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate plugin");
    let shared = Arc::new(Mutex::new(plugin));

    let wasm_tool = WasmTool::new(
        "tool_infinite_loop".to_string(),
        "spins forever".to_string(),
        "timeout-clean-test".to_string(),
        "0.1.0".to_string(),
        "tool_infinite_loop".to_string(),
        json!({"type": "object"}),
        shared,
    );

    // First call: times out.
    let r1 = wasm_tool
        .execute(json!({}))
        .await
        .expect("first call should return Ok");
    assert!(!r1.success, "first call should fail with timeout");

    // Second call: also times out, but crucially does NOT panic, deadlock, or crash.
    let r2 = wasm_tool
        .execute(json!({}))
        .await
        .expect("second call should return Ok");
    assert!(!r2.success, "second call should also fail cleanly");
}
