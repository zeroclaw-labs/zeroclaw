//! Canonical embedding bindings and diagnostics. Historical input is normalized
//! in the configuration migration layer before these consumers run.
use super::{ModelRef, ProviderRegistry};
use crate::schema::Config;
use anyhow::Result;

/// Structured diagnostics; presentation belongs to the calling UI or CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingInputWarning {
    EmptyRouteHint,
    EmptyRouteModel { hint: String },
    InvalidRouteModel { hint: String, model_ref: String },
    UnmatchedHint { hint: String },
}

impl Config {
    /// Report invalid embedding bindings using the same canonical resolver as
    /// provider construction. Historical provider syntax is never interpreted.
    pub fn embedding_input_warnings(&self) -> Vec<EmbeddingInputWarning> {
        let mut warnings = Vec::new();
        let registry = self.provider_registry().ok();
        for route in &self.embedding_routes {
            if route.hint.trim().is_empty() {
                warnings.push(EmbeddingInputWarning::EmptyRouteHint);
            }
            let resolved = registry.as_ref().and_then(|registry| {
                registry
                    .resolve_embedding(&route.model_provider.trim().into())
                    .ok()
            });
            match resolved {
                Some(resolved) if resolved.model.trim().is_empty() => {
                    warnings.push(EmbeddingInputWarning::EmptyRouteModel {
                        hint: route.hint.clone(),
                    })
                }
                Some(_) => {}
                None => {
                    // A pre-admission legacy fixture can still carry the
                    // route-owned model field. Keep its actionable warning,
                    // while canonical routes above derive the model solely
                    // from the resolved model reference.
                    if route.model.trim().is_empty() {
                        warnings.push(EmbeddingInputWarning::EmptyRouteModel {
                            hint: route.hint.clone(),
                        });
                    }
                    warnings.push(EmbeddingInputWarning::InvalidRouteModel {
                        hint: route.hint.clone(),
                        model_ref: route.model_provider.clone(),
                    });
                }
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
            warnings.push(EmbeddingInputWarning::UnmatchedHint { hint: hint.into() });
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
