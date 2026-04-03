#![cfg(feature = "plugins-wasm")]

//! Verify that SHA-256 hash is recorded at install time in plugin metadata
//! (acceptance criterion for US-ZCL-16).

use sha2::{Digest, Sha256};
use tempfile::tempdir;
use zeroclaw::plugins::host::PluginHost;

/// When a plugin is installed via `PluginHost::install`, the resulting
/// `PluginInfo` must contain a `wasm_sha256` field that matches the
/// SHA-256 digest of the installed WASM binary.
#[test]
fn sha256_hash_recorded_at_install_time_in_plugin_metadata() {
    let workspace = tempdir().unwrap();

    // Create a source directory with a valid manifest and WASM binary.
    let source_dir = workspace.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();

    let wasm_content = b"fake-wasm-binary-for-hash-test";

    std::fs::write(
        source_dir.join("manifest.toml"),
        r#"
name = "hash-at-install"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();
    std::fs::write(source_dir.join("plugin.wasm"), wasm_content).unwrap();

    // Compute the expected SHA-256 hash independently.
    let expected_hash = {
        let mut hasher = Sha256::new();
        hasher.update(wasm_content);
        hex::encode(hasher.finalize())
    };

    // Install the plugin.
    let mut host = PluginHost::new(workspace.path()).unwrap();
    host.install(source_dir.to_str().unwrap()).unwrap();

    // The plugin metadata must contain the hash.
    let info = host
        .get_plugin("hash-at-install")
        .expect("plugin should be available after install");

    let recorded_hash = info
        .wasm_sha256
        .as_ref()
        .expect("SHA-256 hash must be recorded in plugin metadata at install time");

    assert_eq!(
        recorded_hash, &expected_hash,
        "recorded hash must match SHA-256 of the installed WASM binary"
    );

    // Integrity verification should also pass.
    host.verify_wasm_integrity("hash-at-install")
        .expect("integrity check should pass immediately after install");
}
