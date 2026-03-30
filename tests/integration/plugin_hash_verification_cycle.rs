//! Full-cycle hash verification test (US-ZCL-16-7):
//! install (hash recorded) -> load succeeds (hash matches) -> tamper binary
//! -> load fails (hash mismatch) -> reinstall -> load succeeds (hash recalculated).

use tempfile::tempdir;
use zeroclaw::plugins::host::PluginHost;

#[test]
fn full_hash_verification_cycle_install_tamper_reinstall() {
    let workspace = tempdir().unwrap();

    // --- Source directory for the plugin ---
    let source_dir = workspace.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("manifest.toml"),
        r#"
name = "cycle-test"
version = "1.0.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();
    std::fs::write(source_dir.join("plugin.wasm"), b"original-wasm-binary").unwrap();

    // === Step 1: Install plugin — hash must be recorded ===
    let mut host = PluginHost::new(workspace.path()).unwrap();
    host.install(source_dir.to_str().unwrap()).unwrap();

    let info = host
        .get_plugin("cycle-test")
        .expect("plugin should be available after install");
    let original_hash = info
        .wasm_sha256
        .as_ref()
        .expect("SHA-256 hash must be recorded at install time")
        .clone();

    // === Step 2: Load succeeds — hash matches ===
    let loaded = host
        .load_plugin("cycle-test")
        .expect("load_plugin must succeed for unmodified binary");
    assert_eq!(loaded.wasm_sha256.as_deref(), Some(original_hash.as_str()));

    // === Step 3: Modify .wasm binary in place ===
    let installed_wasm = workspace
        .path()
        .join("plugins")
        .join("cycle-test")
        .join("plugin.wasm");
    std::fs::write(&installed_wasm, b"tampered-malicious-content").unwrap();

    // === Step 4: Load fails — hash mismatch ===
    let err = host
        .load_plugin("cycle-test")
        .expect_err("load_plugin must reject tampered binary");
    let msg = err.to_string();
    assert!(
        msg.contains("integrity check failed"),
        "error should mention integrity failure: {msg}"
    );
    assert!(
        msg.contains("cycle-test"),
        "error should name the plugin: {msg}"
    );

    // === Step 5: Reinstall plugin (remove + install with new binary) ===
    host.remove("cycle-test").unwrap();

    // Update source with a new legitimate binary
    std::fs::write(source_dir.join("plugin.wasm"), b"updated-legitimate-binary-v2").unwrap();
    host.install(source_dir.to_str().unwrap()).unwrap();

    // === Step 6: Load succeeds — hash recalculated ===
    let reloaded = host
        .load_plugin("cycle-test")
        .expect("load_plugin must succeed after reinstall");
    let new_hash = reloaded
        .wasm_sha256
        .as_ref()
        .expect("hash must be present after reinstall");

    assert_ne!(
        new_hash, &original_hash,
        "hash must differ after reinstall with new binary"
    );

    // Verify integrity passes cleanly
    host.verify_wasm_integrity("cycle-test")
        .expect("integrity check must pass after reinstall");
}
