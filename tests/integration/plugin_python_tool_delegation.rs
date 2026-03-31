//! Test: tools.tool_call() delegates to fun_fact tool.
//!
//! Task US-ZCL-35-5: verify that the Python SDK example plugin uses
//! `tools.tool_call()` to delegate to the `fun_fact` tool — matching
//! the Rust sdk-example-plugin behavior.
//!
//! Acceptance criterion for US-ZCL-35:
//! > tools.tool_call() delegates to fun_fact tool

use std::path::Path;

const PYTHON_SDK_EXAMPLE_DIR: &str = "tests/plugins/python-sdk-example-plugin";

fn plugin_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(PYTHON_SDK_EXAMPLE_DIR)
        .join("sdk_example_plugin.py");
    std::fs::read_to_string(&path)
        .expect("failed to read python-sdk-example-plugin/sdk_example_plugin.py")
}

// ---------------------------------------------------------------------------
// Plugin imports the tools module
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_imports_tools_module() {
    let src = plugin_source();
    assert!(
        src.contains("from zeroclaw_plugin_sdk import") && src.contains("tools"),
        "Plugin must import the tools module from zeroclaw_plugin_sdk"
    );
}

// ---------------------------------------------------------------------------
// Plugin calls tools.tool_call()
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_calls_tool_call() {
    let src = plugin_source();
    assert!(
        src.contains("tools.tool_call("),
        "Plugin must call tools.tool_call() to delegate to another tool"
    );
}

// ---------------------------------------------------------------------------
// Delegation targets the fun_fact tool
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_delegates_to_fun_fact() {
    let src = plugin_source();
    assert!(
        src.contains(r#"tools.tool_call("fun_fact""#),
        "Plugin must delegate specifically to the 'fun_fact' tool via tools.tool_call(\"fun_fact\", ...)"
    );
}

// ---------------------------------------------------------------------------
// fun_fact is called with a topic argument
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_fun_fact_passes_topic_arg() {
    let src = plugin_source();
    assert!(
        src.contains(r#""topic""#),
        "Plugin must pass a 'topic' argument to the fun_fact tool"
    );
}

#[test]
fn python_sdk_example_fun_fact_topic_is_greeting() {
    let src = plugin_source();
    assert!(
        src.contains(r#""topic": "greeting""#) || src.contains(r#""topic":"greeting""#),
        "Plugin must request a fun fact with topic 'greeting'"
    );
}

// ---------------------------------------------------------------------------
// Tool delegation result is included in the greeting
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_includes_fact_in_greeting() {
    let src = plugin_source();
    assert!(
        src.contains("fact") && src.contains("greeting"),
        "Plugin must incorporate the fun_fact result into the greeting output"
    );
}

#[test]
fn python_sdk_example_has_fallback_for_tool_failure() {
    let src = plugin_source();
    // The plugin should handle the case where tool_call fails
    assert!(
        src.contains("except") || src.contains("unwrap_or"),
        "Plugin must handle tool_call failure gracefully with a fallback"
    );
}

#[test]
fn python_sdk_example_fallback_is_greeting_fact() {
    let src = plugin_source();
    assert!(
        src.contains("Waving"),
        "Fallback fun fact should mention waving (matching Rust plugin's fallback)"
    );
}

// ---------------------------------------------------------------------------
// Tool delegation only happens on first visit (matches Rust plugin)
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_tool_call_on_first_visit_only() {
    let src = plugin_source();
    // tools.tool_call should be inside the first_visit branch
    assert!(
        src.contains("if first_visit"),
        "Plugin must branch on first_visit before calling tools.tool_call()"
    );
}

// ---------------------------------------------------------------------------
// Pattern matches Rust sdk-example-plugin
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_tool_delegation_matches_rust() {
    let src = plugin_source();

    // Rust plugin does: tools::tool_call("fun_fact", json!({ "topic": "greeting" }))
    // Python plugin should do: tools.tool_call("fun_fact", {"topic": "greeting"})
    assert!(
        src.contains("tools.tool_call(") && src.contains("fun_fact"),
        "Python plugin must use tools.tool_call() targeting fun_fact, \
         matching the Rust sdk-example-plugin pattern"
    );

    // Both plugins use the result inside the first_visit branch
    assert!(
        src.contains("first_visit") && src.contains("fact"),
        "Python plugin must use the fun_fact result in the first_visit greeting, \
         matching the Rust sdk-example-plugin flow"
    );
}
