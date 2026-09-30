//! Authenticated, read-only plugin package catalog API.
//!
//! The catalog is materialized for each request from canonical config, the
//! host-admitted installed manifests, and the cached registry index. The
//! gateway does not retain a second catalog or plugin lifecycle state; the
//! runtime builds the body, and serves `plugins/list` from the same function.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use zeroclaw_runtime::rpc::catalog::PluginCatalogUnavailable;
pub use zeroclaw_runtime::rpc::catalog::{
    AvailablePluginPackage, InstalledPluginPackage, PluginCatalogEntry, PluginCatalogIssue,
    PluginCatalogIssueCode, PluginCatalogIssueSource, PluginsResponse,
};

use super::AppState;

/// `GET /api/plugins` — return the package catalog without mutating config,
/// registry state, or the plugin directory.
pub async fn list_plugins(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|authorization| authorization.strip_prefix("Bearer "))
            .unwrap_or_default();
        if !state.pairing.is_authenticated(token) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    let config = state.config.read().clone();
    match zeroclaw_runtime::rpc::catalog::plugins_body(&config).await {
        Ok(response) => Json(response).into_response(),
        Err(PluginCatalogUnavailable::Busy) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(PluginCatalogUnavailable::Failed) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
