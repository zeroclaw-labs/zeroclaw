use crate::gateway_client::GatewayClient;
use crate::state::AppState;
use tauri::State;

#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    message: String,
) -> Result<serde_json::Value, String> {
    let url = state.gateway_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state.token.lock().map_err(|e| e.to_string())?.clone();
    let client = GatewayClient::new(&url, token.as_deref());
    client.send_webhook_message(&message).await.map_err(|e| e.to_string())
}
