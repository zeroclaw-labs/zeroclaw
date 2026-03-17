use crate::gateway_client::GatewayClient;
use crate::state::AppState;
use tauri::State;

#[tauri::command]
pub async fn initiate_pairing(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let url = state.gateway_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state.token.lock().map_err(|e| e.to_string())?.clone();
    let client = GatewayClient::new(&url, token.as_deref());
    client.initiate_pairing().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_devices(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let url = state.gateway_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state.token.lock().map_err(|e| e.to_string())?.clone();
    let client = GatewayClient::new(&url, token.as_deref());
    client.get_devices().await.map_err(|e| e.to_string())
}
