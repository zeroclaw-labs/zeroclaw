//! ZeroClaw Desktop — Tauri application library.

pub mod commands;
pub mod gateway_client;
pub mod health;
pub mod state;
pub mod tray;

use gateway_client::GatewayClient;
use state::shared_state;
use tauri::Manager;

/// Attempt to auto-pair with the gateway so the WebView has a valid token
/// before the React frontend mounts. Runs on localhost so the admin endpoints
/// are accessible without auth.
async fn auto_pair(state: &state::SharedState) -> Option<String> {
    let url = {
        let s = state.read().await;
        s.gateway_url.clone()
    };

    let client = GatewayClient::new(&url, None);

    // Check if gateway is reachable and requires pairing.
    if !client.requires_pairing().await.unwrap_or(false) {
        return None; // Pairing disabled — no token needed.
    }

    // Check if we already have a valid token in state.
    {
        let s = state.read().await;
        if let Some(ref token) = s.token {
            let authed = GatewayClient::new(&url, Some(token));
            if authed.validate_token().await.unwrap_or(false) {
                return Some(token.clone()); // Existing token is valid.
            }
        }
    }

    // No valid token — auto-pair by requesting a new code and exchanging it.
    let client = GatewayClient::new(&url, None);
    match client.auto_pair().await {
        Ok(token) => {
            let mut s = state.write().await;
            s.token = Some(token.clone());
            Some(token)
        }
        Err(_) => None, // Gateway may not be ready yet; health poller will retry.
    }
}

/// Inject a bearer token into the WebView's localStorage so the React app
/// skips the pairing dialog. Uses Tauri's WebviewWindow scripting API.
fn inject_token_into_webview<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>, token: &str) {
    let escaped = token.replace('\\', "\\\\").replace('\'', "\\'");
    let script = format!("localStorage.setItem('zeroclaw_token', '{escaped}')");
    // WebviewWindow scripting is the standard Tauri API for running JS in the WebView.
    let _ = window.eval(&script);
}

/// Configure and run the Tauri application.
pub fn run() {
    let shared = shared_state();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // When a second instance launches, focus the existing window.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            commands::gateway::get_status,
            commands::gateway::get_health,
            commands::channels::list_channels,
            commands::pairing::initiate_pairing,
            commands::pairing::get_devices,
            commands::agent::send_message,
        ])
        .setup(move |app| {
            // Set up the system tray.
            let _ = tray::setup_tray(app);

            // Auto-pair with gateway and inject token into the WebView.
            let app_handle = app.handle().clone();
            let pair_state = shared.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(token) = auto_pair(&pair_state).await {
                    if let Some(window) = app_handle.get_webview_window("main") {
                        inject_token_into_webview(&window, &token);
                    }
                }
            });

            // Start background health polling.
            health::spawn_health_poller(app.handle().clone(), shared.clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
