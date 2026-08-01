//! ZeroClaw Desktop — Tauri application library.

pub mod capabilities;
pub mod commands;
pub mod daemon;
pub mod gateway_client;
pub mod health;
pub mod macos;
pub mod state;
pub mod tray;

use gateway_client::GatewayClient;
use state::shared_state;
use tauri::{Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// Loopback port the desktop app expects the gateway/daemon on. Matches the
/// port baked into [`state::AppState::default`]'s `gateway_url`.
const GATEWAY_PORT: u16 = 42617;

/// Status the splash listens for (`zeroclaw://splash-status`). Drives the
/// splash copy when we're starting our own daemon or hit a problem; the happy
/// path is covered by the splash's own health polling, so a missed event is
/// harmless.
#[derive(Clone, serde::Serialize)]
struct SplashStatus {
    /// `starting` | `error` | `missing`.
    kind: &'static str,
    message: String,
}

/// Ensure a gateway/daemon is reachable: reuse one if it already answers,
/// otherwise launch a fresh `zeroclaw daemon`. The splash window's health
/// polling takes over once the daemon is up and opens the dashboard.
async fn ensure_daemon(app: tauri::AppHandle, state: state::SharedState) {
    let url = {
        let s = state.read().await;
        s.gateway_url.clone()
    };
    let client = GatewayClient::new(&url, None);

    // Give an already-running gateway/daemon a moment to answer before we
    // decide nothing is there — avoids racing a daemon that's mid-startup.
    for _ in 0..3 {
        if client.get_health().await.unwrap_or(false) {
            return; // Reuse the existing instance.
        }
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }

    // Nothing listening — start our own daemon.
    match daemon::find_zeroclaw_binary() {
        Some(bin) => {
            let _ = app.emit(
                "zeroclaw://splash-status",
                SplashStatus {
                    kind: "starting",
                    message: "Starting the ZeroClaw daemon…".to_string(),
                },
            );
            if let Err(e) = daemon::spawn_daemon(&bin, GATEWAY_PORT) {
                let _ = app.emit(
                    "zeroclaw://splash-status",
                    SplashStatus {
                        kind: "error",
                        message: format!("Couldn't start the ZeroClaw daemon: {e}"),
                    },
                );
            }
            // On success the splash's health poll detects the daemon and
            // calls `open_dashboard`.
        }
        None => {
            let _ = app.emit(
                "zeroclaw://splash-status",
                SplashStatus {
                    kind: "missing",
                    message: "Couldn't find the `zeroclaw` binary. Install ZeroClaw \
                              (or start a daemon yourself) and reopen the app."
                        .to_string(),
                },
            );
        }
    }
}

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

#[tauri::command]
async fn open_dashboard(
    app: tauri::AppHandle,
    state: tauri::State<'_, state::SharedState>,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    let base = {
        let s = state.read().await;
        s.gateway_url.clone()
    };
    let token = auto_pair(state.inner()).await;

    let dashboard_url = format!("{}/", base.trim_end_matches('/'));
    let parsed = tauri::Url::parse(&dashboard_url).map_err(|e| e.to_string())?;

    let mut builder = WebviewWindowBuilder::new(&app, "main", WebviewUrl::External(parsed))
        .title("ZeroClaw")
        .inner_size(1200.0, 800.0)
        .center()
        .resizable(true);
    if let Some(token) = token {
        let escaped = token.replace('\\', "\\\\").replace('\'', "\\'");
        let script = format!(
            "try {{ localStorage.setItem('zeroclaw_token', '{escaped}'); }} catch (e) {{}}"
        );
        builder = builder.initialization_script(script.as_str());
    }
    builder.build().map_err(|e| e.to_string())?;

    // Hand off from the splash to the dashboard.
    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.close();
    }
    Ok(())
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
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // When a second instance launches, focus whichever surface is current.
            let target = app
                .get_webview_window("splash")
                .or_else(|| app.get_webview_window("main"));
            if let Some(window) = target {
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
            open_dashboard,
            capabilities::screenshot::take_screenshot,
            capabilities::applescript::run_applescript,
        ])
        .setup(move |app| {
            // Set macOS dock icon (needed for dev builds without .app bundle).
            #[cfg(target_os = "macos")]
            set_dock_icon();

            // Set up the system tray.
            let _ = tray::setup_tray(app);

            // Show the splash window on launch. It polls the gateway for
            // readiness and then asks the backend to open the dashboard
            // (`open_dashboard`) pointed at the running web gateway — which
            // takes a first-time user straight into the Quickstart.
            if let Some(splash) = app.get_webview_window("splash") {
                let _ = splash.show();
                let _ = splash.set_focus();
            }

            // Reuse a running gateway/daemon, or start a fresh `zeroclaw daemon`
            // if none is listening, so the app works without a manual setup step.
            let ensure_handle = app.handle().clone();
            let ensure_state = shared.clone();
            tauri::async_runtime::spawn(ensure_daemon(ensure_handle, ensure_state));

            // Start background health polling (drives the tray icon/tooltip).
            health::spawn_health_poller(app.handle().clone(), shared.clone());

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
