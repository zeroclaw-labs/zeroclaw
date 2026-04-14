#![cfg(feature = "plugins-wasm")]
//! Integration test: tool_read_file reads from an allowed path.
//!
//! Loads `multi_tool_plugin.wasm` with `allowed_paths` mapping the
//! `tests/fixtures/` directory into the WASI guest, calls `tool_read_file`,
//! and asserts that the file contents are returned correctly.

use std::path::Path;

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";
const FIXTURES_DIR: &str = "tests/fixtures";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
}

fn fixtures_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR)
}

#[test]
fn tool_read_file_reads_from_allowed_path() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let fixtures = fixtures_path();
    assert!(
        fixtures.is_dir(),
        "fixtures directory not found at {}",
        fixtures.display()
    );

    // Map host fixtures dir to /fixtures inside the WASI guest
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_allowed_path(fixtures.to_string_lossy().into_owned(), "/fixtures");

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");

    let input = r#"{"path": "/fixtures/sample.txt"}"#;
    let output = plugin
        .call::<&str, &str>("tool_read_file", input)
        .expect("tool_read_file call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    let contents = parsed["contents"]
        .as_str()
        .expect("contents should be a string");

    assert_eq!(
        contents.trim(),
        "Hello from ZeroClaw test fixture!",
        "expected fixture file contents, got: {contents}"
    );
}
