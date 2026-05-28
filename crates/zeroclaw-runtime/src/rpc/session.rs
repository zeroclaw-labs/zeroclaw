//! RPC session state.

use crate::agent::agent::Agent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zeroclaw_infra::session_queue::SessionActorQueue;

/// Grace period between a TUI / zerocode transport disconnect and the
/// daemon dropping that connection's sessions. Long enough to ride out
/// a network blip or a quick TUI restart with the same `tui_id`; short
/// enough that genuinely abandoned sessions don't grow daemon RSS for
/// long. Reclaimed early on reconnect via [`SessionStore::reclaim`].
pub const SESSION_DISCONNECT_GRACE: Duration = Duration::from_secs(120);

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
    max_sessions: usize,
    pub session_queue: Arc<SessionActorQueue>,
}

impl SessionStore {
    pub fn new(max_sessions: usize, session_queue: Arc<SessionActorQueue>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            cancel_tokens: std::sync::Mutex::new(HashMap::new()),
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
        if let Some(t) = overrides.temperature {
            guard.set_temperature(t);
        }
        Some(overrides)
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
            token.cancel();
        }
        self.sessions.lock().await.remove(id).is_some()
    }

    /// Mark every session owned by `tui_id` as orphaned, scheduling it for
    /// eviction at `now + grace`. Called from the transport-disconnect
    /// path; the grace window lets a reconnect of the same TUI reclaim
    /// these sessions before they are dropped.
    pub async fn mark_orphaned(&self, tui_id: &str, grace: std::time::Duration) -> usize {
        let deadline = Instant::now() + grace;
        let mut sessions = self.sessions.lock().await;
        let mut count = 0usize;
        for s in sessions.values_mut() {
            if s.owner_tui_id.as_deref() == Some(tui_id) {
                s.evict_at = Some(deadline);
                count += 1;
            }
        }
        count
    }

    /// Cancel any pending eviction for sessions owned by `tui_id`. Called
    /// when the same TUI ID reconnects within the grace window.
    pub async fn reclaim(&self, tui_id: &str) -> usize {
        let mut sessions = self.sessions.lock().await;
        let mut count = 0usize;
        for s in sessions.values_mut() {
            if s.owner_tui_id.as_deref() == Some(tui_id) && s.evict_at.is_some() {
                s.evict_at = None;
                count += 1;
            }
        }
        count
    }

    /// Drop every session whose pending eviction deadline has passed. Any
    /// in-flight turn has its cancel token fired before the agent goes,
    /// so spawned tasks wind down instead of holding the agent past
    /// eviction. Returns the number of sessions removed.
    pub async fn evict_expired(&self) -> usize {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().await;
        let stale: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.evict_at.is_some_and(|d| now >= d))
            .map(|(k, _)| k.clone())
            .collect();
        if stale.is_empty() {
            return 0;
        }
        {
            let mut tokens = self.cancel_tokens.lock().unwrap_or_else(|e| e.into_inner());
            for id in &stale {
                if let Some(token) = tokens.remove(id) {
                    token.cancel();
                }
            }
        }
        let mut evicted = 0usize;
        for id in &stale {
            if sessions.remove(id).is_some() {
                evicted += 1;
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
    }

    pub fn cancel_session(&self, id: &str) -> bool {
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
}
