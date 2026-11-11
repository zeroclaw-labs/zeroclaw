//! Provider/model construction from the canonical configuration selection.
use crate::ModelProvider;
use std::collections::HashSet;
use std::sync::Arc;
use zeroclaw_config::schema::{Config, ModelIdentity};

/// A provider and its configuration-derived selection, built together.
/// Billing uses `provider_name`; selection-sensitive consumers use `identity`.
pub struct BuiltModel {
    provider: Box<dyn ModelProvider>,
    identity: ModelIdentity,
    model_route_resolver: Arc<crate::router::ModelRouteResolver>,
}

impl BuiltModel {
    /// Bind an explicitly injected provider to its request identity, without
    /// adding configured routing or reliability policy.
    pub fn from_provider(provider: Box<dyn ModelProvider>, identity: ModelIdentity) -> Self {
        let model_route_resolver = Arc::new(crate::router::ModelRouteResolver::new(
            Vec::new(),
            identity.provider_ref(),
            identity.model_id.clone(),
        ));
        Self {
            provider,
            identity,
            model_route_resolver,
        }
    }

    /// Bind a supplied routed provider and the resolver belonging to its request.
    pub fn from_routed_provider(
        provider: Box<dyn ModelProvider>,
        identity: ModelIdentity,
        model_route_resolver: Arc<crate::router::ModelRouteResolver>,
    ) -> Self {
        Self {
            provider,
            identity,
            model_route_resolver,
        }
    }

    pub fn provider(&self) -> &dyn ModelProvider {
        self.provider.as_ref()
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    pub fn model_name(&self) -> &str {
        &self.identity.model_id
    }

    pub fn provider_name(&self) -> String {
        self.identity.profile_ref()
    }

    pub fn route_resolver(&self) -> Arc<crate::router::ModelRouteResolver> {
        Arc::clone(&self.model_route_resolver)
    }

    pub fn into_provider(self) -> Box<dyn ModelProvider> {
        self.provider
    }

    pub fn into_parts(
        self,
    ) -> (
        Box<dyn ModelProvider>,
        ModelIdentity,
        Arc<crate::router::ModelRouteResolver>,
    ) {
        (self.provider, self.identity, self.model_route_resolver)
    }
}

/// Only invocation inputs belong here; configured connection and tuning are
/// resolved from the supplied generation and selection reference.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderOverrides<'a> {
    pub model: Option<&'a str>,
    pub api_key: Option<&'a str>,
}

/// Whether a mutable root config property is materialized into every model
/// binding. Profile and route dependencies are resolved separately from Config.
pub fn shared_construction_depends_on_prop(prop: &str) -> bool {
    matches!(
        prop,
        "reliability"
            | "reliability.provider_retries"
            | "reliability.provider_backoff_ms"
            | "reliability.api_keys"
            | "runtime"
            | "runtime.reasoning_enabled"
            | "runtime.reasoning_effort"
            | "secrets"
            | "secrets.encrypt"
            | "multimodal"
    ) || prop.starts_with("reliability.api_keys[")
        || prop.starts_with("multimodal.")
}

/// Run provider materialization outside the async runtime's polling context.
/// Some credential providers perform blocking OAuth refresh during construction.
pub fn build_nonblocking<T, F>(
    factory: F,
) -> impl std::future::Future<Output = anyhow::Result<T>> + Send
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    // Config-owning closures can be large. Box before creating the async
    // state machine so enclosing catalog and delegated calls retain a pointer
    // instead of embedding another complete configuration value.
    let factory = Box::new(factory);
    async move {
        use anyhow::Context;
        tokio::task::spawn_blocking(factory)
            .await
            .context("provider construction task failed")?
    }
}

/// Materialize a routed binding from an already admitted config generation.
pub async fn build_model_nonblocking(
    config: Arc<Config>,
    agent_alias: &str,
    selection_ref: &str,
    model_override: Option<&str>,
) -> anyhow::Result<BuiltModel> {
    let agent_alias = agent_alias.to_string();
    let selection_ref = selection_ref.to_string();
    let model_override = model_override.map(str::to_string);
    build_nonblocking(move || {
        build_model(
            &config,
            &agent_alias,
            &selection_ref,
            model_override.as_deref(),
        )
    })
    .await
}

/// Resolve auth readiness using the same typed-family policy as fallback admission.
/// Explicit endpoints may still be probed anonymously by their caller.
pub fn selection_auth_ready(config: &Config, selection_ref: &str) -> anyhow::Result<bool> {
    let canonical_ref = zeroclaw_config::provider_registry::ModelRef::from(selection_ref.trim());
    if config
        .providers
        .models
        .canonical_model(&canonical_ref)
        .is_some()
    {
        let registry = config.provider_registry()?;
        let selection = registry.resolve_chat(&canonical_ref)?;
        return Ok(crate::factory::canonical_auth_ready(
            selection.selection.connection,
        ));
    }
    let selection = config
        .resolve_model_selection(selection_ref)
        .ok_or_else(|| anyhow::Error::msg("model selection does not exist"))?;
    let mut options =
        crate::provider_runtime_options_for_alias(config, selection.family, selection.alias);
    crate::apply_model_entry_options(&mut options, selection.model_entry);
    Ok(crate::factory::fallback_auth_ready_for_alias(
        config,
        selection.family,
        selection.alias,
        selection.entry.api_key.as_deref(),
        &options,
    ))
}

/// Construct a configured transport without global routes or retry policy.
/// A model-less profile remains usable for catalog or caller-selected requests.
pub fn build_raw_provider(
    config: &Config,
    selection_ref: &str,
    overrides: ProviderOverrides<'_>,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    build_provider(config, selection_ref, overrides, false)
}

/// Construct one selection and its fallback chain, without global hint routes.
pub fn build_reliable_provider(
    config: &Config,
    selection_ref: &str,
    overrides: ProviderOverrides<'_>,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    build_provider(config, selection_ref, overrides, true)
}

/// Whether routed construction borrows model and connection defaults from its
/// agent rather than resolving a named provider profile.
pub fn inherits_agent_selection(config: &Config, selection_ref: &str) -> bool {
    if config
        .providers
        .models
        .canonical_model(&zeroclaw_config::provider_registry::ModelRef::from(
            selection_ref.trim(),
        ))
        .is_some()
    {
        return false;
    }
    !config
        .providers
        .models
        .profile_ref(selection_ref.trim())
        .is_some_and(|(family, _)| !family.contains(':'))
}

fn configured_selection_for_construction<'a>(
    config: &'a Config,
    agent_alias: &str,
    selection_ref: &str,
) -> Option<zeroclaw_config::schema::ModelSelection<'a>> {
    let selection_ref = selection_ref.trim();
    if !inherits_agent_selection(config, selection_ref) {
        config.resolve_model_selection(selection_ref)
    } else {
        config
            .agent(agent_alias)
            .and_then(|agent| config.resolve_model_selection(&agent.model_provider))
    }
}

/// Resolve construction identity without creating a transport. Bare families
/// and URL references inherit their model from the configured agent selection.
pub fn resolve_model_identity(
    config: &Config,
    agent_alias: &str,
    selection_ref: &str,
    model_override: Option<&str>,
) -> Option<ModelIdentity> {
    let selection_ref = selection_ref.trim();
    if config
        .providers
        .models
        .canonical_model(&zeroclaw_config::provider_registry::ModelRef::from(
            selection_ref,
        ))
        .is_some()
    {
        let model_id = model_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| config.resolve_model_id(selection_ref))?;
        return Some(ModelIdentity {
            family: selection_ref.to_owned(),
            alias: String::new(),
            entry_alias: None,
            model_id,
        });
    }
    let selection = configured_selection_for_construction(config, agent_alias, selection_ref);
    let model_id = model_override
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            selection
                .as_ref()
                .and_then(|selection| selection.model_id.clone())
        })?;
    match config.providers.models.profile_ref(selection_ref) {
        Some((family, _)) if !family.contains(':') => {
            selection.map(|selection| selection.identity(Some(&model_id)))
        }
        _ => Some(ModelIdentity {
            family: selection_ref.to_string(),
            alias: String::new(),
            entry_alias: None,
            model_id,
        }),
    }
}

fn build_provider(
    config: &Config,
    selection_ref: &str,
    overrides: ProviderOverrides<'_>,
    reliable: bool,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    let selection_ref = selection_ref.trim();

    // Canonical model references are exact registry keys. They must be
    // admitted before the V4 family/alias parser, which intentionally treats
    // dots as legacy profile boundaries. The initial migration boundary uses
    // the new resolver for both transport construction and the exact-key
    // fallback graph. The legacy family/alias parser is only considered when
    // this lookup does not find a canonical model.
    let canonical_ref = zeroclaw_config::provider_registry::ModelRef::from(selection_ref);
    if config
        .providers
        .models
        .canonical_model(&canonical_ref)
        .is_some()
    {
        let options = crate::provider_runtime_options_for_bare_family(config);
        let registry = config.provider_registry()?;
        let mut reliable_candidates = Vec::new();
        let mut raw_candidates = Vec::new();
        let mut refs = vec![canonical_ref.clone()];
        refs.extend(registry.fallback_chain(&canonical_ref)?);
        for candidate_ref in refs {
            // Reuse the immutable registry/connection descriptors for this
            // construction transaction, but resolve and materialize every
            // candidate's model policy independently.
            let provider = crate::factory::dispatch_model_ref_with_registry(
                &registry,
                &candidate_ref,
                &options,
                if candidate_ref == canonical_ref {
                    overrides.api_key
                } else {
                    None
                },
            )?;
            let configured_model = registry
                .resolve_chat(&candidate_ref)?
                .upstream_model()
                .unwrap_or(candidate_ref.as_str())
                .to_owned();
            let pinned_model = if candidate_ref == canonical_ref {
                overrides
                    .model
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(str::to_owned)
                    .unwrap_or(configured_model)
            } else {
                configured_model
            };
            if reliable {
                let provider: std::sync::Arc<dyn ModelProvider> = provider.into();
                reliable_candidates.push(
                    crate::reliable::ReliableModelProviderEntry::new_pinned(
                        candidate_ref.as_str(),
                        candidate_ref.as_str(),
                        candidate_ref.as_str(),
                        &pinned_model,
                        Box::new(provider),
                    )
                    .with_selection_ref(candidate_ref.as_str()),
                );
            } else {
                raw_candidates.push((pinned_model, provider));
            }
        }
        let provider: Box<dyn ModelProvider> = if reliable {
            Box::new(
                crate::reliable::ReliableModelProvider::new_with_entries(
                    selection_ref,
                    reliable_candidates,
                    config.reliability.provider_retries,
                    config.reliability.provider_backoff_ms,
                )
                .with_api_keys(config.reliability.api_keys.clone()),
            )
        } else {
            raw_candidates
                .into_iter()
                .next()
                .map(|(_, provider)| provider)
                .ok_or_else(|| anyhow::Error::msg("canonical model has no provider candidate"))?
        };
        let model = overrides.model.map(str::to_owned).or_else(|| {
            registry
                .resolve_chat(&canonical_ref)
                .ok()
                .and_then(|resolved| resolved.upstream_model().map(str::to_owned))
        });
        return Ok(crate::ResolvedModelProviderRef { provider, model });
    }
    crate::ensure_known_model_entry_ref(config, selection_ref)?;
    let selection = config.resolve_model_selection(selection_ref);
    let model = overrides
        .model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            selection
                .as_ref()
                .and_then(|selection| selection.model_id.clone())
        })
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let provider = if let Some(selection) = selection {
        let mut options =
            crate::provider_runtime_options_for_alias(config, selection.family, selection.alias);
        crate::apply_model_entry_options(&mut options, selection.model_entry);
        let key = overrides.api_key.or(selection.entry.api_key.as_deref());
        let url = selection.entry.uri.as_deref();
        if reliable {
            crate::create_resilient_model_provider_for_alias_with_model_override(
                config,
                selection.family,
                selection.alias,
                key,
                url,
                &config.reliability,
                &options,
                model.as_deref(),
                selection_ref,
            )?
        } else {
            crate::create_model_provider_inner(
                Some(config),
                selection.family,
                selection.alias,
                key,
                url,
                &options,
            )?
        }
    } else {
        // Preserve complete bare/URL references. Splitting an unknown dotted
        // alias here could silently construct the family's default transport.
        let options = crate::provider_runtime_options_for_bare_family(config);
        if reliable {
            crate::create_resilient_model_provider_with_options(
                selection_ref,
                overrides.api_key,
                None,
                &config.reliability,
                &options,
            )?
        } else {
            crate::create_model_provider_inner(
                None,
                selection_ref,
                "default",
                overrides.api_key,
                None,
                &options,
            )?
        }
    };
    Ok(crate::ResolvedModelProviderRef { provider, model })
}

/// Construct a canonical model with the caller's admitted runtime options.
/// This is kept separate from the legacy family parser so the exact ModelRef
/// remains intact through provider construction and fallback pinning.
pub(crate) fn build_canonical_provider_with_options(
    config: &Config,
    model_ref: &zeroclaw_config::provider_registry::ModelRef,
    overrides: ProviderOverrides<'_>,
    options: &crate::ModelProviderRuntimeOptions,
    pin_primary_model: bool,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    let registry = config.provider_registry()?;
    let mut candidates = Vec::new();
    let mut refs = vec![model_ref.clone()];
    refs.extend(registry.fallback_chain(model_ref)?);
    for candidate_ref in refs {
        let provider = crate::factory::dispatch_model_ref_with_registry(
            &registry,
            &candidate_ref,
            options,
            if candidate_ref == *model_ref {
                overrides.api_key
            } else {
                None
            },
        )?;
        let configured_model = registry
            .resolve_chat(&candidate_ref)?
            .upstream_model()
            .unwrap_or(candidate_ref.as_str())
            .to_owned();
        let pinned_model = if candidate_ref == *model_ref {
            overrides
                .model
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .unwrap_or(configured_model)
        } else {
            configured_model
        };
        let provider: std::sync::Arc<dyn ModelProvider> = provider.into();
        let candidate = if candidate_ref == *model_ref && !pin_primary_model {
            crate::reliable::ReliableModelProviderEntry::new_with_candidate(
                candidate_ref.as_str(),
                candidate_ref.as_str(),
                candidate_ref.as_str(),
                Box::new(provider),
            )
        } else {
            crate::reliable::ReliableModelProviderEntry::new_pinned(
                candidate_ref.as_str(),
                candidate_ref.as_str(),
                candidate_ref.as_str(),
                &pinned_model,
                Box::new(provider),
            )
        };
        candidates.push(candidate.with_selection_ref(candidate_ref.as_str()));
    }
    let provider = crate::reliable::ReliableModelProvider::new_with_entries(
        model_ref.as_str(),
        candidates,
        config.reliability.provider_retries,
        config.reliability.provider_backoff_ms,
    )
    .with_api_keys(config.reliability.api_keys.clone());
    let model = overrides
        .model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            registry
                .resolve_chat(model_ref)
                .ok()?
                .upstream_model()
                .map(str::to_owned)
        });
    Ok(crate::ResolvedModelProviderRef {
        provider: Box::new(provider),
        model,
    })
}

/// Resolve materialized fallback dependencies on demand, without retaining a
/// parallel graph. Call against both generations when an edit removes a ref.
pub fn selection_depends_on_model_ref(
    config: &Config,
    selection_ref: &str,
    changed_model_ref: &str,
) -> bool {
    let selection_ref = selection_ref.trim();
    let changed_model_ref = changed_model_ref.trim();
    // V4 profile references such as `custom.gateway.fast` still resolve
    // their connection and defaults from `custom.gateway`. Keep live-edit
    // invalidation wired to that owner while canonical dotted model keys
    // remain opaque TOML keys and take precedence over the legacy parser.
    let selection_is_canonical = config
        .providers
        .models
        .canonical_model(&zeroclaw_config::provider_registry::ModelRef::from(
            selection_ref,
        ))
        .is_some();
    let changed_is_canonical = config
        .providers
        .models
        .canonical_model(&zeroclaw_config::provider_registry::ModelRef::from(
            changed_model_ref,
        ))
        .is_some();
    if !selection_is_canonical
        && !changed_is_canonical
        && let (Some((selection_family, selection_alias)), Some((changed_family, changed_alias))) = (
            config.providers.models.profile_ref(selection_ref),
            config.providers.models.profile_ref(changed_model_ref),
        )
    {
        return selection_family == changed_family && selection_alias == changed_alias;
    }
    let Ok(registry) = config.provider_registry() else {
        return false;
    };
    let root = zeroclaw_config::provider_registry::ModelRef::from(selection_ref);
    let changed = zeroclaw_config::provider_registry::ModelRef::from(changed_model_ref);
    fn visit(
        registry: &zeroclaw_config::provider_registry::ProviderRegistry,
        current: &zeroclaw_config::provider_registry::ModelRef,
        changed: &zeroclaw_config::provider_registry::ModelRef,
        visited: &mut HashSet<zeroclaw_config::provider_registry::ModelRef>,
        depth: usize,
    ) -> bool {
        if current == changed {
            return true;
        }
        if depth >= zeroclaw_config::providers::MAX_FALLBACK_DEPTH
            || !visited.insert(current.clone())
        {
            return false;
        }
        registry.fallback.get(current).is_some_and(|targets| {
            targets
                .iter()
                .any(|target| visit(registry, target, changed, visited, depth + 1))
        })
    }
    visit(&registry, &root, &changed, &mut HashSet::new(), 0)
}

/// Dependencies of the routed graph built by [`build_model`], including each
/// configured hint target's independent reliability domain and the agent
/// selection inherited by a bare family or URL reference.
pub fn routed_selection_depends_on_model_ref(
    config: &Config,
    agent_alias: &str,
    selection_ref: &str,
    changed_model_ref: &str,
) -> bool {
    let inherited_selection = if !inherits_agent_selection(config, selection_ref) {
        false
    } else {
        let changed_owner = if config
            .providers
            .models
            .canonical_model(&zeroclaw_config::provider_registry::ModelRef::from(
                changed_model_ref.trim(),
            ))
            .is_some()
        {
            changed_model_ref.trim().to_string()
        } else {
            config
                .providers
                .models
                .profile_ref(changed_model_ref)
                .map(|(family, alias)| format!("{family}.{alias}"))
                .unwrap_or_else(|| changed_model_ref.trim().to_string())
        };
        configured_selection_for_construction(config, agent_alias, selection_ref).is_some_and(
            |selection| format!("{}.{}", selection.family, selection.alias) == changed_owner,
        )
    };
    inherited_selection
        || selection_depends_on_model_ref(config, selection_ref, changed_model_ref)
        || config.model_routes.iter().any(|route| {
            !route.hint.trim().is_empty()
                && !route.effective_model(config).trim().is_empty()
                && selection_depends_on_model_ref(config, &route.model_ref, changed_model_ref)
        })
}

/// The single model constructor: initial construction, runtime switches, live
/// refresh, and session overrides all rebuild through here.
///
/// Callers supply only the ref (two- or three-segment, or a bare family name
/// for the legacy CLI `--provider <family>` form), an optional explicit
/// model; everything else (agent config, entry
/// overlay, temperature, context window, identity) is derived
/// from the full config. The raw ref is passed through to the providers
/// construction chain: that chain re-resolves the model entry from the ref it
/// is given, and feeding it a normalised two-segment ref would select
/// `models.default` and clobber the selected entry's tuning overlay.
pub fn build_model(
    config: &Config,
    agent_alias: &str,
    model_provider_ref: &str,
    model_override: Option<&str>,
) -> anyhow::Result<BuiltModel> {
    let model_provider_ref = model_provider_ref.trim();
    let canonical_ref = zeroclaw_config::provider_registry::ModelRef::from(model_provider_ref);
    if config
        .providers
        .models
        .canonical_model(&canonical_ref)
        .is_some()
    {
        let registry = config.provider_registry()?;
        registry.resolve_chat(&canonical_ref)?;
        let options = crate::provider_runtime_options_for_agent(config, agent_alias);
        let model_id = model_override
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                registry
                    .resolve_chat(&canonical_ref)
                    .ok()?
                    .upstream_model()
                    .map(str::to_owned)
            })
            .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model_provider `{model_provider_ref}` has no `model` configured and no model override was supplied"
            ))
            })?;
        let identity = ModelIdentity {
            family: model_provider_ref.to_owned(),
            alias: String::new(),
            entry_alias: None,
            model_id: model_id.clone(),
        };
        let (provider, resolver) = crate::create_routed_model_provider_with_options_and_resolver(
            config,
            model_provider_ref,
            None,
            None,
            &config.reliability,
            &config.model_routes,
            &model_id,
            &options,
        )?;
        return Ok(BuiltModel::from_routed_provider(
            provider, identity, resolver,
        ));
    }
    let (_, explicit_url) = crate::split_v2_colon_url(model_provider_ref);
    crate::ensure_known_model_entry_ref(config, model_provider_ref)?;
    // A two- or three-segment ref names a profile; a third segment selects a
    // model entry under the profile's `models` map. A bare family inherits
    // the agent selection's model and connection, with runtime options
    // inherited from the agent's own profile with the provider-specific bits
    // cleared (exactly what `options_for_provider_ref` gives a bare name).
    // Bare families and URL references need no configured profile. A dotted
    // profile reference must resolve rather than using a family's defaults.
    let (model_provider_name, model_provider_alias, bare_family) =
        match config.providers.models.profile_ref(model_provider_ref) {
            Some((family, alias)) if !family.contains(':') => {
                (family.to_string(), alias.to_string(), false)
            }
            _ => (model_provider_ref.to_string(), String::new(), true),
        };

    // Named refs select their own profile. Bare selectors inherit model
    // defaults, but only the same bare family may reuse the agent connection;
    // a different family or an explicit URL keeps its requested transport.
    let selection = configured_selection_for_construction(config, agent_alias, model_provider_ref);
    let inherit_connection = bare_family
        && explicit_url.is_none()
        && selection
            .as_ref()
            .is_some_and(|selected| selected.family == model_provider_name);
    let entry = selection
        .as_ref()
        .filter(|_| !bare_family || inherit_connection)
        .map(|selection| selection.entry);
    let model_entry = selection.as_ref().and_then(|s| s.model_entry);

    let identity = resolve_model_identity(config, agent_alias, model_provider_ref, model_override)
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model_provider `{model_provider_ref}` has no `model` configured and no model \
                 override was supplied"
            ))
        })?;
    let model_name = &identity.model_id;

    let mut runtime_options = if bare_family {
        // Reuse full connection options only for a same-family shorthand.
        // Other selectors inherit applicable model defaults after removing
        // the configured profile's transport-specific options.
        if inherit_connection {
            crate::provider_runtime_options_for_agent(config, agent_alias)
        } else {
            let agent_options = crate::provider_runtime_options_for_agent(config, agent_alias);
            crate::options_for_provider_ref(config, model_provider_ref, &agent_options)
        }
    } else {
        crate::provider_runtime_options_for_alias(
            config,
            &model_provider_name,
            &model_provider_alias,
        )
    };
    crate::apply_model_entry_options(&mut runtime_options, model_entry);

    let construction_ref = if inherit_connection {
        selection.as_ref().map_or_else(
            || model_provider_ref.to_owned(),
            |selected| {
                let profile = format!("{}.{}", selected.family, selected.alias);
                selected
                    .model_alias
                    .as_ref()
                    .map_or(profile.clone(), |model| format!("{profile}.{model}"))
            },
        )
    } else {
        model_provider_ref.to_owned()
    };

    let (provider, model_route_resolver) =
        crate::create_routed_model_provider_with_options_and_resolver(
            config,
            &construction_ref,
            entry.and_then(|e| e.api_key.as_deref()),
            explicit_url.or_else(|| entry.and_then(|e| e.uri.as_deref())),
            &config.reliability,
            &config.model_routes,
            model_name,
            &runtime_options,
        )?;

    Ok(BuiltModel {
        provider,
        identity,
        model_route_resolver,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{ModelEntryConfig, ModelRouteConfig};

    #[test]
    fn bare_provider_override_keeps_the_requested_family() {
        use zeroclaw_api::attribution::{ModelProviderKind, ProviderKind, Role};
        let mut config = Config::default();
        let profile = config.providers.models.ensure("anthropic", "work").unwrap();
        profile.model = Some("claude-work".into());
        profile.api_key = Some("fixture-anthropic-key".into());
        profile.uri = Some("http://127.0.0.1:9/anthropic".into());
        config.agents.insert(
            "worker".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "anthropic.work".into(),
                ..Default::default()
            },
        );
        let built = build_model(&config, "worker", "openai", Some("gpt-test")).unwrap();
        assert_eq!(
            built.provider().role(),
            Role::Provider(ProviderKind::Model(ModelProviderKind::OpenAi))
        );
        assert_eq!(built.identity().provider_ref(), "openai");
        let built = build_model(
            &config,
            "worker",
            "custom:http://127.0.0.1:9/v1",
            Some("proxy-model"),
        )
        .unwrap();
        assert_eq!(
            built.provider().role(),
            Role::Provider(ProviderKind::Model(ModelProviderKind::Plugin))
        );
    }

    #[test]
    fn configured_options_preserve_an_absent_total_deadline() {
        let mut config = Config::default();
        config.providers.models.ensure("openai", "primary").unwrap();
        assert_eq!(
            crate::provider_runtime_options_for_alias(&config, "openai", "primary")
                .provider_timeout_secs,
            None
        );
        config
            .providers
            .models
            .ensure("openai", "primary")
            .unwrap()
            .timeout_secs = Some(37);
        assert_eq!(
            crate::provider_runtime_options_for_alias(&config, "openai", "primary")
                .provider_timeout_secs,
            Some(37)
        );
    }

    #[test]
    fn shared_construction_dependencies_exclude_unrelated_global_policy() {
        for prop in [
            "reliability.provider_retries",
            "reliability.provider_backoff_ms",
            "reliability.api_keys",
            "reliability.api_keys[0]",
            "runtime.reasoning_enabled",
            "runtime.reasoning_effort",
            "secrets.encrypt",
            "multimodal.max_images",
            "multimodal.vision_model_provider",
        ] {
            assert!(shared_construction_depends_on_prop(prop), "{prop}");
        }
        for prop in [
            "reliability.channel_initial_backoff_secs",
            "reliability.channel_max_backoff_secs",
            "reliability.scheduler_poll_secs",
            "reliability.scheduler_retries",
            "runtime.kind",
            "runtime.reasoning_effort_passthrough",
            "multimodal_extra.max_images",
            "providers.models.custom.primary.model",
            "agents.default.model_provider",
        ] {
            assert!(!shared_construction_depends_on_prop(prop), "{prop}");
        }
    }

    #[test]
    fn inherited_construction_identity_and_dependencies_follow_agent_selection() {
        let mut config = Config::default();
        let primary = config.providers.models.ensure("custom", "primary").unwrap();
        primary.uri = Some("http://127.0.0.1:1/v1".into());
        primary.models.insert(
            "fast".into(),
            ModelEntryConfig {
                id: Some("inherited-model".into()),
                ..Default::default()
            },
        );
        let backup = config.providers.models.ensure("custom", "backup").unwrap();
        backup.uri = Some("http://127.0.0.1:2/v1".into());
        backup.model = Some("backup-model".into());
        config.agents.insert(
            "worker".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "custom.primary.fast".into(),
                ..Default::default()
            },
        );

        for reference in ["custom", "custom:http://127.0.0.1:3/v1"] {
            assert!(inherits_agent_selection(&config, reference));
            let identity = resolve_model_identity(&config, "worker", reference, None).unwrap();
            assert_eq!(identity.provider_ref(), reference);
            assert_eq!(identity.model_id, "inherited-model");
            assert_eq!(
                build_model(&config, "worker", reference, None)
                    .unwrap()
                    .identity(),
                &identity
            );
            assert!(routed_selection_depends_on_model_ref(
                &config,
                "worker",
                reference,
                "custom.primary.fast"
            ));
            assert!(!routed_selection_depends_on_model_ref(
                &config,
                "worker",
                reference,
                "custom.backup"
            ));
            assert_eq!(
                resolve_model_identity(&config, "worker", reference, Some("explicit-model"))
                    .unwrap()
                    .model_id,
                "explicit-model"
            );
        }
        assert!(!routed_selection_depends_on_model_ref(
            &config,
            "worker",
            "custom.primary.fast",
            "custom.backup"
        ));
        assert!(!inherits_agent_selection(&config, "custom.missing"));
        assert!(resolve_model_identity(&config, "worker", "custom.missing", None).is_none());
    }

    #[test]
    fn canonical_fallback_dependencies_keep_dotted_model_keys_opaque() {
        let config: Config = toml::from_str(
            r#"
schema_version = 4

[providers.connections."gateway.prod"]
kind = "custom"
uri = "https://example.invalid/v1"
capabilities = ["chat"]

[providers.models."chat.prod.v1"]
capability = "chat"
provider = "gateway.prod"
model = "primary"

[providers.models."chat.backup.v2"]
capability = "chat"
provider = "gateway.prod"
model = "backup"

[providers.fallback]
"chat.prod.v1" = ["chat.backup.v2"]
"#,
        )
        .expect("canonical dotted model references parse");

        assert!(selection_depends_on_model_ref(
            &config,
            "chat.prod.v1",
            "chat.backup.v2"
        ));
        assert!(!selection_depends_on_model_ref(
            &config,
            "chat.backup.v2",
            "chat.prod.v1"
        ));
    }

    #[tokio::test]
    async fn canonical_route_key_override_applies_only_to_primary() {
        use serde_json::json;
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let config: Config = toml::from_str(&format!(
            r#"
schema_version = 4
[reliability]
provider_retries = 0
provider_backoff_ms = 0
[providers.connections."primary.prod"]
kind = "custom"
uri = "{0}/primary/v1"
capabilities = ["chat"]
auth = {{ mode = "api_key", api_key = "connection-test-key" }}
[providers.connections."backup.prod"]
kind = "custom"
uri = "{0}/backup/v1"
capabilities = ["chat"]
auth = {{ mode = "api_key", api_key = "backup-test-key" }}
[providers.models."chat.primary"]
capability = "chat"
provider = "primary.prod"
model = "primary-model"
[providers.models."chat.backup"]
capability = "chat"
provider = "backup.prod"
model = "backup-model"
[providers.fallback]
"chat.primary" = ["chat.backup"]
"#,
            server.uri()
        ))
        .unwrap();
        for mode in 0..3 {
            let overrides = ProviderOverrides {
                api_key: Some("route-test-key"),
                model: Some("route-model"),
            };
            let resolved = match mode {
                0 => build_raw_provider(&config, "chat.primary", overrides),
                1 => build_reliable_provider(&config, "chat.primary", overrides),
                _ => build_canonical_provider_with_options(
                    &config,
                    &"chat.primary".into(),
                    overrides,
                    &crate::provider_runtime_options_for_bare_family(&config),
                    true,
                ),
            }
            .unwrap();
            let success = ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
            }));
            let primary = Mock::given(method("POST"))
                .and(path("/primary/v1/chat/completions"))
                .and(header("authorization", "Bearer route-test-key"))
                .and(body_partial_json(json!({"model": "route-model"})))
                .respond_with(if mode == 0 {
                    success.clone()
                } else {
                    ResponseTemplate::new(500)
                })
                .expect(1)
                .mount_as_scoped(&server)
                .await;
            let backup = Mock::given(method("POST"))
                .and(path("/backup/v1/chat/completions"))
                .and(header("authorization", "Bearer backup-test-key"))
                .and(body_partial_json(json!({"model": "backup-model"})))
                .respond_with(success)
                .expect(if mode == 0 { 0 } else { 1 })
                .mount_as_scoped(&server)
                .await;
            let result = crate::ProviderDispatch::from_ref(resolved.provider.as_ref())
                .simple_chat("test", resolved.model.as_deref().unwrap(), None)
                .await
                .unwrap();
            assert_eq!(result, "ok");
            assert_eq!(primary.received_requests().await.len(), 1);
            assert_eq!(
                backup.received_requests().await.len(),
                usize::from(mode != 0)
            );
        }
    }

    #[test]
    fn routed_factory_normalizes_selection_refs_and_rejects_unknown_entries() {
        let mut config = Config::default();
        let profile = config
            .providers
            .models
            .ensure("custom", "target.v2")
            .unwrap();
        profile.uri = Some("http://127.0.0.1:1/v1".into());
        profile.models.insert(
            "fast".into(),
            ModelEntryConfig {
                id: Some("fast-id".into()),
                ..Default::default()
            },
        );
        profile.models.insert(
            "default".into(),
            ModelEntryConfig {
                id: Some("default-id".into()),
                ..Default::default()
            },
        );
        let built = build_model(&config, "worker", " custom.target.v2.fast ", None).unwrap();
        assert_eq!(built.model_name(), "fast-id");
        assert_eq!(built.identity().provider_ref(), "custom.target.v2.fast");
        assert_eq!(
            built.route_resolver().resolve("fast-id").provider_name,
            "custom.target.v2.fast"
        );
        assert!(build_model(&config, "worker", " custom.target.v2.missing ", None).is_err());
        assert!(build_model(&config, "worker", " llamacpp.typo ", Some("model-id")).is_err());
        config.model_routes = vec![ModelRouteConfig {
            hint: "fast".into(),
            model_ref: "llamacpp.typo".into(),

            ..Default::default()
        }];
        assert!(build_model(&config, "worker", "custom.target.v2.fast", None).is_err());
        config.model_routes.clear();
        let url_ref = "custom:http://127.0.0.1:1/v1";
        let built = build_model(&config, "worker", url_ref, Some("model-id")).unwrap();
        assert_eq!(built.identity().provider_ref(), url_ref);
        assert_eq!(built.model_name(), "model-id");
    }

    #[tokio::test]
    async fn routed_url_targets_override_inherited_endpoint_and_preserve_call_options() {
        use axum::{Json, Router, extract::State, routing::post};
        use zeroclaw_config::schema::AliasedAgentConfig;

        type Requests = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;
        async fn capture(
            State(requests): State<Requests>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            requests.lock().unwrap().push(body);
            Json(serde_json::json!({"choices": [{"message": {"content": "ok"}}]}))
        }

        let inherited_requests = Requests::default();
        let explicit_requests = Requests::default();
        let inherited_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let explicit_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inherited_url = format!("http://{}/v1", inherited_listener.local_addr().unwrap());
        let explicit_url = format!("http://{}/v1", explicit_listener.local_addr().unwrap());
        let inherited_app = Router::new()
            .route("/v1/chat/completions", post(capture))
            .with_state(Arc::clone(&inherited_requests));
        let explicit_app = Router::new()
            .route("/v1/chat/completions", post(capture))
            .with_state(Arc::clone(&explicit_requests));
        let inherited_server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(inherited_listener, inherited_app)
                .await
                .unwrap();
        });
        let explicit_server = ::zeroclaw_spawn::spawn!(async move {
            axum::serve(explicit_listener, explicit_app).await.unwrap();
        });

        let mut config = Config::default();
        let profile = config.providers.models.ensure("custom", "primary").unwrap();
        profile.uri = Some(inherited_url);
        profile.merge_system_into_user = true;
        profile.models.insert(
            "fast".into(),
            ModelEntryConfig {
                id: Some("inherited-id".into()),
                max_tokens: Some(123),
                ..Default::default()
            },
        );
        config.agents.insert(
            "worker".into(),
            AliasedAgentConfig {
                model_provider: "custom.primary.fast".into(),
                ..Default::default()
            },
        );

        let inherited_options = crate::provider_runtime_options_for_agent(&config, "worker");
        for reference in [
            format!("custom:{explicit_url}"),
            "custom:https://api.example.test/v1".into(),
        ] {
            let options = crate::options_for_provider_ref(&config, &reference, &inherited_options);
            assert!(options.merge_system_into_user);
            assert_eq!(options.provider_max_tokens, Some(123));
            assert_eq!(options.provider_api_url, None);
        }

        for (reference, override_model, expected_model) in [
            (
                format!("custom:{explicit_url}"),
                Some("explicit-id"),
                "explicit-id",
            ),
            ("custom".into(), None, "inherited-id"),
        ] {
            let built = build_model(&config, "worker", &reference, override_model).unwrap();
            assert_eq!(built.model_name(), expected_model);
            built
                .provider()
                .chat_with_system(Some("system fixture"), "hello", built.model_name(), None)
                .await
                .unwrap();
        }

        for (requests, expected_model) in [
            (&inherited_requests, "inherited-id"),
            (&explicit_requests, "explicit-id"),
        ] {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0]["model"], expected_model);
            assert_eq!(requests[0]["max_tokens"], 123);
            let messages = requests[0]["messages"].as_array().unwrap();
            assert!(messages.iter().all(|message| message["role"] != "system"));
            assert!(messages.iter().any(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("system fixture"))
            }));
        }
        inherited_server.abort();
        explicit_server.abort();
    }

    #[tokio::test]
    async fn nonblocking_factory_runs_outside_runtime_and_preserves_errors() {
        let outside = build_nonblocking(|| {
            // Blocking constructors may use their own runtime internally.
            let runtime = tokio::runtime::Builder::new_current_thread().build()?;
            Ok(runtime.block_on(async { 7 }))
        })
        .await
        .unwrap();
        assert_eq!(outside, 7);
        let error = build_nonblocking(|| -> anyhow::Result<()> {
            anyhow::bail!("fixture construction error")
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "fixture construction error");
    }
}
