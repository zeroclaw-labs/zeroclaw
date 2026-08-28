//! The reconnect/resumption session driver and the crate's public API.
//!
//! [`Session`] owns exactly one thing: the *connection lifecycle*. It connects,
//! sends `setup`, drives the transport, parses server frames into [`Event`]s,
//! remembers the session-resumption handle, and — on a resumable close —
//! reconnects with backoff and transparently resumes.
//!
//! The lifecycle runs in an **internal `tokio::spawn`ed task** that owns the
//! transport and the reconnect/backoff loop. The public API is therefore
//! *cancel-safe*: [`Session::recv_event`] is a plain `mpsc` receive (the
//! reconnect loop lives in the task, never in the caller's `select!`, so a
//! caller that races `recv_event` against other futures can never cancel a
//! reconnect mid-flight), and the `send_*` methods enqueue a command to the task
//! over a channel (they take `&self`). This is the fix for the reconnect
//! *starvation* that a caller-driven `next_event` racing a ~20 ms audio arm
//! would cause: every audio frame used to cancel the backoff sleep so the
//! reconnect never completed. Now the task drains (and discards) audio commands
//! while it is reconnecting, and the backoff runs to completion regardless of
//! caller traffic.
//!
//! It deliberately does **not** own call semantics: the greeting timer, energy
//! VAD, audio-forwarding decisions, RESUME_CUE/GREET_CUE wording, the uplink
//! drain, or `end_call` handling. Those live in the caller (kutsu), built on
//! top of this API — the caller reacts to `SessionOpened { is_reconnect, resumed }`
//! and decides what to send via [`Session::send_audio`] /
//! [`Session::send_client_text`] / [`Session::send_tool_response`].
//!
//! The reconnect/backoff policy is ported from kutsu's original `gemini_live`:
//! backoff 0.3s → ×2 → max 5s; the stored resumption handle is dropped as stale
//! after [`HANDLE_DROP_AFTER`] consecutive failed connects; and the *initial*
//! connect applies the same bounded backoff+retry so a transient blip during the
//! SIP ring never kills the call.

use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::transport::{ProxyConfig, Transport, TransportError, WsTransport};
use crate::types::{AffectLabel, CloseReason, Model, Role, ServerEvent, SetupConfig};
use crate::wire;

/// Drop the stored resumption handle after this many consecutive failed
/// connects (kutsu's `ReconnectState::new(4)`).
const HANDLE_DROP_AFTER: u32 = 4;

/// Bounded capacity of the outbound command channel (audio/client-text/tool
/// responses). Mirrors the old uplink channel depth.
const CMD_CAPACITY: usize = 64;

/// Bounded capacity of the inbound event channel.
const EVENT_CAPACITY: usize = 256;

/// A produced-on-demand transport factory: each call establishes a *fresh*
/// connection (a new [`WsTransport`] in production; a scripted double in
/// tests). Used by the reconnect loop; the initial connection is established
/// eagerly by [`Session::connect`].
pub type Reconnector<T> =
    Box<dyn FnMut() -> Pin<Box<dyn Future<Output = Result<T, SessionError>> + Send>> + Send>;

/// Everything the crate needs to open (and reopen) one Gemini Live session.
/// `setup` is assembled by the caller (kutsu builds the prompt/scenario); the
/// crate only serializes and sends it.
pub struct ClientConfig {
    pub model: Model,
    pub api_key: String,
    pub proxy: Option<ProxyConfig>,
    /// The caller-assembled setup. Its `resume_handle` seeds the first connect;
    /// thereafter the crate replaces it with the latest server-issued handle.
    pub setup: SetupConfig,
    /// Give up (terminal [`Event::SessionClosed`], or an `Err` from
    /// [`Session::connect`] on the *initial* connect) after this many
    /// consecutive *non-progressing* attempts (a failed connect, or a connect
    /// that closes before `setupComplete`). `None` (the default and the match
    /// for kutsu) is unbounded: the crate reconnects forever, bounded only by
    /// the caller's own call-level hangup/time-cap. This is safe from
    /// busy-looping because the backoff caps at 5s.
    pub max_reconnect_attempts: Option<u32>,
}

/// One event on the unified session stream. `SessionOpened`/`SessionClosed`
/// bracket the lifecycle; the rest mirror parsed server activity. The
/// resumption handle is intentionally absent — it is managed internally and
/// never surfaced.
#[derive(Clone, Debug)]
pub enum Event {
    /// A connection is open and `setup` has been sent. `is_reconnect` is
    /// `false` for the very first open, `true` for every transparent resume —
    /// the caller's cue to drain stale uplink / send a resume cue. `resumed`
    /// says whether that reopen re-sent `setup` carrying a stored resumption
    /// handle: `true` means context was preserved server-side (the model can
    /// restore it itself), `false` means the reopen was fresh and context was
    /// lost. The first open is always `is_reconnect: false, resumed: false`.
    SessionOpened {
        is_reconnect: bool,
        resumed: bool,
    },
    /// Terminal: reconnection was exhausted or the close is unrecoverable. The
    /// stream ends after this (`recv_event` returns `None` thereafter).
    SessionClosed {
        reason: CloseReason,
    },
    /// 24 kHz PCM16 output audio.
    OutputAudio(Vec<i16>),
    Transcript {
        role: Role,
        text: String,
        final_: bool,
    },
    Affect {
        role: Role,
        label: AffectLabel,
    },
    Interrupted,
    TurnComplete,
    /// A tool/function call. The crate does not special-case `end_call`; the
    /// caller decides its semantics and acks via [`Session::send_tool_response`].
    ToolCall {
        name: String,
        id: String,
        args: serde_json::Value,
    },
}

/// An error from establishing or driving a [`Session`].
#[derive(Debug)]
pub enum SessionError {
    Transport(TransportError),
    /// The session's driver task has terminated (a terminal close, or the
    /// session was dropped); a `send_*` after that point returns this.
    Closed,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Transport(e) => write!(f, "transport: {e}"),
            SessionError::Closed => write!(f, "session closed"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<TransportError> for SessionError {
    fn from(e: TransportError) -> Self {
        SessionError::Transport(e)
    }
}

/// How the just-ended attempt terminated — drives the reconnect bookkeeping.
/// `Resumable` (goAway / send / protocol error) is always a failure;
/// `RemoteClose` (a clean WS close or a dropped stream) is a success only if the
/// attempt made real progress.
#[derive(Clone, Copy)]
enum EndKind {
    Resumable,
    RemoteClose,
}

/// An outbound request from the caller to the driver task. The task performs the
/// actual transport write (byte-identical to the old inline builders).
enum Command {
    Audio(Vec<i16>),
    ClientText(String),
    ToolResponse(String),
}

/// The public session handle: a cancel-safe event stream + an enqueue-only
/// command channel to the internal driver task. Dropping it closes both
/// channels; the task then drains any already-queued commands (e.g. a final
/// `end_call` ack) and exits on its own, dropping the transport (which closes
/// the WS). It is intentionally NOT aborted on drop, so that last enqueued ack
/// is still delivered before teardown.
pub struct Session {
    cmd_tx: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
}

impl Session {
    /// Connect (first open) with bounded backoff+retry, then spawn the driver
    /// task. The first [`Session::recv_event`] yields
    /// `SessionOpened { is_reconnect: false, resumed: false }`.
    ///
    /// The *initial* connect is retried with the same backoff as reconnects, so
    /// a transient network blip (e.g. during the SIP ring) does not kill the
    /// call. It only returns `Err` when `max_reconnect_attempts` is `Some(n)`
    /// and that budget is exhausted; with `None` it retries until the socket
    /// opens (the caller's own task/time-cap bounds it).
    pub async fn connect(cfg: ClientConfig) -> Result<Self, SessionError> {
        let model = cfg.model;
        // The reconnect factory re-establishes a fresh WS each time, cloning the
        // auth/proxy config (the endpoint + key are fixed for the session).
        let reconnect: Reconnector<WsTransport> = {
            let api_key = cfg.api_key.clone();
            let proxy = cfg.proxy.clone();
            Box::new(move || {
                let api_key = api_key.clone();
                let proxy = proxy.clone();
                Box::pin(async move {
                    WsTransport::connect(model, &api_key, proxy.as_ref())
                        .await
                        .map_err(SessionError::from)
                })
            })
        };

        let mut backoff = Backoff::new(300, 5000);
        let mut rstate = ReconnectState::new(HANDLE_DROP_AFTER);
        let mut handle = cfg.setup.resume_handle.clone();

        // Initial connect with the same bounded backoff+retry as reconnects.
        let transport = loop {
            match WsTransport::connect(model, &cfg.api_key, cfg.proxy.as_ref()).await {
                Ok(t) => break t,
                Err(e) => {
                    if note_failure(&mut rstate, &cfg, &mut handle) {
                        return Err(SessionError::from(e));
                    }
                    tokio::time::sleep(backoff.next_delay()).await;
                }
            }
        };

        Ok(Self::spawn(
            cfg, transport, reconnect, handle, backoff, rstate,
        ))
    }

    /// Construct a session over an already-established transport plus a
    /// reconnect factory. Public but hidden: production goes through
    /// [`Session::connect`]; this exists so tests (and kutsu's tests) can
    /// inject a scripted transport.
    #[doc(hidden)]
    pub async fn connect_with_transport<T: Transport + 'static>(
        cfg: ClientConfig,
        transport: T,
        reconnect: Reconnector<T>,
    ) -> Result<Self, SessionError> {
        let handle = cfg.setup.resume_handle.clone();
        let backoff = Backoff::new(300, 5000);
        let rstate = ReconnectState::new(HANDLE_DROP_AFTER);
        Ok(Self::spawn(
            cfg, transport, reconnect, handle, backoff, rstate,
        ))
    }

    /// Wire up the channels and spawn the driver task.
    fn spawn<T: Transport + 'static>(
        cfg: ClientConfig,
        transport: T,
        reconnect: Reconnector<T>,
        handle: Option<String>,
        backoff: Backoff,
        rstate: ReconnectState,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(CMD_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel::<Event>(EVENT_CAPACITY);
        // Detached: the task self-terminates when the caller drops the Session
        // (its `cmd_rx`/`events` close), after draining queued commands.
        tokio::spawn(driver_task(
            transport, cfg, reconnect, handle, backoff, rstate, cmd_rx, event_tx,
        ));
        Session {
            cmd_tx,
            events: event_rx,
        }
    }

    /// The unified event stream across reconnects. Cancel-safe: it is a plain
    /// `mpsc` receive, so racing it in a `select!` never disturbs the reconnect
    /// loop (which runs in the driver task). Returns `None` once the task ends
    /// (after a terminal [`Event::SessionClosed`], or once the session is
    /// dropped).
    pub async fn recv_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// Send one uplink audio frame (PCM16 @ 16 kHz). Enqueues to the driver task
    /// (`&self`); the task writes the byte-identical `realtime_input` frame. If
    /// the task is reconnecting the frame is dropped there (never blocks the
    /// caller). Returns `Err(SessionError::Closed)` once the task has ended.
    pub async fn send_audio(&self, pcm16_16k: &[i16]) -> Result<(), SessionError> {
        self.cmd_tx
            .send(Command::Audio(pcm16_16k.to_vec()))
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// Send a client text turn (kutsu uses this for GREET_CUE / RESUME_CUE).
    pub async fn send_client_text(&self, text: &str) -> Result<(), SessionError> {
        self.cmd_tx
            .send(Command::ClientText(text.to_string()))
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// Acknowledge a tool call by id (kutsu owns the tool's semantics).
    pub async fn send_tool_response(&self, call_id: &str) -> Result<(), SessionError> {
        self.cmd_tx
            .send(Command::ToolResponse(call_id.to_string()))
            .await
            .map_err(|_| SessionError::Closed)
    }
}

/// The reconnect/resumption driver, run inside a spawned task. Owns the
/// transport and the whole lifecycle; communicates with the caller only over
/// `cmd_rx` (in) and `events` (out).
#[allow(clippy::too_many_arguments)] // internal driver: params are the lifecycle wiring
async fn driver_task<T: Transport>(
    mut transport: T,
    cfg: ClientConfig,
    mut reconnect: Reconnector<T>,
    mut handle: Option<String>,
    mut backoff: Backoff,
    mut rstate: ReconnectState,
    mut cmd_rx: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let mut progressed = false;

    // First open (is_reconnect: false, resumed: false).
    if send_setup(&mut transport, &cfg, &handle).await.is_err() {
        match reconnect_loop(
            &mut reconnect,
            &mut backoff,
            &mut rstate,
            &cfg,
            &mut handle,
            false,
            EndKind::Resumable,
            &mut cmd_rx,
            &events,
        )
        .await
        {
            Ok(t) => {
                transport = t;
                progressed = false;
            }
            Err(()) => {
                let _ = events
                    .send(Event::SessionClosed {
                        reason: synth_reason("setup failed"),
                    })
                    .await;
                return;
            }
        }
    } else if events
        .send(Event::SessionOpened {
            is_reconnect: false,
            resumed: false,
        })
        .await
        .is_err()
    {
        return; // caller gone
    }

    'outer: loop {
        // --- LIVE phase: pump commands out, server frames in. ---
        let (end, reason) = loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(c) => {
                        if write_cmd(&mut transport, c).await.is_err() {
                            break (EndKind::Resumable, synth_reason("send error"));
                        }
                    }
                    None => return, // all command senders dropped -> caller gone
                },
                msg = transport.recv() => match msg {
                    Some(Ok(bytes)) => {
                        // DIAG (gated on KUTSU_GEMINI_DIAG=1): compact summary of
                        // every inbound server message, to see what Gemini does
                        // during the silence windows where it stops replying.
                        if std::env::var("KUTSU_GEMINI_DIAG").as_deref() == Ok("1") {
                            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                                let keys: Vec<String> = v.as_object()
                                    .map(|o| o.keys().cloned().collect())
                                    .unwrap_or_default();
                                let sc = &v["serverContent"];
                                tracing::info!(
                                    target: "gemini_diag",
                                    ?keys,
                                    model_turn = !sc["modelTurn"].is_null(),
                                    turn_complete = !sc["turnComplete"].is_null(),
                                    generation_complete = !sc["generationComplete"].is_null(),
                                    interrupted = !sc["interrupted"].is_null(),
                                    input_tx = !sc["inputTranscription"].is_null(),
                                    output_tx = !sc["outputTranscription"].is_null(),
                                    "server msg"
                                );
                            }
                        }
                        // Malformed frames are skipped (never fatal).
                        if let Ok(evs) = wire::parse_server_message(&bytes) {
                            let mut goaway = false;
                            for se in evs {
                                match absorb(se, &mut handle, &mut progressed) {
                                    Absorbed::Emit(ev) => {
                                        if events.send(ev).await.is_err() {
                                            return; // caller gone
                                        }
                                    }
                                    Absorbed::GoAway => goaway = true,
                                    Absorbed::Ignore => {}
                                }
                            }
                            if goaway {
                                break (EndKind::Resumable, synth_reason("goAway"));
                            }
                        }
                    }
                    Some(Err(TransportError::Closed(r))) => break (EndKind::RemoteClose, r),
                    Some(Err(_)) => break (EndKind::Resumable, synth_reason("transport error")),
                    None => break (EndKind::RemoteClose, synth_reason("stream ended")),
                },
            }
        };

        // A no-progress close whose code says the request itself is invalid
        // (not a transient/server condition) can never succeed on retry: the
        // reconnect would re-send the identical, still-invalid setup. Terminate
        // instead of storming it forever. This is the WS-1007 class (a malformed
        // setup schema). Only fatal before any progress — a permanent-coded drop
        // on a session that already reached setupComplete stays retryable.
        if !progressed && is_permanent_close(reason.code) {
            let _ = events.send(Event::SessionClosed { reason }).await;
            return;
        }

        // --- RECONNECT phase (transparent). ---
        match reconnect_loop(
            &mut reconnect,
            &mut backoff,
            &mut rstate,
            &cfg,
            &mut handle,
            progressed,
            end,
            &mut cmd_rx,
            &events,
        )
        .await
        {
            Ok(t) => {
                transport = t;
                progressed = false;
                continue 'outer;
            }
            Err(()) => {
                let _ = events.send(Event::SessionClosed { reason }).await;
                return;
            }
        }
    }
}

/// What one parsed server event maps to.
enum Absorbed {
    Emit(Event),
    GoAway,
    Ignore,
}

/// Map one parsed server event onto internal state or a surfaced [`Event`].
/// The resumption handle is stored (never surfaced); `setupComplete` is internal
/// progress; `goAway` schedules a transparent reconnect.
fn absorb(se: ServerEvent, handle: &mut Option<String>, progressed: &mut bool) -> Absorbed {
    match se {
        ServerEvent::SetupComplete => {
            *progressed = true;
            Absorbed::Ignore
        }
        ServerEvent::OutputAudio(pcm) => Absorbed::Emit(Event::OutputAudio(pcm)),
        ServerEvent::Transcript { role, text, final_ } => {
            Absorbed::Emit(Event::Transcript { role, text, final_ })
        }
        ServerEvent::Affect { role, label } => Absorbed::Emit(Event::Affect { role, label }),
        ServerEvent::Interrupted => Absorbed::Emit(Event::Interrupted),
        ServerEvent::TurnComplete => Absorbed::Emit(Event::TurnComplete),
        ServerEvent::ToolCall { name, id, args } => {
            Absorbed::Emit(Event::ToolCall { name, id, args })
        }
        ServerEvent::ResumptionHandle(h) => {
            *handle = Some(h);
            *progressed = true;
            Absorbed::Ignore
        }
        ServerEvent::GoAway => Absorbed::GoAway,
    }
}

/// Send `setup` (carrying the latest stored handle) on `transport`.
async fn send_setup<T: Transport>(
    transport: &mut T,
    cfg: &ClientConfig,
    handle: &Option<String>,
) -> Result<(), TransportError> {
    let mut setup = cfg.setup.clone();
    setup.resume_handle = handle.clone();
    transport
        .send_text(wire::build_setup(&setup).to_string())
        .await
}

/// Perform the byte-identical transport write for one outbound command.
async fn write_cmd<T: Transport>(transport: &mut T, cmd: Command) -> Result<(), TransportError> {
    match cmd {
        Command::Audio(pcm) => {
            // DIAG (KUTSU_GEMINI_DIAG=1): a per-frame WS write should be near-
            // instant (~50 frames/s). If it takes long the socket is
            // backpressuring — uplink audio isn't leaving fast enough, pointing
            // at a throttled/DPI'd network path rather than Gemini's own VAD.
            let msg = wire::build_realtime_input(&pcm);
            let t0 = std::time::Instant::now();
            let r = transport.send_text(msg).await;
            let ms = t0.elapsed().as_millis();
            if ms > 40 && std::env::var("KUTSU_GEMINI_DIAG").as_deref() == Ok("1") {
                tracing::warn!(target: "gemini_diag", send_ms = ms as u64, "uplink WS send backpressure (network/DPI?)");
            }
            r
        }
        Command::ClientText(t) => transport.send_text(wire::build_client_content(&t)).await,
        Command::ToolResponse(id) => transport.send_text(wire::build_tool_response(&id)).await,
    }
}

/// Record one non-progressing reconnect attempt: advance the consecutive-failure
/// count, drop the stale handle at the threshold, and report whether the
/// configured attempt budget is now exhausted (→ terminal).
fn note_failure(
    rstate: &mut ReconnectState,
    cfg: &ClientConfig,
    handle: &mut Option<String>,
) -> bool {
    if rstate.on_failure() {
        *handle = None; // stale handle after N consecutive failures
    }
    matches!(cfg.max_reconnect_attempts, Some(max) if rstate.fails() >= max)
}

/// Reconnect for a just-ended attempt: apply the bookkeeping, then back off and
/// (re)connect — retrying with escalating backoff and stale-handle drop — until
/// a fresh transport is open + `setup` re-sent (emitting
/// `SessionOpened { is_reconnect: true, resumed }`), or the attempt budget is
/// exhausted (terminal → `Err`).
///
/// Crucially, throughout the backoff it **drains and discards** inbound commands
/// (`drain_backoff`): audio queued during the outage is stale, and draining it
/// keeps the caller's bounded command channel from blocking — this is what
/// prevents caller audio traffic from starving the reconnect. The backoff sleep
/// itself runs to completion regardless of how many commands arrive.
#[allow(clippy::too_many_arguments)] // internal driver: params are the lifecycle wiring
async fn reconnect_loop<T: Transport>(
    reconnect: &mut Reconnector<T>,
    backoff: &mut Backoff,
    rstate: &mut ReconnectState,
    cfg: &ClientConfig,
    handle: &mut Option<String>,
    progressed: bool,
    end: EndKind,
    cmd_rx: &mut mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> Result<T, ()> {
    // Bookkeeping for the attempt that just ended: Resumable is always a
    // failure; RemoteClose is success iff progress was made.
    let success = matches!(end, EndKind::RemoteClose) && progressed;
    if success {
        rstate.on_success();
        backoff.reset();
    } else if note_failure(rstate, cfg, handle) {
        return Err(());
    }

    loop {
        // Back off before (re)connecting, draining stale commands meanwhile.
        if let ControlFlow::Break(()) = drain_backoff(cmd_rx, backoff.next_delay()).await {
            return Err(()); // caller gone
        }

        if let Ok(mut t) = reconnect().await {
            if send_setup(&mut t, cfg, handle).await.is_ok() {
                // A reopen "resumed" iff it re-sent setup carrying a stored handle.
                let resumed = handle.is_some();
                if events
                    .send(Event::SessionOpened {
                        is_reconnect: true,
                        resumed,
                    })
                    .await
                    .is_ok()
                {
                    return Ok(t);
                }
                return Err(()); // caller gone
            }
            // setup send failed on the fresh transport: treat as a failed attempt.
        }
        if note_failure(rstate, cfg, handle) {
            return Err(());
        }
    }
}

/// Sleep `delay` while draining (discarding) inbound commands, so the caller's
/// bounded command channel never blocks during an outage. The `sleep` is pinned
/// so draining commands does not restart the backoff. Returns `Break` if the
/// command channel closed (caller gone).
async fn drain_backoff(cmd_rx: &mut mpsc::Receiver<Command>, delay: Duration) -> ControlFlow<()> {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return ControlFlow::Continue(()),
            cmd = cmd_rx.recv() => {
                if cmd.is_none() {
                    return ControlFlow::Break(());
                }
                // Discard the stale command; keep waiting out the backoff.
            }
        }
    }
}

/// A synthetic close reason for ends that carry no server close frame.
/// WS close codes that mean the request we sent is itself invalid, so
/// reconnecting and re-sending the identical setup can never succeed: 1002
/// (protocol error), 1003 (unsupported data), 1007 (invalid payload — Gemini
/// Live uses it for setup/schema validation failures). Transient/server codes
/// (1006 abnormal, 1011 server error, 1012/1013 restart/overloaded) stay
/// retryable.
fn is_permanent_close(code: u16) -> bool {
    matches!(code, 1002 | 1003 | 1007)
}

fn synth_reason(reason: &str) -> CloseReason {
    CloseReason {
        code: 0,
        reason: reason.to_string(),
    }
}

// --- Pure reconnect policy (ported from kutsu's `src/reconnect.rs`). ---------

/// Exponential backoff: `base` → ×2 → capped at `max`.
struct Backoff {
    current_ms: u64,
    base_ms: u64,
    max_ms: u64,
}

impl Backoff {
    fn new(base_ms: u64, max_ms: u64) -> Self {
        Backoff {
            current_ms: base_ms,
            base_ms,
            max_ms,
        }
    }

    fn next_delay(&mut self) -> std::time::Duration {
        let d = std::time::Duration::from_millis(self.current_ms);
        self.current_ms = (self.current_ms * 2).min(self.max_ms);
        d
    }

    fn reset(&mut self) {
        self.current_ms = self.base_ms;
    }
}

/// Consecutive-failure counter that signals when the (likely stale) resumption
/// handle should be dropped.
struct ReconnectState {
    fails: u32,
    reset_handle_after: u32,
}

impl ReconnectState {
    fn new(reset_handle_after: u32) -> Self {
        ReconnectState {
            fails: 0,
            reset_handle_after,
        }
    }

    fn on_success(&mut self) {
        self.fails = 0;
    }

    /// Record a failure; returns true when the caller should drop the handle.
    fn on_failure(&mut self) -> bool {
        self.fails += 1;
        self.reset_handle_after != 0 && self.fails >= self.reset_handle_after
    }

    /// The current consecutive-failure count (drives the terminal attempt bound).
    fn fails(&self) -> u32 {
        self.fails
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::FakeTransport;

    fn cfg() -> ClientConfig {
        ClientConfig {
            model: Model::HalfCascade,
            api_key: "K".into(),
            proxy: None,
            setup: SetupConfig {
                model: Model::HalfCascade,
                voice: "Autonoe".into(),
                language: Some("en-US".into()),
                system_instruction: "Be nice.".into(),
                temperature: 0.8,
                functions: Vec::new(),
                resume_handle: None,
            },
            // Matches kutsu: unbounded reconnect (backoff caps at 5s). Tests
            // that need deterministic termination override with `Some(n)`.
            max_reconnect_attempts: None,
        }
    }

    /// A reconnector that always fails.
    fn no_reconnect() -> Reconnector<FakeTransport> {
        Box::new(|| {
            Box::pin(async {
                Err(SessionError::Transport(TransportError::Connect(
                    "no reconnect".into(),
                )))
            })
        })
    }

    /// A reconnector that hands back `next` exactly once, then fails.
    fn once(next: FakeTransport) -> Reconnector<FakeTransport> {
        let mut slot = Some(next);
        Box::new(move || {
            let t = slot.take();
            Box::pin(async move {
                t.ok_or(SessionError::Transport(TransportError::Connect(
                    "drained".into(),
                )))
            })
        })
    }

    /// A reconnector that yields `n` accept-then-close transports (each accepts
    /// `setup`, then closes before any `setupComplete` — i.e. no progress),
    /// then fails.
    fn accept_then_close(n: usize) -> Reconnector<FakeTransport> {
        let mut queue: std::collections::VecDeque<FakeTransport> = (0..n)
            .map(|_| {
                let mut t = FakeTransport::new(false);
                t.push_close(1013, "overloaded");
                t
            })
            .collect();
        Box::new(move || {
            let t = queue.pop_front();
            Box::pin(async move {
                t.ok_or(SessionError::Transport(TransportError::Connect(
                    "drained".into(),
                )))
            })
        })
    }

    /// Poll `sent` until it has at least `n` frames (the driver task writes
    /// asynchronously), or panic after a bounded number of yields.
    async fn wait_for_sent(sent: &std::sync::Arc<std::sync::Mutex<Vec<String>>>, n: usize) {
        for _ in 0..1000 {
            if sent.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!(
            "timed out waiting for {n} sent frames (have {})",
            sent.lock().unwrap().len()
        );
    }

    #[tokio::test]
    async fn first_open_emits_session_opened_and_sends_setup() {
        let fake = FakeTransport::new(true);
        let sent = fake.sent.clone();
        let mut s = Session::connect_with_transport(cfg(), fake, no_reconnect())
            .await
            .unwrap();

        let ev = s.recv_event().await;
        assert!(matches!(
            ev,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        // setup was captured on the fake as the first outgoing frame.
        assert!(sent.lock().unwrap()[0].contains("\"setup\""));
    }

    #[tokio::test]
    async fn events_surface_in_order_from_scripted_frames() {
        // base64 "AAABAAIAAwA=" -> i16 LE [0,1,2,3].
        let mut fake = FakeTransport::new(true);
        fake.push_data(br#"{"setupComplete":{}}"#.to_vec());
        fake.push_data(
            br#"{"serverContent":{"modelTurn":{"parts":[{"inlineData":{"data":"AAABAAIAAwA="}}]}}}"#
                .to_vec(),
        );
        fake.push_data(
            br#"{"serverContent":{"outputTranscription":{"text":"Hi","finished":true}}}"#.to_vec(),
        );
        fake.push_data(
            br#"{"toolCall":{"functionCalls":[{"name":"end_call","id":"c1","args":{"d":"x"}}]}}"#
                .to_vec(),
        );
        fake.push_data(br#"{"serverContent":{"turnComplete":true}}"#.to_vec());
        fake.push_data(br#"{"serverContent":{"interrupted":true}}"#.to_vec());

        let mut s = Session::connect_with_transport(cfg(), fake, no_reconnect())
            .await
            .unwrap();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        assert!(
            matches!(s.recv_event().await, Some(Event::OutputAudio(a)) if a == vec![0, 1, 2, 3])
        );
        assert!(matches!(
            s.recv_event().await,
            Some(Event::Transcript { role: Role::Model, text, final_: true }) if text == "Hi"
        ));
        assert!(matches!(
            s.recv_event().await,
            Some(Event::ToolCall { name, id, .. }) if name == "end_call" && id == "c1"
        ));
        assert!(matches!(s.recv_event().await, Some(Event::TurnComplete)));
        assert!(matches!(s.recv_event().await, Some(Event::Interrupted)));
    }

    #[tokio::test(start_paused = true)]
    async fn resumption_handle_stored_not_surfaced_then_replayed_on_reconnect() {
        let mut first = FakeTransport::new(false);
        first.push_data(br#"{"setupComplete":{}}"#.to_vec());
        first.push_data(br#"{"sessionResumptionUpdate":{"newHandle":"H1"}}"#.to_vec());
        first.push_close(1011, "server restart");

        let second = FakeTransport::new(true);
        let second_sent = second.sent.clone();

        let mut s = Session::connect_with_transport(cfg(), first, once(second))
            .await
            .unwrap();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        // The handle update surfaces nothing; the next event is the transparent
        // reopen. It re-sent setup carrying "H1", so `resumed` is true.
        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: true,
                resumed: true
            })
        ));

        let setup = &second_sent.lock().unwrap()[0];
        assert!(setup.contains("\"setup\""));
        assert!(
            setup.contains("H1"),
            "reopened setup must carry the stored handle: {setup}"
        );
    }

    #[tokio::test]
    async fn send_audio_is_byte_identical_to_kutsu() {
        let fake = FakeTransport::new(true);
        let sent = fake.sent.clone();
        let mut s = Session::connect_with_transport(cfg(), fake, no_reconnect())
            .await
            .unwrap();
        let _ = s.recv_event().await; // SessionOpened (setup already sent)

        s.send_audio(&[0i16, 1i16]).await.unwrap();
        // The driver task writes the enqueued command asynchronously.
        wait_for_sent(&sent, 2).await;

        assert_eq!(
            sent.lock().unwrap().last().unwrap(),
            r#"{"realtime_input":{"audio":{"data":"AAABAA==","mimeType":"audio/pcm;rate=16000"}}}"#
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_close_emits_session_closed_then_none() {
        let mut fake = FakeTransport::new(false);
        fake.push_data(br#"{"setupComplete":{}}"#.to_vec());
        fake.push_close(1011, "boom");

        let mut c = cfg();
        c.max_reconnect_attempts = Some(1);
        let mut s = Session::connect_with_transport(c, fake, no_reconnect())
            .await
            .unwrap();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        match s.recv_event().await {
            Some(Event::SessionClosed { reason }) => assert_eq!(reason.code, 1011),
            other => panic!("expected SessionClosed, got {other:?}"),
        }
        assert!(s.recv_event().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_setup_rejection_terminates_without_reconnect() {
        // A no-progress close with a permanent code (1007 = invalid argument,
        // e.g. a malformed setup schema) must terminate immediately — even with
        // unbounded reconnect attempts (the production setting) and a reconnector
        // that would happily keep accepting. Re-sending an identical invalid
        // setup can never succeed; this is the WS-1007 reconnect-storm class.
        let mut initial = FakeTransport::new(false);
        initial.push_close(1007, "parameters.properties: only allowed for OBJECT type");

        // cfg() has max_reconnect_attempts: None (unbounded). accept_then_close
        // would let the driver reconnect forever if permanence were ignored.
        let mut s = Session::connect_with_transport(cfg(), initial, accept_then_close(100))
            .await
            .unwrap();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        match s.recv_event().await {
            Some(Event::SessionClosed { reason }) => assert_eq!(reason.code, 1007),
            other => panic!("expected terminal SessionClosed(1007), got {other:?}"),
        }
        assert!(s.recv_event().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_close_after_progress_still_reconnects() {
        // Guard against over-classification: a permanent-coded close on an
        // attempt that DID progress (reached setupComplete) is treated as a
        // normal drop and still reconnects — only no-progress setup rejections
        // are fatal.
        let mut initial = FakeTransport::new(false);
        initial.push_data(br#"{"setupComplete":{}}"#.to_vec());
        initial.push_close(1007, "mid-session");

        let mut c = cfg();
        c.max_reconnect_attempts = Some(1);
        let mut s = Session::connect_with_transport(c, initial, accept_then_close(1))
            .await
            .unwrap();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        // Progressed → the 1007 drop is retried, not fatal.
        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: true,
                ..
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn accept_then_close_storm_backs_off_and_terminates_at_the_bound() {
        let mut initial = FakeTransport::new(false);
        initial.push_close(1013, "overloaded");

        let mut c = cfg();
        c.max_reconnect_attempts = Some(3);
        let mut s = Session::connect_with_transport(c, initial, accept_then_close(2))
            .await
            .unwrap();

        let start = tokio::time::Instant::now();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));
        // Each reopen is a no-progress accept-then-close with no stored handle.
        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: true,
                resumed: false
            })
        ));
        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: true,
                resumed: false
            })
        ));
        match s.recv_event().await {
            Some(Event::SessionClosed { reason }) => assert_eq!(reason.code, 1013),
            other => panic!("expected SessionClosed after the bound, got {other:?}"),
        }
        assert!(s.recv_event().await.is_none());

        assert!(
            start.elapsed() >= std::time::Duration::from_millis(800),
            "reconnects must back off with an advancing delay, elapsed = {:?}",
            start.elapsed()
        );
    }

    /// The regression the redesign fixes: while the session is reconnecting
    /// (transport closed, backoff pending), the caller keeps sending audio AND
    /// polling events. With the reconnect owned by the driver task, caller
    /// traffic can no longer cancel/starve the backoff — the reopen completes
    /// and `SessionOpened { is_reconnect: true }` is delivered. (Under the old
    /// caller-driven `next_event`, the ~20 ms audio arm cancelled the backoff on
    /// every frame and the reconnect never completed.)
    #[tokio::test(start_paused = true)]
    async fn continuous_audio_does_not_starve_reconnect() {
        let mut first = FakeTransport::new(false);
        first.push_data(br#"{"setupComplete":{}}"#.to_vec());
        first.push_close(1011, "server restart");
        let second = FakeTransport::new(true); // stays open after reopen
        let mut s = Session::connect_with_transport(cfg(), first, once(second))
            .await
            .unwrap();

        assert!(matches!(
            s.recv_event().await,
            Some(Event::SessionOpened {
                is_reconnect: false,
                resumed: false
            })
        ));

        // Simulate continuous ~20 ms uplink while awaiting the reopen: enqueue a
        // frame, then poll for an event with a short timeout, repeatedly. The
        // send + recv are sequential (mirrors kutsu's select! where the send runs
        // in the audio-arm body after recv_event resolves), so no borrow clash.
        let mut got_reopen = false;
        for _ in 0..2000 {
            let _ = s.send_audio(&[0i16; 160]).await;
            match tokio::time::timeout(Duration::from_millis(20), s.recv_event()).await {
                Ok(Some(Event::SessionOpened {
                    is_reconnect: true, ..
                })) => {
                    got_reopen = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {} // timeout: keep sending audio, keep waiting
            }
        }
        assert!(
            got_reopen,
            "reconnect must complete despite continuous audio traffic"
        );
    }

    #[test]
    fn backoff_doubles_and_caps_then_resets() {
        let mut b = Backoff::new(300, 5000);
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(300));
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(600));
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(1200));
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(2400));
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(4800));
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(5000)); // capped
        b.reset();
        assert_eq!(b.next_delay(), std::time::Duration::from_millis(300));
    }

    #[test]
    fn reconnect_state_drops_handle_after_n_failures() {
        let mut s = ReconnectState::new(HANDLE_DROP_AFTER);
        assert!(!s.on_failure());
        assert!(!s.on_failure());
        assert!(!s.on_failure());
        assert!(s.on_failure()); // 4th -> drop
        s.on_success();
        assert!(!s.on_failure()); // reset by success
    }
}
