//! Admission-time validation and inheritance for the canonical registry.

use std::num::NonZeroU64;

use crate::schema::WireApi;

use super::{
    AsrModelConfig, Capability, ChatModelConfig, ChatOptionsPatch, ModelDefinition, ModelPreset,
    ModelRef, Override, Preset, PresetId, ProviderConnection, ProviderRegistry, RegistryError,
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
                if connection.kind() != "system-one" {
                    return Err(RegistryError::model(
                        name,
                        "provider",
                        "decision models require a system-one connection",
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
        if let Some(api) = connection.chat_api()
            && api
                .preset
                .as_ref()
                .is_some_and(|preset| !self.model_presets.contains_key(preset))
        {
            return Err(RegistryError::connection(
                &config.provider,
                "api.chat.preset",
                "preset does not exist",
            ));
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
            if let Some(preset) = &api.preset {
                apply_chat_preset(&self.model_presets, preset, &mut options)?;
            }
            options.overlay(&api.options);
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
    let actual = match found {
        ModelPreset::Chat(_) => Capability::Chat,
        ModelPreset::Tts(_) => Capability::Tts,
        ModelPreset::Asr(_) => Capability::Asr,
    };
    if actual != capability {
        return Err(RegistryError::model(
            model,
            "preset",
            "preset capability does not match model capability",
        ));
    }
    Ok(())
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

#[derive(Debug, Clone, Copy)]
pub struct ResolvedDecision<'a> {
    pub selection: super::ModelSelection<'a>,
    pub model: &'a str,
}
