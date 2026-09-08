//! Deterministic host-side admission and construction for logical plugins.
//!
//! The activation plan is a per-call materialized view of canonical config and
//! the admitted package host. It never stores operator config, authorization,
//! or guest metadata, and building it never executes guest code.

#[cfg(feature = "plugins-wasm")]
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;
use zeroclaw_api::channel::Channel;
use zeroclaw_api::webhook::PluginWebhookRegistryLease;
use zeroclaw_config::schema::Config;

/// Whether this build can execute WASM plugins.
///
/// Consumers use this value instead of duplicating the runtime crate's feature
/// decision in their own feature tables.
pub const WASM_PLUGIN_SUPPORT_COMPILED: bool = cfg!(feature = "plugins-wasm");

#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::PluginCapability;
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

#[cfg(feature = "plugins-wasm")]
struct BuiltChannelCandidate {
    package: String,
    alias: String,
    config_read: bool,
    channel: zeroclaw_plugins::wasm_channel::WasmChannel,
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
    ///
    /// The enumeration order is the contract every loader depends on, so it is
    /// stated once here rather than re-derived per capability:
    ///
    /// 1. Explicit `[channels.plugin.<alias>]` declarations, then
    /// 2. auto-discovered tool and skill package bindings,
    ///
    /// each group ordered by package name, then capability, then binding. The
    /// ceiling truncates that one sequence, so an operator-configured channel
    /// can never be displaced by a package that merely happens to be installed.
    ///
    /// Nothing here touches component bytes: candidates are derived from the
    /// manifests the package host already admitted, so a package with an
    /// unloadable component still plans identically. That property is what lets
    /// the ceiling be applied before any guest runs.
    ///
    /// The decision is a pure function of `config` and `host`: it holds no
    /// counter and consumes no budget. Every physical registry reconstruction
    /// (per agent, per CLI run, per delegate, per SOP execution) re-derives the
    /// identical admitted set from the same snapshot instead of spending slots
    /// cumulatively.
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

    fn find(
        &self,
        package: &str,
        capability: PluginCapability,
        binding: &str,
    ) -> Option<&PluginInstanceScope> {
        self.admitted.iter().find(|scope| {
            let id = scope.id();
            id.package() == package && id.capability() == capability && id.binding() == binding
        })
    }

    /// Whether this plan admitted one exact logical instance.
    ///
    /// The skill loader reads a directory per package rather than a component,
    /// so it walks host details and asks this question instead of re-deriving
    /// the admission rule for itself.
    pub(crate) fn admits(
        &self,
        package: &str,
        capability: PluginCapability,
        binding: &str,
    ) -> bool {
        self.find(package, capability, binding).is_some()
    }

    /// Return the exact host-issued scope admitted for one logical instance.
    ///
    /// Production construction iterates [`Self::scopes`] by capability; this
    /// point lookup exists so tests can assert on a single admission decision.
    #[cfg(test)]
    fn scope(
        &self,
        package: &str,
        capability: PluginCapability,
        binding: &str,
    ) -> Option<PluginInstanceScope> {
        self.find(package, capability, binding).cloned()
    }

    /// Admitted scopes of one capability, in the plan's enumeration order.
    pub(crate) fn scopes(
        &self,
        capability: PluginCapability,
    ) -> impl Iterator<Item = PluginInstanceScope> + '_ {
        self.admitted
            .iter()
            .filter(move |scope| scope.id().capability() == capability)
            .cloned()
    }
}

/// An explicit channel binding activates only when some enabled agent actually
/// routes to it. Without an owner the listener would run with nothing to
/// deliver to, so an orphaned declaration is inert rather than half-live.
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
        call_timeout: std::time::Duration::from_millis(config.plugins.limits.call_timeout_ms),
    }
}

#[cfg(feature = "plugins-wasm")]
fn plugin_sender_allowed(config: &Config, alias: &str, sender: &str) -> bool {
    config
        .channel_external_peers("plugin", alias)
        .iter()
        .any(|allowed| allowed == "*" || allowed == sender)
}

#[cfg(feature = "plugins-wasm")]
fn channel_sender_authorizer(
    config: Arc<Config>,
    live_config: Option<Arc<RwLock<Config>>>,
    alias: String,
) -> zeroclaw_plugins::wasm_channel::SenderAuthorizer {
    match live_config {
        Some(live_config) => {
            Arc::new(move |sender| plugin_sender_allowed(&live_config.read(), &alias, sender))
        }
        None => Arc::new(move |sender| plugin_sender_allowed(&config, &alias, sender)),
    }
}

#[cfg(feature = "plugins-wasm")]
fn valid_webhook_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(feature = "plugins-wasm")]
fn bounded_plugin_log_value(value: impl AsRef<str>, max_chars: usize) -> String {
    let value = value.as_ref();
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut bounded: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    bounded.push('…');
    bounded
}

#[cfg(feature = "plugins-wasm")]
async fn finalize_plugin_webhooks(
    candidates: Vec<BuiltChannelCandidate>,
    registry: Option<&PluginWebhookRegistryLease>,
) -> Vec<Arc<dyn Channel>> {
    let Some(registry) = registry else {
        return candidates
            .into_iter()
            .map(|candidate| Arc::new(candidate.channel) as Arc<dyn Channel>)
            .collect();
    };

    // Resolve every guest declaration before mutating the shared route map.
    // Invalid or duplicate claims reject their entire channel instance so an
    // advertised webhook channel can never run in a silently unreachable mode.
    let mut paths = Vec::with_capacity(candidates.len());
    let mut rejected = HashSet::new();
    let mut claimants: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if !candidate.channel.has_webhook_ingress() {
            paths.push(None);
            continue;
        }
        if !candidate.config_read {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": candidate.package,
                        "channel_alias": candidate.alias,
                        "error_key": "plugin_webhook_config_read_required",
                    })),
                "Webhook channel plugin requires config_read; rejecting channel instance"
            );
            rejected.insert(index);
            paths.push(None);
            continue;
        }

        match candidate.channel.webhook_path().await {
            Ok(Some(path)) if valid_webhook_path(&path) => {
                claimants.entry(path.clone()).or_default().push(index);
                paths.push(Some(path));
            }
            Ok(Some(path)) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": candidate.package,
                            "channel_alias": candidate.alias,
                            "path": bounded_plugin_log_value(path, 128),
                            "error_key": "plugin_webhook_path_invalid",
                        })),
                    "Webhook channel plugin declared an invalid route; rejecting channel instance"
                );
                rejected.insert(index);
                paths.push(None);
            }
            Ok(None) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": candidate.package,
                            "channel_alias": candidate.alias,
                            "error_key": "plugin_webhook_path_missing",
                        })),
                    "Webhook channel plugin advertised ingress without a route; rejecting channel instance"
                );
                rejected.insert(index);
                paths.push(None);
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": candidate.package,
                            "channel_alias": candidate.alias,
                            "error": bounded_plugin_log_value(format!("{error:#}"), 2_048),
                            "error_key": "plugin_webhook_path_unavailable",
                        })),
                    "Webhook channel plugin route could not be resolved; rejecting channel instance"
                );
                rejected.insert(index);
                paths.push(None);
            }
        }
    }

    let ambiguous: HashSet<usize> = claimants
        .values()
        .filter(|indices| indices.len() > 1)
        .flat_map(|indices| indices.iter().copied())
        .collect();
    let mut routes = HashMap::new();
    let mut channels = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.into_iter().enumerate() {
        if rejected.contains(&index) {
            continue;
        }
        if ambiguous.contains(&index) {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": candidate.package,
                        "channel_alias": candidate.alias,
                        "path": paths[index],
                        "error_key": "plugin_webhook_path_ambiguous",
                    })),
                "Multiple channel-plugin instances claimed one webhook route; rejecting every claimant"
            );
            continue;
        }

        if let Some(path) = paths[index].as_ref() {
            let (sink, receiver) = tokio::sync::mpsc::channel(64);
            candidate.channel.set_webhook_receiver(receiver);
            routes.insert(path.clone(), sink);
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                    .with_attrs(::serde_json::json!({
                        "plugin": candidate.package,
                        "channel_alias": candidate.alias,
                        "path": path,
                    })),
                "Registered channel-plugin webhook route"
            );
        }
        channels.push(Arc::new(candidate.channel) as Arc<dyn Channel>);
    }

    if registry.replace(routes) {
        channels
    } else {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "plugin_webhook_generation_stale",
                })),
            "A newer channel supervisor owns plugin webhook routes; rejecting stale plugin channels"
        );
        Vec::new()
    }
}

/// Construct every admitted channel plugin from the package the host verified.
///
/// The returned channels carry their canonical alias in their host-issued
/// `PluginChannelEndpoint`, so the caller can register them under the ordinary
/// `plugin.<alias>` composite key without re-deriving identity from config.
///
/// A package that fails to construct is logged and skipped: one broken plugin
/// must not stop the daemon from starting its remaining channels.
pub async fn configured_plugin_channels(
    config: Arc<Config>,
    live_config: Option<Arc<RwLock<Config>>>,
) -> Vec<Arc<dyn Channel>> {
    Box::pin(configured_plugin_channels_with_webhooks(
        config,
        live_config,
        None,
    ))
    .await
}

/// Construct configured channel plugins and publish their validated webhook
/// claims into one daemon-generation registry.
pub async fn configured_plugin_channels_with_webhooks(
    config: Arc<Config>,
    live_config: Option<Arc<RwLock<Config>>>,
    webhook_registry: Option<&PluginWebhookRegistryLease>,
) -> Vec<Arc<dyn Channel>> {
    #[cfg(not(feature = "plugins-wasm"))]
    {
        let _ = (config, live_config, webhook_registry);
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
        let host_services = crate::tools::plugin_host_services(
            Arc::clone(&host),
            Arc::clone(&config),
            live_config.clone(),
        );
        let limits = plugin_limits(&config);
        let details = host.channel_plugin_details();
        let scopes: Vec<_> = plan.scopes(PluginCapability::Channel).collect();
        let admitted_count = scopes.len();
        let mut candidates = Vec::with_capacity(admitted_count);

        for scope in scopes {
            let package = scope.id().package().to_string();
            let Some((manifest, wasm_path)) = details
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
            let authorizer =
                channel_sender_authorizer(Arc::clone(&config), live_config.clone(), alias.clone());
            match zeroclaw_plugins::wasm_channel::WasmChannel::from_wasm(
                endpoint,
                wasm_path,
                &host_services,
                limits,
            )
            .await
            {
                Ok(channel) => candidates.push(BuiltChannelCandidate {
                    package: package.clone(),
                    alias,
                    config_read: manifest
                        .permissions
                        .contains(&zeroclaw_plugins::PluginPermission::ConfigRead),
                    channel: channel.with_sender_authorizer(authorizer),
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

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load).with_attrs(
                ::serde_json::json!({
                    "admitted": admitted_count,
                    "constructed": candidates.len(),
                })
            ),
            "Registered WASM channel plugins"
        );
        finalize_plugin_webhooks(candidates, webhook_registry).await
    }
}

#[cfg(all(test, feature = "plugins-wasm"))]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use tempfile::TempDir;
    use zeroclaw_config::providers::ChannelRef;
    use zeroclaw_config::schema::{AliasedAgentConfig, PluginChannelConfig};

    use super::*;

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn webhook_paths_use_the_bounded_single_segment_grammar() {
        for path in ["a", "Fixture_01", "a-b", &"x".repeat(64)] {
            assert!(valid_webhook_path(path), "expected valid path: {path:?}");
        }
        for path in [
            "".to_string(),
            "x".repeat(65),
            "has.dot".to_string(),
            "has/slash".to_string(),
            "has space".to_string(),
            "control\n".to_string(),
            "unicode-λ".to_string(),
        ] {
            assert!(
                !valid_webhook_path(&path),
                "expected invalid path: {path:?}"
            );
        }
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn channel_sender_policy_resolves_plugin_peer_groups_live() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        let live = Arc::new(RwLock::new(Config::default()));
        let authorizer = channel_sender_authorizer(
            Arc::new(Config::default()),
            Some(Arc::clone(&live)),
            "operations".to_string(),
        );
        assert!(!authorizer("alice"), "empty peer groups deny by default");

        live.write().peer_groups.insert(
            "operators".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("plugin.operations"),
                external_peers: vec![PeerUsername::new("alice")],
                ..PeerGroupConfig::default()
            },
        );
        assert!(authorizer("alice"));
        assert!(!authorizer("Alice"), "plugin sender identity is exact");

        live.write()
            .peer_groups
            .get_mut("operators")
            .expect("operator peer group")
            .external_peers = vec![PeerUsername::new("*")];
        assert!(
            authorizer("anyone"),
            "wildcard uses native channel semantics"
        );
    }

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

    /// Every package in the fixture carries an unloadable component, so a plan
    /// that reached guest code could not be built at all. Planning the full
    /// candidate set proves admission is decided from manifests alone.
    #[test]
    fn building_a_plan_never_touches_component_bytes() {
        let (plugins, config, host) = fixture();
        for package in ["alpha", "zeta"] {
            let bytes = std::fs::read(plugins.path().join(package).join("plugin.wasm")).unwrap();
            assert_eq!(
                bytes, b"not a component",
                "the fixture must stay unloadable for this proof to mean anything"
            );
        }

        let plan = PluginActivationPlan::build(&config, &host)
            .expect("planning must succeed without loading any component");

        assert_eq!(
            identities(&plan).len(),
            5,
            "two channels, two tools, and one skill are all planned from manifests"
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
    fn explicit_channel_rejects_a_disabled_declaration() {
        let (_plugins, mut config, host) = fixture();
        config.plugins.auto_discover = false;
        config.channels.plugin.get_mut("ops").unwrap().enabled = false;

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert!(
            plan.scope("alpha", PluginCapability::Channel, "ops")
                .is_none(),
            "a disabled declaration must not be admitted"
        );
        assert!(
            plan.scope("alpha", PluginCapability::Channel, "backup")
                .is_some(),
            "disabling one alias must not disturb its sibling"
        );
    }

    #[test]
    fn explicit_channel_rejects_an_uninstalled_or_non_channel_package() {
        let (_plugins, mut config, host) = fixture();
        config.plugins.auto_discover = false;
        config.channels.plugin.insert(
            "missing".to_string(),
            PluginChannelConfig {
                package: "not-installed".to_string(),
                enabled: true,
            },
        );
        // `zeta` is installed but declares only the tool capability.
        config.channels.plugin.insert(
            "toolonly".to_string(),
            PluginChannelConfig {
                package: "zeta".to_string(),
                enabled: true,
            },
        );
        let agent = config.agents.get_mut("operator").unwrap();
        agent.channels.push(ChannelRef::new("plugin.missing"));
        agent.channels.push(ChannelRef::new("plugin.toolonly"));

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert!(
            plan.scope("not-installed", PluginCapability::Channel, "missing")
                .is_none()
        );
        assert!(
            plan.scope("zeta", PluginCapability::Channel, "toolonly")
                .is_none(),
            "a package without the channel capability must not back a channel binding"
        );
    }

    #[test]
    fn disabled_plugin_system_admits_nothing() {
        let (_plugins, mut config, host) = fixture();
        config.plugins.enabled = false;

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

    /// The loader must survive a package whose component cannot be loaded:
    /// every fixture package is unloadable, so construction fails for all of
    /// them and the daemon still gets a well-formed (empty) channel list.
    #[tokio::test]
    async fn an_unloadable_component_is_skipped_rather_than_fatal() {
        let (_plugins, config, _host) = fixture();

        let channels = configured_plugin_channels(Arc::new(config), None).await;

        assert!(channels.is_empty());
    }
}
