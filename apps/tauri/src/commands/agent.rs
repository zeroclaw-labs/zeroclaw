use crate::gateway_client::GatewayClient;
use crate::state::SharedState;
use tauri::State;

#[tauri::command]
pub async fn send_message(
    state: State<'_, SharedState>,
    message: String,
) -> Result<serde_json::Value, String> {
    let s = state.read().await;
    let client = GatewayClient::new(&s.gateway_url, s.token.as_deref());
    drop(s);
    client
        .send_webhook_message(&message)
        .await
        .map_err(|e| e.to_string())
}
