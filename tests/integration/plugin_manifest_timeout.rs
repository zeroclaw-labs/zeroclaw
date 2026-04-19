#![cfg(feature = "plugins-wasm")]

//! Integration test: timeout_ms from plugin manifest is enforced on Extism calls.
//!
//! Verifies three things:
//! 1. `PluginManifest::parse` correctly reads `timeout_ms` from TOML
//! 2. `build_extism_manifest` propagates it to the Extism manifest
//! 3. The Extism runtime actually enforces the timeout on a long-running call

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use zeroclaw::plugins::PluginManifest;
use zeroclaw::plugins::loader::build_extism_manifest;

fn project_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Build a minimal PluginManifest with a specific timeout_ms, pointing at the
/// bad-actor WASM (which has a `tool_infinite_loop` export).
fn manifest_with_timeout(timeout_ms: u64) -> PluginManifest {
    PluginManifest {
        name: "timeout-test".to_string(),
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

#[test]
fn manifest_timeout_ms_is_parsed_from_toml() {
    let toml_str = r#"
[plugin]
name = "timeout-parse-test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
timeout_ms = 2000

[[tools]]
name = "tool_infinite_loop"
description = "spins forever"
export = "tool_infinite_loop"
risk_level = "high"
parameters_schema = { type = "object" }
"#;

    let manifest =
        PluginManifest::parse(toml_str).expect("failed to parse manifest with timeout_ms");

    assert_eq!(
        manifest.timeout_ms, 2_000,
        "parsed timeout_ms should be 2000"
    );
}

#[test]
fn manifest_default_timeout_ms_is_30_seconds() {
    let toml_str = r#"
[plugin]
name = "default-timeout-test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;

    let manifest =
        PluginManifest::parse(toml_str).expect("failed to parse manifest without timeout_ms");

    assert_eq!(
        manifest.timeout_ms, 30_000,
        "default timeout_ms should be 30000"
    );
}

#[test]
fn manifest_timeout_ms_flows_to_extism_manifest() {
    let pm = manifest_with_timeout(5_000);
    let loader_manifest = build_extism_manifest(&pm, Path::new("/tmp"), None);

    assert_eq!(
        loader_manifest.manifest.timeout_ms,
        Some(5_000),
        "Extism manifest timeout should match the plugin manifest timeout_ms"
    );
}

#[test]
fn manifest_timeout_ms_enforced_on_plugin_call() {
    let wasm_dir = project_root().join("tests/plugins/artifacts");
    let wasm_path = wasm_dir.join("bad_actor_plugin.wasm");
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {}",
        wasm_path.display()
    );

    // Use manifest with 2-second timeout, pointing at the bad-actor WASM.
    let pm = manifest_with_timeout(2_000);
    let loader_manifest = build_extism_manifest(&pm, &wasm_dir, None);

    let mut plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate plugin via manifest-derived Extism config");

    let timeout = Duration::from_millis(pm.timeout_ms);
    let start = Instant::now();
    let result = plugin.call::<&str, &str>("tool_infinite_loop", "{}");
    let elapsed = start.elapsed();

    // The call must fail with a timeout error.
    assert!(
        result.is_err(),
        "tool_infinite_loop should fail with a timeout error, but succeeded"
    );

    let err_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err_msg.contains("timeout") || err_msg.contains("timed out"),
        "error should indicate a timeout, got: {}",
        err_msg
    );

    // The call should complete within the manifest timeout plus a small grace window.
    let max_allowed = timeout + Duration::from_secs(2);
    assert!(
        elapsed < max_allowed,
        "call took {:?}, expected it to finish within {:?} (manifest timeout_ms={})",
        elapsed,
        max_allowed,
        pm.timeout_ms
    );
}
