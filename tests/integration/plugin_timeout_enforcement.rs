//! Integration test: timeout enforcement with a short (100ms) timeout.
//!
//! Task US-ZCL-17-7: Create or use a test plugin with an intentional infinite
//! loop. Set timeout_ms to 100ms. Call the plugin and verify it is terminated
//! with a timeout error within a reasonable time.

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

fn manifest_with_timeout(timeout_ms: u64) -> PluginManifest {
    PluginManifest {
        name: "timeout-enforcement-test".to_string(),
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
async fn short_timeout_100ms_terminates_infinite_loop() {
    let wasm_dir = project_root().join("tests/plugins/artifacts");
    let wasm_path = wasm_dir.join("bad_actor_plugin.wasm");
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let pm = manifest_with_timeout(100);
    let loader_manifest = build_extism_manifest(&pm, &wasm_dir, None);

    let plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate plugin");

    let wasm_tool = WasmTool::new(
        "tool_infinite_loop".to_string(),
        "spins forever".to_string(),
        "timeout-enforcement-test".to_string(),
        "0.1.0".to_string(),
        "tool_infinite_loop".to_string(),
        json!({"type": "object"}),
        Arc::new(Mutex::new(plugin)),
    );

    let start = Instant::now();
    let result = wasm_tool.execute(json!({})).await;
    let elapsed = start.elapsed();

    // Must return Ok(ToolResult), not panic or Err.
    let tool_result = result.expect("execute must return Ok(ToolResult), not Err");

    // The call must fail (plugin was killed by timeout).
    assert!(
        !tool_result.success,
        "timed-out plugin must produce success=false"
    );

    // The output must mention the timeout.
    assert!(
        tool_result.output.contains("timed out"),
        "output should contain 'timed out', got: {}",
        tool_result.output
    );

    // The call must complete within 100ms timeout + 2s grace for WASM teardown.
    let max_allowed = Duration::from_millis(100) + Duration::from_secs(2);
    assert!(
        elapsed < max_allowed,
        "call took {:?}, expected it to finish within {:?}",
        elapsed,
        max_allowed
    );
}

#[test]
fn raw_extism_100ms_timeout_enforced() {
    let wasm_dir = project_root().join("tests/plugins/artifacts");
    let wasm_path = wasm_dir.join("bad_actor_plugin.wasm");
    assert!(wasm_path.is_file(), "bad_actor_plugin.wasm missing");

    let pm = manifest_with_timeout(100);
    let loader_manifest = build_extism_manifest(&pm, &wasm_dir, None);

    // Confirm the Extism manifest has the 100ms timeout set.
    assert_eq!(
        loader_manifest.manifest.timeout_ms,
        Some(100),
        "Extism manifest should have timeout_ms=100"
    );

    let mut plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate plugin");

    let start = Instant::now();
    let result = plugin.call::<&str, &str>("tool_infinite_loop", "{}");
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "tool_infinite_loop should fail with timeout, but succeeded"
    );

    let err_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err_msg.contains("timeout") || err_msg.contains("timed out"),
        "error should mention timeout, got: {}",
        err_msg
    );

    let max_allowed = Duration::from_millis(100) + Duration::from_secs(2);
    assert!(
        elapsed < max_allowed,
        "call took {:?}, expected within {:?}",
        elapsed,
        max_allowed
    );
}
