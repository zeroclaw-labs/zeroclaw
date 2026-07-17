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

use parking_lot::RwLock;
use zeroclaw_api::channel::Channel;
use zeroclaw_config::schema::Config;
#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::wasm_channel::SenderAuthorizer;

#[cfg(feature = "plugins-wasm")]
fn load_plugin_host(config: &Config) -> Option<zeroclaw_plugins::host::PluginHost> {
    let plugin_path = config.plugins.resolved_plugins_dir();
    if !config.plugins.enabled || !plugin_path.exists() {
        return None;
    }

    let signature_mode = zeroclaw_plugins::host::PluginHost::resolve_signature_mode(
        &config.plugins.security.signature_mode,
    );
    let trusted_publisher_keys = config.plugins.security.trusted_publisher_keys.clone();
    match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
        &plugin_path,
        signature_mode,
        trusted_publisher_keys,
    ) {
        Ok(host) => Some(host),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({ "error": format!("{}", e) })),
                "Failed to load WASM channel plugins"
            );
            None
        }
    }
}

/// Return whether canonical plugin configuration and installed manifests expose
/// at least one WASM-backed channel for the daemon to supervise.
#[cfg(feature = "plugins-wasm")]
#[must_use]
pub(crate) fn has_channel_plugins(config: &Config) -> bool {
    let active = zeroclaw_config::schema::ActiveChannelAliases::compute(config);
    load_plugin_host(config).is_some_and(|host| {
        host.channel_plugin_details().iter().any(|(manifest, _)| {
            active.contains(&zeroclaw_api::channel::plugin_channel_ref(&manifest.name))
        })
    })
}

/// Builds without a WASM host cannot contribute plugin channels.
#[cfg(not(feature = "plugins-wasm"))]
#[must_use]
pub(crate) fn has_channel_plugins(_config: &Config) -> bool {
    false
}

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
    config: &Arc<RwLock<Config>>,
    native_channel_ids: &HashSet<String>,
) -> Vec<(String, Arc<dyn Channel>)> {
    // Build-time plugin settings are a per-call materialized view. The sender
    // authorizer below retains the shared handle and resolves peer membership
    // from it on every message.
    let config_handle = Arc::clone(config);
    let config = config.read().clone();

    let Some(host) = load_plugin_host(&config) else {
        return Vec::new();
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
    let active = zeroclaw_config::schema::ActiveChannelAliases::compute(&config);
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

        let binding = zeroclaw_api::channel::plugin_channel_ref(&manifest.name);
        if !active.contains(&binding) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "plugin": manifest.name.clone(),
                        "binding": binding,
                    })),
                "Channel plugin has no enabled owning agent; skipping before instantiation"
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
        let authorizer = channel_authorizer(&config_handle, &manifest.name, "");
        note_if_no_allowlist(&config, &manifest.name, "", &manifest.name);
        match zeroclaw_plugins::wasm_channel::WasmChannel::from_wasm_with_digest(
            manifest.name.clone(),
            &wasm_path,
            manifest.wasm_sha256.as_deref(),
            &manifest.permissions,
            &plugin_config,
            limits,
            authorizer,
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
    _config: &Arc<RwLock<Config>>,
    _native_channel_ids: &HashSet<String>,
) -> Vec<(String, Arc<dyn Channel>)> {
    Vec::new()
}

/// Build a default-deny sender gate for one plugin channel.
///
/// Novel plugins in this lower stack use exact sender identities. Mirror
/// plugins add their platform-specific matcher when the `provides` contract is
/// introduced. In both cases the peer list is resolved live from the same
/// `Config::peer_groups` state native channels consult.
#[cfg(feature = "plugins-wasm")]
fn channel_authorizer(
    config: &Arc<RwLock<Config>>,
    channel_type: &str,
    alias: &str,
) -> SenderAuthorizer {
    let config = Arc::clone(config);
    let channel_type = channel_type.to_string();
    let alias = alias.to_string();

    Arc::new(move |sender: &str| {
        let peers = config.read().channel_external_peers(&channel_type, &alias);
        zeroclaw_config::allowlist::is_user_allowed(
            &peers,
            sender,
            zeroclaw_config::allowlist::Match::Sensitive,
        )
    })
}

/// Surface the default-deny state once at startup while leaving the actual
/// decision to the live resolver.
#[cfg(feature = "plugins-wasm")]
fn note_if_no_allowlist(config: &Config, channel_type: &str, alias: &str, plugin: &str) {
    if config
        .channel_external_peers(channel_type, alias)
        .is_empty()
    {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "plugin": plugin,
                    "channel": channel_type,
                    "alias": alias,
                })
            ),
            "Channel plugin has an empty sender allowlist; it will accept no inbound until a peer group authorizes senders (or \"*\" for anyone)"
        );
    }
}

#[cfg(any(test, feature = "plugins-wasm"))]
fn should_build_channel_plugin(plugin_name: &str, native_channel_ids: &HashSet<String>) -> bool {
    !native_channel_ids.contains(plugin_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "plugins-wasm")]
    use zeroclaw_config::multi_agent::PeerGroupConfig;

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn sender_authorizer_resolves_peer_groups_live() {
        let config = Arc::new(RwLock::new(Config::default()));
        let authorizer = channel_authorizer(&config, "fixture", "");

        assert!(!authorizer("tester"), "empty peer groups deny by default");

        config.write().peer_groups.insert(
            "fixture-peers".to_string(),
            PeerGroupConfig {
                channel: "fixture".into(),
                external_peers: vec!["tester".into()],
                ..Default::default()
            },
        );
        assert!(authorizer("tester"), "new canonical peer is visible live");

        config
            .write()
            .peer_groups
            .get_mut("fixture-peers")
            .expect("fixture peer group")
            .external_peers = vec!["someone-else".into()];
        assert!(
            !authorizer("tester"),
            "removing a canonical peer takes effect without rebuilding the channel"
        );

        config
            .write()
            .peer_groups
            .get_mut("fixture-peers")
            .expect("fixture peer group")
            .external_peers = vec!["*".into()];
        assert!(authorizer("tester"), "wildcard uses shared semantics");
    }

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
