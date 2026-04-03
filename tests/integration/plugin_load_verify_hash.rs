#![cfg(feature = "plugins-wasm")]

//! Verify that hash is checked on every plugin load via `load_plugin`
//! (acceptance criterion #2 for US-ZCL-16).

use tempfile::tempdir;
use zeroclaw::plugins::host::PluginHost;

/// `load_plugin` must pass when the WASM binary is unmodified.
#[test]
fn load_plugin_passes_when_binary_is_unmodified() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugins").join("good");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "good"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"valid-wasm-bytes").unwrap();

    let host = PluginHost::new(dir.path()).unwrap();

    // load_plugin should succeed — hash matches.
    let info = host
        .load_plugin("good")
        .expect("load_plugin must succeed for unmodified binary");
    assert_eq!(info.name, "good");
    assert!(info.wasm_sha256.is_some());
}

/// `load_plugin` must reject a binary that was tampered with after discovery.
#[test]
fn load_plugin_rejects_tampered_binary() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugins").join("tampered");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "tampered"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"original-content").unwrap();

    let host = PluginHost::new(dir.path()).unwrap();

    // First load succeeds.
    host.load_plugin("tampered").expect("first load must pass");

    // Tamper with the binary.
    std::fs::write(plugin_dir.join("plugin.wasm"), b"malicious-content").unwrap();

    // Subsequent load must fail with a hash mismatch error.
    let err = host
        .load_plugin("tampered")
        .expect_err("load_plugin must reject tampered binary");

    let msg = err.to_string();
    assert!(
        msg.contains("integrity check failed"),
        "error should mention integrity failure: {msg}"
    );
    assert!(
        msg.contains("tampered"),
        "error should name the plugin: {msg}"
    );
}

/// `load_plugin` verifies hash every time it is called, not just the first.
#[test]
fn load_plugin_verifies_hash_on_every_call() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugins").join("multi-load");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "multi-load"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"original-bytes").unwrap();

    let host = PluginHost::new(dir.path()).unwrap();

    // Multiple successful loads.
    for _ in 0..3 {
        host.load_plugin("multi-load")
            .expect("repeated loads must pass for unmodified binary");
    }

    // Tamper after several successful loads.
    std::fs::write(plugin_dir.join("plugin.wasm"), b"tampered-bytes").unwrap();

    // The very next load must catch the tampering.
    host.load_plugin("multi-load")
        .expect_err("load_plugin must catch tampering even after prior successful loads");
}

/// `load_plugin` for a non-existent plugin returns NotFound.
#[test]
fn load_plugin_not_found_for_unknown_plugin() {
    let dir = tempdir().unwrap();
    let host = PluginHost::new(dir.path()).unwrap();

    let err = host
        .load_plugin("nonexistent")
        .expect_err("load_plugin must fail for unknown plugin");
    let msg = err.to_string();
    assert!(
        msg.contains("nonexistent"),
        "error should name the missing plugin: {msg}"
    );
}
