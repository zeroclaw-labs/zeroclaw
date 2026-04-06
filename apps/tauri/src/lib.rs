//! ZeroClaw Desktop — Tauri application library.

pub mod commands;
pub mod gateway_client;
pub mod health;
pub mod local_node;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod state;
pub mod tray;

use gateway_client::GatewayClient;
use state::shared_state;
use tauri::{Manager, RunEvent};

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

/// Set the macOS dock icon programmatically so it shows even in dev builds
/// (which don't have a proper .app bundle).
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::NSApplication;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSData;

    let icon_bytes = include_bytes!("../icons/128x128.png");
    // Safety: setup() runs on the main thread in Tauri.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let data = NSData::with_bytes(icon_bytes);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        let app = NSApplication::sharedApplication(mtm);
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }
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
            // Permissions
            commands::permissions::get_permissions_status,
            commands::permissions::request_permission,
            commands::permissions::open_privacy_settings,
            // Automation
            commands::automation::applescript::run_applescript_action,
            commands::automation::applescript::run_applescript_raw,
            commands::automation::screen::capture_screen,
            commands::automation::camera::capture_photo,
            commands::automation::microphone::record_audio,
            commands::automation::notifications::send_desktop_notification,
            commands::automation::accessibility::inspect_ui_element,
            commands::automation::accessibility::click_ui_element,
            commands::automation::accessibility::type_into_element,
            // Desktop settings
            commands::settings::get_desktop_settings,
            commands::settings::set_desktop_setting,
            commands::settings::toggle_gateway,
            commands::settings::get_gateway_info,
            commands::settings::set_launch_at_login,
        ])
        .setup(move |app| {
            // Set macOS dock icon (needed for dev builds without .app bundle).
            #[cfg(target_os = "macos")]
            set_dock_icon();

            // Set up the system tray.
            let _ = tray::setup_tray(app);

            // Auto-pair with gateway and inject token into the WebView.
            // Retries up to 10 times (5s total) to wait for gateway readiness.
            let app_handle = app.handle().clone();
            let pair_state = shared.clone();
            tauri::async_runtime::spawn(async move {
                for _ in 0..10 {
                    if let Some(token) = auto_pair(&pair_state).await {
                        if let Some(window) = app_handle.get_webview_window("main") {
                            inject_token_into_webview(&window, &token);
                        }
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                // If pairing not required, no token is needed — frontend handles this.
            });

            // Start background health polling.
            health::spawn_health_poller(app.handle().clone(), shared.clone());

            // Register as a local node with the gateway for desktop automation.
            local_node::spawn_local_node(shared.clone());

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Keep the app running in the background when all windows are closed.
            // This is the standard pattern for menu bar / tray apps.
            if let RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
