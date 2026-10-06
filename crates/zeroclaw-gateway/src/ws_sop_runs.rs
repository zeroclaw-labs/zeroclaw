//! Live SOP-runs WebSocket: pushes run summaries as the engine transitions.
//!
//! - `WS /ws/sops/runs`: initial snapshot then a live run-change feed.
//!
//! The snapshot and every subsequent frame come from the engine directly (its
//! in-memory active set plus retained terminal runs and its run-change
//! broadcast), never from polling. The engine is the source of truth; this
//! handler is a thin bridge from its `subscribe_run_changes` feed to the
//! browser.
//!
//! When the request reaches the core, the same frames come from the core's
//! `sops/subscribe-runs` subscription and its `sops/run-changed`
//! notifications instead.

use super::AppState;
use crate::core_rpc::{
    CoreAccess, CoreCall, CoreError, CoreSubscription, WsCoreAccess, subprotocol_bearer,
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use zeroclaw_api::jsonrpc::error_codes::INTERNAL_ERROR;
use zeroclaw_rpc_client::{Method, Notification};
use zeroclaw_rpc_proto::error_reasons;
use zeroclaw_rpc_proto::notification::{SOPS_RUN_CHANGED, SUBSCRIPTION_LAGGED};
use zeroclaw_rpc_proto::types::{SopRunChanged, SopsSubscribeRunsResult, SubscriptionLagged};
use zeroclaw_runtime::sop::SopRunSummary;

const WS_PROTOCOL: &str = "zeroclaw.v1";

/// WS /ws/sops/runs, real-time SOP run summaries.
pub async fn handle_ws_sop_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    WsCoreAccess(access): WsCoreAccess,
) -> impl IntoResponse {
    if let CoreAccess::Core(core) = access {
        return sop_runs_through_core(core, negotiate(ws, &headers))
            .await
            .unwrap_or_else(IntoResponse::into_response);
    }

    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .or_else(|| subprotocol_bearer(&headers))
            .unwrap_or("");

        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized: provide Authorization header or Sec-WebSocket-Protocol bearer",
            )
                .into_response();
        }
    }

    negotiate(ws, &headers)
        .on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

/// Answer with the `zeroclaw.v1` subprotocol when the client offered it.
pub(crate) fn negotiate(ws: WebSocketUpgrade, headers: &HeaderMap) -> WebSocketUpgrade {
    if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_PROTOCOL))
    {
        ws.protocols([WS_PROTOCOL])
    } else {
        ws
    }
}

async fn handle_socket(socket: WebSocket, state: AppState) {
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

    // No notifier attached (headless embedder): the snapshot stands; keep the
    // socket open until the client leaves rather than closing abruptly.
    let Some(mut rx) = rx else {
        while let Some(m) = receiver.next().await {
            match m {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        return;
    };

    let send_task = zeroclaw_spawn::spawn!(async move {
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
    });

    while let Some(m) = receiver.next().await {
        match m {
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    send_task.abort();
}

/// How the core answered `sops/subscribe-runs`, as the socket will tell it.
#[derive(Debug)]
pub(crate) enum Opened {
    /// A live subscription: its snapshot, then its changes. Dropping it
    /// cancels it on the core, so neither a socket that ends nor an upgrade
    /// that never completes leaves the feed running on a connection the
    /// caller's other requests share.
    Feed(CoreSubscription<SopsSubscribeRunsResult>),
    /// One frame, then the socket closes: the in-process socket's `disabled`
    /// or `error` frame.
    Ended(Value),
}

/// Open the core's run feed for one socket. [`CoreCall::subscribe`] owns
/// the subscription from before the request is sent, so a caller that stops
/// waiting for the result (an abandoned handshake) leaves no feed behind,
/// and it listens to the connection first, so a change that arrives ahead
/// of the result is kept. The two refusals the in-process socket reports as
/// frames, the SOP subsystem disabled and an engine failure, become that
/// frame; any other refusal is the caller's error.
pub(crate) async fn subscribe(core: CoreCall) -> Result<Opened, CoreError> {
    match core
        .subscribe::<SopsSubscribeRunsResult>(Method::SopsSubscribeRuns, json!({}))
        .await
    {
        Ok(subscription) => Ok(Opened::Feed(subscription)),
        Err(CoreError::Rpc(error)) if error.code == INTERNAL_ERROR => {
            let disabled = error
                .data
                .as_ref()
                .and_then(|data| data.get("reason"))
                .and_then(Value::as_str)
                == Some(error_reasons::SOP_DISABLED);
            Ok(Opened::Ended(if disabled {
                json!({ "type": "disabled" })
            } else {
                json!({ "type": "error", "error": error.message })
            }))
        }
        Err(error) => Err(error),
    }
}

/// `GET /ws/sops/runs` through the core, the socket every router serves for
/// it: the in-process socket's frames, from `sops/subscribe-runs`.
///
/// The subscription is opened before the upgrade, so a refused credential,
/// a missing grant or an unreachable core answers as an HTTP error, as every
/// other route does.
pub(crate) async fn sop_runs_through_core(
    core: CoreCall,
    ws: WebSocketUpgrade,
) -> Result<Response, CoreError> {
    let opened = subscribe(core).await?;
    Ok(ws
        .on_upgrade(move |socket| relay_from_core(socket, opened))
        .into_response())
}

async fn relay_from_core(socket: WebSocket, opened: Opened) {
    let (mut sender, mut receiver) = socket.split();
    let text = |frame: Value| Message::Text(frame.to_string().into());
    let mut subscription = match opened {
        Opened::Ended(frame) => {
            let _ = sender.send(text(frame)).await;
            return;
        }
        Opened::Feed(subscription) => subscription,
    };
    let id = subscription.id().to_owned();
    // The core connection ended and the feed with it: close, so the client
    // reconnects and subscribes again.
    let closed = subscription.closed();
    tokio::pin!(closed);

    let runs = std::mem::take(&mut subscription.opened.runs);
    if sender
        .send(text(json!({ "type": "snapshot", "runs": runs })))
        .await
        .is_err()
    {
        return;
    }
    loop {
        let frame = tokio::select! {
            message = receiver.next() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => continue,
            },
            received = subscription.notifications.recv() => match received {
                Ok(notification) => match frame_for(&notification, &id) {
                    Some(frame) => frame,
                    None => continue,
                },
                // Notifications on this connection were dropped before this
                // socket read them, and some may have been this
                // subscription's: report a lag so the client resyncs.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    json!({ "type": "lagged", "missed": missed })
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            () = &mut closed => {
                let _ = sender.send(Message::Close(None)).await;
                return;
            }
        };
        if sender.send(text(frame)).await.is_err() {
            return;
        }
    }
}

/// This subscription's frame for a core notification, in the in-process
/// socket's shape, or `None` when the notification is not about it.
fn frame_for(notification: &Notification, subscription_id: &str) -> Option<Value> {
    match notification.method.as_str() {
        SOPS_RUN_CHANGED => {
            let changed: SopRunChanged =
                serde_json::from_value(notification.params.clone()).ok()?;
            (changed.subscription_id == subscription_id)
                .then(|| json!({ "type": "run", "run": changed.run }))
        }
        SUBSCRIPTION_LAGGED => {
            let lagged: SubscriptionLagged =
                serde_json::from_value(notification.params.clone()).ok()?;
            (lagged.subscription_id == subscription_id).then(|| {
                json!({
                    "type": "lagged",
                    "missed": lagged.resume_seq.saturating_sub(lagged.from_seq),
                })
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(method: &str, params: Value) -> Notification {
        Notification {
            method: method.into(),
            params,
        }
    }

    #[test]
    fn only_this_subscriptions_notifications_become_frames() {
        let run = json!({"run_id": "r1", "status": "running"});
        assert_eq!(
            frame_for(
                &notification(
                    SOPS_RUN_CHANGED,
                    json!({"subscription_id": "mine", "seq": 4, "run": run}),
                ),
                "mine",
            ),
            Some(json!({"type": "run", "run": run}))
        );
        assert_eq!(
            frame_for(
                &notification(
                    SUBSCRIPTION_LAGGED,
                    json!({"subscription_id": "mine", "from_seq": 5, "resume_seq": 8}),
                ),
                "mine",
            ),
            Some(json!({"type": "lagged", "missed": 3}))
        );
        // Another subscription on the same connection, another kind of
        // notification, and a malformed one are none of this socket's.
        for other in [
            notification(
                SOPS_RUN_CHANGED,
                json!({"subscription_id": "theirs", "seq": 1, "run": run}),
            ),
            notification(
                SUBSCRIPTION_LAGGED,
                json!({"subscription_id": "theirs", "from_seq": 1, "resume_seq": 2}),
            ),
            notification("logs/event", json!({"subscription_id": "mine", "seq": 1})),
            notification(SOPS_RUN_CHANGED, json!({"subscription_id": "mine"})),
        ] {
            assert_eq!(frame_for(&other, "mine"), None, "{other:?}");
        }
    }
}
