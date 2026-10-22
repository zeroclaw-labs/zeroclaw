//! Provider/model construction from the canonical configuration selection.
use crate::ModelProvider;
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

/// Resolve materialized fallback dependencies on demand, without retaining a
/// parallel graph. Call against both generations when an edit removes a ref.
pub fn selection_depends_on_profile(
    config: &Config,
    selection_ref: &str,
    changed_profile_ref: &str,
) -> bool {
    fn visit(
        config: &Config,
        reference: &str,
        changed: &str,
        visited: &mut Vec<String>,
        depth: usize,
    ) -> bool {
        if depth > zeroclaw_config::providers::MAX_FALLBACK_DEPTH {
            return false;
        }
        let reference = reference.trim();
        let resolved = if depth == 0 {
            config
                .providers
                .models
                .profile_ref(reference)
                .and_then(|(family, alias)| {
                    config
                        .providers
                        .models
                        .find(family, alias)
                        .map(|entry| (family, alias.to_string(), entry))
                })
        } else {
            // Fallback materialization also accepts an unambiguous bare alias.
            config.providers.models.find_by_name(reference)
        };
        let Some((family, alias, entry)) = resolved else {
            return false;
        };
        if crate::ensure_known_model_entry_ref(config, reference).is_err() {
            return false;
        }
        let profile = format!("{family}.{alias}");
        if profile == changed {
            return true;
        }
        if visited.contains(&profile) {
            return false;
        }
        visited.push(profile);
        let depends = entry
            .fallback
            .iter()
            .any(|fallback| visit(config, fallback, changed, visited, depth + 1));
        visited.pop();
        depends
    }

    let changed = config
        .providers
        .models
        .profile_ref(changed_profile_ref)
        .map(|(family, alias)| format!("{family}.{alias}"))
        .unwrap_or_else(|| changed_profile_ref.to_string());
    visit(config, selection_ref, &changed, &mut Vec::new(), 0)
}

/// Dependencies of the routed graph built by [`build_model`], including each
/// configured hint target's independent reliability domain and the agent
/// selection inherited by a bare family or URL reference.
pub fn routed_selection_depends_on_profile(
    config: &Config,
    agent_alias: &str,
    selection_ref: &str,
    changed_profile_ref: &str,
) -> bool {
    let inherited_profile = if !inherits_agent_selection(config, selection_ref) {
        false
    } else {
        let changed_profile = config
            .providers
            .models
            .profile_ref(changed_profile_ref)
            .map(|(family, alias)| format!("{family}.{alias}"))
            .unwrap_or_else(|| changed_profile_ref.trim().to_string());
        configured_selection_for_construction(config, agent_alias, selection_ref).is_some_and(
            |selection| format!("{}.{}", selection.family, selection.alias) == changed_profile,
        )
    };
    inherited_profile
        || selection_depends_on_profile(config, selection_ref, changed_profile_ref)
        || config.model_routes.iter().any(|route| {
            !route.hint.trim().is_empty()
                && !route.effective_model(config).trim().is_empty()
                && selection_depends_on_profile(config, &route.model_provider, changed_profile_ref)
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

    // Named refs select their own profile; a bare family inherits the
    // configured agent selection, including default/sole nested entries.
    let selection = configured_selection_for_construction(config, agent_alias, model_provider_ref);
    let entry = selection.as_ref().map(|selection| selection.entry);
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
        // Bare family: inherit the agent's own profile options with the
        // provider-specific bits cleared — the legacy `--provider <family>`
        // semantics preserved by `options_for_provider_ref`'s bare arm.
        let agent_options = crate::provider_runtime_options_for_agent(config, agent_alias);
        crate::options_for_provider_ref(config, model_provider_ref, &agent_options)
    } else {
        crate::provider_runtime_options_for_alias(
            config,
            &model_provider_name,
            &model_provider_alias,
        )
    };
    crate::apply_model_entry_options(&mut runtime_options, model_entry);

    let (provider, model_route_resolver) =
        crate::create_routed_model_provider_with_options_and_resolver(
            config,
            // The raw ref, deliberately: see the function-level comment.
            model_provider_ref,
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
        primary.fallback = vec!["custom.backup".into()];
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
            assert!(routed_selection_depends_on_profile(
                &config,
                "worker",
                reference,
                "custom.primary.fast"
            ));
            assert!(!routed_selection_depends_on_profile(
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
        assert!(routed_selection_depends_on_profile(
            &config,
            "worker",
            "custom.primary.fast",
            "custom.backup"
        ));
        assert!(!inherits_agent_selection(&config, "custom.missing"));
        assert!(resolve_model_identity(&config, "worker", "custom.missing", None).is_none());
    }

    #[test]
    fn dependency_resolution_walks_nested_fallbacks_and_route_domains() {
        let mut config = Config::default();
        for alias in ["primary", "backup.v2", "last", "routed", "unrelated"] {
            let profile = config.providers.models.ensure("custom", alias).unwrap();
            profile.model = Some("model-id".into());
        }
        config
            .providers
            .models
            .ensure("custom", "primary")
            .unwrap()
            .fallback = vec!["custom.backup.v2.fast".into()];
        let backup = config
            .providers
            .models
            .ensure("custom", "backup.v2")
            .unwrap();
        backup
            .models
            .insert("fast".into(), ModelEntryConfig::default());
        backup.fallback = vec!["custom.last".into()];
        config
            .providers
            .models
            .ensure("custom", "last")
            .unwrap()
            .fallback = vec!["custom.primary".into()];
        config.model_routes = vec![ModelRouteConfig {
            hint: "fast".into(),
            model_provider: "custom.routed".into(),
            ..Default::default()
        }];
        for changed in [
            "custom.primary",
            "custom.backup.v2",
            "custom.backup.v2.fast",
            "custom.last",
        ] {
            assert!(
                selection_depends_on_profile(&config, "custom.primary", changed),
                "{changed}"
            );
        }
        assert!(!selection_depends_on_profile(
            &config,
            "custom.primary",
            "custom.routed"
        ));
        assert!(routed_selection_depends_on_profile(
            &config,
            "worker",
            "custom.primary",
            "custom.routed"
        ));
        assert!(!routed_selection_depends_on_profile(
            &config,
            "worker",
            "custom.primary",
            "custom.unrelated"
        ));

        let old = config.clone();
        config
            .providers
            .models
            .ensure("custom", "primary")
            .unwrap()
            .fallback
            .clear();
        assert!(selection_depends_on_profile(
            &old,
            "custom.primary",
            "custom.backup.v2"
        ));
        assert!(!selection_depends_on_profile(
            &config,
            "custom.primary",
            "custom.backup.v2"
        ));
        config
            .providers
            .models
            .ensure("custom", "primary")
            .unwrap()
            .fallback = vec!["custom.backup.v2.missing".into()];
        for pruned in ["custom.backup.v2", "custom.last"] {
            assert!(!selection_depends_on_profile(
                &config,
                "custom.primary",
                pruned
            ));
        }

        config
            .providers
            .models
            .ensure("custom", "primary")
            .unwrap()
            .fallback = vec![" last ".into()];
        assert!(selection_depends_on_profile(
            &config,
            "custom.primary",
            "custom.last"
        ));
        assert!(!selection_depends_on_profile(
            &config,
            "last",
            "custom.last"
        ));
        config.providers.models.ensure("openai", "last").unwrap();
        assert!(!selection_depends_on_profile(
            &config,
            "custom.primary",
            "custom.last"
        ));
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
            model_provider: "llamacpp.typo".into(),
            model: "route-id".into(),
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
