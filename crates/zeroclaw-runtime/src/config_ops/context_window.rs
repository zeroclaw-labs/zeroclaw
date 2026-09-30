//! Context-window refresh shared by the gateway's
//! `POST /api/config/providers/{type}/{alias}/refresh-context-window` route
//! and the RPC `providers/refresh-context-window` method.
//!
//! The refresh runs in three steps so no lock is held across the network
//! fetch: [`fetch_request`] reads what the fetch needs from a snapshot,
//! [`fetch`] asks the provider, and [`apply`] writes the result onto a
//! working copy the caller commits under its config write lock.

use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::schema::{Config, ModelProviderConfig};

/// Result of a context-window refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RefreshContextWindowResponse {
    /// The provider profile path, e.g. `providers.models.openai.default`.
    pub path: String,
    pub context_window: usize,
}

/// The provider profile path for `<provider_type>.<alias>`.
pub fn profile_path(provider_type: &str, alias: &str) -> String {
    format!("providers.models.{provider_type}.{alias}")
}

/// The one path a refresh writes. Callers that authorize per path check it.
pub fn target_path(provider_type: &str, alias: &str) -> String {
    format!("{}.context_window", profile_path(provider_type, alias))
}

fn not_found(provider_type: &str, alias: &str) -> ConfigApiError {
    ConfigApiError::new(
        ConfigApiCode::PathNotFound,
        format!("model provider '{provider_type}.{alias}' not found"),
    )
    .with_path(profile_path(provider_type, alias))
}

/// Build the minimal provider config the fetch needs. The api key is read
/// unmasked through serialization; it goes only to the provider fetch.
pub fn fetch_request(
    snapshot: &Config,
    provider_type: &str,
    alias: &str,
) -> Result<ModelProviderConfig, ConfigApiError> {
    let path = profile_path(provider_type, alias);
    let Ok(model) = snapshot.get_prop(&format!("{path}.model")) else {
        return Err(not_found(provider_type, alias));
    };
    let uri = snapshot.get_prop(&format!("{path}.uri")).ok();
    // Read api_key via JSON serialization to bypass #[secret] masking in get_prop.
    let api_key = serde_json::to_value(&snapshot.providers)
        .ok()
        .and_then(|v| {
            v.pointer(&format!("/models/{provider_type}/{alias}/api_key"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && *s != "<unset>")
                .map(String::from)
        });
    Ok(ModelProviderConfig {
        model: Some(model),
        uri,
        api_key,
        ..Default::default()
    })
}

/// Ask the provider for its context window. Holds no lock.
pub async fn fetch(
    provider_type: &str,
    alias: &str,
    request: &ModelProviderConfig,
) -> Result<usize, ConfigApiError> {
    zeroclaw_providers::fetch_context_window(provider_type, request)
        .await
        .ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::InvalidFormat,
                format!("provider '{provider_type}' does not support context window auto-detection or fetch failed"),
            )
            .with_path(profile_path(provider_type, alias))
        })
}

/// Write the fetched value onto `working` and mark it dirty. Re-verifies the
/// profile still exists: a concurrent writer could have removed it while the
/// unlocked fetch was in flight.
pub fn apply(
    working: &mut Config,
    provider_type: &str,
    alias: &str,
    context_window: usize,
) -> Result<RefreshContextWindowResponse, ConfigApiError> {
    let path = profile_path(provider_type, alias);
    if working.get_prop(&format!("{path}.model")).is_err() {
        return Err(not_found(provider_type, alias));
    }
    let target = target_path(provider_type, alias);
    working
        .set_prop_persistent(&target, &context_window.to_string())
        .map_err(|e| {
            ConfigApiError::new(
                ConfigApiCode::InternalError,
                format!("failed to persist context_window: {e}"),
            )
            .with_path(&path)
        })?;
    working.mark_dirty(&target);
    Ok(RefreshContextWindowResponse {
        path,
        context_window,
    })
}
