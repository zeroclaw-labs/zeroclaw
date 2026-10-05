//! ZeroClaw Desktop — Tauri application library.

pub mod capabilities;
pub mod commands;
pub mod daemon;
pub mod gateway_client;
pub mod health;
pub mod macos;
pub mod possession;
pub mod readiness;
pub mod state;
pub mod tray;

use gateway_client::GatewayClient;
use state::{Startup, shared_state};
use tauri::{Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// Loopback port the desktop app expects the gateway/daemon on. Matches the
/// port baked into [`state::AppState::default`]'s `gateway_url`.
const GATEWAY_PORT: u16 = 42617;

/// The event the splash listens on for startup changes. It only prompts the
/// splash to re-read [`get_startup`], so a missed event costs a poll interval.
const STARTUP_EVENT: &str = "zeroclaw://startup";

/// Record where startup stands, then tell the splash.
async fn publish_startup(app: &tauri::AppHandle, state: &state::SharedState, startup: Startup) {
    state.write().await.startup = startup.clone();
    let _ = app.emit(STARTUP_EVENT, startup);
}

fn startup_failed(kind: &'static str, message: String) -> Startup {
    Startup::Failed { kind, message }
}

/// Ensure a gateway/daemon is reachable: reuse one if it already answers,
/// otherwise launch a fresh `zeroclaw daemon` and check it. The outcome is
/// recorded as [`Startup`]; the splash opens the dashboard only once it is
/// ready.
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
            // Reuse the existing instance.
            publish_startup(&app, &state, Startup::Ready).await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }

    // Nothing listening — start our own daemon.
    match daemon::find_zeroclaw_binary() {
        Some(bin) => {
            publish_startup(
                &app,
                &state,
                Startup::Pending {
                    message: "Starting the ZeroClaw daemon…".to_string(),
                },
            )
            .await;
            let bundled = daemon::is_bundled_kernel(&bin);
            match daemon::spawn_daemon(&bin, GATEWAY_PORT) {
                Err(e) => {
                    let failed = match daemon::ReadinessFailure::from_launch_error(&e) {
                        Some(failure) if failure.reason == "endpoint_held" => startup_failed(
                            "endpoint_held",
                            format!(
                                "Couldn't start the ZeroClaw daemon: {failure}. Stop the other ZeroClaw and reopen the app."
                            ),
                        ),
                        _ if e.kind() == std::io::ErrorKind::TimedOut => startup_failed(
                            "timeout",
                            format!("Couldn't start the ZeroClaw daemon in time: {e}"),
                        ),
                        _ => startup_failed(
                            "error",
                            format!("Couldn't start the ZeroClaw daemon: {e}"),
                        ),
                    };
                    publish_startup(&app, &state, failed).await;
                }
                Ok(mut launched) => {
                    let (outcome, core) = match &launched.readiness {
                        daemon::Readiness::Rpc { endpoint, pid } => {
                            match readiness::verify_core(endpoint, *pid, bundled).await {
                                Ok(core) => {
                                    let outcome = readiness::await_gateway(
                                        &url,
                                        Some(&core),
                                        readiness::GATEWAY_READY_DEADLINE,
                                    )
                                    .await;
                                    // Kept for the proofs every credential
                                    // handoff to the dashboard address makes.
                                    let link = readiness::dashboard_addr(&url).map(|dashboard| {
                                        std::sync::Arc::new(possession::CoreLink::new(
                                            core,
                                            endpoint.clone(),
                                            bundled,
                                            dashboard,
                                        ))
                                    });
                                    (outcome, link)
                                }
                                Err(failure) => (Err(failure), None),
                            }
                        }
                        // An older kernel reports readiness at daemon start;
                        // only the gateway's health can say more.
                        daemon::Readiness::Spawned => (
                            readiness::await_gateway(&url, None, readiness::GATEWAY_READY_DEADLINE)
                                .await,
                            None,
                        ),
                    };
                    match outcome {
                        Ok(()) => {
                            state.write().await.core = core;
                            publish_startup(&app, &state, Startup::Ready).await;
                        }
                        Err(failure) => {
                            publish_startup(
                                &app,
                                &state,
                                startup_failed(failure.kind(), failure.message().to_string()),
                            )
                            .await;
                            if failure.is_final() {
                                let _ = daemon::terminate_supervisor_tree(&mut launched.child);
                            }
                        }
                    }
                }
            }
        }
        None => {
            publish_startup(
                &app,
                &state,
                startup_failed(
                    "missing",
                    "Couldn't find the `zeroclaw` binary. Install ZeroClaw \
                     (or start a daemon yourself) and reopen the app."
                        .to_string(),
                ),
            )
            .await;
        }
    }
}

/// Attempt to auto-pair with the gateway so the WebView has a valid token
/// before the React frontend mounts.
///
/// For the daemon this app launched, the code comes from the core over its
/// verified RPC socket, so no admin token is presented and nothing secret
/// crosses the dashboard's port to get it; the code and the stored token
/// then travel only on connections that proved they are the core's own
/// gateway. For a gateway that was already running, the code is minted
/// through the kernel CLI, which presents the gateway's owner-only admin
/// token; see [`daemon::mint_pairing_code`].
///
/// With a launched core, whether pairing is needed is the proven listener's
/// own answer, and not getting it is an error, never "no token needed".
/// `Ok(None)` means no token is handed over, not that the WebView holds none.
async fn auto_pair(state: &state::SharedState) -> Result<Option<String>, String> {
    let (url, core) = {
        let s = state.read().await;
        (s.gateway_url.clone(), s.core.clone())
    };

    let client = GatewayClient::new(&url, None).with_core(core.clone());

    // Check if gateway is reachable and requires pairing.
    let requires_pairing = match client.requires_pairing().await {
        Ok(requires) => requires,
        Err(error) if core.is_some() => return Err(error.to_string()),
        // A gateway that was already running and does not answer pairs
        // nothing; there is nothing to prove it against.
        Err(_) => false,
    };
    if !requires_pairing {
        return Ok(None); // Pairing disabled — no token needed.
    }

    // Check if we already have a valid token in state.
    {
        let s = state.read().await;
        if let Some(ref token) = s.token {
            let authed = GatewayClient::new(&url, Some(token)).with_core(core.clone());
            if authed.validate_token().await.unwrap_or(false) {
                return Ok(Some(token.clone())); // Existing token is valid.
            }
        }
    }

    // No valid token — mint a new code and exchange it.
    let code = match &core {
        Some(core) => core.new_pairing_code().await.ok(),
        None => match daemon::find_zeroclaw_binary() {
            Some(binary) => tokio::task::spawn_blocking(move || {
                daemon::mint_pairing_code(&binary, GATEWAY_PORT)
            })
            .await
            .ok()
            .and_then(Result::ok),
            None => None,
        },
    };
    let Some(code) = code else {
        return Ok(None);
    };
    match client.pair_with_code(&code).await {
        Ok(token) => {
            let mut s = state.write().await;
            s.token = Some(token.clone());
            Ok(Some(token))
        }
        Err(_) => Ok(None), // Gateway may not be ready yet; health poller will retry.
    }
}

/// Where startup stands, for the splash.
#[tauri::command]
async fn get_startup(state: tauri::State<'_, state::SharedState>) -> Result<Startup, String> {
    Ok(state.read().await.startup.clone())
}

/// What the dashboard window opens with.
struct DashboardOpening {
    url: tauri::Url,
    /// The bearer the WebView is given, when pairing produced one.
    token: Option<String>,
}

/// Whether the dashboard may open now, and with what.
async fn admit_dashboard(state: &state::SharedState) -> Result<DashboardOpening, String> {
    // Only a ready startup may open the dashboard or pair with its gateway,
    // whatever the splash believed when it asked.
    let base = {
        let s = state.read().await;
        s.startup.dashboard_gate()?;
        s.gateway_url.clone()
    };
    let token = auto_pair(state).await.map_err(unproven_dashboard)?;
    let core = {
        let s = state.read().await;
        s.startup.dashboard_gate()?;
        s.core.clone()
    };
    // Whenever the app launched the core, the dashboard opens only on an
    // address proven, right now, to be that core's gateway, whatever pairing
    // found: the WebView sends the token there from now on, and may hold the
    // origin's saved credentials even when no token is handed over.
    if let Some(core) = core {
        core.prove()
            .await
            .map_err(|failure| unproven_dashboard(failure.to_string()))?;
    }

    let dashboard_url = format!("{}/", base.trim_end_matches('/'));
    let url = tauri::Url::parse(&dashboard_url).map_err(|e| e.to_string())?;
    Ok(DashboardOpening { url, token })
}

/// Why the dashboard was not opened on an address that did not prove itself.
fn unproven_dashboard(failure: String) -> String {
    format!(
        "The dashboard address is not the ZeroClaw core's gateway right now ({failure}), so the dashboard was not opened. Reopen ZeroClaw."
    )
}

/// Build the dashboard window with `open_window`, only once
/// [`admit_dashboard`] admits it.
async fn open_admitted_dashboard(
    state: &state::SharedState,
    open_window: impl FnOnce(DashboardOpening) -> Result<(), String>,
) -> Result<(), String> {
    open_window(admit_dashboard(state).await?)
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

    open_admitted_dashboard(state.inner(), |opening| {
        let mut builder =
            WebviewWindowBuilder::new(&app, "main", WebviewUrl::External(opening.url))
                .title("ZeroClaw")
                .inner_size(1200.0, 800.0)
                .center()
                .resizable(true);
        if let Some(token) = opening.token {
            let escaped = token.replace('\\', "\\\\").replace('\'', "\\'");
            let script = format!(
                "try {{ localStorage.setItem('zeroclaw_token', '{escaped}'); }} catch (e) {{}}"
            );
            builder = builder.initialization_script(script.as_str());
        }
        builder.build().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await?;

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
        // SAFETY: `mtm` proves this code is running on the AppKit main thread,
        // and both `app` and `image` are live Objective-C objects for the
        // duration of the message send.
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
            get_startup,
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

            // Show the splash window on launch. It polls the startup state
            // (`get_startup`) and, once it is ready, asks the backend to open
            // the dashboard (`open_dashboard`) pointed at the running web
            // gateway — which takes a first-time user straight into the
            // Quickstart.
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
