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

use std::cell::RefCell;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use zeroclaw_api::observability_traits::ObserverMetric;

use crate::observability::{
    BroadcastHookGuard, Observer, ObserverEvent, set_scoped_broadcast_hook,
};

// ── I/O timeouts ──────────────────────────────────────────────────────────────

/// Maximum time to wait for a UDS connect before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
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
    /// Flush the channel and acknowledge that all prior messages were sent.
    Flush(mpsc::Sender<()>),
    /// Terminate the I/O thread.
    Shutdown,
}

/// Dedicated I/O thread: owns the UDS connection, processes messages in FIFO
/// order, reconnects on error, and never outlives the `HerdrClient`.
#[cfg(unix)]
fn io_thread_main(socket_path: &str, rx: mpsc::Receiver<IoCommand>) {
    let mut stream: Option<UnixStream> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            IoCommand::Message(payload) => {
                if stream.is_none() {
                    stream = connect_with_timeout(socket_path, CONNECT_TIMEOUT).ok();
                }
                if let Some(ref mut s) = stream
                    && send_on_stream(s, &payload).is_err()
                {
                    stream = None;
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
/// active (not running inside a herdr pane).
pub fn try_install_hook() -> Option<BroadcastHookGuard> {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return None;
    }
    let socket_path = std::env::var("HERDR_SOCKET_PATH").ok()?;
    let pane_id = std::env::var("HERDR_PANE_ID").ok()?;

    // UDS is Unix-only; silently skip on other platforms.
    #[cfg(not(unix))]
    {
        let _ = (socket_path, pane_id);
        return None;
    }

    let client = HerdrClient::new(socket_path, pane_id);
    // Clear any stale state from a previous crashed session before installing
    // the observer. The timestamp-based seq ensures this call is accepted even
    // if herdr retains a higher seq from a prior session.
    let _ = client.send("pane.release_agent", &serde_json::Map::new());
    // Report initial idle state so herdr shows the agent immediately, even
    // before any user message triggers a state transition.
    client.report_state("idle", None);
    // Flush so the I/O thread has sent both messages before the agent loop
    // starts processing user messages.
    client.flush();
    let reporter = DebouncedReporter::new(client);
    let observer = Arc::new(HerdrObserver::new(reporter));
    Some(set_scoped_broadcast_hook(observer))
}

/// Update the session ID stored by the HerdrObserver (called from loop_.rs
/// after `memory_session_id` becomes available).
pub fn update_session_id(sid: &str) {
    SESSION_ID_SLOT.with(|slot| {
        *slot.borrow_mut() = Some(sid.to_owned());
    });
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

    /// Flush pending messages: block until the I/O thread has sent all
    /// messages that were enqueued before this call.
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
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
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

// ── Session ID slot (thread_local) ───────────────────────────────────────────

thread_local! {
    static SESSION_ID_SLOT: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn current_session_id() -> Option<String> {
    SESSION_ID_SLOT.with(|slot| slot.borrow().clone())
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
        let mut reporter = self.reporter.lock().expect("herdr observer poisoned");
        match event {
            ObserverEvent::LlmRequest { .. } | ObserverEvent::ToolCallStart { .. } => {
                reporter.transit_to(HerdrState::Working, current_session_id().as_deref());
            }
            ObserverEvent::TurnComplete => {
                reporter.transit_to(HerdrState::Idle, None);
            }
            ObserverEvent::AgentEnd { .. } => {
                reporter.transit_to(HerdrState::Released, None);
            }
            ObserverEvent::AuthorizationRequested { .. } => {
                reporter.transit_to(HerdrState::Blocked, None);
            }
            ObserverEvent::AuthorizationResponded { granted, .. } => {
                if *granted {
                    reporter.transit_to(HerdrState::Working, None);
                } else {
                    reporter.transit_to(HerdrState::Idle, None);
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
}
