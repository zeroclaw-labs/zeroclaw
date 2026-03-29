//! Integration test: multi-tool plugin registers 5 distinct tools from one module.
//!
//! Parses the real `multi-tool-plugin/plugin.toml` manifest and verifies that
//! all 5 tool definitions are present with correct names, descriptions, and
//! parameter schemas.

use std::path::Path;

use zeroclaw::plugins::PluginManifest;

const MANIFEST_PATH: &str = "tests/plugins/multi-tool-plugin/plugin.toml";

fn load_manifest() -> PluginManifest {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_PATH);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    PluginManifest::parse(&content)
        .unwrap_or_else(|e| panic!("failed to parse manifest: {e}"))
}

#[test]
fn multi_tool_manifest_declares_five_tools() {
    let manifest = load_manifest();
    assert_eq!(
        manifest.tools.len(),
        5,
        "multi-tool-plugin should declare exactly 5 tools, got: {:?}",
        manifest.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

#[test]
fn multi_tool_names_are_correct() {
    let manifest = load_manifest();
    let names: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();

    let expected = [
        "tool_add",
        "tool_reverse_string",
        "tool_lookup_config",
        "tool_http_get",
        "tool_read_file",
    ];
    for name in &expected {
        assert!(
            names.contains(name),
            "expected tool '{name}' not found in manifest, got: {names:?}"
        );
    }
}

#[test]
fn multi_tool_descriptions_are_non_empty() {
    let manifest = load_manifest();
    for tool in &manifest.tools {
        assert!(
            !tool.description.is_empty(),
            "tool '{}' has an empty description",
            tool.name
        );
    }
}

#[test]
fn multi_tool_exports_match_names() {
    let manifest = load_manifest();
    for tool in &manifest.tools {
        assert_eq!(
            tool.name, tool.export,
            "tool '{}' export should match its name, got export '{}'",
            tool.name, tool.export
        );
    }
}

#[test]
fn tool_add_has_correct_parameter_schema() {
    let manifest = load_manifest();
    let tool = manifest.tools.iter().find(|t| t.name == "tool_add")
        .expect("tool_add not found");

    let schema = tool.parameters_schema.as_ref().expect("tool_add should have a parameters_schema");
    assert_eq!(schema["type"], "object");

    let props = schema["properties"].as_object()
        .expect("schema should have properties");
    assert!(props.contains_key("a"), "tool_add should have parameter 'a'");
    assert!(props.contains_key("b"), "tool_add should have parameter 'b'");

    let required = schema["required"].as_array()
        .expect("schema should have required array");
    let req_strs: Vec<&str> = required.iter().filter_map(serde_json::Value::as_str).collect();
    assert!(req_strs.contains(&"a"), "parameter 'a' should be required");
    assert!(req_strs.contains(&"b"), "parameter 'b' should be required");
}

#[test]
fn tool_reverse_string_has_correct_parameter_schema() {
    let manifest = load_manifest();
    let tool = manifest.tools.iter().find(|t| t.name == "tool_reverse_string")
        .expect("tool_reverse_string not found");

    let schema = tool.parameters_schema.as_ref().expect("should have schema");
    let props = schema["properties"].as_object()
        .expect("schema should have properties");
    assert!(props.contains_key("text"), "tool_reverse_string should have parameter 'text'");

    let required = schema["required"].as_array()
        .expect("schema should have required array");
    let req_strs: Vec<&str> = required.iter().filter_map(serde_json::Value::as_str).collect();
    assert!(req_strs.contains(&"text"), "parameter 'text' should be required");
}

#[test]
fn tool_lookup_config_has_minimal_schema() {
    let manifest = load_manifest();
    let tool = manifest.tools.iter().find(|t| t.name == "tool_lookup_config")
        .expect("tool_lookup_config not found");

    let schema = tool.parameters_schema.as_ref().expect("should have schema");
    assert_eq!(schema["type"], "object");
}

#[test]
fn tool_http_get_has_url_parameter() {
    let manifest = load_manifest();
    let tool = manifest.tools.iter().find(|t| t.name == "tool_http_get")
        .expect("tool_http_get not found");

    let schema = tool.parameters_schema.as_ref().expect("should have schema");
    let props = schema["properties"].as_object()
        .expect("schema should have properties");
    assert!(props.contains_key("url"), "tool_http_get should have parameter 'url'");

    let required = schema["required"].as_array()
        .expect("schema should have required array");
    let req_strs: Vec<&str> = required.iter().filter_map(serde_json::Value::as_str).collect();
    assert!(req_strs.contains(&"url"), "parameter 'url' should be required");
}

#[test]
fn tool_read_file_has_path_parameter() {
    let manifest = load_manifest();
    let tool = manifest.tools.iter().find(|t| t.name == "tool_read_file")
        .expect("tool_read_file not found");

    let schema = tool.parameters_schema.as_ref().expect("should have schema");
    let props = schema["properties"].as_object()
        .expect("schema should have properties");
    assert!(props.contains_key("path"), "tool_read_file should have parameter 'path'");

    let required = schema["required"].as_array()
        .expect("schema should have required array");
    let req_strs: Vec<&str> = required.iter().filter_map(serde_json::Value::as_str).collect();
    assert!(req_strs.contains(&"path"), "parameter 'path' should be required");
}

#[test]
fn each_tool_name_is_unique() {
    let manifest = load_manifest();
    let names: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    for name in &names {
        assert!(seen.insert(name), "duplicate tool name: {name}");
    }
}
