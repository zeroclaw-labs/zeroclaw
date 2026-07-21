//! Deterministic host-side admission and construction for logical plugins.
//!
//! The activation plan is a per-call materialized view of canonical config and
//! the admitted package host. It never stores operator config, authorization,
//! or guest metadata, and building it never executes guest code.

use std::sync::Arc;

use parking_lot::RwLock;
use zeroclaw_api::channel::Channel;
use zeroclaw_config::schema::Config;

/// Whether this build can execute WASM plugins.
///
/// Consumers use this value instead of duplicating the runtime crate's feature
/// decision in their own feature tables.
pub const WASM_PLUGIN_SUPPORT_COMPILED: bool = cfg!(feature = "plugins-wasm");

#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::PluginCapability;
#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::PluginPermission;
#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::error::PluginError;
#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::host::PluginHost;
#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::instance::PluginInstanceScope;

#[cfg(feature = "plugins-wasm")]
struct ActivationCandidate {
    explicit: bool,
    scope: PluginInstanceScope,
}

/// One deterministic, guest-free admission decision across logical plugin
/// tool, channel, and skill instances.
#[cfg(feature = "plugins-wasm")]
pub(crate) struct PluginActivationPlan {
    admitted: Vec<PluginInstanceScope>,
}

#[cfg(feature = "plugins-wasm")]
impl PluginActivationPlan {
    /// Materialize the current activation decision from canonical host state.
    ///
    /// Explicit, enabled channel declarations require an enabled owning agent
    /// and take priority over package-scoped tool and skill auto-discovery.
    /// The single configured ceiling is applied only after all candidates have
    /// been placed in stable package/capability/binding order.
    pub(crate) fn build(config: &Config, host: &PluginHost) -> Result<Self, PluginError> {
        if !config.plugins.enabled {
            return Ok(Self {
                admitted: Vec::new(),
            });
        }

        let channel_packages: std::collections::HashSet<&str> = host
            .channel_plugin_details()
            .into_iter()
            .map(|(manifest, _)| manifest.name.as_str())
            .collect();
        let mut candidates = Vec::new();

        for (binding, declaration) in &config.channels.plugin {
            if !declaration.enabled || !has_enabled_owner(config, binding) {
                continue;
            }
            let Some(manifest) = host.manifest(&declaration.package) else {
                continue;
            };
            if !channel_packages.contains(manifest.name.as_str()) {
                continue;
            }
            candidates.push(ActivationCandidate {
                explicit: true,
                scope: PluginInstanceScope::from_manifest(
                    manifest,
                    PluginCapability::Channel,
                    binding,
                    manifest.permissions.iter().copied(),
                )?,
            });
        }

        if config.plugins.auto_discover {
            for (manifest, _) in host.tool_plugin_details() {
                candidates.push(ActivationCandidate {
                    explicit: false,
                    scope: PluginInstanceScope::for_package_binding(
                        manifest,
                        PluginCapability::Tool,
                        manifest.permissions.iter().copied(),
                    )?,
                });
            }
            for (manifest, _) in host.skill_plugin_details() {
                candidates.push(ActivationCandidate {
                    explicit: false,
                    scope: PluginInstanceScope::for_package_binding(
                        manifest,
                        PluginCapability::Skill,
                        manifest.permissions.iter().copied(),
                    )?,
                });
            }
        }

        candidates.sort_by(|left, right| {
            left.explicit
                .cmp(&right.explicit)
                .reverse()
                .then_with(|| left.scope.id().package().cmp(right.scope.id().package()))
                .then_with(|| {
                    capability_key(left.scope.id().capability())
                        .cmp(capability_key(right.scope.id().capability()))
                })
                .then_with(|| left.scope.id().binding().cmp(right.scope.id().binding()))
        });

        Ok(Self {
            admitted: candidates
                .into_iter()
                .take(config.plugins.max_active_instances)
                .map(|candidate| candidate.scope)
                .collect(),
        })
    }

    /// Return the exact host-issued scope admitted for one logical instance.
    pub(crate) fn scope(
        &self,
        package: &str,
        capability: PluginCapability,
        binding: &str,
    ) -> Option<PluginInstanceScope> {
        self.admitted
            .iter()
            .find(|scope| {
                let id = scope.id();
                id.package() == package && id.capability() == capability && id.binding() == binding
            })
            .cloned()
    }

    fn scopes(
        &self,
        capability: PluginCapability,
    ) -> impl Iterator<Item = PluginInstanceScope> + '_ {
        self.admitted
            .iter()
            .filter(move |scope| scope.id().capability() == capability)
            .cloned()
    }
}

#[cfg(feature = "plugins-wasm")]
fn has_enabled_owner(config: &Config, binding: &str) -> bool {
    let channel_ref = format!("plugin.{binding}");
    config.agents.values().any(|agent| {
        agent.enabled
            && agent
                .channels
                .iter()
                .any(|configured| configured.as_str() == channel_ref)
    })
}

#[cfg(feature = "plugins-wasm")]
const fn capability_key(capability: PluginCapability) -> &'static str {
    match capability {
        PluginCapability::Channel => "channel",
        PluginCapability::Memory => "memory",
        PluginCapability::Observer => "observer",
        PluginCapability::Skill => "skill",
        PluginCapability::Tool => "tool",
    }
}

#[cfg(feature = "plugins-wasm")]
pub(crate) fn plugin_host(config: &Config) -> Result<Arc<PluginHost>, PluginError> {
    let signature_mode =
        PluginHost::resolve_signature_mode(&config.plugins.security.signature_mode);
    PluginHost::from_plugins_dir_with_security(
        &config.plugins.resolved_plugins_dir(),
        signature_mode,
        config.plugins.security.trusted_publisher_keys.clone(),
    )
    .map(Arc::new)
}

#[cfg(feature = "plugins-wasm")]
pub(crate) fn plugin_limits(config: &Config) -> zeroclaw_plugins::component::PluginLimits {
    zeroclaw_plugins::component::PluginLimits {
        call_fuel: config.plugins.limits.call_fuel,
        max_memory_bytes: config
            .plugins
            .limits
            .max_memory_mb
            .saturating_mul(1024 * 1024),
        max_table_elements: config.plugins.limits.max_table_elements,
        max_instances: config.plugins.limits.max_instances,
    }
}

/// Construct every admitted channel plugin from the exact component bytes the
/// package host verified. The returned channels carry their canonical alias in
/// their host-issued `PluginChannelEndpoint`.
pub async fn configured_plugin_channels(
    config: Arc<Config>,
    live_config: Option<Arc<RwLock<Config>>>,
    webhooks: Option<&zeroclaw_api::webhook::PluginWebhookRegistry>,
) -> Vec<Arc<dyn Channel>> {
    #[cfg(not(feature = "plugins-wasm"))]
    {
        let _ = (config, live_config, webhooks);
        Vec::new()
    }

    #[cfg(feature = "plugins-wasm")]
    {
        if !config.plugins.enabled {
            return Vec::new();
        }

        let host = match plugin_host(&config) {
            Ok(host) => host,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{error}")})),
                    "Failed to discover WASM channel plugins"
                );
                return Vec::new();
            }
        };
        let plan = match PluginActivationPlan::build(&config, &host) {
            Ok(plan) => plan,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{error}")})),
                    "Failed to admit logical plugin instances"
                );
                return Vec::new();
            }
        };
        let authorization_config = live_config
            .clone()
            .unwrap_or_else(|| Arc::new(RwLock::new((*config).clone())));
        let services =
            crate::tools::plugin_host_services(Arc::clone(&host), Arc::clone(&config), live_config);
        let limits = plugin_limits(&config);
        let details = host.channel_plugin_details();
        let scopes: Vec<_> = plan.scopes(PluginCapability::Channel).collect();
        let admitted_count = scopes.len();
        let mut candidates = Vec::with_capacity(admitted_count);

        for scope in scopes {
            let package = scope.id().package().to_string();
            let Some((manifest, component)) = details
                .iter()
                .copied()
                .find(|(manifest, _)| manifest.name == package)
            else {
                continue;
            };
            let endpoint =
                match zeroclaw_plugins::endpoint::PluginChannelEndpoint::new(scope, "plugin") {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Load
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "plugin": package,
                                "error": format!("{error}"),
                            })),
                            "Failed to bind WASM channel plugin endpoint"
                        );
                        continue;
                    }
                };
            let alias = endpoint.alias().to_string();
            let authorizer = plugin_sender_authorizer(&authorization_config, &alias);
            match zeroclaw_plugins::wasm_channel::WasmChannel::from_wasm_with_authorizer(
                endpoint, component, &services, limits, authorizer,
            )
            .await
            {
                Ok(channel) => candidates.push(BuiltChannelCandidate {
                    package,
                    alias,
                    config_read: manifest.permissions.contains(&PluginPermission::ConfigRead),
                    channel,
                }),
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "plugin": package,
                                "channel_alias": alias,
                                "error": format!("{error:#}"),
                            })),
                        "Failed to construct WASM channel plugin"
                    );
                }
            }
        }

        let channels = finalize_plugin_webhooks(candidates, webhooks).await;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load).with_attrs(
                ::serde_json::json!({
                    "admitted": admitted_count,
                    "constructed": channels.len(),
                })
            ),
            "Registered WASM channel plugins"
        );
        channels
    }
}

#[cfg(feature = "plugins-wasm")]
struct BuiltChannelCandidate {
    package: String,
    alias: String,
    config_read: bool,
    channel: zeroclaw_plugins::wasm_channel::WasmChannel,
}

#[cfg(feature = "plugins-wasm")]
fn plugin_sender_authorizer(
    config: &Arc<RwLock<Config>>,
    alias: &str,
) -> zeroclaw_plugins::wasm_channel::SenderAuthorizer {
    let config = Arc::clone(config);
    let alias = alias.to_string();
    Arc::new(move |sender| {
        config
            .read()
            .channel_external_peers("plugin", &alias)
            .iter()
            .any(|peer| peer == "*" || peer == sender)
    })
}

#[cfg(feature = "plugins-wasm")]
async fn finalize_plugin_webhooks(
    candidates: Vec<BuiltChannelCandidate>,
    webhooks: Option<&zeroclaw_api::webhook::PluginWebhookRegistry>,
) -> Vec<Arc<dyn Channel>> {
    let Some(registry) = webhooks else {
        return candidates
            .into_iter()
            .map(|candidate| Arc::new(candidate.channel) as Arc<dyn Channel>)
            .collect();
    };

    let mut paths = Vec::with_capacity(candidates.len());
    let mut claimants: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let path = plugin_webhook_path(candidate).await;
        if let Some(path) = path.as_ref() {
            claimants.entry(path.clone()).or_default().push(index);
        }
        paths.push(path);
    }
    let ambiguous: std::collections::HashSet<usize> = claimants
        .values()
        .filter(|indices| indices.len() > 1)
        .flat_map(|indices| indices.iter().copied())
        .collect();

    let mut channels: Vec<Arc<dyn Channel>> = Vec::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        let path = paths[index].as_deref();
        if ambiguous.contains(&index) {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": candidate.package,
                        "channel_alias": candidate.alias,
                        "path": path,
                        "error_key": "plugin_webhook_path_ambiguous",
                    })),
                "Multiple plugin channels claim one webhook path; rejecting every claimant"
            );
            continue;
        }
        if let Some(path) = path {
            let (tx, rx) = tokio::sync::mpsc::channel(64);
            if !registry.insert(path.to_string(), tx) {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": candidate.package,
                            "channel_alias": candidate.alias,
                            "path": path,
                            "error_key": "plugin_webhook_path_claimed",
                        })),
                    "Webhook path is already owned; rejecting plugin channel"
                );
                continue;
            }
            candidate.channel.set_webhook_rx(rx);
        }
        channels.push(Arc::new(candidate.channel));
    }
    channels
}

#[cfg(feature = "plugins-wasm")]
async fn plugin_webhook_path(candidate: &BuiltChannelCandidate) -> Option<String> {
    if !candidate.channel.has_webhook_ingress() {
        return None;
    }
    if !candidate.config_read {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"plugin": candidate.package})),
            "Webhook channel plugin lacks config_read; inbound disabled"
        );
        return None;
    }
    let path = candidate.channel.webhook_path().await?;
    if path.is_empty() || path.contains('/') || path.contains('.') {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "plugin": candidate.package,
                    "channel_alias": candidate.alias,
                    "path": path,
                })),
            "Webhook plugin declared an invalid route segment"
        );
        return None;
    }
    Some(path)
}

#[cfg(all(test, feature = "plugins-wasm"))]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use tempfile::TempDir;
    use zeroclaw_config::providers::ChannelRef;
    use zeroclaw_config::schema::{AliasedAgentConfig, PluginChannelConfig};

    use super::*;

    fn write_executable_plugin(root: &Path, name: &str, capabilities: &[&str]) {
        let plugin_dir = root.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let capabilities = capabilities
            .iter()
            .map(|capability| format!("\"{capability}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [{capabilities}]\n"
            ),
        )
        .unwrap();
        // Package admission intentionally does not compile the component. An
        // invalid payload here proves activation planning remains guest-free.
        std::fs::write(plugin_dir.join("plugin.wasm"), b"not a component").unwrap();
    }

    fn write_skill_plugin(root: &Path, name: &str) {
        let skill_dir = root.join(name).join("skills").join("sample");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            root.join(name).join("manifest.toml"),
            format!("name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n"),
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: sample\ndescription: sample skill\n---\n# Sample\n",
        )
        .unwrap();
    }

    fn fixture() -> (TempDir, Config, Arc<PluginHost>) {
        let plugins = TempDir::new().unwrap();
        write_executable_plugin(plugins.path(), "alpha", &["channel", "tool"]);
        write_skill_plugin(plugins.path(), "beta");
        write_executable_plugin(plugins.path(), "zeta", &["tool"]);

        let mut config = Config::default();
        config.plugins.enabled = true;
        config.plugins.auto_discover = true;
        config.plugins.max_active_instances = 10;
        config.plugins.plugins_dir = plugins.path().display().to_string();
        config.channels.plugin = HashMap::from([
            (
                "ops".to_string(),
                PluginChannelConfig {
                    package: "alpha".to_string(),
                    enabled: true,
                },
            ),
            (
                "backup".to_string(),
                PluginChannelConfig {
                    package: "alpha".to_string(),
                    enabled: true,
                },
            ),
        ]);
        let agent = AliasedAgentConfig {
            channels: vec![
                ChannelRef::new("plugin.ops"),
                ChannelRef::new("plugin.backup"),
            ],
            ..AliasedAgentConfig::default()
        };
        config.agents = HashMap::from([("operator".to_string(), agent)]);

        let host = plugin_host(&config).unwrap();
        (plugins, config, host)
    }

    fn identities(plan: &PluginActivationPlan) -> Vec<(String, &'static str, String)> {
        plan.admitted
            .iter()
            .map(|scope| {
                (
                    scope.id().package().to_string(),
                    capability_key(scope.id().capability()),
                    scope.id().binding().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn one_cap_is_deterministic_across_explicit_channels_tools_and_skills() {
        let (_plugins, mut config, host) = fixture();
        config.plugins.max_active_instances = 4;

        let first = PluginActivationPlan::build(&config, &host).unwrap();
        let second = PluginActivationPlan::build(&config, &host).unwrap();
        let expected = vec![
            ("alpha".to_string(), "channel", "backup".to_string()),
            ("alpha".to_string(), "channel", "ops".to_string()),
            ("alpha".to_string(), "tool", "alpha".to_string()),
            ("beta".to_string(), "skill", "beta".to_string()),
        ];

        assert_eq!(identities(&first), expected);
        assert_eq!(identities(&second), expected);
        assert!(
            first
                .scope("zeta", PluginCapability::Tool, "zeta")
                .is_none(),
            "the shared ceiling must reject the next logical tool candidate"
        );
    }

    #[test]
    fn explicit_channel_does_not_require_auto_discovery() {
        let (_plugins, mut config, host) = fixture();
        config.plugins.auto_discover = false;

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert!(
            plan.scope("alpha", PluginCapability::Channel, "ops")
                .is_some()
        );
        assert!(
            plan.scope("alpha", PluginCapability::Tool, "alpha")
                .is_none()
        );
        assert!(
            plan.scope("beta", PluginCapability::Skill, "beta")
                .is_none()
        );
    }

    #[test]
    fn explicit_channel_rejects_an_inactive_owner() {
        let (_plugins, config, host) = fixture();
        let mut config = config;
        config.agents.get_mut("operator").unwrap().enabled = false;
        config.plugins.auto_discover = false;

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert!(identities(&plan).is_empty());
    }

    #[test]
    fn explicit_channel_keeps_the_exact_host_alias_and_plugin_family() {
        let (_plugins, config, host) = fixture();
        let plan = PluginActivationPlan::build(&config, &host).unwrap();
        let scope = plan
            .scope("alpha", PluginCapability::Channel, "ops")
            .unwrap();
        let endpoint =
            zeroclaw_plugins::endpoint::PluginChannelEndpoint::new(scope, "plugin").unwrap();

        assert_eq!(endpoint.channel_type(), "plugin");
        assert_eq!(endpoint.alias(), "ops");
        assert!(
            plan.scope("alpha", PluginCapability::Channel, "alpha")
                .is_none(),
            "a configured alias must never fall back to the package binding"
        );
    }
}
