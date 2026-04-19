#![cfg(feature = "plugins-wasm")]

//! Integration test: Verify API install endpoint uses the same logic as CLI.
//!
//! Verifies acceptance criterion for US-ZCL-51:
//! > Endpoint invokes the same install logic as CLI
//!
//! The CLI `zeroclaw plugin install <source>` command calls:
//!   `PluginHost::new(&workspace_dir)?.install(&source)`
//!
//! This test verifies the API endpoint uses the same `PluginHost::install` method
//! rather than implementing separate install logic.

// ── Verify endpoint calls PluginHost::install ────────────────────────────

/// The install_plugin handler must delegate to PluginHost::install,
/// not implement its own install logic.
#[test]
fn install_endpoint_uses_plugin_host_install_method() {
    let api_source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    // Find the install_plugin function
    let fn_start = api_source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler must exist in api_plugins.rs");

    // Extract the function body (until next pub fn or end of module)
    let fn_body_start = &api_source[fn_start..];
    let fn_end = fn_body_start[50..]
        .find("\n    pub ")
        .map(|i| i + 50)
        .unwrap_or(fn_body_start.len());
    let fn_body = &fn_body_start[..fn_end];

    // Verify it calls host.install() - the same method CLI uses
    assert!(
        fn_body.contains(".install(") || fn_body.contains("host.install"),
        "install_plugin endpoint must call PluginHost::install() method, \
         the same logic used by CLI. Found function body:\n{}",
        &fn_body[..fn_body.len().min(500)]
    );
}

/// Both CLI and API must use PluginHost for plugin operations.
/// This test verifies the API creates a PluginHost instance.
#[test]
fn install_endpoint_creates_plugin_host() {
    let api_source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = api_source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler must exist");

    let fn_body_start = &api_source[fn_start..];
    let fn_end = fn_body_start[50..]
        .find("\n    pub ")
        .map(|i| i + 50)
        .unwrap_or(fn_body_start.len());
    let fn_body = &fn_body_start[..fn_end];

    // Must use create_plugin_host helper or PluginHost::new directly
    assert!(
        fn_body.contains("create_plugin_host") || fn_body.contains("PluginHost::new"),
        "install_plugin must use PluginHost (same as CLI)"
    );
}

// ── Cross-reference CLI implementation ───────────────────────────────────

/// Verify CLI install command uses PluginHost::install as the reference implementation.
#[test]
fn cli_install_uses_plugin_host_install() {
    let main_source = include_str!("../../src/main.rs");

    // Find the CLI PluginCommands::Install handler
    let install_match = main_source
        .find("PluginCommands::Install")
        .expect("CLI must have PluginCommands::Install variant");

    // Get the handler body
    let handler_start = &main_source[install_match..];
    let handler_end = handler_start
        .find("PluginCommands::")
        .filter(|&i| i > 50)
        .unwrap_or(200);
    let handler_body = &handler_start[..handler_end];

    // CLI must call host.install(&source)
    assert!(
        handler_body.contains("host.install"),
        "CLI install command must call host.install(). This is the reference \
         implementation that the API endpoint should also use."
    );
}

/// Both implementations should use the same PluginHost type.
#[test]
fn both_use_same_plugin_host_type() {
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
