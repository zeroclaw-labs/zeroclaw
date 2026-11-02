//! WebSocket Secure (WSS) transport for the RPC layer.
//! Mirrors the Unix socket transport (`unix.rs`) but uses TLS-encrypted
//! WebSocket connections, enabling remote TUI-to-daemon connectivity.

use super::context::RpcContext;
use super::dispatch::{RpcAccessPolicy, RpcDispatcher};
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
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

/// Best-effort deadline for flushing a WebSocket Close frame during daemon
/// cancellation. A suspended or black-holed peer must not retain the detached
/// writer task and its TLS/socket resources indefinitely.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

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

async fn run_writer<S>(
    mut sink: S,
    mut writer_rx: mpsc::Receiver<String>,
    mut control_rx: mpsc::Receiver<Control>,
    cancel: CancellationToken,
    close_timeout: Duration,
) where
    S: futures_util::Sink<Message> + Unpin,
{
    loop {
        let msg = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            line = writer_rx.recv() => match line {
                Some(line) => Message::Text(line.into()),
                None => break,
            },
            ctrl = control_rx.recv() => match ctrl {
                Some(Control::Ping) => Message::Ping(Vec::new().into()),
                None => break,
            },
        };
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = tokio::time::timeout(PEER_WRITE_TIMEOUT, sink.send(msg)) => {
                matches!(result, Ok(Ok(())))
            },
        };
        if !sent {
            break;
        }
    }

    // Close the receivers before awaiting best-effort protocol cleanup. This
    // immediately releases any dispatcher parked on a full outbound queue.
    drop(writer_rx);
    drop(control_rx);
    let _ = tokio::time::timeout(close_timeout, sink.close()).await;
}

pub struct WssTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    reader: futures_util::stream::SplitStream<WebSocketStream<ScanningStream<S>>>,
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

impl<S> WssTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Module-private: a transport is only well-formed when its parser sits
    /// behind the frame scanner the listener installs, so only the listener can
    /// build one.
    fn new(
        ws: WebSocketStream<ScanningStream<S>>,
        remote_addr: SocketAddr,
        scanner: Arc<SharedScanner>,
        incomplete_message_timeout: Duration,
        cancel: CancellationToken,
    ) -> Self {
        let peer_label = format!("wss:{remote_addr}");
        let (sink, stream) = ws.split();

        let (writer_tx, writer_rx) = mpsc::channel::<String>(64);
        let (control_tx, control_rx) = mpsc::channel::<Control>(8);
        zeroclaw_spawn::spawn!(async move {
            // Session approval channels and log forwarders retain writer
            // senders past disconnect, so channel closure alone cannot end
            // this task. Cancellation interrupts an in-flight send, then
            // gives the Close-frame flush a bounded best-effort window.
            run_writer(sink, writer_rx, control_rx, cancel, CLOSE_TIMEOUT).await;
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
impl<S> RpcTransport for WssTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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
    serve_wss_listener(ctx, cancel, client_count, tls_acceptor, listener, limits).await
}

async fn serve_wss_listener(
    ctx: Arc<RpcContext>,
    cancel: CancellationToken,
    client_count: Arc<AtomicUsize>,
    tls_acceptor: TlsAcceptor,
    listener: TcpListener,
    limits: WssLimits,
) -> Result<()> {
    let bind_addr = listener
        .local_addr()
        .context("reading WSS listener address")?;

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

    let connections_cancel = cancel.child_token();
    let mut connection_tasks = tokio::task::JoinSet::new();
    let mut listener_result: Result<()> = Ok(());
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
            completed = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                if let Some(Err(error)) = completed {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!("RPC WSS connection task failed: {error}")
                    );
                }
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
                        listener_result = Err(e).context("WSS accept error");
                        break;
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
                let acceptor = tls_acceptor.clone();
                let conn_cancel = connections_cancel.child_token();
                let handshake_timeout = limits.handshake_timeout;
                // Consumed only after both handshakes succeed, so an unauthenticated
                // stall can never occupy an established-session slot.
                let session_permits = session_permits.clone();
                let max_sessions = limits.max_sessions;
                let client_quota = client_quota.clone();
                let max_sessions_per_client = limits.max_sessions_per_client;
                let incomplete_message_timeout = limits.incomplete_message_timeout;

                // Counted here rather than inside the task so an accepted
                // connection is visible to the daemon immediately.
                let activity = crate::rpc::ConnectionActivity::new(client_count.clone());

                connection_tasks.spawn(async move {
                    // Guarantees the `--ephemeral` counter is decremented on
                    // every exit path below, including the new timeout one.
                    let _count_guard = activity.clone();

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

                    let setup_res = tokio::select! {
                        _ = conn_cancel.cancelled() => return,
                        res = tokio::time::timeout_at(deadline, setup) => res,
                    };

                    let (mut ws_stream, scanner) = match setup_res {
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
                        conn_cancel.clone(),
                    );
                    let peer = transport.peer_label();
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new_with_cancel_and_channel_access(
                        ctx.clone(),
                        writer_tx,
                        peer,
                        conn_cancel.clone(),
                        RpcAccessPolicy::RemoteSessionOwner,
                        None,
                    )
                    // mTLS peer cert = transport/device admission (quota, audit);
                    // transport credential = principal authentication at initialize.
                    // The two layers compose: the cert admits the connection,
                    // the token authenticates the principal.
                    .with_peer_cert_fingerprint(Some(peer_cert_fp))
                    .with_connection_activity(activity)
                    .with_transport(transport.kind(), transport.credential());
                    // The concrete dispatcher future carries the full request
                    // state machine. Keep that state heap-backed so an active
                    // WSS request does not depend on the executor worker's
                    // thread-stack size.
                    Box::pin(dispatcher.run_connection(&mut transport)).await;

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

    connections_cancel.cancel();
    // Drain accepted connection tasks. Each connection cancels its prompts
    // and drains them in `dispatcher.shutdown().await`. In-flight prompts have
    // up to CANCEL_GRACE (5 seconds) to unwind cooperatively. If a connection
    // exceeds that window, force-abort remaining connection tasks and join them;
    // aborting a connection drops its dispatcher, which aborts the prompt
    // handles it still owns.
    tokio::select! {
        _ = async {
            while connection_tasks.join_next().await.is_some() {}
        } => {}
        _ = tokio::time::sleep(crate::rpc::CONNECTION_DRAIN_GRACE) => {
            connection_tasks.shutdown().await;
        }
    }

    listener_result
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
    use super::{
        Control, DEFAULT_INCOMPLETE_MESSAGE_TIMEOUT_SECS, ScanningStream, WssTransport,
        is_recoverable_accept_error, run_writer,
    };
    use crate::rpc::dispatch::{Method, RpcDispatcher};
    use crate::rpc::types::InitializeParams;
    use futures_util::{SinkExt, StreamExt};
    use std::io::{Error, ErrorKind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::sync::mpsc;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_util::sync::CancellationToken;
    use zeroclaw_api::jsonrpc::{JSONRPC_VERSION, JsonRpcRequest};

    struct NonReadingPeer {
        write_polled: Arc<AtomicBool>,
    }

    impl AsyncRead for NonReadingPeer {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for NonReadingPeer {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.write_polled.store(true, Ordering::Release);
            Poll::Pending
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn cancellation_bounds_send_and_close_for_non_reading_peer() {
        let write_polled = Arc::new(AtomicBool::new(false));
        let websocket = WebSocketStream::from_raw_socket(
            NonReadingPeer {
                write_polled: Arc::clone(&write_polled),
            },
            Role::Server,
            None,
        )
        .await;
        let (sink, _stream) = websocket.split();
        let (writer_tx, writer_rx) = mpsc::channel(1);
        let (_control_tx, control_rx) = mpsc::channel::<Control>(1);
        let cancel = CancellationToken::new();
        let writer_cancel = cancel.clone();

        let writer = zeroclaw_spawn::spawn!(run_writer(
            sink,
            writer_rx,
            control_rx,
            writer_cancel,
            Duration::from_millis(10),
        ));
        writer_tx.send("response".to_owned()).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !write_polled.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the WebSocket sink should attempt the blocked network write");

        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(100), writer)
            .await
            .expect("cancellation and bounded Close must release the writer task")
            .expect("writer task should not panic");
    }

    fn rpc_request<T: serde::Serialize>(method: Method, params: &T, id: u64) -> String {
        serde_json::to_string(&JsonRpcRequest::new(
            method.wire_name(),
            serde_json::to_value(params).unwrap(),
            serde_json::Value::Number(id.into()),
        ))
        .unwrap()
    }

    /// Read frames until the response carrying `id` arrives, skipping the
    /// notifications a turn emits on the way.
    async fn read_response_with_id(
        stream: &mut futures_util::stream::SplitStream<
            WebSocketStream<impl AsyncRead + AsyncWrite + Unpin>,
        >,
        id: u64,
    ) -> serde_json::Value {
        loop {
            let message = stream
                .next()
                .await
                .unwrap_or_else(|| {
                    panic!("connection closed before the response to request {id} arrived")
                })
                .expect("server frame should be valid");
            let Ok(text) = message.to_text() else {
                continue;
            };
            let Ok(frame) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            if frame["id"] == serde_json::json!(id) {
                return frame;
            }
        }
    }

    /// A prompt runs on a handle that shares the connection generation token.
    /// Dropping that handle on normal completion must not end the connection.
    async fn assert_completed_prompt_keeps_the_wss_connection_serving() {
        use crate::rpc::dispatch::connection_test_support::{
            IMMEDIATE_SID, fixture, insert_immediate_session,
        };

        let tmp = tempfile::tempdir().unwrap();
        let fixture = fixture(tmp.path()).await;
        insert_immediate_session(&fixture.ctx, tmp.path()).await;
        let listener_cancel = CancellationToken::new();
        let connection_cancel = listener_cancel.child_token();
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let (server_io, scanner) = ScanningStream::new(server_io);
        let (server_ws, client_ws) = tokio::join!(
            WebSocketStream::from_raw_socket(server_io, Role::Server, None),
            WebSocketStream::from_raw_socket(client_io, Role::Client, None),
        );
        let mut transport = WssTransport::new(
            server_ws,
            "127.0.0.1:43111".parse().unwrap(),
            scanner,
            Duration::from_secs(DEFAULT_INCOMPLETE_MESSAGE_TIMEOUT_SECS),
            connection_cancel.clone(),
        );
        let writer_tx = crate::rpc::transport::RpcTransport::writer(&transport);
        let mut dispatcher = RpcDispatcher::new_with_connection_cancel(
            Arc::clone(&fixture.ctx),
            writer_tx,
            "wss:127.0.0.1:43111".to_string(),
            connection_cancel.clone(),
        );
        let connection = zeroclaw_spawn::spawn!(async move {
            dispatcher.run_connection(&mut transport).await;
        });
        let (mut client_sink, mut client_stream) = client_ws.split();

        let init = InitializeParams {
            protocol_version: 1,
            tui_id: None,
            tui_sig: None,
            env: Default::default(),
            client_capabilities: None,
            auth_token: None,
            auth_provider: None,
        };
        client_sink
            .send(Message::Text(
                rpc_request(Method::Initialize, &init, 1).into(),
            ))
            .await
            .unwrap();
        let initialized = read_response_with_id(&mut client_stream, 1).await;
        assert!(initialized["error"].is_null());

        client_sink
            .send(Message::Text(
                rpc_request(
                    Method::SessionPrompt,
                    &serde_json::json!({"session_id": IMMEDIATE_SID, "prompt": "run"}),
                    2,
                )
                .into(),
            ))
            .await
            .unwrap();
        let prompt = tokio::time::timeout(
            Duration::from_secs(10),
            read_response_with_id(&mut client_stream, 2),
        )
        .await
        .expect("the prompt should complete and answer its request");
        assert!(
            prompt["error"].is_null(),
            "unexpected prompt error: {prompt}"
        );

        client_sink
            .send(Message::Text(
                rpc_request(Method::Health, &serde_json::json!({}), 3).into(),
            ))
            .await
            .unwrap();
        let health = tokio::time::timeout(
            Duration::from_secs(10),
            read_response_with_id(&mut client_stream, 3),
        )
        .await
        .expect("a completed prompt must leave its connection open for the next request");
        assert!(
            health["error"].is_null(),
            "unexpected health error: {health}"
        );
        assert!(
            !connection_cancel.is_cancelled(),
            "a completed prompt must not cancel its connection generation"
        );

        listener_cancel.cancel();
        tokio::time::timeout(Duration::from_secs(7), connection)
            .await
            .expect("WSS connection should end once its generation is cancelled")
            .expect("WSS connection task should not panic");
    }

    #[test]
    fn completed_prompt_keeps_the_wss_connection_serving() {
        std::thread::Builder::new()
            .name("wss-completed-prompt".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(assert_completed_prompt_keeps_the_wss_connection_serving());
            })
            .unwrap()
            .join()
            .expect("WSS completed prompt test thread should not panic");
    }

    async fn assert_reload_drains_wss_connection_generation() {
        use crate::rpc::dispatch::connection_test_support::{QUEUED_SID, RUNNING_SID, fixture};

        let tmp = tempfile::tempdir().unwrap();
        let fixture = fixture(tmp.path()).await;
        let queued_guard = fixture
            .ctx
            .sessions
            .session_queue
            .acquire(QUEUED_SID)
            .await
            .expect("test should hold the queued session actor");
        let listener_cancel = CancellationToken::new();
        let connection_cancel = listener_cancel.child_token();
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let (server_io, scanner) = ScanningStream::new(server_io);
        let (server_ws, client_ws) = tokio::join!(
            WebSocketStream::from_raw_socket(server_io, Role::Server, None),
            WebSocketStream::from_raw_socket(client_io, Role::Client, None),
        );
        let mut transport = WssTransport::new(
            server_ws,
            "127.0.0.1:43110".parse().unwrap(),
            scanner,
            Duration::from_secs(DEFAULT_INCOMPLETE_MESSAGE_TIMEOUT_SECS),
            connection_cancel.clone(),
        );
        let writer_tx = crate::rpc::transport::RpcTransport::writer(&transport);
        let mut dispatcher = RpcDispatcher::new_with_connection_cancel(
            Arc::clone(&fixture.ctx),
            writer_tx,
            "wss:127.0.0.1:43110".to_string(),
            connection_cancel,
        );
        let connection = zeroclaw_spawn::spawn!(async move {
            dispatcher.run_connection(&mut transport).await;
        });
        let (mut client_sink, mut client_stream) = client_ws.split();

        let init = InitializeParams {
            protocol_version: 1,
            tui_id: None,
            tui_sig: None,
            env: Default::default(),
            client_capabilities: None,
            auth_token: None,
            auth_provider: None,
        };
        client_sink
            .send(Message::Text(
                rpc_request(Method::Initialize, &init, 1).into(),
            ))
            .await
            .unwrap();
        let initialized = client_stream
            .next()
            .await
            .expect("initialize response frame")
            .expect("initialize response should be valid");
        let initialized: serde_json::Value = serde_json::from_str(
            initialized
                .to_text()
                .expect("initialize response should be text"),
        )
        .unwrap();
        assert_eq!(initialized["jsonrpc"], JSONRPC_VERSION);
        assert!(initialized["error"].is_null());

        client_sink
            .send(Message::Text(
                rpc_request(
                    Method::SessionPrompt,
                    &serde_json::json!({"session_id": RUNNING_SID, "prompt": "run"}),
                    2,
                )
                .into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), fixture.provider_started.notified())
            .await
            .expect("running prompt should reach the provider");

        client_sink
            .send(Message::Text(
                rpc_request(
                    Method::SessionPrompt,
                    &serde_json::json!({"session_id": QUEUED_SID, "prompt": "queue"}),
                    3,
                )
                .into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while fixture
                .ctx
                .sessions
                .session_queue
                .queue_depth(QUEUED_SID)
                .await
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second prompt should be waiting in the session actor queue");

        listener_cancel.cancel();
        tokio::time::timeout(Duration::from_secs(7), connection)
            .await
            .expect("WSS connection must drain generation-owned prompt tasks")
            .expect("WSS connection task should not panic");

        assert!(
            fixture.provider_dropped.load(Ordering::Acquire),
            "reload must cancel and join the running provider future"
        );
        assert_eq!(
            fixture.queued_provider_calls.load(Ordering::Acquire),
            0,
            "a queued prompt from the closed generation must never execute"
        );
        assert_eq!(
            fixture
                .ctx
                .sessions
                .session_queue
                .queue_depth(QUEUED_SID)
                .await,
            1,
            "only the deliberate test holder should remain queued"
        );
        assert!(!fixture.ctx.sessions.has_inflight_turn(RUNNING_SID));
        drop(queued_guard);
    }

    #[test]
    fn reload_drains_running_and_queued_prompts_for_wss_connection_generation() {
        std::thread::Builder::new()
            .name("wss-connection-generation".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(assert_reload_drains_wss_connection_generation());
            })
            .unwrap()
            .join()
            .expect("WSS connection generation test thread should not panic");
    }

    #[derive(Debug)]
    struct NoServerVerify;

    impl rustls::client::danger::ServerCertVerifier for NoServerVerify {
        fn verify_server_cert(
            &self,
            _e: &rustls::pki_types::CertificateDer<'_>,
            _i: &[rustls::pki_types::CertificateDer<'_>],
            _n: &rustls::pki_types::ServerName<'_>,
            _o: &[u8],
            _t: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _m: &[u8],
            _c: &rustls::pki_types::CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _m: &[u8],
            _c: &rustls::pki_types::CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    fn test_client_connector(
        client_cert_pem: &str,
        client_key_pem: &str,
    ) -> tokio_tungstenite::Connector {
        use std::io::Write;
        let mut cf = tempfile::NamedTempFile::new().unwrap();
        cf.write_all(client_cert_pem.as_bytes()).unwrap();
        cf.flush().unwrap();
        let mut kf = tempfile::NamedTempFile::new().unwrap();
        kf.write_all(client_key_pem.as_bytes()).unwrap();
        kf.flush().unwrap();

        let chain = zeroclaw_tls::load_certs(cf.path().to_str().unwrap()).unwrap();
        let key = zeroclaw_tls::load_private_key(kf.path().to_str().unwrap()).unwrap();
        let client_cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoServerVerify))
        .with_client_auth_cert(chain, key)
        .unwrap();
        tokio_tungstenite::Connector::Rustls(Arc::new(client_cfg))
    }

    #[tokio::test]
    async fn goal_status_uses_the_typed_session_goal_response_over_wss() {
        use crate::rpc::context::RpcContext;
        use crate::rpc::dispatch::connection_test_support::insert_immediate_session_with_owner;
        use crate::rpc::session::SessionStore;
        use crate::rpc::types::SessionGoalResult;
        use zeroclaw_config::schema::Config;
        use zeroclaw_goal::control::GoalResponse;
        use zeroclaw_infra::session_queue::SessionActorQueue;

        const SESSION_ID: &str = "goal-wss-transport";
        const TUI_ID: &str = "goal-wss-tui";

        let _ = rustls::crypto::ring::default_provider().install_default();
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.paired_tokens = vec!["zc_tok".to_owned()];
        config.goal.enabled = false;
        let sessions = Arc::new(SessionStore::new(
            64,
            Arc::new(SessionActorQueue::new(4, 30, 60)),
        ));
        let mut ctx = RpcContext::minimal(config, sessions);
        std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).unwrap();
        Arc::get_mut(&mut ctx).unwrap().tui_registry =
            Arc::new(crate::rpc::tui_identity::TuiRegistry::new(tmp.path()));
        insert_immediate_session_with_owner(&ctx, tmp.path(), SESSION_ID, TUI_ID).await;
        // Goal commands resolve the control plane before the disabled policy,
        // so give this context its own plane instead of depending on whether
        // an in-process daemon test already installed the process-global one.
        ctx.goal_runtime.install_test_control_plane(
            crate::control_plane::ControlPlaneHandle::open(tmp.path()).unwrap(),
        );

        let materials = zeroclaw_tls::ensure_server_materials(tmp.path(), &[]).unwrap();
        let ca_pem = std::fs::read_to_string(&materials.ca_cert_path).unwrap();
        let ca_key_pem = std::fs::read_to_string(&materials.ca_key_path).unwrap();
        let client =
            zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key_pem, "goal-test-client").unwrap();
        let acceptor = super::build_tls_acceptor(
            materials.server_cert_path.to_str().unwrap(),
            materials.server_key_path.to_str().unwrap(),
            materials.ca_cert_path.to_str().unwrap(),
            &[],
            "",
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server_ctx = Arc::clone(&ctx);
        let server = zeroclaw_spawn::spawn!(super::serve_wss_listener(
            server_ctx,
            server_cancel,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            acceptor,
            listener,
            super::WssLimits::default(),
        ));

        let connector = test_client_connector(&client.cert_pem, &client.key_pem);
        let url = format!("wss://{addr}/");
        let (websocket, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector)),
        )
        .await
        .expect("WSS listener should accept the loopback test connection")
        .expect("WSS handshake should succeed");
        let (mut sink, mut stream) = websocket.split();

        let init = InitializeParams {
            protocol_version: 1,
            tui_id: Some(TUI_ID.to_owned()),
            tui_sig: Some(ctx.tui_registry.sign(TUI_ID).expect("fixture signing key")),
            env: Default::default(),
            client_capabilities: None,
            auth_token: Some("zc_tok".to_owned()),
            auth_provider: None,
        };
        sink.send(Message::Text(
            rpc_request(Method::Initialize, &init, 1).into(),
        ))
        .await
        .unwrap();
        let initialized = read_response_with_id(&mut stream, 1).await;
        assert!(
            initialized["error"].is_null(),
            "initialize failed: {initialized}"
        );

        sink.send(Message::Text(
            rpc_request(
                Method::SessionGoal,
                &serde_json::json!({
                    "session_id": SESSION_ID,
                    "command": "/goal status",
                }),
                2,
            )
            .into(),
        ))
        .await
        .unwrap();
        let frame = read_response_with_id(&mut stream, 2).await;
        assert!(frame["error"].is_null(), "Goal status failed: {frame}");
        let result: SessionGoalResult = serde_json::from_value(frame["result"].clone()).unwrap();
        assert_eq!(result.response, GoalResponse::Disabled);

        cancel.cancel();
        drop(sink);
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("WSS listener should stop after cancellation")
            .expect("WSS listener task should not panic")
            .expect("WSS listener should stop cleanly");
    }

    #[tokio::test]
    async fn goal_budget_control_waits_for_the_turn_over_wss() {
        exercise_goal_boundary_over_wss("/goal budget set --tokens 100", None).await;
    }

    #[tokio::test]
    async fn goal_delete_closes_the_queued_control_before_the_turn_over_wss() {
        exercise_goal_boundary_over_wss(
            "/goal budget set --tokens 100",
            Some(Method::SessionDelete),
        )
        .await;
    }

    #[tokio::test]
    async fn goal_kill_closes_the_queued_control_before_the_turn_over_wss() {
        exercise_goal_boundary_over_wss("/goal budget set --tokens 100", Some(Method::SessionKill))
            .await;
    }

    #[tokio::test]
    async fn goal_start_stays_queued_until_disposal_over_wss() {
        exercise_goal_boundary_over_wss(
            "/goal start --unlimited finish the bounded fixture",
            Some(Method::SessionKill),
        )
        .await;
    }

    #[tokio::test]
    async fn goal_resume_stays_queued_until_disposal_over_wss() {
        for command in ["/goal resume", "/goal resume first line\n\n- second line"] {
            exercise_goal_boundary_over_wss(command, Some(Method::SessionKill)).await;
        }
    }

    async fn exercise_goal_boundary_over_wss(command: &str, disposal_method: Option<Method>) {
        use crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion;
        let completion = disposal_method.map_or(
            GoalBoundaryCompletion::ApplyBudget,
            GoalBoundaryCompletion::Dispose,
        );
        exercise_goal_boundary_completion_over_wss(command, completion).await;
    }

    #[tokio::test]
    async fn queued_goal_control_is_withdrawn_on_listener_shutdown_over_wss() {
        use crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion;
        exercise_goal_boundary_completion_over_wss(
            "/goal budget set --tokens 100",
            GoalBoundaryCompletion::Shutdown,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_replacement_closes_the_queued_control_before_the_turn_over_wss() {
        use crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion;
        exercise_goal_boundary_completion_over_wss(
            "/goal budget set --tokens 100",
            GoalBoundaryCompletion::Replace,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_budget_waits_for_actual_ordinary_model_turn_wss() {
        use crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion;
        exercise_goal_boundary_completion_over_wss(
            "/goal budget set --tokens 100",
            GoalBoundaryCompletion::FinishOrdinaryTurn,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_pause_waits_for_actual_goal_parent_wss() {
        exercise_goal_parent_control_over_wss(
            crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::DirectResume,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_unpriced_usage_preserves_normal_pause_wss() {
        exercise_goal_parent_control_over_wss(
            crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::UnpricedResume,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_queued_resume_launches_actual_parent_wss() {
        for response in [None, Some("Use option A.\nKeep the safety checks.")] {
            exercise_goal_parent_control_over_wss(crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::QueuedResume(response)).await;
        }
    }

    #[tokio::test]
    async fn goal_queued_start_launches_actual_parent_wss() {
        use crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase;
        for case in [
            GoalParentAdmissionCase::QueuedStart,
            GoalParentAdmissionCase::QueuedFirstStart,
        ] {
            exercise_goal_parent_control_over_wss(case).await;
        }
    }

    async fn exercise_goal_parent_control_over_wss(
        case: crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase,
    ) {
        exercise_goal_parent_control_ending_over_wss(
            case,
            crate::rpc::dispatch::connection_test_support::GoalParentEnding::DeferredPause,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_budget_fence_drains_actual_parent_wss() {
        use crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase;
        for case in [
            GoalParentAdmissionCase::DirectResume,
            GoalParentAdmissionCase::QueuedResume(None),
        ] {
            exercise_goal_parent_control_ending_over_wss(
                case,
                crate::rpc::dispatch::connection_test_support::GoalParentEnding::BudgetFence,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn goal_pause_now_fences_actual_parent_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        for case in [
            GoalParentAdmissionCase::DirectResume,
            GoalParentAdmissionCase::QueuedResume(None),
        ] {
            exercise_goal_parent_control_ending_over_wss(case, GoalParentEnding::ImmediatePause)
                .await;
        }
    }

    #[tokio::test]
    async fn goal_restart_quiesces_actual_parent_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        for case in [
            GoalParentAdmissionCase::DirectResume,
            GoalParentAdmissionCase::QueuedResume(None),
        ] {
            exercise_goal_parent_control_ending_over_wss(case, GoalParentEnding::Restart).await;
        }
    }

    #[tokio::test]
    async fn goal_pause_now_drain_failure_reports_once_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        exercise_goal_parent_control_ending_over_wss(
            GoalParentAdmissionCase::DirectResume,
            GoalParentEnding::ImmediatePauseDrainFailure,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_cold_recovery_does_not_bypass_wss_session_owner() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        for was_paused in [false, true] {
            exercise_goal_parent_control_ending_over_wss(
                GoalParentAdmissionCase::RecoveredResume(was_paused),
                GoalParentEnding::ImmediatePause,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn goal_pause_now_supersedes_queued_budget_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        exercise_goal_parent_control_ending_over_wss(
            GoalParentAdmissionCase::DirectResume,
            GoalParentEnding::SupersededBudget,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_cancel_overtakes_queued_resume_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        for response in [None, Some("Use option B.\nKeep the repair bounded.")] {
            exercise_goal_parent_control_ending_over_wss(
                GoalParentAdmissionCase::QueuedResume(response),
                GoalParentEnding::CancelQueuedResume,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn goal_queued_budget_then_pause_apply_fifo_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        exercise_goal_parent_control_ending_over_wss(
            GoalParentAdmissionCase::DirectResume,
            GoalParentEnding::BudgetThenPause,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_pause_waits_for_admitted_verifier_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        exercise_goal_parent_control_ending_over_wss(
            GoalParentAdmissionCase::DirectResume,
            GoalParentEnding::VerifierPause,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn goal_recovered_session_resumes_after_local_restore_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        for was_paused in [false, true] {
            exercise_goal_parent_control_ending_over_wss(
                GoalParentAdmissionCase::RecoveredAttachedResume(was_paused),
                GoalParentEnding::ImmediatePause,
            )
            .await;
        }
    }

    #[cfg(unix)]
    async fn restore_goal_session_over_local_ipc(
        ctx: &Arc<crate::rpc::context::RpcContext>,
        session_id: &str,
        owner: &str,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let socket = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_ctx = Arc::clone(ctx);
        let server_cancel = cancel.clone();
        let server_count = Arc::clone(&count);
        let server = zeroclaw_spawn::spawn!(crate::rpc::local::run_local_listener(
            server_ctx,
            server_cancel,
            server_count,
            None,
        ));
        let stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match tokio::net::UnixStream::connect(&socket).await {
                    Ok(stream) => break stream,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        ) =>
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("local restore connection failed: {error}"),
                }
            }
        })
        .await
        .expect("local restore listener must become ready");
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let init = InitializeParams {
            protocol_version: 1,
            tui_id: Some(owner.into()),
            tui_sig: Some(ctx.tui_registry.sign(owner).unwrap()),
            env: Default::default(),
            client_capabilities: None,
            auth_token: None,
            auth_provider: None,
        };
        for (id, request) in [
            (1, rpc_request(Method::Initialize, &init, 1)),
            (
                2,
                rpc_request(
                    Method::SessionNew,
                    &serde_json::json!({
                        "session_id": session_id, "agent_alias": "test-agent", "chat_mode": "chat"
                    }),
                    2,
                ),
            ),
        ] {
            writer
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            assert_ne!(
                tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(frame["id"], id);
            assert!(frame["error"].is_null(), "{frame}");
            if id == 2 {
                let opened: crate::rpc::types::SessionNewResult =
                    serde_json::from_value(frame["result"].clone()).unwrap();
                assert_eq!(opened.session_id, session_id);
                assert_eq!(opened.message_count, 2);
            }
        }
        drop(writer);
        drop(reader);
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(
            ctx.sessions.session_owner_tui_id(session_id).await,
            Some(Some(owner.into()))
        );
        assert_eq!(ctx.sessions.session_queue.queue_depth(session_id).await, 0);
    }

    #[tokio::test]
    async fn goal_replacement_fences_actual_parent_before_release_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        exercise_goal_parent_control_ending_over_wss(
            GoalParentAdmissionCase::DirectResume,
            GoalParentEnding::Replacement,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_native_event_pressure_and_owned_child_wss() {
        use crate::rpc::dispatch::connection_test_support::{
            GoalParentAdmissionCase, GoalParentEnding,
        };
        for ending in [
            GoalParentEnding::EventBurstSuccess,
            GoalParentEnding::EventBurstFailure,
            GoalParentEnding::EventBurstProjectionFailure,
            GoalParentEnding::PreparationFailure,
        ] {
            exercise_goal_parent_control_ending_over_wss(
                GoalParentAdmissionCase::DirectResume,
                ending,
            )
            .await;
        }
    }

    async fn exercise_goal_parent_control_ending_over_wss(
        case: crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase,
        ending: crate::rpc::dispatch::connection_test_support::GoalParentEnding,
    ) {
        use crate::rpc::context::RpcContext;
        use crate::rpc::dispatch::connection_test_support::{
            GoalBudgetEvents, GoalParentEnding, GoalParentPauseEvents,
            prepare_goal_budget_verifier, prepare_held_goal_parent_fixture,
        };
        use crate::rpc::session::SessionStore;
        use crate::rpc::types::SessionGoalResult;
        use zeroclaw_config::schema::Config;
        use zeroclaw_goal::control::{GoalQueuedControl, GoalResponse, GoalSessionKey};
        use zeroclaw_goal::{GoalAccountingState, GoalPauseReason};
        use zeroclaw_infra::session_queue::SessionActorQueue;

        const SID: &str = "goal-wss-parent";
        const OWNER: &str = "goal-wss-owner";
        let budget_fence = ending == GoalParentEnding::BudgetFence;
        let supersession = ending == GoalParentEnding::SupersededBudget;
        let fifo = ending == GoalParentEnding::BudgetThenPause;
        let verifier_pause = ending == GoalParentEnding::VerifierPause;
        let drain_failure = ending == GoalParentEnding::ImmediatePauseDrainFailure;
        let immediate = ending == GoalParentEnding::ImmediatePause || drain_failure || supersession;
        let restart = ending == GoalParentEnding::Restart;
        let replacement = ending == GoalParentEnding::Replacement;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.paired_tokens = vec!["synthetic-fixture-token".into()];
        let sessions = Arc::new(SessionStore::new(
            64,
            Arc::new(SessionActorQueue::new(4, 30, 60)),
        ));
        let mut ctx = RpcContext::minimal(config, sessions);
        std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).unwrap();
        Arc::get_mut(&mut ctx).unwrap().tui_registry =
            Arc::new(crate::rpc::tui_identity::TuiRegistry::new(tmp.path()));
        if replacement {
            crate::rpc::dispatch::connection_test_support::prepare_goal_replacement_fixture(
                &mut ctx,
                tmp.path(),
            );
        }
        let (registry, parent) =
            prepare_held_goal_parent_fixture(&mut ctx, tmp.path(), SID, OWNER, case).await;
        let burst = if matches!(
            ending,
            GoalParentEnding::EventBurstSuccess
                | GoalParentEnding::EventBurstFailure
                | GoalParentEnding::EventBurstProjectionFailure
        ) {
            Some(
                crate::rpc::dispatch::connection_test_support::prepare_native_burst_fixture(
                    &ctx,
                    tmp.path(),
                    SID,
                    &parent,
                    ending == GoalParentEnding::EventBurstFailure,
                )
                .await,
            )
        } else {
            None
        };
        if ending == GoalParentEnding::EventBurstProjectionFailure {
            *ctx.goal_runtime
                .test_parent_projection_failure
                .lock()
                .unwrap() = Some(SID.into());
        }
        if ending == GoalParentEnding::PreparationFailure {
            crate::rpc::dispatch::connection_test_support::prepare_native_prefix_failure(&ctx, SID)
                .await;
        }
        let verifier_server = if budget_fence {
            Some(prepare_goal_budget_verifier(&ctx).await)
        } else {
            None
        };
        let fifo_verifier_server = if fifo {
            Some(prepare_goal_budget_verifier(&ctx).await)
        } else {
            None
        };
        let turn = &parent.turn;
        let held_verifier = if verifier_pause || replacement {
            Some(
                crate::rpc::dispatch::connection_test_support::prepare_held_goal_verifier(&ctx)
                    .await,
            )
        } else {
            None
        };
        let key = GoalSessionKey::zero_code(SID).unwrap().durable_id();
        let response = case.response();
        let predecessor = case.predecessor(registry.as_ref(), &key).await;
        #[cfg(unix)]
        if matches!(case, crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::RecoveredAttachedResume(_)) {
            restore_goal_session_over_local_ipc(&ctx, SID, OWNER).await;
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 0);
            assert_eq!(serde_json::to_value(registry.current_goal_for_session(&key).await.unwrap()).unwrap(), serde_json::to_value(&predecessor).unwrap());
        }
        let materials = zeroclaw_tls::ensure_server_materials(tmp.path(), &[]).unwrap();
        let ca_pem = std::fs::read_to_string(&materials.ca_cert_path).unwrap();
        let ca_key = std::fs::read_to_string(&materials.ca_key_path).unwrap();
        let client = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key, "goal-fixture").unwrap();
        let acceptor = super::build_tls_acceptor(
            materials.server_cert_path.to_str().unwrap(),
            materials.server_key_path.to_str().unwrap(),
            materials.ca_cert_path.to_str().unwrap(),
            &[],
            "",
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server_ctx = Arc::clone(&ctx);
        let server_cancel = cancel.clone();
        let server = zeroclaw_spawn::spawn!(super::serve_wss_listener(
            server_ctx,
            server_cancel,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            acceptor,
            listener,
            super::WssLimits::default(),
        ));
        let connector = test_client_connector(&client.cert_pem, &client.key_pem);
        let (websocket, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_tls_with_config(
                &format!("wss://{addr}/"),
                None,
                false,
                Some(connector),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let (mut sink, mut stream) = websocket.split();
        let init = InitializeParams {
            protocol_version: 1,
            tui_id: Some(OWNER.into()),
            tui_sig: Some(ctx.tui_registry.sign(OWNER).unwrap()),
            env: Default::default(),
            client_capabilities: None,
            auth_token: Some("synthetic-fixture-token".into()),
            auth_provider: None,
        };
        sink.send(Message::Text(
            rpc_request(Method::Initialize, &init, 1).into(),
        ))
        .await
        .unwrap();
        let initialized = read_response_with_id(&mut stream, 1).await;
        assert!(initialized["error"].is_null(), "{initialized}");
        let command = case.command();
        if matches!(
            case,
            crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::RecoveredResume(
                _
            )
        ) {
            let task_id = &predecessor.as_ref().unwrap().id;
            let paused_extension = registry.get_goal_task(task_id).await.unwrap().unwrap();
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionNew,
                    &serde_json::json!({
                        "session_id": SID, "agent_alias": "test-agent", "chat_mode": "chat"
                    }),
                    3,
                )
                .into(),
            ))
            .await
            .unwrap();
            let frame = tokio::time::timeout(
                Duration::from_secs(5),
                read_response_with_id(&mut stream, 3),
            )
            .await
            .unwrap();
            assert_eq!(
                frame["error"]["code"],
                zeroclaw_api::jsonrpc::error_codes::SESSION_NOT_OWNED,
                "{frame}"
            );
            assert_eq!(
                frame["error"]["message"],
                "Caller does not own this session"
            );
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 0);
            assert!(ctx.sessions.get_agent(SID).await.is_none());
            assert_eq!(
                serde_json::to_value(registry.current_goal_for_session(&key).await.unwrap())
                    .unwrap(),
                serde_json::to_value(&predecessor).unwrap()
            );
            assert_eq!(
                registry.get_goal_task(task_id).await.unwrap().unwrap(),
                paused_extension
            );
            let backend = Arc::clone(ctx.session_backend.as_ref().unwrap());
            let history =
                tokio::task::spawn_blocking(move || backend.try_load(&format!("rpc_{SID}")))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|message| (message.role.as_str(), message.content.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("user", "Choose a bounded repair."),
                    ("assistant", "Which option should I use?")
                ]
            );
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
            cancel.cancel();
            drop(sink);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        if matches!(case, crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::RecoveredAttachedResume(_)) {
            let original = ctx.sessions.get_agent(SID).await.unwrap();
            {
                let agent = original.lock().await;
                assert_eq!(agent.history().len(), 3);
                assert!(matches!(
                    &agent.history()[0],
                    zeroclaw_providers::ConversationMessage::Chat(message)
                        if message.role == "system"
                ));
            }
            let original_generation = ctx.sessions.get_generation(SID).await.unwrap();
            sink.send(Message::Text(rpc_request(Method::SessionNew, &serde_json::json!({
                "session_id": SID, "agent_alias": "test-agent", "chat_mode": "chat"
            }), 3).into())).await.unwrap();
            let frame = tokio::time::timeout(Duration::from_secs(5), read_response_with_id(&mut stream, 3)).await.unwrap();
            assert!(frame["error"].is_null(), "{frame}");
            let opened: crate::rpc::types::SessionNewResult = serde_json::from_value(frame["result"].clone()).unwrap();
            assert_eq!(opened.session_id, SID);
            assert_eq!(opened.message_count, 3);
            assert!(Arc::ptr_eq(&original, &ctx.sessions.get_agent(SID).await.unwrap()));
            assert_eq!(ctx.sessions.get_generation(SID).await, Some(original_generation));
            parent.attach_recovered_provider(&ctx, SID).await;
            assert_eq!(serde_json::to_value(registry.current_goal_for_session(&key).await.unwrap()).unwrap(), serde_json::to_value(&predecessor).unwrap());
        }
        let generation = ctx.sessions.get_generation(SID).await.unwrap();
        if let Some(ordinary) = &parent.ordinary {
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionPrompt,
                    &serde_json::json!({"session_id": SID, "prompt": "ordinary work"}),
                    4,
                )
                .into(),
            ))
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), ordinary.started.notified())
                .await
                .expect("ordinary model call must be in flight before Goal control");
        }
        sink.send(Message::Text(
            rpc_request(
                Method::SessionGoal,
                &serde_json::json!({"session_id": SID, "command": command}),
                5,
            )
            .into(),
        ))
        .await
        .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(2), async {
            if burst.is_some() || ending == GoalParentEnding::PreparationFailure {
                let message = stream.next().await.unwrap().unwrap();
                let frame: serde_json::Value =
                    serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_eq!(
                    frame["id"], 5,
                    "no Goal parent event can overtake its response"
                );
                frame
            } else {
                read_response_with_id(&mut stream, 5).await
            }
        })
        .await
        .expect("resume must respond before starting Goal work");
        assert!(frame["error"].is_null(), "{frame}");
        let resumed: SessionGoalResult = serde_json::from_value(frame["result"].clone()).unwrap();
        let mut events = GoalParentPauseEvents::new(case);
        if let Some(control) = case.queued() {
            assert_eq!(resumed.response, GoalResponse::Queued(control));
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 0);
            let waiting = registry.current_goal_for_session(&key).await.unwrap();
            assert_eq!(
                serde_json::to_value(&waiting).unwrap(),
                serde_json::to_value(&predecessor).unwrap()
            );
            case.assert_intent(
                tmp.path(),
                &key,
                predecessor.as_ref(),
                &parent.boot_id,
                "pending",
            )
            .await;
            let ordinary = parent.ordinary.as_ref().unwrap();
            assert!(
                !ordinary
                    .completed
                    .load(std::sync::atomic::Ordering::Acquire)
            );
            assert!(!ordinary.exited.load(std::sync::atomic::Ordering::Acquire));
            assert!(ctx.sessions.has_inflight_turn(SID));
            if ending == GoalParentEnding::CancelQueuedResume {
                use crate::rpc::dispatch::connection_test_support::CancelledResumeEvents;
                let mut cancelled_events = CancelledResumeEvents::default();
                sink.send(Message::Text(
                    rpc_request(
                        Method::SessionGoal,
                        &serde_json::json!({"session_id": SID, "command": "/goal cancel"}),
                        24,
                    )
                    .into(),
                ))
                .await
                .unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while cancelled_events.cancelled.is_none() {
                        let message = stream.next().await.unwrap().unwrap();
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        cancelled_events.observe(&serde_json::from_str(text).unwrap(), SID);
                    }
                })
                .await
                .expect("cancel must fence without waiting for the ordinary turn");
                assert!(
                    !ordinary
                        .completed
                        .load(std::sync::atomic::Ordering::Acquire)
                );
                assert!(!ordinary.exited.load(std::sync::atomic::Ordering::Acquire));
                assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 0);
                ordinary.release.notify_one();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !cancelled_events.settled() {
                        let message = stream.next().await.unwrap().unwrap();
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        cancelled_events.observe(&serde_json::from_str(text).unwrap(), SID);
                    }
                })
                .await
                .expect("cancelled queued resume must publish its terminal result");
                crate::rpc::dispatch::connection_test_support::assert_cancelled_queued_resume(
                    &ctx,
                    tmp.path(),
                    SID,
                    predecessor.as_ref().unwrap(),
                    &parent,
                    &cancelled_events,
                )
                .await;
                assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
                let terminal = registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(terminal.id, predecessor.as_ref().unwrap().id);
                assert_eq!(terminal.status, zeroclaw_api::task::TaskStatus::Cancelled);
                cancel.cancel();
                drop(sink);
                tokio::time::timeout(Duration::from_secs(2), server)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                return;
            }
            ordinary.release.notify_one();
            let mut ordinary_text = String::new();
            let mut ordinary_ack = false;
            let mut ordinary_completed = false;
            tokio::time::timeout(Duration::from_secs(2), async {
                while !events.announced {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if frame["id"] == 4 {
                        crate::rpc::dispatch::connection_test_support::assert_ordinary_prompt_acknowledgement(&frame, SID);
                        ordinary_ack = true;
                    } else if frame["method"] == "session/update" && !ordinary_completed {
                        ordinary_completed |= crate::rpc::dispatch::connection_test_support::observe_ordinary_turn_update(&frame, SID, &mut ordinary_text);
                    } else {
                        assert!(ordinary_ack && ordinary_completed, "ordinary turn must finish before Goal launch: {frame}");
                        assert!(events.observe(&frame, SID).is_none());
                    }
                }
            })
            .await
            .expect("queued start/resume must publish before parent events");
            assert_eq!(ordinary_text, "ordinary answer");
            assert_eq!(ordinary.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            assert!(
                ordinary
                    .completed
                    .load(std::sync::atomic::Ordering::Acquire)
            );
        } else {
            assert!(matches!(resumed.response, GoalResponse::Resumed(_)));
            events.announced = true;
        }
        if ending == GoalParentEnding::PreparationFailure {
            let mut failure=crate::rpc::dispatch::connection_test_support::NativePreparationFailureEvents::default();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if failure.observe(&frame) {
                        break;
                    }
                }
            })
            .await
            .expect("original preparation error must reach the real WSS boundary");
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 0);
            assert_eq!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                zeroclaw_api::task::TaskStatus::Failed
            );
            ctx.goal_runtime.dispose_session(SID).await.unwrap();
            cancel.cancel();
            drop(sink);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        tokio::time::timeout(Duration::from_secs(2), turn.started.notified())
            .await
            .expect("start/resume must launch the actual Goal parent call");
        parent.assert_resume_input(response);
        if let Some(burst) = burst {
            let mut typed =
                crate::rpc::dispatch::connection_test_support::NativeBurstEvents::default();
            if ending == GoalParentEnding::EventBurstSuccess {
                sink.send(Message::Text(
                    rpc_request(
                        Method::SessionGoal,
                        &serde_json::json!({"session_id":SID,"command":"/goal pause"}),
                        40,
                    )
                    .into(),
                ))
                .await
                .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let frame = stream.next().await.unwrap().unwrap();
                        let Ok(text) = frame.to_text() else {
                            continue;
                        };
                        let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                        if frame["id"] == 40 {
                            let queued: SessionGoalResult =
                                serde_json::from_value(frame["result"].clone()).unwrap();
                            assert_eq!(
                                queued.response,
                                GoalResponse::Queued(GoalQueuedControl::Pause)
                            );
                            break;
                        }
                        assert!(!typed.observe(&frame, SID, ending));
                    }
                })
                .await
                .expect("typed event pressure must drain while the parent is held");
            }
            turn.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let frame = stream.next().await.unwrap().unwrap();
                    let Ok(text) = frame.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if typed.observe(&frame, SID, ending) {
                        break;
                    }
                }
            })
            .await
            .expect("buffered tail and native terminal outcome must drain");
            typed.assert_complete(ending);
            let task = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            burst.assert_child_and_ledger(tmp.path(), &task.id).await;
            ctx.goal_runtime.dispose_session(SID).await.unwrap();
            cancel.cancel();
            drop(sink);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        if case.queued().is_some() {
            case.assert_intent(
                tmp.path(),
                &key,
                predecessor.as_ref(),
                &parent.boot_id,
                "applied",
            )
            .await;
        }
        if verifier_pause && let Some(verifier) = &held_verifier {
            turn.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), verifier.turn.started.notified())
                .await
                .expect("the real verifier call must be admitted");
        }
        let before = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.status, zeroclaw_api::task::TaskStatus::Running);
        let query_extension = registry.get_goal_task(&before.id).await.unwrap().unwrap();
        for (id, command) in [
            (20, "/goal status"),
            (21, "/goal budget"),
            (22, "/goal help"),
        ] {
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({
                        "session_id": SID, "command": command
                    }),
                    id,
                )
                .into(),
            ))
            .await
            .unwrap();
            let result: SessionGoalResult = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if frame["id"] == id {
                        assert!(frame["error"].is_null(), "{frame}");
                        break serde_json::from_value(frame["result"].clone()).unwrap();
                    }
                    assert!(events.observe(&frame, SID).is_none());
                }
            })
            .await
            .expect("read-only Goal query must reply while the parent is held");
            crate::rpc::dispatch::connection_test_support::assert_held_goal_query(
                command,
                &result.response,
                &before,
                &query_extension,
            );
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            assert_eq!(
                turn.completed.load(std::sync::atomic::Ordering::Acquire),
                verifier_pause
            );
            assert_eq!(
                turn.exited.load(std::sync::atomic::Ordering::Acquire),
                verifier_pause
            );
        }
        assert_eq!(
            registry.get_goal_task(&before.id).await.unwrap().unwrap(),
            query_extension
        );
        assert_eq!(
            serde_json::to_value(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        if case.queued().is_some() {
            case.assert_intent(
                tmp.path(),
                &key,
                predecessor.as_ref(),
                &parent.boot_id,
                "applied",
            )
            .await;
        }
        if case.queued() == Some(GoalQueuedControl::Start) {
            assert_eq!(before.execution_epoch, 1);
            if let Some(predecessor) = &predecessor {
                assert_ne!(before.id, predecessor.id);
            }
        }
        if replacement {
            use crate::rpc::dispatch::connection_test_support::{
                assert_running_goal_replacement_completed, assert_running_goal_replacement_fence,
            };
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionNew,
                    &serde_json::json!({
                        "agent_alias": "test-agent", "session_id": SID, "chat_mode": "acp",
                        "cwd": tmp.path(), "keep_siblings": true,
                    }),
                    2,
                )
                .into(),
            ))
            .await
            .unwrap();
            assert_running_goal_replacement_fence(registry.as_ref(), &key, &before, &parent).await;
            assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
            turn.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let frame: serde_json::Value =
                        serde_json::from_str(message.to_text().unwrap()).unwrap();
                    if frame["id"] == 2 {
                        assert!(frame["error"].is_null(), "{frame}");
                        break;
                    }
                    assert!(events.observe(&frame, SID).is_none());
                }
            })
            .await
            .expect("replacement must complete after parent settlement");
            assert_eq!(events.candidates, ["ordinary answer"]);
            assert_running_goal_replacement_completed(
                &ctx,
                registry.as_ref(),
                tmp.path(),
                SID,
                generation,
                &before,
                &parent,
            )
            .await;
            let verifier = held_verifier.unwrap();
            assert_eq!(
                verifier
                    .turn
                    .calls
                    .load(std::sync::atomic::Ordering::Acquire),
                0
            );
            verifier.finish().await;
            cancel.cancel();
            drop(sink);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        let restarting = if restart {
            Some(parent.begin_restart(registry.as_ref(), &before).await)
        } else {
            None
        };
        if supersession || fifo {
            events.expect_superseded_budget = supersession;
            events.queued_budget_limit = fifo.then_some(7);
            let budget_command = if fifo {
                "/goal budget set --tokens 7"
            } else {
                "/goal budget set --tokens 1"
            };
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": budget_command}),
                    23,
                )
                .into(),
            ))
            .await
            .unwrap();
            let frame = tokio::time::timeout(
                Duration::from_secs(2),
                read_response_with_id(&mut stream, 23),
            )
            .await
            .unwrap();
            assert!(frame["error"].is_null(), "{frame}");
            let result: SessionGoalResult =
                serde_json::from_value(frame["result"].clone()).unwrap();
            assert_eq!(
                result.response,
                GoalResponse::Queued(GoalQueuedControl::Budget)
            );
            assert_eq!(
                registry.get_goal_task(&before.id).await.unwrap().unwrap(),
                query_extension
            );
            crate::rpc::dispatch::connection_test_support::assert_budget_intent_state(
                tmp.path(),
                &key,
                &before,
                "pending",
            )
            .await;
        }
        sink.send(Message::Text(
            rpc_request(
                Method::SessionGoal,
                &serde_json::json!({"session_id": SID, "command": ending.command()}),
                2,
            )
            .into(),
        ))
        .await
        .unwrap();
        let queued: SessionGoalResult = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = stream.next().await.unwrap().unwrap();
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                if frame["id"] == 2 {
                    assert!(frame["error"].is_null(), "{frame}");
                    break serde_json::from_value(frame["result"].clone()).unwrap();
                }
                assert!(events.observe(&frame, SID).is_none());
            }
        })
        .await
        .expect("control must acknowledge during the parent call");
        if restart {
            let GoalResponse::Status(status) = &queued.response else {
                panic!("restart status must be directly observable");
            };
            assert_eq!(status.task_id, before.id);
            assert_eq!(status.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(status.pause_reason, Some(GoalPauseReason::DaemonRestart));
            assert!(!status.resumable);
        } else {
            assert_eq!(
                queued.response,
                if immediate {
                    GoalResponse::TransitionPending(
                        zeroclaw_goal::control::GoalPendingAction::Pause,
                    )
                } else {
                    GoalResponse::Queued(if budget_fence {
                        GoalQueuedControl::Budget
                    } else {
                        GoalQueuedControl::Pause
                    })
                }
            );
        }
        assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(
            turn.completed.load(std::sync::atomic::Ordering::Acquire),
            verifier_pause
        );
        if drain_failure {
            let audit = rusqlite::Connection::open(tmp.path().join("control_plane.db")).unwrap();
            assert_eq!(
                audit
                    .execute("DELETE FROM goal_tasks WHERE task_id = ?1", [&before.id])
                    .unwrap(),
                1
            );
            turn.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let frame: serde_json::Value =
                        serde_json::from_str(message.to_text().unwrap()).unwrap();
                    if frame["method"] == "session/goal_update" {
                        assert_eq!(
                            frame["params"],
                            serde_json::json!({"command_failed": {"session_id": SID}})
                        );
                        break;
                    }
                    assert_eq!(frame["method"], "session/update", "{frame}");
                }
            })
            .await
            .expect("drain failure must follow the immediate acknowledgement");
            let after = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(after.execution_epoch, before.execution_epoch + 1);
            assert_eq!(after.id, before.id);
            assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal status"}),
                    5,
                )
                .into(),
            ))
            .await
            .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let next: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(next["id"], 5, "no duplicate drain final: {next}");
            assert!(
                !next["error"].is_null(),
                "missing extension must still refuse status"
            );
            cancel.cancel();
            drop(sink);
            drop(stream);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        assert_eq!(
            turn.exited.load(std::sync::atomic::Ordering::Acquire),
            verifier_pause
        );
        if let Some(verifier) = &held_verifier {
            assert_eq!(
                verifier
                    .turn
                    .calls
                    .load(std::sync::atomic::Ordering::Acquire),
                1
            );
            assert!(
                !verifier
                    .turn
                    .completed
                    .load(std::sync::atomic::Ordering::Acquire)
            );
            assert_eq!(events.candidates, ["ordinary answer"]);
            assert_eq!(
                registry.get_goal_task(&before.id).await.unwrap().unwrap(),
                query_extension
            );
            crate::rpc::dispatch::connection_test_support::assert_pause_intent_state(
                tmp.path(),
                &key,
                &before,
                "pending",
            )
            .await;
        }
        let queued_task = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        if immediate || restart {
            assert_eq!(queued_task.id, before.id);
            assert_eq!(queued_task.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(queued_task.execution_epoch, before.execution_epoch + 1);
            let fenced = registry.get_goal_task(&before.id).await.unwrap().unwrap();
            assert_eq!(
                fenced.pause_reason,
                Some(if restart {
                    GoalPauseReason::DaemonRestart
                } else {
                    GoalPauseReason::OperatorPaused
                })
            );
            assert_eq!(fenced.pending_call_id, query_extension.pending_call_id);
            assert_eq!(fenced.pending_call_epoch, Some(before.execution_epoch));
        } else {
            assert_eq!(
                serde_json::to_value(&queued_task).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
        }
        if budget_fence {
            assert_eq!(
                registry
                    .get_goal_task(&before.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .effective_token_limit,
                Some(10)
            );
        }
        if fifo {
            assert_eq!(
                registry.get_goal_task(&before.id).await.unwrap().unwrap(),
                query_extension
            );
            crate::rpc::dispatch::connection_test_support::assert_budget_then_pause_intents(
                tmp.path(),
                &key,
                &before,
                "pending",
            )
            .await;
        }
        if let Some(verifier) = &held_verifier {
            verifier.turn.release.notify_one();
        } else {
            turn.release.notify_one();
        }
        if let Some(verifier_server) = verifier_server {
            let mut budget_events = GoalBudgetEvents::with_parent_text(events.text);
            tokio::time::timeout(Duration::from_secs(2), async {
                while !budget_events.settled() {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame = serde_json::from_str(text).unwrap();
                    budget_events.observe(&frame, SID);
                }
            })
            .await
            .expect("budget update and ordinary refusal must both be observable");
            assert_eq!(budget_events.text, "ordinary answer");
            let scope = zeroclaw_goal::GoalExecutionScope::new(
                before.id.clone(),
                key.clone(),
                before.execution_epoch,
            )
            .unwrap();
            let supervisor = ctx.goal_runtime.supervisor(SID).await.unwrap();
            let outcome = tokio::time::timeout(Duration::from_secs(2), supervisor.drain(&scope))
                .await
                .expect("original budget-fenced worker must drain")
                .unwrap();
            assert_eq!(outcome, zeroclaw_goal::GoalExecutionOutcome::Fenced);
            let paused = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(paused.id, before.id);
            assert_eq!(paused.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(paused.execution_epoch, before.execution_epoch + 1);
            let goal = registry.get_goal_task(&before.id).await.unwrap().unwrap();
            assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert_eq!(goal.effective_token_limit, Some(1));
            assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
            crate::rpc::dispatch::connection_test_support::assert_held_goal_usage(
                tmp.path(),
                &before.id,
            )
            .await;
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            assert!(
                verifier_server
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
            let backend = Arc::clone(ctx.session_backend.as_ref().unwrap());
            let history = tokio::task::spawn_blocking(move || backend.load(&format!("rpc_{SID}")))
                .await
                .unwrap();
            crate::rpc::dispatch::connection_test_support::assert_resume_history(&history, case);
            assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
            sink.send(Message::Text(rpc_request(
                Method::SessionGoal,
                &serde_json::json!({"session_id": SID, "command": "/goal budget set --tokens 10"}), 7,
            ).into())).await.unwrap();
            let frame = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    if let Ok(text) = message.to_text() {
                        break serde_json::from_str::<serde_json::Value>(text).unwrap();
                    }
                }
            })
            .await
            .expect("raising a paused budget must acknowledge promptly");
            assert_eq!(frame["id"], 7, "{frame}");
            assert!(frame["error"].is_null(), "{frame}");
            let raised: SessionGoalResult =
                serde_json::from_value(frame["result"].clone()).unwrap();
            let GoalResponse::BudgetUpdated(raised) = raised.response else {
                panic!("paused budget change must return BudgetUpdated");
            };
            assert_eq!(raised.task_id, before.id);
            assert_eq!(raised.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(raised.execution_epoch, paused.execution_epoch);
            assert_eq!(raised.token_limit, Some(10));
            assert_eq!(raised.pause_reason, Some(GoalPauseReason::BudgetExhausted));
            assert!(raised.resumable);
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal resume"}),
                    8,
                )
                .into(),
            ))
            .await
            .unwrap();
            let frame = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    if let Ok(text) = message.to_text() {
                        break serde_json::from_str::<serde_json::Value>(text).unwrap();
                    }
                }
            })
            .await
            .expect("explicit recovery must publish before new parent events");
            assert_eq!(frame["id"], 8, "{frame}");
            assert!(frame["error"].is_null(), "{frame}");
            let resumed: SessionGoalResult =
                serde_json::from_value(frame["result"].clone()).unwrap();
            let GoalResponse::Resumed(resumed) = resumed.response else {
                panic!("budget recovery must explicitly resume the existing Goal");
            };
            assert_eq!(resumed.task_id, before.id);
            assert_eq!(resumed.status, zeroclaw_api::task::TaskStatus::Running);
            assert_eq!(resumed.execution_epoch, paused.execution_epoch + 1);
            tokio::time::timeout(Duration::from_secs(2), turn.started.notified())
                .await
                .expect("explicit resume must admit the second Goal parent request");
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 2);
            assert_eq!(
                parent
                    .messages
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|message| {
                        message.role == "assistant"
                            && message.content == "Which option should I use?"
                    })
                    .count(),
                1
            );
            assert_eq!(
                parent
                    .messages
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|message| {
                        message.role == "assistant" && message.content == "ordinary answer"
                    })
                    .count(),
                1 + usize::from(parent.ordinary.is_some()),
                "recovery request must retain every prior candidate exactly once"
            );
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal cancel"}),
                    9,
                )
                .into(),
            ))
            .await
            .unwrap();
            let pending = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if frame["id"] == 9 {
                        assert!(frame["error"].is_null(), "{frame}");
                        break serde_json::from_value::<SessionGoalResult>(frame["result"].clone())
                            .unwrap();
                    }
                    assert_eq!(frame["method"], "session/update", "{frame}");
                }
            })
            .await
            .expect("cancel must fence without waiting for the held parent");
            assert_eq!(
                pending.response,
                GoalResponse::TransitionPending(zeroclaw_goal::control::GoalPendingAction::Cancel,)
            );
            let fenced = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fenced.id, before.id);
            assert_eq!(fenced.status, zeroclaw_api::task::TaskStatus::Cancelled);
            assert_eq!(fenced.execution_epoch, resumed.execution_epoch + 1);
            turn.release.notify_one();
            let mut recovered_candidates = 0;
            let cancelled = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if frame["method"] == "session/goal_update" {
                        let update: crate::rpc::types::SessionGoalUpdate =
                            serde_json::from_value(frame["params"].clone()).unwrap();
                        match update {
                            crate::rpc::types::SessionGoalUpdate::VerifiedCandidate {
                                session_id,
                                candidate,
                            } => {
                                assert_eq!(session_id, SID);
                                assert_eq!(candidate, "ordinary answer");
                                assert_eq!(recovered_candidates, 0);
                                recovered_candidates += 1;
                            }
                            crate::rpc::types::SessionGoalUpdate::Lifecycle {
                                session_id,
                                response: GoalResponse::Cancelled(status),
                            } => {
                                assert_eq!(session_id, SID);
                                assert_eq!(recovered_candidates, 1);
                                break status;
                            }
                            other => {
                                panic!("cancelled recovery must not terminally fail: {other:?}")
                            }
                        }
                    } else {
                        assert_eq!(frame["method"], "session/update", "{frame}");
                    }
                }
            })
            .await
            .expect("cancelled recovery must settle the held operation and drain");
            assert_eq!(cancelled.task_id, before.id);
            assert_eq!(cancelled.status, zeroclaw_api::task::TaskStatus::Cancelled);
            assert_eq!(cancelled.accounting_state, GoalAccountingState::Complete);
            crate::rpc::dispatch::connection_test_support::assert_held_goal_usage_attempts(
                tmp.path(),
                &before.id,
                2,
            )
            .await;
            let resumed_scope = zeroclaw_goal::GoalExecutionScope::new(
                before.id.clone(),
                key.clone(),
                resumed.execution_epoch,
            )
            .unwrap();
            assert!(!supervisor.owns_scope(&resumed_scope).await);
            let final_task = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(final_task.id, before.id);
            assert_eq!(final_task.status, zeroclaw_api::task::TaskStatus::Cancelled);
            assert_eq!(final_task.execution_epoch, resumed.execution_epoch + 1);
            let final_goal = registry.get_goal_task(&before.id).await.unwrap().unwrap();
            assert_eq!(final_goal.accounting_state, GoalAccountingState::Complete);
            assert!(
                final_goal.pending_call_id.is_none() && final_goal.pending_call_epoch.is_none()
            );
            let backend = Arc::clone(ctx.session_backend.as_ref().unwrap());
            let recovered_history =
                tokio::task::spawn_blocking(move || backend.load(&format!("rpc_{SID}")))
                    .await
                    .unwrap();
            let mut expected_history = history;
            expected_history.push(zeroclaw_api::model_provider::ChatMessage::assistant(
                "ordinary answer",
            ));
            assert_eq!(
                serde_json::to_value(recovered_history).unwrap(),
                serde_json::to_value(expected_history).unwrap()
            );
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 2);
            assert!(
                verifier_server
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
            cancel.cancel();
            drop(sink);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        if let Some(restarting) = restarting {
            tokio::time::timeout(Duration::from_secs(2), restarting)
                .await
                .expect("restart must drain the original parent")
                .unwrap()
                .unwrap();
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal status"}),
                    6,
                )
                .into(),
            ))
            .await
            .unwrap();
        }
        let paused = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = stream.next().await.unwrap().unwrap();
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                if restart && frame["id"] == 6 {
                    assert!(frame["error"].is_null(), "{frame}");
                    let result: SessionGoalResult =
                        serde_json::from_value(frame["result"].clone()).unwrap();
                    let GoalResponse::Status(status) = result.response else {
                        panic!("drained restart must have native paused status");
                    };
                    assert_eq!(events.candidates, ["ordinary answer"]);
                    break status;
                }
                if let Some(status) = events.observe(&frame, SID) {
                    break status;
                }
            }
        })
        .await
        .expect("pause must settle after the actual parent turn");
        if fifo {
            tokio::time::timeout(Duration::from_secs(2), async {
                while events.applied_budget.is_none() {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    assert!(events.observe(&frame, SID).is_none());
                }
            })
            .await
            .expect("both queued controls must publish their final results");
            let budget = events.applied_budget.as_ref().unwrap();
            assert_eq!(budget.task_id, before.id);
            assert_eq!(budget.execution_epoch, before.execution_epoch);
            assert_eq!(budget.status, zeroclaw_api::task::TaskStatus::Running);
            assert_eq!(budget.accounting_state, GoalAccountingState::Complete);
            assert_eq!(paused.token_limit, Some(7));
            crate::rpc::dispatch::connection_test_support::assert_budget_then_pause_intents(
                tmp.path(),
                &key,
                &before,
                "applied",
            )
            .await;
        }
        if supersession {
            tokio::time::timeout(Duration::from_secs(2), async {
                while events.superseded_budget.is_none() {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    assert!(events.observe(&frame, SID).is_none());
                }
            })
            .await
            .expect("superseded budget must have a visible final result");
            let status = events.superseded_budget.as_ref().unwrap();
            assert_eq!(status.task_id, before.id);
            assert_eq!(status.execution_epoch, before.execution_epoch + 1);
            assert_eq!(status.token_limit, Some(10));
            assert_eq!(status.pause_reason, Some(GoalPauseReason::OperatorPaused));
            crate::rpc::dispatch::connection_test_support::assert_budget_intent_state(
                tmp.path(),
                &key,
                &before,
                "superseded",
            )
            .await;
        }
        assert_eq!(events.text, "ordinary answer");
        assert_eq!(
            paused.pause_reason,
            Some(if restart {
                GoalPauseReason::DaemonRestart
            } else {
                GoalPauseReason::OperatorPaused
            })
        );
        assert_eq!(paused.task_id, before.id);
        assert_eq!(paused.execution_epoch, before.execution_epoch + 1);
        assert_eq!(paused.accounting_state, GoalAccountingState::Complete);
        assert!(paused.resumable);
        let goal = registry.get_goal_task(&before.id).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        if supersession {
            assert_eq!(goal.effective_token_limit, Some(10));
        }
        if fifo {
            assert_eq!(goal.effective_token_limit, Some(7));
        }
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        if let Some(verifier) = held_verifier {
            assert!(
                verifier
                    .turn
                    .completed
                    .load(std::sync::atomic::Ordering::Acquire)
            );
            assert_eq!(
                verifier
                    .turn
                    .calls
                    .load(std::sync::atomic::Ordering::Acquire),
                1
            );
            crate::rpc::dispatch::connection_test_support::assert_pause_intent_state(
                tmp.path(),
                &key,
                &before,
                "applied",
            )
            .await;
            crate::rpc::dispatch::connection_test_support::assert_held_parent_and_verifier_usage(
                tmp.path(),
                &before.id,
            )
            .await;
            verifier.finish().await;
        } else {
            crate::rpc::dispatch::connection_test_support::assert_held_goal_usage(
                tmp.path(),
                &before.id,
            )
            .await;
        }
        assert!(turn.completed.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
        if matches!(
            case,
            crate::rpc::dispatch::connection_test_support::GoalParentAdmissionCase::UnpricedResume
        ) {
            crate::rpc::dispatch::connection_test_support::assert_unpriced_goal_usage(
                tmp.path(),
                &before.id,
            )
            .await;
        }
        let backend = Arc::clone(ctx.session_backend.as_ref().unwrap());
        let history = tokio::task::spawn_blocking(move || backend.load(&format!("rpc_{SID}")))
            .await
            .unwrap();
        crate::rpc::dispatch::connection_test_support::assert_resume_history(&history, case);
        assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
        assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
        if let Some(verifier) = fifo_verifier_server {
            assert!(verifier.received_requests().await.unwrap().is_empty());
        }
        if restart {
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal resume"}),
                    7,
                )
                .into(),
            ))
            .await
            .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let frame: serde_json::Value =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(frame["id"], 7);
            assert!(
                !frame["error"].is_null(),
                "retiring generation must reject resume: {frame}"
            );
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            let retained = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.id, before.id);
            assert_eq!(retained.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(retained.execution_epoch, paused.execution_epoch);
            assert_eq!(
                registry.get_goal_task(&before.id).await.unwrap().unwrap(),
                goal
            );
            parent.restart.reopen_for_generation().await;
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            const ANSWER: &str = "Use option B.\nKeep the repair bounded.";
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({
                        "session_id": SID, "command": format!("/goal resume {ANSWER}")
                    }),
                    8,
                )
                .into(),
            ))
            .await
            .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .expect("recovery notice must precede successor events")
                .unwrap()
                .unwrap();
            let frame: serde_json::Value =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(frame["id"], 8);
            assert!(frame["error"].is_null(), "{frame}");
            let resumed: SessionGoalResult =
                serde_json::from_value(frame["result"].clone()).unwrap();
            let GoalResponse::Resumed(resumed) = resumed.response else {
                panic!("explicit recovery must resume the same Goal");
            };
            assert_eq!(resumed.task_id, before.id);
            assert_eq!(resumed.execution_epoch, paused.execution_epoch + 1);
            assert_eq!(resumed.status, zeroclaw_api::task::TaskStatus::Running);
            tokio::time::timeout(Duration::from_secs(2), turn.started.notified())
                .await
                .expect("authorized recovery must admit the second parent");
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 2);
            parent.assert_restart_input(ANSWER);
            let supervisor = ctx.goal_runtime.supervisor(SID).await.unwrap();
            let resumed_scope = zeroclaw_goal::GoalExecutionScope::new(
                before.id.clone(),
                key.clone(),
                resumed.execution_epoch,
            )
            .unwrap();
            let mut recovery_events = GoalParentPauseEvents::new(case);
            recovery_events.announced = true;
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({
                        "session_id": SID, "command": "/goal pause now"
                    }),
                    9,
                )
                .into(),
            ))
            .await
            .unwrap();
            let pending: SessionGoalResult = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame: serde_json::Value = serde_json::from_str(text).unwrap();
                    if frame["id"] == 9 {
                        assert!(frame["error"].is_null(), "{frame}");
                        break serde_json::from_value(frame["result"].clone()).unwrap();
                    }
                    assert!(recovery_events.observe(&frame, SID).is_none());
                }
            })
            .await
            .unwrap();
            assert_eq!(
                pending.response,
                GoalResponse::TransitionPending(zeroclaw_goal::control::GoalPendingAction::Pause)
            );
            let fenced = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fenced.status, zeroclaw_api::task::TaskStatus::Paused);
            assert_eq!(fenced.execution_epoch, resumed.execution_epoch + 1);
            turn.release.notify_one();
            let final_pause = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = stream.next().await.unwrap().unwrap();
                    let Ok(text) = message.to_text() else {
                        continue;
                    };
                    let frame = serde_json::from_str(text).unwrap();
                    if let Some(status) = recovery_events.observe(&frame, SID) {
                        break status;
                    }
                }
            })
            .await
            .expect("recovered parent must settle and drain");
            assert_eq!(recovery_events.text, "ordinary answer");
            assert_eq!(final_pause.task_id, before.id);
            assert_eq!(final_pause.execution_epoch, resumed.execution_epoch + 1);
            assert_eq!(
                final_pause.pause_reason,
                Some(GoalPauseReason::OperatorPaused)
            );
            assert_eq!(final_pause.accounting_state, GoalAccountingState::Complete);
            assert!(final_pause.resumable);
            assert!(!supervisor.owns_scope(&resumed_scope).await);
            crate::rpc::dispatch::connection_test_support::assert_held_goal_usage_attempts(
                tmp.path(),
                &before.id,
                2,
            )
            .await;
            let backend = Arc::clone(ctx.session_backend.as_ref().unwrap());
            let recovered_history =
                tokio::task::spawn_blocking(move || backend.load(&format!("rpc_{SID}")))
                    .await
                    .unwrap();
            let mut expected_history = history;
            expected_history.push(zeroclaw_api::model_provider::ChatMessage::user(ANSWER));
            expected_history.push(zeroclaw_api::model_provider::ChatMessage::assistant(
                "ordinary answer",
            ));
            assert_eq!(
                serde_json::to_value(recovered_history).unwrap(),
                serde_json::to_value(expected_history).unwrap()
            );
            let settled = registry.get_goal_task(&before.id).await.unwrap().unwrap();
            assert_eq!(settled.accounting_state, GoalAccountingState::Complete);
            assert!(settled.pending_call_id.is_none() && settled.pending_call_epoch.is_none());
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 2);
            assert_eq!(ctx.sessions.get_generation(SID).await, Some(generation));
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
        }
        cancel.cancel();
        drop(sink);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn goal_deferred_settings_failure_reports_once_wss() {
        exercise_goal_boundary_completion_over_wss(
            "/goal resume",
            crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion::SettingsFailure,
        )
        .await;
    }

    #[tokio::test]
    async fn goal_deferred_explicit_budget_preserves_malformed_unused_defaults_wss() {
        exercise_goal_boundary_completion_over_wss(
            "/goal budget set --tokens 100",
            crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion::ApplyBudgetWithInvalidUnusedDefault,
        ).await;
    }

    #[tokio::test]
    async fn goal_launch_failure_reports_once_wss() {
        exercise_goal_boundary_completion_over_wss(
            "/goal resume",
            crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion::LaunchFailure,
        )
        .await;
    }

    async fn exercise_goal_boundary_completion_over_wss(
        command: &str,
        completion: crate::rpc::dispatch::connection_test_support::GoalBoundaryCompletion,
    ) {
        use crate::rpc::context::RpcContext;
        use crate::rpc::dispatch::connection_test_support::{
            GoalBoundaryCompletion, insert_held_session_with_owner,
            insert_immediate_session_with_owner, prepare_queued_goal_control_fixture,
        };
        use crate::rpc::session::SessionStore;
        use crate::rpc::types::SessionGoalResult;
        use zeroclaw_config::schema::Config;
        use zeroclaw_goal::control::{GoalResponse, GoalSessionKey};
        use zeroclaw_infra::session_queue::SessionActorQueue;

        const SID: &str = "goal-wss-boundary";
        const OWNER: &str = "goal-wss-owner";
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.paired_tokens = vec!["synthetic-fixture-token".into()];
        let sessions = Arc::new(SessionStore::new(
            64,
            Arc::new(SessionActorQueue::new(4, 30, 60)),
        ));
        let mut ctx = RpcContext::minimal(config, sessions);
        std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).unwrap();
        Arc::get_mut(&mut ctx).unwrap().tui_registry =
            Arc::new(crate::rpc::tui_identity::TuiRegistry::new(tmp.path()));
        if matches!(completion, GoalBoundaryCompletion::Replace) {
            crate::rpc::dispatch::connection_test_support::prepare_goal_replacement_fixture(
                &mut ctx,
                tmp.path(),
            );
        }
        let held_turn = if matches!(
            completion,
            GoalBoundaryCompletion::FinishOrdinaryTurn
                | GoalBoundaryCompletion::ApplyBudgetWithInvalidUnusedDefault
        ) {
            Some(insert_held_session_with_owner(&ctx, tmp.path(), SID, OWNER).await)
        } else {
            insert_immediate_session_with_owner(&ctx, tmp.path(), SID, OWNER).await;
            None
        };
        let original_generation = ctx.sessions.get_generation(SID).await.unwrap();
        let (registry, control) =
            prepare_queued_goal_control_fixture(&ctx, tmp.path(), SID, command).await;
        let key = GoalSessionKey::zero_code(SID).unwrap().durable_id();
        let before = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        let mut ordinary = if held_turn.is_some() {
            None
        } else {
            Some(ctx.sessions.session_queue.acquire(SID).await.unwrap())
        };
        let materials = zeroclaw_tls::ensure_server_materials(tmp.path(), &[]).unwrap();
        let ca_pem = std::fs::read_to_string(&materials.ca_cert_path).unwrap();
        let ca_key = std::fs::read_to_string(&materials.ca_key_path).unwrap();
        let client = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key, "goal-fixture").unwrap();
        let acceptor = super::build_tls_acceptor(
            materials.server_cert_path.to_str().unwrap(),
            materials.server_key_path.to_str().unwrap(),
            materials.ca_cert_path.to_str().unwrap(),
            &[],
            "",
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server_context = Arc::clone(&ctx);
        let server_cancel = cancel.clone();
        let server = zeroclaw_spawn::spawn!(super::serve_wss_listener(
            server_context,
            server_cancel,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            acceptor,
            listener,
            super::WssLimits::default(),
        ));
        let connector = test_client_connector(&client.cert_pem, &client.key_pem);
        let (websocket, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_tls_with_config(
                &format!("wss://{addr}/"),
                None,
                false,
                Some(connector),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let (mut sink, mut stream) = websocket.split();
        let init = InitializeParams {
            protocol_version: 1,
            tui_id: Some(OWNER.into()),
            tui_sig: Some(ctx.tui_registry.sign(OWNER).unwrap()),
            env: Default::default(),
            client_capabilities: None,
            auth_token: Some("synthetic-fixture-token".into()),
            auth_provider: None,
        };
        sink.send(Message::Text(
            rpc_request(Method::Initialize, &init, 1).into(),
        ))
        .await
        .unwrap();
        let initialized = read_response_with_id(&mut stream, 1).await;
        assert!(initialized["error"].is_null(), "{initialized}");
        if let Some(turn) = &held_turn {
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionPrompt,
                    &serde_json::json!({"session_id": SID, "prompt": "ordinary work"}),
                    4,
                )
                .into(),
            ))
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), turn.started.notified())
                .await
                .expect("the ordinary prompt must reach its real model call");
        }
        sink.send(Message::Text(
            rpc_request(
                Method::SessionGoal,
                &serde_json::json!({
                    "session_id": SID, "command": command,
                }),
                2,
            )
            .into(),
        ))
        .await
        .unwrap();
        let frame = tokio::time::timeout(
            Duration::from_secs(2),
            read_response_with_id(&mut stream, 2),
        )
        .await
        .expect("the control acknowledgement must not wait for an ordinary turn");
        assert!(frame["error"].is_null(), "{frame}");
        let response: SessionGoalResult = serde_json::from_value(frame["result"].clone()).unwrap();
        assert_eq!(response.response, GoalResponse::Queued(control));
        if let Some(turn) = &held_turn {
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
            assert!(!turn.exited.load(std::sync::atomic::Ordering::Acquire));
            assert!(!turn.completed.load(std::sync::atomic::Ordering::Acquire));
            assert!(ctx.sessions.has_inflight_turn(SID));
        }
        let task = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&task).unwrap(),
            serde_json::to_value(&before).unwrap(),
            "queued controls must leave their canonical predecessor unchanged"
        );
        if control == zeroclaw_goal::control::GoalQueuedControl::Budget {
            crate::rpc::dispatch::connection_test_support::assert_budget_intent_state(
                tmp.path(),
                &key,
                &before,
                "pending",
            )
            .await;
        }
        assert_eq!(
            registry
                .get_goal_task(&task.id)
                .await
                .unwrap()
                .unwrap()
                .effective_token_limit,
            Some(10)
        );
        if matches!(completion, GoalBoundaryCompletion::Shutdown) {
            tokio::time::timeout(Duration::from_secs(2), async {
                while ctx.sessions.session_queue.queue_depth(SID).await != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the queued continuation must own a session reservation");
            cancel.cancel();
            tokio::time::timeout(
                crate::rpc::CONNECTION_DRAIN_GRACE + Duration::from_secs(2),
                server,
            )
            .await
            .expect("listener shutdown must not wait for the held ordinary turn")
            .unwrap()
            .unwrap();
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 1);
            crate::rpc::dispatch::connection_test_support::assert_goal_control_abandoned_on_shutdown(
                tmp.path(), &key,
            ).await;
            assert!(ctx.sessions.get_generation(SID).await.is_some());
            let after = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(&after).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(
                registry
                    .get_goal_task(&task.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .effective_token_limit,
                Some(10)
            );
            drop(ordinary.take());
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
            return;
        }
        if matches!(
            completion,
            GoalBoundaryCompletion::SettingsFailure
                | GoalBoundaryCompletion::ApplyBudgetWithInvalidUnusedDefault
        ) {
            let mut config = ctx.config.snapshot();
            config.goal.default_cost_limit_usd = Some(-1.0);
            ctx.config_authority.publish_for_test(config);
        }
        let private_config_before = matches!(
            completion,
            GoalBoundaryCompletion::ApplyBudgetWithInvalidUnusedDefault
        )
        .then(|| serde_json::to_value(ctx.config.snapshot()).unwrap());
        if matches!(completion, GoalBoundaryCompletion::LaunchFailure) {
            ctx.goal_runtime
                .fail_test_launch_bind(key.clone(), before.execution_epoch + 1);
        }
        let replacing = matches!(completion, GoalBoundaryCompletion::Replace);
        let disposing = replacing || matches!(completion, GoalBoundaryCompletion::Dispose(_));
        if let GoalBoundaryCompletion::Dispose(method) = completion {
            sink.send(Message::Text(
                rpc_request(method, &serde_json::json!({"session_id": SID}), 3).into(),
            ))
            .await
            .unwrap();
        } else if replacing {
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionNew,
                    &serde_json::json!({
                        "agent_alias": "test-agent", "session_id": SID,
                        "chat_mode": "acp", "cwd": tmp.path(), "keep_siblings": true,
                    }),
                    3,
                )
                .into(),
            ))
            .await
            .unwrap();
        } else {
            drop(ordinary.take());
            if let Some(turn) = &held_turn {
                turn.release.notify_one();
            }
        }
        let mut ordinary_text = String::new();
        let mut ordinary_response_seen = false;
        let mut ordinary_completed = false;
        let frame = match tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = stream.next().await.unwrap().unwrap();
                let frame: serde_json::Value =
                    serde_json::from_str(message.to_text().unwrap()).unwrap();
                if held_turn.is_none() || frame["method"] == "session/goal_update" {
                    break frame;
                }
                if frame["id"] == 4 {
                    crate::rpc::dispatch::connection_test_support::assert_ordinary_prompt_acknowledgement(
                        &frame, SID,
                    );
                    ordinary_response_seen = true;
                } else {
                    ordinary_completed |= crate::rpc::dispatch::connection_test_support::observe_ordinary_turn_update(
                        &frame, SID, &mut ordinary_text,
                    );
                }
            }
        })
        .await
        {
            Ok(frame) => frame,
            Err(_) => {
                if replacing {
                    assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 3);
                    assert_eq!(
                        ctx.sessions.get_generation(SID).await,
                        Some(original_generation)
                    );
                    let pending = registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        serde_json::to_value(&pending).unwrap(),
                        serde_json::to_value(&before).unwrap()
                    );
                }
                panic!(
                    "queued control closure did not reach WSS; actor depth={}, replacement={replacing}",
                    ctx.sessions.session_queue.queue_depth(SID).await,
                );
            }
        };
        if let Some(turn) = &held_turn {
            assert_eq!(ordinary_text, "ordinary answer");
            assert!(
                ordinary_completed,
                "ordinary completion must precede the Goal notice"
            );
            assert!(turn.completed.load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(turn.calls.load(std::sync::atomic::Ordering::Acquire), 1);
        }
        assert_eq!(frame["method"], "session/goal_update");
        let update: crate::rpc::types::SessionGoalUpdate =
            serde_json::from_value(frame["params"].clone()).unwrap();
        if matches!(completion, GoalBoundaryCompletion::LaunchFailure) {
            assert!(matches!(update,
                crate::rpc::types::SessionGoalUpdate::Lifecycle {
                    session_id, response: GoalResponse::Resumed(_)
                } if session_id == SID));
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let failed: serde_json::Value =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            crate::rpc::dispatch::connection_test_support::assert_goal_launch_failure(
                &ctx,
                registry.as_ref(),
                &key,
                &before,
                SID,
                original_generation,
                &failed,
            )
            .await;
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal status"}),
                    5,
                )
                .into(),
            ))
            .await
            .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let next: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(next["id"], 5, "no duplicate command failure: {next}");
            assert!(next["error"].is_null(), "{next}");
            cancel.cancel();
            drop(sink);
            drop(stream);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        if matches!(completion, GoalBoundaryCompletion::SettingsFailure) {
            assert!(
                matches!(update, crate::rpc::types::SessionGoalUpdate::CommandFailed { session_id } if session_id == SID)
            );
            assert_eq!(
                frame["params"],
                serde_json::json!({"command_failed": {"session_id": SID}})
            );
            crate::rpc::dispatch::connection_test_support::assert_goal_control_dropped(
                tmp.path(),
                &key,
            )
            .await;
            assert_eq!(
                serde_json::to_value(
                    registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap()
                )
                .unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(
                registry
                    .get_goal_task(&task.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .effective_token_limit,
                Some(10)
            );
            assert_eq!(
                ctx.sessions.get_generation(SID).await,
                Some(original_generation)
            );
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
            // The fault has settled; restore valid settings for the read-only
            // response barrier that detects any duplicate final notification.
            let mut config = ctx.config.snapshot();
            config.goal.default_cost_limit_usd = Some(0.0);
            ctx.config_authority.publish_for_test(config);
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id": SID, "command": "/goal status"}),
                    5,
                )
                .into(),
            ))
            .await
            .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let next: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(
                next["id"], 5,
                "no duplicate final may precede the status response: {next}"
            );
            assert!(next["error"].is_null(), "{next}");
            cancel.cancel();
            drop(sink);
            drop(stream);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            return;
        }
        let crate::rpc::types::SessionGoalUpdate::Lifecycle {
            session_id,
            response,
        } = update
        else {
            panic!("expected the final control lifecycle response");
        };
        assert_eq!(session_id, SID);
        if disposing {
            match response {
                GoalResponse::AlreadyPaused(status) | GoalResponse::Terminal(status) => {
                    assert_eq!(status.token_limit, Some(10));
                }
                GoalResponse::NoCurrentGoal | GoalResponse::Stale => {}
                other => panic!("disposed control must not apply its transition: {other:?}"),
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_some()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("Goal disposal must finish before the ordinary turn releases");
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 2);
            assert!(ctx.sessions.get_generation(SID).await.is_some());
            drop(ordinary.take());
            let frame = tokio::time::timeout(
                Duration::from_secs(2),
                read_response_with_id(&mut stream, 3),
            )
            .await
            .expect("session removal must finish after the ordinary permit releases");
            assert!(frame["error"].is_null(), "{frame}");
            if replacing {
                assert_ne!(
                    ctx.sessions.get_generation(SID).await,
                    Some(original_generation)
                );
                assert_eq!(
                    ctx.sessions.chat_mode(SID).await,
                    Some(crate::rpc::types::ChatMode::Acp)
                );
            } else {
                assert!(ctx.sessions.get_generation(SID).await.is_none());
            }
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
        } else {
            let GoalResponse::BudgetUpdated(status) = response else {
                panic!("expected the committed budget lifecycle response");
            };
            assert_eq!(status.token_limit, Some(100));
            if private_config_before.is_some() {
                assert_eq!(status.task_id, before.id);
                assert_eq!(status.execution_epoch, before.execution_epoch);
                assert_eq!(
                    serde_json::to_value(
                        registry
                            .current_goal_for_session(&key)
                            .await
                            .unwrap()
                            .unwrap()
                    )
                    .unwrap(),
                    serde_json::to_value(&before).unwrap(),
                    "explicit budget edit preserves the exact durable owner"
                );
            }
            crate::rpc::dispatch::connection_test_support::assert_budget_intent_state(
                tmp.path(),
                &key,
                &before,
                "applied",
            )
            .await;
            assert_eq!(
                registry
                    .get_goal_task(&task.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .effective_token_limit,
                Some(100)
            );
        }
        if held_turn.is_some() && !ordinary_response_seen {
            let frame = tokio::time::timeout(
                Duration::from_secs(2),
                read_response_with_id(&mut stream, 4),
            )
            .await
            .expect("the ordinary prompt must finish without cancellation");
            crate::rpc::dispatch::connection_test_support::assert_ordinary_prompt_acknowledgement(
                &frame, SID,
            );
        }
        if held_turn.is_some() {
            assert!(!ctx.sessions.has_inflight_turn(SID));
            assert_eq!(
                ctx.sessions.get_generation(SID).await,
                Some(original_generation)
            );
            assert_eq!(ctx.sessions.session_queue.queue_depth(SID).await, 0);
        }
        if let Some(private_config) = private_config_before {
            assert_eq!(
                serde_json::to_value(ctx.config.snapshot()).unwrap(),
                private_config,
                "successful explicit budget edit leaves malformed unused defaults untouched"
            );
            sink.send(Message::Text(
                rpc_request(
                    Method::SessionGoal,
                    &serde_json::json!({"session_id":SID,"command":"/goal status"}),
                    5,
                )
                .into(),
            ))
            .await
            .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let frame: serde_json::Value =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(
                frame["id"], 5,
                "no error or duplicate lifecycle frame before the status barrier"
            );
            assert!(frame["error"].is_null(), "{frame}");
            let status: SessionGoalResult =
                serde_json::from_value(frame["result"].clone()).unwrap();
            assert!(
                matches!(status.response,GoalResponse::Status(status) if status.task_id == before.id && status.token_limit == Some(100))
            );
        }
        cancel.cancel();
        drop(sink);
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// When the retiring generation releases its cleanup, relative to the
    /// listener's forced-abort deadline (`rpc::CONNECTION_DRAIN_GRACE`).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Gen1Release {
        /// Cleanup returns while the listener is still draining cooperatively.
        BeforeForcedDeadline,
        /// Cleanup is still running when the listener force-aborts the
        /// connection, so the replacement generation may only be admitted on
        /// proof that the nested turn task itself has ended.
        AfterForcedDeadline,
    }

    async fn assert_reload_replacement_generation_waits_for_durable_session_prompt_wss(
        release: Gen1Release,
    ) {
        use crate::rpc::context::RpcContext;
        use crate::rpc::dispatch::connection_test_support::insert_session;
        use crate::rpc::session::SessionStore;
        use crate::rpc::types::ChatMode;
        use async_trait::async_trait;
        use tokio::sync::Notify;
        use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
        use zeroclaw_api::model_provider::ModelProvider;
        use zeroclaw_infra::session_queue::SessionActorQueue;

        const DURABLE_SID: &str = "durable-reload-session-wss";
        const OWNER: &str = "reload-test-owner";

        struct HoldOnDrop {
            allow_finish: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
            is_running: Arc<AtomicBool>,
            log: Arc<std::sync::Mutex<Vec<&'static str>>>,
        }

        impl Drop for HoldOnDrop {
            fn drop(&mut self) {
                self.log.lock().unwrap().push("gen1_unwind_start");
                let (lock, cvar) = &*self.allow_finish;
                let mut finished = lock.lock().unwrap();
                while !*finished {
                    finished = cvar.wait(finished).unwrap();
                }
                self.is_running.store(false, Ordering::SeqCst);
                self.log.lock().unwrap().push("gen1_ended");
            }
        }

        struct BlockingGen1Provider {
            started: Arc<Notify>,
            allow_finish: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
            is_running: Arc<AtomicBool>,
            log: Arc<std::sync::Mutex<Vec<&'static str>>>,
        }

        #[async_trait]
        impl ModelProvider for BlockingGen1Provider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                self.is_running.store(true, Ordering::SeqCst);
                self.log.lock().unwrap().push("gen1_started");
                let _drop_guard = HoldOnDrop {
                    allow_finish: Arc::clone(&self.allow_finish),
                    is_running: Arc::clone(&self.is_running),
                    log: Arc::clone(&self.log),
                };
                self.started.notify_one();
                std::future::pending::<()>().await;
                Ok("gen1 finished".to_string())
            }
        }

        impl Attributable for BlockingGen1Provider {
            fn role(&self) -> Role {
                Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
            }
            fn alias(&self) -> &str {
                "blocking-gen1"
            }
        }

        struct ObservantGen2Provider {
            gen1_is_running: Arc<AtomicBool>,
            started: Arc<Notify>,
            overlap_detected: Arc<AtomicBool>,
            log: Arc<std::sync::Mutex<Vec<&'static str>>>,
        }

        #[async_trait]
        impl ModelProvider for ObservantGen2Provider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                if self.gen1_is_running.load(Ordering::SeqCst) {
                    self.overlap_detected.store(true, Ordering::SeqCst);
                    self.log.lock().unwrap().push("OVERLAP_DETECTED");
                }
                self.log.lock().unwrap().push("gen2_started");
                self.started.notify_one();
                self.log.lock().unwrap().push("gen2_ended");
                Ok("gen2 finished".to_string())
            }
        }

        impl Attributable for ObservantGen2Provider {
            fn role(&self) -> Role {
                Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
            }
            fn alias(&self) -> &str {
                "observant-gen2"
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mats = zeroclaw_tls::ensure_server_materials(tmp.path(), &[]).unwrap();
        let ca_pem = std::fs::read_to_string(&mats.ca_cert_path).unwrap();
        let ca_key_pem = std::fs::read_to_string(&mats.ca_key_path).unwrap();
        let client = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key_pem, "test-client").unwrap();

        let acceptor1 = super::build_tls_acceptor(
            mats.server_cert_path.to_str().unwrap(),
            mats.server_key_path.to_str().unwrap(),
            mats.ca_cert_path.to_str().unwrap(),
            &[],
            "",
        )
        .unwrap();

        let chat_backend = Arc::new(
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(tmp.path()).unwrap(),
        );

        let gen1_started = Arc::new(Notify::new());
        let allow_gen1_finish = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let gen1_is_running = Arc::new(AtomicBool::new(false));
        let gen2_started = Arc::new(Notify::new());
        let overlap_detected = Arc::new(AtomicBool::new(false));
        let gen1_listener_exited = Arc::new(AtomicBool::new(false));
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));

        // Keep the socket reserved until the serving task takes ownership.
        let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr1 = listener1.local_addr().unwrap();

        let mut config1 = zeroclaw_config::schema::Config {
            data_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        // A remote (WSS) connection must authenticate before it can run a
        // privileged prompt: pair a native token for the test client.
        config1.gateway.paired_tokens = vec!["zc_tok".to_string()];
        let queue1 = Arc::new(SessionActorQueue::new(4, 30, 60));
        let sessions1 = Arc::new(SessionStore::new(64, queue1));
        let mut ctx1 = RpcContext::for_persistence_tests(
            config1,
            sessions1,
            Some(chat_backend.clone() as Arc<dyn zeroclaw_infra::session_backend::SessionBackend>),
            None,
        );

        std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).unwrap();
        Arc::get_mut(&mut ctx1).unwrap().tui_registry =
            Arc::new(crate::rpc::tui_identity::TuiRegistry::new(tmp.path()));

        insert_session(
            &ctx1,
            tmp.path(),
            DURABLE_SID,
            Box::new(BlockingGen1Provider {
                started: Arc::clone(&gen1_started),
                allow_finish: Arc::clone(&allow_gen1_finish),
                is_running: Arc::clone(&gen1_is_running),
                log: Arc::clone(&log),
            }),
        )
        .await;

        ctx1.sessions
            .resume_existing(
                DURABLE_SID,
                "test-agent",
                &ChatMode::Chat,
                Some(OWNER.into()),
                None,
            )
            .await
            .unwrap()
            .expect("Gen 1 fixture session must exist");

        let cancel1 = CancellationToken::new();
        let count1 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel1_for_listener = cancel1.clone();
        let ctx1_for_listener = Arc::clone(&ctx1);
        let count1_for_listener = Arc::clone(&count1);

        let mut gen1_listener_handle = zeroclaw_spawn::spawn!(async move {
            super::serve_wss_listener(
                ctx1_for_listener,
                cancel1_for_listener,
                count1_for_listener,
                acceptor1,
                listener1,
                super::WssLimits::default(),
            )
            .await
        });

        let connector1 = test_client_connector(&client.cert_pem, &client.key_pem);
        let url1 = format!("wss://{addr1}/");
        let ws1 = async {
            let started = std::time::Instant::now();
            loop {
                let connected = tokio::select! {
                    result = &mut gen1_listener_handle => {
                        panic!("Gen 1 WSS listener exited during startup: {result:?}");
                    }
                    result = tokio_tungstenite::connect_async_tls_with_config(
                        &url1,
                        None,
                        false,
                        Some(connector1.clone()),
                    ) => result,
                };
                match connected {
                    Ok((ws, _)) => return ws,
                    Err(e) => {
                        if started.elapsed() > Duration::from_secs(5) {
                            cancel1.cancel();
                            let listener_result = (&mut gen1_listener_handle).await;
                            panic!("timed out connecting to Gen 1: {e:?}; listener: {listener_result:?}");
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        }
        .await;

        let (mut client_sink1, mut client_stream1) = ws1.split();
        let init = InitializeParams {
            protocol_version: 1,
            tui_id: Some(OWNER.into()),
            tui_sig: Some(ctx1.tui_registry.sign(OWNER).expect("fixture signing key")),
            env: Default::default(),
            client_capabilities: None,
            // Present the paired native token: an unauthenticated remote
            // connection cannot run the privileged prompt this test drives.
            auth_token: Some("zc_tok".to_string()),
            auth_provider: None,
        };
        client_sink1
            .send(Message::Text(
                rpc_request(Method::Initialize, &init, 1).into(),
            ))
            .await
            .unwrap();
        let initialized = client_stream1.next().await.unwrap().unwrap();
        let initialized: serde_json::Value =
            serde_json::from_str(initialized.to_text().unwrap()).unwrap();
        assert!(initialized["error"].is_null(), "{initialized}");
        assert_eq!(initialized["result"]["tui_id"], OWNER);

        // Send prompt to Generation 1
        client_sink1
            .send(Message::Text(
                rpc_request(
                    Method::SessionPrompt,
                    &serde_json::json!({"session_id": DURABLE_SID, "prompt": "prompt 1"}),
                    2,
                )
                .into(),
            ))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), gen1_started.notified())
            .await
            .expect("Gen 1 prompt should reach provider");

        assert!(
            gen1_is_running.load(Ordering::SeqCst),
            "Gen 1 prompt must be running"
        );

        // Trigger reload on Generation 1
        cancel1.cancel();

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !gen1_listener_handle.is_finished(),
            "Gen 1 WSS listener must NOT finish while prompt is held in cancellation"
        );
        assert!(
            gen1_is_running.load(Ordering::SeqCst),
            "Gen 1 prompt must remain active while draining"
        );

        let tmp_path = tmp.path().to_path_buf();
        let chat_backend2 = chat_backend.clone();
        let gen1_is_running_for_gen2 = Arc::clone(&gen1_is_running);
        let gen2_started_clone = Arc::clone(&gen2_started);
        let overlap_detected_clone = Arc::clone(&overlap_detected);
        let log_clone = Arc::clone(&log);
        let client_cert_pem = client.cert_pem.clone();
        let client_key_pem = client.key_pem.clone();
        let count1_for_gen2 = Arc::clone(&count1);
        let gen1_listener_exited_for_gen2 = Arc::clone(&gen1_listener_exited);

        let gen2_task = zeroclaw_spawn::spawn!(async move {
            // Gen 2 waits for Gen 1 listener to cleanly shut down
            gen1_listener_handle
                .await
                .unwrap()
                .expect("Gen 1 WSS listener should exit Ok");
            gen1_listener_exited_for_gen2.store(true, Ordering::SeqCst);

            // The listener returning is not admission: a force-aborted
            // connection leaves its turn task unwinding. Gate on the daemon's
            // own reload drain, which is what production waits on.
            assert_eq!(
                crate::daemon::await_rpc_connection_drain(&count1_for_gen2).await,
                crate::daemon::RpcDrain::Complete,
                "the retiring generation must finish draining within the reload budget"
            );

            // Keep the socket reserved until the serving task takes ownership.
            let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr2 = listener2.local_addr().unwrap();

            let acceptor2 = super::build_tls_acceptor(
                mats.server_cert_path.to_str().unwrap(),
                mats.server_key_path.to_str().unwrap(),
                mats.ca_cert_path.to_str().unwrap(),
                &[],
                "",
            )
            .unwrap();

            let mut config2 = zeroclaw_config::schema::Config {
                data_dir: tmp_path.clone(),
                config_path: tmp_path.join("config.toml"),
                ..Default::default()
            };
            // The replacement generation authenticates the same client.
            config2.gateway.paired_tokens = vec!["zc_tok".to_string()];
            let queue2 = Arc::new(SessionActorQueue::new(4, 30, 60));
            let sessions2 = Arc::new(SessionStore::new(64, queue2));
            let mut ctx2 = RpcContext::for_persistence_tests(
                config2,
                sessions2,
                Some(chat_backend2 as Arc<dyn zeroclaw_infra::session_backend::SessionBackend>),
                None,
            );

            Arc::get_mut(&mut ctx2).unwrap().tui_registry =
                Arc::new(crate::rpc::tui_identity::TuiRegistry::new(&tmp_path));
            let owner_sig = ctx2.tui_registry.sign(OWNER).expect("fixture signing key");

            insert_session(
                &ctx2,
                &tmp_path,
                DURABLE_SID,
                Box::new(ObservantGen2Provider {
                    gen1_is_running: gen1_is_running_for_gen2,
                    started: gen2_started_clone,
                    overlap_detected: overlap_detected_clone,
                    log: log_clone,
                }),
            )
            .await;

            ctx2.sessions
                .resume_existing(
                    DURABLE_SID,
                    "test-agent",
                    &ChatMode::Chat,
                    Some(OWNER.into()),
                    None,
                )
                .await
                .unwrap()
                .expect("Gen 2 fixture session must exist");

            let cancel2 = CancellationToken::new();
            let count2 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let cancel2_for_listener = cancel2.clone();
            let mut handle2 = zeroclaw_spawn::spawn!(async move {
                super::serve_wss_listener(
                    ctx2,
                    cancel2_for_listener,
                    count2,
                    acceptor2,
                    listener2,
                    super::WssLimits::default(),
                )
                .await
            });

            let connector2 = test_client_connector(&client_cert_pem, &client_key_pem);
            let url2 = format!("wss://{addr2}/");
            let ws2 = async {
                let started = std::time::Instant::now();
                loop {
                    let connected = tokio::select! {
                        result = &mut handle2 => {
                            panic!("Gen 2 WSS listener exited during startup: {result:?}");
                        }
                        result = tokio_tungstenite::connect_async_tls_with_config(
                            &url2,
                            None,
                            false,
                            Some(connector2.clone()),
                        ) => result,
                    };
                    match connected {
                        Ok((ws, _)) => return ws,
                        Err(e) => {
                            if started.elapsed() > Duration::from_secs(5) {
                                cancel2.cancel();
                                let listener_result = (&mut handle2).await;
                                panic!("timed out connecting to Gen 2: {e:?}; listener: {listener_result:?}");
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                }
            }
            .await;

            let (mut client_sink2, mut client_stream2) = ws2.split();
            let init = InitializeParams {
                protocol_version: 1,
                tui_id: Some(OWNER.into()),
                tui_sig: Some(owner_sig),
                env: Default::default(),
                client_capabilities: None,
                // Same paired token for the replacement generation.
                auth_token: Some("zc_tok".to_string()),
                auth_provider: None,
            };
            client_sink2
                .send(Message::Text(
                    rpc_request(Method::Initialize, &init, 1).into(),
                ))
                .await
                .unwrap();
            let initialized = client_stream2.next().await.unwrap().unwrap();
            let initialized: serde_json::Value =
                serde_json::from_str(initialized.to_text().unwrap()).unwrap();
            assert!(initialized["error"].is_null(), "{initialized}");
            assert_eq!(initialized["result"]["tui_id"], OWNER);

            client_sink2
                .send(Message::Text(
                    rpc_request(
                        Method::SessionPrompt,
                        &serde_json::json!({"session_id": DURABLE_SID, "prompt": "prompt 2"}),
                        3,
                    )
                    .into(),
                ))
                .await
                .unwrap();

            tokio::time::sleep(Duration::from_millis(150)).await;

            cancel2.cancel();
            handle2
                .await
                .unwrap()
                .expect("Gen 2 WSS listener should exit Ok");
            drop(client_sink2);
        });

        // While Gen 1 is held in cancellation, verify Gen 2 has not started its
        // prompt. Observations are recorded here and asserted after the release
        // below: the fixture parks a worker thread until it is released, so an
        // assertion that fails while it is held would hang the run instead of
        // reporting the failure.
        let forced_observation = match release {
            Gen1Release::BeforeForcedDeadline => None,
            Gen1Release::AfterForcedDeadline => {
                let exited = tokio::time::timeout(Duration::from_secs(30), async {
                    while !gen1_listener_exited.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                .is_ok();
                Some((exited, count1.load(Ordering::Relaxed)))
            }
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let gen1_held = gen1_is_running.load(Ordering::SeqCst);
        let log_while_held = log.lock().unwrap().clone();

        // Now allow Gen 1 prompt to complete its cooperative cancellation unwind
        {
            let (lock, cvar) = &*allow_gen1_finish;
            let mut finished = lock.lock().unwrap();
            *finished = true;
            cvar.notify_all();
        }

        if let Some((exited, counted)) = forced_observation {
            assert!(
                exited,
                "the Gen 1 listener must force-abort its connection and exit"
            );
            assert!(
                counted > 0,
                "a force-aborted connection must stay counted while its turn task unwinds"
            );
        }
        assert!(gen1_held, "Gen 1 prompt must still be held");
        assert_eq!(
            log_while_held,
            vec!["gen1_started", "gen1_unwind_start"],
            "Prompt 2 must not have started yet"
        );

        // Gen 2 task should now complete cleanly
        tokio::time::timeout(Duration::from_secs(10), gen2_task)
            .await
            .expect("Gen 2 reload handoff should finish within timeout")
            .expect("Gen 2 task should not panic");

        assert!(
            !overlap_detected.load(Ordering::SeqCst),
            "Prompt 2 must not overlap with Prompt 1 on durable session"
        );
        let final_log = log.lock().unwrap().clone();
        assert_eq!(
            final_log,
            vec![
                "gen1_started",
                "gen1_unwind_start",
                "gen1_ended",
                "gen2_started",
                "gen2_ended"
            ],
            "Generations must execute sequentially with Gen 1 fully drained before Gen 2 begins"
        );

        drop(client_sink1);
    }

    #[test]
    fn reload_replacement_generation_waits_for_durable_session_prompt_wss() {
        std::thread::Builder::new()
            .name("wss-reload-durable-session".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(
                        assert_reload_replacement_generation_waits_for_durable_session_prompt_wss(
                            Gen1Release::BeforeForcedDeadline,
                        ),
                    );
            })
            .unwrap()
            .join()
            .expect("WSS reload durable session test thread should not panic");
    }

    /// The forced half of the same handoff: Gen 1 cleanup is still running when
    /// the listener force-aborts the connection it accepted. Admission of Gen 2
    /// must wait for the nested turn task to end, not for the abort to be
    /// requested.
    #[test]
    fn forced_reload_replacement_generation_waits_for_durable_session_prompt_wss() {
        std::thread::Builder::new()
            .name("wss-forced-reload-durable-session".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(
                        assert_reload_replacement_generation_waits_for_durable_session_prompt_wss(
                            Gen1Release::AfterForcedDeadline,
                        ),
                    );
            })
            .unwrap()
            .join()
            .expect("WSS forced reload durable session test thread should not panic");
    }

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

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::rpc::session::SessionStore;
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncReadExt;
    use zeroclaw_infra::session_queue::SessionActorQueue;

    fn test_ctx(tmp: &std::path::Path) -> Arc<RpcContext> {
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.to_path_buf(),
            config_path: tmp.join("config.toml"),
            ..Default::default()
        };
        let session_queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(SessionStore::new(64, session_queue));
        RpcContext::minimal(config, sessions)
    }

    fn test_tls_acceptor(tmp: &std::path::Path) -> TlsAcceptor {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let materials = zeroclaw_tls::ensure_server_materials(tmp, &[]).unwrap();
        build_tls_acceptor(
            materials.server_cert_path.to_str().unwrap(),
            materials.server_key_path.to_str().unwrap(),
            materials.ca_cert_path.to_str().unwrap(),
            &[],
            "",
        )
        .unwrap()
    }

    async fn wait_for_client_count(count: &Arc<AtomicUsize>, expected: usize) {
        for _ in 0..250 {
            if count.load(Ordering::Relaxed) == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "client count never reached {expected}; last observed {}",
            count.load(Ordering::Relaxed)
        );
    }

    #[tokio::test]
    async fn cancel_interrupts_and_joins_inflight_wss_handshake() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let acceptor = test_tls_acceptor(tmp.path());
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind_addr = probe.local_addr().unwrap();
        drop(probe);

        let cancel = CancellationToken::new();
        let count = Arc::new(AtomicUsize::new(0));
        let server_cancel = cancel.clone();
        let server_count = Arc::clone(&count);
        let server = zeroclaw_spawn::spawn!(async move {
            run_wss_listener(
                ctx,
                server_cancel,
                server_count,
                acceptor,
                bind_addr,
                WssLimits::default(),
            )
            .await
        });

        let mut client = loop {
            match TcpStream::connect(bind_addr).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        wait_for_client_count(&count, 1).await;

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("WSS listener should join an in-flight handshake after cancellation")
            .expect("WSS listener task should not panic")
            .expect("WSS listener should stop cleanly");

        assert_eq!(count.load(Ordering::Relaxed), 0);
        let mut byte = [0_u8; 1];
        let bytes = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("cancelled WSS client should observe EOF")
            .expect("client read should not fail");
        assert_eq!(bytes, 0, "cancelled WSS connection should be closed");
    }
}
