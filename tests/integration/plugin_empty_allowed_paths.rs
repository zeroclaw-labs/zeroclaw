//! Integration test: empty allowed_paths means no filesystem access.
//!
//! Loads `multi_tool_plugin.wasm` WITHOUT any allowed_path mappings, calls
//! `tool_read_file`, and asserts the WASI sandbox denies the read.

use std::path::Path;

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
}

#[test]
fn read_file_fails_with_no_allowed_paths() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    // No with_allowed_path() call — the plugin has zero filesystem access.
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");

    let input = r#"{"path": "/fixtures/sample.txt"}"#;
    let result = plugin.call::<&str, &str>("tool_read_file", input);

    assert!(
        result.is_err(),
        "tool_read_file should fail when no allowed_paths are configured, \
         but succeeded with: {:?}",
        result
    );
}
