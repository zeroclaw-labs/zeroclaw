//! Canonical embedding bindings and diagnostics. Historical input is normalized
//! in the configuration migration layer before these consumers run.
use super::{ModelRef, ProviderRegistry};
use crate::schema::Config;
use anyhow::Result;

impl Config {
    /// Report invalid embedding bindings using the same canonical resolver as
    /// provider construction. Historical provider syntax is never interpreted.
    pub fn embedding_input_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let registry = self.provider_registry().ok();
        for route in &self.embedding_routes {
            if route.hint.trim().is_empty() {
                warnings.push("embedding route with empty hint".into());
            }
            if route.model.trim().is_empty() {
                warnings.push(format!(
                    "embedding route \"{}\" has empty model",
                    route.hint
                ));
            }
            let invalid = registry.as_ref().is_none_or(|registry| {
                registry
                    .resolve_embedding(&route.model_provider.trim().into())
                    .is_err()
            });
            if invalid {
                warnings.push(format!(
                    "embedding route \"{}\" uses invalid model_provider \"{}\"",
                    route.hint, route.model_provider
                ));
            }
        }
        if let Some(hint) = self
            .memory
            .embedding_model
            .strip_prefix("hint:")
            .map(str::trim)
            .filter(|hint| !hint.is_empty())
            && !self
                .embedding_routes
                .iter()
                .any(|route| route.hint.trim() == hint)
        {
            warnings.push(format!(
                "embedding model hint \"{hint}\" has no matching [[embedding_routes]] entry exists"
            ));
        }
        warnings
    }

    /// Resolve an admitted exact embedding model. This never fabricates a model
    /// or interprets retired provider/profile fields.
    pub fn embedding_registry(&self) -> Result<(ProviderRegistry, Option<ModelRef>)> {
        let registry = self.provider_registry()?;
        let reference = self
            .memory
            .embedding_model_ref
            .as_deref()
            .map(str::trim)
            .filter(|reference| !reference.is_empty())
            .map(ModelRef::from);
        if let Some(reference) = &reference {
            registry.resolve_embedding(reference)?;
        }
        Ok((registry, reference))
    }
}
