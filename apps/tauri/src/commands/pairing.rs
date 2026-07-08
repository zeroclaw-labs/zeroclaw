use crate::gateway_client::GatewayClient;
use crate::state::SharedState;
use tauri::State;

#[tauri::command]
pub async fn initiate_pairing(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let s = state.read().await;
    let client = GatewayClient::new(&s.gateway_url, s.token.as_deref());
    drop(s);
    client.initiate_pairing().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_devices(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let s = state.read().await;
    let client = GatewayClient::new(&s.gateway_url, s.token.as_deref());
    drop(s);
    client.get_devices().await.map_err(|e| e.to_string())
}
