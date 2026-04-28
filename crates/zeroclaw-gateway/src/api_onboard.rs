//! Onboard catalog endpoint — exposes the provider + model catalog the CLI
//! wizard already uses, so the dashboard's "+ Add provider" affordance and
//! model-picker dropdown share the same source of truth as the CLI.
//!
//! No catalog data is hand-maintained at this layer. `list_providers()` lives
//! in `zeroclaw-providers` and is the canonical list; `list_models()` per
//! provider fetches from models.dev (cached) or the provider's own /models
//! endpoint. Same code paths as the CLI wizard.
//!
//! Issue #6175.

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

use super::AppState;
use super::api::require_auth;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CatalogProvider {
    /// Canonical provider name as used in `[providers.models.<name>]`.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Whether the provider is fully local (no API key required).
    pub local: bool,
    /// Aliases the provider also responds to (informational).
    pub aliases: Vec<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CatalogResponse {
    pub providers: Vec<CatalogProvider>,
}

/// `GET /api/onboard/catalog` — list every provider the CLI wizard knows
/// about. The dashboard shows these in the "+ Add provider" picker so
/// CLI / web stay in sync.
pub async fn handle_catalog(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let _ = state;

    let providers: Vec<CatalogProvider> = zeroclaw_providers::list_providers()
        .into_iter()
        .map(|p| CatalogProvider {
            name: p.name.to_string(),
            display_name: p.display_name.to_string(),
            local: p.local,
            aliases: p.aliases.iter().map(|s| s.to_string()).collect(),
        })
        .collect();

    axum::Json(CatalogResponse { providers }).into_response()
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelsQuery {
    /// Provider name (canonical, from CatalogProvider.name).
    pub provider: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelsResponse {
    pub provider: String,
    pub models: Vec<String>,
    /// `true` when the catalog was fetched live; `false` if the cache was
    /// served (or if this provider has no remote catalog and the empty list
    /// is the genuine answer).
    pub live: bool,
}

/// `GET /api/onboard/catalog/models?provider=<name>` — fetch the model list
/// for one provider. Same code path the CLI wizard uses
/// (`zeroclaw_providers::create_provider(...).list_models()`), which goes
/// through the models.dev cached catalog for OpenAI / Anthropic / Gemini,
/// the live `/v1/models` endpoint for OpenRouter, etc.
///
/// Lazy: the dashboard hits this only when the user picks a provider, so
/// initial catalog load stays fast. Fetch failures return an empty list
/// with `live: false` so the form falls back to a free-text input.
pub async fn handle_catalog_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ModelsQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let _ = state;

    let handle = match zeroclaw_providers::create_provider(&q.provider, None) {
        Ok(h) => h,
        Err(e) => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::PathNotFound,
                    format!("unknown provider `{}`: {e}", q.provider),
                )
                .with_path(&q.provider),
            );
        }
    };

    let (models, live) = match handle.list_models().await {
        Ok(m) => (m, true),
        Err(e) => {
            tracing::debug!(provider = %q.provider, error = ?e, "model catalog fetch failed");
            (Vec::new(), false)
        }
    };

    axum::Json(ModelsResponse {
        provider: q.provider,
        models,
        live,
    })
    .into_response()
}

fn error_response(err: ConfigApiError) -> Response {
    let status = axum::http::StatusCode::from_u16(err.code.http_status())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    (status, axum::Json(err)).into_response()
}
