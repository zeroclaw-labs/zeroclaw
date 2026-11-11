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
        self.identity.provider_ref()
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
    let registry = config.chat_registry_for_input(None, selection_ref)?;
    let selection = registry.resolve_chat(&selection_ref.trim().into())?;
    Ok(crate::factory::canonical_auth_ready(
        selection.selection.connection,
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
    !config.is_configured_model_ref(selection_ref)
        && config.chat_registry_for_input(None, selection_ref).is_ok()
}

/// Resolve options for the same selection used by model construction, without
/// constructing a transport or borrowing tuning from another model handle.
pub fn runtime_options(
    config: &Config,
    agent_alias: &str,
    selection_ref: &str,
) -> anyhow::Result<crate::ModelProviderRuntimeOptions> {
    let registry = config.chat_registry_for_input(Some(agent_alias), selection_ref)?;
    let selected = registry.resolve_chat(&selection_ref.trim().into())?;
    Ok(crate::factory::runtime_options_for_chat(
        &selected,
        &crate::provider_runtime_options_for_bare_family(config),
    ))
}

/// Resolve the exact identity without constructing a transport.
pub fn resolve_model_identity(
    config: &Config,
    agent_alias: &str,
    selection_ref: &str,
    model_override: Option<&str>,
) -> Option<ModelIdentity> {
    let registry = config
        .chat_registry_for_input(Some(agent_alias), selection_ref)
        .ok()?;
    let selected = registry.resolve_chat(&selection_ref.trim().into()).ok()?;
    let model_id = model_override
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| selected.upstream_model())?
        .to_owned();
    Some(ModelIdentity::new(selection_ref.trim(), model_id))
}

fn build_provider(
    config: &Config,
    selection_ref: &str,
    overrides: ProviderOverrides<'_>,
    reliable: bool,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    let registry = config.chat_registry_for_input(None, selection_ref)?;
    let reference = selection_ref.trim().into();
    let options = crate::provider_runtime_options_for_bare_family(config);
    if reliable {
        build_provider_in_registry(config, &registry, &reference, overrides, &options, true)
    } else {
        let provider = crate::factory::dispatch_model_ref_with_registry(
            &registry,
            &reference,
            &options,
            overrides.api_key,
        )?;
        let model = overrides.model.map(str::to_owned).or_else(|| {
            registry
                .resolve_chat(&reference)
                .ok()?
                .upstream_model()
                .map(str::to_owned)
        });
        Ok(crate::ResolvedModelProviderRef { provider, model })
    }
}

/// Construct a canonical model with the caller's admitted runtime options.
/// This is kept separate from the legacy family parser so the exact ModelRef
/// remains intact through provider construction and fallback pinning.
#[cfg(test)]
pub(crate) fn build_canonical_provider_with_options(
    config: &Config,
    model_ref: &zeroclaw_config::provider_registry::ModelRef,
    overrides: ProviderOverrides<'_>,
    options: &crate::ModelProviderRuntimeOptions,
    pin_primary_model: bool,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    let registry = config.chat_registry_for_input(None, model_ref.as_str())?;
    build_provider_in_registry(
        config,
        &registry,
        model_ref,
        overrides,
        options,
        pin_primary_model,
    )
}

fn build_provider_in_registry(
    config: &Config,
    registry: &zeroclaw_config::provider_registry::ProviderRegistry,
    model_ref: &zeroclaw_config::provider_registry::ModelRef,
    overrides: ProviderOverrides<'_>,
    options: &crate::ModelProviderRuntimeOptions,
    pin_primary_model: bool,
) -> anyhow::Result<crate::ResolvedModelProviderRef> {
    let mut candidates = Vec::new();
    let mut refs = vec![model_ref.clone()];
    refs.extend(registry.fallback_chain(model_ref)?);
    for candidate_ref in refs {
        let provider = crate::factory::dispatch_model_ref_with_registry(
            registry,
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
    ) -> bool {
        if current == changed {
            return true;
        }
        if !visited.insert(current.clone()) {
            return false;
        }
        registry.fallback.get(current).is_some_and(|targets| {
            targets
                .iter()
                .any(|target| visit(registry, target, changed, visited))
        })
    }
    visit(&registry, &root, &changed, &mut HashSet::new())
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
    let inherited_selection = inherits_agent_selection(config, selection_ref)
        && config.agent(agent_alias).is_some_and(|agent| {
            selection_depends_on_model_ref(config, &agent.model_provider, changed_model_ref)
        });
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
/// Callers supply an exact model key and optional invocation override. Config
/// normalizes historical CLI input before construction. Primary, hint and
/// fallback targets each resolve their complete policy from that registry.
pub fn build_model(
    config: &Config,
    agent_alias: &str,
    model_provider_ref: &str,
    model_override: Option<&str>,
) -> anyhow::Result<BuiltModel> {
    let model_provider_ref = model_provider_ref.trim();
    let registry = config.chat_registry_for_input(Some(agent_alias), model_provider_ref)?;
    let reference = zeroclaw_config::provider_registry::ModelRef::from(model_provider_ref);
    let selected = registry.resolve_chat(&reference)?;
    let model_id = model_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| selected.upstream_model())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model `{model_provider_ref}` has no upstream model"
            ))
        })?;
    let options = crate::provider_runtime_options_for_bare_family(config);
    let primary = build_provider_in_registry(
        config,
        &registry,
        &reference,
        ProviderOverrides {
            model: model_override,
            api_key: None,
        },
        &options,
        true,
    )?
    .provider;
    let mut routes = Vec::new();
    let mut providers = vec![(model_provider_ref.to_owned(), primary)];
    // Hint targets share only the immutable registry. Every target is built
    // again from its complete model definition, even when it equals primary.
    for route in config
        .model_routes
        .iter()
        .filter(|route| !route.hint.trim().is_empty() && !route.model_ref.trim().is_empty())
    {
        let target = zeroclaw_config::provider_registry::ModelRef::from(route.model_ref.trim());
        let upstream = registry
            .resolve_chat(&target)?
            .upstream_model()
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| {
                anyhow::Error::msg(format!("route `{}` has no upstream model", route.hint))
            })?
            .to_owned();
        if !providers
            .iter()
            .skip(1)
            .any(|(key, _)| key == target.as_str())
        {
            let provider = build_provider_in_registry(
                config,
                &registry,
                &target,
                ProviderOverrides::default(),
                &options,
                true,
            )?
            .provider;
            providers.push((target.to_string(), provider));
        }
        routes.push((
            route.hint.clone(),
            crate::router::Route {
                provider_name: target.to_string(),
                model: upstream,
            },
        ));
    }
    let identity = ModelIdentity::new(model_provider_ref, model_id.clone());
    if routes.is_empty() {
        let (_, provider) = providers.remove(0);
        let resolver = Arc::new(crate::router::ModelRouteResolver::new(
            Vec::new(),
            model_provider_ref.to_owned(),
            model_id,
        ));
        return Ok(BuiltModel::from_routed_provider(
            provider, identity, resolver,
        ));
    }
    let router =
        crate::router::RouterModelProvider::new(model_provider_ref, providers, routes, model_id);
    let resolver = router.route_resolver();
    Ok(BuiltModel::from_routed_provider(
        Box::new(router),
        identity,
        resolver,
    ))
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
        let built = build_model(
            &zeroclaw_config::migration::admit_test_input(&config),
            "worker",
            "openai",
            Some("gpt-test"),
        )
        .unwrap();
        assert_eq!(
            built.provider().role(),
            Role::Provider(ProviderKind::Model(ModelProviderKind::OpenAi))
        );
        assert_eq!(built.identity().provider_ref(), "openai");
        let built = build_model(
            &zeroclaw_config::migration::admit_test_input(&config),
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
            crate::provider_runtime_options_for_alias(
                &zeroclaw_config::migration::admit_test_input(&config),
                "openai",
                "primary"
            )
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
            crate::provider_runtime_options_for_alias(
                &zeroclaw_config::migration::admit_test_input(&config),
                "openai",
                "primary"
            )
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
            assert!(inherits_agent_selection(
                &zeroclaw_config::migration::admit_test_input(&config),
                reference
            ));
            let identity = resolve_model_identity(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                reference,
                None,
            )
            .unwrap();
            assert_eq!(identity.provider_ref(), reference);
            assert_eq!(identity.model_id, "inherited-model");
            assert_eq!(
                build_model(
                    &zeroclaw_config::migration::admit_test_input(&config),
                    "worker",
                    reference,
                    None
                )
                .unwrap()
                .identity(),
                &identity
            );
            assert!(routed_selection_depends_on_model_ref(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                reference,
                "custom.primary.fast"
            ));
            assert!(!routed_selection_depends_on_model_ref(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                reference,
                "custom.backup"
            ));
            assert_eq!(
                resolve_model_identity(
                    &zeroclaw_config::migration::admit_test_input(&config),
                    "worker",
                    reference,
                    Some("explicit-model")
                )
                .unwrap()
                .model_id,
                "explicit-model"
            );
        }
        assert!(!routed_selection_depends_on_model_ref(
            &zeroclaw_config::migration::admit_test_input(&config),
            "worker",
            "custom.primary.fast",
            "custom.backup"
        ));
        assert!(!inherits_agent_selection(
            &zeroclaw_config::migration::admit_test_input(&config),
            "custom.missing"
        ));
        assert!(
            resolve_model_identity(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                "custom.missing",
                None
            )
            .is_none()
        );
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

    #[test]
    fn normalized_model_keys_have_no_implicit_prefix_dependencies() {
        let mut config = Config::default();
        let profile = config
            .providers
            .models
            .ensure("custom", "gateway.prod")
            .unwrap();
        profile.uri = Some("https://example.invalid/v1".into());
        profile.model = Some("primary".into());
        for name in ["fast", "slow"] {
            profile.models.insert(
                name.into(),
                ModelEntryConfig {
                    id: Some(name.into()),
                    ..Default::default()
                },
            );
        }
        assert!(!selection_depends_on_model_ref(
            &zeroclaw_config::migration::admit_test_input(&config),
            "custom.gateway.prod.fast",
            "custom.gateway.prod"
        ));
        assert!(!selection_depends_on_model_ref(
            &zeroclaw_config::migration::admit_test_input(&config),
            "custom.gateway.prod.slow",
            "custom.gateway.prod"
        ));
        assert!(!selection_depends_on_model_ref(
            &zeroclaw_config::migration::admit_test_input(&config),
            "custom.gateway.prod.fast",
            "custom.gateway.prod.slow"
        ));
        assert!(!selection_depends_on_model_ref(
            &zeroclaw_config::migration::admit_test_input(&config),
            "custom.gateway.prod",
            "custom.gateway.prod.fast"
        ));

        // An explicit exact-key model shadows the profile's projected key.
        let registry = zeroclaw_config::migration::admit_test_input(&config)
            .provider_registry()
            .unwrap();
        let model = registry.models["custom.gateway.prod"].clone();
        config.providers.models.insert_canonical_model(
            "custom.gateway.prod".into(),
            serde_json::from_value(serde_json::to_value(model).unwrap()).unwrap(),
        );
        assert!(!selection_depends_on_model_ref(
            &zeroclaw_config::migration::admit_test_input(&config),
            "custom.gateway.prod.fast",
            "custom.gateway.prod"
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
        let built = build_model(
            &zeroclaw_config::migration::admit_test_input(&config),
            "worker",
            " custom.target.v2.fast ",
            None,
        )
        .unwrap();
        assert_eq!(built.model_name(), "fast-id");
        assert_eq!(built.identity().provider_ref(), "custom.target.v2.fast");
        assert_eq!(
            built.route_resolver().resolve("fast-id").provider_name,
            "custom.target.v2.fast"
        );
        assert!(
            build_model(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                " custom.target.v2.missing ",
                None
            )
            .is_err()
        );
        assert!(
            build_model(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                " llamacpp.typo ",
                Some("model-id")
            )
            .is_err()
        );
        config.model_routes = vec![ModelRouteConfig {
            hint: "fast".into(),
            model_ref: "llamacpp.typo".into(),
        }];
        assert!(
            build_model(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                "custom.target.v2.fast",
                None
            )
            .is_err()
        );
        config
            .providers
            .models
            .ensure("custom", "unfinished")
            .unwrap()
            .uri = Some("http://127.0.0.1:2/v1".into());
        config.model_routes[0].model_ref = "custom.unfinished".into();
        assert!(
            build_model(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                "custom.target.v2.fast",
                None
            )
            .is_err()
        );
        config.model_routes.clear();
        let url_ref = "custom:http://127.0.0.1:1/v1";
        let built = build_model(
            &zeroclaw_config::migration::admit_test_input(&config),
            "worker",
            url_ref,
            Some("model-id"),
        )
        .unwrap();
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

        let inherited_options = crate::provider_runtime_options_for_agent(
            &zeroclaw_config::migration::admit_test_input(&config),
            "worker",
        );
        for reference in [
            format!("custom:{explicit_url}"),
            "custom:https://api.example.test/v1".into(),
        ] {
            let options = crate::options_for_provider_ref(
                &zeroclaw_config::migration::admit_test_input(&config),
                &reference,
                &inherited_options,
            );
            assert!(!options.merge_system_into_user);
            assert_eq!(options.provider_max_tokens, None);
            assert_eq!(
                options.provider_api_url.as_deref(),
                reference.strip_prefix("custom:")
            );
        }

        for (reference, override_model, expected_model) in [
            (
                format!("custom:{explicit_url}"),
                Some("explicit-id"),
                "explicit-id",
            ),
            ("custom".into(), None, "inherited-id"),
        ] {
            let built = build_model(
                &zeroclaw_config::migration::admit_test_input(&config),
                "worker",
                &reference,
                override_model,
            )
            .unwrap();
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
