//! Integration test: HTTP test plugin fetches from an allowed host.
//!
//! Loads `http_plugin.wasm` with `allowed_hosts = ["example.com"]`,
//! injects `base_url` via config, calls `tool_http_fetch`, and asserts
//! a successful HTTP 200 response.

use std::path::Path;

const HTTP_PLUGIN_WASM: &str = "tests/plugins/artifacts/http_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(HTTP_PLUGIN_WASM)
}

#[test]
fn http_plugin_fetches_from_allowed_host() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "http_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(10))
        .with_allowed_hosts(["example.com"].iter().map(|s| s.to_string()))
        .with_config([("base_url", "http://example.com"), ("auth_token", "unused")].into_iter());

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate http-plugin");

    let output = plugin
        .call::<&str, &str>("tool_http_fetch", "{}")
        .expect("tool_http_fetch call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert_eq!(
        parsed["status_code"].as_u64(),
        Some(200),
        "expected status_code=200, got: {parsed}"
    );

    let body = parsed["body"].as_str().expect("body should be a string");
    assert!(!body.is_empty(), "expected non-empty body from example.com");
    assert!(
        body.contains("Example Domain"),
        "expected body to contain 'Example Domain', got: {body:.200}"
    );
}
