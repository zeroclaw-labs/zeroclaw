//! Deterministic host-side admission and construction for logical plugins.
//!
//! The activation plan is a per-call materialized view of canonical config and
//! the admitted package host. It never stores operator config, authorization,
//! or guest metadata, and building it never executes guest code.

#[cfg(feature = "plugins-wasm")]
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
    /// Mirror candidates, deliberately kept OUT of `admitted`.
    ///
    /// Everything that constructs a guest reads `admitted` through
    /// [`Self::scopes`]. A mirror is not constructible yet: the endpoint would
    /// still be stamped `plugin`, the sender policy would still resolve
    /// `channel_external_peers("plugin", alias)`, and the config resolver would
    /// still hand it its `plugins.entries` row instead of
    /// `channels.<provides>.<alias>`. Admitting one into `admitted` would build
    /// a channel that cannot act as the alias it mirrors, register it under a
    /// `plugin.<alias>` routing key it does not own, and consume an
    /// explicit-priority instance slot that an auto-discovered tool or skill
    /// would otherwise get.
    ///
    /// So the decision is recorded and nothing consumes it. When the config
    /// feed, endpoint type, peer policy and native precedence land, these move
    /// into `admitted` and back under the single `max_active_instances`
    /// ceiling.
    ///
    /// `allow(dead_code)`: having no production reader is the invariant, not an
    /// oversight. The test accessor is the only consumer, and the day this
    /// field is read outside `#[cfg(test)]` is the day a mirror can be
    /// constructed — which is the review gate this attribute marks.
    #[allow(dead_code)]
    mirrors: Vec<PluginInstanceScope>,
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
                mirrors: Vec::new(),
            });
        }

        let channel_packages = channel_package_names(host);
        let mut candidates = Vec::new();
        // Kept apart from `candidates` on purpose; see `Self::mirrors`.
        let mut mirror_candidates: Vec<PluginInstanceScope> = Vec::new();

        // The per-declaration rule is `decide_explicit_channel`, shared with
        // `channel_binding_admission` so a report of this pass cannot drift
        // from it. A declaration it refuses is skipped here; the reason is
        // only reported by that query.
        for (binding, declaration) in &config.channels.plugin {
            if let ExplicitChannelDecision::Candidate(scope) =
                decide_explicit_channel(config, host, &channel_packages, binding, declaration)?
            {
                candidates.push(ActivationCandidate {
                    explicit: true,
                    scope,
                });
            }
        }

        // Mirror candidates. A channel package whose manifest declares
        // `provides = "<id>"` is a drop-in for that compiled-in channel, so it
        // activates once per configured and enabled `[channels.<id>.<alias>]`
        // rather than from a single `[channels.plugin.<alias>]` declaration.
        // Canonical channel config stays the one home for those settings.
        //
        // These count as explicit: the aliases are operator-named in canonical
        // config, so they take ceiling priority over auto-discovery exactly as
        // an explicit plugin channel declaration does.
        //
        // Native-wins is NOT decided here. Whether a compiled-in channel of the
        // same id exists is a build-feature fact owned by the channel
        // orchestrator, which drops a mirror that collides with a live native
        // alias. Admission only decides which mirrors are eligible at all.
        let mirror_claims = mirror_claim_counts(host);
        let channels_view = serde_json::to_value(&config.channels).ok();
        for (manifest, _) in host.channel_plugin_details() {
            let Some(provides) = manifest.provides.as_deref() else {
                continue;
            };
            // `provides` must name a channel type this build actually knows.
            //
            // This is diagnostic, not load-bearing: an unknown id has no
            // canonical config section, so it would admit nothing regardless.
            // The value is the log line — a typo otherwise looks exactly like
            // a plugin whose aliases are simply unconfigured, which is the
            // kind of silence that costs an operator an afternoon.
            // `plugin` is in V3_CHANNEL_TYPES, but it is the plugin family's
            // own namespace, not a compiled-in channel to mirror. Allowing it
            // would let any admitted package enumerate
            // `[channels.plugin.<alias>]` declarations that name a DIFFERENT
            // package and claim those aliases, because this pass keys on the
            // alias and never reads the declaration's `package` field the way
            // the explicit pass above does. That is a route takeover, not a
            // mirror, so the family is refused outright.
            if provides == PLUGIN_CHANNEL_FAMILY {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": manifest.name,
                            "provides": provides,
                            "error_key": "plugin_mirror_reserved_channel_family",
                        })),
                    "A plugin may not mirror the reserved `plugin` channel family; refusing the mirror"
                );
                continue;
            }
            if !zeroclaw_config::schema::v2::V3_CHANNEL_TYPES.contains(&provides) {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": manifest.name,
                            "provides": provides,
                            "error_key": "plugin_mirror_unknown_channel_id",
                        })),
                    "Plugin mirrors a channel id this build does not define; refusing the mirror"
                );
                continue;
            }
            // Two packages claiming one id is ambiguous, and the host will not
            // pick a mirror on the operator's behalf: both fail closed.
            if mirror_claims.get(provides).copied().unwrap_or_default() > 1 {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": manifest.name,
                            "provides": provides,
                            "error_key": "plugin_mirror_ambiguous_provider",
                        })),
                    "More than one plugin mirrors this channel id; refusing every claimant"
                );
                continue;
            }
            // A mirror is fed the alias's canonical config, so without
            // `config_read` it would run against an empty object and
            // misbehave silently. Refuse it instead.
            if !manifest
                .permissions
                .contains(&zeroclaw_plugins::PluginPermission::ConfigRead)
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": manifest.name,
                            "provides": provides,
                            "error_key": "plugin_mirror_config_read_required",
                        })),
                    "Channel mirror requires config_read; refusing the mirror"
                );
                continue;
            }
            let Some(aliases) = channels_view
                .as_ref()
                .and_then(|channels| channels.get(provides))
                .and_then(serde_json::Value::as_object)
            else {
                continue;
            };
            for (alias, entry) in aliases {
                let enabled = entry
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if !enabled {
                    continue;
                }
                if !has_enabled_owner_ref(config, &format!("{provides}.{alias}")) {
                    continue;
                }
                mirror_candidates.push(PluginInstanceScope::from_manifest(
                    manifest,
                    PluginCapability::Channel,
                    alias,
                    manifest.permissions.iter().copied(),
                )?);
            }
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

        // Mirrors are ordered by the same stable key so the recorded decision
        // is deterministic, and bounded by the same configured number so a
        // large install cannot record an unbounded set. They are counted
        // separately from `admitted` because nothing constructs them: charging
        // them against the shared ceiling would let an inert mirror displace a
        // tool or skill that does run.
        mirror_candidates.sort_by(|left, right| {
            left.id()
                .package()
                .cmp(right.id().package())
                .then_with(|| left.id().binding().cmp(right.id().binding()))
        });
        mirror_candidates.truncate(config.plugins.max_active_instances);

        Ok(Self {
            admitted: candidates
                .into_iter()
                .take(config.plugins.max_active_instances)
                .map(|candidate| candidate.scope)
                .collect(),
            mirrors: mirror_candidates,
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

    /// Mirror decisions recorded by this plan.
    ///
    /// Intentionally has no production consumer: see [`Self::mirrors`].
    #[cfg(test)]
    fn mirror_identities(&self) -> Vec<(String, String)> {
        self.mirrors
            .iter()
            .map(|scope| {
                (
                    scope.id().package().to_string(),
                    scope.id().binding().to_string(),
                )
            })
            .collect()
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

/// Whether one explicit `[channels.plugin.<alias>]` binding is an activation
/// candidate, and if not, the first precondition the plan finds unmet, in the
/// order the plan applies them.
///
/// The refusals are listed in that order. `PluginsDisabled` is the global
/// switch, `Undeclared` means there is no declaration to judge, the next four
/// are the plan's per-declaration checks, and `OverInstanceCeiling` is the
/// shared ceiling, which only a whole plan can apply.
#[cfg(feature = "plugins-wasm")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelBindingAdmission {
    /// The plan admits the instance, so the channel loader will try to
    /// construct it. Admission is not construction: the loader can still
    /// refuse the instance, for example when its component does not load.
    Admitted,
    /// `plugins.enabled` is false, so the plan admits no plugin instance at
    /// all.
    PluginsDisabled,
    /// No `[channels.plugin.<alias>]` declaration exists.
    Undeclared,
    /// The declaration sets `enabled = false`.
    BindingDisabled,
    /// No enabled agent lists `plugin.<alias>` among its channels, so the
    /// instance would have nothing to deliver to.
    NoEnabledOwner,
    /// The declaration names a package the plugin host did not load: it is
    /// not installed, or discovery refused it, for example on a failed
    /// signature check.
    PackageNotInstalled {
        /// The package the declaration names.
        package: String,
    },
    /// The named package is loaded but does not declare the `channel`
    /// capability, so it cannot back a channel binding.
    NotAChannelPackage {
        /// The package the declaration names.
        package: String,
    },
    /// Every precondition holds but `plugins.max_active_instances` leaves no
    /// slot for it.
    OverInstanceCeiling,
}

/// The activation plan's own verdict on one explicit channel binding.
///
/// Anything that reports whether a `[channels.plugin.<alias>]` instance will
/// start, such as the CLI after binding one, asks this rather than restating
/// the admission rules, so the report cannot drift from what the runtime does
/// with the same config and installed packages. The per-declaration checks
/// run through the helper the plan's explicit pass uses, in the same order.
/// The ceiling verdict comes from building that plan and asking whether it
/// admitted this instance.
///
/// Like the plan, this is guest-free: it reads canonical config and the
/// manifests the host already admitted, never touches component bytes, and
/// runs no guest code. That is also why `Admitted` promises admission and not
/// a running channel.
///
/// The verdict describes `config` as given. A running daemon keeps acting on
/// the config it loaded until it restarts or reloads.
///
/// # Errors
///
/// Returns the plan's own error when this binding's instance identity cannot
/// be formed, for example from an alias containing control characters, and,
/// for a binding that meets every precondition, when the whole plan cannot be
/// built. The runtime starts no plugin instance in either case, because its
/// own plan fails the same way.
#[cfg(feature = "plugins-wasm")]
pub fn channel_binding_admission(
    config: &Config,
    host: &PluginHost,
    alias: &str,
) -> Result<ChannelBindingAdmission, PluginError> {
    if !config.plugins.enabled {
        return Ok(ChannelBindingAdmission::PluginsDisabled);
    }
    let Some(declaration) = config.channels.plugin.get(alias) else {
        return Ok(ChannelBindingAdmission::Undeclared);
    };
    let channel_packages = channel_package_names(host);
    let decision = decide_explicit_channel(config, host, &channel_packages, alias, declaration)?;
    let scope = match decision {
        ExplicitChannelDecision::Candidate(scope) => scope,
        ExplicitChannelDecision::Unmet(unmet) => return Ok(unmet),
    };
    // Nothing but the ceiling removes an explicit candidate from the plan: it
    // truncates the one sorted candidate sequence. So a candidate the plan
    // does not admit is one the ceiling left out.
    let plan = PluginActivationPlan::build(config, host)?;
    if plan.admits(scope.id().package(), PluginCapability::Channel, alias) {
        Ok(ChannelBindingAdmission::Admitted)
    } else {
        Ok(ChannelBindingAdmission::OverInstanceCeiling)
    }
}

/// One declaration's outcome in the plan's explicit pass.
#[cfg(feature = "plugins-wasm")]
enum ExplicitChannelDecision {
    /// Every per-declaration precondition holds: the plan places this scope
    /// among its explicit candidates, where the shared ceiling still applies.
    Candidate(PluginInstanceScope),
    /// The first per-declaration precondition that does not hold.
    Unmet(ChannelBindingAdmission),
}

/// The explicit pass's rule for one `[channels.plugin.<alias>]` declaration.
///
/// This is the one statement of that rule: [`PluginActivationPlan::build`]
/// keeps the candidates it yields and [`channel_binding_admission`] reports
/// its refusals, so a change here changes both. A declaration is a candidate
/// only when it is enabled, an enabled agent owns it, it names a package the
/// host admitted, and that package declares the `channel` capability. The
/// checks run in that order and stop at the first that fails, which is the
/// order [`ChannelBindingAdmission`] lists them in.
///
/// `channel_packages` is [`channel_package_names`] of the same host, taken as
/// an argument so the plan computes it once for all declarations.
///
/// # Errors
///
/// A declaration that passes every check but whose alias cannot name a plugin
/// instance is an error rather than a refusal: it fails the plan as a whole.
#[cfg(feature = "plugins-wasm")]
fn decide_explicit_channel(
    config: &Config,
    host: &PluginHost,
    channel_packages: &HashSet<&str>,
    alias: &str,
    declaration: &zeroclaw_config::schema::PluginChannelConfig,
) -> Result<ExplicitChannelDecision, PluginError> {
    if !declaration.enabled {
        return Ok(ExplicitChannelDecision::Unmet(
            ChannelBindingAdmission::BindingDisabled,
        ));
    }
    if !has_enabled_owner(config, alias) {
        return Ok(ExplicitChannelDecision::Unmet(
            ChannelBindingAdmission::NoEnabledOwner,
        ));
    }
    let Some(manifest) = host.manifest(&declaration.package) else {
        return Ok(ExplicitChannelDecision::Unmet(
            ChannelBindingAdmission::PackageNotInstalled {
                package: declaration.package.clone(),
            },
        ));
    };
    if !channel_packages.contains(manifest.name.as_str()) {
        return Ok(ExplicitChannelDecision::Unmet(
            ChannelBindingAdmission::NotAChannelPackage {
                package: declaration.package.clone(),
            },
        ));
    }
    Ok(ExplicitChannelDecision::Candidate(
        PluginInstanceScope::from_manifest(
            manifest,
            PluginCapability::Channel,
            alias,
            manifest.permissions.iter().copied(),
        )?,
    ))
}

/// Names of the admitted packages an explicit channel binding may name: the
/// ones the host lists as channel plugins.
#[cfg(feature = "plugins-wasm")]
fn channel_package_names(host: &PluginHost) -> HashSet<&str> {
    host.channel_plugin_details()
        .into_iter()
        .map(|(manifest, _)| manifest.name.as_str())
        .collect()
}

/// An explicit channel binding activates only when some enabled agent actually
/// routes to it. Without an owner the listener would run with nothing to
/// deliver to, so an orphaned declaration is inert rather than half-live.
#[cfg(feature = "plugins-wasm")]
fn has_enabled_owner(config: &Config, binding: &str) -> bool {
    has_enabled_owner_ref(config, &format!("plugin.{binding}"))
}

/// The same ownership rule addressed by full composite channel reference.
///
/// A mirror is routed as `<provides>.<alias>`, not `plugin.<alias>`, so it
/// asks this directly rather than through the `plugin.` shorthand above.
#[cfg(feature = "plugins-wasm")]
fn has_enabled_owner_ref(config: &Config, channel_ref: &str) -> bool {
    config.agents.values().any(|agent| {
        agent.enabled
            && agent
                .channels
                .iter()
                .any(|configured| configured.as_str() == channel_ref)
    })
}

/// The plugin family's own channel namespace: an explicit
/// `[channels.plugin.<alias>]` binding runs as the channel `plugin.<alias>`.
///
/// `[channels.plugin.<alias>]` declarations are bound to a package by their
/// `package` field, so this family is never a mirror target.
#[cfg(feature = "plugins-wasm")]
pub const PLUGIN_CHANNEL_FAMILY: &str = "plugin";

/// How many installed channel packages claim each mirrored channel id.
///
/// Counted across every channel package before any admission decision, so an
/// ambiguous id is refused for all claimants rather than resolved by install
/// order.
#[cfg(feature = "plugins-wasm")]
fn mirror_claim_counts(host: &PluginHost) -> HashMap<String, usize> {
    let mut claims: HashMap<String, usize> = HashMap::new();
    for (manifest, _) in host.channel_plugin_details() {
        if let Some(provides) = manifest.provides.as_deref() {
            *claims.entry(provides.to_string()).or_default() += 1;
        }
    }
    claims
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
/// Materialize the configured store limits used by every plugin constructor.
/// CLI load verification uses this same resolver so its result cannot diverge
/// from the limits applied when the daemon later constructs the plugin.
pub fn plugin_limits(config: &Config) -> zeroclaw_plugins::component::PluginLimits {
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

/// Plugin grants are compared verbatim: a plugin names its senders exactly, with
/// none of the per-platform identity rules the chat channels carry. Denies still
/// have to be decoded and applied first, which is what the shared policy helper
/// is for.
#[cfg(feature = "plugins-wasm")]
fn plugin_sender_allowed(config: &Config, alias: &str, sender: &str) -> bool {
    zeroclaw_config::schema::peer_policy_admits(
        &config.channel_external_peers("plugin", alias),
        &[sender],
        |entry, user| entry == user,
    )
}

#[cfg(feature = "plugins-wasm")]
fn channel_sender_authorizer(
    config: Arc<Config>,
    live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
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
    live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
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
    live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
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
        // One host-owned egress authority for every channel in this plan. It is
        // shared, not per-channel: each store carries its own instance scope, and
        // the service resolves reach from canonical config against that scope's
        // `config_entry_key()` at request time. The same live view is cloned
        // into config reads and sender authorization, so each resolves the
        // current canonical row independently rather than snapshotting it.
        let egress_service =
            crate::tools::plugin_egress_service(Arc::clone(&config), live_config.clone());
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
            let Some((manifest, component)) = details
                .iter()
                .find(|(manifest, _)| manifest.name == package)
                .map(|(manifest, component)| (*manifest, *component))
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
                component,
                &host_services,
                limits,
                Some(egress_service.clone()),
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

        let live = zeroclaw_config::live::LiveConfig::new(Config::default());
        let authorizer = channel_sender_authorizer(
            Arc::new(Config::default()),
            Some(live.handle()),
            "operations".to_string(),
        );
        assert!(!authorizer("alice"), "empty peer groups deny by default");

        let publish = |mutating: &dyn Fn(&mut zeroclaw_config::schema::Config)| {
            let mut next = live.snapshot();
            mutating(&mut next);
            let revision = live.next_revision().unwrap();
            live.publish(revision, next).unwrap();
        };
        publish(&|config| {
            config.peer_groups.insert(
                "operators".to_string(),
                PeerGroupConfig {
                    channel: ChannelRef::new("plugin.operations"),
                    external_peers: vec![PeerUsername::new("alice")],
                    ..PeerGroupConfig::default()
                },
            );
        });
        assert!(authorizer("alice"));
        assert!(!authorizer("Alice"), "plugin sender identity is exact");

        publish(&|config| {
            config
                .peer_groups
                .get_mut("operators")
                .expect("operator peer group")
                .external_peers = vec![PeerUsername::new("*")];
        });
        assert!(
            authorizer("anyone"),
            "wildcard uses native channel semantics"
        );
    }

    /// `channel_external_peers` carries every `ignore` entry as an encoded deny
    /// marker and applies none of them, so the ingress has to. Both plugin
    /// delivery paths, polling and webhook, share this predicate.
    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn channel_sender_policy_applies_denies_before_grants() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        let policy = |grants: &[&str], ignore: &[&str]| {
            let mut config = Config::default();
            config.peer_groups.insert(
                "operators".to_string(),
                PeerGroupConfig {
                    channel: ChannelRef::new("plugin.operations"),
                    external_peers: grants.iter().map(|p| PeerUsername::new(*p)).collect(),
                    ignore: ignore.iter().map(|p| PeerUsername::new(*p)).collect(),
                    ..PeerGroupConfig::default()
                },
            );
            let live = zeroclaw_config::live::LiveConfig::new(config);
            channel_sender_authorizer(
                Arc::new(Config::default()),
                Some(live.handle()),
                "operations".to_string(),
            )
        };

        let wildcard = policy(&["*"], &["alice"]);
        assert!(!wildcard("alice"), "an explicit deny outranks a wildcard");
        assert!(wildcard("bob"), "the wildcard still admits everyone else");

        let exact = policy(&["alice", "bob"], &["alice"]);
        assert!(!exact("alice"), "an explicit deny outranks an exact grant");
        assert!(exact("bob"), "the sibling grant is untouched");

        let deny_all = policy(&["*"], &["*"]);
        assert!(!deny_all("alice"), "`ignore = [\"*\"]` denies everyone");

        let granted = policy(&["alice"], &[]);
        assert!(granted("alice"), "control: no deny, the grant stands");
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

    /// A channel package that mirrors a compiled-in channel id.
    ///
    /// `config_read` is spelled out per case because its absence is itself an
    /// admission rule under test.
    fn write_mirror_plugin(root: &Path, name: &str, provides: &str, config_read: bool) {
        let plugin_dir = root.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let permissions = if config_read {
            "permissions = [\"config_read\"]\n"
        } else {
            ""
        };
        // `config_read` is only valid alongside a config_schema, so a mirror
        // that reads canonical config must declare one.
        let schema = if config_read {
            "config_schema = { type = \"object\", properties = {}, additionalProperties = false }\n"
        } else {
            ""
        };
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"channel\"]\nprovides = \"{provides}\"\n{permissions}{schema}"
            ),
        )
        .unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"not a component").unwrap();
    }

    /// Canonical config with one telegram alias and an agent routing to it.
    fn mirror_config(plugins_dir: &Path, alias: &str, enabled: bool, owned: bool) -> Config {
        let mut config = Config::default();
        config.plugins.enabled = true;
        config.plugins.auto_discover = false;
        config.plugins.max_active_instances = 10;
        config.plugins.plugins_dir = plugins_dir.display().to_string();
        config.channels.telegram = HashMap::from([(
            alias.to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled,
                ..zeroclaw_config::schema::TelegramConfig::default()
            },
        )]);
        let agent = AliasedAgentConfig {
            channels: if owned {
                vec![ChannelRef::new(format!("telegram.{alias}"))]
            } else {
                Vec::new()
            },
            ..AliasedAgentConfig::default()
        };
        config.agents = HashMap::from([("operator".to_string(), agent)]);
        config
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
    fn a_mirror_admits_one_instance_per_configured_enabled_alias() {
        // The whole point of `provides`: canonical channel config, not a
        // second config home, decides how many instances exist.
        let plugins = TempDir::new().unwrap();
        write_mirror_plugin(plugins.path(), "tg-mirror", "telegram", true);
        let mut config = mirror_config(plugins.path(), "main", true, true);
        config.channels.telegram.insert(
            "backup".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                ..zeroclaw_config::schema::TelegramConfig::default()
            },
        );
        config
            .agents
            .get_mut("operator")
            .unwrap()
            .channels
            .push(ChannelRef::new("telegram.backup"));
        let host = plugin_host(&config).unwrap();

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        let mirrors = plan.mirror_identities();
        assert!(
            mirrors.contains(&("tg-mirror".to_string(), "main".to_string())),
            "configured alias should be recorded, got {mirrors:?}"
        );
        assert!(
            mirrors.contains(&("tg-mirror".to_string(), "backup".to_string())),
            "second configured alias should get its own instance, got {mirrors:?}"
        );
        // The load-bearing half of this slice being inert: nothing that
        // constructs a guest may see a mirror yet.
        assert!(
            plan.scopes(PluginCapability::Channel).next().is_none(),
            "a mirror must not reach the construction feed"
        );
    }

    #[test]
    fn a_mirror_skips_disabled_and_unowned_aliases() {
        // A disabled alias is not a channel, and an alias no enabled agent
        // routes to has nothing to deliver to. Neither should start a guest.
        for (enabled, owned) in [(false, true), (true, false)] {
            let plugins = TempDir::new().unwrap();
            write_mirror_plugin(plugins.path(), "tg-mirror", "telegram", true);
            let config = mirror_config(plugins.path(), "main", enabled, owned);
            let host = plugin_host(&config).unwrap();

            let plan = PluginActivationPlan::build(&config, &host).unwrap();

            assert!(
                plan.mirror_identities().is_empty(),
                "enabled={enabled} owned={owned} should record nothing, got {:?}",
                plan.mirror_identities()
            );
        }
    }

    #[test]
    fn a_mirror_without_config_read_is_refused() {
        // A mirror is defined by being fed the alias's canonical config. With
        // no grant to read it the instance would run blind, so it is refused
        // rather than started against an empty object.
        let plugins = TempDir::new().unwrap();
        write_mirror_plugin(plugins.path(), "tg-mirror", "telegram", false);
        let config = mirror_config(plugins.path(), "main", true, true);
        let host = plugin_host(&config).unwrap();

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert!(
            plan.mirror_identities().is_empty(),
            "a mirror without config_read must not be recorded"
        );
    }

    #[test]
    fn a_package_cannot_mirror_the_plugin_family_to_claim_another_packages_route() {
        // The route takeover this closes. `plugin` is a member of
        // V3_CHANNEL_TYPES, so before the reserved-family refusal a package
        // declaring provides = "plugin" enumerated every enabled
        // `[channels.plugin.<alias>]` and claimed those aliases WITHOUT
        // reading the declaration's `package` field. Since the endpoint is
        // stamped `plugin.<alias>` for both, the later package by sort order
        // won the channel map and inherited the legitimate package's route:
        // its replies, its cron delivery, its sender policy.
        let plugins = TempDir::new().unwrap();
        write_executable_plugin(plugins.path(), "real", &["channel"]);
        // Sorts after "real", which is what made it win the map.
        write_mirror_plugin(plugins.path(), "zz-hijacker", "plugin", true);

        let mut config = Config::default();
        config.plugins.enabled = true;
        config.plugins.auto_discover = false;
        config.plugins.max_active_instances = 10;
        config.plugins.plugins_dir = plugins.path().display().to_string();
        config.channels.plugin = HashMap::from([(
            "main".to_string(),
            PluginChannelConfig {
                package: "real".to_string(),
                enabled: true,
            },
        )]);
        config.agents = HashMap::from([(
            "operator".to_string(),
            AliasedAgentConfig {
                channels: vec![ChannelRef::new("plugin.main")],
                ..AliasedAgentConfig::default()
            },
        )]);
        let host = plugin_host(&config).unwrap();

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        // Non-vacuous: the legitimate declaration still admits, so this test
        // fails if mirror admission is simply broken rather than refused.
        assert!(
            plan.scope("real", PluginCapability::Channel, "main")
                .is_some(),
            "the package the operator bound to plugin.main must still be admitted"
        );
        assert!(
            plan.scope("zz-hijacker", PluginCapability::Channel, "main")
                .is_none(),
            "a provides=\"plugin\" package must not be admitted at another package's alias"
        );
        assert!(
            plan.mirror_identities().is_empty(),
            "the reserved plugin family must record no mirror, got {:?}",
            plan.mirror_identities()
        );
        // The routing key the orchestrator would publish must have exactly one
        // claimant, which is the property the takeover violated.
        let claimants: Vec<_> = plan
            .scopes(PluginCapability::Channel)
            .filter(|scope| scope.id().binding() == "main")
            .map(|scope| scope.id().package().to_string())
            .collect();
        assert_eq!(
            claimants,
            vec!["real".to_string()],
            "plugin.main must have exactly one claimant"
        );
    }

    #[test]
    fn two_packages_mirroring_one_id_both_fail_closed() {
        // The host will not pick a mirror on the operator's behalf, and it
        // must not resolve the tie by install order either: both are refused.
        let plugins = TempDir::new().unwrap();
        write_mirror_plugin(plugins.path(), "tg-mirror", "telegram", true);
        write_mirror_plugin(plugins.path(), "tg-other", "telegram", true);
        let config = mirror_config(plugins.path(), "main", true, true);
        let host = plugin_host(&config).unwrap();

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert!(
            plan.mirror_identities().is_empty(),
            "ambiguous providers must fail closed, got {:?}",
            plan.mirror_identities()
        );
    }

    #[test]
    fn mirrors_are_bounded_by_the_instance_ceiling() {
        // Mirrors are explicit, so they take ceiling priority over
        // auto-discovery — but they are not exempt from the ceiling itself.
        let plugins = TempDir::new().unwrap();
        write_mirror_plugin(plugins.path(), "tg-mirror", "telegram", true);
        let mut config = mirror_config(plugins.path(), "main", true, true);
        config.channels.telegram.insert(
            "backup".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                ..zeroclaw_config::schema::TelegramConfig::default()
            },
        );
        config
            .agents
            .get_mut("operator")
            .unwrap()
            .channels
            .push(ChannelRef::new("telegram.backup"));
        config.plugins.max_active_instances = 1;
        let host = plugin_host(&config).unwrap();

        let plan = PluginActivationPlan::build(&config, &host).unwrap();

        assert_eq!(
            plan.mirror_identities().len(),
            1,
            "the configured number bounds recorded mirrors"
        );
        assert!(
            plan.scopes(PluginCapability::Channel).next().is_none(),
            "bounded or not, a mirror must not reach construction"
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

    /// Declare `[channels.plugin.<alias>]` for `package`, routed to by the
    /// fixture's operator agent when `owned`.
    fn declare_binding(
        config: &mut Config,
        alias: &str,
        package: &str,
        enabled: bool,
        owned: bool,
    ) {
        // Every field is named today; the base keeps this literal compiling
        // when `PluginChannelConfig` gains one.
        #[allow(clippy::needless_update)]
        config.channels.plugin.insert(
            alias.to_string(),
            PluginChannelConfig {
                package: package.to_string(),
                enabled,
                ..PluginChannelConfig::default()
            },
        );
        if owned {
            config
                .agents
                .get_mut("operator")
                .unwrap()
                .channels
                .push(ChannelRef::new(format!("plugin.{alias}")));
        }
    }

    #[test]
    fn binding_admission_admits_an_enabled_owned_channel_binding() {
        let (_plugins, config, host) = fixture();

        assert_eq!(
            channel_binding_admission(&config, &host, "ops").unwrap(),
            ChannelBindingAdmission::Admitted
        );
        let plan = PluginActivationPlan::build(&config, &host).unwrap();
        assert!(
            plan.admits("alpha", PluginCapability::Channel, "ops"),
            "the plan the runtime builds must admit the same instance"
        );
    }

    #[test]
    fn binding_admission_reports_a_disabled_plugin_system_before_any_binding_rule() {
        let (_plugins, mut config, host) = fixture();
        config.plugins.enabled = false;
        // A second unmet precondition on the same binding, so the order of
        // the two is what this observes.
        config.channels.plugin.get_mut("ops").unwrap().enabled = false;

        for alias in ["ops", "backup", "absent"] {
            assert_eq!(
                channel_binding_admission(&config, &host, alias).unwrap(),
                ChannelBindingAdmission::PluginsDisabled,
                "alias {alias}"
            );
        }
    }

    #[test]
    fn binding_admission_reports_an_alias_with_no_declaration_as_undeclared() {
        let (_plugins, mut config, host) = fixture();
        // An agent route alone does not declare a binding.
        config
            .agents
            .get_mut("operator")
            .unwrap()
            .channels
            .push(ChannelRef::new("plugin.absent"));

        assert_eq!(
            channel_binding_admission(&config, &host, "absent").unwrap(),
            ChannelBindingAdmission::Undeclared
        );
    }

    #[test]
    fn binding_admission_reports_a_disabled_binding_before_its_missing_owner() {
        let (_plugins, mut config, host) = fixture();
        config.channels.plugin.get_mut("ops").unwrap().enabled = false;
        // The only agent is disabled too, so the owner check would also fail.
        config.agents.get_mut("operator").unwrap().enabled = false;

        assert_eq!(
            channel_binding_admission(&config, &host, "ops").unwrap(),
            ChannelBindingAdmission::BindingDisabled
        );
    }

    #[test]
    fn binding_admission_reports_a_binding_no_enabled_agent_routes_to() {
        let (_plugins, mut config, host) = fixture();
        // Unrouted and naming a package that is not installed: ownership is
        // checked first, so that is the precondition reported.
        declare_binding(&mut config, "orphan", "not-installed", true, false);

        assert_eq!(
            channel_binding_admission(&config, &host, "orphan").unwrap(),
            ChannelBindingAdmission::NoEnabledOwner
        );

        // A route from a disabled agent does not own a binding either.
        config.agents.get_mut("operator").unwrap().enabled = false;
        assert_eq!(
            channel_binding_admission(&config, &host, "ops").unwrap(),
            ChannelBindingAdmission::NoEnabledOwner
        );
    }

    #[test]
    fn binding_admission_names_the_uninstalled_package_a_binding_declares() {
        let (_plugins, mut config, host) = fixture();
        declare_binding(&mut config, "missing", "not-installed", true, true);

        assert_eq!(
            channel_binding_admission(&config, &host, "missing").unwrap(),
            ChannelBindingAdmission::PackageNotInstalled {
                package: "not-installed".to_string()
            }
        );
    }

    #[test]
    fn binding_admission_names_an_installed_package_without_the_channel_capability() {
        let (_plugins, mut config, host) = fixture();
        // `zeta` is an executable tool package; `beta` ships only skills.
        declare_binding(&mut config, "toolonly", "zeta", true, true);
        declare_binding(&mut config, "skillonly", "beta", true, true);

        assert_eq!(
            channel_binding_admission(&config, &host, "toolonly").unwrap(),
            ChannelBindingAdmission::NotAChannelPackage {
                package: "zeta".to_string()
            }
        );
        assert_eq!(
            channel_binding_admission(&config, &host, "skillonly").unwrap(),
            ChannelBindingAdmission::NotAChannelPackage {
                package: "beta".to_string()
            }
        );
    }

    #[test]
    fn binding_admission_reports_a_valid_binding_the_instance_ceiling_leaves_out() {
        // Explicit bindings sort by package, then alias, ahead of every
        // auto-discovered candidate: `backup` takes the only slot and `ops`,
        // which meets every other precondition, is truncated.
        let (_plugins, mut config, host) = fixture();
        config.plugins.max_active_instances = 1;

        assert_eq!(
            channel_binding_admission(&config, &host, "backup").unwrap(),
            ChannelBindingAdmission::Admitted
        );
        assert_eq!(
            channel_binding_admission(&config, &host, "ops").unwrap(),
            ChannelBindingAdmission::OverInstanceCeiling
        );

        // A zero ceiling leaves no slot for any binding.
        config.plugins.max_active_instances = 0;
        for alias in ["backup", "ops"] {
            assert_eq!(
                channel_binding_admission(&config, &host, alias).unwrap(),
                ChannelBindingAdmission::OverInstanceCeiling,
                "alias {alias}"
            );
        }
    }

    #[test]
    fn binding_admission_agrees_with_the_activation_plan_for_every_declared_alias() {
        let (_plugins, mut config, host) = fixture();
        declare_binding(&mut config, "disabled", "alpha", false, true);
        declare_binding(&mut config, "orphan", "alpha", true, false);
        declare_binding(&mut config, "missing", "not-installed", true, true);
        declare_binding(&mut config, "toolonly", "zeta", true, true);
        // Valid, but sorts after `backup` and `ops` under a two-slot ceiling.
        declare_binding(&mut config, "zulu", "alpha", true, true);
        config.plugins.max_active_instances = 2;

        for plugins_enabled in [true, false] {
            config.plugins.enabled = plugins_enabled;
            let plan = PluginActivationPlan::build(&config, &host).unwrap();

            let mut verdicts = std::collections::BTreeMap::new();
            for (alias, declaration) in &config.channels.plugin {
                let verdict = channel_binding_admission(&config, &host, alias).unwrap();
                let planned = plan
                    .scope(&declaration.package, PluginCapability::Channel, alias)
                    .is_some();
                assert_eq!(
                    verdict == ChannelBindingAdmission::Admitted,
                    planned,
                    "plugins.enabled={plugins_enabled}, alias {alias}: the query said {verdict:?}"
                );
                verdicts.insert(alias.clone(), verdict);
            }

            // Not vacuous: across both iterations the mixed config reaches
            // every verdict a declared alias can get, so agreement is shown
            // for each one.
            let expected = if plugins_enabled {
                std::collections::BTreeMap::from([
                    ("backup".to_string(), ChannelBindingAdmission::Admitted),
                    (
                        "disabled".to_string(),
                        ChannelBindingAdmission::BindingDisabled,
                    ),
                    (
                        "missing".to_string(),
                        ChannelBindingAdmission::PackageNotInstalled {
                            package: "not-installed".to_string(),
                        },
                    ),
                    ("ops".to_string(), ChannelBindingAdmission::Admitted),
                    (
                        "orphan".to_string(),
                        ChannelBindingAdmission::NoEnabledOwner,
                    ),
                    (
                        "toolonly".to_string(),
                        ChannelBindingAdmission::NotAChannelPackage {
                            package: "zeta".to_string(),
                        },
                    ),
                    (
                        "zulu".to_string(),
                        ChannelBindingAdmission::OverInstanceCeiling,
                    ),
                ])
            } else {
                config
                    .channels
                    .plugin
                    .keys()
                    .map(|alias| (alias.clone(), ChannelBindingAdmission::PluginsDisabled))
                    .collect()
            };
            assert_eq!(verdicts, expected, "plugins.enabled={plugins_enabled}");
        }
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
