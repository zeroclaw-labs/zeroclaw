#![cfg(feature = "plugins-wasm")]

//! Integration test: Verify API remove endpoint uses the same logic as CLI.
//!
//! Verifies acceptance criterion for US-ZCL-52:
//! > Endpoint invokes same removal logic as CLI
//!
//! The CLI `zeroclaw plugin remove <name>` command calls:
//!   `PluginHost::new(&workspace_dir)?.remove(&name)`
//!
//! This test verifies the API endpoint uses the same `PluginHost::remove` method
//! rather than implementing separate removal logic.

// ── Verify endpoint calls PluginHost::remove ─────────────────────────────

/// The remove_plugin handler must delegate to PluginHost::remove,
/// not implement its own removal logic.
#[test]
fn remove_endpoint_uses_plugin_host_remove_method() {
    let api_source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    // Find the remove_plugin function
    let fn_start = api_source
        .find("pub async fn remove_plugin")
        .expect("remove_plugin handler must exist in api_plugins.rs");

    // Extract the function body (until next pub fn or end of module)
    let fn_body_start = &api_source[fn_start..];
    let fn_end = fn_body_start[50..]
        .find("\n    pub ")
        .map(|i| i + 50)
        .unwrap_or(fn_body_start.len());
    let fn_body = &fn_body_start[..fn_end];

    // Verify it calls host.remove() - the same method CLI uses
    assert!(
        fn_body.contains(".remove(") || fn_body.contains("host.remove"),
        "remove_plugin endpoint must call PluginHost::remove() method, \
         the same logic used by CLI. Found function body:\n{}",
        &fn_body[..fn_body.len().min(500)]
    );
}

/// Both CLI and API must use PluginHost for plugin operations.
/// This test verifies the API creates a PluginHost instance.
#[test]
fn remove_endpoint_creates_plugin_host() {
    let api_source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = api_source
        .find("pub async fn remove_plugin")
        .expect("remove_plugin handler must exist");

    let fn_body_start = &api_source[fn_start..];
    let fn_end = fn_body_start[50..]
        .find("\n    pub ")
        .map(|i| i + 50)
        .unwrap_or(fn_body_start.len());
    let fn_body = &fn_body_start[..fn_end];

    // Must use create_plugin_host helper or PluginHost::new directly
    assert!(
        fn_body.contains("create_plugin_host") || fn_body.contains("PluginHost::new"),
        "remove_plugin must use PluginHost (same as CLI)"
    );
}

// ── Cross-reference CLI implementation ───────────────────────────────────

/// Verify CLI remove command uses PluginHost::remove as the reference implementation.
#[test]
fn cli_remove_uses_plugin_host_remove() {
    let main_source = include_str!("../../src/main.rs");

    // Find the CLI PluginCommands::Remove handler
    let remove_match = main_source
        .find("PluginCommands::Remove")
        .expect("CLI must have PluginCommands::Remove variant");

    // Get the handler body
    let handler_start = &main_source[remove_match..];
    let handler_end = handler_start
        .find("PluginCommands::")
        .filter(|&i| i > 50)
        .unwrap_or(200);
    let handler_body = &handler_start[..handler_end];

    // CLI must call host.remove(&name)
    assert!(
        handler_body.contains("host.remove"),
        "CLI remove command must call host.remove(). This is the reference \
         implementation that the API endpoint should also use."
    );
}

/// Both implementations should use the same PluginHost type.
#[test]
fn both_remove_use_same_plugin_host_type() {
    let api_source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");
    let main_source = include_str!("../../src/main.rs");

    // Both must reference the same PluginHost from crate::plugins::host
    let api_uses_host = api_source.contains("crate::plugins::host::PluginHost")
        || api_source.contains("plugins::host::PluginHost");

    let cli_uses_host = main_source.contains("zeroclaw::plugins::host::PluginHost");

    assert!(
        api_uses_host,
        "API must use crate::plugins::host::PluginHost"
    );
    assert!(
        cli_uses_host,
        "CLI must use zeroclaw::plugins::host::PluginHost"
    );
}

/// The remove logic in PluginHost removes from loaded map and deletes the directory.
/// Both CLI and API get this behavior by calling the same method.
#[test]
fn plugin_host_remove_is_shared_implementation() {
    let host_source = include_str!("../../crates/zeroclaw-plugins/src/host.rs");

    // Verify the remove method exists and handles both in-memory and disk cleanup
    let remove_fn = host_source
        .find("pub fn remove(&mut self, name: &str)")
        .expect("PluginHost::remove method must exist");

    let fn_body_start = &host_source[remove_fn..];
    let fn_end = fn_body_start
        .find("\n    pub fn")
        .unwrap_or(fn_body_start.len().min(500));
    let fn_body = &fn_body_start[..fn_end];

    // Verify it removes from loaded map
    assert!(
        fn_body.contains("self.loaded.remove"),
        "remove() must remove from loaded plugins map"
    );

    // Verify it removes the plugin directory
    assert!(
        fn_body.contains("remove_dir_all"),
        "remove() must delete the plugin directory"
    );
}
