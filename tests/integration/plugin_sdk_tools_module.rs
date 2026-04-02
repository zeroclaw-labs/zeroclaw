//! Verify that the zeroclaw-plugin-sdk tools module wraps the tool_call host
//! function.
//!
//! Acceptance criterion for US-ZCL-27:
//! > tools module wraps tool_call host function

use std::path::Path;

const SDK_DIR: &str = "crates/zeroclaw-plugin-sdk";

fn sdk_tools_source() -> String {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let tools_rs = base.join("src/tools.rs");
    assert!(
        tools_rs.is_file(),
        "zeroclaw-plugin-sdk/src/tools.rs is missing — cannot verify tools wrappers"
    );
    std::fs::read_to_string(&tools_rs).expect("failed to read tools.rs")
}

// ---------------------------------------------------------------------------
// Wrapper function existence
// ---------------------------------------------------------------------------

#[test]
fn tools_module_has_tool_call_function() {
    let src = sdk_tools_source();
    assert!(
        src.contains("fn tool_call") || src.contains("fn call_tool"),
        "tools module must expose a tool_call wrapper function"
    );
}

// ---------------------------------------------------------------------------
// Host function import — the module must reference the extern host function
// ---------------------------------------------------------------------------

#[test]
fn tools_module_imports_zeroclaw_tool_call() {
    let src = sdk_tools_source();
    assert!(
        src.contains("zeroclaw_tool_call"),
        "tools module must reference the zeroclaw_tool_call host function"
    );
}

// ---------------------------------------------------------------------------
// The wrapper should use typed request/response structs (JSON ABI)
// ---------------------------------------------------------------------------

#[test]
fn tools_module_uses_tool_call_request_struct() {
    let src = sdk_tools_source();
    assert!(
        src.contains("ToolCallRequest") || (src.contains("tool_name") && src.contains("arguments")),
        "tool_call wrapper should serialize a typed request with tool_name and arguments fields"
    );
}

#[test]
fn tools_module_uses_tool_call_response_struct() {
    let src = sdk_tools_source();
    assert!(
        src.contains("ToolCallResponse") || (src.contains("success") && src.contains("output")),
        "tool_call wrapper should deserialize a typed response with success and output fields"
    );
}

// ---------------------------------------------------------------------------
// Public API — the wrapper should be pub
// ---------------------------------------------------------------------------

#[test]
fn tools_tool_call_is_public() {
    let src = sdk_tools_source();
    assert!(
        src.contains("pub fn tool_call") || src.contains("pub fn call_tool"),
        "tool_call wrapper must be pub so plugin authors can call it"
    );
}
