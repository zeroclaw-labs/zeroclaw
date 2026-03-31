//! Verify that the Python echo plugin source correctly defines an echo tool
//! that accepts JSON input and returns it unchanged.
//!
//! Acceptance criterion for US-ZCL-34:
//! > Echo tool accepts JSON input and returns it unchanged
//!
//! This test validates the source-level contract: the plugin declares a
//! `tool_echo` export with an open JSON object schema, and the Python
//! implementation returns its input unmodified.

use std::path::Path;

/// The plugin manifest must declare tool_echo with an open object schema,
/// meaning any JSON input is accepted.
#[test]
fn python_echo_plugin_accepts_json_input_via_schema() {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins/python-echo-plugin/plugin.toml");
    let content = std::fs::read_to_string(&manifest_path)
        .expect("python-echo-plugin/plugin.toml should exist");

    let manifest =
        zeroclaw::plugins::PluginManifest::parse(&content).expect("manifest should parse");

    let tool = &manifest.tools[0];
    assert_eq!(tool.name, "tool_echo", "tool must be named tool_echo");
    assert_eq!(tool.export, "tool_echo", "export must match function name");

    // The schema must accept any JSON object (type: "object" with no required fields)
    let schema = tool
        .parameters_schema
        .as_ref()
        .expect("schema must be present");
    let schema_type = schema.get("type").and_then(|v| v.as_str());
    assert_eq!(
        schema_type,
        Some("object"),
        "parameters_schema must accept JSON objects"
    );
    // No required fields — any JSON object is valid input
    assert!(
        schema.get("required").is_none(),
        "schema should not restrict required fields — echo accepts any JSON"
    );
}

/// The Python source must define tool_echo as a pure echo: `return input`.
/// We verify this by inspecting the source for the identity return pattern.
#[test]
fn python_echo_source_returns_input_unchanged() {
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/python-echo-plugin/echo_plugin.py");
    let source = std::fs::read_to_string(&source_path).expect("echo_plugin.py should exist");

    // Must use the @plugin_fn decorator (handles JSON marshalling)
    assert!(
        source.contains("@plugin_fn"),
        "echo plugin must use @plugin_fn decorator for JSON marshalling"
    );

    // Must define tool_echo function
    assert!(
        source.contains("def tool_echo("),
        "echo plugin must define tool_echo function"
    );

    // The function body must return input unchanged — the identity pattern
    assert!(
        source.contains("return input"),
        "tool_echo must return input unchanged (identity function)"
    );
}

/// The echo plugin must NOT transform, filter, or wrap the input.
/// Verify the source has no JSON manipulation between receive and return.
#[test]
fn python_echo_source_has_no_input_transformation() {
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/python-echo-plugin/echo_plugin.py");
    let source = std::fs::read_to_string(&source_path).expect("echo_plugin.py should exist");

    // Extract the function body (everything after def tool_echo)
    let fn_start = source
        .find("def tool_echo(")
        .expect("tool_echo function must exist");
    let fn_body = &source[fn_start..];

    // Should not contain mutation operations on input
    for forbidden in &[
        "input[",     // indexing/modifying
        "input.pop",  // removing keys
        "del input",  // deleting keys
        ".update(",   // merging data
        "json.dumps", // re-serializing (decorator handles this)
        "json.loads", // re-parsing (decorator handles this)
    ] {
        assert!(
            !fn_body.contains(forbidden),
            "tool_echo must not transform input — found '{}'",
            forbidden
        );
    }
}
