//! Historical provider input decoding. This module runs before configuration
//! admission; registry consumers never inspect retired provider tables.
use crate::provider_registry::{
    self as registry, ModelDefinition, ModelRef, ProviderId, ProviderRegistry,
};
use crate::providers::{Providers, TranscriptionProviderEntry};
use crate::schema::{Config, ModelEntryConfig, ModelSelection};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use zeroclaw_api::capability::Capability;

fn project_provider_tables(
    providers: &Providers,
) -> Result<ProviderRegistry, registry::RegistryError> {
    // This decoder runs only at the configuration input boundary.
    let mut connections = providers.connections.clone();
    let mut models = providers.models.canonical_models.clone();
    let legacy = serde_json::to_value(&providers.models).map_err(|error| {
        registry::RegistryError::new(["providers", "models"], error.to_string())
    })?;
    for (family, alias, profile) in providers.models.iter_entries() {
        let profile_ref = format!("{family}.{alias}");
        // JSON cannot represent non-finite floats. Validate the typed input
        // before normalization can turn a legacy NaN into an absent value.
        for temperature in std::iter::once(profile.temperature)
            .chain(profile.models.values().map(|entry| entry.temperature))
            .flatten()
        {
            crate::schema::validate_temperature(temperature).map_err(|error| {
                registry::RegistryError::new(
                    ["providers", "models", profile_ref.as_str(), "temperature"],
                    error,
                )
            })?;
        }
        if profile
            .pricing
            .values()
            .any(|value| value.is_nan() || *value < 0.0)
        {
            return Err(registry::RegistryError::new(
                ["providers", "models", profile_ref.as_str(), "pricing"],
                "price must be non-negative and not NaN",
            ));
        }
        let mut raw = legacy[family][alias].clone();
        if profile.uri.is_none()
            && family != "qwen"
            && let Some(uri) = providers.models.resolved_endpoint_uri(family, alias)
        {
            raw["uri"] = serde_json::Value::String(uri.to_string());
        }
        let selected = profile.models.get("default").or_else(|| {
            (profile.models.len() == 1)
                .then(|| profile.models.values().next())
                .flatten()
        });
        let entries = std::iter::once((profile_ref.clone(), selected)).chain(
            profile
                .models
                .iter()
                .map(|(name, entry)| (format!("{profile_ref}.{name}"), Some(entry))),
        );
        for (reference, entry) in entries {
            let model_ref = ModelRef::from(reference.as_str());
            if models.contains_key(&model_ref) {
                continue;
            }
            let (connection, mut model) = crate::migration::legacy_chat_definition(
                &raw, family, entry, None,
            )
            .map_err(|error| {
                registry::RegistryError::new(
                    ["providers", "models", reference.as_str()],
                    error.to_string(),
                )
            })?;
            let provider_id = ProviderId::from(format!("legacy.{profile_ref}"));
            if providers.connections.contains_key(&provider_id) {
                return Err(registry::RegistryError::new(
                    ["providers", "connections", provider_id.as_str()],
                    "legacy connection name conflicts with an explicit connection",
                ));
            }
            let connection = serde_json::from_value(connection).map_err(|error| {
                registry::RegistryError::new(
                    ["providers", "models", reference.as_str()],
                    error.to_string(),
                )
            })?;
            connections.entry(provider_id.clone()).or_insert(connection);
            model["provider"] = serde_json::Value::String(provider_id.to_string());
            models.insert(
                model_ref,
                serde_json::from_value(model).map_err(|error| {
                    registry::RegistryError::new(
                        ["providers", "models", reference.as_str()],
                        error.to_string(),
                    )
                })?,
            );
        }
    }

    // Project the remaining typed compatibility tables into the same
    // exact-key registry. This keeps TTS/ASR consumers independent from
    // their retired family maps while preserving their legacy defaults.
    for (family, alias, base) in providers.tts.iter_entries() {
        let legacy_ref = ModelRef::from(format!("{family}.{alias}"));
        let reference = if models.contains_key(&legacy_ref) {
            ModelRef::from(format!("tts.{family}.{alias}"))
        } else {
            legacy_ref
        };
        if models.contains_key(&reference) {
            continue;
        }
        let provider_id = ProviderId::from(format!("legacy.tts.{family}.{alias}"));
        let mut connection = serde_json::json!({"kind": family});
        if let Some(uri) = base.uri.as_deref() {
            connection["uri"] = serde_json::Value::String(uri.to_owned());
        }
        if let Some(api_key) = base.api_key.as_deref() {
            connection["auth"] = serde_json::json!({"mode":"api_key","api_key":api_key});
        }
        if let Some(command) = base.binary_path.as_deref()
            && matches!(family, "edge" | "piper")
        {
            connection["api"] = serde_json::json!({"tts":{"command":command}});
        }
        let mut options = serde_json::Map::new();
        for (field, value) in [
            ("voice", base.voice.clone().map(serde_json::Value::String)),
            ("speed", base.speed.map(serde_json::Value::from)),
            ("stability", base.stability.map(serde_json::Value::from)),
            (
                "similarity_boost",
                base.similarity_boost.map(serde_json::Value::from),
            ),
            (
                "language",
                base.language_code.clone().map(serde_json::Value::String),
            ),
            (
                "format",
                base.response_format.clone().map(serde_json::Value::String),
            ),
        ] {
            if let Some(value) = value {
                options.insert(field.to_owned(), value);
            }
        }
        let mut model = serde_json::json!({
            "capability": "tts",
            "provider": provider_id.as_str(),
            "options": options,
        });
        if let Some(model_id) = base.model.as_deref() {
            model["model"] = serde_json::Value::String(model_id.to_owned());
        }
        connections.insert(
            provider_id.clone(),
            serde_json::from_value(connection).map_err(|error| {
                registry::RegistryError::new(["providers", "tts", family, alias], error.to_string())
            })?,
        );
        models.insert(
            reference,
            serde_json::from_value(model).map_err(|error| {
                registry::RegistryError::new(["providers", "tts", family, alias], error.to_string())
            })?,
        );
    }
    for (family, alias, entry) in providers.transcription.iter_entries() {
        let legacy_ref = ModelRef::from(format!("{family}.{alias}"));
        let reference = if models.contains_key(&legacy_ref) {
            ModelRef::from(format!("asr.{family}.{alias}"))
        } else {
            legacy_ref
        };
        if models.contains_key(&reference) {
            continue;
        }
        let raw = match entry {
            TranscriptionProviderEntry::Groq(value) => serde_json::to_value(value),
            TranscriptionProviderEntry::OpenAi(value) => serde_json::to_value(value),
            TranscriptionProviderEntry::Deepgram(value) => serde_json::to_value(value),
            TranscriptionProviderEntry::AssemblyAi(value) => serde_json::to_value(value),
            TranscriptionProviderEntry::Google(value) => serde_json::to_value(value),
            TranscriptionProviderEntry::LocalWhisper(value) => serde_json::to_value(value),
        }
        .map_err(|error| {
            registry::RegistryError::new(
                ["providers", "transcription", family, alias],
                error.to_string(),
            )
        })?;
        let provider_id = ProviderId::from(format!("legacy.asr.{family}.{alias}"));
        let (connection, model) = asr_definition(&raw, family, &provider_id).map_err(|error| {
            registry::RegistryError::new(
                ["providers", "transcription", family, alias],
                error.to_string(),
            )
        })?;
        connections.entry(provider_id).or_insert(connection);
        models.insert(
            reference,
            serde_json::to_value(model)
                .and_then(serde_json::from_value)
                .map_err(|error| {
                    registry::RegistryError::new(
                        ["providers", "transcription", family, alias],
                        error.to_string(),
                    )
                })?,
        );
    }
    ProviderRegistry::from_raw_parts(
        connections,
        models,
        providers.model_presets.clone(),
        providers.fallback.clone(),
    )
}

/// Fold section-only ASR input into the same typed migration adapter. Existing
/// explicit connections/models remain authoritative over these historical rows.
fn providers_for_input(config: &Config) -> Result<crate::providers::Providers> {
    let mut providers = config.providers.clone();
    for (alias, legacy) in &config.decision_models {
        let key = ModelRef::from(alias.as_str());
        // An exact canonical selection is authoritative, including its
        // capability. Never reinterpret a colliding chat model as decision.
        if providers.models.canonical_model(&key).is_some() {
            continue;
        }
        let Some((uri, model)) = legacy.endpoint() else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(json!({"alias": alias})),
                "legacy decision model skipped: custom provider has no base URL"
            );
            continue;
        };
        let mut provider = ProviderId::from(format!("legacy.decision.{alias}"));
        while providers.connections.contains_key(&provider) {
            provider = ProviderId::from(format!("legacy.{}", provider.as_str()));
        }
        let mut connection = json!({
            "kind": "system-one", "uri": uri,
            "api": {"decision": {"wire_api": "system_one"}},
        });
        if let Some(api_key) = legacy.api_key.as_deref() {
            connection["auth"] = json!({"mode": "api_key", "api_key": api_key});
        }
        providers
            .connections
            .insert(provider.clone(), serde_json::from_value(connection)?);
        providers.models.insert_canonical_model(
            key,
            serde_json::from_value(json!({
                "capability": "decision", "provider": provider, "model": model,
            }))?,
        );
    }
    let raw = serde_json::to_value(&config.transcription)?;
    for family in [
        "groq",
        "openai",
        "deepgram",
        "assemblyai",
        "google",
        "local_whisper",
    ] {
        let input = if family == "groq" {
            if config
                .transcription
                .api_key
                .as_deref()
                .is_none_or(|key| key.trim().is_empty())
            {
                continue;
            }
            json!({"api_key": config.transcription.api_key, "uri": config.transcription.api_url,
                "model": config.transcription.model, "language": config.transcription.language,
                "initial_prompt": config.transcription.initial_prompt})
        } else {
            let Some(input) = raw.get(family).filter(|value| !value.is_null()) else {
                continue;
            };
            input.clone()
        };
        let key = ModelRef::from(format!("asr.{family}"));
        if providers.models.canonical_model(&key).is_some() {
            continue;
        }
        let provider = ProviderId::from(format!("legacy.asr.section.{family}"));
        let (connection, model) = asr_definition(&input, family, &provider)?;
        providers.connections.entry(provider).or_insert(connection);
        providers.models.insert_canonical_model(key, model);
    }
    Ok(providers)
}

/// One compatibility decoder for both historical ASR shapes.
fn asr_definition(
    raw: &serde_json::Value,
    family: &str,
    provider: &ProviderId,
) -> Result<(
    registry::ProviderConnection,
    ModelDefinition<serde_json::Value>,
)> {
    let mut connection = json!({"kind": family});
    if let Some(uri) = raw
        .get("uri")
        .or_else(|| raw.get("url"))
        .or_else(|| raw.get("api_url"))
        .filter(|value| !value.is_null())
    {
        connection["uri"] = uri.clone();
    }
    if let Some(key) = raw
        .get("api_key")
        .or_else(|| raw.get("bearer_token"))
        .filter(|value| !value.is_null())
    {
        connection["auth"] = json!({"mode":"api_key", "api_key":key});
    }
    if family == "local_whisper" {
        let mut api = json!({});
        for (old, new) in [
            ("timeout_secs", "request_timeout_secs"),
            ("max_audio_bytes", "max_audio_bytes"),
        ] {
            if let Some(value) = raw.get(old).filter(|value| !value.is_null()) {
                api[new] = value.clone();
            }
        }
        connection["api"] = json!({"asr":api});
    }
    let mut model = json!({"capability":"asr", "provider":provider.as_str(), "options":{}});
    if let Some(value) = raw.get("model").filter(|value| !value.is_null()) {
        model["model"] = value.clone();
    } else if family == "groq" {
        model["model"] = "whisper-large-v3-turbo".into();
    }
    for field in ["language", "initial_prompt"] {
        let value = raw.get(field).or_else(|| {
            (field == "language")
                .then(|| raw.get("language_code"))
                .flatten()
        });
        if let Some(value) = value.filter(|value| !value.is_null()) {
            model["options"][field] = value.clone();
        }
    }
    Ok((
        serde_json::from_value(connection)?,
        serde_json::from_value(model)?,
    ))
}

fn project(config: &Config) -> Result<ProviderRegistry> {
    Ok(project_provider_tables(&providers_for_input(config)?)?)
}

/// Convert embedding credentials and upstream IDs into ordinary connection/model
/// definitions. This is admission-time migration, never runtime construction.
fn promote_embedding_bindings(
    config: &mut Config,
    current: &ProviderRegistry,
    candidate: &mut ProviderRegistry,
) -> Result<()> {
    fn promote(
        candidate: &mut ProviderRegistry,
        current: &ProviderRegistry,
        reference: &str,
        model: &str,
        dimensions: usize,
        api_key: Option<&str>,
        suggested: &str,
    ) -> Result<Option<ModelRef>> {
        let reference = reference.trim();
        if reference.is_empty()
            || reference.eq_ignore_ascii_case("none")
            || reference.eq_ignore_ascii_case("sqlite")
        {
            return Ok(None);
        }
        if let Some(source) = current.models.get(reference)
            && source.capability() == Capability::Embedding
            && model.trim().is_empty()
            && api_key.is_none()
        {
            return Ok(Some(ModelRef::from(reference)));
        }
        let mut connection = if let Some(source) = current.models.get(reference) {
            let connection = current
                .connections
                .get(source.provider())
                .context("embedding source has no connection")?;
            if connection
                .implemented_capabilities()
                .contains(&Capability::Embedding)
            {
                ensure!(
                    connection.supports(Capability::Embedding),
                    "embedding capability is explicitly disabled on the connection"
                );
                connection.clone()
            } else if let Some(uri) = connection.uri() {
                serde_json::from_value(json!({"kind":"custom", "uri":uri,
                    "auth":{"mode":"api_key", "api_key":connection.api_key()},
                    "extra_headers":connection.extra_headers().cloned().unwrap_or_default(),
                    "tls_ca_cert_path":connection.tls_ca_cert_path()}))?
            } else {
                // The old configuration had no embedding protocol for this
                // chat-only provider. Preserve its disabled embedding state.
                return Ok(None);
            }
        } else if let Some(uri) = reference.strip_prefix("custom:") {
            serde_json::from_value(json!({"kind":"custom", "uri":uri}))?
        } else if matches!(reference, "openai" | "openrouter") {
            serde_json::from_value(json!({"kind":reference}))?
        } else {
            // Keep dangling references for Config::validate and startup repair;
            // the migration boundary must not discard unrelated sections.
            return Ok(Some(ModelRef::from(reference)));
        };
        let source = current.models.get(reference);
        let model_was_empty = model.trim().is_empty();
        let model = if model_was_empty {
            source
                .and_then(ModelDefinition::upstream_model)
                .unwrap_or_default()
        } else {
            model
        };
        let dimensions = match (model_was_empty, source) {
            (true, Some(ModelDefinition::Embedding(source))) => {
                source.metadata.dimensions.unwrap_or(dimensions)
            }
            _ => dimensions,
        };
        ensure!(
            !model.trim().is_empty() && dimensions > 0,
            "legacy embedding model and dimensions must be configured"
        );
        if let Some(api_key) = api_key.filter(|key| !key.trim().is_empty()) {
            let mut raw = serde_json::to_value(&connection)?;
            raw["auth"] = json!({"mode":"api_key", "api_key":api_key});
            connection = serde_json::from_value(raw)?;
        }
        let mut reference = ModelRef::from(suggested);
        let mut suffix = 2;
        while candidate.models.contains_key(&reference)
            || candidate.connections.contains_key(reference.as_str())
        {
            reference = format!("{suggested}.{suffix}").into();
            suffix += 1;
        }
        // Reuse the canonical connection when its complete definition matches.
        let definition = serde_json::to_value(&connection)?;
        let mut provider = ProviderId::from(reference.as_str());
        for (id, existing) in &candidate.connections {
            if serde_json::to_value(existing)? == definition {
                provider = id.clone();
                break;
            }
        }
        candidate
            .connections
            .entry(provider.clone())
            .or_insert(connection);
        candidate.models.insert(
            reference.clone(),
            ModelDefinition::Embedding(registry::EmbeddingModelConfig {
                provider,
                model: model.trim().to_owned(),
                metadata: registry::EmbeddingMetadata {
                    dimensions: Some(dimensions),
                },
            }),
        );
        Ok(Some(reference))
    }
    let had_model_ref = config
        .memory
        .embedding_model_ref
        .as_deref()
        .is_some_and(|reference| !reference.trim().is_empty());
    let selected_hint = config
        .memory
        .embedding_model
        .strip_prefix("hint:")
        .map(str::trim)
        .map(str::to_owned);
    for route in &mut config.embedding_routes {
        if let Some(reference) = promote(
            candidate,
            current,
            &route.model_provider,
            &route.model,
            route
                .dimensions
                .unwrap_or(config.memory.embedding_dimensions),
            route
                .api_key
                .as_deref()
                .or(config.memory.embedding_api_key.as_deref()),
            &format!("embedding.route.{}", route.hint),
        )? {
            route.model_provider = reference.to_string();
        }
        route.model.clear();
        route.dimensions = None;
        route.api_key = None;
    }
    if config
        .memory
        .embedding_model_ref
        .as_deref()
        .is_none_or(|reference| reference.trim().is_empty())
    {
        let selected = selected_hint
            .as_deref()
            .and_then(|hint| {
                config
                    .embedding_routes
                    .iter()
                    .find(|route| route.hint.trim() == hint)
            })
            .map(|route| ModelRef::from(route.model_provider.trim()));
        let reference = if let Some(reference) = selected {
            Some(reference)
        } else {
            promote(
                candidate,
                current,
                &config.memory.embedding_provider,
                &config.memory.embedding_model,
                config.memory.embedding_dimensions,
                config.memory.embedding_api_key.as_deref(),
                "embedding.memory",
            )?
        };
        config.memory.embedding_model_ref = reference.map(|reference| reference.to_string());
    }
    if had_model_ref && config.memory.embedding_api_key.is_some() {
        let reference = config
            .memory
            .embedding_model_ref
            .as_deref()
            .unwrap_or_default();
        // A memory-only credential must not mutate a shared model connection.
        let existing = candidate.clone();
        config.memory.embedding_model_ref = promote(
            candidate,
            &existing,
            reference,
            "",
            config.memory.embedding_dimensions,
            config.memory.embedding_api_key.as_deref(),
            "embedding.memory.credentials",
        )?
        .map(|reference| reference.to_string());
    }
    config.memory.embedding_provider = "none".into();
    config.memory.embedding_model = crate::schema::MemoryConfig::default().embedding_model;
    config.memory.embedding_api_key = None;
    Ok(())
}

fn speech_reference(
    providers: &Providers,
    registry: &ProviderRegistry,
    reference: &str,
    capability: Capability,
) -> ModelRef {
    let key = ModelRef::from(reference.trim());
    // Existing exact keys, including invalid capability bindings, retain their
    // meaning. Normalization must not replace the normal config validator.
    if providers.models.canonical_model(&key).is_some()
        || registry
            .models
            .get(&key)
            .is_some_and(|model| model.capability() == capability)
    {
        return key;
    }
    let prefix = match capability {
        Capability::Tts => "tts",
        Capability::Asr => "asr",
        _ => "chat",
    };
    let projected = ModelRef::from(format!("{prefix}.{}", key.as_str()));
    if registry
        .models
        .get(&projected)
        .is_some_and(|model| model.capability() == capability)
    {
        return projected;
    }
    if !reference.contains('.') {
        let default = ModelRef::from(format!("{}.default", key.as_str()));
        if registry
            .models
            .get(&default)
            .is_some_and(|model| model.capability() == capability)
        {
            return default;
        }
    }
    key
}

/// Redirect historical env names after file input has already been admitted.
/// Protocol/extra field maps are shared with the file migration decoder.
pub(crate) fn legacy_env_segments(config: &Config, parts: &[&str]) -> Option<Vec<String>> {
    let ["providers", "models", family, alias, field, rest @ ..] = parts else {
        return None;
    };
    let (reference, field, rest) = if *field == "models" {
        let [nested, field, rest @ ..] = rest else {
            return None;
        };
        (
            ModelRef::from(format!("{family}.{alias}.{nested}")),
            *field,
            rest,
        )
    } else {
        (ModelRef::from(format!("{family}.{alias}")), *field, rest)
    };
    let model = config.providers.models.canonical_model(&reference)?;
    let connection = config.providers.connections.get(model.provider())?;
    let kind = connection.kind();
    let mut target = vec!["providers".into()];
    let model_field = if matches!(field, "model" | "id") {
        Some(vec!["model"])
    } else if registry::ChatOptionsPatch::FIELDS.contains(&field) {
        Some(vec!["options", field])
    } else if matches!(field, "context_window" | "vision") {
        Some(vec!["metadata", field])
    } else if (kind == "azure" && field == "deployment")
        || (kind == "anthropic" && field == "server_fallback_models")
    {
        Some(vec!["extra", field])
    } else {
        None
    };
    if let Some(path) = model_field {
        target.extend(["models".into(), reference.to_string()]);
        target.extend(path.into_iter().map(str::to_owned));
    } else {
        target.extend(["connections".into(), model.provider().to_string()]);
        match field {
            "api_key" => target.extend(["auth".into(), "api_key".into()]),
            "requires_openai_auth"
            | "auth_mode"
            | "oauth_refresh_token"
            | "oauth_client_id"
            | "oauth_client_secret" => target.push("auth".into()),
            "uri" | "extra_headers" | "tls_ca_cert_path" => target.push(field.into()),
            "oauth_resource_url" if kind == "qwen" => target.push("uri".into()),
            "endpoint"
                if matches!(
                    kind,
                    "qwen" | "moonshot" | "stepfun" | "glm" | "minimax" | "zai" | "kilo"
                ) =>
            {
                target.extend(["extra".into(), "endpoint".into()]);
            }
            _ => {
                if let Some((_, renamed)) = super::model_routes::LEGACY_CONNECTION_EXTRA_FIELDS
                    .iter()
                    .find(|(old, _)| *old == field)
                {
                    target.extend(["extra".into(), (*renamed).into()]);
                } else if let Some((_, renamed)) = super::model_routes::legacy_chat_api_fields(kind)
                    .iter()
                    .find(|(old, _)| *old == field)
                {
                    target.extend(["api".into(), "chat".into(), (*renamed).into()]);
                } else {
                    return None;
                }
            }
        }
    }
    target.extend(rest.iter().map(|part| (*part).into()));
    Some(target)
}

/// A legacy memory env override is one input transaction. Seed its temporary
/// provider/model fields from the admitted binding, and snapshot every seeded
/// field so save restores the exact previous reference and registry.
pub(crate) fn prepare_memory_env_input(
    config: &mut Config,
    tail: &str,
) -> Result<Vec<(String, String)>> {
    if !matches!(
        tail,
        "memory__embedding_provider"
            | "memory__embedding_model"
            | "memory__embedding_dimensions"
            | "memory__embedding_api_key"
    ) {
        return Ok(Vec::new());
    }
    let Some(reference) = config
        .memory
        .embedding_model_ref
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
    else {
        return Ok(Vec::new());
    };
    let registry = config.provider_registry()?;
    let selected = registry.resolve_embedding(&reference.into())?;
    let reference = reference.to_owned();
    let model = selected.model.to_owned();
    let dimensions = selected
        .dimensions
        .unwrap_or(config.memory.embedding_dimensions);
    let snapshots = vec![
        (
            "memory.embedding_model_ref".into(),
            config
                .memory
                .embedding_model_ref
                .clone()
                .unwrap_or_default(),
        ),
        (
            "memory.embedding_provider".into(),
            config.memory.embedding_provider.clone(),
        ),
        (
            "memory.embedding_model".into(),
            config.memory.embedding_model.clone(),
        ),
        (
            "memory.embedding_dimensions".into(),
            config.memory.embedding_dimensions.to_string(),
        ),
    ];
    config.memory.embedding_model_ref = None;
    config.memory.embedding_provider = reference;
    config.memory.embedding_model = model;
    config.memory.embedding_dimensions = dimensions;
    Ok(snapshots)
}

/// Authentication-mode input replaces one auth object atomically. The
/// pre-override snapshot therefore restores the original mode and credential.
pub(crate) fn legacy_env_value(config: &Config, tail: &str, value: &str) -> Result<String> {
    let parts: Vec<_> = tail.split("__").collect();
    let ["providers", "models", family, alias, field] = parts.as_slice() else {
        return Ok(value.into());
    };
    let reference = ModelRef::from(format!("{family}.{alias}"));
    let Some(model) = config.providers.models.canonical_model(&reference) else {
        return Ok(value.into());
    };
    let connection = config
        .providers
        .connections
        .get(model.provider())
        .context("historical env target has no connection")?;
    let oauth_field = super::model_routes::LEGACY_OAUTH_FIELDS
        .iter()
        .find(|(old, _)| old == field);
    let mode = match *field {
        "requires_openai_auth" => {
            if value
                .trim()
                .parse::<bool>()
                .context("invalid requires_openai_auth override")?
            {
                "codex"
            } else {
                "api_key"
            }
        }
        "auth_mode" => match value.trim() {
            "oauth" | "o_auth" => "oauth",
            "api_key" => "api_key",
            _ => anyhow::bail!("unsupported historical auth_mode override"),
        },
        _ if oauth_field.is_some() => "oauth",
        _ => return Ok(value.into()),
    };
    let raw = serde_json::to_value(connection)?;
    let mut auth = json!({"mode":mode});
    if mode == "api_key" {
        if let Some(key) = connection.api_key() {
            auth["api_key"] = key.into();
        }
    } else if mode == "oauth" {
        for &(_, key) in super::model_routes::LEGACY_OAUTH_FIELDS {
            if let Some(value) = raw.get("auth").and_then(|auth| auth.get(key)) {
                auth[key] = value.clone();
            }
        }
        if let Some((_, key)) = oauth_field {
            auth[*key] = value.into();
        }
    }
    Ok(serde_json::to_string(&auth)?)
}

/// Normalize historical input before Config escapes deserialization or an
/// environment-input transaction. Never called by a registry consumer.
pub(crate) fn normalize(config: &mut Config) -> Result<()> {
    if !has_legacy_input(config) {
        config.providers.canonical_registry()?;
        return Ok(());
    }
    let source = (!config.pre_override_snapshots.is_empty()).then(|| config.clone());
    normalize_definitions(config)?;
    if let Some(source) = source {
        registry::env_input::remap_after_promotion(&source, config)?;
    }
    Ok(())
}

pub(crate) fn normalize_definitions(config: &mut Config) -> Result<()> {
    if !has_legacy_input(config) {
        config.providers.canonical_registry()?;
        return Ok(());
    }
    let current = project(config)?;
    let mut registry = current.clone();
    promote_embedding_bindings(config, &current, &mut registry)?;
    for agent in config.agents.values_mut() {
        if !agent.tts_provider.trim().is_empty() {
            let reference = speech_reference(
                &config.providers,
                &current,
                agent.tts_provider.as_str(),
                Capability::Tts,
            );
            agent.tts_provider = reference.to_string().into();
        }
        if !agent.transcription_provider.trim().is_empty() {
            let reference = speech_reference(
                &config.providers,
                &current,
                agent.transcription_provider.as_str(),
                Capability::Asr,
            );
            agent.transcription_provider = reference.to_string().into();
        }
    }
    registry.validate()?;
    config.providers.connections = registry.connections;
    config.providers.models = Default::default();
    config.providers.models.canonical_models =
        serde_json::from_value(serde_json::to_value(registry.models)?)?;
    config.providers.tts = Default::default();
    config.providers.transcription = Default::default();
    config.providers.model_presets = registry.model_presets;
    config.providers.fallback = registry.fallback;
    config.decision_models.clear();
    config.transcription.api_key = None;
    let transcription_defaults = crate::schema::TranscriptionConfig::default();
    config.transcription.api_url = transcription_defaults.api_url;
    config.transcription.model = transcription_defaults.model;
    config.transcription.language = None;
    config.transcription.initial_prompt = None;
    config.transcription.openai = None;
    config.transcription.deepgram = None;
    config.transcription.assemblyai = None;
    config.transcription.google = None;
    config.transcription.local_whisper = None;
    Ok(())
}

fn has_legacy_input(config: &Config) -> bool {
    !config.decision_models.is_empty()
        || config.memory.embedding_api_key.is_some()
        // A hint is a legacy selector even when the route table is already
        // canonical.  Without this bit the normalization gate returns early
        // and the replacement `embedding_model_ref` remains unset.
        || config.memory.embedding_model.trim().starts_with("hint:")
        || !config
            .memory
            .embedding_provider
            .trim()
            .eq_ignore_ascii_case("none")
        || config.embedding_routes.iter().any(|route| {
            !route.model.is_empty() || route.api_key.is_some() || route.dimensions.is_some()
        })
        || config.providers.models.iter_entries().next().is_some()
        || config.providers.tts.iter_entries().next().is_some()
        || config
            .providers
            .transcription
            .iter_entries()
            .next()
            .is_some()
        || config.transcription.api_key.is_some()
        || config.transcription.openai.is_some()
        || config.transcription.deepgram.is_some()
        || config.transcription.assemblyai.is_some()
        || config.transcription.google.is_some()
        || config.transcription.local_whisper.is_some()
}

/// Detect structural provider input before choosing a comment-preserving
/// version-only migration. No provider name is split into TOML path segments.
pub(crate) fn document_needs_migration(document: &toml::Value) -> bool {
    let providers = document.get("providers");
    document
        .get("decision_models")
        .and_then(toml::Value::as_table)
        .is_some_and(|models| !models.is_empty())
        || providers
            .and_then(|value| value.get("models"))
            .and_then(toml::Value::as_table)
            .is_some_and(|models| {
                models
                    .values()
                    .any(|model| model.get("capability").is_none())
            })
        || ["tts", "transcription"].iter().any(|section| {
            providers
                .and_then(|value| value.get(*section))
                .and_then(toml::Value::as_table)
                .is_some_and(|table| !table.is_empty())
        })
        || [
            "api_key",
            "openai",
            "deepgram",
            "assemblyai",
            "google",
            "local_whisper",
        ]
        .iter()
        .any(|field| {
            document
                .get("transcription")
                .and_then(|value| value.get(*field))
                .is_some()
        })
        || document.get("memory").is_some_and(|memory| {
            memory.get("embedding_api_key").is_some()
                || memory
                    .get("embedding_provider")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|provider| !provider.eq_ignore_ascii_case("none"))
        })
        || document
            .get("embedding_routes")
            .and_then(toml::Value::as_array)
            .is_some_and(|routes| {
                routes.iter().any(|route| {
                    ["model", "api_key", "dimensions"].iter().any(|field| {
                        route.get(*field).is_some_and(|value| {
                            value.as_str().is_none_or(|value| !value.trim().is_empty())
                        })
                    })
                })
            })
}

/// Persist exactly the same normalization as the in-memory input boundary,
/// retaining unrelated sections and their comments via the caller's TOML sync.
pub(crate) fn migrate_document(mut document: toml::Value) -> Result<toml::Value> {
    if !document_needs_migration(&document) {
        return Ok(document);
    }
    let mut input = toml::Table::new();
    for section in [
        "providers",
        "agents",
        "memory",
        "embedding_routes",
        "transcription",
        "decision_models",
    ] {
        if let Some(value) = document.get(section) {
            input.insert(section.into(), value.clone());
        }
    }
    let normalized: Config = toml::Value::Table(input)
        .try_into()
        .context("normalize provider configuration input")?;
    document["providers"] = toml::Value::try_from(&normalized.providers)?;
    if let Some(table) = document.as_table_mut() {
        table.remove("decision_models");
    }
    if let Some(agents) = document
        .get_mut("agents")
        .and_then(toml::Value::as_table_mut)
    {
        for (name, agent) in agents {
            if let Some(normalized) = normalized.agents.get(name) {
                for (field, value) in [
                    ("tts_provider", normalized.tts_provider.as_str()),
                    (
                        "transcription_provider",
                        normalized.transcription_provider.as_str(),
                    ),
                ] {
                    if agent.get(field).is_some() {
                        agent[field] = value.into();
                    }
                }
            }
        }
    }
    if let Some(memory) = document
        .get_mut("memory")
        .and_then(toml::Value::as_table_mut)
        && let Some(reference) = normalized.memory.embedding_model_ref
    {
        memory.insert("embedding_model_ref".into(), reference.into());
        for field in ["embedding_provider", "embedding_model", "embedding_api_key"] {
            memory.remove(field);
        }
    }
    if document.get("embedding_routes").is_some() {
        let mut routes = toml::Value::try_from(&normalized.embedding_routes)?;
        if let Some(routes) = routes.as_array_mut() {
            for route in routes {
                if let Some(route) = route.as_table_mut() {
                    for field in ["model", "api_key", "dimensions"] {
                        route.remove(field);
                    }
                }
            }
        }
        document["embedding_routes"] = routes;
    }
    if let Some(transcription) = document
        .get_mut("transcription")
        .and_then(toml::Value::as_table_mut)
    {
        for field in [
            "api_key",
            "api_url",
            "model",
            "language",
            "initial_prompt",
            "openai",
            "deepgram",
            "assemblyai",
            "google",
            "local_whisper",
        ] {
            transcription.remove(field);
        }
    }
    Ok(document)
}

// Historical selection is retained only as a migration/fixture decoding API.
impl Config {
    /// Resolve a model_provider reference into its provider profile, the
    /// selected model entry (if any), and the provider-local model id to send.
    ///
    /// Accepts two reference shapes:
    /// - Three or more segments first resolve the longest configured provider
    ///   alias. An exact alias wins; otherwise the remaining suffix selects a
    ///   named entry under the provider profile's `models` map. This preserves
    ///   legacy aliases containing dots while supporting nested model refs.
    /// - Two segments `<family>.<alias>` — selects a model entry by these
    ///   rules, in order: the entry named `default`; else, when exactly one
    ///   entry exists, that sole entry; else the provider profile's own `model`
    ///   field (legacy single-model behavior).
    ///
    /// The returned model id is the entry's `id` (falling back to the provider
    /// profile's `model`) for the three-segment / entry-selected cases, or the
    /// provider profile's `model` for the legacy path. Returns `None` when the
    /// reference is unparseable, the provider profile is missing, or a named
    /// three-segment model entry does not exist. Returns `Some` with
    /// `model_id == None` when no model id can be determined (caller decides
    /// whether an override rescues it).
    #[must_use]
    pub fn resolve_model_selection<'a>(
        &'a self,
        model_provider_ref: &str,
    ) -> Option<ModelSelection<'a>> {
        let (family, tail) = model_provider_ref.split_once('.')?;
        if family.is_empty() || tail.is_empty() {
            return None;
        }

        let (family, alias_str) = self.providers.models.profile_ref(model_provider_ref)?;
        let (family_key, alias_str, entry) = self
            .providers
            .models
            .iter_entries()
            .find(|(ty, alias, _)| *ty == family && *alias == alias_str)?;
        let model_alias = tail
            .strip_prefix(alias_str)?
            .strip_prefix('.')
            .filter(|model_alias| !model_alias.is_empty());

        let model_entry: Option<(&str, &ModelEntryConfig)> = match model_alias {
            // Three-segment ref: the named entry must exist.
            Some(name) => Some((name, entry.models.get(name)?)),
            // Two-segment ref: prefer `default`, then a sole entry, else legacy.
            None => entry
                .models
                .get("default")
                .map(|e| ("default", e))
                .or_else(|| {
                    if entry.models.len() == 1 {
                        entry.models.iter().next().map(|(k, v)| (k.as_str(), v))
                    } else {
                        None
                    }
                }),
        };

        let model_id = match model_entry {
            Some((_, e)) => {
                e.id.as_deref()
                    .or(entry.model.as_deref())
                    .map(str::to_string)
            }
            None => entry.model.as_deref().map(str::to_string),
        };

        Some(ModelSelection {
            family: family_key,
            alias: alias_str,
            entry,
            model_entry: model_entry.map(|(_, e)| e),
            model_alias: model_entry.map(|(k, _)| k.to_string()),
            model_id,
        })
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl Config {
    /// Historical section-only injection is translated here, before any
    /// capability consumer is constructed.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn from_transcription_input(input: &crate::schema::TranscriptionConfig) -> Self {
        serde_json::from_value(json!({"transcription":input}))
            .expect("valid transcription fixture input")
    }
    /// Historical family/alias input used by fixture-level factory adapters.
    /// Joining this shape is config compatibility, never protocol dispatch.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn chat_registry_for_legacy_alias(
        &self,
        family: &str,
        alias: &str,
    ) -> Result<(ProviderRegistry, ModelRef)> {
        let reference = ModelRef::from(format!("{family}.{alias}"));
        let mut admitted = self.clone();
        crate::migration::provider_inputs::normalize(&mut admitted)?;
        let registry = admitted.chat_registry_for_input(None, reference.as_str())?;
        Ok((registry, reference))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_decision_migration_matches_configuration_admission() {
        let source = r#"
            schema_version = 3
            [decision_models."gpu.v2"]
            provider = "custom"
            base_url = "http://127.0.0.1:8000"
            model = "laya"
            [sop]
            sops_dir = "shared/sops"
        "#;
        let document = migrate_document(toml::from_str(source).unwrap()).unwrap();
        assert!(document.get("decision_models").is_none());
        assert_eq!(document["sop"]["sops_dir"].as_str(), Some("shared/sops"));
        let admitted: Config = toml::from_str(source).unwrap();
        assert_eq!(
            document["providers"],
            toml::Value::try_from(&admitted.providers).unwrap()
        );
        assert!(!document_needs_migration(&document));
        assert_eq!(migrate_document(document.clone()).unwrap(), document);
        let migrated = crate::migration::migrate_file(source).unwrap().unwrap();
        let persisted: toml::Value = toml::from_str(&migrated).unwrap();
        assert_eq!(persisted["schema_version"].as_integer(), Some(4));
        assert!(persisted.get("decision_models").is_none());
        assert_eq!(persisted["providers"], document["providers"]);
    }

    #[test]
    fn decision_input_promotes_exact_keys_and_preserves_authentication() {
        let config: Config = toml::from_str(
            r#"
            [decision_models."local.v1"]
            provider = "laya"
            api_key = "fixture-key"

            [decision_models.hosted]

            [decision_models.invalid]
            provider = "custom"
        "#,
        )
        .unwrap();
        assert!(config.decision_models.is_empty());
        let registry = config.provider_registry().unwrap();
        let local = registry.resolve_decision(&"local.v1".into()).unwrap();
        assert_eq!(local.model, "laya");
        assert_eq!(
            local.selection.connection.uri(),
            Some("http://127.0.0.1:8000")
        );
        assert_eq!(local.selection.connection.api_key(), Some("fixture-key"));
        assert_eq!(local.wire_api, crate::schema::DecisionWireApi::SystemOne);
        let hosted = registry.resolve_decision(&"hosted".into()).unwrap();
        assert_eq!(hosted.model, "jev-latest");
        assert_eq!(
            hosted.selection.connection.uri(),
            Some("https://api.typesafe.ai")
        );
        assert!(registry.resolve_decision(&"invalid".into()).is_err());
        let serialized = serde_json::to_value(&config).unwrap();
        assert!(serialized.get("decision_models").is_none());
        let roundtrip: Config = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), serialized);
    }

    #[test]
    fn canonical_decision_selection_wins_over_legacy_input() {
        let config: Config = toml::from_str(
            r#"
            [decision_models.shared]
            provider = "laya"

            [providers.connections.native]
            kind = "openai"

            [providers.models.shared]
            capability = "decision"
            provider = "native"
            model = "native-decision"
        "#,
        )
        .unwrap();
        let registry = config.provider_registry().unwrap();
        let resolved = registry.resolve_decision(&"shared".into()).unwrap();
        assert_eq!(resolved.model, "native-decision");
        assert_eq!(resolved.selection.provider_id.as_str(), "native");
        assert_eq!(config.providers.connections.len(), 1);
    }

    #[test]
    fn decision_env_credentials_follow_promotion_without_persisting_the_override() {
        let mut config = Config::default();
        config.decision_models.insert(
            "hosted".into(),
            crate::schema::SopDecisionModelConfig {
                api_key: Some("disk-fixture-key".into()),
                ..Default::default()
            },
        );
        let path = "decision_models.hosted.api_key";
        let saved = crate::env_overrides::raw_value_for_path(&config, path).unwrap();
        config.decision_models.get_mut("hosted").unwrap().api_key = Some("env-fixture-key".into());
        config.pre_override_snapshots.insert(path.into(), saved);
        config.env_overridden_paths.insert(path.into());
        normalize(&mut config).unwrap();
        assert_eq!(
            config.providers.connections["legacy.decision.hosted"].api_key(),
            Some("env-fixture-key")
        );
        let snapshots = config.pre_override_snapshots.clone();
        crate::env_overrides::mask_env_overrides_for_save(&mut config, &snapshots).unwrap();
        assert_eq!(
            config.providers.connections["legacy.decision.hosted"].api_key(),
            Some("disk-fixture-key")
        );
        assert!(config.decision_models.is_empty());
    }

    #[test]
    fn env_credential_snapshots_follow_the_migrated_connection() {
        let mut config = Config::default();
        let profile = config.providers.models.ensure("openai", "work").unwrap();
        profile.model = Some("chat-model".into());
        profile.api_key = Some("disk-key".into());
        let path = "providers.models.openai.work.api_key";
        let saved = crate::env_overrides::raw_value_for_path(&config, path).unwrap();
        config
            .providers
            .models
            .openai
            .get_mut("work")
            .unwrap()
            .base
            .api_key = Some("env-key".into());
        config.pre_override_snapshots.insert(path.into(), saved);
        config.env_overridden_paths.insert(path.into());
        normalize(&mut config).unwrap();
        assert!(config.providers.models.openai.is_empty());
        assert_eq!(
            config.providers.connections["legacy.openai.work"].api_key(),
            Some("env-key")
        );
        assert!(!config.pre_override_snapshots.contains_key(path));
        let snapshots = config.pre_override_snapshots.clone();
        crate::env_overrides::mask_env_overrides_for_save(&mut config, &snapshots).unwrap();
        assert_eq!(
            config.providers.connections["legacy.openai.work"].api_key(),
            Some("disk-key")
        );
        assert!(config.providers.models.openai.is_empty());
    }

    #[test]
    fn env_created_embedding_binding_is_not_persisted() {
        let mut config = Config::default();
        config
            .pre_override_snapshots
            .insert("memory.embedding_provider".into(), "none".into());
        config
            .pre_override_snapshots
            .insert("memory.embedding_api_key".into(), String::new());
        config
            .env_overridden_paths
            .extend(config.pre_override_snapshots.keys().cloned());
        config.memory.embedding_provider = "openai".into();
        config.memory.embedding_api_key = Some("env-fixture-secret".into());
        normalize(&mut config).unwrap();
        assert!(config.memory.embedding_model_ref.is_some());
        let snapshots = config.pre_override_snapshots.clone();
        crate::env_overrides::mask_env_overrides_for_save(&mut config, &snapshots).unwrap();
        assert!(config.memory.embedding_model_ref.is_none());
        assert!(config.providers.connections.is_empty());
        assert!(
            !serde_json::to_string(&config)
                .unwrap()
                .contains("env-fixture-secret")
        );
    }

    #[test]
    fn embedding_route_binding_snapshot_restores_a_collection() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("openai", "work")
            .unwrap()
            .model = Some("chat".into());
        config
            .embedding_routes
            .push(crate::schema::EmbeddingRouteConfig {
                hint: "semantic.v1".into(),
                model_provider: "openai.work".into(),
                model: "embedding-upstream".into(),
                api_key: Some("env-route-secret".into()),
                ..Default::default()
            });
        config.pre_override_snapshots.insert(
            "embedding_routes.semantic.v1.model_provider".into(),
            String::new(),
        );
        config
            .pre_override_snapshots
            .insert("embedding_routes.semantic.v1.api_key".into(), String::new());
        normalize(&mut config).unwrap();
        assert!(
            config
                .pre_override_snapshots
                .contains_key("embedding_routes")
        );
        let snapshots = config.pre_override_snapshots.clone();
        crate::env_overrides::mask_env_overrides_for_save(&mut config, &snapshots).unwrap();
        assert!(config.embedding_routes[0].model_provider.is_empty());
        assert!(config.embedding_routes[0].api_key.is_none());
        assert!(
            !serde_json::to_string(&config)
                .unwrap()
                .contains("env-route-secret")
        );
    }

    #[test]
    fn unresolved_speech_reference_is_left_for_normal_config_validation() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("openai", "work")
            .unwrap()
            .model = Some("chat-model".into());
        config.agents.insert(
            "worker".into(),
            crate::schema::AliasedAgentConfig {
                model_provider: "openai.work".into(),
                transcription_provider: "missing.model".into(),
                ..Default::default()
            },
        );
        normalize(&mut config).unwrap();
        assert_eq!(
            config
                .agent("worker")
                .unwrap()
                .transcription_provider
                .as_str(),
            "missing.model"
        );
        assert!(config.validate().is_err());
    }
}
