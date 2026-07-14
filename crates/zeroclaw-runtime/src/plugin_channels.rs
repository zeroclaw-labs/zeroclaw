//! Build installed WASM channel plugins into runnable [`Channel`] trait objects.
//!
//! The channel orchestrator deliberately does not depend on
//! `zeroclaw-plugins`, so this runtime module owns component construction. It
//! derives mirror instances directly from canonical channel config and applies
//! native collision and enabled-agent ownership gates before guest startup.

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
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{error:#}") })),
                "Failed to load WASM channel plugins"
            );
            None
        }
    }
}

#[cfg(feature = "plugins-wasm")]
fn canonical_channels_json(config: &Config) -> Option<serde_json::Value> {
    match serde_json::to_value(&config.channels) {
        Ok(value) => Some(value),
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{error:#}") })),
                "Failed to materialize canonical channel config for WASM mirrors"
            );
            None
        }
    }
}

#[cfg(feature = "plugins-wasm")]
fn mirror_sections<'a>(
    channels_json: &'a serde_json::Value,
    channel_type: &str,
) -> impl Iterator<Item = (&'a String, &'a serde_json::Value)> {
    channels_json
        .get(channel_type)
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|aliases| aliases.iter())
        .filter(|(_, section)| {
            section
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
}

/// Materialize the effective typed config passed to one channel mirror.
///
/// Schema-derived environment overrides are already present in [`Config`].
/// Slack and LINE additionally support legacy direct environment contracts in
/// their native constructors, so those exact credential fields are resolved
/// through the same typed helpers here. The returned view exists only for this
/// guest startup call; canonical state remains in `config.channels`.
#[cfg(feature = "plugins-wasm")]
fn resolve_mirror_config(
    config: &Config,
    channel_type: &str,
    alias: &str,
    section: &serde_json::Value,
) -> serde_json::Value {
    let mut resolved = section.clone();
    let Some(fields) = resolved.as_object_mut() else {
        return resolved;
    };

    match channel_type {
        "slack" => {
            let Some(slack) = config.channels.slack.get(alias) else {
                return resolved;
            };
            fields.insert(
                "bot_token".to_string(),
                slack
                    .resolved_bot_token()
                    .map_or(serde_json::Value::Null, serde_json::Value::String),
            );
            fields.insert(
                "app_token".to_string(),
                slack
                    .resolved_app_token()
                    .map_or(serde_json::Value::Null, serde_json::Value::String),
            );
        }
        "line" => {
            let Some(line) = config.channels.line.get(alias) else {
                return resolved;
            };
            fields.insert(
                "channel_access_token".to_string(),
                serde_json::Value::String(line.resolved_channel_access_token()),
            );
            fields.insert(
                "channel_secret".to_string(),
                serde_json::Value::String(line.resolved_channel_secret()),
            );
        }
        _ => {}
    }

    resolved
}

#[cfg(feature = "plugins-wasm")]
fn ambiguous_mirror_types<'a>(
    manifests: impl IntoIterator<Item = &'a zeroclaw_plugins::PluginManifest>,
) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut ambiguous = HashSet::new();
    for channel_type in manifests
        .into_iter()
        .filter_map(|manifest| manifest.provides.as_deref())
    {
        if !seen.insert(channel_type) {
            ambiguous.insert(channel_type);
        }
    }
    ambiguous
}

/// Return whether installed manifests expose an owned channel that the daemon
/// may need to supervise.
#[cfg(feature = "plugins-wasm")]
#[must_use]
pub(crate) fn has_channel_plugins(config: &Config) -> bool {
    let active = zeroclaw_config::schema::ActiveChannelAliases::compute(config);
    let channels_json = canonical_channels_json(config);
    load_plugin_host(config).is_some_and(|host| {
        let details = host.channel_plugin_details();
        let ambiguous = ambiguous_mirror_types(details.iter().map(|(manifest, _)| *manifest));
        details
            .iter()
            .any(|(manifest, _)| match manifest.provides.as_deref() {
                Some(channel_type) => {
                    !ambiguous.contains(channel_type)
                        && manifest
                            .permissions
                            .contains(&zeroclaw_plugins::PluginPermission::ConfigRead)
                        && channels_json.as_ref().is_some_and(|sections| {
                            mirror_sections(sections, channel_type).any(|(alias, _)| {
                                active.contains(&format!("{channel_type}.{alias}"))
                            })
                        })
                }
                None => active.contains(&zeroclaw_api::channel::plugin_channel_ref(&manifest.name)),
            })
    })
}

/// Builds without a WASM host cannot contribute plugin channels.
#[cfg(not(feature = "plugins-wasm"))]
#[must_use]
pub(crate) fn has_channel_plugins(_config: &Config) -> bool {
    false
}

/// Instantiate every owned channel plugin that does not collide with an active
/// native channel.
///
/// A manifest with `provides = "<channel-type>"` creates one mirror per enabled
/// canonical `[channels.<type>.<alias>]` section. A manifest without `provides`
/// creates one novel `plugin.<manifest-name>` channel from its plugin entry.
/// Every ownership and collision decision happens before any guest export.
#[cfg(feature = "plugins-wasm")]
pub async fn build_channel_plugins(
    config: &Arc<RwLock<Config>>,
    occupied_channel_keys: &HashSet<String>,
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
    let active = zeroclaw_config::schema::ActiveChannelAliases::compute(&config);
    let channels_json = canonical_channels_json(&config);
    let mut claimed_channel_keys = occupied_channel_keys.clone();
    let mut built: Vec<(String, Arc<dyn Channel>)> = Vec::new();
    let plugin_details = host.channel_plugin_details();
    let ambiguous = ambiguous_mirror_types(plugin_details.iter().map(|(manifest, _)| *manifest));

    for (manifest, wasm_path) in plugin_details {
        match manifest.provides.as_deref() {
            Some(channel_type) => {
                if ambiguous.contains(channel_type) {
                    log_skipped_manifest(
                        &manifest.name,
                        "multiple installed plugins provide the same channel type",
                    );
                    continue;
                }
                if !manifest
                    .permissions
                    .contains(&zeroclaw_plugins::PluginPermission::ConfigRead)
                {
                    log_skipped_manifest(
                        &manifest.name,
                        "mirror channel plugin requires config_read",
                    );
                    continue;
                }
                let Some(sections) = channels_json.as_ref() else {
                    continue;
                };
                if sections
                    .get(channel_type)
                    .and_then(serde_json::Value::as_object)
                    .is_none()
                {
                    log_skipped_manifest(&manifest.name, "provides names an unknown channel type");
                    continue;
                }

                for (alias, section) in mirror_sections(sections, channel_type) {
                    let channel_key = format!("{channel_type}.{alias}");
                    if !active.contains(&channel_key) {
                        log_unowned_plugin(&manifest.name, &channel_key).await;
                        continue;
                    }
                    if !channel_key_is_available(&channel_key, &claimed_channel_keys) {
                        log_shadowed_plugin(&manifest.name, &channel_key);
                        continue;
                    }
                    let resolved = resolve_mirror_config(&config, channel_type, alias, section);
                    let config_json = match serde_json::to_string(&resolved) {
                        Ok(json) => json,
                        Err(error) => {
                            log_instantiate_failure(&manifest.name, &error.into());
                            continue;
                        }
                    };
                    let authorizer = channel_authorizer(
                        &config_handle,
                        channel_type,
                        alias,
                        manifest.sender_match,
                    );
                    note_if_no_allowlist(&config, channel_type, alias, &manifest.name);
                    match zeroclaw_plugins::wasm_channel::WasmChannel::from_wasm_mirror_with_digest(
                        channel_type,
                        alias.as_str(),
                        &wasm_path,
                        manifest.wasm_sha256.as_deref(),
                        &manifest.permissions,
                        &config_json,
                        limits,
                        authorizer,
                    )
                    .await
                    {
                        Ok(channel) => {
                            claimed_channel_keys.insert(channel_key);
                            built.push((alias.clone(), Arc::new(channel)));
                        }
                        Err(error) => log_instantiate_failure(&manifest.name, &error),
                    }
                }
            }
            None => {
                let binding = zeroclaw_api::channel::plugin_channel_ref(&manifest.name);
                if !active.contains(&binding) {
                    log_unowned_plugin(&manifest.name, &binding).await;
                    continue;
                }
                if !channel_key_is_available(&manifest.name, &claimed_channel_keys) {
                    log_shadowed_plugin(&manifest.name, &manifest.name);
                    continue;
                }
                let plugin_config = config
                    .plugins
                    .entry_config(&manifest.name)
                    .cloned()
                    .unwrap_or_default();
                let authorizer =
                    channel_authorizer(&config_handle, &manifest.name, "", manifest.sender_match);
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
                        claimed_channel_keys.insert(manifest.name.clone());
                        built.push((manifest.name.clone(), Arc::new(channel)));
                    }
                    Err(error) => log_instantiate_failure(&manifest.name, &error),
                }
            }
        }
    }
    built
}

#[cfg(any(test, feature = "plugins-wasm"))]
fn channel_key_is_available(channel_key: &str, claimed_channel_keys: &HashSet<String>) -> bool {
    !claimed_channel_keys.contains(channel_key)
}

#[cfg(feature = "plugins-wasm")]
fn log_shadowed_plugin(plugin: &str, channel_key: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "plugin": plugin,
                "channel_key": channel_key,
            })),
        "Channel plugin shadows an active native channel; skipping before instantiation"
    );
}

#[cfg(feature = "plugins-wasm")]
async fn log_unowned_plugin(plugin: &str, binding: &str) {
    ::zeroclaw_log::scope!(
        category: "channel",
        channel: binding,
        => async {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "plugin": plugin,
                        "binding": binding,
                    })),
                "Channel plugin has no enabled owning agent; skipping before instantiation"
            );
        }
    )
    .await;
}

#[cfg(feature = "plugins-wasm")]
fn log_skipped_manifest(plugin: &str, reason: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "plugin": plugin,
                "reason": reason,
            })),
        "Skipping invalid channel mirror manifest"
    );
}

#[cfg(feature = "plugins-wasm")]
fn log_instantiate_failure(plugin: &str, error: &anyhow::Error) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "plugin": plugin,
                "error": format!("{error:#}"),
            })),
        "Failed to instantiate WASM channel plugin"
    );
}

/// Stub for builds without a WASM engine.
#[cfg(not(feature = "plugins-wasm"))]
pub async fn build_channel_plugins(
    _config: &Arc<RwLock<Config>>,
    _occupied_channel_keys: &HashSet<String>,
) -> Vec<(String, Arc<dyn Channel>)> {
    Vec::new()
}

/// Apply a manifest-declared sender representation to a freshly-resolved peer
/// list. Matching primitives live in `zeroclaw-config`; the manifest is the
/// only source of truth for which representation a guest emits.
#[cfg(feature = "plugins-wasm")]
fn sender_allowed(
    sender_match: zeroclaw_plugins::SenderMatch,
    peers: &[String],
    sender: &str,
) -> bool {
    use zeroclaw_config::allowlist::{self, Match};

    match sender_match {
        zeroclaw_plugins::SenderMatch::Exact => {
            allowlist::is_user_allowed(peers, sender, Match::Sensitive)
        }
        zeroclaw_plugins::SenderMatch::CaseInsensitive => {
            allowlist::is_user_allowed(peers, sender, Match::CaseInsensitive)
        }
        zeroclaw_plugins::SenderMatch::Handle => {
            allowlist::is_user_allowed_by(peers, sender, allowlist::handle_match)
        }
        zeroclaw_plugins::SenderMatch::Email => {
            allowlist::is_user_allowed_by(peers, sender, allowlist::email_match)
        }
    }
}

/// Build a default-deny sender gate for one plugin channel.
///
/// Both novel and mirror plugins declare how their guest represents `sender`
/// in `PluginManifest::sender_match`. Peer membership is resolved live from the
/// same `Config::peer_groups` state native channels consult.
#[cfg(feature = "plugins-wasm")]
fn channel_authorizer(
    config: &Arc<RwLock<Config>>,
    channel_type: &str,
    alias: &str,
    sender_match: zeroclaw_plugins::SenderMatch,
) -> SenderAuthorizer {
    let config = Arc::clone(config);
    let channel_type = channel_type.to_string();
    let alias = alias.to_string();

    Arc::new(move |sender: &str| {
        let peers = config.read().channel_external_peers(&channel_type, &alias);
        sender_allowed(sender_match, &peers, sender)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "plugins-wasm")]
    use zeroclaw_config::{
        multi_agent::PeerGroupConfig,
        schema::{LineConfig, SlackConfig, TelegramConfig},
    };

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn sender_authorizer_resolves_peer_groups_live() {
        let config = Arc::new(RwLock::new(Config::default()));
        let authorizer =
            channel_authorizer(&config, "fixture", "", zeroclaw_plugins::SenderMatch::Exact);

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

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn manifest_sender_match_selects_shared_identity_semantics() {
        use zeroclaw_plugins::SenderMatch;

        let peers = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| (*value).to_string()).collect()
        };

        assert!(sender_allowed(
            SenderMatch::Exact,
            &peers(&["alice"]),
            "alice"
        ));
        assert!(!sender_allowed(
            SenderMatch::Exact,
            &peers(&["alice"]),
            "Alice"
        ));
        assert!(sender_allowed(
            SenderMatch::CaseInsensitive,
            &peers(&["Alice"]),
            "alice"
        ));
        assert!(sender_allowed(
            SenderMatch::Handle,
            &peers(&[" @alice "]),
            "@alice"
        ));
        assert!(sender_allowed(
            SenderMatch::Email,
            &peers(&["@example.com"]),
            "user@Example.com"
        ));

        for sender_match in [
            SenderMatch::Exact,
            SenderMatch::CaseInsensitive,
            SenderMatch::Handle,
            SenderMatch::Email,
        ] {
            assert!(!sender_allowed(sender_match, &[], "anyone"));
            assert!(sender_allowed(sender_match, &peers(&["*"]), "anyone"));
        }
    }
    #[test]
    fn shadowed_plugins_are_filtered_before_instantiation() {
        let claimed = HashSet::from(["telegram.main".to_string()]);
        let available: Vec<_> = ["telegram.main", "weather-alerts"]
            .into_iter()
            .filter(|key| channel_key_is_available(key, &claimed))
            .collect();
        assert_eq!(available, ["weather-alerts"]);
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn duplicate_mirror_types_are_ambiguous_regardless_of_manifest_order() {
        fn manifest(name: &str, provides: Option<&str>) -> zeroclaw_plugins::PluginManifest {
            zeroclaw_plugins::PluginManifest {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                description: None,
                author: None,
                wasm_path: Some("plugin.wasm".to_string()),
                wasm_sha256: None,
                capabilities: vec![zeroclaw_plugins::PluginCapability::Channel],
                provides: provides.map(str::to_string),
                sender_match: zeroclaw_plugins::SenderMatch::Exact,
                permissions: Vec::new(),
                signature: None,
                publisher_key: None,
            }
        }

        let first = manifest("first", Some("telegram"));
        let second = manifest("second", Some("telegram"));
        let unique = manifest("unique", Some("slack"));
        let novel = manifest("novel", None);
        let manifests = [&second, &novel, &unique, &first];

        assert_eq!(
            ambiguous_mirror_types(manifests),
            HashSet::from(["telegram"])
        );
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn mirror_credentials_follow_typed_channel_contracts() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let env_guard = ENV_LOCK.lock().expect("environment test lock");
        let keys = [
            "ZEROCLAW_SLACK_BOT_TOKEN",
            "SLACK_BOT_TOKEN",
            "ZEROCLAW_SLACK_APP_TOKEN",
            "SLACK_APP_TOKEN",
            "LINE_CHANNEL_ACCESS_TOKEN",
            "LINE_CHANNEL_SECRET",
            "ZEROCLAW_LINE_CHANNEL_ACCESS_TOKEN",
            "ZEROCLAW_LINE_SENDER_NAME",
            "TELEGRAM_BOT_TOKEN",
            "ZEROCLAW_TELEGRAM_API_BASE_URL",
        ];
        let previous = keys.map(|key| (key, std::env::var_os(key)));
        let restore_guard = scopeguard::guard(previous, |previous| {
            for (key, value) in previous {
                // SAFETY: this test serializes its environment mutations.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        });

        // SAFETY: this test serializes its environment mutations and restores
        // every value before releasing the lock.
        unsafe {
            std::env::set_var("ZEROCLAW_SLACK_BOT_TOKEN", "  xoxb-scoped  ");
            std::env::set_var("SLACK_BOT_TOKEN", "xoxb-plain");
            std::env::remove_var("ZEROCLAW_SLACK_APP_TOKEN");
            std::env::set_var("SLACK_APP_TOKEN", "  xapp-plain  ");
            std::env::set_var("LINE_CHANNEL_ACCESS_TOKEN", "line-access");
            std::env::set_var("LINE_CHANNEL_SECRET", "line-secret");
            std::env::set_var(
                "ZEROCLAW_LINE_CHANNEL_ACCESS_TOKEN",
                "invented-name-must-not-win",
            );
            std::env::set_var("ZEROCLAW_LINE_SENDER_NAME", "must-not-populate");
            std::env::set_var("TELEGRAM_BOT_TOKEN", "must-not-populate");
            std::env::set_var("ZEROCLAW_TELEGRAM_API_BASE_URL", "must-not-populate");
        }

        let mut config = Config::default();
        config.channels.slack.insert(
            "main".to_string(),
            SlackConfig {
                enabled: true,
                bot_token: None,
                app_token: Some("   ".to_string()),
                ..Default::default()
            },
        );
        config.channels.line.insert(
            "main".to_string(),
            LineConfig {
                enabled: true,
                channel_access_token: String::new(),
                channel_secret: String::new(),
                sender_name: Some(String::new()),
                ..Default::default()
            },
        );
        config.channels.telegram.insert(
            "main".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: "schema-resolved-token".to_string(),
                api_base_url: String::new(),
                ..Default::default()
            },
        );
        config.channels.telegram.insert(
            "blank".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: String::new(),
                api_base_url: String::new(),
                ..Default::default()
            },
        );

        let slack =
            serde_json::to_value(&config.channels.slack["main"]).expect("serialize Slack config");
        let slack = resolve_mirror_config(&config, "slack", "main", &slack);
        assert_eq!(slack["bot_token"], "xoxb-scoped");
        assert_eq!(slack["app_token"], "xapp-plain");

        let line =
            serde_json::to_value(&config.channels.line["main"]).expect("serialize LINE config");
        let line = resolve_mirror_config(&config, "line", "main", &line);
        assert_eq!(line["channel_access_token"], "line-access");
        assert_eq!(line["channel_secret"], "line-secret");
        assert_eq!(line["sender_name"], "", "unrelated blank field changed");

        // The legacy LINE contract has no ZEROCLAW-prefixed spelling.
        unsafe { std::env::remove_var("LINE_CHANNEL_ACCESS_TOKEN") };
        let line =
            serde_json::to_value(&config.channels.line["main"]).expect("serialize LINE config");
        let line = resolve_mirror_config(&config, "line", "main", &line);
        assert_eq!(line["channel_access_token"], "");

        let telegram = serde_json::to_value(&config.channels.telegram["main"])
            .expect("serialize Telegram config");
        let telegram = resolve_mirror_config(&config, "telegram", "main", &telegram);
        assert_eq!(telegram["bot_token"], "schema-resolved-token");
        assert_eq!(
            telegram["api_base_url"], "",
            "unrelated blank field changed"
        );

        let blank_telegram = serde_json::to_value(&config.channels.telegram["blank"])
            .expect("serialize blank Telegram config");
        let blank_telegram = resolve_mirror_config(&config, "telegram", "blank", &blank_telegram);
        assert_eq!(
            blank_telegram["bot_token"], "",
            "runtime invented a direct Telegram environment contract"
        );

        drop(restore_guard);
        drop(env_guard);
    }

    #[cfg(feature = "plugins-wasm")]
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn unowned_plugin_log_has_channel_attribution() {
        let writer_guard = zeroclaw_log::__private_test_writer_lock();
        let hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::clear_broadcast_hook();
        let hook_cleanup = scopeguard::guard((), |_| zeroclaw_log::clear_broadcast_hook());
        zeroclaw_log::try_install_capture_subscriber();
        let mut receiver = zeroclaw_log::subscribe_or_install();

        log_unowned_plugin("slack-mirror", "slack.ops").await;

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match receiver.recv().await {
                    Ok(event)
                        if event.get("message").and_then(serde_json::Value::as_str)
                            == Some(
                                "Channel plugin has no enabled owning agent; skipping before instantiation",
                            ) =>
                    {
                        return event;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        panic!("log broadcast closed")
                    }
                }
            }
        })
        .await
        .expect("unowned channel log is captured");

        assert_eq!(event["zeroclaw"]["channel"], "slack.ops");
        assert_eq!(event["zeroclaw"]["channel_type"], "slack");
        assert_eq!(event["zeroclaw"]["channel_alias"], "ops");

        drop(hook_cleanup);
        drop(hook_guard);
        drop(writer_guard);
    }
}
