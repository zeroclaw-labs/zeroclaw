//! Integration test: tool_http_get fetches from an allowed host.
//!
//! Loads `multi_tool_plugin.wasm` with allowed hosts `["httpbin.org", "example.com"]`,
//! calls `tool_http_get` targeting `http://example.com`, and asserts a successful
//! response with status 200 and body content.

use std::path::Path;

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
}

#[test]
fn tool_http_get_fetches_from_allowed_host() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(10))
        .with_allowed_hosts(
            ["httpbin.org", "example.com"]
                .iter()
                .map(|s| s.to_string()),
        );

    let mut plugin = extism::Plugin::new(&manifest, [], true)
        .expect("failed to instantiate multi-tool plugin");

    let input = r#"{"url": "http://example.com"}"#;
    let output = plugin
        .call::<&str, &str>("tool_http_get", input)
        .expect("tool_http_get call failed");

    let parsed: serde_json::Value =
        serde_json::from_str(output).expect("output is not valid JSON");

    // Assert status_code is 200
    assert_eq!(
        parsed["status_code"].as_u64(),
        Some(200),
        "expected status_code=200, got: {parsed}"
    );

    // Assert body is non-empty and contains expected content from example.com
    let body = parsed["body"]
        .as_str()
        .expect("body should be a string");
    assert!(
        !body.is_empty(),
        "expected non-empty body from example.com"
    );
    assert!(
        body.contains("Example Domain"),
        "expected body to contain 'Example Domain', got: {body:.200}"
    );
}
