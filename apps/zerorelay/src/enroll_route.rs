//! An enrollment route the RELAY itself originates against a registered daemon.
//!
//! Every other route on this relay is a splice: a client asks for a node, the
//! relay pairs it with that daemon's link, and the relay never becomes a party to
//! the inner exchange. This module is the one deliberate exception, and it exists
//! only for the browser frontdoor.
//!
//! A browser cannot speak the daemon's enrollment protocol. That endpoint is TLS
//! (`crates/zeroclaw-runtime/src/enroll/mod.rs` accepts every connection through
//! `TlsAcceptor::accept`), and the daemon-side relay bridge splices DATA frames
//! into it as raw bytes, so what travels the tunnel for native enrollment is a
//! TLS record stream. Producing that stream in a page means shipping a TLS
//! implementation in relay-served JavaScript - the design this frontdoor
//! deliberately does NOT bring back.
//!
//! So the relay terminates instead: it opens this route, performs the enrollment
//! exchange as a real TLS client (see [`crate::enroll_proxy`]), and hands the
//! result to the page. That makes the relay a PRINCIPAL in browser enrollment
//! rather than a blind forwarder, which is the whole reason `[frontdoor]` is
//! off by default and warns loudly when enabled. zerocode/native enrollment does
//! not pass through here and is unaffected.
//!
//! What this module does NOT change: routing is still by node-id through the
//! normal registry, and the route obeys the same per-node connect budget, the
//! same `max_conns_per_node` cap, the same pairing deadline, and the same
//! credit-based flow control as any client route.

use crate::{
    ConnEvent, ConnRoute, DAEMON_HANDOFF_BUDGET, Inner, LiveConnGuard, PAIR_TIMEOUT, release_conn,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use zeroclaw_relay_proto::{
    ConnWindow, Control, INITIAL_WINDOW, MAX_DATA_PAYLOAD, PEER_HINT_ENROLL, encode_data,
};

/// Buffer between the TLS client above and the relay pump below. One credit
/// window each way, so the duplex never becomes a second, unaccounted queue in
/// front of the window the daemon actually granted.
const ROUTE_BUFFER_BYTES: usize = INITIAL_WINDOW as usize;

/// Why a route could not be opened. These are the same refusals a WebSocket
/// client would receive as `Control::error` frames; the frontdoor turns them
/// into HTTP status codes instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// No daemon is registered under that node-id.
    NoSuchNode,
    /// The per-node connect budget is exhausted.
    RateLimited,
    /// The node is at `max_conns_per_node`.
    Busy,
    /// The daemon never accepted the `Open` within [`PAIR_TIMEOUT`].
    NotAccepted,
}

impl OpenError {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoSuchNode => "no_such_node",
            Self::RateLimited => "rate_limited",
            Self::Busy => "busy",
            Self::NotAccepted => "daemon_did_not_accept",
        }
    }
}

/// Aborts the pump when the route is dropped.
///
/// The pump holds the route's `LiveConnGuard` and its entry in the daemon's conn
/// map, so "the caller is finished with this stream" and "the route is torn down"
/// have to be the same event. Without the abort, an enrollment leg that returns
/// early - a TLS failure, a byte cap, an expired budget - would leave the pump
/// parked on `conn_rx` holding a live conn against `max_conns_per_node`.
struct PumpGuard(tokio::task::JoinHandle<()>);

impl Drop for PumpGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One relay-originated enrollment route: a byte stream to the daemon's
/// enrollment endpoint, plus the pump that keeps it alive.
pub(crate) struct EnrollRoute {
    stream: DuplexStream,
    _pump: PumpGuard,
}

impl EnrollRoute {
    /// Split into the stream to hand to a TLS client and the guard that must be
    /// held for as long as that stream is in use. Mirrors the shape zerocode's
    /// `dial_enrollment_through_relay(..).split()` returns, so the leg helpers
    /// read the same on both sides.
    pub(crate) fn split(self) -> (DuplexStream, impl Send) {
        (self.stream, self._pump)
    }
}

/// Open a route to `node_id`'s enrollment endpoint on this relay's own behalf.
///
/// Mirrors `handle_client`'s admission sequence exactly - registry lookup,
/// per-node connect budget, conn cap, `Open` with the enrollment peer hint, and
/// the pairing deadline - because a relay-originated route must not be able to
/// skip a limit that a WebSocket client would have been held to.
pub(crate) async fn open_enroll_route(
    inner: &Arc<Inner>,
    node_id: &str,
) -> Result<EnrollRoute, OpenError> {
    let handle = {
        let daemons = inner.daemons.lock().await;
        daemons.get(node_id).map(|h| {
            (
                h.to_daemon.clone(),
                h.conns.clone(),
                h.metrics.clone(),
                h.connect_bucket.clone(),
            )
        })
    };
    let Some((to_daemon, conns, metrics, connect_bucket)) = handle else {
        return Err(OpenError::NoSuchNode);
    };

    // Per-node connect cap (A6). The frontdoor is an unauthenticated HTTP
    // surface, so without this a browser could drive enrollment attempts at a
    // daemon faster than a relay client ever could.
    if !connect_bucket.lock().await.try_take() {
        metrics.connects_rejected.fetch_add(1, Ordering::Relaxed);
        return Err(OpenError::RateLimited);
    }

    let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
    let (conn_tx, mut conn_rx) = mpsc::channel::<ConnEvent>(256);
    let (cancel_tx, cancelled) = tokio::sync::oneshot::channel::<()>();
    {
        let mut cs = conns.lock().await;
        if cs.len() >= inner.max_conns_per_node {
            return Err(OpenError::Busy);
        }
        cs.insert(
            conn_id,
            ConnRoute {
                events: conn_tx,
                _cancel_on_drop: cancel_tx,
            },
        );
    }
    let live = LiveConnGuard::new(metrics);

    if !matches!(
        tokio::time::timeout(
            DAEMON_HANDOFF_BUDGET,
            to_daemon.send(Message::text(
                Control::Open {
                    conn_id,
                    peer_hint: Some(PEER_HINT_ENROLL.to_string()),
                }
                .to_json(),
            )),
        )
        .await,
        Ok(Ok(()))
    ) {
        conns.lock().await.remove(&conn_id);
        return Err(OpenError::NoSuchNode);
    }

    let paired = tokio::time::timeout(PAIR_TIMEOUT, async {
        while let Some(ev) = conn_rx.recv().await {
            match ev {
                ConnEvent::Opened => return true,
                ConnEvent::Close(..) => return false,
                ConnEvent::Data(_) | ConnEvent::Window(_) | ConnEvent::Ack(_) => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if !paired {
        let _ = release_conn(&to_daemon, &conns, conn_id).await;
        return Err(OpenError::NotAccepted);
    }

    let (proxy_io, relay_io) = tokio::io::duplex(ROUTE_BUFFER_BYTES);
    let pump = tokio::spawn(pump_route(
        conn_id, relay_io, conn_rx, to_daemon, conns, live, cancelled,
    ));

    Ok(EnrollRoute {
        stream: proxy_io,
        _pump: PumpGuard(pump),
    })
}

/// Move bytes between the local TLS client and the daemon link under the same
/// credit discipline every other relay client follows.
///
/// The relay never originates credit for a route it is splicing, but here it IS
/// the client: it grants the daemon a receive window up front, debits its own
/// send window per DATA frame, and acknowledges what it has drained. A client
/// that ignored this would be torn down by the daemon bridge after one window
/// (the browser frontdoor's predecessor did exactly that).
#[allow(clippy::too_many_arguments)]
async fn pump_route(
    conn_id: u64,
    mut relay_io: DuplexStream,
    mut conn_rx: mpsc::Receiver<ConnEvent>,
    to_daemon: mpsc::Sender<Message>,
    conns: Arc<tokio::sync::Mutex<crate::ConnRoutes>>,
    _live: LiveConnGuard,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
) {
    let mut send_window = ConnWindow::new(INITIAL_WINDOW);
    let mut recv_drained: u32 = 0;

    // Grant the daemon our receive window up front, exactly as a native client
    // does before its first byte.
    if tokio::time::timeout(
        DAEMON_HANDOFF_BUDGET,
        to_daemon.send(Message::text(
            Control::Window {
                conn_id,
                credit: INITIAL_WINDOW,
            }
            .to_json(),
        )),
    )
    .await
    .is_err()
    {
        let _ = release_conn(&to_daemon, &conns, conn_id).await;
        return;
    }

    let mut buf = vec![0u8; MAX_DATA_PAYLOAD];
    loop {
        tokio::select! {
            // The route left the daemon's conn map (link death, supersede,
            // backpressure shedding): stop, even mid-write.
            _ = &mut cancelled => break,
            // Window exhausted: stop reading the TLS side so backpressure
            // reaches it, instead of queueing past the daemon's grant.
            n = relay_io.read(&mut buf), if !send_window.is_blocked() => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    send_window.debit(n);
                    if tokio::time::timeout(
                        DAEMON_HANDOFF_BUDGET,
                        to_daemon.send(Message::binary(encode_data(conn_id, &buf[..n]))),
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
            },
            ev = conn_rx.recv() => match ev {
                Some(ConnEvent::Data(payload)) => {
                    if relay_io.write_all(&payload).await.is_err() {
                        break;
                    }
                    recv_drained = recv_drained.saturating_add(payload.len() as u32);
                    // Amortize acks at half a window, matching the native client.
                    if recv_drained >= INITIAL_WINDOW / 2 {
                        if tokio::time::timeout(
                            DAEMON_HANDOFF_BUDGET,
                            to_daemon.send(Message::text(
                                Control::DataAck {
                                    conn_id,
                                    consumed: recv_drained,
                                }
                                .to_json(),
                            )),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        recv_drained = 0;
                    }
                }
                Some(ConnEvent::Window(credit)) => send_window.set(credit),
                Some(ConnEvent::Ack(consumed)) => send_window.ack(consumed),
                // A graceful close carries a drain handle; dropping it lets the
                // relay's teardown proceed once we have taken the tail above.
                Some(ConnEvent::Close(..)) | None => break,
                Some(ConnEvent::Opened) => {}
            }
        }
    }

    // Half-close so the TLS client above observes EOF rather than a stall, then
    // give the daemon its half back.
    let _ = relay_io.shutdown().await;
    let _ = release_conn(&to_daemon, &conns, conn_id).await;
}
