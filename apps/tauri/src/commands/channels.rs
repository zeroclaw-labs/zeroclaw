use crate::gateway_client::GatewayClient;
use crate::state::SharedState;
use tauri::State;

#[tauri::command]
pub async fn list_channels(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let s = state.read().await;
    let client = GatewayClient::new(&s.gateway_url, s.token.as_deref());
    drop(s);
    client.get_status().await.map_err(|e| e.to_string())
}
