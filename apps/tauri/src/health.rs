//! Background health polling for the ZeroClaw gateway.

use crate::gateway_client::GatewayClient;
use crate::state::SharedState;
use crate::tray::icon;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Inject a bearer token into the WebView's localStorage so the React app
/// can authenticate without showing the pairing dialog.
///
/// Uses Tauri's `WebviewWindow::eval` — the standard Tauri API for running
/// scripts in the WebView context. The token is escaped to prevent injection.
fn inject_token<R: Runtime>(app: &AppHandle<R>, token: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let escaped = token
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        // Tauri's eval() is the documented API for WebView scripting.
        let script = format!("localStorage.setItem('zeroclaw_token', '{escaped}')");
        let _ = window.eval(&script);
        // Notify the React app that a fresh token is available so it can
        // recover from an unauthorized state without a full page reload.
        let _ = window.eval("window.dispatchEvent(new Event('zeroclaw-token-refreshed'))");
    }
}

/// Spawn a background task that polls gateway health and updates state + tray.
pub fn spawn_health_poller<R: Runtime>(app: AppHandle<R>, state: SharedState) {
    tauri::async_runtime::spawn(async move {
        loop {
            let (url, token) = {
                let s = state.read().await;
                (s.gateway_url.clone(), s.token.clone())
            };

            let client = GatewayClient::new(&url, token.as_deref());
            let healthy = client.get_health().await.unwrap_or(false);

            // If the gateway is up but our token is stale, auto-re-pair.
            if healthy {
                let needs_repair = if let Some(ref tok) = token {
                    let authed = GatewayClient::new(&url, Some(tok));
                    !authed.validate_token().await.unwrap_or(false)
                } else {
                    client.requires_pairing().await.unwrap_or(false)
                };

                if needs_repair {
                    let fresh = GatewayClient::new(&url, None);
                    if let Ok(new_token) = fresh.auto_pair().await {
                        {
                            let mut s = state.write().await;
                            s.token = Some(new_token.clone());
                        }
                        inject_token(&app, &new_token);
                    }
                }
            }

            let (connected, agent_status) = {
                let mut s = state.write().await;
                s.connected = healthy;
                (s.connected, s.agent_status)
            };

            // Update the tray icon and tooltip to reflect current state.
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_icon(Some(icon::icon_for_state(connected, agent_status)));
                let _ = tray.set_tooltip(Some(icon::tooltip_for_state(connected, agent_status)));
            }

            let _ = app.emit("zeroclaw://status-changed", healthy);

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}
