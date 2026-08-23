//! Typed host services injected into every plugin store.
//!
//! The bundle contains service handles, not materialized values. Each lookup
//! resolves from canonical host state under the store's host-issued instance
//! scope, keeping identity and authorization out of guest-controlled inputs.

use crate::config::{PluginConfigResolver, ResolvedPluginConfig};
use crate::error::PluginError;
use crate::instance::PluginInstanceScope;

/// Required host-service bundle for one plugin store.
///
/// This is deliberately a typed struct rather than a string-keyed or `Any`
/// registry. Adding a service is an explicit API change, and a store cannot be
/// constructed while a required service is missing.
#[derive(Clone)]
pub struct PluginHostServices {
    config: PluginConfigResolver,
}

/// Detail-free internal result converted to each WIT world's generated enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretLookupError {
    AccessDenied,
    NotFound,
    Unavailable,
}

impl PluginHostServices {
    /// Build the complete required host-service bundle.
    #[must_use]
    pub fn new(config: PluginConfigResolver) -> Self {
        Self { config }
    }

    /// Resolve a typed, validated config view from canonical host state.
    ///
    /// # Errors
    ///
    /// Returns an error when canonical config cannot be read, does not satisfy
    /// the manifest schema, or was issued for a different instance scope.
    pub fn resolve_config(
        &self,
        scope: &PluginInstanceScope,
    ) -> Result<ResolvedPluginConfig, PluginError> {
        self.config.resolve(scope)
    }
}

/// Empty config service for store-plumbing tests. Production callers must
/// inject their canonical resolver explicitly.
#[cfg(test)]
pub(crate) fn test_host_services() -> PluginHostServices {
    use crate::{PluginManifest, PluginPermission, config::resolve_plugin_config};

    PluginHostServices::new(PluginConfigResolver::new(|scope| {
        let manifest = PluginManifest {
            name: scope.id().package().to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            capabilities: vec![scope.id().capability()],
            permissions: vec![PluginPermission::ConfigRead],
            config_schema: Some(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            signature: None,
            publisher_key: None,
        };
        resolve_plugin_config(&manifest, scope, None)
    }))
}
