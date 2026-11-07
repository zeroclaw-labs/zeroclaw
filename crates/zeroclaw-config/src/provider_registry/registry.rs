use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use super::{ModelDefinition, ModelPreset, ModelRef, PresetId, ProviderConnection, ProviderId};
use zeroclaw_api::capability::Capability;

/// The only stored provider/model state. All effective plans are materialized
/// from this registry at the existing admission boundary.
#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProviderRegistry {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<ProviderId, ProviderConnection>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<ModelRef, ModelDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_presets: BTreeMap<PresetId, ModelPreset>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fallback: BTreeMap<ModelRef, Vec<ModelRef>>,
}

impl<'de> Deserialize<'de> for ProviderRegistry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Only extra is transiently untyped. Ordinary fields, options and
        // container strictness use the same definitions as canonical state.
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct Input {
            connections: BTreeMap<ProviderId, ProviderConnection>,
            models: BTreeMap<ModelRef, ModelDefinition<serde_json::Value>>,
            model_presets: BTreeMap<PresetId, ModelPreset>,
            fallback: BTreeMap<ModelRef, Vec<ModelRef>>,
        }
        let input = Input::deserialize(deserializer)?;
        Self::from_raw_parts(
            input.connections,
            input.models,
            input.model_presets,
            input.fallback,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl ProviderRegistry {
    pub(crate) fn from_raw_parts(
        connections: BTreeMap<ProviderId, ProviderConnection>,
        raw_models: BTreeMap<ModelRef, ModelDefinition<serde_json::Value>>,
        model_presets: BTreeMap<PresetId, ModelPreset>,
        fallback: BTreeMap<ModelRef, Vec<ModelRef>>,
    ) -> Result<Self, RegistryError> {
        let mut models = BTreeMap::new();
        for (name, model) in raw_models {
            let connection = connections.get(model.provider()).ok_or_else(|| {
                RegistryError::model(&name, "provider", "connection does not exist")
            })?;
            let model = model
                .associate(connection)
                .map_err(|error| RegistryError::model(&name, "extra", error.to_string()))?;
            models.insert(name, model);
        }
        let registry = Self {
            connections,
            models,
            model_presets,
            fallback,
        };
        registry.validate()?;
        Ok(registry)
    }
}

/// A structured path is kept until presentation. A dot in a registry key never
/// becomes a path separator. Errors describe constraints, not configured values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{display_path}: {message}")]
pub struct RegistryError {
    pub path: Vec<String>,
    pub message: String,
    display_path: String,
}

impl RegistryError {
    pub fn new(
        path: impl IntoIterator<Item = impl Into<String>>,
        message: impl Into<String>,
    ) -> Self {
        let path: Vec<String> = path.into_iter().map(Into::into).collect();
        let display_path = path
            .iter()
            .map(|key| toml_edit::Key::new(key.as_str()).to_string())
            .collect::<Vec<_>>()
            .join(".");
        Self {
            path,
            message: message.into(),
            display_path,
        }
    }

    pub(super) fn model(name: &ModelRef, leaf: &str, message: impl Into<String>) -> Self {
        let mut path = vec![
            "providers".to_owned(),
            "models".to_owned(),
            name.to_string(),
        ];
        if !leaf.is_empty() {
            path.extend(leaf.split('.').map(str::to_owned));
        }
        Self::new(path, message)
    }

    pub(super) fn connection(name: &ProviderId, leaf: &str, message: impl Into<String>) -> Self {
        let mut path = vec![
            "providers".to_owned(),
            "connections".to_owned(),
            name.to_string(),
        ];
        if !leaf.is_empty() {
            path.extend(leaf.split('.').map(str::to_owned));
        }
        Self::new(path, message)
    }
}

impl ProviderRegistry {
    /// Validate Rust-constructed candidates too; serde is not the sole gate.
    pub fn validate(&self) -> Result<(), RegistryError> {
        for (name, connection) in &self.connections {
            if name.is_empty() {
                return Err(RegistryError::connection(
                    name,
                    "",
                    "connection name must not be empty",
                ));
            }
            for &capability in connection.capabilities() {
                if !connection.supports(capability) {
                    return Err(RegistryError::connection(
                        name,
                        "capabilities",
                        "capability is not implemented by this connection kind",
                    ));
                }
            }
            for capability in connection.configured_apis() {
                if !connection.capabilities().contains(&capability) {
                    return Err(RegistryError::connection(
                        name,
                        "api",
                        "configured API must be explicitly enabled in capabilities",
                    ));
                }
            }
            self.validate_connection(name, connection)?;
        }
        for (name, preset) in &self.model_presets {
            if name.is_empty() {
                return Err(RegistryError::new(
                    ["providers", "model_presets", name.as_str()],
                    "preset name must not be empty",
                ));
            }
            super::resolve::validate_preset(name, preset)?;
        }
        for (name, model) in &self.models {
            if name.is_empty() {
                return Err(RegistryError::model(
                    name,
                    "",
                    "model name must not be empty",
                ));
            }
            self.selection(name, model.capability())?;
            self.validate_model(name, model)?;
        }
        self.validate_fallback()
    }

    /// Exact lookup, shared by every capability's consumer.
    pub fn selection(
        &self,
        name: &ModelRef,
        capability: Capability,
    ) -> Result<ModelSelection<'_>, RegistryError> {
        let (name, model) = self
            .models
            .get_key_value(name)
            .ok_or_else(|| RegistryError::model(name, "", "model does not exist"))?;
        if model.capability() != capability {
            return Err(RegistryError::model(
                name,
                "capability",
                "model capability does not match the consumer",
            ));
        }
        let (provider_id, connection) = self
            .connections
            .get_key_value(model.provider())
            .ok_or_else(|| RegistryError::model(name, "provider", "connection does not exist"))?;
        if !connection.supports(capability) || !connection.capabilities().contains(&capability) {
            return Err(RegistryError::model(
                name,
                "provider",
                "connection does not enable this capability",
            ));
        }
        Ok(ModelSelection {
            name,
            model,
            provider_id,
            connection,
        })
    }

    fn validate_fallback(&self) -> Result<(), RegistryError> {
        for (source, targets) in &self.fallback {
            self.selection(source, Capability::Chat)?;
            let mut seen = BTreeSet::new();
            for target in targets {
                self.selection(target, Capability::Chat)?;
                if !seen.insert(target) {
                    return Err(RegistryError::new(
                        ["providers", "fallback", source.as_str()],
                        "fallback contains a duplicate target",
                    ));
                }
            }
        }
        // Iterative DFS avoids recursion proportional to untrusted input.
        let mut complete = BTreeSet::new();
        for source in self.fallback.keys() {
            let mut active = BTreeSet::new();
            let mut stack = vec![(source, false)];
            while let Some((node, exiting)) = stack.pop() {
                if exiting {
                    active.remove(node);
                    complete.insert(node);
                    continue;
                }
                if complete.contains(node) {
                    continue;
                }
                if !active.insert(node) {
                    return Err(RegistryError::new(
                        ["providers", "fallback", source.as_str()],
                        "fallback graph contains a cycle",
                    ));
                }
                stack.push((node, true));
                if let Some(targets) = self.fallback.get(node) {
                    stack.extend(targets.iter().rev().map(|target| (target, false)));
                }
            }
        }
        Ok(())
    }
}

/// A borrowed view of one candidate generation. It is never stored in Config.
#[derive(Debug, Clone, Copy)]
pub struct ModelSelection<'a> {
    pub name: &'a ModelRef,
    pub model: &'a ModelDefinition,
    pub provider_id: &'a ProviderId,
    pub connection: &'a ProviderConnection,
}
