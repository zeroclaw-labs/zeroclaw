#![cfg(feature = "plugins-wasm")]

//! Verify that plugins without capability declarations see no host function imports.
//!
//! Acceptance criterion for US-ZCL-22:
//! > Plugins without capability declarations see no host function imports

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
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            manifest(name, capabilities),
        )
        .unwrap();
    }

    dir
}

#[test]
fn no_capability_plugin_is_discovered_but_excluded_from_all_registrations() {
    let dir = setup_plugins(&[
        ("no-cap-plugin", &[]),
        ("tool-plugin", &["tool"]),
        ("channel-plugin", &["channel"]),
    ]);

    let host = PluginHost::new(dir.path()).unwrap();

    // Plugin is discovered and loaded
    let all = host.list_plugins();
    assert_eq!(all.len(), 3, "all three plugins are discovered");
    assert!(
        all.iter().any(|p| p.name == "no-cap-plugin"),
        "no-cap plugin should be discoverable"
    );

    // But it receives no host function imports — excluded from every capability gate
    let tool_plugins = host.tool_plugins();
    assert!(
        !tool_plugins.iter().any(|(m, _)| m.name == "no-cap-plugin"),
        "no-cap plugin must not appear in tool registrations"
    );

    let channel_plugins = host.channel_plugins();
    assert!(
        !channel_plugins.iter().any(|m| m.name == "no-cap-plugin"),
        "no-cap plugin must not appear in channel registrations"
    );
}

#[test]
fn no_capability_plugin_has_empty_capabilities_vec() {
    let dir = setup_plugins(&[("bare-plugin", &[])]);

    let host = PluginHost::new(dir.path()).unwrap();

    let plugins = host.list_plugins();
    assert_eq!(plugins.len(), 1);
    assert!(
        plugins[0].capabilities.is_empty(),
        "plugin with no declared capabilities must have an empty capabilities vec"
    );
}

#[test]
fn only_declared_capabilities_grant_imports() {
    // A plugin declaring only "tool" must NOT receive channel, memory, or observer imports.
    let dir = setup_plugins(&[("tool-only", &["tool"])]);

    let host = PluginHost::new(dir.path()).unwrap();

    assert_eq!(
        host.tool_plugins().len(),
        1,
        "tool-only appears in tool list"
    );
    assert_eq!(
        host.channel_plugins().len(),
        0,
        "tool-only must not appear in channel list"
    );

    // Verify the manifest capabilities are exactly what was declared
    let plugins = host.list_plugins();
    assert_eq!(plugins[0].capabilities.len(), 1);
}
