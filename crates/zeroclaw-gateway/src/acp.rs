//! ACP-over-WebSocket gateway endpoint.

use super::AppState;
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::HeaderMap,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeroclaw_channels::orchestrator::acp_server::{AcpServer, AcpServerConfig};
use zeroclaw_infra::acp_session_store::AcpSessionStore;

const ACP_WS_PROTOCOL: &str = "zeroclaw.acp.v1";

#[derive(Debug, Deserialize)]
pub struct AcpQuery {
    token: Option<String>,
    /// Connection-scoped default agent alias. Used when `session/new` omits
    /// `agentAlias`, so spec-vanilla one-agent-per-endpoint ACP clients can
    /// address each configured agent via its own URL. Validated exactly like
    /// an explicit `agentAlias` at `session/new`; ignored by session restore.
    agent: Option<String>,
}

pub async fn handle_ws_acp(
    State(state): State<AppState>,
    Query(params): Query<AcpQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if state.pairing.require_pairing() {
        let token = extract_ws_token(&headers, params.token.as_deref()).unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized - provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
            )
                .into_response();
        }
    }

    let ws = if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == ACP_WS_PROTOCOL))
    {
        ws.protocols([ACP_WS_PROTOCOL])
    } else {
        ws
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, params.agent))
        .into_response()
}

async fn handle_socket(socket: WebSocket, state: AppState, default_agent: Option<String>) {
    let (mut sender, mut receiver) = socket.split();
    let (input_tx, input_rx) = mpsc::channel::<String>(256);
    let (output_tx, mut output_rx) = mpsc::channel::<String>(256);

    let config = state.config.read().clone();
    let acp_config = AcpServerConfig {
        max_sessions: config.acp.max_sessions,
        session_timeout_secs: config.acp.session_timeout_secs,
    };
    let store = AcpSessionStore::new(&config.data_dir)
        .map(Arc::new)
        .inspect_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "Failed to open ACP session store"
            );
        })
        .ok();
    let canvas_store = state.canvas_store.clone();
    let server = if let Some(store) = store {
        Arc::new(
            AcpServer::new_with_live_config_and_writer_and_store(
                Arc::clone(&state.config),
                acp_config,
                output_tx,
                store,
            )
            .with_canvas_store(canvas_store)
            .with_sop_engine(state.sop_engine.clone(), state.sop_audit.clone())
            .with_connection_default_agent(default_agent),
        )
    } else {
        Arc::new(
            AcpServer::new_with_live_config_and_writer(
                Arc::clone(&state.config),
                acp_config,
                output_tx,
            )
            .with_canvas_store(canvas_store)
            .with_sop_engine(state.sop_engine.clone(), state.sop_audit.clone())
            .with_connection_default_agent(default_agent),
        )
    };

    let server_task = zeroclaw_spawn::spawn!(Arc::clone(&server).run_messages(input_rx));

    let output_task = zeroclaw_spawn::spawn!(async move {
        while let Some(line) = output_rx.recv().await {
            if sender.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(message) = receiver.next().await {
        match message {
            Ok(Message::Text(text)) => {
                if input_tx.send(text.to_string()).await.is_err() {
                    break;
                }
            }
            Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => {
                    if input_tx.send(text).await.is_err() {
                        break;
                    }
                }
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "ACP WebSocket received non-UTF-8 binary frame"
                ),
            },
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Connection reset without closing handshake")
                    || msg.contains("Connection closed normally")
                {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "ACP WebSocket closed without handshake"
                    );
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "ACP WebSocket receive error"
                    );
                }
                break;
            }
        }
    }

    drop(input_tx);

    if let Err(e) = server_task.await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "ACP WebSocket server task panicked"
        );
    }
    output_task.abort();
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "ACP WebSocket disconnected"
    );
}

fn extract_ws_token<'a>(headers: &'a HeaderMap, query_token: Option<&'a str>) -> Option<&'a str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .or_else(|| {
            headers
                .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|v| v.to_str().ok())
                .and_then(|protocols| {
                    protocols
                        .split(',')
                        .map(str::trim)
                        .find_map(|p| p.strip_prefix("bearer."))
                })
                .filter(|token| !token.is_empty())
        })
        .or_else(|| query_token.filter(|token| !token.is_empty()))
}
