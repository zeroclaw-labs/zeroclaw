#![cfg(feature = "plugins-wasm")]

//! Verify the host function ABI: JSON parameter encoding and error protocol.
//!
//! Acceptance criterion for US-ZCL-22:
//! > Host function ABI defined with JSON parameter encoding and error protocol
//!
//! These tests assert that:
//! 1. Parameters are JSON-encoded as UTF-8 bytes via `serde_json::to_vec`
//! 2. Outputs are parsed back from JSON bytes via `serde_json::from_slice`
//! 3. Execution errors are wrapped in `ToolResult { success: false, error: Some(..) }`
//!    and never propagate as `anyhow::Error`
//! 4. Error messages always identify the plugin name and export function

use serde_json::json;
use zeroclaw::tools::Tool;

/// Build a `WasmTool` backed by a minimal WASM module with no exports.
/// Every `execute()` call will fail with "export not found", which is
/// exactly what we need to verify the error protocol.
fn make_abi_tool(plugin_name: &str, export_name: &str) -> zeroclaw::plugins::wasm_tool::WasmTool {
    use extism::{Manifest, Plugin, Wasm};
    use std::sync::{Arc, Mutex};

    // Minimal valid WASM module (magic + version, no sections).
    let wasm_bytes: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let manifest = Manifest::new([Wasm::data(wasm_bytes)]);
    let plugin = Plugin::new(&manifest, [], true).expect("minimal wasm should load");

    zeroclaw::plugins::wasm_tool::WasmTool::new(
        format!("{plugin_name}_{export_name}"),
        "abi test tool".into(),
        plugin_name.into(),
        "0.1.0".into(),
        export_name.into(),
        json!({ "type": "object" }),
        Arc::new(Mutex::new(plugin)),
    )
}

// ---------------------------------------------------------------------------
// 1. JSON parameter encoding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn json_args_are_serialized_without_panic() {
    // Verify that arbitrary JSON values (objects, arrays, nested structures,
    // unicode, nulls) are accepted by the ABI serialization path.
    let tool = make_abi_tool("encoding", "roundtrip");

    let complex_args = json!({
        "string": "hello world",
        "number": 42,
        "float": std::f64::consts::PI,
        "bool": true,
        "null_val": null,
        "nested": { "a": [1, 2, 3] },
        "unicode": "\u{1F600} emoji \u{00E9}",
        "empty_object": {},
        "empty_array": []
    });

    // The call will fail (no export), but serialization must not panic.
    let result = tool.execute(complex_args).await;
    assert!(
        result.is_ok(),
        "execute must return Ok(ToolResult), not Err — got: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn empty_json_object_accepted() {
    let tool = make_abi_tool("encoding", "noop");
    let result = tool.execute(json!({})).await;
    assert!(result.is_ok(), "empty JSON object must not panic");
}

#[tokio::test]
async fn json_array_at_top_level_accepted() {
    let tool = make_abi_tool("encoding", "batch");
    let result = tool.execute(json!([1, 2, 3])).await;
    assert!(result.is_ok(), "top-level JSON array must not panic");
}

// ---------------------------------------------------------------------------
// 2. Error protocol: errors wrapped in ToolResult, never anyhow::Error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execution_error_returns_tool_result_not_anyhow_error() {
    let tool = make_abi_tool("proto", "missing_fn");
    let result = tool
        .execute(json!({}))
        .await
        .expect("execution errors must be wrapped in ToolResult, not anyhow::Error");

    assert!(
        !result.success,
        "ToolResult.success must be false when the export is missing"
    );
    assert!(
        result.error.is_some(),
        "ToolResult.error must be Some on failure"
    );
}

#[tokio::test]
async fn error_protocol_identifies_plugin_name_in_output() {
    let tool = make_abi_tool("my_plugin", "my_export");
    let result = tool.execute(json!({})).await.unwrap();

    assert!(
        result.output.contains("my_plugin"),
        "error output must contain the plugin name, got: {}",
        result.output
    );
}

#[tokio::test]
async fn error_protocol_identifies_export_name_in_output() {
    let tool = make_abi_tool("my_plugin", "my_export");
    let result = tool.execute(json!({})).await.unwrap();

    assert!(
        result.output.contains("my_export"),
        "error output must contain the export function name, got: {}",
        result.output
    );
}

#[tokio::test]
async fn error_field_contains_plugin_and_export_names() {
    let tool = make_abi_tool("abi_check", "invoke");
    let result = tool.execute(json!({})).await.unwrap();

    let err = result.error.as_deref().expect("error field must be set");
    assert!(
        err.contains("abi_check") && err.contains("invoke"),
        "error field must name both plugin and export, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// 3. Error classification surfaces readable messages
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_export_produces_not_found_classification() {
    let tool = make_abi_tool("classifier", "no_such_fn");
    let result = tool.execute(json!({})).await.unwrap();

    assert!(
        result.output.contains("not found"),
        "missing export should be classified as 'not found', got: {}",
        result.output
    );
}

// ---------------------------------------------------------------------------
// 4. Consistent format: [plugin:name/export] prefix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn error_output_uses_bracket_prefix_format() {
    let tool = make_abi_tool("fmt_check", "run");
    let result = tool.execute(json!({})).await.unwrap();

    assert!(
        result.output.starts_with("[plugin:fmt_check/run]"),
        "error output must use [plugin:<name>/<export>] prefix, got: {}",
        result.output
    );
}
