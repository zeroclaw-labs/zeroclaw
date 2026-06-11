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

        let (plugins_enabled, configured_plugins_dir, resolved_plugins_dir) = {
            let config = state.config.read();
            (
                config.plugins.enabled,
                config.plugins.plugins_dir.clone(),
                config.resolved_plugins_discovery_dir(),
            )
        };

        let plugins: Vec<serde_json::Value> = if plugins_enabled {
            list_plugins_from_dir(&resolved_plugins_dir)
        } else {
            vec![]
        };

        Json(serde_json::json!({
            "plugins_enabled": plugins_enabled,
            "plugins_dir": configured_plugins_dir,
            "plugins": plugins,
        }))
        .into_response()
    }

    pub(crate) fn list_plugins_from_dir(plugins_dir: &std::path::Path) -> Vec<serde_json::Value> {
        if !plugins_dir.exists() {
            return vec![];
        }

        match zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins_dir) {
            Ok(host) => host.list_plugins().into_iter().map(plugin_json).collect(),
            Err(_) => vec![],
        }
    }

    fn plugin_json(p: zeroclaw_plugins::PluginInfo) -> serde_json::Value {
        serde_json::json!({
            "name": p.name,
            "version": p.version,
            "description": p.description,
            "capabilities": p.capabilities,
            "loaded": p.loaded,
        })
    }

    #[cfg(test)]
    pub(crate) fn resolved_plugins_discovery_dir_for_gateway(
        config: &zeroclaw_config::schema::Config,
    ) -> std::path::PathBuf {
        config.resolved_plugins_discovery_dir()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tempfile::TempDir;
        use zeroclaw_config::schema::{Config, PluginsConfig};

        fn write_tool_plugin(plugins_dir: &std::path::Path, name: &str) {
            let plugin_dir = plugins_dir.join(name);
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(
                plugin_dir.join("manifest.toml"),
                format!(
                    "name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"tool\"]\nwasm_path = \"plugin.wasm\"\n"
                ),
            )
            .unwrap();
        }

        #[test]
        fn gateway_plugin_listing_uses_exact_resolved_plugins_dir() {
            let tmp = TempDir::new().unwrap();
            let configured = tmp.path().join("configured-plugins");
            write_tool_plugin(&configured, "gateway-plugin");

            let plugins = list_plugins_from_dir(&configured);

            assert_eq!(plugins.len(), 1);
            assert_eq!(plugins[0]["name"], "gateway-plugin");
        }

        #[test]
        fn gateway_plugin_path_resolution_uses_config_helper_with_legacy_fallback() {
            let tmp = TempDir::new().unwrap();
            let config_dir = tmp.path().join("config");
            let workspace = config_dir.join("workspace");
            write_tool_plugin(&workspace.join("plugins"), "legacy-plugin");

            let plugins = PluginsConfig {
                enabled: true,
                plugins_dir: config_dir.join("plugins").to_string_lossy().into_owned(),
                ..PluginsConfig::default()
            };

            let config = Config {
                config_path: config_dir.join("config.toml"),
                plugins,
                ..Config::default()
            };

            assert_eq!(
                resolved_plugins_discovery_dir_for_gateway(&config),
                workspace.join("plugins")
            );
        }
    }
}
