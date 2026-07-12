//! Build installed WASM channel plugins into runnable [`Channel`] trait objects.
//!
//! The tool-plugin equivalent lives inline in [`crate::tools`]; this is the
//! channel side. The channel orchestrator (`zeroclaw-channels`) deliberately
//! does **not** depend on `zeroclaw-plugins` (that would pull wasmtime into the
//! channels crate), so it cannot name `WasmChannel`/`PluginHost` directly.
//! `zeroclaw-runtime` already depends on both `zeroclaw-plugins` and
//! `zeroclaw-api`, and the dependency direction is `channels → runtime`, so this
//! helper is the cycle-safe home for the wiring: the orchestrator calls it and
//! applies native-wins dedup itself (it owns the set of compiled-in channel ids).

use std::collections::HashSet;
use std::sync::Arc;

use zeroclaw_api::channel::Channel;
use zeroclaw_config::schema::Config;

/// Instantiate every unshadowed installed channel plugin as an `(id, channel)`
/// pair.
///
/// `id` is the plugin's manifest name — its channel kind key, used by the caller
/// as the dedup key against compiled-in channels and returned by the built
/// channel's `name()`. `channel` is the runnable trait object the orchestrator
/// registers and supervises exactly like a native channel (`WasmChannel::listen`
/// drives the `poll-message` bridge under the standard supervised listener).
///
/// `native_channel_ids` comes from the orchestrator's configured native
/// channels. A matching plugin is skipped before `WasmChannel::from_wasm` is
/// called, so native-wins resolution never executes a shadowed component or
/// exposes its config.
///
/// Returns an empty vec when the plugin system is disabled, the plugins
/// directory is absent, or the host fails to load. Per-plugin instantiation
/// failures are logged and skipped so one broken component cannot sink channel
/// startup. The `#[cfg(not(feature = "plugins-wasm"))]` stub below returns empty
/// for builds with no WASM engine, so the call site compiles unconditionally.
#[cfg(feature = "plugins-wasm")]
pub async fn build_channel_plugins(
    config: &Config,
    native_channel_ids: &HashSet<String>,
) -> Vec<(String, Arc<dyn Channel>)> {
    let plugin_path = config.plugins.resolved_plugins_dir();
    if !config.plugins.enabled || !plugin_path.exists() {
        return Vec::new();
    }

    let signature_mode = zeroclaw_plugins::host::PluginHost::resolve_signature_mode(
        &config.plugins.security.signature_mode,
    );
    let trusted_publisher_keys = config.plugins.security.trusted_publisher_keys.clone();
    let host = match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
        &plugin_path,
        signature_mode,
        trusted_publisher_keys,
    ) {
        Ok(host) => host,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({ "error": format!("{}", e) })),
                "Failed to load WASM channel plugins"
            );
            return Vec::new();
        }
    };

    let limits = zeroclaw_plugins::component::PluginLimits {
        call_fuel: config.plugins.limits.call_fuel,
        max_memory_bytes: config
            .plugins
            .limits
            .max_memory_mb
            .saturating_mul(1024 * 1024),
        max_table_elements: config.plugins.limits.max_table_elements,
        max_instances: config.plugins.limits.max_instances,
    };

    let mut built: Vec<(String, Arc<dyn Channel>)> = Vec::new();
    for (manifest, wasm_path) in host.channel_plugin_details() {
        if !should_build_channel_plugin(&manifest.name, native_channel_ids) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Plugin channel '{}' shadows a compiled-in channel, skipping",
                    manifest.name
                )
            );
            continue;
        }

        // Per-plugin config comes from `[[plugins.entries.<name>]]`. A plugin
        // that mirrors a built-in channel will instead resolve that channel's
        // canonical `[channels.<id>.*]` section (via the `provides` manifest
        // field) — a follow-on; a novel channel plugin's sole config home is
        // here, so this is not duplicate state.
        let plugin_config = config
            .plugins
            .entry_config(&manifest.name)
            .cloned()
            .unwrap_or_default();
        match zeroclaw_plugins::wasm_channel::WasmChannel::from_wasm(
            manifest.name.clone(),
            wasm_path,
            &manifest.permissions,
            &plugin_config,
            limits,
        )
        .await
        {
            Ok(channel) => {
                built.push((manifest.name.clone(), Arc::new(channel) as Arc<dyn Channel>))
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "plugin": manifest.name.clone(),
                            "error": format!("{}", e),
                        })),
                    "Failed to instantiate WASM channel plugin"
                );
            }
        }
    }
    built
}

/// Stub for builds without a WASM engine: channel plugins are unavailable, so no
/// channels are contributed. Keeps the orchestrator call site feature-agnostic.
#[cfg(not(feature = "plugins-wasm"))]
pub async fn build_channel_plugins(
    _config: &Config,
    _native_channel_ids: &HashSet<String>,
) -> Vec<(String, Arc<dyn Channel>)> {
    Vec::new()
}

fn should_build_channel_plugin(plugin_name: &str, native_channel_ids: &HashSet<String>) -> bool {
    !native_channel_ids.contains(plugin_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadowed_plugins_are_filtered_before_instantiation() {
        let native_channel_ids = HashSet::from(["telegram".to_string()]);
        let discovered_plugins = ["telegram", "weather-alerts"];
        let mut instantiated = Vec::new();

        for plugin_name in discovered_plugins {
            if should_build_channel_plugin(plugin_name, &native_channel_ids) {
                instantiated.push(plugin_name);
            }
        }

        assert_eq!(instantiated, ["weather-alerts"]);
    }
}
