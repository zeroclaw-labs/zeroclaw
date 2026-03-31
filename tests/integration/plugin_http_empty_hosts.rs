//! Integration test: empty `allowed_hosts` means no network access.
//!
//! Acceptance criterion for US-ZCL-6:
//! > Empty allowed_hosts means no network access
//!
//! Builds the Extism manifest through `build_extism_manifest` with a
//! `PluginManifest` whose `allowed_hosts` is empty, instantiates the WASM
//! plugin, and asserts that any HTTP request is denied by the sandbox.

use std::path::Path;

use zeroclaw::plugins::loader::build_extism_manifest;
use zeroclaw::plugins::PluginManifest;

const BAD_ACTOR_WASM: &str = "tests/plugins/artifacts/bad_actor_plugin.wasm";

fn bad_actor_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(BAD_ACTOR_WASM)
}

/// When `allowed_hosts` is empty, the Extism manifest should have `None` for
/// allowed_hosts, which tells the runtime to deny all HTTP requests.
#[test]
fn empty_allowed_hosts_produces_no_network_manifest() {
    let toml_str = r#"
        name = "no-net-plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
        allowed_hosts = []

        [[tools]]
        name = "noop"
        description = "placeholder"
        export = "noop"
        risk_level = "low"
    "#;

    let pm = PluginManifest::parse(toml_str).expect("should parse");
    assert!(pm.allowed_hosts.is_empty());

    let result = build_extism_manifest(&pm, Path::new("/tmp"), None);
    assert!(
        result.manifest.allowed_hosts.is_none(),
        "empty allowed_hosts should produce None (no network), got {:?}",
        result.manifest.allowed_hosts
    );
}

/// End-to-end: instantiate a real plugin with empty `allowed_hosts` and
/// confirm HTTP requests are denied at runtime.
#[test]
fn http_request_denied_when_allowed_hosts_empty() {
    let wasm_path = bad_actor_wasm_path();
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {} — run build-test-plugins.sh first",
        wasm_path.display()
    );

    // Build manifest the same way the loader does: empty allowed_hosts → no
    // with_allowed_hosts() call → Extism denies all HTTP.
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));
    // Notably: no .with_allowed_hosts() — mirrors build_extism_manifest
    // behaviour when allowed_hosts is empty.

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate bad-actor plugin");

    // tool_http_blocked tries to reach https://evil.example.com — should fail.
    let result = plugin.call::<&str, &str>("tool_http_blocked", "{}");

    assert!(
        result.is_err(),
        "HTTP request must be denied when allowed_hosts is empty, but succeeded: {:?}",
        result
    );
}

/// Same test but going through `build_extism_manifest` to verify the full
/// ZeroClaw loader path produces a sandbox that blocks HTTP.
#[test]
fn build_extism_manifest_with_empty_hosts_blocks_http() {
    let wasm_path = bad_actor_wasm_path();
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {} — run build-test-plugins.sh first",
        wasm_path.display()
    );

    // Construct a PluginManifest with empty allowed_hosts, pointing at the
    // real bad-actor WASM binary.
    let toml_str = format!(
        r#"
        name = "empty-hosts-test"
        version = "0.1.0"
        wasm_path = "{}"
        capabilities = ["tool"]
        permissions = ["http_client"]
        allowed_hosts = []

        [[tools]]
        name = "tool_http_blocked"
        description = "tries to hit evil.example.com"
        export = "tool_http_blocked"
        risk_level = "high"
    "#,
        wasm_path.display()
    );

    let pm = PluginManifest::parse(&toml_str).expect("should parse");
    assert!(
        pm.allowed_hosts.is_empty(),
        "precondition: allowed_hosts must be empty"
    );

    // Build through the loader — this is the code path used in production.
    // plugin_dir is "/" so the absolute wasm_path resolves correctly.
    let loader_manifest = build_extism_manifest(&pm, Path::new("/"), None);

    // Verify the extism manifest has no allowed_hosts.
    assert!(
        loader_manifest.manifest.allowed_hosts.is_none(),
        "loader should not set allowed_hosts when the list is empty"
    );

    // Instantiate and attempt HTTP — must be denied.
    let mut plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("plugin instantiation should succeed");

    let result = plugin.call::<&str, &str>("tool_http_blocked", "{}");
    assert!(
        result.is_err(),
        "HTTP request must be blocked when allowed_hosts is empty (via build_extism_manifest), \
         but succeeded: {:?}",
        result
    );
}
