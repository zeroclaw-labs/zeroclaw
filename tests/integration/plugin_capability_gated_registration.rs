//! Verify that host functions are only registered for plugins that declare
//! the corresponding capability.
//!
//! Acceptance criterion for US-ZCL-22:
//! > Host functions only registered for plugins that declare the corresponding capability

use zeroclaw::plugins::host::PluginHost;

/// Creates a plugin manifest TOML string with the given name and capabilities.
fn manifest(name: &str, capabilities: &[&str]) -> String {
    let caps: Vec<String> = capabilities.iter().map(|c| format!("\"{}\"", c)).collect();
    format!(
        r#"
name = "{name}"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = [{caps}]
"#,
        name = name,
        caps = caps.join(", ")
    )
}

/// Helper to set up a plugins directory with multiple plugins.
fn setup_plugins(specs: &[(&str, &[&str])]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let plugins_base = dir.path().join("plugins");

    for (name, capabilities) in specs {
        let plugin_dir = plugins_base.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("manifest.toml"), manifest(name, capabilities)).unwrap();
    }

    dir
}

#[test]
fn tool_capability_gates_tool_plugin_registration() {
    let dir = setup_plugins(&[
        ("tool-plugin", &["tool"]),
        ("channel-plugin", &["channel"]),
        ("observer-plugin", &["observer"]),
    ]);

    let host = PluginHost::new(dir.path()).unwrap();

    let tool_plugins = host.tool_plugins();
    assert_eq!(tool_plugins.len(), 1, "only one plugin declares 'tool' capability");
    assert_eq!(tool_plugins[0].0.name, "tool-plugin");
}

#[test]
fn channel_capability_gates_channel_plugin_registration() {
    let dir = setup_plugins(&[
        ("tool-plugin", &["tool"]),
        ("channel-plugin", &["channel"]),
        ("memory-plugin", &["memory"]),
    ]);

    let host = PluginHost::new(dir.path()).unwrap();

    let channel_plugins = host.channel_plugins();
    assert_eq!(channel_plugins.len(), 1, "only one plugin declares 'channel' capability");
    assert_eq!(channel_plugins[0].name, "channel-plugin");
}

#[test]
fn plugin_with_no_capabilities_excluded_from_all_registrations() {
    let dir = setup_plugins(&[
        ("bare-plugin", &[]),
        ("tool-plugin", &["tool"]),
    ]);

    let host = PluginHost::new(dir.path()).unwrap();

    assert_eq!(host.list_plugins().len(), 2, "both plugins are discovered");
    assert_eq!(host.tool_plugins().len(), 1, "bare plugin excluded from tool registration");
    assert_eq!(host.channel_plugins().len(), 0, "bare plugin excluded from channel registration");
}

#[test]
fn multi_capability_plugin_appears_in_all_matching_registrations() {
    let dir = setup_plugins(&[
        ("multi-plugin", &["tool", "channel"]),
        ("tool-only", &["tool"]),
    ]);

    let host = PluginHost::new(dir.path()).unwrap();

    assert_eq!(host.tool_plugins().len(), 2, "both plugins have tool capability");
    assert_eq!(host.channel_plugins().len(), 1, "only multi-plugin has channel capability");
    assert_eq!(host.channel_plugins()[0].name, "multi-plugin");
}

#[test]
fn memory_and_observer_capabilities_do_not_grant_tool_or_channel_access() {
    let dir = setup_plugins(&[
        ("memory-plugin", &["memory"]),
        ("observer-plugin", &["observer"]),
    ]);

    let host = PluginHost::new(dir.path()).unwrap();

    assert_eq!(host.list_plugins().len(), 2, "both plugins are loaded");
    assert_eq!(host.tool_plugins().len(), 0, "no tool-capable plugins");
    assert_eq!(host.channel_plugins().len(), 0, "no channel-capable plugins");
}
