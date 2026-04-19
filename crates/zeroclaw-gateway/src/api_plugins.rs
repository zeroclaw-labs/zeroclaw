//! Plugin management API routes (requires `plugins-wasm` feature).

#[cfg(feature = "plugins-wasm")]
pub mod plugin_routes {
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode, header},
        response::{IntoResponse, Json},
    };

    use super::super::AppState;

    /// Resolve the plugins directory from config, expanding `~/` if needed.
    fn resolve_plugins_path(plugins_dir: &str) -> std::path::PathBuf {
        if plugins_dir.starts_with("~/") {
            directories::UserDirs::new()
                .map(|u| u.home_dir().join(&plugins_dir[2..]))
                .unwrap_or_else(|| std::path::PathBuf::from(plugins_dir))
        } else {
            std::path::PathBuf::from(plugins_dir)
        }
    }

    /// Authenticate request, returning an error response if auth fails.
    fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
        if state.pairing.require_pairing() {
            let token = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|auth| auth.strip_prefix("Bearer "))
                .unwrap_or("");
            if !state.pairing.is_authenticated(token) {
                return Err((StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
        }
        Ok(())
    }

    /// `GET /api/plugins` — list loaded plugins and their status.
    pub async fn list_plugins(
        State(state): State<AppState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let config = state.config.lock();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        drop(config);

        let plugins: Vec<serde_json::Value> = if plugins_enabled {
            let plugin_path = resolve_plugins_path(&plugins_dir);

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
                                "status": if p.loaded { "loaded" } else { "discovered" },
                                "tools": p.tools,
                                "capabilities": p.capabilities,
                                "allowed_hosts": p.allowed_hosts,
                                "allowed_paths": p.allowed_paths,
                                "config": p.config,
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

    /// Helper to create a PluginHost from the current config state.
    fn create_plugin_host(
        plugins_dir: &str,
    ) -> Result<crate::plugins::host::PluginHost, (StatusCode, &'static str)> {
        let plugin_path = resolve_plugins_path(plugins_dir);
        if !plugin_path.exists() {
            return Err((StatusCode::NOT_FOUND, "Plugin not found"));
        }
        crate::plugins::host::PluginHost::new(plugin_path.parent().unwrap_or(&plugin_path))
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load plugins"))
    }

    /// `POST /api/plugins/:name/enable` — enable a disabled plugin.
    pub async fn enable_plugin(
        State(state): State<AppState>,
        headers: HeaderMap,
        axum::extract::Path(name): axum::extract::Path<String>,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        // Collect everything we need before the .await so no !Send types
        // (parking_lot MutexGuard) are held across the suspend point.
        let (response_json, config_snapshot) = {
            let plugins_enabled;
            let plugins_dir;
            {
                let config = state.config.lock();
                plugins_enabled = config.plugins.enabled;
                plugins_dir = config.plugins.plugins_dir.clone();
            }

            if !plugins_enabled {
                return (StatusCode::NOT_FOUND, "Plugins not enabled").into_response();
            }

            let mut host = match create_plugin_host(&plugins_dir) {
                Ok(h) => h,
                Err(e) => return e.into_response(),
            };

            if host.get_plugin(&name).is_none() {
                return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
            }

            if let Err(_) = host.enable_plugin(&name) {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to enable plugin")
                    .into_response();
            }

            // Persist: remove from disabled_plugins list in config
            {
                let mut config = state.config.lock();
                config.plugins.disabled_plugins.retain(|p| p != &name);
            }

            let info = host.get_plugin(&name).expect("plugin was just enabled");
            let json = Json(serde_json::json!({
                "name": info.name,
                "version": info.version,
                "description": info.description,
                "status": if info.loaded { "loaded" } else { "discovered" },
                "enabled": info.enabled,
                "capabilities": info.capabilities,
                "tools": info.tools,
            }));

            let snapshot = state.config.lock().clone();
            (json, snapshot)
        };

        // Now safe to .await — no MutexGuard or PluginHost in scope.
        if let Err(e) = config_snapshot.save().await {
            tracing::error!(error = %e, "failed to persist plugin enable to config.toml");
        }

        response_json.into_response()
    }

    /// `POST /api/plugins/:name/disable` — disable an enabled plugin.
    pub async fn disable_plugin(
        State(state): State<AppState>,
        headers: HeaderMap,
        axum::extract::Path(name): axum::extract::Path<String>,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let (response_json, config_snapshot) = {
            let plugins_enabled;
            let plugins_dir;
            {
                let config = state.config.lock();
                plugins_enabled = config.plugins.enabled;
                plugins_dir = config.plugins.plugins_dir.clone();
            }

            if !plugins_enabled {
                return (StatusCode::NOT_FOUND, "Plugins not enabled").into_response();
            }

            let mut host = match create_plugin_host(&plugins_dir) {
                Ok(h) => h,
                Err(e) => return e.into_response(),
            };

            if host.get_plugin(&name).is_none() {
                return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
            }

            if let Err(_) = host.disable_plugin(&name) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to disable plugin",
                )
                    .into_response();
            }

            // Persist: add to disabled_plugins list in config
            {
                let mut config = state.config.lock();
                if !config.plugins.disabled_plugins.contains(&name) {
                    config.plugins.disabled_plugins.push(name.clone());
                }
            }

            let info = host.get_plugin(&name).expect("plugin was just disabled");
            let json = Json(serde_json::json!({
                "name": info.name,
                "version": info.version,
                "description": info.description,
                "status": if info.loaded { "loaded" } else { "discovered" },
                "enabled": info.enabled,
                "capabilities": info.capabilities,
                "tools": info.tools,
            }));

            let snapshot = state.config.lock().clone();
            (json, snapshot)
        };

        if let Err(e) = config_snapshot.save().await {
            tracing::error!(error = %e, "failed to persist plugin disable to config.toml");
        }

        response_json.into_response()
    }

    /// `PATCH /api/plugins/:name/config` — update non-sensitive config values.
    pub async fn patch_plugin_config(
        State(state): State<AppState>,
        headers: HeaderMap,
        axum::extract::Path(name): axum::extract::Path<String>,
        Json(body): Json<std::collections::HashMap<String, String>>,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let config_snapshot = {
            let plugins_enabled;
            let plugins_dir;
            {
                let config = state.config.lock();
                plugins_enabled = config.plugins.enabled;
                plugins_dir = config.plugins.plugins_dir.clone();
            }

            if !plugins_enabled {
                return (StatusCode::NOT_FOUND, "Plugins not enabled").into_response();
            }

            let host = match create_plugin_host(&plugins_dir) {
                Ok(h) => h,
                Err(e) => return e.into_response(),
            };

            let info = match host.get_plugin(&name) {
                Some(i) => i,
                None => return (StatusCode::NOT_FOUND, "Plugin not found").into_response(),
            };

            // Reject writes to sensitive keys
            for key in body.keys() {
                if let Some(decl) = info.config.get(key) {
                    if let Some(obj) = decl.as_object() {
                        if obj
                            .get("sensitive")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        {
                            return (
                                StatusCode::BAD_REQUEST,
                                "Cannot edit sensitive config keys via API",
                            )
                                .into_response();
                        }
                    }
                }
            }

            // Update per-plugin config in the main config
            {
                let mut config = state.config.lock();
                let plugin_cfg = config.plugins.per_plugin.entry(name.clone()).or_default();
                for (k, v) in &body {
                    plugin_cfg.insert(k.clone(), v.clone());
                }
            }

            let snapshot = state.config.lock().clone();
            snapshot
        };

        // Now safe to .await — no MutexGuard in scope.
        if let Err(e) = config_snapshot.save().await {
            tracing::error!(error = %e, "failed to persist plugin config to config.toml");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save config").into_response();
        }

        Json(serde_json::json!({ "status": "ok" })).into_response()
    }

    /// `POST /api/plugins/reload` — reload all plugins from disk.
    ///
    /// Re-scans the plugins directory and returns a summary of what changed.
    pub async fn reload_plugins(
        State(state): State<AppState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let config = state.config.lock();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        drop(config);

        if !plugins_enabled {
            return (StatusCode::BAD_REQUEST, "Plugins not enabled").into_response();
        }

        let mut host = match create_plugin_host(&plugins_dir) {
            Ok(h) => h,
            Err(e) => return e.into_response(),
        };

        match host.reload() {
            Ok(summary) => {
                tracing::info!(
                    total = summary.total,
                    loaded = ?summary.loaded,
                    unloaded = ?summary.unloaded,
                    failed = ?summary.failed,
                    "Plugins reloaded via API"
                );
                Json(serde_json::json!({
                    "ok": true,
                    "total": summary.total,
                    "loaded": summary.loaded,
                    "unloaded": summary.unloaded,
                    "failed": summary.failed,
                }))
                .into_response()
            }
            Err(e) => {
                tracing::error!(error = %e, "Plugin reload failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "ok": false,
                        "error": e.to_string(),
                    })),
                )
                    .into_response()
            }
        }
    }

    /// `GET /api/plugins/:name` — full plugin details including manifest and config status.
    pub async fn get_plugin_detail(
        State(state): State<AppState>,
        headers: HeaderMap,
        axum::extract::Path(name): axum::extract::Path<String>,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let config = state.config.lock();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        drop(config);

        if !plugins_enabled {
            return (StatusCode::NOT_FOUND, "Plugins not enabled").into_response();
        }

        let plugin_path = resolve_plugins_path(&plugins_dir);
        if !plugin_path.exists() {
            return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
        }

        let host = match crate::plugins::host::PluginHost::new(
            plugin_path.parent().unwrap_or(&plugin_path),
        ) {
            Ok(h) => h,
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load plugins")
                    .into_response();
            }
        };

        match host.get_plugin(&name) {
            Some(info) => Json(serde_json::json!({
                "name": info.name,
                "version": info.version,
                "description": info.description,
                "status": if info.loaded { "loaded" } else { "discovered" },
                "capabilities": info.capabilities,
                "permissions": info.permissions,
                "tools": info.tools,
                "wasm_path": info.wasm_path,
                "wasm_sha256": info.wasm_sha256,
                "config_status": if info.loaded { "ok" } else { "not_loaded" },
                "allowed_hosts": info.allowed_hosts,
                "allowed_paths": info.allowed_paths,
                "config": info.config,
            }))
            .into_response(),
            None => (StatusCode::NOT_FOUND, "Plugin not found").into_response(),
        }
    }

    /// Request body for plugin installation.
    #[derive(serde::Deserialize)]
    pub struct InstallPluginRequest {
        /// Source path or URL for the plugin
        pub source: String,
    }

    /// `POST /api/plugins/install` — install a plugin from a directory or URL.
    ///
    /// Uses the same `PluginHost::install` logic as the CLI command:
    /// `zeroclaw plugin install <source>`
    pub async fn install_plugin(
        State(state): State<AppState>,
        headers: HeaderMap,
        Json(body): Json<InstallPluginRequest>,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let config = state.config.lock();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        drop(config);

        if !plugins_enabled {
            return (StatusCode::BAD_REQUEST, "Plugins not enabled").into_response();
        }

        // Use the same PluginHost::install method as CLI
        let mut host = match create_plugin_host(&plugins_dir) {
            Ok(h) => h,
            Err(e) => return e.into_response(),
        };

        match host.install(&body.source) {
            Ok(()) => {
                tracing::info!(source = %body.source, "Plugin installed via API");
                Json(serde_json::json!({
                    "ok": true,
                    "message": format!("Plugin installed from {}", body.source),
                }))
                .into_response()
            }
            Err(e) => {
                tracing::error!(source = %body.source, error = %e, "Plugin install failed");
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "ok": false,
                        "error": e.to_string(),
                    })),
                )
                    .into_response()
            }
        }
    }

    /// `DELETE /api/plugins/:name` — remove a plugin by name.
    ///
    /// Uses the same `PluginHost::remove` logic as the CLI command:
    /// `zeroclaw plugin remove <name>`
    pub async fn remove_plugin(
        State(state): State<AppState>,
        headers: HeaderMap,
        axum::extract::Path(name): axum::extract::Path<String>,
    ) -> impl IntoResponse {
        if let Err(e) = check_auth(&state, &headers) {
            return e.into_response();
        }

        let config = state.config.lock();
        let plugins_enabled = config.plugins.enabled;
        let plugins_dir = config.plugins.plugins_dir.clone();
        drop(config);

        if !plugins_enabled {
            return (StatusCode::NOT_FOUND, "Plugins not enabled").into_response();
        }

        let mut host = match create_plugin_host(&plugins_dir) {
            Ok(h) => h,
            Err(e) => return e.into_response(),
        };

        match host.remove(&name) {
            Ok(()) => {
                tracing::info!(plugin = %name, "Plugin removed via API");
                Json(serde_json::json!({
                    "ok": true,
                    "message": format!("Plugin '{}' removed", name),
                }))
                .into_response()
            }
            Err(e) => {
                tracing::error!(plugin = %name, error = %e, "Plugin remove failed");
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "ok": false,
                        "error": e.to_string(),
                    })),
                )
                    .into_response()
            }
        }
    }
}
