//! Live Canvas gateway routes — REST + WebSocket for real-time canvas updates.

use super::AppState;
use super::api::require_auth;
use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use zeroclaw_runtime::rpc::canvas::{self as canvas_rpc, CanvasFailure};

/// POST /api/canvas/:id request body.
#[derive(Deserialize)]
pub struct CanvasPostBody {
    pub content_type: Option<String>,
    pub content: String,
}

/// A canvas refusal as the dashboard sees it: the failure's own status and
/// an `{"error": message}` body, the same message `canvas/*` returns over RPC.
fn canvas_failure_response(failure: CanvasFailure) -> axum::response::Response {
    let status =
        StatusCode::from_u16(failure.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        Json(serde_json::json!({ "error": failure.message() })),
    )
        .into_response()
}

/// GET /api/canvas — list all active canvases.
pub async fn handle_canvas_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(canvas_rpc::list_body(&state.canvas_store)).into_response()
}

/// GET /api/canvas/:id — get current canvas content.
pub async fn handle_canvas_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    match canvas_rpc::get_body(&state.canvas_store, &id) {
        Ok(body) => Json(body).into_response(),
        Err(failure) => canvas_failure_response(failure),
    }
}

/// GET /api/canvas/:id/history — get canvas frame history.
pub async fn handle_canvas_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(canvas_rpc::history_body(&state.canvas_store, &id)).into_response()
}

/// POST /api/canvas/:id — push content to a canvas.
pub async fn handle_canvas_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CanvasPostBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    match canvas_rpc::render_body(
        &state.canvas_store,
        &id,
        body.content_type.as_deref(),
        &body.content,
    ) {
        Ok(rendered) => (StatusCode::CREATED, Json(rendered)).into_response(),
        Err(failure) => canvas_failure_response(failure),
    }
}

/// DELETE /api/canvas/:id — clear a canvas.
pub async fn handle_canvas_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(canvas_rpc::clear_body(&state.canvas_store, &id)).into_response()
}

/// WS /ws/canvas/:id — real-time canvas updates.
pub async fn handle_ws_canvas(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth check (same pattern as ws::handle_ws_chat)
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .or_else(|| {
                // Fallback: check query params in the upgrade request URI
                headers
                    .get("sec-websocket-protocol")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|protos| {
                        protos
                            .split(',')
                            .map(|p| p.trim())
                            .find_map(|p| p.strip_prefix("bearer."))
                    })
            })
            .unwrap_or("");

        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization header or Sec-WebSocket-Protocol bearer",
            )
                .into_response();
        }
    }

    // Echo Sec-WebSocket-Protocol if the client requests our sub-protocol
    // (browsers reject the upgrade if a requested protocol isn't echoed back).
    const WS_CANVAS_PROTOCOL: &str = "zeroclaw.v1";
    let ws = if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_CANVAS_PROTOCOL))
    {
        ws.protocols([WS_CANVAS_PROTOCOL])
    } else {
        ws
    };

    ws.on_upgrade(move |socket| handle_canvas_socket(socket, state, id))
        .into_response()
}

async fn handle_canvas_socket(socket: WebSocket, state: AppState, canvas_id: String) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to canvas updates
    let mut rx = match state.canvas_store.subscribe(&canvas_id) {
        Some(rx) => rx,
        None => {
            let msg = serde_json::json!({
                "type": "error",
                "error": "Maximum canvas count reached",
            });
            let _ = sender.send(Message::Text(msg.to_string().into())).await;
            return;
        }
    };

    // Send current state immediately if available
    if let Some(frame) = state.canvas_store.snapshot(&canvas_id) {
        let msg = serde_json::json!({
            "type": "frame",
            "canvas_id": canvas_id,
            "frame": frame,
        });
        let _ = sender.send(Message::Text(msg.to_string().into())).await;
    }

    // Send a connected acknowledgement
    let ack = serde_json::json!({
        "type": "connected",
        "canvas_id": canvas_id,
    });
    let _ = sender.send(Message::Text(ack.to_string().into())).await;

    // Spawn a task that forwards broadcast updates to the WebSocket
    let canvas_id_clone = canvas_id.clone();
    let send_task = zeroclaw_spawn::spawn!(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    let msg = serde_json::json!({
                        "type": "frame",
                        "canvas_id": canvas_id_clone,
                        "frame": frame,
                    });
                    if sender
                        .send(Message::Text(msg.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Client fell behind — notify and continue rather than disconnecting.
                    let msg = serde_json::json!({
                        "type": "lagged",
                        "canvas_id": canvas_id_clone,
                        "missed_frames": n,
                    });
                    let _ = sender.send(Message::Text(msg.to_string().into())).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Read loop: we mostly ignore incoming messages but handle close/ping
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {} // Ignore all other messages (pings are handled by axum)
        }
    }

    // Abort the send task when the connection is closed
    send_task.abort();
}
