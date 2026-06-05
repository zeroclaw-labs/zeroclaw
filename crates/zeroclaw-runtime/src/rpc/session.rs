//! RPC session state.

use crate::agent::agent::Agent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zeroclaw_infra::session_queue::SessionActorQueue;
use zeroclaw_providers::ModelProvider;

/// Grace period between a TUI / zerocode transport disconnect and the
/// daemon dropping that connection's sessions. Long enough to ride out
/// a network blip or a quick TUI restart with the same `tui_id`; short
/// enough that genuinely abandoned sessions don't grow daemon RSS for
/// long. Reclaimed early on reconnect via [`SessionStore::reclaim`].
pub const SESSION_DISCONNECT_GRACE: Duration = Duration::from_secs(1);

/// Hard upper bound on how long a live session may sit idle (no prompt,
/// no touch) before the reaper drops it regardless of connection state.
/// This is the backstop that keeps daemon RSS bounded: a client that
/// connects, opens sessions, and walks away without a clean disconnect
/// still has its agents reclaimed once they go cold. Ten minutes matches
/// the SessionActorQueue idle TTL so the two layers expire in step.
pub const SESSION_IDLE_TTL: Duration = Duration::from_secs(600);

/// Why the reaper removed a session — drives the eviction log so an
/// operator can tell a disconnect-orphan from a cold idle session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictReason {
    /// The owning TUI/WSS transport disconnected and the grace window
    /// elapsed without a reconnect.
    Orphaned,
    /// The session sat idle past [`SESSION_IDLE_TTL`] with no prompt.
    Idle,
}

/// Why a session's in-flight turn cancel token was fired. Recorded at the
/// firing site and drained at the turn-verdict site so the durable audit row
/// names the trigger instead of leaving a bare "cancelled" with no provenance.
/// Each variant is a distinct, named path — there is deliberately no catch-all
/// "unknown": a fired token must be attributable. `ReaperOrphaned` /
/// `ReaperIdle` mirror [`EvictReason`]; `ClientRpc` is an explicit
/// `session/cancel`; `SessionRemoved` is teardown via `remove`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelCause {
    /// Explicit `session/cancel` RPC from the client (e.g. zerocode Ctrl+D).
    ClientRpc,
    /// The reaper evicted the session after a transport-disconnect orphan
    /// grace window elapsed ([`EvictReason::Orphaned`]).
    ReaperOrphaned,
    /// The reaper evicted the session after it sat idle past
    /// [`SESSION_IDLE_TTL`] ([`EvictReason::Idle`]).
    ReaperIdle,
    /// The session was explicitly removed/torn down while a turn was live.
    SessionRemoved,
}

impl CancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelCause::ClientRpc => "client_rpc",
            CancelCause::ReaperOrphaned => "reaper_orphaned",
            CancelCause::ReaperIdle => "reaper_idle",
            CancelCause::SessionRemoved => "session_removed",
        }
    }
}

/// Record of one session the reaper freed. Carries enough provenance for
/// the eviction log to be useful: which session, which agent, the owning
/// TUI (if any), why it died, and how long it had been idle.
#[derive(Debug, Clone)]
pub struct EvictedSession {
    pub session_key: String,
    pub agent_alias: String,
    pub owner_tui_id: Option<String>,
    pub reason: EvictReason,
    pub idle_secs: u64,
}

/// Per-session runtime overrides. All fields are optional — `None` means
/// "use config default". Overrides are session-scoped, do not persist,
/// and evaporate when the session ends.
///
/// `reasoning_effort` is deferred — it requires `ModelProvider` trait
/// changes to support mutation after construction.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// An entry in the per-session upload index (content-addressed by SHA-256).
#[derive(Clone, Debug)]
pub struct UploadEntry {
    pub ref_id: String,
    pub marker: String,
    pub workspace_path: String,
    pub size_bytes: u64,
}

pub struct RpcSession {
    pub agent: Arc<Mutex<Agent>>,
    pub created_at: Instant,
    pub last_active: Instant,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub overrides: SessionOverrides,
    pub uploads: HashMap<String, UploadEntry>,
    pub chat_mode: crate::rpc::types::ChatMode,
    pub owner_tui_id: Option<String>,
    pub evict_at: Option<Instant>,
}

impl RpcSession {
    pub fn new(
        agent: Agent,
        alias: &str,
        workspace: &str,
        chat_mode: crate::rpc::types::ChatMode,
    ) -> Self {
        Self {
            agent: Arc::new(Mutex::new(agent)),
            created_at: Instant::now(),
            last_active: Instant::now(),
            agent_alias: alias.to_string(),
            workspace_dir: workspace.to_string(),
            overrides: SessionOverrides::default(),
            uploads: HashMap::new(),
            chat_mode,
            owner_tui_id: None,
            evict_at: None,
        }
    }

    /// Bind this session to a TUI owner; transport-disconnect cleanup
    /// uses this to mark orphaned sessions for grace-period eviction.
    pub fn with_owner(mut self, tui_id: Option<String>) -> Self {
        self.owner_tui_id = tui_id;
        self
    }
}

pub struct SessionStore {
    sessions: Mutex<HashMap<String, RpcSession>>,
    cancel_tokens: std::sync::Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    /// Records WHY each session's cancel token was fired. Populated at the
    /// firing site immediately before `token.cancel()`; drained by the
    /// turn-verdict site. Every known firing site records before firing; a
    /// fired token with no entry means a new path was added without wiring
    /// the cause — treat it as a bug, not as user attribution.
    cancel_causes: std::sync::Mutex<HashMap<String, CancelCause>>,
    max_sessions: usize,
    pub session_queue: Arc<SessionActorQueue>,
}

impl SessionStore {
    pub fn new(max_sessions: usize, session_queue: Arc<SessionActorQueue>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            cancel_tokens: std::sync::Mutex::new(HashMap::new()),
            cancel_causes: std::sync::Mutex::new(HashMap::new()),
            max_sessions,
            session_queue,
        }
    }

    pub async fn insert(&self, id: String, session: RpcSession) -> Result<(), &'static str> {
        let mut sessions = self.sessions.lock().await;
        if sessions.len() >= self.max_sessions {
            return Err("session limit reached");
        }
        sessions.insert(id, session);
        Ok(())
    }

    pub async fn get_agent(&self, id: &str) -> Option<Arc<Mutex<Agent>>> {
        self.sessions.lock().await.get(id).map(|s| s.agent.clone())
    }

    pub async fn touch(&self, id: &str) {
        if let Some(s) = self.sessions.lock().await.get_mut(id) {
            s.last_active = Instant::now();
        }
    }

    /// Apply overrides to the session and immediately mutate the agent.
    /// Returns the merged overrides for confirmation.
    ///
    /// Note: `model_provider` is recorded here but the live provider swap is
    /// driven by the dispatcher via [`Self::apply_model_provider`], because
    /// rebuilding the `ModelProvider` box needs `Config` access that the
    /// session store deliberately does not hold.
    pub async fn set_overrides(
        &self,
        id: &str,
        patch: SessionOverrides,
    ) -> Option<SessionOverrides> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id)?;
        if let Some(ref m) = patch.model {
            session.overrides.model = Some(m.clone());
        }
        if let Some(ref p) = patch.model_provider {
            session.overrides.model_provider = Some(p.clone());
            // A provider switch without an explicit model must not carry the
            // previous provider's model forward (e.g. switching to an Ollama
            // alias while a Claude model override lingers). Clear it so the
            // dispatcher resolves the new alias's configured model.
            if patch.model.is_none() {
                session.overrides.model = None;
            }
        }
        if let Some(t) = patch.temperature {
            session.overrides.temperature = Some(t);
        }
        // Apply to agent immediately.
        let overrides = session.overrides.clone();
        let agent = session.agent.clone();
        drop(sessions);
        let mut guard = agent.lock().await;
        if let Some(ref m) = overrides.model {
            guard.set_model_name(m.clone());
        }
        if overrides.temperature.is_some() {
            guard.set_temperature(overrides.temperature);
        }
        Some(overrides)
    }

    /// Swap a freshly built `ModelProvider` box (and its name) onto the
    /// session's agent. Called by the dispatcher after it constructs the
    /// box from config, keeping model_provider-build logic out of the store.
    pub async fn apply_model_provider(
        &self,
        id: &str,
        model_provider: Box<dyn ModelProvider>,
        model_provider_name: String,
    ) -> bool {
        let agent = {
            let sessions = self.sessions.lock().await;
            match sessions.get(id) {
                Some(s) => s.agent.clone(),
                None => return false,
            }
        };
        let mut guard = agent.lock().await;
        guard.set_model_provider(model_provider);
        guard.set_model_provider_name(model_provider_name);
        true
    }

    pub async fn get_overrides(&self, id: &str) -> Option<SessionOverrides> {
        self.sessions
            .lock()
            .await
            .get(id)
            .map(|s| s.overrides.clone())
    }

    /// Look up an existing upload by ref_id. Returns `None` if the session
    /// or entry doesn't exist.
    pub async fn get_upload(&self, session_id: &str, ref_id: &str) -> Option<UploadEntry> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|s| s.uploads.get(ref_id).cloned())
    }

    /// Insert (or overwrite) an upload entry in the session's index.
    pub async fn insert_upload(&self, session_id: &str, entry: UploadEntry) {
        if let Some(s) = self.sessions.lock().await.get_mut(session_id) {
            s.uploads.insert(entry.ref_id.clone(), entry);
        }
    }

    /// Get the workspace directory for a session.
    pub async fn get_workspace_dir(&self, session_id: &str) -> Option<String> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|s| s.workspace_dir.clone())
    }

    /// Get the agent alias bound to a session, if known. Used by the
    /// dispatcher to route uploads to the agent's own workspace dir
    /// rather than to the user's session cwd (which is often a git
    /// repo we shouldn't be writing into).
    pub async fn get_agent_alias(&self, session_id: &str) -> Option<String> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|s| s.agent_alias.clone())
    }

    pub async fn seed_history(&self, id: &str, msgs: &[zeroclaw_api::model_provider::ChatMessage]) {
        if let Some(s) = self.sessions.lock().await.get(id) {
            s.agent.lock().await.seed_history(msgs);
        }
    }

    pub async fn seed_conversation_history(
        &self,
        id: &str,
        msgs: Vec<zeroclaw_api::model_provider::ConversationMessage>,
    ) {
        if let Some(s) = self.sessions.lock().await.get(id) {
            s.agent.lock().await.seed_conversation_history(msgs);
        }
    }

    pub async fn chat_mode(&self, id: &str) -> Option<crate::rpc::types::ChatMode> {
        self.sessions
            .lock()
            .await
            .get(id)
            .map(|s| s.chat_mode.clone())
    }

    pub async fn history_len(&self, id: &str) -> Option<usize> {
        let sessions = self.sessions.lock().await;
        let s = sessions.get(id)?;
        Some(s.agent.lock().await.history().len())
    }

    pub async fn history_slice_from(
        &self,
        id: &str,
        from: usize,
    ) -> Option<Vec<zeroclaw_api::model_provider::ConversationMessage>> {
        let sessions = self.sessions.lock().await;
        let s = sessions.get(id)?;
        let h = s.agent.lock().await;
        // Saturate: `trim_history` can shift indices past `from` between polls.
        let history = h.history();
        Some(history[from.min(history.len())..].to_vec())
    }

    pub async fn remove(&self, id: &str) -> bool {
        if let Some(token) = self
            .cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            self.record_cancel_cause(id, CancelCause::SessionRemoved);
            token.cancel();
        }
        self.sessions.lock().await.remove(id).is_some()
    }

    /// Mark every session owned by `tui_id` as orphaned, scheduling it for
    /// eviction at `now + grace`. Called from the transport-disconnect
    /// path; the grace window lets a reconnect of the same TUI reclaim
    /// these sessions before they are dropped. Returns the
    /// `(session_key, agent_alias)` of each orphaned session so the caller
    /// can attribute the disconnect log to the real sessions.
    pub async fn mark_orphaned(
        &self,
        tui_id: &str,
        grace: std::time::Duration,
    ) -> Vec<(String, String)> {
        let deadline = Instant::now() + grace;
        let mut sessions = self.sessions.lock().await;
        let mut orphaned = Vec::new();
        for (key, s) in sessions.iter_mut() {
            if s.owner_tui_id.as_deref() == Some(tui_id) {
                s.evict_at = Some(deadline);
                orphaned.push((key.clone(), s.agent_alias.clone()));
            }
        }
        orphaned
    }

    /// Drop every *idle* session owned by `tui_id` in the same `chat_mode` as a
    /// freshly created session, except `except_id` itself. zerocode keeps one
    /// active session per mode per TUI: creating or loading another session of
    /// that mode abandons the prior one until it is explicitly reloaded, so the
    /// prior agent and its history are dead weight in RSS. Chat and Code
    /// sessions are orthogonal, so a Chat switch must never evict the live Code
    /// session and vice versa.
    ///
    /// A session with a registered cancel token has a turn in flight: a spawned
    /// `session/prompt` task still holds an `Arc<Mutex<Agent>>` clone, so
    /// removing the map's strong ref would neither free the agent nor be safe to
    /// trim against, and force-cancelling another TUI's mid-turn work is exactly
    /// the freeze the reaper guards against. Such sessions are skipped; they
    /// finish their turn and are reclaimed later by the idle reaper. Returns the
    /// `(session_key, agent_alias)` of each session actually dropped, so the
    /// caller can attribute the eviction and knows the agents are freed before
    /// it trims.
    pub async fn evict_same_mode_sibling(
        &self,
        tui_id: &str,
        chat_mode: &crate::rpc::types::ChatMode,
        except_id: &str,
    ) -> Vec<(String, String)> {
        let in_flight: std::collections::HashSet<String> = self
            .cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let mut sessions = self.sessions.lock().await;
        let victims: Vec<String> = sessions
            .iter()
            .filter(|(key, s)| {
                key.as_str() != except_id
                    && s.owner_tui_id.as_deref() == Some(tui_id)
                    && &s.chat_mode == chat_mode
                    && !in_flight.contains(key.as_str())
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut evicted = Vec::with_capacity(victims.len());
        for key in victims {
            if let Some(s) = sessions.remove(&key) {
                evicted.push((key, s.agent_alias));
            }
        }
        evicted
    }

    /// Read the `owner_tui_id` stamp from a session. Returns `None` if the
    /// session doesn't exist, `Some(None)` if it exists but is unowned (e.g.
    /// created by an anonymous connection), `Some(Some(id))` if owned by `id`.
    pub async fn session_owner_tui_id(&self, session_id: &str) -> Option<Option<String>> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).map(|s| s.owner_tui_id.clone())
    }

    /// Cancel any pending eviction for sessions owned by `tui_id`. Called
    /// when the same TUI ID reconnects within the grace window.
    pub async fn reclaim(&self, tui_id: &str) -> Vec<(String, String)> {
        let mut sessions = self.sessions.lock().await;
        let mut reclaimed = Vec::new();
        for (key, s) in sessions.iter_mut() {
            if s.owner_tui_id.as_deref() == Some(tui_id) && s.evict_at.is_some() {
                s.evict_at = None;
                reclaimed.push((key.clone(), s.agent_alias.clone()));
            }
        }
        reclaimed
    }

    /// Drop every session whose pending eviction deadline has passed, or
    /// that has sat idle past [`SESSION_IDLE_TTL`] AND has no in-flight
    /// turn. The cancel token map is the daemon's source of truth for
    /// "turn in progress"; an entry there means `handle_chat` is mid-drain
    /// and `last_active` is stale only because the tool loop has not
    /// returned to `handle_chat` to call `touch()` again. Reaping under a
    /// live turn was the production freeze: the cancel token fired with
    /// `ReaperIdle`, the turn aborted, and the next prompt landed on a
    /// gone-from-memory session that silently 404'd — the TUI's `working`
    /// state never cleared. The orphan path (transport disconnected) is
    /// NOT gated on in-flight: an orphaned session whose owner is gone has
    /// nobody to deliver `TurnComplete` to, so the turn is collateral.
    /// Returns one [`EvictedSession`] per removed entry.
    pub async fn evict_expired(&self) -> Vec<EvictedSession> {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().await;
        let in_flight: std::collections::HashSet<String> = self
            .cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let stale: Vec<(String, EvictReason, u64)> = sessions
            .iter()
            .filter_map(|(k, s)| {
                let orphaned = s.evict_at.is_some_and(|d| now >= d);
                let idle_secs = now.duration_since(s.last_active).as_secs();
                let idle = now.duration_since(s.last_active) >= SESSION_IDLE_TTL;
                if orphaned {
                    Some((k.clone(), EvictReason::Orphaned, idle_secs))
                } else if idle && !in_flight.contains(k) {
                    Some((k.clone(), EvictReason::Idle, idle_secs))
                } else {
                    None
                }
            })
            .collect();
        if stale.is_empty() {
            return Vec::new();
        }
        {
            let mut tokens = self.cancel_tokens.lock().unwrap_or_else(|e| e.into_inner());
            let mut causes = self.cancel_causes.lock().unwrap_or_else(|e| e.into_inner());
            for (id, reason, _) in &stale {
                if let Some(token) = tokens.remove(id) {
                    causes.insert(
                        id.clone(),
                        match reason {
                            EvictReason::Orphaned => CancelCause::ReaperOrphaned,
                            EvictReason::Idle => CancelCause::ReaperIdle,
                        },
                    );
                    token.cancel();
                }
            }
        }
        let mut evicted = Vec::with_capacity(stale.len());
        for (id, reason, idle_secs) in stale {
            if let Some(s) = sessions.remove(&id) {
                evicted.push(EvictedSession {
                    session_key: id,
                    agent_alias: s.agent_alias,
                    owner_tui_id: s.owner_tui_id,
                    reason,
                    idle_secs,
                });
            }
        }
        evicted
    }

    pub async fn list_ids(&self) -> Vec<String> {
        self.sessions.lock().await.keys().cloned().collect()
    }

    pub fn register_cancel_token(&self, id: &str, token: tokio_util::sync::CancellationToken) {
        self.cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), token);
    }

    pub fn remove_cancel_token(&self, id: &str) {
        self.cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        // A token removed at clean turn end carries no cancel; drop any stale
        // cause so it cannot leak onto a later turn for the same session id.
        self.cancel_causes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    pub fn cancel_session(&self, id: &str) -> bool {
        self.record_cancel_cause(id, CancelCause::ClientRpc);
        self.cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|t| {
                t.cancel();
                true
            })
            .unwrap_or(false)
    }

    /// Record the cause for an imminent cancel-token fire. Call immediately
    /// before firing so the verdict site can attribute the cancel.
    pub fn record_cancel_cause(&self, id: &str, cause: CancelCause) {
        self.cancel_causes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), cause);
    }

    /// Drain the recorded cancel cause for a session. Returns `None` only
    /// when no cancel actually fired (clean completion); every firing path
    /// records before `token.cancel()`, so `Some(_)` after a fired token is
    /// the invariant the verdict audit relies on.
    pub fn take_cancel_cause(&self, id: &str) -> Option<CancelCause> {
        self.cancel_causes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
    }

    pub async fn count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Count active sessions grouped by agent alias.
    pub async fn count_by_agent(&self) -> HashMap<String, usize> {
        let sessions = self.sessions.lock().await;
        let mut counts: HashMap<String, usize> = HashMap::new();
        for session in sessions.values() {
            *counts.entry(session.agent_alias.clone()).or_insert(0) += 1;
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(max: usize) -> SessionStore {
        SessionStore::new(max, Arc::new(SessionActorQueue::new(4, 10, 60)))
    }

    fn make_agent() -> Agent {
        use crate::agent::dispatcher::NativeToolDispatcher;
        use crate::observability::NoopObserver;

        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem = Arc::from(
            zeroclaw_memory::create_memory(&mem_cfg, &std::env::temp_dir(), None).unwrap(),
        );

        Agent::builder()
            .model_provider(Box::new(StubProvider))
            .tools(vec![])
            .memory(mem)
            .observer(Arc::new(NoopObserver {}) as Arc<dyn crate::observability::Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::env::temp_dir())
            .build()
            .unwrap()
    }

    /// Minimal provider that satisfies the builder. Never called in these tests.
    struct StubProvider;

    #[async_trait::async_trait]
    impl zeroclaw_providers::ModelProvider for StubProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn chat(
            &self,
            _: zeroclaw_providers::ChatRequest<'_>,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("stub".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl zeroclaw_api::attribution::Attributable for StubProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "stub"
        }
    }

    #[tokio::test]
    async fn insert_and_count() {
        let store = make_store(4);
        assert_eq!(store.count().await, 0);

        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        assert_eq!(store.count().await, 1);
    }

    #[tokio::test]
    async fn insert_rejects_over_limit() {
        let store = make_store(1);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let err = store
            .insert(
                "s2".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn get_agent_returns_arc() {
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        assert!(store.get_agent("s1").await.is_some());
        assert!(store.get_agent("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn set_overrides_applies_model_and_temperature_live() {
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        let merged = store
            .set_overrides(
                "s1",
                SessionOverrides {
                    model: Some("model-x".into()),
                    temperature: Some(0.42),
                    ..Default::default()
                },
            )
            .await
            .expect("session exists");
        assert_eq!(merged.model.as_deref(), Some("model-x"));
        assert_eq!(merged.temperature, Some(0.42));

        // The override is applied to the live agent immediately.
        let agent = store.get_agent("s1").await.unwrap();
        let (_, _, model_name) = agent.lock().await.attribution_fields();
        assert_eq!(model_name, "model-x");
    }

    #[tokio::test]
    async fn set_overrides_records_model_provider_without_rebuilding() {
        // The store records the model_provider override but does NOT rebuild the
        // provider box — that is the dispatcher's job (needs Config). Here we
        // only assert the field round-trips through the merge.
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        let merged = store
            .set_overrides(
                "s1",
                SessionOverrides {
                    model_provider: Some("anthropic.default".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("session exists");
        assert_eq!(merged.model_provider.as_deref(), Some("anthropic.default"));
    }

    #[tokio::test]
    async fn provider_switch_without_model_clears_prior_model() {
        // Switching provider with no explicit model must drop the prior
        // model override so the dispatcher resolves the new alias's
        // configured model (e.g. Ollama alias must not keep a Claude model).
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        store
            .set_overrides(
                "s1",
                SessionOverrides {
                    model: Some("claude-opus-4-5".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("session exists");
        let merged = store
            .set_overrides(
                "s1",
                SessionOverrides {
                    model_provider: Some("ollama.default".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("session exists");
        assert_eq!(merged.model_provider.as_deref(), Some("ollama.default"));
        assert_eq!(
            merged.model, None,
            "a provider-only switch must clear the lingering model override"
        );
    }

    #[tokio::test]
    async fn set_overrides_missing_session_is_none() {
        let store = make_store(4);
        assert!(
            store
                .set_overrides("ghost", SessionOverrides::default())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn remove_cleans_up() {
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        let token = tokio_util::sync::CancellationToken::new();
        store.register_cancel_token("s1", token.clone());

        assert!(store.remove("s1").await);
        assert_eq!(store.count().await, 0);
        // Cancel token was also removed -- cancelling is a no-op now.
        assert!(!store.cancel_session("s1"));
    }

    #[tokio::test]
    async fn remove_nonexistent_returns_false() {
        let store = make_store(4);
        assert!(!store.remove("ghost").await);
    }

    #[tokio::test]
    async fn evict_same_mode_sibling_drops_only_same_mode_owner() {
        use crate::rpc::types::ChatMode;
        let store = make_store(8);
        let mk = |mode: ChatMode, owner: &str| {
            RpcSession::new(make_agent(), "a", ".", mode).with_owner(Some(owner.to_string()))
        };
        store
            .insert("old_chat".into(), mk(ChatMode::Chat, "tui1"))
            .await
            .unwrap();
        store
            .insert("old_code".into(), mk(ChatMode::Acp, "tui1"))
            .await
            .unwrap();
        store
            .insert("other_chat".into(), mk(ChatMode::Chat, "tui2"))
            .await
            .unwrap();
        store
            .insert("new_chat".into(), mk(ChatMode::Chat, "tui1"))
            .await
            .unwrap();

        let evicted = store
            .evict_same_mode_sibling("tui1", &ChatMode::Chat, "new_chat")
            .await;

        let ids: Vec<&str> = evicted.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["old_chat"]);
        assert!(
            store.get_agent("new_chat").await.is_some(),
            "new session preserved"
        );
        assert!(
            store.get_agent("old_code").await.is_some(),
            "cross-mode Code session preserved"
        );
        assert!(
            store.get_agent("other_chat").await.is_some(),
            "other TUI session preserved"
        );
        assert!(
            store.get_agent("old_chat").await.is_none(),
            "abandoned same-mode session evicted"
        );
    }

    #[tokio::test]
    async fn evict_same_mode_sibling_skips_in_flight_turn() {
        use crate::rpc::types::ChatMode;
        let store = make_store(8);
        let mk = |mode: ChatMode, owner: &str| {
            RpcSession::new(make_agent(), "a", ".", mode).with_owner(Some(owner.to_string()))
        };
        store
            .insert("busy_chat".into(), mk(ChatMode::Chat, "tui1"))
            .await
            .unwrap();
        store
            .insert("new_chat".into(), mk(ChatMode::Chat, "tui1"))
            .await
            .unwrap();
        // A registered cancel token marks a turn in flight: a spawned prompt
        // task still holds an Agent clone, so this session must NOT be force
        // evicted (that is the reaper's documented mid-turn freeze).
        let token = tokio_util::sync::CancellationToken::new();
        store.register_cancel_token("busy_chat", token.clone());

        let evicted = store
            .evict_same_mode_sibling("tui1", &ChatMode::Chat, "new_chat")
            .await;

        assert!(
            evicted.is_empty(),
            "in-flight same-mode session must be left to finish its turn"
        );
        assert!(
            store.get_agent("busy_chat").await.is_some(),
            "mid-turn session preserved"
        );
        assert!(
            !token.is_cancelled(),
            "eviction must not fire a mid-turn cancel token"
        );
    }

    #[tokio::test]
    async fn cancel_token_lifecycle() {
        let store = make_store(4);
        let token = tokio_util::sync::CancellationToken::new();
        store.register_cancel_token("s1", token.clone());

        assert!(!token.is_cancelled());
        assert!(store.cancel_session("s1"));
        assert!(token.is_cancelled());

        // Second cancel returns false (token was consumed by remove).
        store.remove_cancel_token("s1");
        assert!(!store.cancel_session("s1"));
    }

    #[tokio::test]
    async fn cancel_nonexistent_returns_false() {
        let store = make_store(4);
        assert!(!store.cancel_session("nope"));
    }

    #[tokio::test]
    async fn list_ids() {
        let store = make_store(4);
        store
            .insert(
                "b".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        store
            .insert(
                "a".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let mut ids = store.list_ids().await;
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn touch_updates_last_active() {
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        let before = { store.sessions.lock().await.get("s1").unwrap().last_active };
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        store.touch("s1").await;
        let after = { store.sessions.lock().await.get("s1").unwrap().last_active };
        assert!(after > before);
    }

    /// RED guard: a session whose cancel token is registered is mid-turn.
    /// The reaper must NOT evict it on idle even when `last_active` is older
    /// than [`SESSION_IDLE_TTL`]. This is the production freeze: a tool loop
    /// that runs for >10min between `touch()` calls (the agent iterates
    /// without re-entering `handle_chat`) gets reaped under itself, the
    /// in-flight turn aborts with `ReaperIdle`, and the next prompt lands on
    /// a half-dead session that silently 404s — the TUI hangs forever in
    /// `(working..)` with no `TurnComplete` ever arriving.
    #[tokio::test]
    async fn evict_expired_skips_session_with_inflight_cancel_token() {
        let store = make_store(4);
        store
            .insert(
                "live-turn".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        // Backdate `last_active` to simulate a long-running tool loop:
        // the turn began 11 minutes ago and never re-entered handle_chat.
        {
            let mut sessions = store.sessions.lock().await;
            let s = sessions.get_mut("live-turn").unwrap();
            s.last_active = Instant::now() - (SESSION_IDLE_TTL + Duration::from_secs(60));
        }

        // Register a cancel token — this is the daemon's signal that a turn
        // is in flight. The reaper must consult it before evicting.
        let token = tokio_util::sync::CancellationToken::new();
        store.register_cancel_token("live-turn", token.clone());

        let evicted = store.evict_expired().await;
        assert!(
            evicted.is_empty(),
            "reaper evicted a session mid-turn (idle timer outran the tool \
             loop). This is the production freeze. evicted={evicted:?}"
        );
        assert!(
            !token.is_cancelled(),
            "reaper fired the in-flight turn's cancel token on an idle race \
             — the very next prompt on this session is now doomed"
        );
        assert_eq!(
            store.count().await,
            1,
            "session must remain present so the running turn can complete"
        );
    }

    /// GREEN guard: a session with no in-flight turn must still be reaped
    /// when idle past the TTL. The fix must NOT make the reaper toothless.
    #[tokio::test]
    async fn evict_expired_still_drops_idle_session_with_no_inflight_turn() {
        let store = make_store(4);
        store
            .insert(
                "cold".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        {
            let mut sessions = store.sessions.lock().await;
            let s = sessions.get_mut("cold").unwrap();
            s.last_active = Instant::now() - (SESSION_IDLE_TTL + Duration::from_secs(60));
        }
        // No cancel token registered: no turn in flight.

        let evicted = store.evict_expired().await;
        assert_eq!(evicted.len(), 1, "cold idle session must still be reaped");
        assert_eq!(evicted[0].reason, EvictReason::Idle);
        assert_eq!(store.count().await, 0);
    }
}
