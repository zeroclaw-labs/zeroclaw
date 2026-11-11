//! Compatibility decoding for historical embedding bindings. Only config
//! owns these old shapes; runtime consumers resolve capability models.
use super::{ModelRef, ProviderId, ProviderRegistry};
use crate::providers::ModelProviders;
use crate::schema::{Config, EmbeddingRouteConfig, MemoryConfig};
use anyhow::Result;
use serde_json::json;
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

impl Config {
    /// Diagnostics for historical embedding route fields. Consumers render
    /// these messages without interpreting provider strings or old hint syntax.
    pub fn embedding_input_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let registry = self.provider_registry().ok();
        for route in &self.embedding_routes {
            if route.hint.trim().is_empty() {
                warnings.push("embedding route with empty hint".into());
            }
            if registry.as_ref().is_some_and(|registry| {
                registry
                    .resolve_embedding(&route.model_provider.trim().into())
                    .is_ok()
            }) {
                continue;
            }
            if let Some(reason) = embedding_provider_validation_error(&route.model_provider) {
                warnings.push(format!(
                    "embedding route \"{}\" uses invalid model_provider \"{}\": {}",
                    route.hint, route.model_provider, reason
                ));
            }
            if route.model.trim().is_empty() {
                warnings.push(format!(
                    "embedding route \"{}\" has empty model",
                    route.hint
                ));
            }
            if route.dimensions.is_some_and(|value| value == 0) {
                warnings.push(format!(
                    "embedding route \"{}\" has invalid dimensions=0",
                    route.hint
                ));
            }
        }
        if let Some(hint) = self
            .memory
            .embedding_model
            .strip_prefix("hint:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && !self
                .embedding_routes
                .iter()
                .any(|route| route.hint.trim() == hint)
        {
            warnings.push(format!("memory.embedding_model uses hint \"{hint}\" but no matching [[embedding_routes]] entry exists"));
        }
        warnings
    }

    /// Credential precedence for historical embedding input. The connection
    /// key supplied here belongs to the already resolved exact model.
    pub fn embedding_api_key_for_input(
        &self,
        connection_key: Option<&str>,
        inherited_key: Option<&str>,
    ) -> Option<String> {
        let routed = self
            .memory
            .embedding_model_ref
            .as_deref()
            .is_none_or(|reference| reference.trim().is_empty())
            .then(|| self.memory.embedding_model.strip_prefix("hint:"))
            .flatten()
            .and_then(|hint| {
                self.embedding_routes
                    .iter()
                    .find(|route| route.hint.trim() == hint.trim())
            })
            .and_then(|route| route.api_key.as_deref())
            .filter(|key| !key.trim().is_empty());
        routed
            .or(self
                .memory
                .embedding_api_key
                .as_deref()
                .filter(|key| !key.trim().is_empty()))
            .or(connection_key)
            .or(inherited_key)
            .map(str::to_owned)
    }

    /// Materialize a complete embedding selection from canonical or historical
    /// input. Disabled embeddings produce no binding, not a fabricated model.
    pub fn embedding_registry_for_input(
        &self,
        api_key: Option<&str>,
    ) -> Result<(ProviderRegistry, Option<ModelRef>)> {
        let mut registry = self.provider_registry()?;
        if let Some(reference) = self
            .memory
            .embedding_model_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let key = ModelRef::from(reference);
            registry.resolve_embedding(&key)?;
            return Ok((registry, Some(key)));
        }
        if let Some(hint) = self.memory.embedding_model.strip_prefix("hint:")
            && let Some(route) = self
                .embedding_routes
                .iter()
                .find(|route| route.hint.trim() == hint.trim())
        {
            let key = ModelRef::from(route.model_provider.trim());
            if registry.models.contains_key(&key) {
                registry.resolve_embedding(&key)?;
                return Ok((registry, Some(key)));
            }
        }
        let selected = resolve_embedding_config(
            &self.memory,
            &self.embedding_routes,
            api_key,
            Some(&self.providers.models),
        );
        let provider = selected.model_provider.trim();
        if !matches!(provider, "openai" | "openrouter") && !provider.starts_with("custom:") {
            return Ok((registry, None));
        }
        let mut connection = if let Some(uri) = provider.strip_prefix("custom:") {
            json!({"kind":"custom", "uri":uri})
        } else {
            json!({"kind":provider})
        };
        connection["auth"] = json!({"mode":"api_key", "api_key":selected.api_key});
        let mut key = ModelRef::from("temporary.embedding.memory");
        while registry.models.contains_key(&key)
            || registry
                .connections
                .contains_key(&ProviderId::from(key.as_str()))
        {
            key = ModelRef::from(format!("temporary.{}", key.as_str()));
        }
        let provider = ProviderId::from(key.as_str());
        registry
            .connections
            .insert(provider.clone(), serde_json::from_value(connection)?);
        registry.models.insert(key.clone(), serde_json::from_value(json!({"capability":"embedding", "provider":provider.as_str(), "model":selected.model, "metadata":{"dimensions":selected.dimensions}}))?);
        registry.validate()?;
        registry.resolve_embedding(&key)?;
        Ok((registry, Some(key)))
    }
}

fn embedding_provider_validation_error(name: &str) -> Option<String> {
    let normalized = name.trim();
    if normalized.eq_ignore_ascii_case("none") || normalized.eq_ignore_ascii_case("openai") {
        return None;
    }

    let Some(url) = normalized.strip_prefix("custom:") else {
        return Some("supported values: none, openai, custom:<url>".into());
    };

    let url = url.trim();
    if url.is_empty() {
        return Some("custom model_provider requires a non-empty URL after 'custom:'".into());
    }

    match reqwest::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => None,
        Ok(parsed) => Some(format!(
            "custom model_provider URL must use http/https, got '{}'",
            parsed.scheme()
        )),
        Err(err) => Some(format!("invalid custom model_provider URL: {err}")),
    }
}
