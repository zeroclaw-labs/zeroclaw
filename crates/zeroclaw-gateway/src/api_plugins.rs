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

        let config = state.config.read();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        let plugin_path = config.plugins.resolved_plugins_dir();
        let signature_mode_raw = config.plugins.security.signature_mode.clone();
        let trusted_publisher_keys = config.plugins.security.trusted_publisher_keys.clone();
        drop(config);

        let plugins: Vec<serde_json::Value> = if plugins_enabled {
            if plugin_path.exists() {
                // Resolve the configured policy so the listing matches the
                // runtime tool path exactly: a plugin the agent refuses to load
                // in strict mode must not appear here as `loaded: true`. An
                // unrecognized value fails safe to strict, same as the runtime.
                let signature_mode =
                    zeroclaw_plugins::host::PluginHost::resolve_signature_mode(&signature_mode_raw);
                match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
                    &plugin_path,
                    signature_mode,
                    trusted_publisher_keys,
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
