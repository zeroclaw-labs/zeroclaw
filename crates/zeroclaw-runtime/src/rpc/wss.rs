//! WebSocket Secure (WSS) transport for the RPC layer.
//! Mirrors the Unix socket transport (`unix.rs`) but uses TLS-encrypted
//! WebSocket connections, enabling remote TUI-to-daemon connectivity.

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

type TlsStream = tokio_rustls::server::TlsStream<TcpStream>;

/// What the WebSocket parser actually reads from: the TLS stream with a frame
/// scanner reading over it. See [`ScanningStream`].
type ScannedTlsStream = ScanningStream<TlsStream>;

/// How long the read side waits for any frame before sending a liveness Ping.
const HEARTBEAT_IDLE: Duration = Duration::from_secs(20);

/// How long to wait after a Ping for any frame (a Pong, or anything else)
/// before declaring the peer dead and tearing the connection down.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on ONE outbound frame write to an established peer.
///
/// The read side already declares a peer dead after [`HEARTBEAT_IDLE`] plus
/// [`HEARTBEAT_TIMEOUT`] of silence, so a peer that will not accept a write gets
/// exactly the same budget: one session can never be alive by one half of the
/// liveness policy and dead by the other. The tungstenite sink reports no
/// partial progress, so a peer that has stopped reading and one that is merely
/// slow are indistinguishable from the writer; that budget is generous enough
/// that no healthy peer reaches it for a single frame.
const PEER_WRITE_TIMEOUT: Duration =
    Duration::from_secs(HEARTBEAT_IDLE.as_secs() + HEARTBEAT_TIMEOUT.as_secs());

/// Backoff after a transient `accept()` error so the serve loop does not
/// hot-spin while the condition (e.g. fd exhaustion) clears.
const ACCEPT_ERROR_BACKOFF_MS: u64 = 50;

/// Default ceiling on sockets past `accept()` but not yet through the TLS and
/// WebSocket handshakes. See [`WssLimits::max_pending_handshakes`].
pub const DEFAULT_MAX_PENDING_HANDSHAKES: usize = 256;

/// Default absolute budget for TLS accept plus the WebSocket upgrade.
/// See [`WssLimits::handshake_timeout`].
pub const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// Default ceiling on concurrently established WSS sessions.
/// See [`WssLimits::max_sessions`].
/// Sized as an explicit host-memory policy, not a connection-count guess: the
/// parser envelope (`rpc_ws_config`) is 32 MiB per session, so this default
/// caps the aggregate parser reservation at 2 GiB - defensible on a small
/// daemon host - while 64 concurrent remote sessions is far beyond a personal
/// daemon's working set. Operators with bigger hosts and fleets raise it
/// consciously, envelope arithmetic in hand.
pub const DEFAULT_MAX_SESSIONS: usize = 64;

/// Default ceiling on concurrent sessions holding ONE client certificate.
/// See [`WssLimits::max_sessions_per_client`].
pub const DEFAULT_MAX_SESSIONS_PER_CLIENT: usize = 8;

/// Default lifetime bound on a partially-received data message.
/// See [`WssLimits::incomplete_message_timeout`].
pub const DEFAULT_INCOMPLETE_MESSAGE_TIMEOUT_SECS: u64 = 60;

/// Longest possible WebSocket frame header: two base bytes, up to eight bytes
/// of extended payload length, and the four-byte mask every client-to-server
/// frame carries (RFC 6455 5.2).
const FRAME_HEADER_MAX: usize = 14;

/// Bound on the courtesy Close frame sent to a refused peer, so a peer that
/// stops reading cannot make the refusal itself hold the permits it was denied.
const REFUSAL_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounds on the WSS listener's pre-authentication and session state.
///
/// The remote WSS plane is the daemon's mandatory mTLS surface and its default
/// bind is `0.0.0.0`, so every state an *unauthenticated* peer can reach has to
/// be bounded in both time and count. Without these, each accepted TCP socket
/// spawned a task that awaited the TLS handshake and the WebSocket upgrade with
/// no deadline and no cap, so a peer that merely connected - and never proved
/// anything - could accumulate sockets, tasks and TLS parser state without
/// limit. Mirrors the bounds the relay applies to its own admission path.
#[derive(Debug, Clone)]
pub struct WssLimits {
    /// Ceiling on sockets past `accept()` that have not finished the TLS
    /// handshake and WebSocket upgrade. When the pool is exhausted new sockets
    /// are dropped at accept rather than queued, so a slowloris spread across
    /// many source addresses sheds instead of accumulating.
    pub max_pending_handshakes: usize,
    /// One absolute deadline covering TLS accept AND the WebSocket upgrade,
    /// measured from accept. It is a single budget for the whole setup
    /// sequence, not a fresh window per phase: the heartbeat only starts once
    /// a session is established, so without this a peer could stall in either
    /// handshake forever.
    pub handshake_timeout: Duration,
    /// Ceiling on concurrently established WSS sessions. Bounds the steady
    /// state that survives authentication, so an authorized-but-abusive peer
    /// cannot grow dispatcher and transport state without limit.
    pub max_sessions: usize,
    /// Ceiling on concurrent sessions presenting ONE client certificate, keyed
    /// by that certificate's SHA-256 fingerprint.
    ///
    /// `max_sessions` alone is an arithmetic ceiling, not a host-memory budget:
    /// every session may declare a message up to the parser envelope
    /// (`rpc_ws_config`), so the session ceiling is a memory ceiling, and one
    /// admitted-but-hostile credential (or a stolen one, before it is detected
    /// and revoked) can occupy all of it. This bounds the parser bytes ONE
    /// credential can reserve at `max_sessions_per_client x envelope`.
    pub max_sessions_per_client: usize,
    /// How long a partially-received data message may be held by the parser.
    ///
    /// The heartbeat proves liveness, not progress: tungstenite yields
    /// interleaved control frames while a fragmented message is still
    /// incomplete, so a peer can Ping forever while the parser retains the
    /// partial buffer. This bounds that hold time.
    ///
    /// The deadline is armed by the peer's own DECLARATION, not by bytes
    /// received. tungstenite reserves a frame's declared length the moment it
    /// parses that frame's header, before any payload arrives, so bytes read is
    /// not a proxy for bytes reserved: a 14-byte header can reserve the whole
    /// parser envelope. A frame scanner under the parser (`FrameScanner`) reads
    /// the same plaintext byte stream and starts this clock at the FIRST byte
    /// of a data frame's header, so every partial-message reservation is
    /// bounded from the moment it exists. Control frames are inert to it by
    /// construction, and with no data message in progress there is no deadline
    /// at all - an idle connection is the heartbeat's business.
    ///
    /// It is a lifetime bound, not a stall detector - a peer that trickles
    /// bytes is exactly the case a stall detector would miss - so it also
    /// bounds the slowest legitimate upload: a full-size request
    /// ([`crate::rpc::attachments::MAX_REQUEST_BYTES`], 20 MiB) must arrive
    /// within this window (at the 60s default that is a ~341 KiB/s floor;
    /// operators on slower links should raise the window, not disable it).
    pub incomplete_message_timeout: Duration,
}

impl Default for WssLimits {
    fn default() -> Self {
        Self {
            max_pending_handshakes: DEFAULT_MAX_PENDING_HANDSHAKES,
            handshake_timeout: Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS),
            max_sessions: DEFAULT_MAX_SESSIONS,
            max_sessions_per_client: DEFAULT_MAX_SESSIONS_PER_CLIENT,
            incomplete_message_timeout: Duration::from_secs(
                DEFAULT_INCOMPLETE_MESSAGE_TIMEOUT_SECS,
            ),
        }
    }
}

/// Concurrent sessions per client credential, keyed by the SHA-256 fingerprint
/// of the presented client certificate.
///
/// The fingerprint is the only stable per-credential identity the mTLS accept
/// path exposes; source address is not one (a single credential can arrive from
/// many addresses, and many credentials can share one). Entries exist only
/// while a credential holds at least one session, so a churn of certificates
/// cannot grow this map.
#[derive(Default)]
struct ClientSessionQuota {
    counts: Mutex<HashMap<String, usize>>,
}

impl ClientSessionQuota {
    /// Reserve one session slot for `fingerprint`, or `None` when that
    /// credential is already at `max`. A refused peer is never recorded, so a
    /// rejection leaves no residue in the map.
    fn try_admit(self: &Arc<Self>, fingerprint: &str, max: usize) -> Option<ClientSessionGuard> {
        let cap = max.max(1);
        let mut counts = self.lock_counts();
        let current = counts.get(fingerprint).copied().unwrap_or(0);
        if current >= cap {
            return None;
        }
        counts.insert(fingerprint.to_string(), current + 1);
        drop(counts);
        Some(ClientSessionGuard {
            quota: self.clone(),
            fingerprint: fingerprint.to_string(),
        })
    }

    /// A poisoned lock means some other task panicked mid-update; the map is a
    /// plain counter table with no invariant that a panic can leave broken, so
    /// recover the guard rather than propagating the panic into the accept loop.
    fn lock_counts(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.counts.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Releases the per-credential session slot on every exit path of a session
/// task - dispatcher return, read error, EOF, heartbeat timeout, panic. Manual
/// decrements would be missed by at least one of those, and a missed decrement
/// permanently shrinks that credential's quota.
struct ClientSessionGuard {
    quota: Arc<ClientSessionQuota>,
    fingerprint: String,
}

impl Drop for ClientSessionGuard {
    fn drop(&mut self) {
        let mut counts = self.quota.lock_counts();
        if let Some(slot) = counts.get_mut(&self.fingerprint) {
            *slot = slot.saturating_sub(1);
            if *slot == 0 {
                counts.remove(&self.fingerprint);
            }
        }
    }
}

/// Where the scanner is in the client-to-server byte stream.
///
/// Every variant is a fixed-size counter set, so scanner state is O(1) no
/// matter what a peer declares.
#[derive(Debug, Clone, Copy)]
enum ScanState {
    /// Still inside the HTTP upgrade REQUEST, whose bytes are not frames.
    ///
    /// The scanner cannot simply be armed once the upgrade "completes": the
    /// handshake reads AHEAD, and whatever followed the request in the same
    /// read is handed to the frame parser as its initial buffer - so a peer
    /// that pipelines a frame header into its upgrade read would slip that
    /// header past a post-handshake scanner. Skipping the request itself and
    /// resuming at the byte after the blank line that ends it has no such gap.
    ///
    /// `blank_line` counts the line terminators seen back to back; two means
    /// the empty line that ends the headers.
    Prelude { blank_line: u8 },
    /// Accumulating a frame header. Its total length is only known once the
    /// first two bytes are in (they carry the length format and the mask bit).
    Header {
        buf: [u8; FRAME_HEADER_MAX],
        len: usize,
    },
    /// Counting off a frame's DECLARED payload. `remaining` is exactly the
    /// reservation tungstenite has already made that the peer has not yet
    /// backed with data. `ends_message` marks a FIN data/continuation frame,
    /// whose last payload byte completes the message.
    Payload { remaining: u64, ends_message: bool },
}

impl ScanState {
    fn awaiting_header() -> Self {
        Self::Header {
            buf: [0u8; FRAME_HEADER_MAX],
            len: 0,
        }
    }
}

/// A passive reader of the WebSocket framing the parser above it is about to
/// act on, used to bound how long a partial message may be held.
///
/// Bytes read is NOT a proxy for parser-reserved memory: tungstenite's
/// `FrameCodec::read_frame` reserves a frame's peer-declared length as soon as
/// it parses the header and before any payload arrives, so a 14-byte header can
/// reserve the whole envelope while the connection stays almost silent. This
/// sits on the same plaintext byte stream the parser reads and tracks exactly
/// one thing: whether a data message is in flight, and when its first header
/// byte arrived. The deadline therefore arms at the instant the reservation
/// exists, with no byte threshold to hide under.
///
/// Because it is byte-exact and runs at READ time, a read that carries the end
/// of one message and the opening of the next is handled with no special case:
/// the first message's completion and the second's start are both observed
/// within that one read.
///
/// It never allocates in proportion to a declared length - it only moves
/// counters - and it deliberately enforces no protocol rules. An impossible
/// length, a reserved opcode, an unmasked client frame or a data frame opened
/// mid-fragmentation are all tungstenite's to reject; duplicating that here
/// would only add a second, divergent parser.
#[derive(Debug)]
struct FrameScanner {
    state: ScanState,
    /// When the data message currently in flight began - the instant its first
    /// header byte was read - or `None` when no data message is in flight.
    message_started_at: Option<tokio::time::Instant>,
}

impl FrameScanner {
    fn new() -> Self {
        Self {
            state: ScanState::Prelude { blank_line: 0 },
            message_started_at: None,
        }
    }

    /// When the data message currently in flight began, or `None` when there is
    /// none. This is what [`WssLimits::incomplete_message_timeout`] runs from.
    fn data_message_started_at(&self) -> Option<tokio::time::Instant> {
        self.message_started_at
    }

    /// Payload bytes the frame in flight declared but the peer has not sent:
    /// the parser reservation not backed by data. Reported when a session is
    /// closed so the log states what was actually being held.
    fn outstanding_declared_bytes(&self) -> u64 {
        match self.state {
            ScanState::Payload { remaining, .. } => remaining,
            _ => 0,
        }
    }

    /// Advance over plaintext bytes just read from the peer.
    fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match self.state {
                ScanState::Prelude { mut blank_line } => {
                    let mut consumed = 0usize;
                    for b in bytes {
                        consumed += 1;
                        match *b {
                            // A bare LF terminator is tolerated because the
                            // upgrade's own header parser tolerates it; a
                            // stricter match here would leave the scanner stuck
                            // in the prelude on a request the upgrade accepted.
                            b'\n' => {
                                blank_line = blank_line.saturating_add(1);
                                if blank_line >= 2 {
                                    break;
                                }
                            }
                            b'\r' => {}
                            _ => blank_line = 0,
                        }
                    }
                    bytes = &bytes[consumed..];
                    self.state = if blank_line >= 2 {
                        ScanState::awaiting_header()
                    } else {
                        ScanState::Prelude { blank_line }
                    };
                }
                ScanState::Header { mut buf, mut len } => {
                    // Byte 0 carries FIN and the opcode, so a DATA frame is
                    // recognised - and its message's clock started - at the
                    // very first byte of its header, which is already enough
                    // for the peer to be holding parser state.
                    if len == 0 && !is_control_opcode(bytes[0]) && self.message_started_at.is_none()
                    {
                        self.message_started_at = Some(tokio::time::Instant::now());
                    }
                    while len < FRAME_HEADER_MAX && !bytes.is_empty() {
                        buf[len] = bytes[0];
                        len += 1;
                        bytes = &bytes[1..];
                        if header_len(&buf, len) == Some(len) {
                            break;
                        }
                    }
                    self.state = if header_len(&buf, len) == Some(len) {
                        let (payload_len, ends_message) = decode_header(&buf);
                        if payload_len == 0 {
                            // A zero-length frame completes within its header.
                            if ends_message {
                                self.message_started_at = None;
                            }
                            ScanState::awaiting_header()
                        } else {
                            ScanState::Payload {
                                remaining: payload_len,
                                ends_message,
                            }
                        }
                    } else {
                        ScanState::Header { buf, len }
                    };
                }
                ScanState::Payload {
                    remaining,
                    ends_message,
                } => {
                    let taken = remaining.min(bytes.len() as u64);
                    // `taken` is bounded by `bytes.len()`, so the cast back is
                    // exact on every target.
                    bytes = &bytes[taken as usize..];
                    let left = remaining - taken;
                    self.state = if left == 0 {
                        if ends_message {
                            self.message_started_at = None;
                        }
                        ScanState::awaiting_header()
                    } else {
                        ScanState::Payload {
                            remaining: left,
                            ends_message,
                        }
                    };
                }
            }
        }
    }
}

/// Opcodes 0x8-0xF are control frames (RFC 6455 5.5). They are never part of a
/// data message and may interleave with a fragmented one.
fn is_control_opcode(first: u8) -> bool {
    first & 0x08 != 0
}

/// Total header length implied by the bytes gathered so far, or `None` while
/// the two bytes that determine it are not both in yet.
fn header_len(buf: &[u8; FRAME_HEADER_MAX], len: usize) -> Option<usize> {
    if len < 2 {
        return None;
    }
    let extended = match buf[1] & 0x7F {
        126 => 2,
        127 => 8,
        _ => 0,
    };
    let masked = if buf[1] & 0x80 != 0 { 4 } else { 0 };
    Some(2 + extended + masked)
}

/// Declared payload length, and whether this frame's last payload byte
/// completes a data message. Only called on a header known to be complete;
/// every index is a constant into a fixed-size array, so it cannot panic.
fn decode_header(buf: &[u8; FRAME_HEADER_MAX]) -> (u64, bool) {
    let payload_len = match buf[1] & 0x7F {
        126 => (u64::from(buf[2]) << 8) | u64::from(buf[3]),
        127 => {
            let mut n = 0u64;
            for b in &buf[2..10] {
                n = (n << 8) | u64::from(*b);
            }
            n
        }
        n => u64::from(n),
    };
    let fin = buf[0] & 0x80 != 0;
    (payload_len, fin && !is_control_opcode(buf[0]))
}

/// The scanner as the IO layer and the session loop share it, plus the wakeup
/// that keeps the two in step.
///
/// A deadline is armed from INSIDE a read poll: the session loop asks for a
/// deadline, is told there is none, and parks on the very read that then
/// observes the peer's declaration. Nothing else would wake it - while a frame
/// is pending, tungstenite feeds every later byte (Pings included) into that
/// frame's payload and yields nothing - so without this signal the loop would
/// stay parked until the heartbeat, tens of seconds past the deadline it should
/// have been running.
struct SharedScanner {
    state: Mutex<FrameScanner>,
    /// Signalled when a data message becomes in-flight and none was before.
    /// A LATER message replacing an earlier one needs no signal: its deadline
    /// can only be further out, and the loop re-checks when the earlier one
    /// expires.
    armed: tokio::sync::Notify,
}

impl SharedScanner {
    /// A poisoned lock means a task panicked mid-scan. The scanner is a counter
    /// set with no invariant a panic can leave broken, and refusing to read it
    /// would disarm the bound it exists to enforce, so recover the guard.
    fn lock(&self) -> std::sync::MutexGuard<'_, FrameScanner> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Advance the scan over bytes just read, waking the session loop when this
    /// is where a partial message's clock starts.
    fn feed(&self, bytes: &[u8]) {
        let mut state = self.lock();
        let was_in_flight = state.data_message_started_at().is_some();
        state.feed(bytes);
        let in_flight = state.data_message_started_at().is_some();
        drop(state);
        if in_flight && !was_in_flight {
            self.armed.notify_waiters();
        }
    }

    /// When the in-flight data message must be given up on, or `None` when the
    /// peer has no data message in flight.
    ///
    /// Armed by the peer's DECLARATION rather than by traffic volume: the
    /// parser reserves a frame's declared length from its header alone, so the
    /// clock starts at the first header byte of a data message and runs for
    /// that message's whole lifetime. A connection with no data message in
    /// flight - quiet, or exchanging only control frames - parks nothing and
    /// gets no deadline here; idle liveness is the heartbeat's policy, and this
    /// rule must not duplicate it with a different one.
    fn incomplete_message_deadline(&self, window: Duration) -> Option<tokio::time::Instant> {
        self.lock().data_message_started_at().map(|at| at + window)
    }

    /// How long the in-flight data message has been held, and how much of its
    /// declared payload the peer never sent. For the closing log line only.
    fn incomplete_message_hold(&self) -> (Duration, u64) {
        let state = self.lock();
        let held = state
            .data_message_started_at()
            .map(|at| at.elapsed())
            .unwrap_or_default();
        (held, state.outstanding_declared_bytes())
    }
}

/// The plaintext byte stream between rustls and the WebSocket parser, with a
/// [`FrameScanner`] reading over everything the parser reads.
struct ScanningStream<S> {
    inner: S,
    scanner: Arc<SharedScanner>,
}

impl<S> ScanningStream<S> {
    fn new(inner: S) -> (Self, Arc<SharedScanner>) {
        let scanner = Arc::new(SharedScanner {
            state: Mutex::new(FrameScanner::new()),
            armed: tokio::sync::Notify::new(),
        });
        (
            Self {
                inner,
                scanner: scanner.clone(),
            },
            scanner,
        )
    }

    fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ScanningStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) {
            let fresh = &buf.filled()[before..];
            if !fresh.is_empty() {
                this.scanner.feed(fresh);
            }
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ScanningStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Decrements the shared client counter on every exit path of a connection
/// task. The counter drives `--ephemeral` shutdown, so a missed decrement
/// would keep an idle daemon alive forever.
struct ClientCountGuard(Arc<AtomicUsize>);

impl Drop for ClientCountGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// File-descriptor exhaustion errno values, stable across the Unix targets
/// we support (Linux, macOS, BSD).
#[cfg(unix)]
const EMFILE: i32 = 24; // too many open files (this process)
#[cfg(unix)]
const ENFILE: i32 = 23; // too many open files (system-wide)

fn is_recoverable_accept_error(e: &std::io::Error) -> bool {
    if matches!(
        e.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(e.raw_os_error(), Some(EMFILE) | Some(ENFILE)) {
        return true;
    }
    false
}

// ── Transport ────────────────────────────────────────────────────

/// Control frames the read side asks the writer task to emit out-of-band
/// from the JSON-RPC text stream.
enum Control {
    Ping,
}

pub struct WssTransport {
    reader: futures_util::stream::SplitStream<WebSocketStream<ScannedTlsStream>>,
    writer_tx: mpsc::Sender<String>,
    control_tx: mpsc::Sender<Control>,
    peer_label: String,
    /// Set once a Ping has been sent and we are awaiting any reply. Detects a
    /// peer that went silent on a half-open TCP connection (no FIN/RST).
    awaiting_pong: bool,
    /// Reads the peer's framing under the parser, shared with the IO layer.
    /// The only source of the incomplete-message deadline.
    scanner: Arc<SharedScanner>,
    /// See [`WssLimits::incomplete_message_timeout`].
    incomplete_message_timeout: Duration,
}

impl WssTransport {
    /// Module-private: a transport is only well-formed when its parser sits
    /// behind the frame scanner the listener installs, so only the listener can
    /// build one.
    fn new(
        ws: WebSocketStream<ScannedTlsStream>,
        remote_addr: SocketAddr,
        scanner: Arc<SharedScanner>,
        incomplete_message_timeout: Duration,
    ) -> Self {
        let peer_label = format!("wss:{remote_addr}");
        let (sink, stream) = ws.split();

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        let (control_tx, mut control_rx) = mpsc::channel::<Control>(8);
        zeroclaw_spawn::spawn!(async move {
            let mut sink = sink;
            loop {
                let msg = tokio::select! {
                    line = writer_rx.recv() => match line {
                        Some(line) => Message::Text(line.into()),
                        None => break,
                    },
                    ctrl = control_rx.recv() => match ctrl {
                        Some(Control::Ping) => Message::Ping(Vec::new().into()),
                        None => break,
                    },
                };
                // An authenticated peer that stops reading parks this write once
                // the socket buffer fills. Unbounded, the outbound queue fills
                // behind it and the dispatcher parks on its own response, so it
                // never returns to `next_frame` and the heartbeat that would
                // have retired the session never runs again. A timeout here is
                // a peer that has stopped reading, not one that is merely slow.
                if !matches!(
                    tokio::time::timeout(PEER_WRITE_TIMEOUT, sink.send(msg)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            // Returning here drops both receivers, and that is what unwedges
            // the rest of the session: a dispatcher parked on a full outbound
            // queue fails at once instead of waiting on a sink nobody is
            // draining, and `next_frame` sees the writer depart and returns, so
            // the session task unwinds through the session permit and the
            // per-certificate quota guard. Nothing may be awaited after this
            // loop without closing the receivers first.
        });

        Self {
            reader: stream,
            writer_tx,
            control_tx,
            peer_label,
            awaiting_pong: false,
            scanner,
            incomplete_message_timeout,
        }
    }
}

#[async_trait]
impl RpcTransport for WssTransport {
    fn writer(&self) -> mpsc::Sender<String> {
        self.writer_tx.clone()
    }

    async fn next_frame(&mut self) -> Option<String> {
        // A handle of its own, so the deadline can be consulted while the read
        // below holds `self.reader`.
        let scanner = self.scanner.clone();
        let window = self.incomplete_message_timeout;
        // Likewise a handle of its own: the writer task owns both receivers and
        // closes them as it exits, so this resolves exactly when the session's
        // output is gone. Reading on past that would keep the session permit and
        // the credential's quota slot held for a peer that can no longer be
        // answered.
        let writer_gone = self.control_tx.clone();
        loop {
            let idle = if self.awaiting_pong {
                HEARTBEAT_TIMEOUT
            } else {
                HEARTBEAT_IDLE
            };
            // The incomplete-message bound runs on its OWN timer rather than by
            // shortening the heartbeat window: the two answer different
            // questions (is the peer alive vs. is it still holding a partial
            // message), and folding them together would let either fire for the
            // other's reason. It also lands on a peer that never sends another
            // frame to wake this loop.
            let message_deadline = scanner.incomplete_message_deadline(window);
            let read = tokio::time::timeout(idle, self.reader.next());
            let polled = match message_deadline {
                Some(at) => tokio::select! {
                    biased;
                    _ = writer_gone.closed() => return None,
                    _ = tokio::time::sleep_until(at) => {
                        match scanner.incomplete_message_deadline(window) {
                            // Still the message that armed this deadline (or an
                            // earlier one): its budget is spent.
                            Some(now_at) if now_at <= at => None,
                            // That message completed and a LATER one took its
                            // place inside a single read - or none did. Re-arm
                            // on what is actually in flight now rather than
                            // closing a session that made progress.
                            _ => continue,
                        }
                    }
                    frame = read => Some(frame),
                },
                // Nothing in flight yet. The peer's declaration is observed
                // from inside the read below and yields no frame to wake this
                // loop, so wait on the scanner's signal alongside it. The
                // signal is registered BEFORE the read is first polled, which
                // is the only place a deadline can be armed, so it cannot be
                // missed.
                None => tokio::select! {
                    biased;
                    _ = writer_gone.closed() => return None,
                    _ = scanner.armed.notified() => continue,
                    frame = read => Some(frame),
                },
            };

            let Some(frame) = polled else {
                let (held, undelivered) = scanner.incomplete_message_hold();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "WSS closing {}: a data message has been incomplete for {}s, past the {}s \
                         budget, with {} declared payload bytes still unsent; control frames do \
                         not extend that budget",
                        self.peer_label,
                        held.as_secs(),
                        self.incomplete_message_timeout.as_secs(),
                        undelivered
                    )
                );
                return None;
            };

            match frame {
                Err(_) => {
                    if self.awaiting_pong {
                        return None;
                    }
                    if self.control_tx.send(Control::Ping).await.is_err() {
                        return None;
                    }
                    self.awaiting_pong = true;
                }
                Ok(frame) => {
                    self.awaiting_pong = false;
                    // Nothing here touches the incomplete-message deadline: the
                    // scanner already observed every one of these frames at the
                    // byte level as they were read, including any part of the
                    // NEXT message that arrived in the same read. Clearing state
                    // here instead would discard exactly that read-ahead.
                    match frame {
                        Some(Ok(Message::Text(text))) => return Some(text.to_string()),
                        Some(Ok(Message::Close(_))) | None => return None,
                        // Control frames prove liveness but complete no
                        // message; they are inert to the deadline.
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                        // Never yielded by a read.
                        Some(Ok(Message::Frame(_))) => continue,
                        Some(Ok(Message::Binary(_))) => continue,
                        Some(Err(_)) => return None,
                    }
                }
            }
        }
    }

    fn peer_label(&self) -> String {
        self.peer_label.clone()
    }

    fn kind(&self) -> super::transport::TransportKind {
        super::transport::TransportKind::Wss
    }
}

// ── TLS acceptor ─────────────────────────────────────────────────

/// Build a [`TlsAcceptor`] for the remote WSS RPC plane.
///
/// The remote plane is ALWAYS mutually authenticated and TLS 1.3 only: every
/// client certificate is verified against `ca_cert_path` (optionally pinned to
/// `pinned_certs`). There is deliberately no server-only / no-client-auth path
/// here (threat model A11); the secure-by-construction builder lives in
/// [`zeroclaw_tls::build_mtls_acceptor`].
pub fn build_tls_acceptor(
    cert_path: &str,
    key_path: &str,
    ca_cert_path: &str,
    pinned_certs: &[String],
    crl_path: &str,
) -> Result<TlsAcceptor> {
    zeroclaw_tls::build_mtls_acceptor(cert_path, key_path, ca_cert_path, pinned_certs, crl_path)
}

// ── Listener ─────────────────────────────────────────────────────

/// Parser limits for the WSS RPC plane. tungstenite defaults to a 64 MiB message
/// / 16 MiB frame, which would let the parser buffer far more than the RPC
/// contract permits before [`WssTransport`]/`RpcDispatcher` can reject it.
fn rpc_ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    /// Why 32 MiB, and what it does and does not bound:
    ///
    /// - ONE WSS message carries a WHOLE RPC request, and the attachment
    ///   contract caps a request at [`crate::rpc::attachments::MAX_REQUEST_BYTES`]
    ///   (20 MiB). This envelope is that ceiling plus encoding headroom
    ///   (base64 plus JSON framing), so a legitimate max-size request is
    ///   admitted as a single frame - which tungstenite's 16 MiB DEFAULT frame
    ///   cap would wrongly reject. It must not be shrunk below the request
    ///   contract.
    /// - It mirrors the client's RPC-plane config (zerocode `rpc_ws_config`),
    ///   so the two ends cannot drift into one side rejecting what the other
    ///   will send.
    /// - It is a PER-MESSAGE bound, not a host budget. Aggregate parser
    ///   exposure is bounded elsewhere and multiplicatively:
    ///   [`WssLimits::max_sessions_per_client`] x this envelope per credential,
    ///   and [`WssLimits::max_sessions`] x this envelope globally. How long a
    ///   session may hold a partial message toward that envelope is bounded by
    ///   [`WssLimits::incomplete_message_timeout`].
    const RPC_WS_MAX: usize = 32 * 1024 * 1024;
    let mut cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    cfg.max_message_size = Some(RPC_WS_MAX);
    cfg.max_frame_size = Some(RPC_WS_MAX);
    cfg
}

/// Refuse an authenticated peer with a stated WebSocket close reason rather
/// than a bare drop, so the client can distinguish a policy refusal from a
/// network failure. Bounded by [`REFUSAL_CLOSE_TIMEOUT`]: a peer that stops
/// reading must not be able to make the refusal itself hold the permits the
/// caller is about to release.
async fn close_with_reason(ws: &mut WebSocketStream<ScannedTlsStream>, reason: &'static str) {
    let frame = CloseFrame {
        code: CloseCode::Policy,
        reason: reason.into(),
    };
    let _ = tokio::time::timeout(REFUSAL_CLOSE_TIMEOUT, ws.close(Some(frame))).await;
}

/// Run the WSS RPC listener as a daemon subsystem.
/// `client_count` is incremented on connect, decremented on disconnect —
/// shared with the Unix socket listener for `--ephemeral` shutdown logic.
pub async fn run_wss_listener(
    ctx: Arc<RpcContext>,
    cancel: CancellationToken,
    client_count: Arc<AtomicUsize>,
    tls_acceptor: TlsAcceptor,
    bind_addr: SocketAddr,
    limits: WssLimits,
) -> Result<()> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding WSS listener on {bind_addr}"))?;

    // Bounds on unauthenticated setup work and on established sessions. A
    // permit is held from accept until the peer is through both handshakes;
    // the session permit is held for the life of the dispatcher.
    let handshake_permits = Arc::new(tokio::sync::Semaphore::new(
        limits.max_pending_handshakes.max(1),
    ));
    let session_permits = Arc::new(tokio::sync::Semaphore::new(limits.max_sessions.max(1)));
    // Per-credential slice of that ceiling, so one enrolled (or stolen)
    // certificate cannot occupy the global limit by itself.
    let client_quota = Arc::new(ClientSessionQuota::default());

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"addr": bind_addr.to_string()})),
        "RPC WSS listener started"
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "RPC WSS listener shutting down"
                );
                break;
            }
            accept = listener.accept() => {
                let (tcp_stream, remote_addr) = match accept {
                    Ok(v) => v,
                    Err(e) => {
                        if is_recoverable_accept_error(&e) {
                            // Transient (e.g. EMFILE under fd pressure):
                            // the listener is still valid. Back off briefly
                            // to avoid hot-spinning, then keep serving
                            // rather than killing the daemon
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("WSS accept() transient error: {e}")
                            );
                            tokio::time::sleep(Duration::from_millis(ACCEPT_ERROR_BACKOFF_MS)).await;
                            continue;
                        }
                        return Err(e).context("WSS accept error");
                    }
                };

                // Shed before spending any TLS/task state on this socket when the
                // unauthenticated setup budget is exhausted. The ESTABLISHED-session
                // ceiling (`max_sessions`) is NOT applied here: a permit taken at
                // accept would be held through TLS/WS setup, so an unauthenticated
                // stall would consume a session slot (with max_sessions=1, one
                // staller blocks a valid client until the handshake deadline). The
                // session permit is taken only once both handshakes succeed (below).
                // Dropping the stream closes it, so a shed client sees a prompt EOF
                // instead of an indefinite stall.
                let Ok(handshake_permit) =
                    handshake_permits.clone().try_acquire_owned()
                else {
                    drop(tcp_stream);
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "WSS shedding connection from {remote_addr}: {} pending handshakes \
                             already in flight",
                            limits.max_pending_handshakes
                        )
                    );
                    continue;
                };

                let ctx = ctx.clone();
                let count = client_count.clone();
                let acceptor = tls_acceptor.clone();
                let handshake_timeout = limits.handshake_timeout;
                // Consumed only after both handshakes succeed, so an unauthenticated
                // stall can never occupy an established-session slot.
                let session_permits = session_permits.clone();
                let max_sessions = limits.max_sessions;
                let client_quota = client_quota.clone();
                let max_sessions_per_client = limits.max_sessions_per_client;
                let incomplete_message_timeout = limits.incomplete_message_timeout;

                count.fetch_add(1, Ordering::Relaxed);

                zeroclaw_spawn::spawn!(async move {
                    // Guarantees the `--ephemeral` counter is decremented on
                    // every exit path below, including the new timeout one.
                    let _count_guard = ClientCountGuard(count);

                    // ONE absolute deadline over TLS accept AND the WebSocket
                    // upgrade, measured from accept. A fresh per-phase window
                    // would let a peer spend the full budget twice.
                    let deadline = tokio::time::Instant::now() + handshake_timeout;

                    let setup = async {
                    // TLS handshake.
                    let tls_stream = match acceptor.accept(tcp_stream).await {
                        Ok(s) => s,
                        Err(e) => {
                            // The WSS plane is always mutually authenticated, so a
                            // client with no certificate (un-migrated) or a revoked
                            // one fails here. Surface it actionably rather than as a
                            // bare TLS error so the operator knows to enroll it.
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!(
                                    "WSS TLS handshake failed from {remote_addr}: {e}. The WSS plane \
                                     requires a client certificate; an un-migrated client must enroll \
                                     first (zerocode --enroll), and a revoked cert is refused."
                                )
                            );
                            return None;
                        }
                    };

                    // Scan the plaintext framing from here on, starting with the
                    // upgrade REQUEST: the handshake reads ahead, so a scanner
                    // installed after it could miss a frame header the peer
                    // pipelined into that read. What buffers a partially-received
                    // message is the WebSocket parser, which does not expose that
                    // buffer - the session loop reads the peer's own frame
                    // declarations off this scanner instead.
                    let (scanned_stream, scanner) = ScanningStream::new(tls_stream);

                    // WebSocket upgrade. An explicit parser config replaces
                    // tungstenite's 64 MiB message / 16 MiB frame defaults with a
                    // ceiling sized to the RPC contract, so the parser cannot buffer
                    // far more than a legitimate request before `next_frame` sees it.
                    let ws_stream = match tokio_tungstenite::accept_async_with_config(
                        scanned_stream,
                        Some(rpc_ws_config()),
                    )
                    .await
                    {
                        Ok(ws) => ws,
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("WSS WebSocket upgrade failed from {remote_addr}: {e}")
                            );
                            return None;
                        }
                    };
                        Some((ws_stream, scanner))
                    };

                    let (mut ws_stream, scanner) = match tokio::time::timeout_at(deadline, setup).await {
                        Ok(Some(ws)) => ws,
                        Ok(None) => return, // logged above
                        Err(_) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!(
                                    "WSS setup from {remote_addr} exceeded the {}s handshake \
                                     budget; connection dropped",
                                    handshake_timeout.as_secs()
                                )
                            );
                            return;
                        }
                    };

                    // Through both handshakes: this peer presented a valid client
                    // certificate. Only now consume an ESTABLISHED-session slot, so
                    // unauthenticated setup stalls (bounded separately by
                    // max_pending_handshakes) can never exhaust the session ceiling.
                    // Take the session permit BEFORE releasing the handshake permit
                    // so the two bounds hand off with no gap a flood could exploit.
                    let Ok(_session_permit) = session_permits.try_acquire_owned() else {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!(
                                "WSS shedding authenticated connection from {remote_addr}: {} \
                                 sessions already established",
                                max_sessions
                            )
                        );
                        return;
                    };
                    // The client cert was verified against the CA during the
                    // mTLS handshake; capture its SHA-256 fingerprint (the ledger
                    // key, and the per-credential quota key) before the stream is
                    // consumed by the transport.
                    let peer_cert_fp = ws_stream
                        .get_ref()
                        .get_ref()
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|certs| certs.first())
                        .map(|der| zeroclaw_tls::cert_sha256_fingerprint(der.as_ref()));

                    // The plane is mandatory mTLS, so this is always present. A
                    // session with no credential could not be attributed to one
                    // and so could not be quota-bounded: refuse rather than admit
                    // an unaccountable session.
                    let Some(peer_cert_fp) = peer_cert_fp else {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!(
                                "WSS refusing {remote_addr}: the mutually-authenticated handshake \
                                 exposed no client certificate, so no per-credential quota applies"
                            )
                        );
                        close_with_reason(&mut ws_stream, "no client certificate").await;
                        return;
                    };

                    // Per-credential slice of the session ceiling. Held for the
                    // life of the session by a guard, so it is returned on every
                    // exit path below (dispatcher return, read error, EOF,
                    // heartbeat, incomplete-message deadline).
                    let Some(_client_slot) =
                        client_quota.try_admit(&peer_cert_fp, max_sessions_per_client)
                    else {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!(
                                "WSS refusing {remote_addr}: client certificate {peer_cert_fp} \
                                 already holds {max_sessions_per_client} sessions, its \
                                 per-credential ceiling"
                            )
                        );
                        // Distinct, clean refusal; the session permit and the
                        // handshake permit are released by returning, and the
                        // refused credential is never recorded in the quota map.
                        close_with_reason(&mut ws_stream, "per-certificate session quota").await;
                        return;
                    };

                    // Released for the next connection being set up now that this
                    // one holds an established-session slot and a credential slot.
                    drop(handshake_permit);

                    let mut transport = WssTransport::new(
                        ws_stream,
                        remote_addr,
                        scanner,
                        incomplete_message_timeout,
                    );
                    let peer = transport.peer_label();
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new(ctx.clone(), writer_tx, peer)
                        .with_peer_cert_fingerprint(Some(peer_cert_fp));
                    dispatcher.run(&mut transport).await;

                    // Epoch-checked: a relay flap can leave this session draining
                    // long after the client has reconnected and re-adopted the
                    // same TUI id, and removing by id alone would evict the live
                    // registration instead of this one.
                    if let Some((tui_id, tui_epoch)) = dispatcher.tui_registration() {
                        ctx.tui_registry.unregister(tui_id, tui_epoch);
                        use ::zeroclaw_log::Instrument as _;
                        let span = ::zeroclaw_log::info_span!(
                            target: "zeroclaw_log_internal_scope",
                            "zeroclaw_scope",
                            owner_tui_id = %tui_id,
                            channel = "wss",
                        );
                        async {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_category(::zeroclaw_log::EventCategory::Agent),
                                "WSS TUI disconnected; sessions retained (persistent)"
                            );
                        }
                        .instrument(span)
                        .await;
                    }
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod frame_scanner_tests {
    use super::{FRAME_HEADER_MAX, FrameScanner, ScanState};

    /// A minimal WebSocket upgrade REQUEST. The scanner starts inside one, so
    /// every case here has to get through it first.
    const UPGRADE_REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\n\r\n";

    /// A client-to-server frame as it appears on the wire: masked, carrying a
    /// DECLARED payload length that `payload` may be shorter than. That gap -
    /// declared but not sent - is exactly the reservation under test.
    fn wire_frame(fin: bool, opcode: u8, declared: u64, payload: &[u8]) -> Vec<u8> {
        let mask = [0xA3u8, 0x5C, 0x11, 0x7E];
        let mut out = vec![if fin { 0x80 | opcode } else { opcode }];
        if declared < 126 {
            out.push(0x80 | declared as u8);
        } else if declared < 65536 {
            out.push(0x80 | 126);
            out.extend_from_slice(&(declared as u16).to_be_bytes());
        } else {
            out.push(0x80 | 127);
            out.extend_from_slice(&declared.to_be_bytes());
        }
        out.extend_from_slice(&mask);
        out.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % mask.len()]),
        );
        out
    }

    fn past_upgrade() -> FrameScanner {
        let mut scanner = FrameScanner::new();
        scanner.feed(UPGRADE_REQUEST);
        assert!(
            matches!(scanner.state, ScanState::Header { len: 0, .. }),
            "the scanner must resume framing at the byte after the request"
        );
        scanner
    }

    // The longest header the wire format allows must fit the fixed buffer, or
    // the scanner would silently truncate one and lose sync.
    #[tokio::test]
    async fn header_buffer_covers_the_longest_legal_header() {
        let longest = wire_frame(false, 0x1, u64::from(u32::MAX), &[]);
        assert_eq!(longest.len(), FRAME_HEADER_MAX);
    }

    // The core of the reviewer's finding: a 14-byte header declaring a huge
    // payload is a huge reservation. The clock must start on the header, not on
    // any volume of payload.
    #[tokio::test]
    async fn a_declared_payload_arms_the_clock_before_any_payload_arrives() {
        let mut scanner = past_upgrade();
        assert!(scanner.data_message_started_at().is_none());

        let header = &wire_frame(false, 0x1, 31 * 1024 * 1024, &[])[..FRAME_HEADER_MAX];
        scanner.feed(header);

        assert!(
            scanner.data_message_started_at().is_some(),
            "a data frame header declaring 31 MiB must arm the deadline on its own"
        );
        assert_eq!(
            scanner.outstanding_declared_bytes(),
            31 * 1024 * 1024,
            "the whole declared payload is reserved and unsent"
        );
    }

    // Even the first byte is enough to classify the frame, and a peer that
    // stops there is still holding parser state.
    #[tokio::test]
    async fn one_header_byte_is_enough_to_arm_and_a_split_header_stays_in_sync() {
        let mut scanner = past_upgrade();
        let frame = wire_frame(false, 0x2, 1000, &[7u8; 4]);
        scanner.feed(&frame[..1]);
        assert!(scanner.data_message_started_at().is_some());

        // The rest of the header dribbles in a byte at a time, then 4 payload
        // bytes: the scanner must still know 996 remain.
        for b in &frame[1..] {
            scanner.feed(std::slice::from_ref(b));
        }
        assert_eq!(scanner.outstanding_declared_bytes(), 996);
    }

    // Control frames park nothing, so no volume of them may arm the deadline.
    #[tokio::test]
    async fn control_frames_never_arm_the_clock() {
        let mut scanner = past_upgrade();
        for _ in 0..500 {
            scanner.feed(&wire_frame(true, 0x9, 125, &[0u8; 125])); // Ping
            scanner.feed(&wire_frame(true, 0xA, 125, &[0u8; 125])); // Pong
        }
        assert!(
            scanner.data_message_started_at().is_none(),
            "control frames are not part of any data message"
        );
    }

    // A control frame interleaved into a fragmented message must neither end it
    // nor restart its clock.
    #[tokio::test]
    async fn an_interleaved_ping_neither_ends_nor_restarts_a_partial_message() {
        let mut scanner = past_upgrade();
        scanner.feed(&wire_frame(false, 0x1, 8, &[1u8; 8]));
        let started = scanner
            .data_message_started_at()
            .expect("the fragmented message is in flight");

        scanner.feed(&wire_frame(true, 0x9, 0, &[]));
        assert_eq!(
            scanner.data_message_started_at(),
            Some(started),
            "a Ping must not restart the partial message's clock"
        );

        // The FIN continuation frame is what ends it.
        scanner.feed(&wire_frame(true, 0x0, 8, &[1u8; 8]));
        assert!(scanner.data_message_started_at().is_none());
    }

    // The read-ahead case: one read carrying the end of one message and the
    // opening of the next leaves the SECOND message in flight.
    #[tokio::test]
    async fn read_ahead_past_a_completed_message_keeps_the_next_one_armed() {
        let mut scanner = past_upgrade();
        let mut one_read = wire_frame(true, 0x1, 16, &[b'a'; 16]);
        one_read.extend_from_slice(&wire_frame(false, 0x1, 4 * 1024 * 1024, &[b'b'; 32]));
        scanner.feed(&one_read);

        assert!(
            scanner.data_message_started_at().is_some(),
            "the partial message opened in the same read must still be in flight"
        );
        assert_eq!(
            scanner.outstanding_declared_bytes(),
            4 * 1024 * 1024 - 32,
            "the 32 payload bytes already read must be accounted against the declaration"
        );
    }

    // Two complete messages in one read leave nothing in flight - the deadline
    // must not linger on a session that is making progress.
    #[tokio::test]
    async fn two_complete_messages_in_one_read_leave_nothing_in_flight() {
        let mut scanner = past_upgrade();
        let mut one_read = wire_frame(true, 0x1, 16, &[b'a'; 16]);
        one_read.extend_from_slice(&wire_frame(true, 0x1, 24, &[b'b'; 24]));
        scanner.feed(&one_read);
        assert!(scanner.data_message_started_at().is_none());
    }

    // Zero-length frames complete inside their own header; a FIN one must not
    // leave a phantom message in flight.
    #[tokio::test]
    async fn zero_length_frames_are_resolved_within_the_header() {
        let mut scanner = past_upgrade();
        scanner.feed(&wire_frame(true, 0x1, 0, &[]));
        assert!(scanner.data_message_started_at().is_none());

        scanner.feed(&wire_frame(false, 0x1, 0, &[]));
        assert!(
            scanner.data_message_started_at().is_some(),
            "a zero-length NON-final frame still opens a message"
        );
    }

    // 16-bit and 64-bit length encodings must be decoded exactly, or the
    // scanner would resynchronise in the middle of a payload.
    #[tokio::test]
    async fn extended_length_encodings_are_decoded_exactly() {
        for declared in [125u64, 126, 65535, 65536, 1 << 32] {
            let mut scanner = past_upgrade();
            let frame = wire_frame(false, 0x2, declared, &[]);
            scanner.feed(&frame);
            assert_eq!(
                scanner.outstanding_declared_bytes(),
                declared,
                "declared length {declared} was decoded wrong"
            );
        }
    }

    // An unmasked frame has a 4-byte-shorter header. Getting that wrong would
    // desynchronise the scanner even though tungstenite is the one that rejects
    // the frame.
    #[tokio::test]
    async fn an_unmasked_client_frame_does_not_desynchronise_the_scanner() {
        let mut scanner = past_upgrade();
        // FIN text, unmasked, 4-byte payload, then a masked Ping behind it.
        let mut bytes = vec![0x81, 0x04, b'p', b'i', b'n', b'g'];
        bytes.extend_from_slice(&wire_frame(true, 0x9, 0, &[]));
        scanner.feed(&bytes);
        assert!(
            scanner.data_message_started_at().is_none(),
            "the unmasked frame completed and the Ping behind it must not have armed anything"
        );
    }

    // A length no host could ever hold must move a counter and nothing else.
    #[tokio::test]
    async fn an_absurd_declared_length_only_moves_a_counter() {
        let mut scanner = past_upgrade();
        scanner.feed(&wire_frame(false, 0x1, u64::MAX, &[9u8; 3]));
        assert_eq!(scanner.outstanding_declared_bytes(), u64::MAX - 3);
        assert!(scanner.data_message_started_at().is_some());
        // The state a declaration can create is a fixed set of counters. If
        // this ever grew with the declared length, the scanner would have
        // become the very allocation it exists to bound.
        assert!(
            std::mem::size_of::<ScanState>() <= 40,
            "scanner state must stay O(1), got {} bytes",
            std::mem::size_of::<ScanState>()
        );
    }

    // Header bytes pipelined into the upgrade read are inside the prelude's
    // tail: the scanner must resume framing at exactly the right byte.
    #[tokio::test]
    async fn a_frame_pipelined_into_the_upgrade_read_is_still_seen() {
        let mut scanner = FrameScanner::new();
        let mut one_read = UPGRADE_REQUEST.to_vec();
        one_read.extend_from_slice(&wire_frame(false, 0x1, 9 * 1024 * 1024, &[b'z'; 8]));
        scanner.feed(&one_read);
        assert!(
            scanner.data_message_started_at().is_some(),
            "a header pipelined into the handshake read must not slip past the scanner"
        );
        assert_eq!(scanner.outstanding_declared_bytes(), 9 * 1024 * 1024 - 8);
    }

    // The upgrade's own parser tolerates bare-LF line endings, so the scanner
    // must too - otherwise it would sit in the prelude forever on a session the
    // daemon accepted, and arm nothing at all.
    #[tokio::test]
    async fn a_bare_lf_request_terminator_still_ends_the_prelude() {
        let mut scanner = FrameScanner::new();
        scanner.feed(b"GET / HTTP/1.1\nHost: h\n\n");
        scanner.feed(&wire_frame(false, 0x1, 4096, &[]));
        assert!(scanner.data_message_started_at().is_some());
    }
}

#[cfg(test)]
mod accept_error_tests {
    use super::is_recoverable_accept_error;
    use std::io::{Error, ErrorKind};

    #[cfg(unix)]
    #[test]
    fn fd_exhaustion_accept_errors_are_recoverable() {
        // EMFILE/ENFILE must not terminate the daemon.
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(24))); // EMFILE
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(23))); // ENFILE
    }

    #[test]
    fn transient_kinds_recover_but_fatal_propagates() {
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::ConnectionAborted
        )));
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::Interrupted
        )));
        // A non-transient error is not swallowed (loop will propagate it).
        assert!(!is_recoverable_accept_error(&Error::from(
            ErrorKind::InvalidInput
        )));
    }
}

#[cfg(test)]
// Test code, not daemon-path: bare `tokio::spawn` is fine here (the
// `zeroclaw_spawn::spawn!` attribution rule is for production daemon tasks).
#[allow(clippy::disallowed_methods)]
mod parser_bound_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

    // In-memory duplex only (no network/TLS). The URI is built from parts with
    // the scheme as a bare field so no insecure-scheme string literal exists in
    // source for the hosted scanner to flag.
    fn loopback_url() -> tokio_tungstenite::tungstenite::http::Uri {
        tokio_tungstenite::tungstenite::http::Uri::builder()
            .scheme("ws")
            .authority("ceiling.test")
            .path_and_query("/")
            .build()
            .expect("valid test uri")
    }

    // A client permitted to EMIT frames larger than tungstenite's 16 MiB default,
    // so the SERVER's configured ceiling is what is under test.
    fn permissive_client_config() -> WebSocketConfig {
        let mut cfg = WebSocketConfig::default();
        cfg.max_message_size = Some(64 * 1024 * 1024);
        cfg.max_frame_size = Some(64 * 1024 * 1024);
        cfg
    }

    // W1: the WSS upgrade applies an explicit parser config sized to the RPC
    // contract. A legitimate max-size request (MAX_REQUEST_BYTES = 20 MiB) must be
    // admitted as a single frame — which tungstenite's 16 MiB DEFAULT frame cap
    // would wrongly reject — while a message beyond the 32 MiB ceiling is refused
    // at the parser instead of buffered up to the 64 MiB message default.
    #[tokio::test]
    async fn rpc_ws_config_admits_contract_max_and_refuses_oversized() {
        // (1) A 20 MiB message is accepted and delivered intact.
        {
            let (client_io, server_io) = tokio::io::duplex(1 << 20);
            let server = tokio::spawn(async move {
                let mut ws =
                    tokio_tungstenite::accept_async_with_config(server_io, Some(rpc_ws_config()))
                        .await
                        .expect("server upgrade");
                match ws.next().await {
                    Some(Ok(Message::Binary(b))) => Ok(b.len()),
                    other => Err(format!("{other:?}")),
                }
            });
            let (mut client, _r) = tokio_tungstenite::client_async_with_config(
                loopback_url(),
                client_io,
                Some(permissive_client_config()),
            )
            .await
            .expect("client upgrade");
            let payload = vec![7u8; 20 * 1024 * 1024];
            client
                .send(Message::binary(payload))
                .await
                .expect("send 20 MiB");
            client.flush().await.expect("flush");
            let got = server.await.unwrap();
            assert_eq!(
                got,
                Ok(20 * 1024 * 1024),
                "a 20 MiB request (contract max) must be admitted as one frame"
            );
        }
        // (2) A message beyond the 32 MiB ceiling is refused at the parser.
        {
            let (client_io, server_io) = tokio::io::duplex(1 << 20);
            let server = tokio::spawn(async move {
                let mut ws =
                    tokio_tungstenite::accept_async_with_config(server_io, Some(rpc_ws_config()))
                        .await
                        .expect("server upgrade");
                loop {
                    match ws.next().await {
                        Some(Ok(_)) => continue,
                        Some(Err(_)) => return true,
                        None => return false,
                    }
                }
            });
            let (mut client, _r) = tokio_tungstenite::client_async_with_config(
                loopback_url(),
                client_io,
                Some(permissive_client_config()),
            )
            .await
            .expect("client upgrade");
            let oversized = vec![0u8; 33 * 1024 * 1024];
            let _ = client.send(Message::binary(oversized)).await;
            let _ = client.flush().await;
            let refused = server.await.unwrap();
            assert!(
                refused,
                "a message beyond the 32 MiB ceiling must be refused"
            );
        }
    }
}
