//! Plugin management API routes (requires `plugins-wasm` feature).

#[cfg(feature = "plugins-wasm")]
pub mod plugin_routes {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode, header},
        response::{IntoResponse, Json},
    };

    use super::super::AppState;

    /// `GET /api/plugins` — list loaded plugins and their status.
    pub async fn list_plugins(
        State(state): State<AppState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        // Auth check
        if state.pairing.require_pairing() {
            let token = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|auth| auth.strip_prefix("Bearer "))
                .unwrap_or("");
            if !state.pairing.is_authenticated(token) {
                return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
            }
        }

        let config = state.config.read().clone();
        Json(zeroclaw_runtime::rpc::catalog::plugins_body(&config)).into_response()
    }
}
