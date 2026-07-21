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

/// Maximum number of pending messages in the writer queue. Bounded to prevent
/// unbounded accumulation when the Herdr daemon is slow or unavailable.
/// Capacity 64: 64 * 700ms (max connect+write) = ~45s theoretical max drain,
/// but shutdown timeout caps actual wait at 2s.
const WRITER_QUEUE_CAPACITY: usize = 64;

/// Drop guard that signals the drain-done channel during a panic unwind, so
/// the sync `shutdown_drain()` never waits the full 2s timeout for a task
/// that already crashed.
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

/// Client that sends JSON-RPC notifications to the herdr daemon via tokio UDS.
/// The `send()` method serializes and fires off an async write — it never
/// blocks the caller. Call `shutdown_drain()` to wait until pending writes complete
/// (used at startup and shutdown for guaranteed delivery).
pub(crate) struct HerdrClient {
    pane_id: String,
    #[cfg(test)]
    spy: Option<SpyFn>,
    #[cfg(unix)]
    writer: Mutex<Option<mpsc::Sender<String>>>,
    #[cfg(unix)]
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Sync channel signaled by the writer task when it has finished draining.
    /// Allows the sync `flush()` path to wait without `block_in_place`.
    #[cfg(unix)]
    drain_done_rx: Mutex<Option<std_mpsc::Receiver<()>>>,
}

impl HerdrClient {
    pub(crate) fn new(socket_path: String, pane_id: String) -> Self {
        #[cfg(unix)]
        {
            let (tx, mut rx) = mpsc::channel::<String>(WRITER_QUEUE_CAPACITY);
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
            // Sync channel for the writer task to signal drain completion to
            // the sync `flush()` path. Buffered(1) so the writer task can send
            // without blocking even if the receiver isn't waiting yet.
            let (drain_done_tx, drain_done_rx) = std_mpsc::sync_channel::<()>(1);
            // Clone so the panic guard can signal drain completion even if
            // the writer task panics mid-write.
            let drain_tx_for_panic = drain_done_tx.clone();
            let socket_path = socket_path.clone();
            let _writer_handle = zeroclaw_spawn::spawn!(async move {
                let _drain_guard = DrainOnPanic(Some(drain_tx_for_panic));
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut shutdown_rx => {
                            // Shutdown signal received, drain remaining messages
                            while let Some(payload) = rx.recv().await {
                                let mut stream = match connect_with_timeout(&socket_path).await {
                                    Ok(s) => s,
                                    Err(_) => continue,
                                };
                                let _ = send_on_stream(&mut stream, &payload).await;
                            }
                            // Signal drain completion to the sync flush path.
                            let _ = drain_done_tx.send(());
                            break;
                        }
                        maybe_payload = rx.recv() => {
                            match maybe_payload {
                                Some(payload) => {
                                    let mut stream = match connect_with_timeout(&socket_path).await {
                                        Ok(s) => s,
                                        Err(_) => continue,
                                    };
                                    let _ = send_on_stream(&mut stream, &payload).await;
                                }
                                None => {
                                    // Channel closed without shutdown signal;
                                    // signal drain done and exit.
                                    let _ = drain_done_tx.send(());
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            Self {
                pane_id,
                #[cfg(test)]
                spy: None,
                writer: Mutex::new(Some(tx)),
                shutdown_tx: Mutex::new(Some(shutdown_tx)),
                drain_done_rx: Mutex::new(Some(drain_done_rx)),
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
            writer: Mutex::new(None),
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

            // Signal the writer task to enter drain mode
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

        let payload = serde_json::json!({
            "id": self.request_id(),
            "method": method,
            "params": params_map,
        });

        let payload_str = serde_json::to_string(&payload)?;

        // Fire-and-forget: push to writer task via bounded channel. Use
        // `try_send` so the caller never blocks on a slow/unavailable peer.
        // On queue full, drop the new message — `transit_to` already suppresses
        // redundant state transitions, so this is a rare
        // backpressure case that loses a stale lifecycle snapshot.
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

    fn report_state(&self, state: &str) {
        let mut params = serde_json::Map::new();
        params.insert("state".into(), serde_json::Value::String(state.into()));
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
}

impl HerdrObserver {
    fn transit_to(&self, state: &mut HerdrState, target: HerdrState) {
        if *state == target {
            return;
        }
        *state = target;
        match target {
            HerdrState::Released => {
                self.client.report_state("idle");
                self.client.report_released();
            }
            HerdrState::Working => self.client.report_state("working"),
            HerdrState::Idle => self.client.report_state("idle"),
            HerdrState::Blocked => self.client.report_state("blocked"),
        }
    }
}

impl Observer for HerdrObserver {
    fn record_event(&self, event: &ObserverEvent) {
        // Filter by owning turn_id: only events from the owning interactive
        // run are forwarded. This prevents child agents (subagents) from
        // mutating the parent's herdr pane state.
        if let Some(owning) = self.owning_turn_id.as_deref() {
            let event_turn: Option<&str> = match event {
                ObserverEvent::AgentStart { turn_id, .. }
                | ObserverEvent::LlmRequest { turn_id, .. }
                | ObserverEvent::LlmResponse { turn_id, .. }
                | ObserverEvent::AgentEnd { turn_id, .. }
                | ObserverEvent::ToolCallStart { turn_id, .. }
                | ObserverEvent::ToolCall { turn_id, .. }
                | ObserverEvent::HistoryTrimmed { turn_id, .. }
                | ObserverEvent::AuthorizationRequested { turn_id, .. }
                | ObserverEvent::AuthorizationResponded { turn_id, .. } => turn_id.as_deref(),
                _ => None,
            };
            if event_turn.is_some_and(|t| t != owning) {
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
            if *state != HerdrState::Released {
                *state = HerdrState::Released;
                self.client.report_state("idle");
                self.client.report_released();
            }
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

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

    /// Build a `HerdrClient` with the spy instead of connecting to a real UDS socket.
    pub(crate) fn make_spy_reporter(spy: HerdrSpy) -> (HerdrClient, Arc<Mutex<Vec<HerdrSpyCall>>>) {
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
        let spy = HerdrSpy::new();
        let (client, calls) = make_spy_reporter(spy);
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

        // Capture `now_micros` BEFORE generating seq. The seq counter is
        // seeded from wall clock on first use, so seq1 should be >= the
        // clock value captured here (within the resolution of SystemTime).
        let now_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);

        let seq1 = client.next_seq();
        let seq2 = client.next_seq();
        let seq3 = client.next_seq();

        assert!(seq2 > seq1, "seq must be monotonic: {} <= {}", seq2, seq1);
        assert!(seq3 > seq2, "seq must be monotonic: {} <= {}", seq3, seq2);

        // seq1 was captured AFTER now_micros, so seq1 >= now_micros. We allow
        // 10ms slack to absorb scheduler latency and SystemTime granularity.
        assert!(
            seq1 >= now_micros.saturating_sub(10_000),
            "seq {} should be seeded from wall clock (now ~{})",
            seq1,
            now_micros
        );
    }

    /// Shutdown drain test: verify ordered receipt of `idle` then
    /// `pane.release_agent` before shutdown completes. Uses a real
    /// `UnixListener` to receive messages and confirm ordering.
    #[tokio::test]
    async fn herdr_shutdown_drain_ordered_receipt() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("herdr-test.sock");
        let sock_str = sock_path.to_str().unwrap().to_string();

        // Bind a listener before starting the client
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Create client and send messages
        let client = HerdrClient::new(sock_str.clone(), "test-pane".into());
        client.report_state("idle");
        client.report_released();

        // Flush (drains the writer task)
        client.shutdown_drain(SHUTDOWN_TIMEOUT);

        // Now accept and read messages from the listener
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && received.len() < 2 {
            match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.is_ok() {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                            if let Some(method) = val.get("method").and_then(|m| m.as_str()) {
                                received.push(method.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }

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

    /// Shutdown drain bounded wait test: slow/unavailable peer must not
    /// block shutdown longer than the timeout. Simulates a slow listener
    /// that accepts but never reads, verifying the 2s timeout is honored.
    #[tokio::test]
    async fn herdr_shutdown_drain_bounded_wait() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("herdr-test-slow.sock");
        let sock_str = sock_path.to_str().unwrap().to_string();

        // Bind a listener that accepts but never reads (slow peer)
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Create client and send a message
        let client = HerdrClient::new(sock_str.clone(), "test-pane".into());
        client.report_state("idle");
        client.report_released();

        // Flush should complete within timeout even with slow peer
        let start = Instant::now();
        client.shutdown_drain(SHUTDOWN_TIMEOUT);
        let elapsed = start.elapsed();

        // Must complete within SHUTDOWN_TIMEOUT (2s) + some slack
        assert!(
            elapsed < Duration::from_secs(3),
            "shutdown drain must be bounded, took {:?}",
            elapsed
        );

        // Clean up: drop listener to unblock any pending accept
        drop(listener);
    }

    /// Nested run isolation test: parent interactive run + child subagent
    /// (interactive=false). Verifies child events don't reach parent's
    /// herdr hook, parent session unchanged, child AgentEnd doesn't
    /// release parent's pane.
    #[tokio::test]
    async fn herdr_nested_run_isolation() {
        use crate::integrations::herdr::tests::{HerdrSpy, make_spy_reporter};
        use crate::observability::{clear_broadcast_hook, set_scoped_broadcast_hook};

        clear_broadcast_hook();

        // Parent installs hook with owning turn_id
        let parent_turn_id = "parent-turn-123";
        let spy_parent = HerdrSpy::new();
        let (client_parent, calls_parent) = make_spy_reporter(spy_parent);
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
}
