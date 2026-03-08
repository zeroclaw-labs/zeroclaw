//! WebSocket agent chat handler.
//!
//! Protocol:
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! ```

use super::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// GET /ws/chat — WebSocket upgrade for agent chat
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    Query(params): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth via query param (browser WebSocket limitation)
    if state.pairing.require_pairing() {
        let token = params.token.as_deref().unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide ?token=<bearer_token>",
            )
                .into_response();
        }
    }

    let requested_subprotocols = parse_requested_subprotocols(&headers);
    let ws = if requested_subprotocols.is_empty() {
        ws
    } else {
        ws.protocols(requested_subprotocols)
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

fn parse_requested_subprotocols(headers: &HeaderMap) -> Vec<String> {
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => continue,
        };

        // Parse incoming message
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => {
                let err = serde_json::json!({"type": "error", "message": "Invalid JSON"});
                let _ = sender.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let msg_type = parsed["type"].as_str().unwrap_or("");
        if msg_type != "message" {
            continue;
        }

        let content = parsed["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            continue;
        }

        // Process message with the LLM provider
        let provider_label = state
            .config
            .lock()
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Broadcast agent_start event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": state.model,
        }));

        // Simple single-turn chat (no streaming for now — use provider.chat_with_system)
        let system_prompt = {
            let config_guard = state.config.lock();
            crate::channels::build_system_prompt(
                &config_guard.workspace_dir,
                &state.model,
                &[],
                &[],
                Some(&config_guard.identity),
                None,
            )
        };

        let messages = vec![
            crate::providers::ChatMessage::system(system_prompt),
            crate::providers::ChatMessage::user(&content),
        ];

        let multimodal_config = state.config.lock().multimodal.clone();
        let prepared =
            match crate::multimodal::prepare_messages_for_provider(&messages, &multimodal_config)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": format!("Multimodal prep failed: {e}")
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    continue;
                }
            };

        match state
            .provider
            .chat_with_history(&prepared.messages, &state.model, state.temperature)
            .await
        {
            Ok(response) => {
                // Send the full response as a done message
                let done = serde_json::json!({
                    "type": "done",
                    "full_response": response,
                });
                let _ = sender.send(Message::Text(done.to_string().into())).await;

                // Broadcast agent_end event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": provider_label,
                    "model": state.model,
                }));
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                let err = serde_json::json!({
                    "type": "error",
                    "message": sanitized,
                });
                let _ = sender.send(Message::Text(err.to_string().into())).await;

                // Broadcast error event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "error",
                    "component": "ws_chat",
                    "message": sanitized,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::gateway::{GatewayRateLimiter, IdempotencyStore};
    use crate::memory::{Memory, MemoryCategory, MemoryEntry};
    use crate::providers::Provider;
    use crate::security::pairing::PairingGuard;
    use async_trait::async_trait;
    use axum::routing::get;
    use axum::Router;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[tokio::test]
    async fn ws_handshake_echoes_requested_subprotocol() {
        let app = Router::new()
            .route("/ws/chat", get(handle_ws_chat))
            .with_state(test_state());
        let (addr, server_task) = spawn_test_server(app).await;

        let mut request = format!("ws://{addr}/ws/chat")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "agent-chat, json".parse().unwrap(),
        );

        let (socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            response
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok()),
            Some("agent-chat")
        );
        drop(socket);
        server_task.abort();
    }

    #[tokio::test]
    async fn ws_handshake_omits_subprotocol_when_not_requested() {
        let app = Router::new()
            .route("/ws/chat", get(handle_ws_chat))
            .with_state(test_state());
        let (addr, server_task) = spawn_test_server(app).await;

        let request = format!("ws://{addr}/ws/chat")
            .into_client_request()
            .unwrap();
        let (socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();

        assert!(response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .is_none());
        drop(socket);
        server_task.abort();
    }

    async fn spawn_test_server(app: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, task)
    }

    fn test_state() -> AppState {
        AppState {
            config: Arc::new(Mutex::new(Config::default())),
            provider: Arc::new(MockProvider),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            whatsapp: None,
            whatsapp_app_secret: None,
            linq: None,
            linq_signing_secret: None,
            nextcloud_talk: None,
            nextcloud_talk_webhook_secret: None,
            wati: None,
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
        }
    }

    struct MockMemory;

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }
}
