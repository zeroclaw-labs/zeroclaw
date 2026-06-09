//! Plugin management API routes (requires `plugins-wasm` feature).

#[cfg(feature = "plugins-wasm")]
pub mod plugin_routes {
    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode, header},
        response::{IntoResponse, Json},
    };
    use serde::Deserialize;

    use super::super::AppState;

    /// Shared bearer-token check, mirroring `list_plugins`. Returns `Err` with a
    /// ready `401` response when pairing is required and the token is invalid.
    fn require_auth(state: &AppState, headers: &HeaderMap) -> Result<(), axum::response::Response> {
        if state.pairing.require_pairing() {
            let token = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|auth| auth.strip_prefix("Bearer "))
                .unwrap_or("");
            if !state.pairing.is_authenticated(token) {
                return Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response());
            }
        }
        Ok(())
    }

    #[derive(Deserialize)]
    pub struct InstallRequest {
        source: String,
    }

    #[derive(Deserialize)]
    pub struct EnabledRequest {
        enabled: bool,
    }

    /// `POST /api/plugins/install` — STUB. Validates the request and echoes back a
    /// `stub: true` response. Real installation (download/verify/copy WASM into
    /// `plugins_dir` via `PluginHost`) is not yet wired; the `stub` flag tells the
    /// dashboard not to claim a plugin was actually installed.
    pub async fn install_plugin(
        State(state): State<AppState>,
        headers: HeaderMap,
        Json(req): Json<InstallRequest>,
    ) -> impl IntoResponse {
        if let Err(resp) = require_auth(&state, &headers) {
            return resp;
        }
        let source = req.source.trim().to_string();
        if source.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "stub": true,
                    "message": "A plugin source (path, registry name, or git URL) is required.",
                })),
            )
                .into_response();
        }
        Json(serde_json::json!({
            "ok": true,
            "stub": true,
            "source": source,
            "message": format!(
                "Stub: install of '{source}' was accepted but is not yet wired to PluginHost. No plugin was actually installed."
            ),
        }))
        .into_response()
    }

    /// `DELETE /api/plugins/{name}` — STUB. Validates the name and reports a
    /// `stub: true` response without removing anything from disk.
    pub async fn remove_plugin(
        State(state): State<AppState>,
        headers: HeaderMap,
        Path(name): Path<String>,
    ) -> impl IntoResponse {
        if let Err(resp) = require_auth(&state, &headers) {
            return resp;
        }
        Json(serde_json::json!({
            "ok": true,
            "stub": true,
            "name": name,
            "message": format!(
                "Stub: removal of '{name}' was accepted but is not yet wired to PluginHost. No plugin was actually removed."
            ),
        }))
        .into_response()
    }

    /// `POST /api/plugins/enabled` — STUB. Reports the requested `[plugins].enabled`
    /// value back without persisting it to config yet.
    pub async fn set_plugins_enabled(
        State(state): State<AppState>,
        headers: HeaderMap,
        Json(req): Json<EnabledRequest>,
    ) -> impl IntoResponse {
        if let Err(resp) = require_auth(&state, &headers) {
            return resp;
        }
        Json(serde_json::json!({
            "ok": true,
            "stub": true,
            "enabled": req.enabled,
            "message": format!(
                "Stub: setting [plugins].enabled = {} was accepted but is not yet persisted to config.",
                req.enabled
            ),
        }))
        .into_response()
    }

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

        let config = state.config.read();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        drop(config);

        let plugins: Vec<serde_json::Value> = if plugins_enabled {
            let plugin_path = if plugins_dir.starts_with("~/") {
                directories::UserDirs::new()
                    .map(|u| u.home_dir().join(&plugins_dir[2..]))
                    .unwrap_or_else(|| std::path::PathBuf::from(&plugins_dir))
            } else {
                std::path::PathBuf::from(&plugins_dir)
            };

            if plugin_path.exists() {
                match zeroclaw_plugins::host::PluginHost::new(
                    plugin_path.parent().unwrap_or(&plugin_path),
                ) {
                    Ok(host) => host
                        .list_plugins()
                        .into_iter()
                        .map(|p| {
                            serde_json::json!({
                                "name": p.name,
                                "version": p.version,
                                "description": p.description,
                                "capabilities": p.capabilities,
                                "loaded": p.loaded,
                            })
                        })
                        .collect(),
                    Err(_) => vec![],
                }
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        Json(serde_json::json!({
            "plugins_enabled": plugins_enabled,
            "plugins_dir": plugins_dir,
            "plugins": plugins,
        }))
        .into_response()
    }
}
