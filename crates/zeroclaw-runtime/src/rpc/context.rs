//! Shared context threaded from `daemon::run()` through the Unix socket
//! listener into each per-connection [`super::dispatch::RpcDispatcher`].
//!
//! Every subsystem handle the RPC layer might need lives here. Fields
//! beyond `config` and `sessions` are `Option` so the context works in
//! tests and minimal (kernel-only) daemon configurations.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::oneshot;

use zeroclaw_api::channel::ChannelApprovalResponse;
use zeroclaw_config::cost::tracker::CostTracker;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;

use super::session::SessionStore;
use super::tui_identity::TuiRegistry;

/// Registry for in-flight tool approval requests.
///
/// The RpcApprovalChannel inserts a (request_id, oneshot::Sender) pair
/// before sending the approval_request notification.
/// handle_session_approve resolves it when the client sends session/approve.
#[derive(Default)]
pub struct ApprovalPendingMap {
    inner: std::sync::Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>,
}

impl ApprovalPendingMap {
    pub fn insert(&self, request_id: String, tx: oneshot::Sender<ChannelApprovalResponse>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
    }

    pub fn resolve(&self, request_id: &str, response: ChannelApprovalResponse) {
        let tx = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id);
        if let Some(tx) = tx {
            let _ = tx.send(response);
        }
    }
}

/// Daemon-wide state shared across all RPC connections.
pub struct RpcContext {
    /// Live config behind a read-write lock so `config/set` can mutate
    /// without a full daemon reload. Mirrors the gateway's
    /// `Arc<RwLock<Config>>` pattern.
    pub config: Arc<RwLock<Config>>,

    /// In-memory session store for active RPC sessions.
    pub sessions: Arc<SessionStore>,

    /// Persistent session backend (SQLite / JSONL) for history and
    /// session metadata. `None` when persistence is disabled.
    pub session_backend: Option<Arc<dyn SessionBackend>>,

    /// Memory subsystem (`dyn Memory` from `zeroclaw-api`).
    pub memory: Option<Arc<dyn zeroclaw_api::memory_traits::Memory>>,

    /// Cost tracking. `None` when cost tracking is disabled.
    pub cost_tracker: Option<Arc<CostTracker>>,

    /// Daemon-wide event broadcast. RPC handlers subscribe to forward
    /// events as JSON-RPC notifications (`logs/subscribe`).
    pub event_tx: Option<tokio::sync::broadcast::Sender<Value>>,

    /// Write `true` to trigger a daemon-level config reload. Mirrors
    /// the gateway's `/admin/reload` mechanism.
    pub reload_tx: Option<tokio::sync::watch::Sender<bool>>,

    /// In-flight approval requests waiting for session/approve RPC calls.
    pub approval_pending: Arc<ApprovalPendingMap>,

    /// Live TUI client registry. Tracks connected TUI sessions by UID.
    /// **Source of truth** for "which TUIs are connected right now."
    pub tui_registry: Arc<TuiRegistry>,

    /// ACP session persistence. Opened (and the DB file created) at
    /// daemon boot under `<data_dir>/sessions/acp-sessions.db`. `None`
    /// when the store could not be opened (read-only FS, bad perms) —
    /// callers must treat persistence as best-effort.
    pub acp_session_store: Option<Arc<AcpSessionStore>>,
}

impl RpcContext {
    /// Minimal context for tests — only config and sessions, everything
    /// else `None`.
    #[cfg(test)]
    pub fn minimal(config: Config, sessions: Arc<SessionStore>) -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(RwLock::new(config)),
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
        })
    }

    #[cfg(test)]
    pub fn for_persistence_tests(
        config: Config,
        sessions: Arc<SessionStore>,
        session_backend: Option<Arc<dyn SessionBackend>>,
        acp_session_store: Option<Arc<AcpSessionStore>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(RwLock::new(config)),
            sessions,
            session_backend,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use zeroclaw_api::channel::ChannelApprovalResponse;

    #[test]
    fn pending_map_insert_and_resolve() {
        let map = ApprovalPendingMap::default();
        let (tx, mut rx) = oneshot::channel::<ChannelApprovalResponse>();
        map.insert("req-1".to_string(), tx);
        map.resolve("req-1", ChannelApprovalResponse::Approve);
        assert_eq!(rx.try_recv().unwrap(), ChannelApprovalResponse::Approve);
    }

    #[test]
    fn pending_map_resolve_unknown_key_is_noop() {
        let map = ApprovalPendingMap::default();
        map.resolve("nonexistent", ChannelApprovalResponse::Deny);
    }

    #[test]
    fn pending_map_insert_then_drop_is_safe() {
        let map = ApprovalPendingMap::default();
        let (tx, _rx) = oneshot::channel::<ChannelApprovalResponse>();
        map.insert("req-2".to_string(), tx);
        // _rx is dropped — resolve sends to a closed channel; must not panic
        map.resolve("req-2", ChannelApprovalResponse::Approve);
    }
}
