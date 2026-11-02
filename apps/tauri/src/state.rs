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

/// Status the splash listens for (`zeroclaw://splash-status`). Drives the
/// splash copy when the app starts its own daemon or hits a problem; the happy
/// path is covered by the splash's own health polling.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SplashStatus {
    /// `starting` | `error` | `missing`.
    pub kind: &'static str,
    pub message: String,
}

/// Shared application state behind an `Arc<RwLock<_>>`.
#[derive(Debug, Clone)]
pub struct AppState {
    pub gateway_url: String,
    pub token: Option<String>,
    pub connected: bool,
    pub agent_status: AgentStatus,
    /// The failure reported while finding or starting a daemon. It is kept
    /// here because a splash opened after the status event never saw it.
    pub startup_failure: Option<SplashStatus>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            gateway_url: "http://127.0.0.1:42617".to_string(),
            token: None,
            connected: false,
            agent_status: AgentStatus::Idle,
            startup_failure: None,
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
        assert!(state.startup_failure.is_none());
    }

    #[test]
    fn splash_status_serializes_the_fields_the_splash_reads() {
        let status = SplashStatus {
            kind: "missing",
            message: "Couldn't find the `zeroclaw` binary.".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "kind": "missing",
                "message": "Couldn't find the `zeroclaw` binary.",
            })
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
