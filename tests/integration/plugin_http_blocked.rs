//! Integration test: HTTP requests to non-allowed hosts are blocked by Extism.
//!
//! Loads `bad_actor_plugin.wasm` WITHOUT granting network access, calls
//! `tool_http_blocked` which attempts to reach `evil.example.com`, and asserts
//! the sandbox denies the request.

use std::path::Path;

const BAD_ACTOR_WASM: &str = "tests/plugins/artifacts/bad_actor_plugin.wasm";

fn bad_actor_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(BAD_ACTOR_WASM)
}

#[test]
fn http_request_to_non_allowed_host_is_blocked() {
    let wasm_path = bad_actor_wasm_path();
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {}",
        wasm_path.display()
    );

    // No with_allowed_hosts() call — the plugin has zero network access.
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let mut plugin = extism::Plugin::new(&manifest, [], true)
        .expect("failed to instantiate bad-actor plugin");

    let result = plugin.call::<&str, &str>("tool_http_blocked", "{}");

    assert!(
        result.is_err(),
        "HTTP request to non-allowed host should be denied, but succeeded with: {:?}",
        result
    );
}

#[test]
fn http_request_blocked_when_different_host_allowed() {
    let wasm_path = bad_actor_wasm_path();
    assert!(wasm_path.is_file());

    // Allow only example.com — evil.example.com should still be blocked.
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_allowed_hosts(["example.com"].iter().map(|s| s.to_string()));

    let mut plugin = extism::Plugin::new(&manifest, [], true)
        .expect("failed to instantiate bad-actor plugin");

    let result = plugin.call::<&str, &str>("tool_http_blocked", "{}");

    assert!(
        result.is_err(),
        "HTTP request to evil.example.com should be denied when only example.com is allowed, \
         but succeeded with: {:?}",
        result
    );
}
