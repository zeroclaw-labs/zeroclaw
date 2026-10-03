//! Shared application state for Tauri.

use std::sync::Arc;
use tokio::sync::RwLock;

/// Agent status as reported by the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Error,
}

/// Where startup stands. The splash reads it to decide what to show, and
/// `open_dashboard` refuses to open the dashboard unless it is
/// [`Startup::Ready`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Startup {
    /// Still looking for a gateway, or starting one and checking it.
    Pending { message: String },
    /// The dashboard may open: a gateway that was already running answered,
    /// or the daemon this app launched passed its startup checks.
    Ready,
    /// Startup failed and the dashboard must not open. `kind` names the
    /// failure for the splash.
    Failed { kind: &'static str, message: String },
}

impl Default for Startup {
    fn default() -> Self {
        Self::Pending {
            message: "Starting ZeroClaw…".to_string(),
        }
    }
}

impl Startup {
    /// Whether the dashboard may open now; otherwise the reason it may not.
    pub fn dashboard_gate(&self) -> Result<(), String> {
        match self {
            Self::Ready => Ok(()),
            Self::Pending { .. } => {
                Err("ZeroClaw is still starting; the dashboard opens once it is ready.".to_string())
            }
            Self::Failed { message, .. } => Err(message.clone()),
        }
    }
}

/// Shared application state behind an `Arc<RwLock<_>>`.
#[derive(Debug, Clone)]
pub struct AppState {
    pub gateway_url: String,
    pub token: Option<String>,
    pub connected: bool,
    pub agent_status: AgentStatus,
    pub startup: Startup,
    /// The daemon this app launched, verified over RPC. While it is set,
    /// every credential sent to the dashboard address travels on a
    /// connection that first proved it is this core's own gateway.
    pub core: Option<Arc<crate::possession::CoreLink>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            gateway_url: "http://127.0.0.1:42617".to_string(),
            token: None,
            connected: false,
            agent_status: AgentStatus::Idle,
            startup: Startup::default(),
            core: None,
        }
    }
}

/// Thread-safe wrapper around `AppState`.
pub type SharedState = Arc<RwLock<AppState>>;

/// Create the default shared state.
pub fn shared_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state() {
        let state = AppState::default();
        assert_eq!(state.gateway_url, "http://127.0.0.1:42617");
        assert!(state.token.is_none());
        assert!(!state.connected);
        assert_eq!(state.agent_status, AgentStatus::Idle);
        assert!(matches!(state.startup, Startup::Pending { .. }));
    }

    #[test]
    fn only_a_ready_startup_lets_the_dashboard_open() {
        assert_eq!(Startup::Ready.dashboard_gate(), Ok(()));
        assert!(Startup::default().dashboard_gate().is_err());
        let failed = Startup::Failed {
            kind: "incompatible",
            message: "bundled core version mismatch".to_string(),
        };
        assert_eq!(
            failed.dashboard_gate(),
            Err("bundled core version mismatch".to_string())
        );
    }

    #[test]
    fn startup_serializes_for_the_splash() {
        assert_eq!(
            serde_json::to_value(Startup::Ready).unwrap(),
            serde_json::json!({ "state": "ready" })
        );
        assert_eq!(
            serde_json::to_value(Startup::Failed {
                kind: "port_held",
                message: "m".to_string()
            })
            .unwrap(),
            serde_json::json!({ "state": "failed", "kind": "port_held", "message": "m" })
        );
        assert_eq!(
            serde_json::to_value(Startup::default()).unwrap()["state"],
            "pending"
        );
    }

    #[test]
    fn shared_state_is_cloneable() {
        let s1 = shared_state();
        let s2 = s1.clone();
        // Both references point to the same allocation.
        assert!(Arc::ptr_eq(&s1, &s2));
    }

    #[tokio::test]
    async fn shared_state_concurrent_read_write() {
        let state = shared_state();

        // Write from one handle.
        {
            let mut s = state.write().await;
            s.connected = true;
            s.agent_status = AgentStatus::Working;
            s.token = Some("zc_test".to_string());
        }

        // Read from cloned handle.
        let state2 = state.clone();
        let s = state2.read().await;
        assert!(s.connected);
        assert_eq!(s.agent_status, AgentStatus::Working);
        assert_eq!(s.token.as_deref(), Some("zc_test"));
    }

    #[test]
    fn agent_status_serialization() {
        assert_eq!(
            serde_json::to_string(&AgentStatus::Idle).unwrap(),
            "\"idle\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Error).unwrap(),
            "\"error\""
        );
    }
}
