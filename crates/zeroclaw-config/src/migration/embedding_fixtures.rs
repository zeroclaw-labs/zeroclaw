//! Historical embedding resolution retained only for migration fixtures.
use crate::providers::ModelProviders;
use crate::schema::{EmbeddingRouteConfig, MemoryConfig};

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedEmbeddingConfig {
    pub model_provider: String,
    pub model: String,
    pub dimensions: usize,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ResolvedEmbeddingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedEmbeddingConfig")
            .field("model_provider", &self.model_provider)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

pub fn resolve_embedding_config(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    api_key: Option<&str>,
    providers: Option<&ModelProviders>,
) -> ResolvedEmbeddingConfig {
    let inherited_api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let configured_api_key = config
        .embedding_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let fallback = || {
        resolve_provider_ref(
            config.embedding_provider.trim().to_string(),
            config.embedding_model.trim().to_string(),
            config.embedding_dimensions,
            configured_api_key.clone(),
            inherited_api_key.clone(),
            providers,
        )
    };

    let Some(hint) = config
        .embedding_model
        .strip_prefix("hint:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return fallback();
    };

    let Some(route) = embedding_routes
        .iter()
        .find(|route| route.hint.trim() == hint)
    else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"hint": hint})),
            "Unknown embedding route hint; falling back to [memory] embedding settings"
        );
        return fallback();
    };

    let model_provider = route.model_provider.trim();
    let model = route.model.trim();
    let dimensions = route.dimensions.unwrap_or(config.embedding_dimensions);
    if model_provider.is_empty() || model.is_empty() || dimensions == 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"hint": hint})),
            "Invalid embedding route configuration; falling back to [memory] embedding settings"
        );
        return fallback();
    }

    let routed_api_key = route
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value: &&str| !value.is_empty())
        .map(|value| value.to_string());

    resolve_provider_ref(
        model_provider.to_string(),
        model.to_string(),
        dimensions,
        routed_api_key.or(configured_api_key),
        inherited_api_key,
        providers,
    )
}

fn resolve_provider_ref(
    model_provider: String,
    model: String,
    dimensions: usize,
    explicit_api_key: Option<String>,
    inherited_api_key: Option<String>,
    providers: Option<&ModelProviders>,
) -> ResolvedEmbeddingConfig {
    let trimmed = model_provider.trim();
    let is_dotted_ref =
        !trimmed.is_empty() && !trimmed.starts_with("custom:") && trimmed.contains('.');
    if !is_dotted_ref {
        return ResolvedEmbeddingConfig {
            model_provider,
            model,
            dimensions,
            api_key: explicit_api_key.or(inherited_api_key),
        };
    }

    let reference = trimmed.to_string();
    let Some((kind, _alias, provider_cfg)) =
        providers.and_then(|catalog| catalog.find_by_name(&reference))
    else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "memory.embedding_route_unresolved",
                    "provider_ref": reference,
                })),
            "Embedding provider reference did not resolve against providers.models; \
             embeddings disabled (keyword-only) for this profile"
        );
        return ResolvedEmbeddingConfig {
            model_provider,
            model,
            dimensions,
            api_key: explicit_api_key.or(inherited_api_key),
        };
    };

    let provider_key = provider_cfg
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let concrete_provider = match provider_cfg
        .uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(uri) => Some(format!("custom:{uri}")),
        None if matches!(kind, "openai" | "openrouter") => Some(kind.to_string()),
        None => None,
    };
    let Some(concrete_provider) = concrete_provider else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "memory.embedding_route_no_endpoint",
                    "provider_ref": reference,
                    "provider_kind": kind,
                })),
            "Embedding provider reference resolved but has no usable embeddings \
             endpoint (set its `uri`, or point the route at an openai/openrouter \
             compatible profile); embeddings disabled (keyword-only) for this profile"
        );
        return ResolvedEmbeddingConfig {
            model_provider,
            model,
            dimensions,
            api_key: explicit_api_key.or(inherited_api_key),
        };
    };

    ResolvedEmbeddingConfig {
        model_provider: concrete_provider,
        model,
        dimensions,
        api_key: explicit_api_key.or(provider_key).or(inherited_api_key),
    }
}
