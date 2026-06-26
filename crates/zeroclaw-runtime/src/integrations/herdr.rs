use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

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
pub(crate) struct HerdrClient {
    socket_path: String,
    pane_id: String,
    #[cfg(test)]
    spy: Option<Arc<dyn Fn(&str, &serde_json::Map<String, serde_json::Value>) + Send + Sync>>,
}

impl HerdrClient {
    pub(crate) fn new(socket_path: String, pane_id: String) -> Self {
        Self {
            socket_path,
            pane_id,
            #[cfg(test)]
            spy: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_spy<F>(socket_path: String, pane_id: String, spy: F) -> Self
    where
        F: Fn(&str, &serde_json::Map<String, serde_json::Value>) + Send + Sync + 'static,
    {
        Self {
            socket_path,
            pane_id,
            spy: Some(Arc::new(spy)),
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
        let msg = serde_json::to_string(&request)?;

        #[cfg(unix)]
        {
            let mut stream = UnixStream::connect(&self.socket_path)?;
            stream.write_all(msg.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;

            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);

            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = msg;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "UDS requires a Unix platform",
            ))
        }
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

    fn flush(&self) {}

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
}
