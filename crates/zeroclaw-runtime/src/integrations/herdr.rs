//! Herdr integration — agent lifecycle reporting to the Herdr sidebar.
//!
//! This integration is purely environment-variable driven. There is no `[herdr]`
//! config section. Enable it by setting these env vars:
//!
//! - `HERDR_ENV=1` — must be set to activate the integration
//! - `HERDR_SOCKET_PATH` — path to the Herdr daemon's Unix socket
//! - `HERDR_PANE_ID` — the Herdr pane identifier
//!
//! A dedicated background I/O thread owns the UDS connection. The observer
//! never touches a socket — it pushes state transitions to a channel (sub-µs)
//! and the thread processes them in order with bounded timeouts. Startup and
//! shutdown messages are guaranteed by flushing the channel synchronously.

use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use parking_lot::RwLock;

use zeroclaw_api::observability_traits::ObserverMetric;

use crate::observability::{
    BroadcastHookGuard, Observer, ObserverEvent, set_scoped_broadcast_hook,
};

// ── I/O timeouts ──────────────────────────────────────────────────────────────

/// Maximum time to wait for a UDS connect before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
/// Maximum time to wait for a UDS write or read before giving up.
const IO_TIMEOUT: Duration = Duration::from_millis(500);

/// Connect to a Unix domain socket with a timeout by delegating to a
/// background thread. `std::os::unix::net::UnixStream::connect` has no
/// built-in timeout, so we use a channel + `recv_timeout` to bound it.
#[cfg(unix)]
fn connect_with_timeout(path: &str, timeout: Duration) -> Result<UnixStream, std::io::Error> {
    let path = path.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(&path));
    });
    rx.recv_timeout(timeout).unwrap_or(Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "herdr connect timed out",
    )))
}

// ── Background I/O thread ─────────────────────────────────────────────────────

/// Commands sent from the observer to the dedicated I/O thread.
#[cfg(unix)]
enum IoCommand {
    /// A pre-serialized JSON-RPC notification to write to the herdr daemon.
    Message(String),
    /// Flush the channel and acknowledge that all prior messages were sent on the wire.
    Flush(mpsc::Sender<()>),
    /// Terminate the I/O thread.
    Shutdown,
}

/// Dedicated I/O thread: owns the UDS connection, processes messages in FIFO
/// order, reconnects on error, and never outlives the `HerdrClient`.
#[cfg(unix)]
fn io_thread_main(socket_path: &str, rx: mpsc::Receiver<IoCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            IoCommand::Message(payload) => {
                if let Ok(mut stream) = connect_with_timeout(socket_path, CONNECT_TIMEOUT) {
                    let _ = send_on_stream(&mut stream, &payload);
                }
            }
            IoCommand::Flush(ack) => {
                let _ = ack.send(());
            }
            IoCommand::Shutdown => break,
        }
    }
}

/// Write a JSON-RPC notification to a connected UDS stream with bounded
/// timeouts. Returns `Err` if any operation times out or the peer disconnects.
#[cfg(unix)]
fn send_on_stream(s: &mut UnixStream, payload: &str) -> Result<(), std::io::Error> {
    s.set_write_timeout(Some(IO_TIMEOUT))?;
    s.write_all(payload.as_bytes())?;
    s.write_all(b"\n")?;
    s.flush()?;
    Ok(())
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
pub fn try_install_hook(interactive: bool, agent_alias: &str) -> Option<BroadcastHookGuard> {
    if !interactive {
        return None;
    }
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return None;
    }
    let socket_path = std::env::var("HERDR_SOCKET_PATH").ok()?;
    let pane_id = std::env::var("HERDR_PANE_ID").ok()?;
    install_hook_from_env(socket_path, pane_id, agent_alias)
}

/// Install the hook from already-resolved env values. Factored out of
/// [`try_install_hook`] so the gating logic can be tested without touching
/// the process environment (`std::env::set_var` is `unsafe` on Rust >= 1.80
/// because it is not thread-safe with concurrent reads).
fn install_hook_from_env(
    socket_path: String,
    pane_id: String,
    agent_alias: &str,
) -> Option<BroadcastHookGuard> {
    // UDS is Unix-only; silently skip on other platforms.
    #[cfg(not(unix))]
    {
        let _ = (socket_path, pane_id, agent_alias);
        return None;
    }

    let client = HerdrClient::new(socket_path, pane_id.clone());

    // Compute unique display name: agent alias + last 2 chars of pane_id
    let display_name = if pane_id.len() > 2 {
        format!("{}-{}", agent_alias, &pane_id[pane_id.len() - 2..])
    } else {
        agent_alias.to_string()
    };
    client.report_metadata(&display_name);

    // Clear any stale state from a previous crashed session before installing
    // the observer. The wall-clock-seeded seq ensures this call is accepted even
    // if herdr retains a higher seq from a prior session.
    let _ = client.send("pane.release_agent", &serde_json::Map::new());
    // Report initial idle state so herdr shows the agent immediately, even
    // before any user message triggers a state transition.
    client.report_state("idle", None);
    // Startup messages are best-effort; the first ObserverEvent will re-emit
    // idle if the daemon was unavailable. The I/O thread processes messages
    // independently and Shutdown is handled via the observer's Drop.
    let reporter = DebouncedReporter::new(client);
    let observer = Arc::new(HerdrObserver::new(reporter));
    Some(set_scoped_broadcast_hook(observer))
}

// ── HerdrClient ──────────────────────────────────────────────────────────────

#[cfg(test)]
type SpyFn = Arc<dyn Fn(&str, &serde_json::Map<String, serde_json::Value>) + Send + Sync>;

/// Client that sends JSON-RPC notifications to the herdr daemon via a
/// dedicated background I/O thread. The `send()` method serialises the
/// message and pushes it to an mpsc channel — it never blocks on I/O.
/// Call `flush()` to wait until the I/O thread has processed all pending
/// messages (used at startup and shutdown for guaranteed delivery).
pub(crate) struct HerdrClient {
    pane_id: String,
    #[cfg(test)]
    spy: Option<SpyFn>,
    /// Channel to the background I/O thread. `None` on non-Unix or when the
    /// thread failed to spawn (best-effort: messages are silently dropped).
    #[cfg(unix)]
    io_tx: Option<mpsc::Sender<IoCommand>>,
}

impl HerdrClient {
    pub(crate) fn new(socket_path: String, pane_id: String) -> Self {
        #[cfg(unix)]
        {
            let (tx, rx) = mpsc::channel::<IoCommand>();
            let spawned = std::thread::Builder::new()
                .name("herdr-io".into())
                .spawn(move || io_thread_main(&socket_path, rx))
                .is_ok();
            Self {
                pane_id,
                #[cfg(test)]
                spy: None,
                io_tx: if spawned { Some(tx) } else { None },
            }
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

    #[cfg(test)]
    pub(crate) fn new_with_spy<F>(_socket_path: String, pane_id: String, spy: F) -> Self
    where
        F: Fn(&str, &serde_json::Map<String, serde_json::Value>) + Send + Sync + 'static,
    {
        Self {
            pane_id,
            spy: Some(Arc::new(spy)),
            #[cfg(unix)]
            io_tx: None,
        }
    }

    /// Flush pending messages: acknowledge that all prior messages were sent.
    /// With one-shot connections, each message is sent independently, so flush
    /// just acknowledges that the I/O thread has processed all queued messages.
    pub(crate) fn flush(&self) {
        #[cfg(unix)]
        if let Some(tx) = &self.io_tx {
            let (ack_tx, ack_rx) = mpsc::channel::<()>();
            if tx.send(IoCommand::Flush(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(CONNECT_TIMEOUT + Duration::from_secs(1));
            }
        }
    }

    fn next_seq(&self) -> u64 {
        static NEXT_SEQ: OnceLock<AtomicU64> = OnceLock::new();
        let counter = NEXT_SEQ.get_or_init(|| {
            let base = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(1_000_000_000_000_000);
            AtomicU64::new(base)
        });
        counter.fetch_add(1, Ordering::Relaxed)
    }

    fn request_id(&self) -> String {
        format!("{SOURCE}:{}", self.next_seq())
    }

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

        let mut map = serde_json::Map::new();
        map.insert("id".into(), serde_json::Value::String(self.request_id()));
        map.insert("method".into(), serde_json::Value::String(method.into()));
        map.insert("params".into(), serde_json::Value::Object(params_map));

        let request = serde_json::Value::Object(map);
        let payload = serde_json::to_string(&request)?;

        // Push to the background I/O thread. This never blocks — the channel
        // is unbounded, so send is O(1) even if the thread is busy.
        #[cfg(unix)]
        if let Some(tx) = &self.io_tx {
            let _ = tx.send(IoCommand::Message(payload));
        }

        Ok(())
    }

    fn report_state(&self, state: &str, session_id: Option<&str>) {
        let mut params = serde_json::Map::new();
        params.insert("state".into(), serde_json::Value::String(state.into()));
        if let Some(sid) = session_id {
            params.insert(
                "agent_session_id".into(),
                serde_json::Value::String(sid.into()),
            );
        }
        let _ = self.send("pane.report_agent", &params);
    }

    fn report_released(&self) {
        let _ = self.send("pane.release_agent", &serde_json::Map::new());
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

impl Drop for HerdrClient {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(tx) = self.io_tx.take() {
            let _ = tx.send(IoCommand::Shutdown);
        }
    }
}

// ── DebouncedReporter ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HerdrState {
    Idle,
    Working,
    Blocked,
    Released,
}

/// Debounces status changes so the herdr socket is only contacted on actual
/// state transitions, not on every ObserverEvent.
pub(crate) struct DebouncedReporter {
    pub(crate) client: HerdrClient,
    state: HerdrState,
}

impl DebouncedReporter {
    pub(crate) fn new(client: HerdrClient) -> Self {
        Self {
            client,
            state: HerdrState::Idle,
        }
    }

    fn transit_to(&mut self, target: HerdrState, session_id: Option<&str>) {
        if self.state == target {
            return;
        }
        self.state = target;
        if target == HerdrState::Released {
            self.client.report_state("idle", None);
            self.client.report_released();
        } else {
            let label = match target {
                HerdrState::Working => "working",
                HerdrState::Idle => "idle",
                HerdrState::Blocked => "blocked",
                HerdrState::Released => unreachable!(),
            };
            self.client.report_state(label, session_id);
        }
    }

    /// Flush the reporter at shutdown: if the agent has not yet reported
    /// Released, send the release messages now, then drain the I/O channel.
    fn flush(&mut self) {
        if self.state != HerdrState::Released {
            self.state = HerdrState::Released;
            self.client.report_state("idle", None);
            self.client.report_released();
        }
        self.client.flush();
    }
}

// ── Session ID slot (process-wide) ──────────────────────────────────────────
//
// This is NOT a duplicate of canonical state. The source of truth for the
// session id is `memory_session_id` resolved in `agent::loop_::run()` from the
// session state file. This slot is a cross-thread resolver: a single writer
// (`update_session_id`, called from the agent loop) populates it, and the
// `HerdrObserver::record_event` path reads it from whichever thread the
// `TeeObserver` broadcast fan-out happens to land on. The previous
// `thread_local!` was unsound for that use case because tokio can migrate the
// observer's task to a different worker thread, where the slot would read
// `None`. The pattern mirrors `BROADCAST_HOOK` in `observability/mod.rs`.

static SESSION_ID_SLOT: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn session_id_slot() -> &'static RwLock<Option<String>> {
    SESSION_ID_SLOT.get_or_init(|| RwLock::new(None))
}

pub fn update_session_id(sid: &str) {
    session_id_slot().write().replace(sid.to_owned());
}

/// Set the herdr session ID. If `memory_session_id` is present, use it directly.
/// Otherwise, fall back to `pane:{agent_alias}` for herdr session mapping.
pub fn set_session_id(agent_alias: &str, memory_session_id: Option<&str>) {
    let sid = memory_session_id.map(|s| s.to_owned()).or_else(|| {
        Some(zeroclaw_api::session_keys::sanitize_session_key(&format!(
            "pane:{agent_alias}"
        )))
    });
    if let Some(sid) = sid {
        update_session_id(&sid);
    }
}

fn current_session_id() -> Option<String> {
    session_id_slot().read().clone()
}

// ── HerdrObserver ────────────────────────────────────────────────────────────

/// Observer that reports agent lifecycle to the herdr daemon.
///
/// State machine: `Idle` → activity event → `Working` → `AgentEnd` → `Idle`.
pub struct HerdrObserver {
    reporter: Mutex<DebouncedReporter>,
}

impl HerdrObserver {
    pub(crate) fn new(reporter: DebouncedReporter) -> Self {
        Self {
            reporter: Mutex::new(reporter),
        }
    }

    /// Test-only constructor that accepts a pre-built reporter.
    #[cfg(test)]
    pub(crate) fn new_with_reporter(reporter: DebouncedReporter) -> Self {
        Self::new(reporter)
    }
}

impl Observer for HerdrObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let sid = current_session_id();
        let mut reporter = self.reporter.lock().expect("herdr observer poisoned");
        match event {
            ObserverEvent::AgentStart { .. } => {
                reporter.transit_to(HerdrState::Idle, sid.as_deref());
            }
            ObserverEvent::LlmRequest { .. } | ObserverEvent::ToolCallStart { .. } => {
                reporter.transit_to(HerdrState::Working, sid.as_deref());
            }
            ObserverEvent::LlmResponse { .. } => {
                reporter.transit_to(HerdrState::Working, sid.as_deref());
            }
            ObserverEvent::ToolCall { .. } => {
                reporter.transit_to(HerdrState::Working, sid.as_deref());
            }
            ObserverEvent::TurnComplete => {
                reporter.transit_to(HerdrState::Idle, sid.as_deref());
            }
            ObserverEvent::AgentEnd { .. } => {
                reporter.transit_to(HerdrState::Released, sid.as_deref());
            }
            ObserverEvent::AuthorizationRequested { .. } => {
                reporter.transit_to(HerdrState::Blocked, sid.as_deref());
            }
            ObserverEvent::AuthorizationResponded { granted, .. } => {
                if *granted {
                    reporter.transit_to(HerdrState::Working, sid.as_deref());
                } else {
                    reporter.transit_to(HerdrState::Idle, sid.as_deref());
                }
            }
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn flush(&self) {
        let mut reporter = self.reporter.lock().expect("herdr observer poisoned");
        reporter.flush();
    }

    fn name(&self) -> &str {
        "herdr"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use parking_lot::Mutex;

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
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn into_inner(self) -> Arc<Mutex<Vec<HerdrSpyCall>>> {
            self.calls
        }
    }

    /// Build a `DebouncedReporter` with a `HerdrClient` that uses the spy
    /// instead of connecting to a real UDS socket.
    pub(crate) fn make_spy_reporter(
        spy: HerdrSpy,
    ) -> (DebouncedReporter, Arc<Mutex<Vec<HerdrSpyCall>>>) {
        let calls = spy.into_inner();
        let calls_clone = calls.clone();
        let client = HerdrClient::new_with_spy(
            "/tmp/test-herdr.sock".into(),
            "test-pane".into(),
            move |method, params| {
                calls_clone.lock().push(HerdrSpyCall {
                    method: method.to_string(),
                    params: serde_json::Value::Object(params.clone()),
                });
            },
        );
        let reporter = DebouncedReporter::new(client);
        (reporter, calls)
    }

    #[test]
    fn send_fire_and_forget_returns_immediately() {
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
    /// We allow some slack for thread spawn overhead.
    #[test]
    fn startup_with_stale_socket_returns_quickly() {
        let start = std::time::Instant::now();
        let _guard = install_hook_from_env(
            "/tmp/nonexistent-herdr-test-socket.sock".into(),
            "test-pane".into(),
            "test-agent",
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
            try_install_hook(false, "test-agent").is_none(),
            "try_install_hook(false) must return None without consulting env vars"
        );
    }

    /// `DebouncedReporter::flush()` must emit the idle + release_agent
    /// notifications exactly once and transition to `Released`, matching the
    /// AgentEnd / run-teardown drain contract. This is the coverage the
    /// reviewer requested: the teardown drains the final idle +
    /// pane.release_agent notifications instead of merely enqueueing them.
    #[test]
    fn debounced_reporter_flush_drains_release_messages() {
        let spy = HerdrSpy::new();
        let (mut reporter, calls) = make_spy_reporter(spy);

        // Simulate the agent reaching Working state first so flush has
        // something to release from.
        reporter.transit_to(HerdrState::Working, Some("test-session"));
        calls.lock().clear();

        reporter.flush();

        let captured: Vec<HerdrSpyCall> = calls.lock().clone();
        let methods: Vec<&str> = captured.iter().map(|c| c.method.as_str()).collect();

        // The flush must emit exactly two messages: an idle state report
        // followed by a release_agent notification.
        assert!(
            reporter.state == HerdrState::Released,
            "flush must transition state to Released, got {:?}",
            reporter.state,
        );
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

        // Double-flush is a no-op — the reporter is already Released.
        let count_after_first = calls.lock().len();
        reporter.flush();
        assert_eq!(
            calls.lock().len(),
            count_after_first,
            "second flush must not emit duplicate release messages"
        );
    }

    /// `current_session_id()` must return the value set by `update_session_id()`
    /// regardless of which thread reads it. The previous `thread_local!` slot
    /// was unsound because tokio can migrate the observer's task to a different
    /// worker thread.
    #[test]
    fn session_id_is_visible_across_threads() {
        // Guard: clear any session_id set by a previous test.
        session_id_slot().write().take();

        // Write the session ID on the current thread.
        update_session_id("cross-thread-session-1");

        // Spawn a distinct OS thread and read the session ID.
        let t = std::thread::spawn(|| {
            let sid = current_session_id();
            assert_eq!(
                sid.as_deref(),
                Some("cross-thread-session-1"),
                "session_id written on one thread must be visible on another thread"
            );
            sid
        });
        let result = t.join().expect("cross-thread session_id thread panicked");
        assert_eq!(result.as_deref(), Some("cross-thread-session-1"));

        // Update the session ID and verify the new value is visible on a fresh thread.
        update_session_id("cross-thread-session-2");
        let t2 = std::thread::spawn(current_session_id);
        let result2 = t2
            .join()
            .expect("cross-thread session_id thread 2 panicked");
        assert_eq!(result2.as_deref(), Some("cross-thread-session-2"));

        // Clean up so other tests don't observe stale state.
        session_id_slot().write().take();
    }

    /// `next_seq()` must return monotonically increasing values starting from
    /// a wall-clock-seeded base. This ensures restart resilience: a process
    /// restarted after herdr stores a prior seq will have a higher starting
    /// value, avoiding silent message rejection.
    #[test]
    fn next_seq_is_monotonic_and_restart_safe() {
        let client = HerdrClient::new(
            "/tmp/nonexistent-herdr-test-socket.sock".into(),
            "test-pane".into(),
        );

        let seq1 = client.next_seq();
        let seq2 = client.next_seq();
        let seq3 = client.next_seq();

        assert!(seq2 > seq1, "seq must be monotonic: {} <= {}", seq2, seq1);
        assert!(seq3 > seq2, "seq must be monotonic: {} <= {}", seq3, seq2);

        let now_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        assert!(
            seq1 >= now_micros.saturating_sub(1000),
            "seq {} should be seeded from wall clock (now ~{})",
            seq1,
            now_micros
        );
    }
}
