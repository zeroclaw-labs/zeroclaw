//! Onboarding state commands. Persisted via tauri-plugin-store.

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use crate::commands::permissions::get_permissions_status;
use crate::gateway_client::GatewayClient;
use crate::state::SharedState;

const STORE_FILE: &str = "settings.json";
const KEY_ONBOARDING_COMPLETE: &str = "onboarding_complete";

/// Best-effort: push the current "granted" capability list to the gateway so the agent
/// knows what this Mac can do. Failure is non-fatal — gateway may be offline or
/// the device may not yet have a token.
pub fn sync_capabilities_to_gateway<R: Runtime>(app: &AppHandle<R>) {
    let granted: Vec<String> = get_permissions_status()
        .into_iter()
        .filter(|p| p.status == "granted")
        .map(|p| p.name)
        .collect();

    let state: SharedState = app.state::<SharedState>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let (url, token) = {
            let s = state.read().await;
            (s.gateway_url.clone(), s.token.clone())
        };
        let Some(token) = token else { return };
        let client = GatewayClient::new(&url, Some(&token));
        let _ = client.update_capabilities(&granted).await;
    });
}

#[derive(Debug, Serialize)]
pub struct OnboardingState {
    pub complete: bool,
}

pub fn read_onboarding_complete<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.store(STORE_FILE)
        .ok()
        .and_then(|store| store.get(KEY_ONBOARDING_COMPLETE))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[tauri::command]
pub fn get_onboarding_state<R: Runtime>(app: AppHandle<R>) -> OnboardingState {
    OnboardingState {
        complete: read_onboarding_complete(&app),
    }
}

#[tauri::command]
pub fn complete_onboarding<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(KEY_ONBOARDING_COMPLETE, Value::Bool(true));
    store.save().map_err(|e| e.to_string())?;

    // Close the onboarding window and hand the user off to the dashboard.
    if let Some(window) = app.get_webview_window("onboarding") {
        let _ = window.close();
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.set_focus();
    }

    // Push the granted capability list to the gateway so the agent knows what
    // this Mac can do. Best-effort — non-fatal if the gateway is offline.
    sync_capabilities_to_gateway(&app);

    Ok(())
}

#[tauri::command]
pub fn reset_onboarding<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.delete(KEY_ONBOARDING_COMPLETE);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}
