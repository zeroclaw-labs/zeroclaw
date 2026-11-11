//! Admission-time validation and inheritance for the canonical registry.

use std::num::NonZeroU64;

use crate::schema::{DecisionWireApi, WireApi};
use zeroclaw_api::capability::Capability;

use super::{
    AsrModelConfig, ChatModelConfig, ChatOptionsPatch, ModelDefinition, ModelPreset, ModelRef,
    Override, Preset, PresetId, ProviderConnection, ProviderRegistry, RegistryError,
    TtsModelConfig,
};

pub(super) fn validate_preset(name: &PresetId, preset: &ModelPreset) -> Result<(), RegistryError> {
    // Preset IDs are TOML keys, not structured paths.  A dot in an ID is
    // part of the key (for example `voice.en-US.default`) and must never be
    // interpreted as a capability prefix.  Capability compatibility is
    // checked where a model references the preset, by `ensure_preset` and the
    // capability-specific resolvers below.
    let _ = (name, preset);
    Ok(())
}

impl ProviderRegistry {
    pub(super) fn validate_connection(
        &self,
        name: &super::ProviderId,
        connection: &ProviderConnection,
    ) -> Result<(), RegistryError> {
        if let Some(uri) = connection
            .uri()
            .map(str::trim)
            .filter(|uri| !uri.is_empty())
        {
            let parsed = reqwest::Url::parse(uri).map_err(|_| {
                RegistryError::connection(name, "uri", "uri must be a valid HTTP(S) URL")
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(RegistryError::connection(
                    name,
                    "uri",
                    "uri must use http/https",
                ));
            }
        }
        let configured = connection.configured_apis();
        for capability in configured {
            if !connection.supports(capability) {
                return Err(RegistryError::connection(
                    name,
                    "api",
                    "configured API is unsupported by the connection kind",
                ));
            }
        }
        for (capability, path, preset) in [
            (
                Capability::Chat,
                "api.chat.preset",
                connection.chat_defaults().and_then(|(preset, _)| preset),
            ),
            (
                Capability::Tts,
                "api.tts.preset",
                connection.tts_defaults().and_then(|(preset, _)| preset),
            ),
            (
                Capability::Asr,
                "api.asr.preset",
                connection.asr_defaults().and_then(|(preset, _)| preset),
            ),
        ] {
            if let Some(preset) = preset {
                let found = self.model_presets.get(preset).ok_or_else(|| {
                    RegistryError::connection(name, path, "preset does not exist")
                })?;
                if preset_capability(found) != capability {
                    return Err(RegistryError::connection(
                        name,
                        path,
                        "preset capability does not match API capability",
                    ));
                }
            }
        }
        if let Some(wire_api) = connection.decision_api().and_then(|api| api.wire_api)
            && !connection.decision_wire_apis().contains(&wire_api)
        {
            return Err(RegistryError::connection(
                name,
                "api.decision.wire_api",
                "native decision protocol is unsupported by the connection kind",
            ));
        }
        if connection.kind() == "azure" && connection.uri().is_some() {
            return Err(RegistryError::connection(
                name,
                "uri",
                "azure derives its endpoint from extra.resource",
            ));
        }
        if connection.kind() == "system-one" && !connection.supports(Capability::Decision) {
            return Err(RegistryError::connection(
                name,
                "capabilities",
                "system-one must enable decision",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_model(
        &self,
        name: &ModelRef,
        model: &ModelDefinition,
    ) -> Result<(), RegistryError> {
        let connection = self
            .connections
            .get(model.provider())
            .ok_or_else(|| RegistryError::model(name, "provider", "connection does not exist"))?;
        match model {
            ModelDefinition::Chat(config) => {
                self.validate_chat(name, config, connection)?;
            }
            ModelDefinition::Embedding(config) => {
                if config.model.trim().is_empty() {
                    return Err(RegistryError::model(
                        name,
                        "model",
                        "embedding model must not be empty",
                    ));
                }
            }
            ModelDefinition::Tts(config) => self.validate_tts(name, config, connection)?,
            ModelDefinition::Asr(config) => self.validate_asr(name, config, connection)?,
            ModelDefinition::Decision(config) => {
                if !connection.supports(Capability::Decision) {
                    return Err(RegistryError::model(
                        name,
                        "provider",
                        "connection does not support decision models",
                    ));
                }
                if let Some(wire_api) = config.api.as_ref().and_then(|api| api.wire_api)
                    && !connection.decision_wire_apis().contains(&wire_api)
                {
                    return Err(RegistryError::model(
                        name,
                        "api.wire_api",
                        "native decision protocol is unsupported by the connection kind",
                    ));
                }
                if config.model.trim().is_empty() {
                    return Err(RegistryError::model(
                        name,
                        "model",
                        "decision model must not be empty",
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_chat(
        &self,
        name: &ModelRef,
        config: &ChatModelConfig,
        connection: &ProviderConnection,
    ) -> Result<(), RegistryError> {
        if config
            .model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty())
        {
            return Err(RegistryError::model(
                name,
                "model",
                "model must not be empty when set",
            ));
        }
        if config.metadata.context_window == Some(0) {
            return Err(RegistryError::model(
                name,
                "metadata.context_window",
                "context window must be greater than 0",
            ));
        }
        let effective = self.resolve_chat(name)?;
        if let Some(super::Override::Set(temperature)) = effective.options.temperature {
            crate::schema::validate_temperature(temperature)
                .map_err(|error| RegistryError::model(name, "options.temperature", error))?;
        }
        if let Some(pricing) = &config.metadata.pricing {
            for (field, value) in [
                ("input_per_mtok", pricing.input_per_mtok),
                ("output_per_mtok", pricing.output_per_mtok),
                ("cached_input_per_mtok", pricing.cached_input_per_mtok),
                ("cache_write_per_mtok", pricing.cache_write_per_mtok),
            ] {
                if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                    return Err(RegistryError::model(
                        name,
                        &format!("metadata.pricing.{field}"),
                        "rate must be finite and non-negative",
                    ));
                }
            }
        }
        if config
            .api
            .as_ref()
            .is_some_and(|api| api.wire_api.is_some() && connection.chat_api().is_none())
        {
            return Err(RegistryError::model(
                name,
                "api.wire_api",
                "connection has no chat API configuration",
            ));
        }
        if let Some(preset) = config.preset.as_ref() {
            ensure_preset(self, name, preset, Capability::Chat)?;
        }
        Ok(())
    }

    fn validate_tts(
        &self,
        name: &ModelRef,
        config: &TtsModelConfig,
        connection: &ProviderConnection,
    ) -> Result<(), RegistryError> {
        if let Some(preset) = config.preset.as_ref() {
            ensure_preset(self, name, preset, Capability::Tts)?;
        }
        let _ = connection;
        Ok(())
    }

    fn validate_asr(
        &self,
        name: &ModelRef,
        config: &AsrModelConfig,
        connection: &ProviderConnection,
    ) -> Result<(), RegistryError> {
        if let Some(preset) = config.preset.as_ref() {
            ensure_preset(self, name, preset, Capability::Asr)?;
        }
        let _ = connection;
        Ok(())
    }

    pub fn resolve_chat(&self, name: &ModelRef) -> Result<ResolvedChat<'_>, RegistryError> {
        let selection = self.selection(name, Capability::Chat)?;
        let ModelDefinition::Chat(model) = selection.model else {
            unreachable!("selection checks capability")
        };
        let mut options = ChatOptionsPatch::default();
        let mut wire_api = None;
        let mut request_timeout_secs = None;
        let mut read_timeout_secs = None;
        let mut live_pricing = None;
        if let Some(api) = selection.connection.chat_api() {
            wire_api = api.wire_api;
            request_timeout_secs = api.request_timeout_secs.clone();
            read_timeout_secs = api.read_timeout_secs.clone();
            live_pricing = api.live_pricing;
        }
        // CLI and Hailo drivers have their own transport object, but inherit
        // the same chat preset/options shape as HTTP drivers.
        if let Some((preset, defaults)) = selection.connection.chat_defaults() {
            if let Some(preset) = preset {
                apply_chat_preset(&self.model_presets, preset, &mut options)?;
            }
            options.overlay(defaults);
        }
        if let Some(preset) = &model.preset {
            apply_chat_preset(&self.model_presets, preset, &mut options)?;
        }
        if let Some(api) = &model.api {
            wire_api = api.wire_api.or(wire_api);
            request_timeout_secs = api.request_timeout_secs.clone().or(request_timeout_secs);
            read_timeout_secs = api.read_timeout_secs.clone().or(read_timeout_secs);
        }
        options.overlay(&model.options);
        Ok(ResolvedChat {
            selection,
            wire_api,
            request_timeout_secs,
            read_timeout_secs,
            live_pricing,
            options,
        })
    }
}

fn ensure_preset(
    registry: &ProviderRegistry,
    model: &ModelRef,
    preset: &PresetId,
    capability: Capability,
) -> Result<(), RegistryError> {
    let found = registry
        .model_presets
        .get(preset)
        .ok_or_else(|| RegistryError::model(model, "preset", "preset does not exist"))?;
    if preset_capability(found) != capability {
        return Err(RegistryError::model(
            model,
            "preset",
            "preset capability does not match model capability",
        ));
    }
    Ok(())
}

fn preset_capability(preset: &ModelPreset) -> Capability {
    match preset {
        ModelPreset::Chat(_) => Capability::Chat,
        ModelPreset::Tts(_) => Capability::Tts,
        ModelPreset::Asr(_) => Capability::Asr,
    }
}

fn apply_chat_preset(
    presets: &std::collections::BTreeMap<PresetId, ModelPreset>,
    preset: &PresetId,
    options: &mut ChatOptionsPatch,
) -> Result<(), RegistryError> {
    match presets.get(preset) {
        Some(ModelPreset::Chat(Preset { options: patch })) => options.overlay(patch),
        Some(_) => {
            return Err(RegistryError::new(
                ["providers", "model_presets", preset.as_str()],
                "preset capability does not match chat",
            ));
        }
        None => {
            return Err(RegistryError::new(
                ["providers", "model_presets", preset.as_str()],
                "preset does not exist",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ResolvedChat<'a> {
    pub selection: super::ModelSelection<'a>,
    pub wire_api: Option<WireApi>,
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
    pub read_timeout_secs: Option<Override<NonZeroU64>>,
    pub live_pricing: Option<bool>,
    pub options: ChatOptionsPatch,
}

impl ResolvedChat<'_> {
    pub fn upstream_model(&self) -> Option<&str> {
        self.selection.model.upstream_model()
    }

    /// Resolve explicit API/preset/model options over provider defaults.
    #[must_use]
    pub fn text_extraction(&self) -> super::TextExtraction {
        let defaults = super::TextExtraction::for_provider(
            self.selection.connection.kind(),
            self.wire_api,
            self.options.native_tools,
        );
        super::TextExtraction {
            reasoning: self
                .options
                .extract_text_reasoning
                .unwrap_or(defaults.reasoning),
            tool_calls: self
                .options
                .extract_text_tool_calls
                .unwrap_or(defaults.tool_calls),
        }
    }

    /// Whether textual `<think>` blocks are extracted from the response.
    #[must_use]
    pub fn extract_text_reasoning(&self) -> bool {
        self.text_extraction().reasoning
    }

    /// Whether textual XML/JSON tool-call envelopes are extracted from the
    /// response. Native tool calls are unaffected by this setting.
    #[must_use]
    pub fn extract_text_tool_calls(&self) -> bool {
        self.text_extraction().tool_calls
    }
}

impl ProviderRegistry {
    pub fn resolve_tts(&self, name: &ModelRef) -> Result<ResolvedTts<'_>, RegistryError> {
        let selection = self.selection(name, Capability::Tts)?;
        let super::ModelDefinition::Tts(model) = selection.model else {
            unreachable!("selection checks capability")
        };
        let mut options = super::TtsOptionsPatch::default();
        if let Some((preset, defaults)) = selection.connection.tts_defaults() {
            if let Some(preset) = preset {
                apply_tts_preset(&self.model_presets, preset, &mut options)?;
            }
            options.overlay(defaults);
        }
        if let Some(preset) = &model.preset {
            apply_tts_preset(&self.model_presets, preset, &mut options)?;
        }
        options.overlay(&model.options);
        Ok(ResolvedTts { selection, options })
    }

    pub fn resolve_asr(&self, name: &ModelRef) -> Result<ResolvedAsr<'_>, RegistryError> {
        let selection = self.selection(name, Capability::Asr)?;
        let super::ModelDefinition::Asr(model) = selection.model else {
            unreachable!("selection checks capability")
        };
        let mut options = super::AsrOptionsPatch::default();
        let mut request_timeout_secs = None;
        if let Some((preset, defaults)) = selection.connection.asr_defaults() {
            if let Some(preset) = preset {
                apply_asr_preset(&self.model_presets, preset, &mut options)?;
            }
            options.overlay(defaults);
        }
        if let super::ProviderConnection::LocalWhisper(connection) = selection.connection
            && let Some(api) = &connection.api.asr
        {
            request_timeout_secs = api.request_timeout_secs.clone();
        }
        if let Some(preset) = &model.preset {
            apply_asr_preset(&self.model_presets, preset, &mut options)?;
        }
        if let Some(api) = &model.api {
            request_timeout_secs = api.request_timeout_secs.clone();
        }
        options.overlay(&model.options);
        Ok(ResolvedAsr {
            selection,
            request_timeout_secs,
            options,
        })
    }

    pub fn resolve_embedding(
        &self,
        name: &ModelRef,
    ) -> Result<ResolvedEmbedding<'_>, RegistryError> {
        let selection = self.selection(name, Capability::Embedding)?;
        let super::ModelDefinition::Embedding(model) = selection.model else {
            unreachable!("selection checks capability")
        };
        Ok(ResolvedEmbedding {
            selection,
            model: &model.model,
            dimensions: model.metadata.dimensions,
        })
    }

    pub fn resolve_decision(&self, name: &ModelRef) -> Result<ResolvedDecision<'_>, RegistryError> {
        let selection = self.selection(name, Capability::Decision)?;
        let super::ModelDefinition::Decision(model) = selection.model else {
            unreachable!("selection checks capability")
        };
        Ok(ResolvedDecision {
            selection,
            model: &model.model,
            wire_api: model
                .api
                .as_ref()
                .and_then(|api| api.wire_api)
                .or_else(|| {
                    selection
                        .connection
                        .decision_api()
                        .and_then(|api| api.wire_api)
                })
                .or_else(|| selection.connection.default_decision_wire_api())
                .ok_or_else(|| {
                    RegistryError::model(
                        name,
                        "provider",
                        "connection has no native decision protocol",
                    )
                })?,
            request_timeout_secs: model
                .api
                .as_ref()
                .and_then(|api| api.request_timeout_secs.clone())
                .or_else(|| {
                    selection
                        .connection
                        .decision_api()
                        .and_then(|api| api.request_timeout_secs.clone())
                }),
        })
    }

    pub fn fallback_targets(&self, source: &ModelRef) -> Result<&[ModelRef], RegistryError> {
        let targets = self.fallback.get(source).map(Vec::as_slice).unwrap_or(&[]);
        self.selection(source, Capability::Chat)?;
        for target in targets {
            self.selection(target, Capability::Chat)?;
        }
        Ok(targets)
    }

    /// Return every reachable fallback in declaration order. The source is
    /// excluded; callers prepend it when constructing the primary attempt.
    /// Duplicate edges are skipped while the graph's validation still rejects
    /// duplicate targets and cycles in persisted configuration.
    pub fn fallback_chain(&self, source: &ModelRef) -> Result<Vec<ModelRef>, RegistryError> {
        self.fallback_targets(source)?;
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut pending: Vec<ModelRef> = self
            .fallback
            .get(source)
            .into_iter()
            .flat_map(|targets| targets.iter().rev().cloned())
            .collect();

        while let Some(candidate) = pending.pop() {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            self.selection(&candidate, Capability::Chat)?;
            chain.push(candidate.clone());
            if let Some(targets) = self.fallback.get(&candidate) {
                pending.extend(targets.iter().rev().cloned());
            }
        }
        Ok(chain)
    }
}

fn apply_tts_preset(
    presets: &std::collections::BTreeMap<PresetId, ModelPreset>,
    preset: &PresetId,
    options: &mut super::TtsOptionsPatch,
) -> Result<(), RegistryError> {
    match presets.get(preset) {
        Some(ModelPreset::Tts(Preset { options: patch })) => options.overlay(patch),
        Some(_) => {
            return Err(RegistryError::new(
                ["providers", "model_presets", preset.as_str()],
                "preset capability does not match tts",
            ));
        }
        None => {
            return Err(RegistryError::new(
                ["providers", "model_presets", preset.as_str()],
                "preset does not exist",
            ));
        }
    }
    Ok(())
}

fn apply_asr_preset(
    presets: &std::collections::BTreeMap<PresetId, ModelPreset>,
    preset: &PresetId,
    options: &mut super::AsrOptionsPatch,
) -> Result<(), RegistryError> {
    match presets.get(preset) {
        Some(ModelPreset::Asr(Preset { options: patch })) => options.overlay(patch),
        Some(_) => {
            return Err(RegistryError::new(
                ["providers", "model_presets", preset.as_str()],
                "preset capability does not match asr",
            ));
        }
        None => {
            return Err(RegistryError::new(
                ["providers", "model_presets", preset.as_str()],
                "preset does not exist",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ResolvedTts<'a> {
    pub selection: super::ModelSelection<'a>,
    pub options: super::TtsOptionsPatch,
}

#[derive(Debug, Clone)]
pub struct ResolvedAsr<'a> {
    pub selection: super::ModelSelection<'a>,
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
    pub options: super::AsrOptionsPatch,
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedEmbedding<'a> {
    pub selection: super::ModelSelection<'a>,
    pub model: &'a str,
    pub dimensions: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ResolvedDecision<'a> {
    pub selection: super::ModelSelection<'a>,
    pub model: &'a str,
    pub wire_api: DecisionWireApi,
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
}
