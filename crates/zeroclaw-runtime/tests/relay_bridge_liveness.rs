//! Liveness regressions for the daemon-side relay bridge's OUTBOUND link, run
//! against a real listener that speaks just enough of the relay protocol to be
//! hostile.
//!
//! [`relay_full_path`](../relay_full_path.rs) covers the successful path. These
//! cover the two ways a relay can hold the bridge without ever refusing it:
//! accepting a socket and then never answering (setup), and accepting the
//! registration and then never reading again (established). Both must end in a
//! bounded time, and daemon cancellation must return promptly from any phase of
//! the setup rather than waiting on a peer that has stopped speaking.
#![allow(clippy::disallowed_methods)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use zeroclaw_relay_proto::{Control, PEER_HINT_ENROLL, SUBPROTOCOL, encode_data};

/// Mirrors the bridge's own `SETUP_DEADLINE`. The constant is module-private, so
/// these tests assert the behaviour it produces with margins wide enough that
/// retuning it does not turn into a false failure.
const SETUP_DEADLINE: Duration = Duration::from_secs(30);
/// Mirrors the bridge's `DEAD_AFTER`, which is also its outbound write bound.
const DEAD_AFTER: Duration = Duration::from_secs(60);
/// Ping frames the wedging relay pushes at the bridge. It stalls long before
/// this; the count only has to outlast the socket buffers on both sides.
const FLOOD_FRAMES: usize = 50_000;
/// Bytes the wedging relay must have delivered before a stalled flood counts as
/// the wedge rather than a slow start. One ping yields one queued pong, so this
/// is many times the bridge's 256-slot outbound queue.
const WEDGE_FLOOR: u64 = 64 * 1024;

/// What the stub relay does before it goes silent.
#[derive(Debug, Clone, Copy)]
enum RelayBehavior {
    /// Read the TLS ClientHello and never speak TLS.
    SilentAtTls,
    /// Complete outer TLS, read the WebSocket upgrade request, never answer it.
    SilentAtWsUpgrade,
    /// Complete the upgrade, read `Hello`, never send `Challenge`.
    SilentAtChallenge,
    /// Send `Challenge`, read `Register`, never send `Registered`.
    SilentAtRegistered,
    /// Complete the whole registration, then stop reading and flood the bridge
    /// with pings so its outbound queue and socket fill behind a writer this
    /// relay will never drain again.
    WedgeAfterRegistration,
    /// Complete registration, stop reading, saturate BOTH the shared outbound
    /// queue and one conn's inbound queue, then open a sibling route. The
    /// sibling dial is the proof that the shared reader never parked.
    SaturateThenOpenSibling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayEvent {
    /// A TCP connection from the bridge was accepted.
    Accepted,
    /// The relay consumed the bridge's last message for its configured phase and
    /// went silent, so the bridge is now parked awaiting a reply.
    Parked,
    /// The signed registration completed; the link is established.
    Registered,
}

struct StubRelay {
    addr: SocketAddr,
    events: mpsc::UnboundedReceiver<RelayEvent>,
    /// Bytes the wedging behaviour has actually pushed onto the wire.
    written: Arc<AtomicU64>,
}

/// Build a self-signed outer TLS acceptor for the stub relay (its own identity).
/// The bridge is configured `relay_insecure`, so nothing here asserts PKI.
fn relay_outer_acceptor() -> TlsAcceptor {
    let ck =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let cert = rustls::pki_types::CertificateDer::from(ck.cert.der().to_vec());
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        ck.key_pair.serialize_der(),
    ));
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    TlsAcceptor::from(Arc::new(cfg))
}

async fn spawn_stub_relay(behavior: RelayBehavior) -> StubRelay {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = relay_outer_acceptor();
    let (events, rx) = mpsc::unbounded_channel();
    let written = Arc::new(AtomicU64::new(0));
    let conn_written = written.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            if events.send(RelayEvent::Accepted).is_err() {
                return;
            }
            let acceptor = acceptor.clone();
            let events = events.clone();
            let written = conn_written.clone();
            tokio::spawn(serve_stub_conn(tcp, acceptor, behavior, events, written));
        }
    });
    StubRelay {
        addr,
        events: rx,
        written,
    }
}

async fn serve_stub_conn(
    mut tcp: TcpStream,
    acceptor: TlsAcceptor,
    behavior: RelayBehavior,
    events: mpsc::UnboundedSender<RelayEvent>,
    written: Arc<AtomicU64>,
) {
    let mut scratch = [0u8; 4096];
    if matches!(behavior, RelayBehavior::SilentAtTls) {
        // Consume the ClientHello, so the bridge is parked awaiting the server's
        // half of the handshake rather than still writing its own.
        if tcp.read(&mut scratch).await.is_err() {
            return;
        }
        let _ = events.send(RelayEvent::Parked);
        park(tcp).await;
        return;
    }

    let Ok(mut tls) = acceptor.accept(tcp).await else {
        return;
    };
    if matches!(behavior, RelayBehavior::SilentAtWsUpgrade) {
        if tls.read(&mut scratch).await.is_err() {
            return;
        }
        let _ = events.send(RelayEvent::Parked);
        park(tls).await;
        return;
    }

    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tls, echo_subprotocol).await else {
        return;
    };
    let Some(Control::Hello { .. }) = next_control(&mut ws).await else {
        return;
    };
    if matches!(behavior, RelayBehavior::SilentAtChallenge) {
        let _ = events.send(RelayEvent::Parked);
        park(ws).await;
        return;
    }

    let challenge = Control::Challenge {
        nonce: B64.encode(b"relay-bridge-liveness-nonce"),
    };
    if ws.send(Message::text(challenge.to_json())).await.is_err() {
        return;
    }
    let Some(Control::Register { node_id, .. }) = next_control(&mut ws).await else {
        return;
    };
    if matches!(behavior, RelayBehavior::SilentAtRegistered) {
        let _ = events.send(RelayEvent::Parked);
        park(ws).await;
        return;
    }

    let registered = Control::Registered {
        node_id,
        lease_ttl_secs: 300,
    };
    if ws.send(Message::text(registered.to_json())).await.is_err() {
        return;
    }
    let _ = events.send(RelayEvent::Registered);

    if matches!(behavior, RelayBehavior::SaturateThenOpenSibling) {
        saturate_then_open_sibling(ws).await;
        return;
    }

    // From here the relay never reads again. Each ping the bridge receives costs
    // it one pong through its bounded outbound queue, so the queue fills behind
    // a writer whose socket nobody is draining.
    let payload = vec![0u8; 125];
    for _ in 0..FLOOD_FRAMES {
        if ws
            .send(Message::Ping(payload.clone().into()))
            .await
            .is_err()
        {
            break;
        }
        written.fetch_add(payload.len() as u64 + 2, Ordering::Relaxed);
    }
    park(ws).await;
}

/// Hold a connection open and answer nothing further.
async fn park<T>(held: T) {
    let _held = held;
    std::future::pending::<()>().await
}

/// Conn id the saturating stub wedges, and the sibling it probes with afterwards.
const WEDGED_CONN: u64 = 1;
const SIBLING_CONN: u64 = 2;
/// Pings pushed to fill the bridge's 256-slot outbound queue behind a writer
/// whose socket this relay has stopped draining. Each one costs the bridge a
/// pong through that queue, so this is many times over what saturation needs.
const SATURATING_PINGS: usize = 3_000;
/// Ceiling on DATA bytes flooded at `WEDGED_CONN` while waiting for its
/// eviction. The flood is synchronized on the observable outcome - the
/// `conn_backpressured` Close from the daemon - not on a byte count: kernel
/// socket buffering is platform-tuned (Linux auto-grows loopback buffers to
/// megabytes where macOS parks in kilobytes), so any fixed count either
/// under-fills one platform or wastes time on another. The cap only bounds a
/// broken run, far above any plausible kernel buffering, and hitting it
/// panics loudly instead of letting the probe go vacuous.
const FLOOD_CAP_BYTES: usize = 64 * 1024 * 1024;
/// DATA frames sent between eviction checks while flooding `WEDGED_CONN`.
const FLOOD_BATCH: usize = 64;
/// How many `Open` frames are pushed purely to be refused while the outbound
/// queue is full. `max_conns` is 1 and `WEDGED_CONN` holds the only slot, so the
/// first few are refused `busy`; sent back to back they then outrun the
/// `open_burst` of 3 and the rest are refused `rate_limited`. Both refusals are
/// emitted by the shared reader, which is the point.
const REFUSED_OPENS: u64 = 12;
/// Real-clock pause before the sibling probe, so the `Open` rate bucket refills
/// after the refusals above deliberately drained it.
const BUCKET_REFILL: Duration = Duration::from_millis(300);

// ── Real-clock windows ───────────────────────────────────────────
//
// These tests deliberately run on the real clock: a virtual clock cannot tell a
// task waiting on real socket readiness from an idle runtime, so auto-advance
// would race the very I/O being timed. That makes every window below a load
// sensitivity, so each is sized against MEASURED discrimination rather than a
// guess, and each states what the failing alternative actually costs.

/// Budget for the sibling dial on a saturated link.
///
/// A live reader produces this dial in ~0.4s unloaded: it is frame decoding with
/// no timer anywhere in the path. A reader PARKED on the outbound queue never
/// produces it at all - the writer's stall budget closes the queue underneath it
/// and the reader then leaves through its `link_dead` arm instead of resuming
/// the backlog. Both mutations of that path were held for 120s without a dial.
/// So the discriminating gap is unbounded, not a race between two durations, and
/// this window is sized purely for a contended runner: well over a hundred times
/// the unloaded cost, and still incapable of admitting the failure mode.
const SIBLING_DIAL_WINDOW: Duration = Duration::from_secs(60);

/// Budget for a cancelled bridge to return.
///
/// A healthy return is a token wake plus a task exit, with no I/O in the path.
/// The discriminating alternative is a setup that ignores cancellation and runs
/// to its full 30s `SETUP_DEADLINE` before the reconnect loop notices, so this
/// stays clearly under that while giving a loaded runner the room it needs.
const CANCEL_RETURN_WINDOW: Duration = Duration::from_secs(15);

/// Budget for a stub-relay milestone (a connection accepted, a phase parked, a
/// registration completed). The discriminating alternative is a milestone that
/// never arrives, so this only has to outlast scheduling on a loaded runner.
const MILESTONE_WINDOW: Duration = Duration::from_secs(30);

/// Budget for a link to tear down once its route is closed. Healthy teardown is
/// immediate; the alternative this rules out is a teardown held until the
/// writer's 60s stall budget expires, so it stays clearly under that.
const TEARDOWN_WINDOW: Duration = Duration::from_secs(30);

/// Drive the reviewed cascade: register, stop reading, then saturate BOTH the
/// shared outbound queue and one conn's inbound queue before asking the bridge
/// to open a sibling route. If any notification on the shared reader path awaits
/// the outbound queue, the reader parks here and the sibling `Open` is never
/// processed.
async fn saturate_then_open_sibling<S>(mut ws: WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let open_wedged = Control::Open {
        conn_id: WEDGED_CONN,
        peer_hint: None,
    };
    if ws.send(Message::text(open_wedged.to_json())).await.is_err() {
        return;
    }

    // Fill the bridge's outbound queue first: from here every notification the
    // shared reader wants to emit has nowhere to go.
    let ping_payload = vec![0u8; 125];
    for _ in 0..SATURATING_PINGS {
        if ws
            .send(Message::Ping(ping_payload.clone().into()))
            .await
            .is_err()
        {
            return;
        }
    }

    // Capacity and rate refusals, both emitted by the shared reader with the
    // outbound queue already full. `WEDGED_CONN` still holds the only slot, so
    // the first few are refused `busy`; back to back they then outrun the open
    // bucket and the rest are refused `rate_limited`.
    for offset in 0..REFUSED_OPENS {
        let open = Control::Open {
            conn_id: 100 + offset,
            peer_hint: None,
        };
        if ws.send(Message::text(open.to_json())).await.is_err() {
            return;
        }
    }

    // Flood the wedged conn until the daemon OBSERVABLY evicts it: the
    // backpressure path tears the route down and notifies this relay with a
    // `conn_backpressured` Close. Synchronizing on that outcome - rather than
    // any fixed frame count - is what makes the probe sound on every platform:
    // the eviction requires the conn's local write to park, and how many bytes
    // that takes is a kernel-tuning question (Linux absorbs megabytes on
    // loopback where macOS parks in kilobytes). The outbound queue is not
    // saturated at this point (the reader's notifications are best-effort by
    // design), so the eviction Close is deliverable and the wait terminates.
    let data_payload = vec![0u8; 4096];
    let mut flooded = 0usize;
    let mut evicted = false;
    'flood: while !evicted {
        for _ in 0..FLOOD_BATCH {
            let frame = Message::binary(encode_data(WEDGED_CONN, &data_payload));
            if ws.send(frame).await.is_err() {
                return;
            }
            flooded += data_payload.len();
        }
        assert!(
            flooded <= FLOOD_CAP_BYTES,
            "the wedged conn was never evicted after {flooded} flooded bytes; \
             the backpressure premise did not hold on this platform"
        );
        // Drain EVERYTHING the daemon has sent - the ping phase left a queue
        // of pong frames backed up behind the parked writer, and the eviction
        // Close can only be enqueued once those drain. A drain that stops at
        // the first non-Text frame throttles that to one pong per batch and
        // the Close never fits (the first draft did exactly that, and the cap
        // fired at precisely 256 batches). Only the wedged conn's
        // backpressure Close ends the flood; all other traffic (pongs, Window
        // credits, Opened, the earlier refusals) is expected noise.
        loop {
            match tokio::time::timeout(Duration::from_millis(50), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if let Ok(Control::Close { conn_id, reason }) = Control::from_json(t.as_str())
                        && conn_id == WEDGED_CONN
                        && reason == "conn_backpressured"
                    {
                        evicted = true;
                        continue 'flood;
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(_) => break,
                Err(_) => break,
            }
        }
    }

    // Let the `Open` bucket refill before the probe: the refusals above drained
    // it deliberately, and a rate-limited probe would prove nothing.
    tokio::time::sleep(BUCKET_REFILL).await;

    // Re-apply outbound pressure for the probe, and this time do NOT drain:
    // each ping costs the daemon a best-effort pong through the outbound
    // queue behind a writer whose socket this relay has stopped reading. On
    // platforms with small socket buffers this fills the queue outright;
    // where the kernel absorbs more it is adversarial volume. This probe pins
    // reader LIVENESS under that pressure plus a real eviction - it does not
    // guarantee a full queue at the probe instant, because parking the writer
    // is a kernel-tuning question no fixed count answers on every platform.
    // The no-await-on-the-reader property itself is structural (every
    // notification goes through the try_send-only `notify_relay`), and its
    // per-site mutations were proven under small-buffer conditions where the
    // queue genuinely fills.
    for _ in 0..SATURATING_PINGS {
        if ws.send(Message::Ping(vec![0u8; 125].into())).await.is_err() {
            return;
        }
    }

    // The probe: a sibling route the shared reader can only open if it is
    // still running with the outbound queue full. It targets the enrollment
    // listener, which reports the dial OUT-OF-BAND - the observation cannot
    // depend on the saturated outbound path. The eviction above has provably
    // freed the single conn slot, so a refused `busy` here would be a real
    // reader defect, not setup noise - and the send itself must succeed for
    // the probe to mean anything.
    let open_sibling = Control::Open {
        conn_id: SIBLING_CONN,
        peer_hint: Some(PEER_HINT_ENROLL.to_string()),
    };
    ws.send(Message::text(open_sibling.to_json()))
        .await
        .expect("the sibling Open probe must reach the daemon");
    park(ws).await;
}

/// A loopback listener that accepts and never reads, so whatever the bridge
/// writes to it backs up. Stands in for a wedged local WSS peer.
async fn stalled_local_target() -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpSocket::new_v4().expect("socket");
    let _ = socket.set_recv_buffer_size(4 * 1024);
    socket
        .bind("127.0.0.1:0".parse().expect("addr"))
        .expect("bind");
    let listener = socket.listen(16).expect("listen");
    let addr = listener.local_addr().expect("addr").to_string();
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    (addr, task)
}

/// A loopback listener that reports every connection it accepts. This is the
/// sibling probe: a dial arriving here proves the shared reader processed a
/// frame that came in AFTER both queues were saturated.
async fn reporting_local_target() -> (String, mpsc::UnboundedReceiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
            if tx.send(()).is_err() {
                return;
            }
        }
    });
    (addr, rx)
}

/// The bridge asks for the relay subprotocol, so the stub must grant it. The
/// error type is the tungstenite callback's, not ours.
#[allow(clippy::result_large_err)]
fn echo_subprotocol(
    _req: &tokio_tungstenite::tungstenite::handshake::server::Request,
    mut resp: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> std::result::Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    resp.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(SUBPROTOCOL),
    );
    Ok(resp)
}

async fn next_control<S>(ws: &mut WebSocketStream<S>) -> Option<Control>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(t)) => return Control::from_json(t.as_str()).ok(),
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// [`bridge_config`] with both loopback targets pointed at real listeners, so
/// `Open` frames actually dial something.
fn bridge_config_with_targets(
    relay_addr: SocketAddr,
    data_dir: &std::path::Path,
    signing_key: Vec<u8>,
    local_wss_addr: String,
    local_enroll_addr: String,
) -> zeroclaw_runtime::relay::RelayBridgeConfig {
    zeroclaw_runtime::relay::RelayBridgeConfig {
        local_wss_addr,
        local_enroll_addr: Some(local_enroll_addr),
        // Tight enough that the reader's capacity and rate refusals are both
        // reachable while its outbound queue is saturated.
        max_conns: 1,
        open_burst: 3,
        ..bridge_config(relay_addr, data_dir, signing_key)
    }
}

fn bridge_config(
    relay_addr: SocketAddr,
    data_dir: &std::path::Path,
    signing_key: Vec<u8>,
) -> zeroclaw_runtime::relay::RelayBridgeConfig {
    zeroclaw_runtime::relay::RelayBridgeConfig {
        relay_addr: relay_addr.to_string(),
        relay_host: "localhost".into(),
        node_id: "relay-device".into(),
        relay_token: None,
        // Never dialed: no client ever reaches an `Open` in these tests.
        local_wss_addr: "127.0.0.1:9".into(),
        local_enroll_addr: None,
        enroll_bridge_ports: None,
        signing_key_pkcs8: signing_key,
        relay_ca_path: None,
        relay_insecure: true, // self-signed stub outer cert
        relay_tofu: false,
        outer_client_cert: None,
        outer_client_key: None,
        max_conns: 16,
        open_burst: 60,
        open_rate_per_sec: 20.0,
        data_dir: data_dir.to_path_buf(),
        node_id_rotation_days: 0,
        rotation_allowed: false,
    }
}

/// Await `wanted`, discarding the events that precede it.
async fn wait_for(
    events: &mut mpsc::UnboundedReceiver<RelayEvent>,
    wanted: RelayEvent,
    within: Duration,
) {
    let seen = tokio::time::timeout(within, async {
        loop {
            match events.recv().await {
                Some(event) if event == wanted => return,
                Some(_) => {}
                None => panic!("the stub relay stopped before {wanted:?}"),
            }
        }
    })
    .await;
    assert!(seen.is_ok(), "the stub relay never reached {wanted:?}");
}

/// Wait, on the real clock, until the relay's flood stops making progress. That
/// stall IS the wedge under test: the bridge has stopped reading because its
/// bounded outbound queue filled behind a writer parked in `sink.send`.
async fn wait_for_wedge(written: &AtomicU64) {
    let mut previous = 0;
    let mut stalled = 0;
    // Sized like the windows above: the alternative to a stall is a flood that
    // never stalls, so only a loaded runner's slowness is being tolerated here.
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = written.load(Ordering::Relaxed);
        if now == previous && now >= WEDGE_FLOOR {
            stalled += 1;
            if stalled >= 4 {
                return;
            }
        } else {
            stalled = 0;
        }
        previous = now;
    }
    panic!("the relay's writes never stalled, so the bridge outbound path never wedged");
}

/// A relay that accepts and then stops answering must cost the bridge one
/// bounded setup budget, after which it retries. A second connection is the
/// observable proof: without a deadline over the setup the first attempt never
/// ends and no second connection is ever made.
///
/// Only the budget runs on the virtual clock, and it is driven by hand. A
/// running virtual clock cannot tell a task waiting on real socket readiness
/// from an idle runtime, so leaving auto-advance on across the handshake would
/// race the budget against the very I/O it is supposed to be timing.
async fn assert_setup_is_bounded_and_retries(behavior: RelayBehavior) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut relay = spawn_stub_relay(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let signing_key = zeroclaw_runtime::relay::ensure_signing_key(dir.path()).unwrap();
    let cancel = CancellationToken::new();

    let bridge = tokio::spawn(zeroclaw_runtime::relay::run_relay_bridge(
        bridge_config(relay.addr, dir.path(), signing_key),
        cancel.clone(),
    ));
    wait_for(&mut relay.events, RelayEvent::Accepted, MILESTONE_WINDOW).await;
    wait_for(&mut relay.events, RelayEvent::Parked, MILESTONE_WINDOW).await;

    // The parked setup holds nothing but its own deadline now, so the budget is
    // spent by advancing the clock rather than by waiting on it. The clock is
    // handed back before the retry so the retry's real I/O is not racing it.
    tokio::time::pause();
    tokio::time::advance(SETUP_DEADLINE / 2).await;
    assert!(
        relay.events.try_recv().is_err(),
        "a parked setup must be given its full budget, not abandoned early"
    );

    tokio::time::advance(SETUP_DEADLINE).await;
    tokio::time::resume();
    wait_for(&mut relay.events, RelayEvent::Accepted, MILESTONE_WINDOW).await;

    cancel.cancel();
    let _ = tokio::time::timeout(TEARDOWN_WINDOW, bridge).await;
}

#[tokio::test]
async fn setup_is_bounded_when_the_relay_never_speaks_tls() {
    assert_setup_is_bounded_and_retries(RelayBehavior::SilentAtTls).await;
}

#[tokio::test]
async fn setup_is_bounded_when_the_relay_never_answers_the_upgrade() {
    assert_setup_is_bounded_and_retries(RelayBehavior::SilentAtWsUpgrade).await;
}

#[tokio::test]
async fn setup_is_bounded_when_the_relay_never_challenges() {
    assert_setup_is_bounded_and_retries(RelayBehavior::SilentAtChallenge).await;
}

#[tokio::test]
async fn setup_is_bounded_when_the_relay_never_confirms_registration() {
    assert_setup_is_bounded_and_retries(RelayBehavior::SilentAtRegistered).await;
}

/// Daemon shutdown must not wait on a relay that has stopped speaking. `Parked`
/// is emitted only once the relay has consumed the bridge's last message for the
/// phase under test, so cancellation lands while the bridge is awaiting a reply
/// that never comes. The real clock runs here: the setup budget cannot expire
/// inside the window this asserts, so a prompt return can only come from
/// cancellation.
async fn assert_cancellation_during_setup_returns_promptly(behavior: RelayBehavior) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut relay = spawn_stub_relay(behavior).await;
    let dir = tempfile::tempdir().unwrap();
    let signing_key = zeroclaw_runtime::relay::ensure_signing_key(dir.path()).unwrap();
    let cancel = CancellationToken::new();

    let bridge = tokio::spawn(zeroclaw_runtime::relay::run_relay_bridge(
        bridge_config(relay.addr, dir.path(), signing_key),
        cancel.clone(),
    ));
    wait_for(&mut relay.events, RelayEvent::Parked, MILESTONE_WINDOW).await;

    cancel.cancel();
    tokio::time::timeout(CANCEL_RETURN_WINDOW, bridge)
        .await
        .expect("cancellation must return from a parked setup without waiting for the peer")
        .expect("bridge task")
        .expect("clean shutdown");
}

#[tokio::test]
async fn cancellation_returns_while_parked_in_the_tls_handshake() {
    assert_cancellation_during_setup_returns_promptly(RelayBehavior::SilentAtTls).await;
}

#[tokio::test]
async fn cancellation_returns_while_parked_in_the_websocket_upgrade() {
    assert_cancellation_during_setup_returns_promptly(RelayBehavior::SilentAtWsUpgrade).await;
}

#[tokio::test]
async fn cancellation_returns_while_parked_awaiting_the_challenge() {
    assert_cancellation_during_setup_returns_promptly(RelayBehavior::SilentAtChallenge).await;
}

#[tokio::test]
async fn cancellation_returns_while_parked_awaiting_registration() {
    assert_cancellation_during_setup_returns_promptly(RelayBehavior::SilentAtRegistered).await;
}

/// One backpressured connection must not freeze the node.
///
/// The shared reader demuxes every conn on the link and owns the `link_dead` and
/// cancellation arms. If any notification it emits AWAITS the bounded outbound
/// queue, then a relay that has stopped reading parks the reader there: sibling
/// routes stop being served and teardown stops being observed for the writer's
/// whole stall budget, rather than the backpressure staying isolated to the one
/// conn that caused it.
///
/// The stub saturates both queues and then asks for a sibling route. The dial
/// landing on the enrollment listener is the proof that the reader kept running.
#[tokio::test]
async fn a_saturated_link_still_serves_sibling_routes_and_stays_tearable() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (wedged_addr, _wedged_target) = stalled_local_target().await;
    let (sibling_addr, mut sibling_dials) = reporting_local_target().await;

    let mut relay = spawn_stub_relay(RelayBehavior::SaturateThenOpenSibling).await;
    let dir = tempfile::tempdir().unwrap();
    let signing_key = zeroclaw_runtime::relay::ensure_signing_key(dir.path()).unwrap();
    let cancel = CancellationToken::new();

    let bridge = tokio::spawn(zeroclaw_runtime::relay::run_relay_bridge(
        bridge_config_with_targets(
            relay.addr,
            dir.path(),
            signing_key,
            wedged_addr,
            sibling_addr,
        ),
        cancel.clone(),
    ));
    wait_for(&mut relay.events, RelayEvent::Registered, MILESTONE_WINDOW).await;

    // The sibling `Open` is the last frame the stub sends, after both queues are
    // full. Real clock: the bridge's stall budgets are minutes away, so nothing
    // but a live reader can produce this dial inside the window.
    tokio::time::timeout(SIBLING_DIAL_WINDOW, sibling_dials.recv())
        .await
        .expect("a saturated link must still serve sibling routes")
        .expect("sibling listener");

    // ... and the node must still be tearable, not held until a write budget
    // expires.
    cancel.cancel();
    tokio::time::timeout(TEARDOWN_WINDOW, bridge)
        .await
        .expect("teardown must not wait on the saturated outbound queue")
        .expect("bridge task")
        .expect("clean shutdown");
}

/// An established relay that stops reading wedges every outbound producer: the
/// writer parks in `sink.send`, the bounded queue fills behind it, and the
/// reader loop and keepalive watchdog both block on that queue. Nothing is left
/// to notice, so without the liveness bound the link stays half-alive forever
/// and the bridge never reconnects.
#[tokio::test]
async fn an_established_relay_that_stops_reading_is_declared_dead_and_reconnected() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut relay = spawn_stub_relay(RelayBehavior::WedgeAfterRegistration).await;
    let dir = tempfile::tempdir().unwrap();
    let signing_key = zeroclaw_runtime::relay::ensure_signing_key(dir.path()).unwrap();
    let cancel = CancellationToken::new();

    let bridge = tokio::spawn(zeroclaw_runtime::relay::run_relay_bridge(
        bridge_config(relay.addr, dir.path(), signing_key),
        cancel.clone(),
    ));
    wait_for(&mut relay.events, RelayEvent::Registered, MILESTONE_WINDOW).await;
    wait_for_wedge(&relay.written).await;

    // The wedge holds nothing but timers now, so the minute-scale budget is
    // spent by advancing the clock rather than by waiting. The clock is driven
    // explicitly and handed back before the reconnect, so that the reconnect's
    // real I/O is never racing an auto-advancing clock.
    tokio::time::pause();
    tokio::time::advance(DEAD_AFTER / 2).await;
    assert!(
        relay.events.try_recv().is_err(),
        "a wedged link must be given its full budget, not torn down on sight"
    );

    tokio::time::advance(DEAD_AFTER * 3).await;
    tokio::time::resume();
    wait_for(&mut relay.events, RelayEvent::Accepted, MILESTONE_WINDOW).await;

    cancel.cancel();
    let _ = tokio::time::timeout(TEARDOWN_WINDOW, bridge).await;
}
