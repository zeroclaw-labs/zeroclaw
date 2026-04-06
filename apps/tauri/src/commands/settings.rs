//! Tauri commands for desktop-specific settings management.

use crate::gateway_client::GatewayClient;
use crate::state::SharedState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSettings {
    pub launch_at_login: bool,
    pub show_dock_icon: bool,
    pub play_tray_animations: bool,
    pub allow_canvas: bool,
    pub allow_camera: bool,
    pub enable_peekaboo_bridge: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            show_dock_icon: true,
            play_tray_animations: true,
            allow_canvas: true,
            allow_camera: true,
            enable_peekaboo_bridge: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GatewayInfo {
    pub active: bool,
    pub version: Option<String>,
    pub port: u16,
    pub pid: Option<u32>,
    pub health_status: String,
}

/// Load desktop settings from tauri-plugin-store.
fn load_settings(app: &tauri::AppHandle) -> DesktopSettings {
    use tauri_plugin_store::StoreExt;
    let store = app.store("desktop_settings.json");
    match store {
        Ok(s) => {
            let get = |key: &str, default: bool| -> bool {
                s.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
            };
            DesktopSettings {
                launch_at_login: get("launch_at_login", false),
                show_dock_icon: get("show_dock_icon", true),
                play_tray_animations: get("play_tray_animations", true),
                allow_canvas: get("allow_canvas", true),
                allow_camera: get("allow_camera", true),
                enable_peekaboo_bridge: get("enable_peekaboo_bridge", false),
            }
        }
        Err(_) => DesktopSettings::default(),
    }
}

/// Save a single setting to the store.
fn save_setting(app: &tauri::AppHandle, key: &str, value: bool) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;
    let store = app
        .store("desktop_settings.json")
        .map_err(|e| e.to_string())?;
    store.set(key.to_string(), serde_json::Value::Bool(value));
    store.save().map_err(|e| e.to_string())
}

/// Get all desktop settings.
#[tauri::command]
pub fn get_desktop_settings(app: tauri::AppHandle) -> DesktopSettings {
    load_settings(&app)
}

/// Set a single desktop setting by key.
#[tauri::command]
pub fn set_desktop_setting(app: tauri::AppHandle, key: String, value: bool) -> Result<(), String> {
    let valid_keys = [
        "launch_at_login",
        "show_dock_icon",
        "play_tray_animations",
        "allow_canvas",
        "allow_camera",
        "enable_peekaboo_bridge",
    ];
    if !valid_keys.contains(&key.as_str()) {
        return Err(format!("Unknown setting: {key}"));
    }
    save_setting(&app, &key, value)
}

/// Toggle the gateway on/off. Returns the new active state.
#[tauri::command]
pub async fn toggle_gateway(state: State<'_, SharedState>) -> Result<bool, String> {
    let (url, connected) = {
        let s = state.read().await;
        (s.gateway_url.clone(), s.connected)
    };

    if connected {
        // Send shutdown to the gateway.
        let client = reqwest::Client::new();
        let _ = client.post(format!("{url}/admin/shutdown")).send().await;
        {
            let mut s = state.write().await;
            s.connected = false;
        }
        Ok(false)
    } else {
        // Attempt to start gateway via CLI.
        let _ = tokio::process::Command::new("zeroclaw")
            .args(["service", "start"])
            .status()
            .await;
        // Give it a moment to start.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let client = GatewayClient::new(&url, None);
        let healthy = client.get_health().await.unwrap_or(false);
        {
            let mut s = state.write().await;
            s.connected = healthy;
        }
        Ok(healthy)
    }
}

/// Get gateway status information.
#[tauri::command]
pub async fn get_gateway_info(state: State<'_, SharedState>) -> Result<GatewayInfo, String> {
    let (url, token, connected) = {
        let s = state.read().await;
        (s.gateway_url.clone(), s.token.clone(), s.connected)
    };

    let port = url
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(42617);

    let (active, version) = if connected {
        let client = GatewayClient::new(&url, token.as_deref());
        let status = client.get_status().await;
        match status {
            Ok(v) => (
                true,
                v.get("version").and_then(|v| v.as_str()).map(String::from),
            ),
            Err(_) => (false, None),
        }
    } else {
        (false, None)
    };

    Ok(GatewayInfo {
        active,
        version,
        port,
        pid: None, // PID discovery would require reading the launchd plist or pidfile.
        health_status: if active {
            "healthy".into()
        } else {
            "unreachable".into()
        },
    })
}

/// Enable or disable launch at login.
#[tauri::command]
pub fn set_launch_at_login(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    save_setting(&app, "launch_at_login", enabled)?;

    // On macOS, manage the login item via osascript.
    #[cfg(target_os = "macos")]
    {
        let action = if enabled { "true" } else { "false" };
        let script = format!(
            r#"tell application "System Events" to make login item at end with properties {{path:"/Applications/ZeroClaw.app", hidden:{action}}}"#
        );
        if enabled {
            let _ = std::process::Command::new("osascript")
                .args(["-e", &script])
                .status();
        } else {
            let script = r#"tell application "System Events" to delete login item "ZeroClaw""#;
            let _ = std::process::Command::new("osascript")
                .args(["-e", script])
                .status();
        }
    }

    Ok(())
}
