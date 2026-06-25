use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use zeroclaw_api::observability_traits::ObserverMetric;

use crate::observability::{
    BroadcastHookGuard, Observer, ObserverEvent, set_scoped_broadcast_hook,
};

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
    let client = HerdrClient::new(socket_path, pane_id);
    // Clear any stale state from a previous crashed session before installing
    // the observer. The timestamp-based seq ensures this call is accepted even
    // if herdr retains a higher seq from a prior session.
    let _ = client.send("pane.release_agent", &serde_json::Map::new());
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

/// Low-level client that sends JSON-RPC requests to the herdr daemon over a
/// Unix domain socket. Connects, writes, reads response (fire-and-forget),
/// and disconnects per message.
struct HerdrClient {
    socket_path: String,
    pane_id: String,
}

impl HerdrClient {
    fn new(socket_path: String, pane_id: String) -> Self {
        Self { socket_path, pane_id }
    }

    fn next_seq(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn request_id(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("{SOURCE}:{now}")
    }

    fn send(&self, method: &str, params: &serde_json::Map<String, serde_json::Value>) -> Result<(), std::io::Error> {
        let mut params_map = serde_json::Map::new();
        params_map.insert("pane_id".into(), serde_json::Value::String(self.pane_id.clone()));
        params_map.insert("source".into(), serde_json::Value::String(SOURCE.into()));
        params_map.insert("agent".into(), serde_json::Value::String(AGENT.into()));
        params_map.insert("seq".into(), serde_json::Value::Number(self.next_seq().into()));
        for (k, v) in params {
            params_map.insert(k.clone(), v.clone());
        }

        let mut map = serde_json::Map::new();
        map.insert("id".into(), serde_json::Value::String(self.request_id()));
        map.insert("method".into(), serde_json::Value::String(method.into()));
        map.insert("params".into(), serde_json::Value::Object(params_map));

        let request = serde_json::Value::Object(map);

        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket_path)?;
        let msg = serde_json::to_string(&request)?;
        stream.write_all(msg.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        // Read and discard response (fire-and-forget, but drain the socket)
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);

        Ok(())
    }

    fn report_state(&self, state: &str, session_id: Option<&str>) {
        let mut params = serde_json::Map::new();
        params.insert("state".into(), serde_json::Value::String(state.into()));
        if let Some(sid) = session_id {
            params.insert("agent_session_id".into(), serde_json::Value::String(sid.into()));
        }
    let _ = self.send("pane.report_agent", &params);
}

fn report_released(&self) {
    let _ = self.send("pane.release_agent", &serde_json::Map::new());
}

}

// ── DebouncedReporter ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HerdrState {
    Idle,
    Working,
    Released,
}

/// Debounces status changes so the herdr socket is only contacted on actual
/// state transitions, not on every ObserverEvent.
struct DebouncedReporter {
    client: HerdrClient,
    state: HerdrState,
}

impl DebouncedReporter {
    fn new(client: HerdrClient) -> Self {
        Self {
            client,
            state: HerdrState::Idle,
        }
    }

    fn report_working(&mut self, session_id: Option<&str>) {
        if self.state == HerdrState::Working {
            return;
        }
        self.state = HerdrState::Working;
        self.client.report_state("working", session_id);
    }

    fn report_idle(&mut self) {
        if self.state == HerdrState::Idle {
            return;
        }
        self.state = HerdrState::Idle;
        self.client.report_state("idle", None);
    }

    fn report_released(&mut self) {
        if matches!(self.state, HerdrState::Released) {
            return;
        }
        self.state = HerdrState::Released;
        self.client.report_state("idle", None);
        self.client.report_released();
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
    fn new(reporter: DebouncedReporter) -> Self {
        Self {
            reporter: Mutex::new(reporter),
        }
    }
}

impl Observer for HerdrObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::AgentStart { .. }
            | ObserverEvent::LlmRequest { .. }
            | ObserverEvent::ToolCallStart { .. } => {
                let sid = current_session_id();
                if let Ok(mut reporter) = self.reporter.lock() {
                    reporter.report_working(sid.as_deref());
                }
            }
            ObserverEvent::TurnComplete => {
                if let Ok(mut reporter) = self.reporter.lock() {
                    reporter.report_idle();
                }
            }
            ObserverEvent::AgentEnd { .. } => {
                if let Ok(mut reporter) = self.reporter.lock() {
                    reporter.report_released();
                }
            }
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn flush(&self) {}

    fn name(&self) -> &str {
        "herdr"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
