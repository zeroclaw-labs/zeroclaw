//! Live SOP-runs WebSocket: pushes run summaries as the engine transitions.
//!
//! - `WS /ws/sops/runs`: initial snapshot then a live run-change feed.
//!
//! The snapshot and every subsequent frame come from the engine directly (its
//! in-memory active set plus retained terminal runs and its run-change
//! broadcast), never from polling. The engine is the source of truth; this
//! handler is a thin bridge from its `subscribe_run_changes` feed to the
//! browser.

use super::AppState;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use zeroclaw_runtime::sop::SopRunSummary;

const WS_PROTOCOL: &str = "zeroclaw.v1";

/// WS /ws/sops/runs, real-time SOP run summaries.
pub async fn handle_ws_sop_runs(
    State(state): State<AppState>,
    dashboard: Option<axum::Extension<std::sync::Arc<crate::dashboard_session::DashboardAuth>>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let mut session_watch = None;
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .or_else(|| {
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

        // A signed-in administrator session presents no token; the feed
        // ends when the session stops admitting it.
        let session = if token.is_empty() {
            crate::dashboard_session::operator_session(
                dashboard.as_ref().map(|axum::Extension(auth)| auth),
                &state,
            )
        } else {
            crate::dashboard_session::OperatorSession::Absent
        };
        match session {
            crate::dashboard_session::OperatorSession::Admitted(watch) => session_watch = watch,
            crate::dashboard_session::OperatorSession::Denied(denied) => return denied,
            crate::dashboard_session::OperatorSession::Absent => {
                if !state.pairing.is_authenticated(token) {
                    return (
                        StatusCode::UNAUTHORIZED,
                        "Unauthorized: provide Authorization header or Sec-WebSocket-Protocol bearer",
                    )
                        .into_response();
                }
            }
        }
    }

    let ws = if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_PROTOCOL))
    {
        ws.protocols([WS_PROTOCOL])
    } else {
        ws
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, session_watch))
        .into_response()
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    mut session_watch: Option<crate::dashboard_session::SessionWatch>,
) {
    let (mut sender, mut receiver) = socket.split();

    // The SOP subsystem may be disabled; tell the client and close.
    let Some(engine) = state.sop_engine.as_ref() else {
        let msg = serde_json::json!({ "type": "disabled" });
        let _ = sender.send(Message::Text(msg.to_string().into())).await;
        return;
    };

    // Snapshot + subscribe under one lock so no transition is missed between
    // reading the current set and arming the feed. The guard is dropped before
    // any await (the WS send) so the future stays Send.
    let locked: Result<
        (
            Vec<SopRunSummary>,
            Option<tokio::sync::broadcast::Receiver<SopRunSummary>>,
        ),
        (),
    > = {
        match engine.lock() {
            Ok(guard) => Ok((guard.run_summaries(None), guard.subscribe_run_changes())),
            Err(_) => Err(()),
        }
    };
    let (snapshot, rx) = match locked {
        Ok(v) => v,
        Err(()) => {
            let msg = serde_json::json!({ "type": "error", "error": "engine lock poisoned" });
            let _ = sender.send(Message::Text(msg.to_string().into())).await;
            return;
        }
    };

    let msg = serde_json::json!({ "type": "snapshot", "runs": snapshot });
    if sender
        .send(Message::Text(msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Forward run changes. It never completes on its own: with no notifier
    // attached (headless embedder) the snapshot stands, and the socket stays
    // open until the client leaves rather than closing abruptly.
    let forward = async {
        let Some(mut rx) = rx else {
            return std::future::pending::<()>().await;
        };
        loop {
            match rx.recv().await {
                Ok(run) => {
                    let msg = serde_json::json!({ "type": "run", "run": run });
                    if sender
                        .send(Message::Text(msg.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    let msg = serde_json::json!({ "type": "lagged", "missed": n });
                    let _ = sender.send(Message::Text(msg.to_string().into())).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        std::future::pending::<()>().await;
    };

    let read_loop = async {
        while let Some(m) = receiver.next().await {
            match m {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    };
    // A dashboard session that ends, or loses its access, ends the feed with
    // the session's close code, with or without a notifier.
    let lost = tokio::select! {
        () = read_loop => None,
        () = forward => None,
        code = crate::dashboard_session::session_lost(&mut session_watch, &state) => Some(code),
    };
    if let Some(code) = lost {
        crate::ws::close_with(&mut sender, &mut receiver, code, "dashboard session ended").await;
    }
}
