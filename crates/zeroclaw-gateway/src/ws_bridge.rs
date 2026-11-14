//! `/ws/bridge`: the control socket of a channel bridge.
//!
//! A bridge (a separate process relaying a messaging platform) keeps one
//! control socket open. The gateway sends proactive messages queued in the
//! bridge outbox (cron output, heartbeat alerts, the `notify` tool) as
//! `deliver {id, to, thread_id?, content}` frames, and the bridge answers
//! `delivered {id}` once the platform accepted the message; a confirmed
//! receipt remains in the outbox. Socket handoff alone is never confirmation.
//!
//! - The bridge is identified by its token (`[gateway.bridges.<name>]`),
//!   never by a query parameter. The token is always required, whatever
//!   `require_pairing` says, and paired tokens are not accepted.
//! - Accepted rows are evaluated against live attention policy on every poll.
//!   Attempted rows with no receipt are unknown and never blindly resent.
//! - One control socket per bridge: a new connection closes the old one.

use super::AppState;
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_infra::bridge_outbox::BridgeOutbox;

/// The sub-protocol echoed when the client offers it.
const BRIDGE_PROTOCOL: &str = "zeroclaw.bridge.v2";
/// Close code sent to a control socket that a newer connection replaced.
const CLOSE_REPLACED: u16 = 4000;
/// How often a socket re-reads the outbox without a local wake-up; picks up
/// rows written by other processes (for example `zeroclaw cron run`).
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Rows read per outbox query.
const BATCH: usize = 100;
/// A missing receipt closes the socket without claiming subsequent candidates.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// The live control socket of each bridge.
#[derive(Default)]
pub struct BridgeSockets {
    next_id: AtomicU64,
    live: Mutex<HashMap<String, (u64, CancellationToken)>>,
}

impl BridgeSockets {
    /// Register a new socket for `bridge`, closing the previous one.
    fn claim(
        &self,
        bridge: &str,
        outbox: &BridgeOutbox,
    ) -> anyhow::Result<(u64, CancellationToken)> {
        let mut live = self.live.lock();
        // Recover only with no registered socket, under the same lock used to
        // install a replacement. Recovery cannot rewrite a new active attempt.
        if !live.contains_key(bridge) {
            outbox.mark_unknown(bridge)?;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        if let Some((_, old)) = live.insert(bridge.to_string(), (id, token.clone())) {
            old.cancel();
        }
        Ok((id, token))
    }

    /// A delayed old socket owns only its attempted ID, never a replacement's.
    fn release(
        &self,
        bridge: &str,
        id: u64,
        outbox: &BridgeOutbox,
        in_flight: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut live = self.live.lock();
        let outcome = match in_flight {
            Some(candidate) => outbox.mark_attempt_unknown(bridge, candidate),
            None => Ok(()),
        };
        if live.get(bridge).is_some_and(|(current, _)| *current == id) {
            live.remove(bridge);
        }
        outcome
    }
}

#[derive(serde::Deserialize)]
pub struct BridgeQuery {
    pub token: Option<String>,
}

/// GET /ws/bridge — a bridge's control socket.
pub async fn handle_ws_bridge(
    State(state): State<AppState>,
    Query(query): Query<BridgeQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = crate::ws::extract_ws_token(&headers, query.token.as_deref()).unwrap_or("");
    let (bridge, data_dir) = {
        let config = state.config.read();
        let bridge = config
            .gateway
            .bridge_for_token(token)
            .filter(|(_, bridge)| !bridge.scoped())
            .map(|(name, _)| name.to_string());
        (bridge, config.data_dir.clone())
    };
    let Some(bridge) = bridge else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "bridge control socket refused: no bridge token matched"
        );
        return (
            StatusCode::UNAUTHORIZED,
            "Unauthorized: /ws/bridge needs a bridge token (zeroclaw gateway bridge add <name>)",
        )
            .into_response();
    };
    let outbox = match BridgeOutbox::shared(&data_dir) {
        Ok(outbox) => outbox,
        Err(e) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "bridge": bridge,
                        "error": format!("{e:#}"),
                    })),
                "bridge outbox unavailable"
            );
            return (StatusCode::SERVICE_UNAVAILABLE, "bridge outbox unavailable").into_response();
        }
    };
    let offers_protocol = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == BRIDGE_PROTOCOL));
    if !offers_protocol {
        return (StatusCode::UPGRADE_REQUIRED, "bridge_protocol_v2_required").into_response();
    }
    let ws = ws.protocols([BRIDGE_PROTOCOL]);
    let sockets = Arc::clone(&state.bridge_sockets);
    ws.on_upgrade(move |socket| run_control_socket(socket, bridge, outbox, sockets, state))
        .into_response()
}

async fn run_control_socket(
    socket: WebSocket,
    bridge: String,
    outbox: Arc<BridgeOutbox>,
    sockets: Arc<BridgeSockets>,
    state: AppState,
) {
    let (conn_id, replaced) = match sockets.claim(&bridge, &outbox) {
        Ok(claim) => claim,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_attrs(serde_json::json!({"error": error.to_string()})),
                "attention_recovery_failed"
            );
            return;
        }
    };
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Connect)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({"bridge": bridge})),
        "bridge control socket attached"
    );
    let mut in_flight = None;
    let shutdown = state.shutdown_tx.subscribe();
    let reason = tokio::select! {
        biased;
        () = crate::gateway_shutdown(shutdown) => "gateway shutdown",
        reason = serve(socket, &bridge, &outbox, &replaced, &state, conn_id, &mut in_flight) => reason,
    };
    // Always release the connection and mark an unacknowledged attempt unknown,
    // including shutdown while a send is suspended. Never requeue it as accepted.
    if let Err(error) = sockets.release(
        &bridge,
        conn_id,
        &outbox,
        in_flight.as_ref().map(|(id, _)| id.as_str()),
    ) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_attrs(serde_json::json!({"error": error.to_string()})),
            "attention_mark_unknown_failed"
        );
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Disconnect)
            .with_attrs(::serde_json::json!({"bridge": bridge, "reason": reason})),
        "bridge control socket detached"
    );
}

/// Drive one control socket until it closes; returns why it ended.
async fn serve(
    socket: WebSocket,
    bridge: &str,
    outbox: &BridgeOutbox,
    replaced: &CancellationToken,
    state: &AppState,
    conn_id: u64,
    in_flight: &mut Option<(String, tokio::time::Instant)>,
) -> &'static str {
    let (mut sender, mut receiver) = socket.split();
    // Subscribe before the first read so a write that lands between the
    // read and the wait still wakes this socket.
    let mut changed = outbox.subscribe();
    if outbox.purge_expired().is_err() {
        return "attention_store_unavailable";
    }
    let start = serde_json::json!({ "type": "bridge_start", "bridge": bridge });
    if sender
        .send(Message::Text(start.to_string().into()))
        .await
        .is_err()
    {
        return "send failed";
    }

    // Each poll scans accepted rows again: a deferred low sequence must not
    // disappear behind a later delivered row. Claims prevent duplicate sends.
    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if let Some((id, _)) = in_flight.as_ref() {
            match outbox.is_resolved(bridge, id) {
                Ok(true) => *in_flight = None,
                Ok(false) => {}
                Err(_) => return "attention_store_unavailable",
            }
        }
        if in_flight.is_none() {
            match send_pending(&mut sender, bridge, outbox, state, conn_id).await {
                Ok(Some(id)) => {
                    *in_flight = Some((id, tokio::time::Instant::now() + RECEIPT_TIMEOUT))
                }
                Ok(None) => {}
                Err(reason) => return reason,
            }
        }
        let deadline = in_flight
            .as_ref()
            .map(|(_, deadline)| *deadline)
            .unwrap_or_else(|| tokio::time::Instant::now() + RECEIPT_TIMEOUT);
        tokio::select! {
            _ = tokio::time::sleep_until(deadline), if in_flight.is_some() => {
                return "attention_receipt_timeout";
            }
            () = replaced.cancelled() => {
                let _ = sender
                    .send(Message::Close(Some(CloseFrame {
                        code: CLOSE_REPLACED,
                        reason: Utf8Bytes::from_static("replaced by a newer connection"),
                    })))
                    .await;
                return "replaced";
            }
            message = receiver.next() => match message {
                Some(Ok(Message::Text(text))) => on_client_frame(bridge, outbox, &text),
                Some(Ok(Message::Close(_)) | Err(_)) | None => return "closed by the bridge",
                Some(Ok(_)) => {}
            },
            result = changed.changed() => {
                if result.is_err() {
                    return "outbox closed";
                }
            }
            _ = poll.tick() => {}
        }
    }
}

type Sender = futures_util::stream::SplitSink<WebSocket, Message>;

async fn send_pending(
    sender: &mut Sender,
    bridge: &str,
    outbox: &BridgeOutbox,
    state: &AppState,
    conn_id: u64,
) -> Result<Option<String>, &'static str> {
    outbox
        .purge_expired()
        .map_err(|_| "attention_store_unavailable")?;
    let mut after_seq = 0;
    loop {
        let rows = match outbox.pending(bridge, after_seq, BATCH) {
            Ok(rows) => rows,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Receive)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "bridge": bridge,
                            "error": format!("{e:#}"),
                        })),
                    "reading the bridge outbox failed"
                );
                return Err("attention_store_unavailable");
            }
        };
        let full = rows.len() == BATCH;
        for row in rows {
            after_seq = row.seq;
            let now = chrono::Utc::now();
            // Hold the canonical config read lock through the durable claim;
            // a policy update cannot slip between evaluation and reservation.
            let claimed = {
                // Serialize generation validation and durable reservation with
                // replacement. A cancelled old socket cannot claim new work.
                let live = state.bridge_sockets.live.lock();
                if live.get(bridge).is_none_or(|(id, _)| *id != conn_id) {
                    return Err("replaced");
                }
                let config = state.config.read();
                let permitted = crate::attention::permits(
                    config.gateway.attention.as_ref(),
                    bridge,
                    &row.to,
                    &row.source_kind,
                    &row.source_id,
                    now,
                )
                .map_err(|_| "attention_invalid_policy")?;
                permitted
                    && crate::review_notifications::permits_delivery(&config, outbox, &row)
                    && crate::delegation_notifications::permits_delivery(&config, outbox, &row)
                    && crate::reminder_notifications::permits_delivery(&config, outbox, &row)
                    && crate::reminder_notifications::claim(
                        &config.data_dir,
                        outbox,
                        &row,
                        now.timestamp(),
                    )?
            };
            if !claimed {
                continue;
            }
            let mut frame = serde_json::json!({
                "type": "deliver",
                "id": row.id,
                "to": row.to,
                "content": row.content,
            });
            if let Some(thread_id) = row.thread_id {
                frame["thread_id"] = serde_json::Value::String(thread_id);
            }
            if !matches!(
                tokio::time::timeout(
                    RECEIPT_TIMEOUT,
                    sender.send(Message::Text(frame.to_string().into()))
                )
                .await,
                Ok(Ok(()))
            ) {
                return Err("send failed");
            }
            outbox
                .mark_sent(bridge, &row.id)
                .map_err(|_| "attention_store_unavailable")?;
            // Do not reserve buffered frames that the bridge cannot attempt
            // until this one's platform result is known.
            return Ok(Some(row.id));
        }
        if !full {
            return Ok(None);
        }
    }
}

fn on_client_frame(bridge: &str, outbox: &BridgeOutbox, text: &str) {
    let Ok(frame) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if frame["type"] != "delivered" {
        return;
    }
    let Some(id) = frame["id"].as_str() else {
        return;
    };
    match outbox.ack(bridge, id) {
        Ok(acked) => {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "bridge": bridge,
                        "id": id,
                        "was_queued": acked,
                    })),
                "bridge acknowledged a delivery"
            );
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "bridge": bridge,
                        "id": id,
                        "error": format!("{e:#}"),
                    })),
                "recording a bridge acknowledgement failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use zeroclaw_config::schema::GatewayBridgeConfig;
    use zeroclaw_gateway_client::{BridgeClient, Deliver, Rejected};
    use zeroclaw_runtime::security::pairing::PairingGuard;

    const TOKEN: &str = "zcb_test_bridge_token";
    const WAIT: Duration = Duration::from_secs(10);

    fn bridge_state(tmp: &tempfile::TempDir, require_pairing: bool) -> AppState {
        let state = crate::tests::admin_paircode_state(tmp, require_pairing, false);
        state.config.write().gateway.bridges.insert(
            "tg".into(),
            GatewayBridgeConfig {
                token_hash: PairingGuard::token_hash(TOKEN),
                session_prefix: Some("tg:".into()),
                sessions: vec!["main".into()],
                owner: None,
                device: None,
            },
        );
        state
    }

    async fn serve(state: AppState) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/ws/bridge", axum::routing::get(handle_ws_bridge))
            .route("/ws/chat", axum::routing::get(crate::ws::handle_ws_chat))
            .with_state(state);
        zeroclaw_spawn::spawn!(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        format!("ws://{addr}")
    }

    fn outbox(state: &AppState) -> Arc<BridgeOutbox> {
        BridgeOutbox::shared(&state.config.read().data_dir).unwrap()
    }

    async fn next(client: &mut BridgeClient) -> Option<Deliver> {
        tokio::time::timeout(WAIT, client.next_deliver())
            .await
            .expect("a frame in time")
            .unwrap()
    }

    async fn refusal(gateway: &str, token: &str) -> u16 {
        let err = BridgeClient::connect(gateway, token).await.err().unwrap();
        err.downcast_ref::<Rejected>().expect("refused").status
    }

    #[tokio::test]
    async fn replaced_generation_cannot_claim_through_send_pending() {
        use tokio::io::AsyncWriteExt;
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        let candidate = outbox.enqueue("tg", "42", None, "still accepted").unwrap();
        let (old, _) = state.bridge_sockets.claim("tg", &outbox).unwrap();
        let (current, _) = state.bridge_sockets.claim("tg", &outbox).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let handler_state = state.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(move |ws: WebSocketUpgrade| {
                let state = handler_state.clone();
                let tx = tx.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        let outbox = self::outbox(&state);
                        let (mut sender, _) = socket.split();
                        let stale = send_pending(&mut sender, "tg", &outbox, &state, old).await;
                        let accepted = outbox.pending("tg", 0, BATCH).unwrap().len();
                        let active =
                            send_pending(&mut sender, "tg", &outbox, &state, current).await;
                        tx.send((stale, accepted, active)).await.unwrap();
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server =
            zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await.unwrap() });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").as_bytes()).await.unwrap();
        let (stale, accepted, active) = tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stale, Err("replaced"));
        assert_eq!(
            accepted, 1,
            "old generation must leave the candidate accepted"
        );
        assert_eq!(active, Ok(Some(candidate.clone())));
        assert_eq!(
            outbox.inspect("tg", &candidate).unwrap().unwrap()["delivery_state"],
            "sent"
        );
        server.abort();
    }

    #[test]
    fn delayed_old_socket_cleanup_cannot_change_replacement_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = BridgeOutbox::open(tmp.path()).unwrap();
        let sockets = BridgeSockets::default();
        let x = outbox.enqueue("tg", "42", None, "X").unwrap();
        let y = outbox.enqueue("tg", "42", None, "Y").unwrap();
        let (old, old_cancel) = sockets.claim("tg", &outbox).unwrap();
        assert!(
            outbox
                .claim("tg", &x, chrono::Utc::now().timestamp())
                .unwrap()
        );
        outbox.mark_sent("tg", &x).unwrap();
        let (new, _) = sockets.claim("tg", &outbox).unwrap();
        assert!(old_cancel.is_cancelled());
        assert!(
            outbox
                .claim("tg", &y, chrono::Utc::now().timestamp())
                .unwrap()
        );
        outbox.mark_sent("tg", &y).unwrap();
        // Deterministic late teardown: new socket already handed off Y.
        sockets.release("tg", old, &outbox, Some(&x)).unwrap();
        assert_eq!(
            outbox.inspect("tg", &x).unwrap().unwrap()["delivery_state"],
            "unknown"
        );
        assert_eq!(
            outbox.inspect("tg", &y).unwrap().unwrap()["delivery_state"],
            "sent"
        );
        assert_eq!(sockets.live.lock().get("tg").unwrap().0, new);
        let (newer, _) = sockets.claim("tg", &outbox).unwrap();
        assert_eq!(
            outbox.inspect("tg", &y).unwrap().unwrap()["delivery_state"],
            "sent",
            "replacement startup must not recover another socket's attempt"
        );
        sockets.release("tg", new, &outbox, Some(&y)).unwrap();
        sockets.release("tg", newer, &outbox, None).unwrap();
        let z = outbox
            .enqueue("tg", "42", None, "orphan after process death")
            .unwrap();
        assert!(
            outbox
                .claim("tg", &z, chrono::Utc::now().timestamp())
                .unwrap()
        );
        outbox.mark_sent("tg", &z).unwrap();
        sockets.claim("tg", &outbox).unwrap();
        assert_eq!(
            outbox.inspect("tg", &z).unwrap().unwrap()["delivery_state"],
            "unknown"
        );
    }

    #[tokio::test]
    async fn the_control_socket_needs_a_bridge_token_even_without_pairing() {
        let tmp = tempfile::tempdir().unwrap();
        let gateway = serve(bridge_state(&tmp, false)).await;
        assert_eq!(refusal(&gateway, "").await, 401);
        assert_eq!(refusal(&gateway, "zc_paired_or_anything").await, 401);
        let client = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        assert_eq!(client.bridge(), "tg");
    }

    #[tokio::test]
    async fn old_or_unversioned_bridge_control_is_refused_before_delivery() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        let id = outbox.enqueue("tg", "42", None, "waiting").unwrap();
        let gateway = serve(state).await;
        let addr = gateway.trim_start_matches("ws://");
        for protocol in ["", "Sec-WebSocket-Protocol: zeroclaw.bridge.v1\r\n"] {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let request = format!(
                "GET /ws/bridge HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nAuthorization: Bearer {TOKEN}\r\n{protocol}\r\n"
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = [0u8; 1024];
            let n = stream.read(&mut response).await.unwrap();
            assert_eq!(
                String::from_utf8_lossy(&response[..n])
                    .split_whitespace()
                    .nth(1),
                Some("426")
            );
        }
        assert_eq!(
            outbox.inspect("tg", &id).unwrap().unwrap()["delivery_state"],
            "accepted"
        );
    }

    #[tokio::test]
    async fn lost_first_receipt_leaves_later_candidate_accepted_for_reconnect() {
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        let first_id = outbox.enqueue("tg", "42", None, "first").unwrap();
        let second_id = outbox.enqueue("tg", "42", Some("5"), "second").unwrap();
        let gateway = serve(state).await;
        let mut client = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        assert_eq!(next(&mut client).await.unwrap().id, first_id);
        // An unrelated/forged receipt cannot release the current attempt.
        client.ack(&second_id).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), client.next_deliver())
                .await
                .is_err()
        );
        assert_eq!(
            outbox.inspect("tg", &second_id).unwrap().unwrap()["delivery_state"],
            "accepted"
        );
        // Refusal or a lost response closes the control socket without an ack.
        client.close().await.unwrap();
        let mut again = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        let second = next(&mut again).await.unwrap();
        assert_eq!(second.id, second_id);
        assert_eq!(second.thread_id.as_deref(), Some("5"));
        assert_eq!(
            outbox.inspect("tg", &first_id).unwrap().unwrap()["delivery_state"],
            "unknown"
        );
        again.ack(&second.id).await.unwrap();
        let third = outbox.enqueue("tg", "42", None, "third").unwrap();
        assert_eq!(next(&mut again).await.unwrap().id, third);
        let fourth = outbox.enqueue("tg", "42", None, "fourth").unwrap();
        let item = outbox.inspect("tg", &third).unwrap().unwrap();
        outbox
            .owner_action(
                "tg",
                "42",
                item["source_kind"].as_str().unwrap(),
                item["source_id"].as_str().unwrap(),
                &third,
                "dismiss",
                None,
                "owner",
            )
            .unwrap();
        assert_eq!(next(&mut again).await.unwrap().id, fourth);
    }

    fn queue_review_notice(state: &AppState) -> String {
        let data_dir = state.config.read().data_dir.clone();
        {
            let mut config = state.config.write();
            config.agents.insert(
                "nova".into(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    enabled: true,
                    ..Default::default()
                },
            );
            config.companion_memory.review_notification =
                Some(zeroclaw_config::companion::ReviewNotificationConfig {
                    bridge: "tg".into(),
                    recipient: "42".into(),
                    thread_id: None,
                });
        }
        let now = chrono::Utc::now().timestamp() as u64;
        zeroclaw_memory::companion::SoulProfileStore::shared(&data_dir)
            .unwrap()
            .record_reflection(
                "nova",
                &zeroclaw_memory::companion::SoulReflectionReceipt {
                    period_from_unix: now - 604800,
                    messages_read: 2,
                    proposals_created: 1,
                    user_model_candidates_created: 1,
                    outcome: "ok".into(),
                    ran_at_unix: now,
                    drift: None,
                },
            )
            .unwrap();
        crate::review_notifications::reconcile(
            &state.config,
            &data_dir,
            &state.path_prefix,
            now,
            &CancellationToken::new(),
        )
        .unwrap();
        outbox(state)
            .pending("tg", 0, 100)
            .unwrap()
            .into_iter()
            .find(|row| row.source_kind == "weekly_review")
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn weekly_review_live_opt_out_and_target_change_hold_old_unclaimed_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        let original = queue_review_notice(&state);
        let target = state
            .config
            .write()
            .companion_memory
            .review_notification
            .take()
            .unwrap();
        let gateway = serve(state.clone()).await;
        let mut client = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.next_deliver())
                .await
                .is_err()
        );
        state.config.write().companion_memory.review_notification =
            Some(zeroclaw_config::companion::ReviewNotificationConfig {
                recipient: "99".into(),
                ..target.clone()
            });
        let data_dir = state.config.read().data_dir.clone();
        crate::review_notifications::reconcile(
            &state.config,
            &data_dir,
            &state.path_prefix,
            chrono::Utc::now().timestamp() as u64,
            &CancellationToken::new(),
        )
        .unwrap();
        let new_target = next(&mut client).await.unwrap();
        assert_ne!(new_target.id, original);
        assert_eq!(new_target.to, "99");
        assert_eq!(
            outbox.inspect("tg", &original).unwrap().unwrap()["delivery_state"],
            "accepted"
        );
        client.ack(&new_target.id).await.unwrap();
        state.config.write().companion_memory.review_notification = Some(target);
        outbox.enqueue("tg", "42", None, "wake").unwrap();
        assert_eq!(next(&mut client).await.unwrap().id, original);
    }

    #[tokio::test]
    async fn quiet_lower_sequence_is_revisited_after_live_policy_change() {
        use zeroclaw_config::attention::{AttentionConfig, ImportantSource};
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        state.config.write().gateway.attention = Some(AttentionConfig {
            timezone: "America/New_York".into(),
            quiet_start: "00:00".into(),
            quiet_end: "00:00".into(),
            important_sources: vec![ImportantSource {
                bridge: "tg".into(),
                recipient: "42".into(),
                source_kind: "cron".into(),
                source_id: "urgent-job".into(),
            }],
        });
        let quiet = queue_review_notice(&state);
        // Cross a read-batch boundary; deferred candidates must not starve
        // an eligible later source or become permanently skipped.
        for n in 0..BATCH {
            outbox
                .enqueue_source(
                    "tg",
                    "42",
                    None,
                    "quiet",
                    "cron",
                    "quiet-job",
                    &format!("extra-{n}"),
                )
                .unwrap();
        }
        let urgent = outbox
            .enqueue_source("tg", "42", None, "urgent", "cron", "urgent-job", "run1")
            .unwrap();
        let gateway = serve(state.clone()).await;
        let mut client = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        assert_eq!(next(&mut client).await.unwrap().id, urgent);
        assert_eq!(
            outbox.inspect("tg", &quiet).unwrap().unwrap()["delivery_state"],
            "accepted"
        );
        client.ack(&urgent).await.unwrap();
        state.config.write().gateway.attention = None;
        // New write wakes the same socket; no restart or config snapshot.
        outbox.enqueue("tg", "42", None, "wake").unwrap();
        assert_eq!(next(&mut client).await.unwrap().id, quiet);
    }

    #[tokio::test]
    async fn a_new_control_socket_replaces_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        let gateway = serve(state).await;

        let mut old = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        let mut new = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        assert!(next(&mut old).await.is_none(), "the old socket is closed");
        outbox.enqueue("tg", "42", None, "to the new one").unwrap();
        assert_eq!(next(&mut new).await.unwrap().content, "to the new one");
    }

    #[tokio::test]
    async fn a_bridge_token_opens_only_its_chat_sessions() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let tmp = tempfile::tempdir().unwrap();
        let gateway = serve(bridge_state(&tmp, true)).await;
        let addr = gateway.trim_start_matches("ws://").to_string();
        let status = |query: &'static str, token: &'static str| {
            let addr = addr.clone();
            async move {
                let mut stream = tokio::net::TcpStream::connect(&addr).await.unwrap();
                let request = format!(
                    "GET /ws/chat?{query} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\n\
                     Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                     Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                     Authorization: Bearer {token}\r\n\r\n"
                );
                stream.write_all(request.as_bytes()).await.unwrap();
                let mut buf = vec![0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap();
                String::from_utf8_lossy(&buf[..n])
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse::<u16>().ok())
                    .unwrap_or(0)
            }
        };
        // In scope: auth passes and the request fails later, on the agent.
        assert_eq!(status("agent=nobody&session_id=main", TOKEN).await, 400);
        assert_eq!(status("agent=nobody&session_id=tg:-100", TOKEN).await, 400);
        // Out of scope, or no session at all: refused.
        assert_eq!(status("agent=nobody&session_id=work", TOKEN).await, 403);
        assert_eq!(status("agent=nobody", TOKEN).await, 403);
        // Neither a bridge nor a paired token.
        assert_eq!(
            status("agent=nobody&session_id=main", "zcb_nope").await,
            401
        );
    }
    #[tokio::test]
    async fn gateway_shutdown_releases_bridge_and_marks_inflight_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let state = bridge_state(&tmp, true);
        let outbox = outbox(&state);
        let candidate = outbox.enqueue("tg", "42", None, "shutdown test").unwrap();
        let gateway = serve(state.clone()).await;
        let mut client = BridgeClient::connect(&gateway, TOKEN).await.unwrap();
        assert!(next(&mut client).await.is_some());
        state.shutdown_tx.send(true).unwrap();
        // Shutdown retires the transport, including a send that cannot finish
        // a WebSocket close handshake. EOF must not become another delivery.
        let ended = tokio::time::timeout(WAIT, client.next_deliver())
            .await
            .unwrap();
        assert!(!matches!(ended, Ok(Some(_))));
        tokio::time::timeout(WAIT, async {
            while !state.bridge_sockets.live.lock().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            outbox.inspect("tg", &candidate).unwrap().unwrap()["delivery_state"],
            "unknown"
        );
        assert!(outbox.pending("tg", 0, BATCH).unwrap().is_empty());
    }
}
