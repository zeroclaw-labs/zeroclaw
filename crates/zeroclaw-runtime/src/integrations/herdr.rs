//! Herdr integration — agent lifecycle reporting to the Herdr sidebar.
//!
//! This integration is purely environment-variable driven. There is no `[herdr]`
//! config section. Enable it by setting these env vars:
//!
//! - `HERDR_ENV=1` — must be set to activate the integration
//! - `HERDR_SOCKET_PATH` — path to the Herdr daemon's Unix socket
//! - `HERDR_PANE_ID` — the Herdr pane identifier
//!
//! Uses tokio for async UDS I/O with bounded timeouts. Messages are sent
//! fire-and-forget; flush synchronously waits for pending writes at shutdown.

use std::future::Future;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;

use zeroclaw_api::observability_traits::ObserverMetric;

use crate::observability::{
    BroadcastHookGuard, Observer, ObserverEvent, set_scoped_broadcast_hook,
};

// ── I/O timeouts ──────────────────────────────────────────────────────────────

/// Maximum time to wait for a UDS connect before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
/// Maximum time to wait for a UDS write before giving up.
const IO_TIMEOUT: Duration = Duration::from_millis(500);
/// Maximum time to wait for the writer task to drain all pending messages
/// at shutdown. Bounds the total teardown latency.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Wall-clock seed for the sequence counter, in microseconds since the epoch.
///
/// Seeding from the clock is what makes sequence numbers survive a restart:
/// Herdr orders reports by `seq`, so a fresh process must not resume from a
/// value below the one it last sent. Split out from `next_seq` so the seeding
/// rule can be tested independently of the process-global counter, whose value
/// depends on whichever caller initialized it first.
fn seq_base() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(1_000_000_000_000_000)
}

/// Connect to a Unix domain socket with a timeout using tokio.
#[cfg(unix)]
async fn connect_with_timeout(path: &str) -> Result<UnixStream, std::io::Error> {
    timeout(CONNECT_TIMEOUT, UnixStream::connect(path))
        .await
        .unwrap_or(Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "herdr connect timed out",
        )))
}

/// Write a JSON-RPC notification to a connected UDS stream with bounded timeouts.
#[cfg(unix)]
async fn send_on_stream(stream: &mut UnixStream, payload: &str) -> Result<(), std::io::Error> {
    timeout(IO_TIMEOUT, async {
        stream.write_all(payload.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await
    })
    .await
    .unwrap_or(Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "herdr write timed out",
    )))
}

// ── Socket discovery ─────────────────────────────────────────────────────────

const SOURCE: &str = "herdr:zeroclaw";
const AGENT: &str = "zeroclaw";

/// Try to install a HerdrObserver via the broadcast hook. Returns a guard
/// that uninstalls it on drop, or `None` if the herdr environment isn't
/// active (not running inside a herdr pane) or the caller is not the
/// interactive CLI agent path.
///
/// `interactive` must be `true` for the hook to be installed. The Herdr
/// integration is advertised as CLI-interactive-only; daemon, cron, channel,
/// and subagent callers pass `interactive = false` and must not mutate the
/// pane's process-wide Herdr state, since their lifecycle and flush
/// assumptions differ from the CLI one-shot / REPL path.
pub fn try_install_hook(
    interactive: bool,
    agent_alias: &str,
    owning_turn_id: Option<&str>,
) -> Option<BroadcastHookGuard> {
    if !interactive {
        return None;
    }
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return None;
    }
    let socket_path = std::env::var("HERDR_SOCKET_PATH").ok()?;
    let pane_id = std::env::var("HERDR_PANE_ID").ok()?;
    install_hook_from_env(socket_path, pane_id, agent_alias, owning_turn_id)
}

/// Install the hook from already-resolved env values. Factored out of
/// [`try_install_hook`] so the gating logic can be tested without touching
/// the process environment (`std::env::set_var` is `unsafe` on Rust >= 1.80
/// because it is not thread-safe with concurrent reads).
fn install_hook_from_env(
    socket_path: String,
    pane_id: String,
    agent_alias: &str,
    owning_turn_id: Option<&str>,
) -> Option<BroadcastHookGuard> {
    // UDS is Unix-only; silently skip on other platforms.
    #[cfg(not(unix))]
    {
        let _ = (socket_path, pane_id, agent_alias, owning_turn_id);
        return None;
    }

    let client = HerdrClient::new(socket_path, pane_id.clone());

    // Compute unique display name: agent alias + last 2 chars of pane_id.
    // Use char-aware slicing to handle multi-byte UTF-8 pane IDs safely.
    let display_name = {
        let chars: Vec<char> = pane_id.chars().collect();
        if chars.len() > 2 {
            let suffix: String = chars[chars.len() - 2..].iter().collect();
            format!("{}-{}", agent_alias, suffix)
        } else {
            agent_alias.to_string()
        }
    };
    client.report_metadata(&display_name);

    // Clear any stale state from a previous crashed session before installing
    // the observer. The wall-clock-seeded seq ensures this call is accepted even
    // if herdr retains a higher seq from a prior session.
    let _ = client.send("pane.release_agent", &serde_json::Map::new());
    // Report initial idle state so herdr shows the agent immediately, even
    // before any user message triggers a state transition.
    client.report_state("idle");
    // Startup messages are best-effort; the first ObserverEvent will re-emit
    // idle if the daemon was unavailable.
    let observer = Arc::new(HerdrObserver::new(client, owning_turn_id));
    Some(set_scoped_broadcast_hook(observer))
}

// ── HerdrClient ──────────────────────────────────────────────────────────────

#[cfg(test)]
type SpyFn = Arc<dyn Fn(&str, &serde_json::Map<String, serde_json::Value>) + Send + Sync>;

/// Maximum number of pending messages in the writer queue.
const WRITER_QUEUE_CAPACITY: usize = 64;

/// Reserved capacity for terminal lifecycle payloads (`idle` + `release_agent`).
const TERMINAL_QUEUE_CAPACITY: usize = 4;

/// Connect and write one payload, ignoring transport failures.
#[cfg(unix)]
async fn write_payload(socket_path: &str, payload: &str) {
    if let Ok(mut stream) = connect_with_timeout(socket_path).await {
        let _ = send_on_stream(&mut stream, payload).await;
    }
}

/// Signals a blocked synchronous drain if the writer thread panics.
#[cfg(unix)]
struct DrainOnPanic(Option<std_mpsc::SyncSender<()>>);

#[cfg(unix)]
impl Drop for DrainOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(tx) = self.0.take()
        {
            let _ = tx.send(());
        }
    }
}

#[cfg(unix)]
async fn run_writer<F, Fut>(
    mut rx: mpsc::Receiver<String>,
    mut terminal_rx: mpsc::Receiver<String>,
    mut shutdown_rx: oneshot::Receiver<()>,
    drain_done_tx: std_mpsc::SyncSender<()>,
    write: F,
) where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut terminal_only = false;
    loop {
        tokio::select! {
            biased;
            Some(payload) = terminal_rx.recv() => {
                write(payload).await;
                terminal_only = true;
            }
            _ = &mut shutdown_rx => {
                while let Ok(payload) = terminal_rx.try_recv() {
                    write(payload).await;
                }
                let _ = drain_done_tx.send(());
                break;
            }
            maybe_payload = rx.recv(), if !terminal_only => {
                match maybe_payload {
                    Some(payload) => {
                        enum SnapshotOutcome {
                            Completed,
                            Terminal(String),
                            Shutdown,
                        }

                        let outcome = {
                            let snapshot_write = write(payload);
                            tokio::pin!(snapshot_write);
                            tokio::select! {
                                biased;
                                Some(payload) = terminal_rx.recv() => {
                                    SnapshotOutcome::Terminal(payload)
                                }
                                _ = &mut shutdown_rx => SnapshotOutcome::Shutdown,
                                _ = &mut snapshot_write => SnapshotOutcome::Completed,
                            }
                        };

                        match outcome {
                            SnapshotOutcome::Completed => {}
                            SnapshotOutcome::Terminal(payload) => {
                                // Drop the snapshot connection before terminal I/O.
                                write(payload).await;
                                terminal_only = true;
                            }
                            SnapshotOutcome::Shutdown => {
                                while let Ok(payload) = terminal_rx.try_recv() {
                                    write(payload).await;
                                }
                                let _ = drain_done_tx.send(());
                                break;
                            }
                        }
                    }
                    None => {
                        while let Ok(payload) = terminal_rx.try_recv() {
                            write(payload).await;
                        }
                        let _ = drain_done_tx.send(());
                        break;
                    }
                }
            }
        }
    }
}

/// Client that sends JSON-RPC notifications to the herdr daemon via tokio UDS.
/// The `send()` method serializes and fires off an async write — it never
/// blocks the caller. Call `shutdown_drain()` to wait until pending writes complete
/// (used at startup and shutdown for best-effort delivery).
pub(crate) struct HerdrClient {
    pane_id: String,
    #[cfg(test)]
    spy: Option<SpyFn>,
    #[cfg(unix)]
    writer: Mutex<Option<mpsc::Sender<String>>>,
    /// Reserved sender for the terminal pair. Kept separate from `writer` so a
    /// saturated snapshot queue cannot drop it, and so the writer thread can
    /// service it ahead of any queued snapshot.
    #[cfg(unix)]
    terminal: Mutex<Option<mpsc::Sender<String>>>,
    #[cfg(unix)]
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Sync channel signaled by the writer thread when it has finished draining.
    /// Allows the sync `flush()` path to wait without `block_in_place`.
    #[cfg(unix)]
    drain_done_rx: Mutex<Option<std_mpsc::Receiver<()>>>,
}

impl HerdrClient {
    pub(crate) fn new(socket_path: String, pane_id: String) -> Self {
        #[cfg(unix)]
        {
            let socket_path: Arc<str> = Arc::from(socket_path);
            Self::new_with_writer(pane_id, move |payload| {
                let socket_path = Arc::clone(&socket_path);
                async move {
                    write_payload(&socket_path, &payload).await;
                }
            })
        }
        #[cfg(not(unix))]
        {
            let _ = socket_path;
            Self {
                pane_id,
                #[cfg(test)]
                spy: None,
            }
        }
    }

    #[cfg(unix)]
    fn new_with_writer<F, Fut>(pane_id: String, write: F) -> Self
    where
        F: Fn(String) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<String>(WRITER_QUEUE_CAPACITY);
        let (terminal_tx, terminal_rx) = mpsc::channel::<String>(TERMINAL_QUEUE_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (drain_done_tx, drain_done_rx) = std_mpsc::sync_channel::<()>(1);
        let drain_tx_for_panic = drain_done_tx.clone();
        let _writer_handle = std::thread::Builder::new()
            .name("zeroclaw-herdr-writer".into())
            .spawn(move || {
                let _drain_guard = DrainOnPanic(Some(drain_tx_for_panic));
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = drain_done_tx.send(());
                    return;
                };
                runtime.block_on(run_writer(
                    rx,
                    terminal_rx,
                    shutdown_rx,
                    drain_done_tx,
                    write,
                ));
            });
        Self {
            pane_id,
            #[cfg(test)]
            spy: None,
            writer: Mutex::new(Some(tx)),
            terminal: Mutex::new(Some(terminal_tx)),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            drain_done_rx: Mutex::new(Some(drain_done_rx)),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_spy<F>(pane_id: String, spy: F) -> Self
    where
        F: Fn(&str, &serde_json::Map<String, serde_json::Value>) + Send + Sync + 'static,
    {
        Self {
            pane_id,
            spy: Some(Arc::new(spy)),
            #[cfg(unix)]
            writer: Mutex::new(None),
            #[cfg(unix)]
            terminal: Mutex::new(None),
            #[cfg(unix)]
            shutdown_tx: Mutex::new(None),
            #[cfg(unix)]
            drain_done_rx: Mutex::new(None),
        }
    }

    /// Wait for the writer task to drain all pending messages and exit.
    /// Uses a sync channel so this can be called from a sync context without
    /// `block_in_place`. The timeout bounds the total drain time.
    pub(crate) fn shutdown_drain(&self, timeout_dur: Duration) {
        #[cfg(unix)]
        {
            // Close the sender so no new messages can be queued
            self.writer.lock().take();

            // Signal the writer thread to enter drain mode
            if let Some(shutdown_tx) = self.shutdown_tx.lock().take() {
                let _ = shutdown_tx.send(());
            }

            // Wait for drain completion via the sync channel with a timeout.
            if let Some(rx) = self.drain_done_rx.lock().take() {
                let deadline = Instant::now() + timeout_dur;
                if let Some(rem) = deadline.checked_duration_since(Instant::now())
                    && !rem.is_zero()
                {
                    let _ = rx.recv_timeout(rem);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = timeout_dur;
        }
    }

    fn next_seq(&self) -> u64 {
        static NEXT_SEQ: OnceLock<AtomicU64> = OnceLock::new();
        let counter = NEXT_SEQ.get_or_init(|| AtomicU64::new(seq_base()));
        counter.fetch_add(1, Ordering::Relaxed)
    }

    fn request_id(&self) -> String {
        format!("{SOURCE}:{}", self.next_seq())
    }

    fn build_payload(
        &self,
        method: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, std::io::Error> {
        let mut params_map = serde_json::Map::new();
        params_map.insert(
            "pane_id".into(),
            serde_json::Value::String(self.pane_id.clone()),
        );
        params_map.insert("source".into(), serde_json::Value::String(SOURCE.into()));
        params_map.insert("agent".into(), serde_json::Value::String(AGENT.into()));
        params_map.insert(
            "seq".into(),
            serde_json::Value::Number(self.next_seq().into()),
        );
        for (k, v) in params {
            params_map.insert(k.clone(), v.clone());
        }

        let payload = serde_json::json!({
            "id": self.request_id(),
            "method": method,
            "params": params_map,
        });

        Ok(serde_json::to_string(&payload)?)
    }

    /// Enqueue a lifecycle *snapshot*. Best-effort: a saturated queue drops the
    /// message, which is acceptable because the next transition supersedes it.
    fn send(
        &self,
        method: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), std::io::Error> {
        #[cfg(test)]
        if let Some(spy) = &self.spy {
            spy(method, params);
            return Ok(());
        }

        let payload_str = self.build_payload(method, params)?;

        // Fire-and-forget: push to writer task via bounded channel. Use
        // `try_send` so the caller never blocks on a slow/unavailable peer.
        // On queue full, drop the new message.
        #[cfg(unix)]
        if let Some(tx) = self.writer.lock().as_ref() {
            match tx.try_send(payload_str) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }

        Ok(())
    }

    /// Enqueue a *terminal* payload on the reserved path.
    ///
    /// Returns `true` only when the payload reached the writer. Callers must not
    /// commit terminal state on `false`: `transit_to` suppresses repeat
    /// transitions, so a committed-but-undelivered `Released` would block the
    /// retry that `flush()` would otherwise make, stranding the pane as
    /// `working` forever.
    fn send_terminal(
        &self,
        method: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        #[cfg(test)]
        if let Some(spy) = &self.spy {
            spy(method, params);
            return true;
        }

        let Ok(payload_str) = self.build_payload(method, params) else {
            return false;
        };

        #[cfg(unix)]
        {
            let tx = self.terminal.lock();
            match tx.as_ref() {
                Some(tx) => tx.try_send(payload_str).is_ok(),
                None => false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = payload_str;
            true
        }
    }

    fn report_state(&self, state: &str) {
        let mut params = serde_json::Map::new();
        params.insert("state".into(), serde_json::Value::String(state.into()));
        let _ = self.send("pane.report_agent", &params);
    }

    /// Report the terminal pair (`idle` then `release_agent`) on the reserved
    /// path. Returns `true` only when both payloads were queued.
    fn report_terminal(&self) -> bool {
        let mut params = serde_json::Map::new();
        params.insert("state".into(), serde_json::Value::String("idle".into()));
        let idle_queued = self.send_terminal("pane.report_agent", &params);
        let release_queued = self.send_terminal("pane.release_agent", &serde_json::Map::new());
        idle_queued && release_queued
    }

    fn report_metadata(&self, display_agent: &str) {
        let mut params = serde_json::Map::new();
        params.insert(
            "display_agent".into(),
            serde_json::Value::String(display_agent.into()),
        );
        let _ = self.send("pane.report_metadata", &params);
    }
}

// ── HerdrState ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HerdrState {
    Idle,
    Working,
    Blocked,
    Released,
}

// ── HerdrObserver ────────────────────────────────────────────────────────────

/// Observer that reports agent lifecycle to the herdr daemon.
///
/// State machine: `Idle` → activity event → `Working` → `AgentEnd` → `Idle`.
///
/// Events are filtered by `owning_turn_id`: only events whose `turn_id`
/// matches the owning interactive run are forwarded to herdr. This isolates
/// nested non-interactive runs (subagents) from the parent's pane state.
/// Child agents pass `interactive = false` to `try_install_hook`, which
/// returns `None` and installs no hook; even if they did install one, the
/// `owning_turn_id` filter would prevent their events from reaching the
/// parent's observer.
pub struct HerdrObserver {
    state: Mutex<HerdrState>,
    client: HerdrClient,
    /// Owning turn identity for event filtering.
    owning_turn_id: Option<String>,
}

impl HerdrObserver {
    pub(crate) fn new(client: HerdrClient, owning_turn_id: Option<&str>) -> Self {
        Self {
            state: Mutex::new(HerdrState::Idle),
            client,
            owning_turn_id: owning_turn_id.map(|s| s.to_owned()),
        }
    }

    fn transit_to(&self, state: &mut HerdrState, target: HerdrState) {
        if *state == target {
            return;
        }
        match target {
            // Commit only once the pair is queued on the reserved path. If the
            // enqueue fails the state stays put so `flush()` can retry, rather
            // than reporting a release that was never sent.
            HerdrState::Released => {
                if self.client.report_terminal() {
                    *state = target;
                }
            }
            HerdrState::Working => {
                *state = target;
                self.client.report_state("working");
            }
            HerdrState::Idle => {
                *state = target;
                self.client.report_state("idle");
            }
            HerdrState::Blocked => {
                *state = target;
                self.client.report_state("blocked");
            }
        }
    }
}

impl Observer for HerdrObserver {
    fn record_event(&self, event: &ObserverEvent) {
        // Filter by owning turn_id: only events from the owning interactive
        // run are forwarded. This prevents child agents (subagents) from
        // mutating the parent's herdr pane state.
        if let Some(owning) = self.owning_turn_id.as_deref() {
            let event_turn: Option<Option<&str>> = match event {
                ObserverEvent::AgentStart { turn_id, .. }
                | ObserverEvent::LlmRequest { turn_id, .. }
                | ObserverEvent::LlmResponse { turn_id, .. }
                | ObserverEvent::AgentEnd { turn_id, .. }
                | ObserverEvent::ToolCallStart { turn_id, .. }
                | ObserverEvent::ToolCall { turn_id, .. }
                | ObserverEvent::HistoryTrimmed { turn_id, .. }
                | ObserverEvent::AuthorizationRequested { turn_id, .. }
                | ObserverEvent::AuthorizationResponded { turn_id, .. } => Some(turn_id.as_deref()),
                ObserverEvent::TurnComplete => Some(None),
                ObserverEvent::TurnCompleteAttributed { turn_id } => Some(Some(turn_id.as_str())),
                _ => None,
            };
            if event_turn.is_some_and(|turn_id| turn_id != Some(owning)) {
                return;
            }
        }
        let mut state = self.state.lock();
        match event {
            ObserverEvent::AgentStart { .. } => {
                self.transit_to(&mut state, HerdrState::Idle);
            }
            ObserverEvent::LlmRequest { .. } | ObserverEvent::ToolCallStart { .. } => {
                self.transit_to(&mut state, HerdrState::Working);
            }
            ObserverEvent::LlmResponse { .. } => {
                self.transit_to(&mut state, HerdrState::Working);
            }
            ObserverEvent::ToolCall { .. } => {
                self.transit_to(&mut state, HerdrState::Working);
            }
            ObserverEvent::TurnComplete => {
                if self.owning_turn_id.is_none() {
                    self.transit_to(&mut state, HerdrState::Idle);
                }
            }
            ObserverEvent::TurnCompleteAttributed { .. } => {
                self.transit_to(&mut state, HerdrState::Idle);
            }
            ObserverEvent::AgentEnd { .. } => {
                self.transit_to(&mut state, HerdrState::Released);
            }
            ObserverEvent::AuthorizationRequested { .. } => {
                self.transit_to(&mut state, HerdrState::Blocked);
            }
            ObserverEvent::AuthorizationResponded { granted, .. } => {
                if *granted {
                    self.transit_to(&mut state, HerdrState::Working);
                } else {
                    self.transit_to(&mut state, HerdrState::Idle);
                }
            }
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn flush(&self) {
        {
            let mut state = self.state.lock();
            self.transit_to(&mut state, HerdrState::Released);
        }
        self.client.shutdown_drain(SHUTDOWN_TIMEOUT);
    }

    fn name(&self) -> &str {
        "herdr"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::observability::{
        FlushGuard, HOOK_TEST_LOCK, clear_broadcast_hook, create_observer,
        set_scoped_broadcast_hook,
    };
    use std::future::pending;
    use std::io::Read as _;
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::{Duration, Instant};
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::sync::Notify;
    use zeroclaw_config::schema::ObservabilityConfig;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A spy that captures all `pane.report_agent` / `pane.release_agent`
    /// calls instead of sending them over UDS.
    #[derive(Clone, Default)]
    pub(crate) struct HerdrSpy {
        calls: Arc<Mutex<Vec<HerdrSpyCall>>>,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct HerdrSpyCall {
        pub method: String,
        pub params: serde_json::Value,
    }

    impl HerdrSpy {
        pub(crate) fn into_inner(self) -> Arc<Mutex<Vec<HerdrSpyCall>>> {
            self.calls
        }
    }

    /// Build a `HerdrClient` with a spy instead of connecting to a real UDS socket.
    pub(crate) fn make_spy_reporter() -> (HerdrClient, Arc<Mutex<Vec<HerdrSpyCall>>>) {
        let spy = HerdrSpy::default();
        let calls = spy.into_inner();
        let calls_clone = calls.clone();
        let client = HerdrClient::new_with_spy("test-pane".into(), move |method, params| {
            calls_clone.lock().push(HerdrSpyCall {
                method: method.to_string(),
                params: serde_json::Value::Object(params.clone()),
            });
        });
        (client, calls)
    }

    #[tokio::test]
    async fn send_fire_and_forget_returns_immediately() {
        let client = HerdrClient::new(
            "/tmp/nonexistent-herdr-test-socket.sock".into(),
            "test-pane".into(),
        );

        let start = std::time::Instant::now();
        let _result = client.send("pane.report_agent", &serde_json::Map::new());
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(100),
            "fire-and-forget send should not block the caller, took {:?}",
            elapsed,
        );
    }

    /// Startup path with stale socket must return quickly. This tests the
    /// real blocker: install_hook_from_env creates a client, sends two
    /// messages (release_agent + report_state), and returns. With a stale
    /// socket, each connect attempt times out in 200ms; two messages = 400ms.
    /// We allow some slack for task spawn overhead.
    #[tokio::test]
    async fn startup_with_stale_socket_returns_quickly() {
        let _hook_lock = HOOK_TEST_LOCK.lock().await;
        clear_broadcast_hook();

        let start = std::time::Instant::now();
        let _guard = install_hook_from_env(
            "/tmp/nonexistent-herdr-test-socket.sock".into(),
            "test-pane".into(),
            "test-agent",
            None,
        );
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(500),
            "startup with unavailable herdr socket should return quickly, took {:?}",
            elapsed,
        );
    }

    /// `try_install_hook(interactive)` must return `None` for non-interactive
    /// callers (daemon, cron, channels, subagents) regardless of env state.
    /// The integration is advertised as CLI-interactive-only and must not
    /// mutate pane state from other paths.
    ///
    /// We avoid `std::env::set_var` here because it is `unsafe` on Rust >= 1.80
    /// (not thread-safe with concurrent reads). The `interactive` gate runs
    /// before any env access, so we can verify it without touching the
    /// environment.
    #[test]
    fn try_install_hook_skips_non_interactive() {
        // Non-interactive callers must never install the hook, even if env
        // vars were set by some other process. The gate short-circuits before
        // any env read.
        assert!(
            try_install_hook(false, "test-agent", None).is_none(),
            "try_install_hook(false) must return None without consulting env vars"
        );
    }

    /// Non-ASCII pane IDs (e.g., emoji) must not panic on UTF-8 slicing.
    /// This tests the fix: display_name uses char-aware suffix extraction
    /// instead of byte indexing, which would panic on multi-byte chars like 🦀.
    #[tokio::test]
    async fn non_ascii_pane_id_does_not_panic() {
        let _hook_lock = HOOK_TEST_LOCK.lock().await;
        clear_broadcast_hook();

        let _guard = install_hook_from_env(
            "/tmp/nonexistent-herdr-test-socket.sock".into(),
            "test-🦀".into(),
            "test-agent",
            None,
        );
    }

    /// `HerdrObserver::flush()` must emit the idle + release_agent
    /// notifications exactly once and transition to `Released`, matching the
    /// AgentEnd / run-teardown drain contract.
    #[tokio::test]
    async fn herdr_observer_flush_drains_release_messages() {
        let (client, calls) = make_spy_reporter();
        let observer = HerdrObserver::new(client, None);

        // Simulate the agent reaching Working state first so flush has
        // something to release from.
        observer.record_event(&ObserverEvent::LlmRequest {
            model_provider: "test".into(),
            model: "test".into(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        calls.lock().clear();

        observer.flush();

        let captured: Vec<HerdrSpyCall> = calls.lock().clone();
        let methods: Vec<&str> = captured.iter().map(|c| c.method.as_str()).collect();

        // The flush must emit exactly two messages: an idle state report
        // followed by a release_agent notification.
        assert_eq!(
            captured.len(),
            2,
            "flush must emit exactly idle + release_agent, got {:?}",
            methods
        );
        assert_eq!(
            captured[0].method, "pane.report_agent",
            "first flush message must be a state report, got {:?}",
            methods
        );
        assert_eq!(
            captured[0].params.get("state").and_then(|s| s.as_str()),
            Some("idle"),
            "first flush message must report idle state"
        );
        assert_eq!(
            captured[1].method, "pane.release_agent",
            "second flush message must be release_agent, got {:?}",
            methods
        );

        // Double-flush is a no-op — the observer is already Released.
        let count_after_first = calls.lock().len();
        observer.flush();
        assert_eq!(
            calls.lock().len(),
            count_after_first,
            "second flush must not emit duplicate release messages"
        );
    }

    /// `next_seq()` must return monotonically increasing values starting from
    /// a wall-clock-seeded base. This ensures restart resilience: a process
    /// restarted after herdr stores a prior seq will have a higher starting
    /// value, avoiding silent message rejection.
    #[tokio::test]
    async fn next_seq_is_monotonic_and_restart_safe() {
        let client = HerdrClient::new(
            "/tmp/nonexistent-herdr-test-socket.sock".into(),
            "test-pane".into(),
        );

        let seq1 = client.next_seq();
        let seq2 = client.next_seq();
        let seq3 = client.next_seq();

        assert!(seq2 > seq1, "seq must be monotonic: {} <= {}", seq2, seq1);
        assert!(seq3 > seq2, "seq must be monotonic: {} <= {}", seq3, seq2);

        // Note: the absolute value of seq1 depends on whether the static OnceLock
        // was initialized by a prior test. We only assert monotonicity here,
        // which is the critical property for restart resilience (a fresh process
        // will always seed from wall clock on first use).
    }

    async fn collect_methods(listener: UnixListener, want: usize, budget: Duration) -> Vec<String> {
        let mut received = Vec::new();
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline && received.len() < want {
            if let Ok(Ok((stream, _))) =
                tokio::time::timeout(Duration::from_millis(50), listener.accept()).await
            {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_ok()
                    && let Ok(val) = serde_json::from_str::<serde_json::Value>(&line)
                    && let Some(method) = val.get("method").and_then(|m| m.as_str())
                {
                    received.push(method.to_string());
                }
            }
        }
        received
    }

    #[tokio::test(flavor = "current_thread")]
    async fn herdr_flush_delivers_terminal_pair_on_current_thread_runtime() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("herdr-test-current-thread.sock");
        let sock_str = sock_path.to_str().unwrap().to_string();

        let listener = StdUnixListener::bind(&sock_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let receiver = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut received = Vec::new();
            while Instant::now() < deadline && received.len() < 2 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(true).unwrap();
                        let mut payload = Vec::new();
                        while Instant::now() < deadline {
                            let mut chunk = [0_u8; 1024];
                            match stream.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(read) => {
                                    payload.extend_from_slice(&chunk[..read]);
                                    if payload.contains(&b'\n') {
                                        break;
                                    }
                                }
                                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                                Err(_) => break,
                            }
                        }
                        if let Some(line) = payload.split(|byte| *byte == b'\n').next()
                            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(line)
                            && let Some(method) = value.get("method").and_then(|m| m.as_str())
                        {
                            received.push(method.to_string());
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            received
        });

        let observer = HerdrObserver::new(HerdrClient::new(sock_str, "test-pane".into()), None);
        observer.flush();

        let received = receiver.join().unwrap();
        assert_eq!(
            received,
            ["pane.report_agent", "pane.release_agent"],
            "flush must deliver the terminal pair while the caller's current-thread runtime is blocked"
        );
    }

    /// Shutdown drain test: verify ordered receipt of `idle` then
    /// `pane.release_agent` before shutdown completes. Uses a real
    /// `UnixListener` to receive messages and confirm ordering.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn herdr_shutdown_drain_ordered_receipt() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("herdr-test.sock");
        let sock_str = sock_path.to_str().unwrap().to_string();

        let listener = UnixListener::bind(&sock_path).unwrap();
        let collector = zeroclaw_spawn::spawn!(async move {
            collect_methods(listener, 2, Duration::from_secs(3)).await
        });

        let client = HerdrClient::new(sock_str.clone(), "test-pane".into());
        assert!(client.report_terminal(), "terminal pair must enqueue");

        client.shutdown_drain(SHUTDOWN_TIMEOUT);
        let received = collector.await.unwrap();

        // Verify ordered receipt: idle then release_agent
        assert_eq!(
            received.len(),
            2,
            "expected 2 messages, got {}: {:?}",
            received.len(),
            received
        );
        assert_eq!(received[0], "pane.report_agent");
        assert_eq!(received[1], "pane.release_agent");
    }

    /// Saturation regression: the terminal pair must survive a snapshot queue
    /// that is full. Enqueues well past `WRITER_QUEUE_CAPACITY` so `try_send`
    /// is dropping snapshots, then asserts the release still reaches the peer.
    /// Before the reserved terminal path this lost the release outright, and
    /// the committed `Released` state suppressed any retry.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn herdr_terminal_pair_survives_saturated_queue() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("herdr-test-saturated.sock");
        let sock_str = sock_path.to_str().unwrap().to_string();

        let listener = UnixListener::bind(&sock_path).unwrap();
        // Ask for more than can arrive so the collector runs to its deadline and
        // captures the tail of the stream rather than stopping early.
        let collector = zeroclaw_spawn::spawn!(async move {
            collect_methods(listener, 4096, Duration::from_secs(5)).await
        });

        let client = HerdrClient::new(sock_str.clone(), "test-pane".into());
        // Saturate: 4x capacity, enqueued far faster than the writer can drain
        // (each payload costs a connect + write).
        for _ in 0..(WRITER_QUEUE_CAPACITY * 4) {
            client.report_state("working");
        }
        assert!(
            client.report_terminal(),
            "terminal pair must enqueue even when the snapshot queue is full"
        );

        client.shutdown_drain(SHUTDOWN_TIMEOUT);

        let received = collector.await.unwrap();
        let release_at = received.iter().position(|m| m == "pane.release_agent");
        let release_at = release_at.unwrap_or_else(|| {
            panic!(
                "terminal release must be delivered despite backpressure; got {} messages: {:?}",
                received.len(),
                received
            )
        });
        assert!(
            release_at > 0 && received[release_at - 1] == "pane.report_agent",
            "release must be immediately preceded by the terminal idle: {:?}",
            &received[release_at.saturating_sub(2)..=release_at]
        );
    }

    /// Bounded wait against a peer that genuinely accepts but never reads, so
    /// writes block on the socket rather than failing fast. The drain must
    /// still return within the timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn herdr_shutdown_drain_bounded_wait() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("herdr-test-slow.sock");
        let sock_str = sock_path.to_str().unwrap().to_string();

        let listener = UnixListener::bind(&sock_path).unwrap();
        // Accept every connection and then sit on it without reading, holding
        // the streams open for the life of the test.
        let acceptor = zeroclaw_spawn::spawn!(async move {
            let mut held = Vec::new();
            loop {
                match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
                    Ok(Ok((stream, _))) => held.push(stream),
                    Ok(Err(_)) => break,
                    Err(_) => {}
                }
            }
        });

        let client = HerdrClient::new(sock_str.clone(), "test-pane".into());
        for _ in 0..WRITER_QUEUE_CAPACITY {
            client.report_state("working");
        }
        assert!(client.report_terminal(), "terminal pair must enqueue");

        let start = Instant::now();
        client.shutdown_drain(SHUTDOWN_TIMEOUT);
        let elapsed = start.elapsed();

        assert!(
            elapsed < SHUTDOWN_TIMEOUT + Duration::from_secs(1),
            "shutdown drain must be bounded, took {:?}",
            elapsed
        );

        acceptor.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn herdr_shutdown_preempts_in_flight_snapshot_for_terminal_pair() {
        let call_index = Arc::new(AtomicUsize::new(0));
        let snapshot_started = Arc::new(Notify::new());
        let snapshot_cancelled = Arc::new(AtomicBool::new(false));
        let terminal_pair_completed = Arc::new(Notify::new());
        let stale_snapshot_started = Arc::new(Notify::new());
        let completed_payloads = Arc::new(Mutex::new(Vec::new()));

        let client = Arc::new(HerdrClient::new_with_writer("test-pane".into(), {
            let call_index = Arc::clone(&call_index);
            let snapshot_started = Arc::clone(&snapshot_started);
            let snapshot_cancelled = Arc::clone(&snapshot_cancelled);
            let terminal_pair_completed = Arc::clone(&terminal_pair_completed);
            let stale_snapshot_started = Arc::clone(&stale_snapshot_started);
            let completed_payloads = Arc::clone(&completed_payloads);
            move |payload| {
                let index = call_index.fetch_add(1, Ordering::SeqCst);
                let snapshot_started = Arc::clone(&snapshot_started);
                let snapshot_cancelled = Arc::clone(&snapshot_cancelled);
                let terminal_pair_completed = Arc::clone(&terminal_pair_completed);
                let stale_snapshot_started = Arc::clone(&stale_snapshot_started);
                let completed_payloads = Arc::clone(&completed_payloads);
                async move {
                    if index == 0 {
                        let _drop_flag = DropFlag(snapshot_cancelled);
                        snapshot_started.notify_one();
                        pending::<()>().await;
                    } else {
                        assert!(
                            snapshot_cancelled.load(Ordering::SeqCst),
                            "snapshot must be dropped before terminal I/O starts"
                        );
                        completed_payloads.lock().push(payload);
                        if index == 2 {
                            terminal_pair_completed.notify_one();
                        } else if index > 2 {
                            stale_snapshot_started.notify_one();
                        }
                    }
                }
            }
        }));

        client.report_state("working");
        tokio::time::timeout(Duration::from_secs(1), snapshot_started.notified())
            .await
            .expect("scripted snapshot write must start");
        client.report_state("blocked");
        assert!(client.report_terminal(), "terminal pair must enqueue");
        tokio::time::timeout(Duration::from_secs(1), terminal_pair_completed.notified())
            .await
            .expect("terminal pair must complete before shutdown is signaled");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                stale_snapshot_started.notified()
            )
            .await
            .is_err(),
            "writer must not start the queued snapshot after terminal delivery begins"
        );
        assert_eq!(
            call_index.load(Ordering::SeqCst),
            3,
            "only the cancelled snapshot and terminal pair may start"
        );

        let drain_client = Arc::clone(&client);
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || {
                drain_client.shutdown_drain(SHUTDOWN_TIMEOUT);
            }),
        )
        .await
        .expect("terminal drain must not wait for the stalled snapshot")
        .expect("drain worker must not panic");

        assert!(
            snapshot_cancelled.load(Ordering::SeqCst),
            "terminal shutdown must cancel the in-flight snapshot"
        );
        let methods: Vec<String> = completed_payloads
            .lock()
            .iter()
            .map(|payload| {
                serde_json::from_str::<serde_json::Value>(payload)
                    .unwrap()
                    .get("method")
                    .and_then(|method| method.as_str())
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            methods,
            ["pane.report_agent", "pane.release_agent"],
            "terminal requests must complete in order after snapshot cancellation"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_guard_composes_parent_isolation_and_terminal_delivery() {
        let _hook_lock = HOOK_TEST_LOCK.lock().await;
        clear_broadcast_hook();

        let dir = tempdir().unwrap();
        let socket_path = Arc::<str>::from(
            dir.path()
                .join("herdr-composed-shutdown.sock")
                .to_str()
                .unwrap(),
        );
        let listener = UnixListener::bind(socket_path.as_ref()).unwrap();
        let received = zeroclaw_spawn::spawn!(async move {
            let mut payloads = Vec::new();
            while payloads.len() < 2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() > 0 {
                    payloads.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
                }
            }
            payloads
        });

        let write_calls = Arc::new(AtomicUsize::new(0));
        let snapshot_started = Arc::new(AtomicBool::new(false));
        let snapshot_connected = Arc::new(Notify::new());
        let snapshot_cancelled = Arc::new(AtomicBool::new(false));
        let client = HerdrClient::new_with_writer("composed-pane".into(), {
            let socket_path = Arc::clone(&socket_path);
            let write_calls = Arc::clone(&write_calls);
            let snapshot_started = Arc::clone(&snapshot_started);
            let snapshot_connected = Arc::clone(&snapshot_connected);
            let snapshot_cancelled = Arc::clone(&snapshot_cancelled);
            move |payload| {
                let socket_path = Arc::clone(&socket_path);
                let write_calls = Arc::clone(&write_calls);
                let snapshot_started = Arc::clone(&snapshot_started);
                let snapshot_connected = Arc::clone(&snapshot_connected);
                let snapshot_cancelled = Arc::clone(&snapshot_cancelled);
                async move {
                    write_calls.fetch_add(1, Ordering::SeqCst);
                    let mut stream = connect_with_timeout(&socket_path).await.unwrap();
                    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
                    let is_working_snapshot = value["method"] == "pane.report_agent"
                        && value["params"]["state"] == "working";
                    if is_working_snapshot && !snapshot_started.swap(true, Ordering::SeqCst) {
                        let _drop_flag = DropFlag(snapshot_cancelled);
                        snapshot_connected.notify_one();
                        let _stream = stream;
                        pending::<()>().await;
                    } else {
                        assert!(
                            snapshot_cancelled.load(Ordering::SeqCst),
                            "snapshot connection must close before terminal I/O starts"
                        );
                        send_on_stream(&mut stream, &payload).await.unwrap();
                    }
                }
            }
        });
        let herdr = Arc::new(HerdrObserver::new(client, Some("parent-turn")));
        let _hook_guard = set_scoped_broadcast_hook(herdr.clone());
        let tee: Arc<dyn Observer> = Arc::from(create_observer(&ObservabilityConfig::default()));
        let flush_guard = FlushGuard::new(tee.clone());

        tee.record_event(&ObserverEvent::LlmRequest {
            model_provider: "test".into(),
            model: "test".into(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: Some("parent-turn".into()),
        });
        tokio::time::timeout(Duration::from_secs(1), snapshot_connected.notified())
            .await
            .expect("working snapshot must establish its UDS connection");

        tee.record_event(&ObserverEvent::AuthorizationRequested {
            tool_name: "shell".into(),
            arguments_summary: "test approval".into(),
            channel: None,
            turn_id: Some("parent-turn".into()),
        });
        tee.record_event(&ObserverEvent::TurnCompleteAttributed {
            turn_id: "child-turn".into(),
        });
        assert_eq!(
            *herdr.state.lock(),
            HerdrState::Blocked,
            "a child completion must not idle the blocked parent"
        );

        let drain_started = Instant::now();
        drop(flush_guard);
        let drain_elapsed = drain_started.elapsed();
        assert!(
            drain_elapsed < SHUTDOWN_TIMEOUT,
            "FlushGuard teardown must complete within the drain budget: {drain_elapsed:?}"
        );
        assert!(
            snapshot_cancelled.load(Ordering::SeqCst),
            "FlushGuard teardown must cancel the stalled snapshot connection"
        );
        assert_eq!(
            write_calls.load(Ordering::SeqCst),
            3,
            "teardown must not write stale snapshots after the terminal pair"
        );
        assert_eq!(*herdr.state.lock(), HerdrState::Released);

        let payloads = tokio::time::timeout(Duration::from_secs(1), received)
            .await
            .expect("terminal UDS payloads must arrive promptly")
            .expect("terminal payload collector must not panic");
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["method"], "pane.report_agent");
        assert_eq!(payloads[0]["params"]["state"], "idle");
        assert_eq!(payloads[1]["method"], "pane.release_agent");
        for payload in &payloads {
            assert_eq!(payload["params"]["pane_id"], "composed-pane");
            assert_eq!(payload["params"]["source"], SOURCE);
            assert_eq!(payload["params"]["agent"], AGENT);
            assert_ne!(payload["params"]["state"], "working");
            assert_ne!(payload["params"]["state"], "blocked");
        }
    }

    /// Nested run isolation test: parent interactive run + child subagent
    /// (interactive=false). Verifies child events don't reach parent's
    /// herdr hook, parent session unchanged, child AgentEnd doesn't
    /// release parent's pane.
    #[tokio::test]
    async fn herdr_nested_run_isolation() {
        use crate::integrations::herdr::tests::make_spy_reporter;
        use crate::observability::{clear_broadcast_hook, set_scoped_broadcast_hook};

        let _hook_lock = HOOK_TEST_LOCK.lock().await;
        clear_broadcast_hook();

        // Parent installs hook with owning turn_id
        let parent_turn_id = "parent-turn-123";
        let (client_parent, calls_parent) = make_spy_reporter();
        let parent_observer = Arc::new(HerdrObserver::new(client_parent, Some(parent_turn_id)));
        let _parent_guard = set_scoped_broadcast_hook(parent_observer.clone());

        // Simulate parent activity
        let parent_start = ObserverEvent::AgentStart {
            model_provider: "test".into(),
            model: "test".into(),
            channel: None,
            agent_alias: None,
            turn_id: Some(parent_turn_id.to_string()),
        };
        let parent_llm = ObserverEvent::LlmRequest {
            model_provider: "test".into(),
            model: "test".into(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: Some(parent_turn_id.to_string()),
        };
        let parent_end = ObserverEvent::AgentEnd {
            model_provider: "test".into(),
            model: "test".into(),
            duration: Duration::from_millis(100),
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: Some(parent_turn_id.to_string()),
        };

        // Parent events should be processed
        parent_observer.record_event(&parent_start);
        parent_observer.record_event(&parent_llm);
        parent_observer.record_event(&parent_end);

        // Child (subagent) events with different turn_id should be filtered out
        let child_turn_id = "child-turn-456";
        let child_start = ObserverEvent::AgentStart {
            model_provider: "test".into(),
            model: "test".into(),
            channel: None,
            agent_alias: None,
            turn_id: Some(child_turn_id.to_string()),
        };
        let child_llm = ObserverEvent::LlmRequest {
            model_provider: "test".into(),
            model: "test".into(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: Some(child_turn_id.to_string()),
        };
        let child_end = ObserverEvent::AgentEnd {
            model_provider: "test".into(),
            model: "test".into(),
            duration: Duration::from_millis(100),
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: Some(child_turn_id.to_string()),
        };

        // Child events should NOT be processed by parent observer
        parent_observer.record_event(&child_start);
        parent_observer.record_event(&child_llm);
        parent_observer.record_event(&child_end);

        // Verify only parent events were captured (6 events: start, llm, end for parent)
        let captured: Vec<_> = calls_parent.lock().drain(..).collect();
        let state_methods: Vec<&str> = captured
            .iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();

        // Parent: LlmRequest→Working, AgentEnd→Idle+Release (initial Idle is implicit)
        assert_eq!(
            state_methods,
            vec!["working", "idle"],
            "child events should be filtered out, got {:?}",
            state_methods
        );

        // Verify no release_agent from child (child's AgentEnd would have emitted it)
        let release_count = captured
            .iter()
            .filter(|c| c.method == "pane.release_agent")
            .count();
        assert_eq!(
            release_count, 1,
            "only parent AgentEnd should emit release_agent"
        );
    }

    /// TurnComplete scoping test: parent in Working/Blocked + child TurnComplete
    /// should NOT transition parent to Idle. Only parent's TurnComplete with matching
    /// turn_id should trigger Idle transition.
    #[tokio::test]
    async fn herdr_turn_complete_scoping() {
        use crate::integrations::herdr::tests::make_spy_reporter;
        use crate::observability::{clear_broadcast_hook, set_scoped_broadcast_hook};

        let _hook_lock = HOOK_TEST_LOCK.lock().await;
        clear_broadcast_hook();

        // Parent installs hook with owning turn_id
        let parent_turn_id = "parent-turn-123";
        let (client_parent, calls_parent) = make_spy_reporter();
        let parent_observer = Arc::new(HerdrObserver::new(client_parent, Some(parent_turn_id)));
        let _parent_guard = set_scoped_broadcast_hook(parent_observer.clone());

        // Put parent in Working state
        let parent_llm = ObserverEvent::LlmRequest {
            model_provider: "test".into(),
            model: "test".into(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: Some(parent_turn_id.to_string()),
        };
        parent_observer.record_event(&parent_llm);

        // Verify parent is Working
        let captured: Vec<_> = calls_parent.lock().drain(..).collect();
        let state_methods: Vec<&str> = captured
            .iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();
        assert_eq!(
            state_methods,
            vec!["working"],
            "parent LlmRequest should transition to Working"
        );

        // Put parent in Blocked state
        let auth_req = ObserverEvent::AuthorizationRequested {
            tool_name: "shell".into(),
            arguments_summary: "ls".into(),
            channel: None,
            turn_id: Some(parent_turn_id.to_string()),
        };
        parent_observer.record_event(&auth_req);

        let captured: Vec<_> = calls_parent.lock().drain(..).collect();
        let state_methods: Vec<&str> = captured
            .iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();
        assert_eq!(
            state_methods,
            vec!["blocked"],
            "parent AuthorizationRequested should transition to Blocked"
        );

        // Emit child TurnComplete with different turn_id — should NOT affect parent
        let child_turn_id = "child-turn-456";
        let child_turn_complete = ObserverEvent::TurnCompleteAttributed {
            turn_id: child_turn_id.to_string(),
        };
        parent_observer.record_event(&child_turn_complete);

        // Parent state should remain Blocked (no new state change)
        let captured: Vec<_> = calls_parent.lock().drain(..).collect();
        assert_eq!(
            captured.len(),
            0,
            "child TurnComplete with different turn_id should not change parent state"
        );

        parent_observer.record_event(&ObserverEvent::LlmRequest {
            model_provider: "test".into(),
            model: "test".into(),
            messages_count: 1,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        parent_observer.record_event(&ObserverEvent::AuthorizationRequested {
            tool_name: "shell".into(),
            arguments_summary: "ls".into(),
            channel: None,
            turn_id: None,
        });
        parent_observer.record_event(&ObserverEvent::AgentEnd {
            model_provider: "test".into(),
            model: "test".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        parent_observer.record_event(&ObserverEvent::TurnComplete);
        assert!(
            calls_parent.lock().is_empty(),
            "unattributed lifecycle events must not change an owned pane"
        );
        assert_eq!(*parent_observer.state.lock(), HerdrState::Blocked);

        // A matching attributed completion should transition the parent to Idle.
        let parent_turn_complete = ObserverEvent::TurnCompleteAttributed {
            turn_id: parent_turn_id.to_string(),
        };
        parent_observer.record_event(&parent_turn_complete);

        let captured: Vec<_> = calls_parent.lock().drain(..).collect();
        let state_methods: Vec<&str> = captured
            .iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();
        assert_eq!(
            state_methods,
            vec!["idle"],
            "parent TurnComplete with matching turn_id should transition to Idle"
        );
    }
}
