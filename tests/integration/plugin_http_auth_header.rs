#![cfg(feature = "plugins-wasm")]
//! Integration test: tool_http_get_auth passes Authorization header from config values.
//!
//! Loads `multi_tool_plugin.wasm` with config `{api_key: "test-secret-token"}` and
//! allowed hosts including `httpbin.org`, calls `tool_http_get_auth` targeting
//! `http://httpbin.org/headers`, and asserts the response body contains the
//! `Authorization: Bearer test-secret-token` header.

use std::path::Path;

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
}

#[test]
fn tool_http_get_auth_passes_authorization_header_from_config() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let config = [("api_key", "test-secret-token")];

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(10))
        .with_config(config.into_iter())
        .with_allowed_hosts(["httpbin.org"].iter().map(|s| s.to_string()));

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");

    // httpbin.org/headers echoes back request headers as JSON
    let input = r#"{"url": "http://httpbin.org/headers"}"#;
    let output = plugin
        .call::<&str, &str>("tool_http_get_auth", input)
        .expect("tool_http_get_auth call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert_eq!(
        parsed["status_code"].as_u64(),
        Some(200),
        "expected status_code=200, got: {parsed}"
    );

    // Parse the inner body (httpbin returns JSON with a "headers" object)
    let body = parsed["body"].as_str().expect("body should be a string");
    let body_json: serde_json::Value =
        serde_json::from_str(body).expect("httpbin body should be valid JSON");

    let auth_header = body_json["headers"]["Authorization"]
        .as_str()
        .expect("Authorization header should be present in httpbin response");

    assert_eq!(
        auth_header, "Bearer test-secret-token",
        "expected Authorization header to contain the config api_key value"
    );
}
