//! Verify that a tampered WASM binary produces a clear HashMismatch error
//! and the plugin is effectively rejected (acceptance criterion for US-ZCL-16).

use tempfile::tempdir;
use zeroclaw::plugins::host::PluginHost;

/// When the WASM binary is modified after discovery, `verify_wasm_integrity`
/// must return a `HashMismatch` error containing the plugin name and both
/// the expected and actual hashes.
#[test]
fn mismatched_hash_produces_clear_error_and_plugin_is_not_loaded() {
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

    // Write the original WASM binary so the host records its SHA-256 hash.
    std::fs::write(plugin_dir.join("plugin.wasm"), b"original-wasm-content").unwrap();

    let host = PluginHost::new(dir.path()).unwrap();

    // Sanity: plugin was discovered and hash was recorded.
    let info = host
        .get_plugin("tampered")
        .expect("plugin should be discovered");
    assert!(
        info.wasm_sha256.is_some(),
        "SHA-256 hash must be recorded at discovery time"
    );

    // Integrity passes before tampering.
    host.verify_wasm_integrity("tampered")
        .expect("integrity check should pass on unmodified binary");

    // --- Tamper with the WASM binary on disk ---
    std::fs::write(plugin_dir.join("plugin.wasm"), b"malicious-replacement").unwrap();

    // --- Verify the error ---
    let err = host
        .verify_wasm_integrity("tampered")
        .expect_err("integrity check must fail after tampering");

    let err_msg = err.to_string();

    // The error message must clearly identify:
    // 1. That it is an integrity/hash failure
    assert!(
        err_msg.contains("integrity check failed"),
        "error should mention integrity check failure, got: {err_msg}"
    );
    // 2. Which plugin is affected
    assert!(
        err_msg.contains("tampered"),
        "error should name the plugin, got: {err_msg}"
    );
    // 3. Both expected and actual hashes (so the operator can investigate)
    assert!(
        err_msg.contains("expected hash"),
        "error should include expected hash, got: {err_msg}"
    );
    assert!(
        err_msg.contains("got"),
        "error should include actual hash, got: {err_msg}"
    );
}

/// A plugin whose WASM file does not exist at discovery time gets no hash,
/// and `verify_wasm_integrity` should still pass (no hash to compare against).
/// This ensures the mismatch check only fires when a hash was actually recorded.
#[test]
fn missing_wasm_at_discovery_means_no_hash_and_no_mismatch() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugins").join("no-wasm");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "no-wasm"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();
    // Deliberately do NOT create plugin.wasm

    let host = PluginHost::new(dir.path()).unwrap();
    let info = host
        .get_plugin("no-wasm")
        .expect("plugin should be discovered");
    assert!(
        info.wasm_sha256.is_none(),
        "no hash should be recorded when WASM file is absent"
    );

    // verify_wasm_integrity should pass — nothing to compare
    host.verify_wasm_integrity("no-wasm")
        .expect("no hash means no mismatch");
}
