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
        let registry = match self.provider_registry() {
            Ok(registry) => registry,
            Err(error) => return vec![error.to_string()],
        };
        for route in &self.embedding_routes {
            if route.hint.trim().is_empty() {
                warnings.push("embedding route with empty hint".into());
            }
            if let Err(error) = registry.resolve_embedding(&route.model_provider.trim().into()) {
                warnings.push(format!("embedding route \"{}\": {error}", route.hint));
            }
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
