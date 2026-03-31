//! Integration test: echo plugin JSON round-trip.
//!
//! Loads `echo_plugin.wasm` from the checked-in artifacts, calls `tool_echo`
//! with sample JSON via Extism, and asserts the output equals the input.

use std::path::Path;

const ECHO_WASM: &str = "tests/plugins/artifacts/echo_plugin.wasm";

fn echo_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(ECHO_WASM)
}

#[test]
fn echo_plugin_round_trips_simple_json() {
    let wasm_path = echo_wasm_path();
    assert!(
        wasm_path.is_file(),
        "echo_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");

    let input = r#"{"hello":"world","count":42}"#;
    let output = plugin
        .call::<&str, &str>("tool_echo", input)
        .expect("tool_echo call failed");

    let input_val: serde_json::Value = serde_json::from_str(input).unwrap();
    let output_val: serde_json::Value = serde_json::from_str(output).unwrap();
    assert_eq!(input_val, output_val, "round-trip mismatch");
}

#[test]
fn echo_plugin_round_trips_nested_json() {
    let wasm_path = echo_wasm_path();
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");

    let input = r#"{"nested":{"array":[1,2,3],"flag":true,"nothing":null},"emoji":"🦀"}"#;
    let output = plugin
        .call::<&str, &str>("tool_echo", input)
        .expect("tool_echo call failed");

    let input_val: serde_json::Value = serde_json::from_str(input).unwrap();
    let output_val: serde_json::Value = serde_json::from_str(output).unwrap();
    assert_eq!(input_val, output_val, "nested round-trip mismatch");
}

#[test]
fn echo_plugin_round_trips_empty_object() {
    let wasm_path = echo_wasm_path();
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate echo plugin");

    let input = "{}";
    let output = plugin
        .call::<&str, &str>("tool_echo", input)
        .expect("tool_echo call failed");

    let input_val: serde_json::Value = serde_json::from_str(input).unwrap();
    let output_val: serde_json::Value = serde_json::from_str(output).unwrap();
    assert_eq!(input_val, output_val, "empty object round-trip mismatch");
}
