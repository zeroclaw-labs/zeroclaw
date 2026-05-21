//! Shared context threaded from `daemon::run()` through the Unix socket
//! listener into each per-connection [`super::dispatch::RpcDispatcher`].
//!
//! Every subsystem handle the RPC layer might need lives here. Fields
//! beyond `config` and `sessions` are `Option` so the context works in
//! tests and minimal (kernel-only) daemon configurations.

use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;

use zeroclaw_config::cost::tracker::CostTracker;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::SessionBackend;

use super::session::SessionStore;

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
        })
    }
}
