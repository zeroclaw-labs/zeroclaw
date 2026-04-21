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

        let config = state.config.lock();
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
