//! Shared context threaded from `daemon::run()` through the Unix socket
//! listener into each per-connection [`super::dispatch::RpcDispatcher`].

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::oneshot;

use zeroclaw_api::channel::ChannelApprovalResponse;
use zeroclaw_config::cost::tracker::CostTracker;
use zeroclaw_config::live::LiveConfigHandle;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;

use super::session::SessionStore;
use super::tui_identity::TuiRegistry;
use crate::LiveConfigAuthority;
use crate::daemon::ChannelGenerationControl;
use crate::live_config_authority::{ConfigCommit, ConfigCommitError};

#[derive(Default)]
pub struct ApprovalPendingMap {
    inner: std::sync::Mutex<HashMap<String, PendingApprovalEntry>>,
}

struct PendingApprovalEntry {
    session_id: String,
    tx: oneshot::Sender<ChannelApprovalResponse>,
}

pub struct PendingApproval {
    map: Arc<ApprovalPendingMap>,
    request_id: String,
    active: bool,
}

impl PendingApproval {
    pub fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for PendingApproval {
    fn drop(&mut self) {
        if self.active {
            self.map.remove(&self.request_id);
        }
    }
}

impl ApprovalPendingMap {
    pub fn register(
        self: &Arc<Self>,
        request_id: String,
        session_id: String,
        tx: oneshot::Sender<ChannelApprovalResponse>,
    ) -> PendingApproval {
        self.insert(request_id.clone(), session_id, tx);
        PendingApproval {
            map: Arc::clone(self),
            request_id,
            active: true,
        }
    }

    pub fn insert(
        &self,
        request_id: String,
        session_id: String,
        tx: oneshot::Sender<ChannelApprovalResponse>,
    ) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, PendingApprovalEntry { session_id, tx });
    }

    pub fn resolve(
        &self,
        request_id: &str,
        session_id: &str,
        response: ChannelApprovalResponse,
    ) -> bool {
        let mut pending = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if pending
            .get(request_id)
            .is_none_or(|entry| entry.session_id != session_id)
        {
            return false;
        }
        if let Some(entry) = pending.remove(request_id) {
            let _ = entry.tx.send(response);
            return true;
        }
        false
    }

    pub fn remove(&self, request_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id)
            .is_some()
    }

    #[cfg(test)]
    pub fn contains(&self, request_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(request_id)
    }
}

/// Daemon-wide state shared across all RPC connections.
pub struct RpcContext {
    /// Read-only live config handle: RPC readers observe the published
    /// config and its revision as one pair and cannot bypass publication
    /// with a raw write. Mutating handlers admit through
    /// `RpcContext::begin_config_commit` instead.
    pub config: LiveConfigHandle,

    /// The live-config authority that owns this context's publication
    /// transaction: the daemon-wide writer mutex, the config-work
    /// lifecycle lease, and the published pair. Every RPC config writer
    /// serializes through it before cloning the current config, and the
    /// irreversible save-and-publish phase of each commit runs retained
    /// (see `save_and_publish_config` in `dispatch.rs`), so request
    /// cancellation cannot abandon a dispatched commit.
    pub config_authority: LiveConfigAuthority,

    /// Alias-scoped admission and destructive lifecycle authority paired with
    /// this context's live config identity.
    pub agent_lifecycle: crate::live_config_authority::AgentLifecycleCoordinator,

    /// Current daemon channel generation. Present only for daemon-owned RPC
    /// contexts; standalone/test contexts cannot retire a live channel set.
    pub(crate) channel_generation_control: Option<Arc<ChannelGenerationControl>>,

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

    /// Write `true` to ask the current gateway listener to shut down before
    /// daemon reload rebinds the same address.
    pub gateway_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,

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

    /// Shared SOP engine from the daemon (for RPC/TUI agent sessions).
    /// `None` when standalone — sessions build their own.
    pub sop_engine: Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
    pub sop_audit: Option<Arc<crate::sop::SopAuditLogger>>,

    /// Lifecycle hook runner. `None` when hooks are disabled in config.
    pub hooks: Option<Arc<crate::hooks::HookRunner>>,

    /// The daemon's single certificate audit logger — the ONE writer of the
    /// Merkle-chained audit file, shared by enrollment, in-band renewal and
    /// the issued-cert ledger.
    ///
    /// This field is the source of truth for "which logger owns the audit
    /// file". `AuditLogger` serializes writers with a mutex held inside the
    /// instance, so a per-request logger only appears safe: two instances
    /// recover the same chain tip and both claim it, and `verify_chain` then
    /// rejects a file every individual write was correct against. Certificate
    /// paths must clone this `Arc`, never call `AuditLogger::new`.
    ///
    /// `None` only when the logger could not be constructed (for example
    /// `sign_events = true` with no usable `ZEROCLAW_AUDIT_SIGNING_KEY`).
    /// Certificate paths fail closed on `None` rather than issuing
    /// credentials with no trail.
    pub cert_audit: Option<Arc<crate::security::audit::AuditLogger>>,
}

impl RpcContext {
    /// Admit one serialized config write on this context's authority.
    /// Fails closed once the daemon generation is closing.
    pub(crate) async fn begin_config_commit(&self) -> Result<ConfigCommit, ConfigCommitError> {
        self.config_authority.begin_config_commit().await
    }

    /// Build a minimal context sharing `authority`'s publication domain —
    /// the cross-surface shape the daemon wires and the mixed
    /// HTTP/RPC writer tests exercise.
    #[cfg(any(test, feature = "test-util"))]
    pub fn for_authority(
        authority: &LiveConfigAuthority,
        sessions: Arc<SessionStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    pub fn for_live_test(config: Config, sessions: Arc<SessionStore>) -> Arc<Self> {
        let tui_dir = config
            .config_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| config.data_dir.clone());
        let data_dir = config.data_dir.clone();
        // Mirrors the daemon: one shared certificate audit logger for the
        // whole context, best-effort like the ACP store above.
        let cert_audit = crate::security::audit::AuditLogger::open_shared(
            config.security.audit.clone(),
            data_dir.clone(),
        )
        .ok();
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new(&tui_dir)),
            acp_session_store: AcpSessionStore::new(data_dir.as_path()).ok().map(Arc::new),
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit,
        })
    }

    #[cfg(test)]
    pub fn minimal(config: Config, sessions: Arc<SessionStore>) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    /// Like [`RpcContext::minimal`] but with the shared certificate audit
    /// logger the daemon wires in production. Certificate-path tests must use
    /// this: `minimal` leaves `cert_audit` unset, and those handlers fail
    /// closed without it.
    #[cfg(test)]
    pub fn minimal_with_cert_audit(config: Config, sessions: Arc<SessionStore>) -> Arc<Self> {
        let cert_audit = crate::security::audit::AuditLogger::open_shared(
            config.security.audit.clone(),
            config.data_dir.clone(),
        )
        .ok();
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit,
        })
    }

    #[cfg(test)]
    pub fn minimal_with_event_tx(
        config: Config,
        sessions: Arc<SessionStore>,
        event_tx: tokio::sync::broadcast::Sender<Value>,
    ) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: Some(event_tx),
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    #[cfg(test)]
    pub fn minimal_with_sop_engine(
        config: Config,
        sessions: Arc<SessionStore>,
        sop_engine: Arc<std::sync::Mutex<crate::sop::SopEngine>>,
    ) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: Some(sop_engine),
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    #[cfg(test)]
    pub fn minimal_with_memory(
        config: Config,
        sessions: Arc<SessionStore>,
        memory: Arc<dyn zeroclaw_api::memory_traits::Memory>,
    ) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: Some(memory),
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    #[cfg(test)]
    pub fn minimal_with_cost_tracker(
        config: Config,
        sessions: Arc<SessionStore>,
        cost_tracker: Arc<CostTracker>,
    ) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: Some(cost_tracker),
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    #[cfg(test)]
    pub fn for_persistence_tests(
        config: Config,
        sessions: Arc<SessionStore>,
        session_backend: Option<Arc<dyn SessionBackend>>,
        acp_session_store: Option<Arc<AcpSessionStore>>,
    ) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }

    #[cfg(test)]
    pub fn minimal_with_reload_controls(
        config: Config,
        sessions: Arc<SessionStore>,
        gateway_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
        reload_tx: Option<tokio::sync::watch::Sender<bool>>,
    ) -> Arc<Self> {
        let authority = LiveConfigAuthority::new(config);
        Arc::new(Self {
            config: authority.live_handle(),
            config_authority: authority.clone(),
            agent_lifecycle: authority.agent_lifecycle(),
            channel_generation_control: None,
            sessions,
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx,
            gateway_shutdown_tx,
            approval_pending: Arc::new(ApprovalPendingMap::default()),
            tui_registry: Arc::new(TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: None,
            cert_audit: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;
    use zeroclaw_api::channel::ChannelApprovalResponse;

    #[test]
    fn context_for_authority_shares_the_publication_domain() {
        let authority = LiveConfigAuthority::new(Config::default());
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            8, 30, 600,
        ));
        let sessions = Arc::new(SessionStore::new(16, queue));
        let ctx = RpcContext::for_authority(&authority, sessions);

        assert!(ctx.config.same_storage(&authority.live_handle()));
        assert!(
            ctx.config_authority
                .live_handle()
                .same_storage(&authority.live_handle())
        );
        assert_eq!(ctx.config.revision(), authority.published_revision());
    }

    #[test]
    fn pending_map_insert_and_resolve() {
        let map = ApprovalPendingMap::default();
        let (tx, mut rx) = oneshot::channel::<ChannelApprovalResponse>();
        map.insert("req-1".to_string(), "sess-1".to_string(), tx);
        assert!(map.resolve("req-1", "sess-1", ChannelApprovalResponse::Approve));
        assert!(!map.contains("req-1"));
        assert_eq!(rx.try_recv().unwrap(), ChannelApprovalResponse::Approve);
    }

    #[test]
    fn pending_map_resolve_unknown_key_is_noop() {
        let map = ApprovalPendingMap::default();
        assert!(!map.resolve("nonexistent", "sess-1", ChannelApprovalResponse::Deny));
    }

    #[test]
    fn pending_map_insert_then_drop_is_safe() {
        let map = ApprovalPendingMap::default();
        let (tx, _rx) = oneshot::channel::<ChannelApprovalResponse>();
        map.insert("req-2".to_string(), "sess-2".to_string(), tx);
        // _rx is dropped — resolve sends to a closed channel; must not panic
        assert!(map.resolve("req-2", "sess-2", ChannelApprovalResponse::Approve));
        assert!(!map.contains("req-2"));
    }

    #[test]
    fn pending_map_remove_drops_stale_request() {
        let map = ApprovalPendingMap::default();
        let (tx, _rx) = oneshot::channel::<ChannelApprovalResponse>();
        map.insert("req-3".to_string(), "sess-3".to_string(), tx);
        assert!(map.contains("req-3"));
        assert!(map.remove("req-3"));
        assert!(!map.contains("req-3"));
        assert!(!map.remove("req-3"));
    }

    #[test]
    fn pending_guard_drop_removes_registered_request() {
        let map = Arc::new(ApprovalPendingMap::default());
        let (tx, _rx) = oneshot::channel::<ChannelApprovalResponse>();
        let guard = map.register("req-4".to_string(), "sess-4".to_string(), tx);
        assert!(map.contains("req-4"));
        drop(guard);
        assert!(!map.contains("req-4"));
    }

    #[test]
    fn pending_guard_can_be_disarmed_after_resolution() {
        let map = Arc::new(ApprovalPendingMap::default());
        let (tx, _rx) = oneshot::channel::<ChannelApprovalResponse>();
        let mut guard = map.register("req-5".to_string(), "sess-5".to_string(), tx);
        assert!(map.resolve("req-5", "sess-5", ChannelApprovalResponse::Approve));
        guard.disarm();
        drop(guard);
        assert!(!map.contains("req-5"));
    }

    #[test]
    fn pending_map_rejects_foreign_session_without_consuming_request() {
        let map = ApprovalPendingMap::default();
        let (tx, mut rx) = oneshot::channel::<ChannelApprovalResponse>();
        map.insert("req-6".to_string(), "sess-owner".to_string(), tx);

        assert!(!map.resolve("req-6", "sess-foreign", ChannelApprovalResponse::Approve));
        assert!(map.contains("req-6"));
        assert!(rx.try_recv().is_err());
        assert!(map.resolve("req-6", "sess-owner", ChannelApprovalResponse::Deny));
        assert_eq!(rx.try_recv().unwrap(), ChannelApprovalResponse::Deny);
    }
}
