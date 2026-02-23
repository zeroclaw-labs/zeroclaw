//! WebSocket agent chat 处理器。
//!
//! 协议格式：
//! ```text
//! 客户端 -> 服务端: {"type":"message","content":"Hello"}
//! 服务端 -> 客户端: {"type":"chunk","content":"Hi! "}
//! 服务端 -> 客户端: {"type":"tool_call","name":"shell","args":{...}}
//! 服务端 -> 客户端: {"type":"tool_result","name":"shell","output":"..."}
//! 服务端 -> 客户端: {"type":"done","full_response":"..."}
//! ```
//!
//! 注意：此处使用完整的 Agent 工具循环（process_message），而非简单聊天接口，
//! 确保网页端对话也能调用工具（shell、file_read 等）。

use super::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// GET /ws/chat — WebSocket 升级，进入 Agent 聊天会话
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    Query(params): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // 通过 query 参数鉴权（浏览器 WebSocket 不支持自定义 Header）
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

    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

/// 处理单个 WebSocket 连接，每条消息都走完整 Agent 工具循环
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => continue,
        };

        // 解析客户端消息
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => {
                let err = serde_json::json!({"type": "error", "message": "消息格式错误，请发送合法的 JSON"});
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

        // 从当前运行时 config 克隆配置（用于 Agent 执行链）
        let config = state.config.lock().clone();

        let provider_label = config
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // 广播 agent_start 事件（供 SSE 仪表盘使用）
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": state.model,
        }));

        // 调用完整 Agent 执行链（含工具调用循环）
        match crate::agent::loop_::process_message(config, &content).await {
            Ok(response) => {
                // 返回完整响应
                let done = serde_json::json!({
                    "type": "done",
                    "full_response": response,
                });
                let _ = sender.send(Message::Text(done.to_string().into())).await;

                // 广播 agent_end 事件
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

                // 广播错误事件
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "error",
                    "component": "ws_chat",
                    "message": sanitized,
                }));
            }
        }
    }
}
