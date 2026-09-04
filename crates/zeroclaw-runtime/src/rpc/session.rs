//! RPC session state.

use crate::agent::agent::{Agent, TurnEvent};
use crate::agent::dispatcher::ToolDispatcher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use zeroclaw_api::plan::PlanEntry;
use zeroclaw_infra::session_queue::SessionActorQueue;
use zeroclaw_providers::ModelProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelCause {
    /// `session/cancel` RPC arrived over the client channel. The actor is not
    /// verified here: a human interrupt and a programmatic client cancel (new
    /// prompt superseding an in-flight turn, reconnect, client-side timeout,
    /// pane teardown) all land on this path. Attribute to the channel, not a user.
    ClientRpc,
    /// Explicit `session/kill` from the dashboard or admin RPC.
    AdminKill,
    /// The session was explicitly removed/torn down while a turn was live.
    SessionRemoved,
}

impl CancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelCause::ClientRpc => "client_rpc",
            CancelCause::AdminKill => "admin_kill",
            CancelCause::SessionRemoved => "session_removed",
        }
    }
}

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
    /// Orders provider refreshes and configuration within this session.
    model_provider_update: Arc<Mutex<()>>,
    pub created_at: Instant,
    pub last_active: Instant,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub overrides: SessionOverrides,
    pub uploads: HashMap<String, UploadEntry>,
    pub plan: Vec<PlanEntry>,
    pub chat_mode: crate::rpc::types::ChatMode,
    pub owner_tui_id: Option<String>,
    /// Monotonic generation counter stamped by `SessionStore::insert`.
    /// Provider-refresh callers capture this before building a provider box
    /// and pass it to `SessionStore::apply_model_provider` so stale work
    /// cannot mutate a successor installed under the same session ID.
    pub generation: u64,
    /// Owning principal for session isolation. `None` for sessions created
    /// by unscoped connections (shared operator, admin): such sessions are
    /// visible to unscoped connections and invisible to scoped principals.
    pub owner_principal_id: Option<String>,
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
            model_provider_update: Arc::new(Mutex::new(())),
            created_at: Instant::now(),
            last_active: Instant::now(),
            agent_alias: alias.to_string(),
            workspace_dir: workspace.to_string(),
            overrides: SessionOverrides::default(),
            uploads: HashMap::new(),
            plan: Vec::new(),
            chat_mode,
            owner_tui_id: None,
            generation: 0,
            owner_principal_id: None,
        }
    }

    /// Bind this session to a TUI owner.
    pub fn with_owner(mut self, tui_id: Option<String>) -> Self {
        self.owner_tui_id = tui_id;
        self
    }

    /// Bind this session to its owning principal (scoped principals only;
    /// unscoped connections pass `None`).
    pub fn with_owner_principal(mut self, principal_id: Option<String>) -> Self {
        self.owner_principal_id = principal_id;
        self
    }
}

#[cfg(test)]
type GatedOpPause = (
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
);

#[cfg(test)]
type PromptRegistrationPause = (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);

pub struct SessionStore {
    sessions: Mutex<HashMap<String, RpcSession>>,
    #[cfg(test)]
    model_provider_update_waiting: Arc<tokio::sync::Notify>,
    cancel_tokens: std::sync::Mutex<HashMap<String, (u64, tokio_util::sync::CancellationToken)>>,
    cancel_generation: std::sync::atomic::AtomicU64,
    cancel_causes: std::sync::Mutex<HashMap<String, CancelCause>>,
    max_sessions: usize,
    pub session_queue: Arc<SessionActorQueue>,
    /// Monotonic counter incremented on every `insert` that installs or
    /// replaces a session entry. Captured by provider-refresh callers so
    /// `apply_model_provider` can reject work from a stale instance.
    session_generation: std::sync::atomic::AtomicU64,
    /// Test-only gate: when set, `set_overrides_gated` and
    /// `apply_model_provider` signal `entered` on entry, wait on
    /// `release`, then signal `done` on exit, letting a regression test
    /// atomically replace the session and wait for completion.
    #[cfg(test)]
    test_gated_op_pause: std::sync::Mutex<Option<GatedOpPause>>,
    /// Test-only pause immediately after a prompt registers its cancellation
    /// token. Removal-race tests use this to issue close/kill/delete while the
    /// prompt owns admission but before any fallible setup or provider work.
    #[cfg(test)]
    test_prompt_registration_pause: std::sync::Mutex<Option<PromptRegistrationPause>>,
}

/// Generation-owned handle for the canonical cancellation-token registration.
///
/// The token map in [`SessionStore`] remains the source of truth. This handle
/// only guarantees that the exact generation installed by one admitted prompt
/// is removed on every exit path, including setup failures before provider
/// execution starts.
pub(crate) struct CancelTokenRegistration<'a> {
    store: &'a SessionStore,
    session_id: &'a str,
    generation: Option<u64>,
}

impl CancelTokenRegistration<'_> {
    /// Drain the turn's cancellation attribution before unregistering the
    /// token. `remove_cancel_token` intentionally clears any leftover cause.
    pub(crate) fn finish(mut self) -> Option<CancelCause> {
        let cause = self.store.take_cancel_cause(self.session_id);
        if let Some(generation) = self.generation.take() {
            self.store.remove_cancel_token(self.session_id, generation);
        }
        cause
    }
}

impl Drop for CancelTokenRegistration<'_> {
    fn drop(&mut self) {
        if let Some(generation) = self.generation.take() {
            self.store.remove_cancel_token(self.session_id, generation);
        }
    }
}

impl SessionStore {
    pub fn new(max_sessions: usize, session_queue: Arc<SessionActorQueue>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            #[cfg(test)]
            model_provider_update_waiting: Arc::new(tokio::sync::Notify::new()),
            cancel_tokens: std::sync::Mutex::new(HashMap::new()),
            cancel_generation: std::sync::atomic::AtomicU64::new(0),
            cancel_causes: std::sync::Mutex::new(HashMap::new()),
            max_sessions,
            session_queue,
            session_generation: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            test_gated_op_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            test_prompt_registration_pause: std::sync::Mutex::new(None),
        }
    }

    pub async fn insert(&self, id: String, mut session: RpcSession) -> Result<(), &'static str> {
        let mut sessions = self.sessions.lock().await;
        if sessions.len() >= self.max_sessions {
            return Err("session limit reached");
        }
        let generation = self
            .session_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .wrapping_add(1);
        session.generation = generation;
        sessions.insert(id, session);
        Ok(())
    }

    /// Atomically reserve a NEW session id: fails if the id is already
    /// live, so `session/new` can never replace (and thereby hijack) an
    /// existing session's agent, whoever owns it.
    pub async fn insert_if_absent(
        &self,
        id: String,
        session: RpcSession,
    ) -> Result<(), &'static str> {
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&id) {
            return Err("session id already in use");
        }
        if sessions.len() >= self.max_sessions {
            return Err("session limit reached");
        }
        sessions.insert(id, session);
        Ok(())
    }

    pub async fn get_agent(&self, id: &str) -> Option<Arc<Mutex<Agent>>> {
        self.sessions.lock().await.get(id).map(|s| s.agent.clone())
    }

    pub(crate) async fn lock_model_provider_update(
        &self,
        id: &str,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let lock = self
            .sessions
            .lock()
            .await
            .get(id)
            .map(|session| Arc::clone(&session.model_provider_update))?;
        #[cfg(test)]
        {
            let waiting = Arc::clone(&self.model_provider_update_waiting);
            let mut lock = Box::pin(lock.lock_owned());
            let mut notified = false;
            return Some(
                std::future::poll_fn(move |cx| {
                    match std::future::Future::poll(lock.as_mut(), cx) {
                        std::task::Poll::Pending => {
                            if !notified {
                                waiting.notify_one();
                                notified = true;
                            }
                            std::task::Poll::Pending
                        }
                        std::task::Poll::Ready(guard) => std::task::Poll::Ready(guard),
                    }
                })
                .await,
            );
        }
        #[cfg(not(test))]
        Some(lock.lock_owned().await)
    }

    #[cfg(test)]
    pub(crate) fn model_provider_update_waiting(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.model_provider_update_waiting)
    }

    /// Return the current generation for the session with `id`, or `None` if
    /// the session is absent. Provider-refresh callers capture this value
    /// before building a provider box and thread it through
    /// `apply_model_provider` so stale work targeting a replaced session
    /// becomes a no-op.
    pub async fn get_generation(&self, id: &str) -> Option<u64> {
        self.sessions.lock().await.get(id).map(|s| s.generation)
    }

    /// Await the test-only pause gate before validating generation in
    /// `set_overrides_gated` and `apply_model_provider`. Returns the
    /// `done` notifier which must be signalled after the generation check
    /// (and any apply attempt) completes so tests don't observe the
    /// successor before the stale work has run its course.
    #[cfg(test)]
    async fn wait_test_gate(&self) -> Option<Arc<tokio::sync::Notify>> {
        let (entered, release, done) = {
            let guard = self.test_gated_op_pause.lock().unwrap();
            match &*guard {
                Some((e, r, d)) => (e.clone(), r.clone(), d.clone()),
                None => return None,
            }
        };
        entered.notify_one();
        release.notified().await;
        Some(done)
    }

    /// Signal the `done` notifier returned by `wait_test_gate`.
    #[cfg(test)]
    fn signal_test_gate_done(&self, done: Option<Arc<tokio::sync::Notify>>) {
        if let Some(d) = done {
            d.notify_one();
        }
    }

    #[cfg(not(test))]
    #[inline(always)]
    async fn wait_test_gate(&self) {}
    #[cfg(not(test))]
    #[inline(always)]
    fn signal_test_gate_done(&self, _done: ()) {}

    /// Install a test-only pause gate at the pre-commit boundary inside
    /// `set_overrides_gated` and `apply_model_provider`. Returns
    /// `(entered, release, done)`.
    #[cfg(test)]
    pub fn set_test_gated_op_pause(&self) -> GatedOpPause {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let done = Arc::new(tokio::sync::Notify::new());
        *self.test_gated_op_pause.lock().unwrap() = Some((
            Arc::clone(&entered),
            Arc::clone(&release),
            Arc::clone(&done),
        ));
        (entered, release, done)
    }

    #[cfg(test)]
    pub fn clear_test_gated_op_pause(&self) {
        *self.test_gated_op_pause.lock().unwrap() = None;
    }

    pub async fn touch(&self, id: &str) {
        if let Some(s) = self.sessions.lock().await.get_mut(id) {
            s.last_active = Instant::now();
        }
    }

    pub async fn set_overrides(
        &self,
        id: &str,
        patch: SessionOverrides,
    ) -> Option<SessionOverrides> {
        let merged = self.preview_overrides(id, &patch).await?;
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id)?;
        session.overrides = merged.clone();
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

    /// Like `set_overrides`, but validates `generation` before mutating the
    /// session entry or its agent. Returns `None` if the session is absent
    /// *or* if it was replaced under the same ID — both cases mean the caller
    /// captured a stale generation and the work must be discarded.
    pub async fn set_overrides_gated(
        &self,
        id: &str,
        generation: u64,
        patch: SessionOverrides,
    ) -> Option<SessionOverrides> {
        let done = self.wait_test_gate().await;
        let merged = self.preview_overrides(id, &patch).await?;
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id)?;
        if session.generation != generation {
            self.signal_test_gate_done(done);
            return None;
        }
        session.overrides = merged.clone();
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
        self.signal_test_gate_done(done);
        Some(overrides)
    }

    pub async fn preview_overrides(
        &self,
        id: &str,
        patch: &SessionOverrides,
    ) -> Option<SessionOverrides> {
        let sessions = self.sessions.lock().await;
        let session = sessions.get(id)?;
        let mut merged = session.overrides.clone();
        if let Some(ref m) = patch.model {
            merged.model = Some(m.clone());
        }
        if let Some(ref p) = patch.model_provider {
            merged.model_provider = Some(p.clone());
            // A provider switch without an explicit model must not carry the
            // previous provider's model forward (e.g. switching to an Ollama
            // alias while a Claude model override lingers). Clear it so the
            // dispatcher resolves the new alias's configured model.
            if patch.model.is_none() {
                merged.model = None;
            }
        }
        if let Some(t) = patch.temperature {
            merged.temperature = Some(t);
        }
        Some(merged)
    }

    /// Swap a freshly built `ModelProvider` box (and its name) onto the
    /// session's agent. Called by the dispatcher after it constructs the
    /// box from config, keeping model_provider-build logic out of the store.
    ///
    /// `generation` must match the session's current generation (captured
    /// before the caller built the provider box). If the session was replaced
    /// under the same ID — e.g. by `session/new` or ACP rehydration — while
    /// the provider was being built, the generations won't match and this
    /// call becomes a no-op (returns `false`).
    ///
    /// When `temperature` is `Some(v)`, the captured agent's temperature is
    /// set to `v` (which may be `None`, clearing a prior profile temperature).
    /// When `temperature` is `None`, the agent's temperature is left unchanged
    /// — used by `session/configure` where temperature is already committed
    /// via `set_overrides_gated`.
    pub async fn apply_model_provider(
        &self,
        id: &str,
        generation: u64,
        model_provider: Box<dyn ModelProvider>,
        model_provider_name: String,
        model_name: String,
        tool_dispatcher: Box<dyn ToolDispatcher>,
        temperature: Option<Option<f64>>,
    ) -> bool {
        let done = self.wait_test_gate().await;
        let agent = {
            let sessions = self.sessions.lock().await;
            match sessions.get(id) {
                Some(s) if s.generation == generation => s.agent.clone(),
                Some(_) => {
                    self.signal_test_gate_done(done);
                    return false;
                }
                None => {
                    self.signal_test_gate_done(done);
                    return false;
                }
            }
        };
        let mut guard = agent.lock().await;
        guard.set_model_provider(model_provider);
        guard.set_model_provider_name(model_provider_name);
        guard.set_model_name(model_name);
        guard.set_tool_dispatcher(tool_dispatcher);
        if let Some(t) = temperature {
            guard.set_temperature(t);
        }
        self.signal_test_gate_done(done);
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
        let _ = self.seed_history_with_event(id, msgs).await;
    }

    pub async fn seed_history_with_event(
        &self,
        id: &str,
        msgs: &[zeroclaw_api::model_provider::ChatMessage],
    ) -> Option<TurnEvent> {
        if let Some(s) = self.sessions.lock().await.get(id) {
            s.agent.lock().await.seed_history_with_event(msgs)
        } else {
            None
        }
    }

    /// Replace the session's execution plan wholesale (TodoWrite
    /// whole-list semantics). No-op if the session is unknown.
    pub async fn set_plan(&self, id: &str, entries: Vec<PlanEntry>) {
        if let Some(s) = self.sessions.lock().await.get_mut(id) {
            s.plan = entries;
        }
    }

    /// Current stored plan for a session. `None` if the session is
    /// unknown; `Some(empty)` if the session exists with no/cleared plan.
    pub async fn get_plan(&self, id: &str) -> Option<Vec<PlanEntry>> {
        self.sessions.lock().await.get(id).map(|s| s.plan.clone())
    }

    pub async fn seed_conversation_history(
        &self,
        id: &str,
        msgs: Vec<zeroclaw_api::model_provider::ConversationMessage>,
    ) {
        let _ = self.seed_conversation_history_with_event(id, msgs).await;
    }

    pub async fn seed_conversation_history_with_event(
        &self,
        id: &str,
        msgs: Vec<zeroclaw_api::model_provider::ConversationMessage>,
    ) -> Option<TurnEvent> {
        if let Some(s) = self.sessions.lock().await.get(id) {
            s.agent
                .lock()
                .await
                .seed_conversation_history_with_event(msgs)
        } else {
            None
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
        if let Some((_, token)) = self
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

    /// Read the owning-principal stamp from a LIVE session. Same tri-state
    /// contract as [`Self::session_owner_tui_id`].
    pub async fn session_owner_principal(&self, session_id: &str) -> Option<Option<String>> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(session_id)
            .map(|s| s.owner_principal_id.clone())
    }

    pub async fn list_ids(&self) -> Vec<String> {
        self.sessions.lock().await.keys().cloned().collect()
    }

    pub fn register_cancel_token(
        &self,
        id: &str,
        token: tokio_util::sync::CancellationToken,
    ) -> u64 {
        let generation = self
            .cancel_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .wrapping_add(1);
        if let Some((_, stale)) = self
            .cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), (generation, token))
        {
            stale.cancel();
        }
        generation
    }

    pub(crate) fn register_cancel_token_guard<'a>(
        &'a self,
        id: &'a str,
        token: tokio_util::sync::CancellationToken,
    ) -> CancelTokenRegistration<'a> {
        CancelTokenRegistration {
            store: self,
            session_id: id,
            generation: Some(self.register_cancel_token(id, token)),
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_test_prompt_registration_pause(&self) {
        let (entered, release) = {
            let guard = self.test_prompt_registration_pause.lock().unwrap();
            match &*guard {
                Some((entered, release)) => (Arc::clone(entered), Arc::clone(release)),
                None => return,
            }
        };
        entered.notify_one();
        release.notified().await;
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) async fn wait_test_prompt_registration_pause(&self) {}

    #[cfg(test)]
    pub(crate) fn set_test_prompt_registration_pause(&self) -> PromptRegistrationPause {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *self.test_prompt_registration_pause.lock().unwrap() =
            Some((Arc::clone(&entered), Arc::clone(&release)));
        (entered, release)
    }

    pub fn remove_cancel_token(&self, id: &str, generation: u64) {
        {
            let mut tokens = self.cancel_tokens.lock().unwrap_or_else(|e| e.into_inner());
            match tokens.get(id) {
                Some((g, _)) if *g == generation => {
                    tokens.remove(id);
                }
                _ => return,
            }
        }
        self.cancel_causes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    pub fn cancel_session(&self, id: &str) -> bool {
        self.signal_cancellation(id, CancelCause::ClientRpc)
    }

    /// Signal an in-flight turn before a close/delete handler waits for the
    /// session admission permit. The handler removes the session only after
    /// the admitted prompt has finalized under its original incarnation.
    pub fn signal_session_removal(&self, id: &str) -> bool {
        self.signal_cancellation(id, CancelCause::SessionRemoved)
    }

    /// Signal an in-flight turn before an administrative kill waits for the
    /// session admission permit.
    pub fn signal_session_kill(&self, id: &str) -> bool {
        self.signal_cancellation(id, CancelCause::AdminKill)
    }

    fn signal_cancellation(&self, id: &str, cause: CancelCause) -> bool {
        let tokens = self.cancel_tokens.lock().unwrap_or_else(|e| e.into_inner());
        tokens
            .get(id)
            .map(|(_, token)| {
                self.record_cancel_cause(id, cause);
                token.cancel();
                true
            })
            .unwrap_or(false)
    }

    /// Returns true if a cancel token is registered — i.e. a turn is in flight.
    pub fn has_inflight_turn(&self, id: &str) -> bool {
        self.cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    pub async fn kill_session(&self, id: &str) -> bool {
        if let Some((_, token)) = self
            .cancel_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            self.record_cancel_cause(id, CancelCause::AdminKill);
            token.cancel();
        }
        self.sessions.lock().await.remove(id).is_some()
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
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![],
            ))
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
    async fn set_and_get_plan_round_trips() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};
        fn sample_entry(content: &str, status: PlanStatus) -> PlanEntry {
            PlanEntry {
                content: content.to_string(),
                status,
                priority: PlanPriority::Medium,
                active_form: None,
            }
        }

        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        // Fresh session has an empty (but present) plan.
        assert!(store.get_plan("s1").await.unwrap().is_empty());

        let plan = vec![
            sample_entry("A", PlanStatus::Completed),
            sample_entry("B", PlanStatus::InProgress),
        ];
        store.set_plan("s1", plan.clone()).await;
        assert_eq!(store.get_plan("s1").await.unwrap(), plan);

        // Whole-list replace: empty list clears.
        store.set_plan("s1", vec![]).await;
        assert!(store.get_plan("s1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_plan_none_for_unknown_session() {
        let store = make_store(4);
        assert!(store.get_plan("does-not-exist").await.is_none());
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
        let generation = store.register_cancel_token("s1", token.clone());

        assert!(!token.is_cancelled());
        assert!(store.cancel_session("s1"));
        assert!(token.is_cancelled());

        // Second cancel returns false (token was consumed by remove).
        store.remove_cancel_token("s1", generation);
        assert!(!store.cancel_session("s1"));
    }

    #[tokio::test]
    async fn reregister_force_cancels_prior_turn() {
        let store = make_store(4);
        let old = tokio_util::sync::CancellationToken::new();
        let old_gen = store.register_cancel_token("s", old.clone());

        let new = tokio_util::sync::CancellationToken::new();
        let new_gen = store.register_cancel_token("s", new.clone());

        assert!(old.is_cancelled(), "re-register must kill the prior turn");
        assert!(!new.is_cancelled());
        assert_ne!(old_gen, new_gen);

        store.remove_cancel_token("s", old_gen);
        assert!(
            store.cancel_session("s"),
            "stale-generation remove must not orphan the live turn's token"
        );
        assert!(new.is_cancelled());
    }

    #[tokio::test]
    async fn stale_remove_is_a_noop() {
        let store = make_store(4);
        let token = tokio_util::sync::CancellationToken::new();
        let generation = store.register_cancel_token("s", token.clone());
        store.remove_cancel_token("s", generation.wrapping_sub(1));
        assert!(
            store.cancel_session("s"),
            "a remove with a non-matching generation must leave the token intact"
        );
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

    #[tokio::test]
    async fn session_persists_after_transport_disconnect() {
        let store = make_store(4);
        store
            .insert(
                "s1".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat)
                    .with_owner(Some("tui-x".to_string())),
            )
            .await
            .unwrap();
        // Simulate transport disconnect — store must still hold the session.
        assert_eq!(
            store.count().await,
            1,
            "session must survive transport disconnect"
        );
    }

    #[tokio::test]
    async fn kill_session_cancels_inflight_and_removes() {
        let store = make_store(4);
        store
            .insert(
                "live".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        store.register_cancel_token("live", token.clone());

        let removed = store.kill_session("live").await;
        assert!(removed, "kill_session must return true for a real session");
        assert!(
            token.is_cancelled(),
            "kill_session must fire the cancel token"
        );
        assert_eq!(store.count().await, 0, "session must be removed");
    }

    #[tokio::test]
    async fn kill_session_idle_session_removed() {
        let store = make_store(4);
        store
            .insert(
                "cold".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        // No cancel token registered.
        let removed = store.kill_session("cold").await;
        assert!(removed);
        assert_eq!(store.count().await, 0);
    }

    #[tokio::test]
    async fn kill_session_missing_returns_false() {
        let store = make_store(4);
        assert!(!store.kill_session("ghost").await);
    }

    #[tokio::test]
    async fn kill_session_cause_is_admin_kill() {
        let store = make_store(4);
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        store.register_cancel_token("s", token.clone());

        store.kill_session("s").await;
        // The verdict site must see AdminKill, not None.
        assert_eq!(
            store.take_cancel_cause("s"),
            Some(CancelCause::AdminKill),
            "kill_session must preserve AdminKill cause for verdict-site attribution"
        );
    }

    // ── generation-gated stale-refresh regression tests ──

    use crate::agent::dispatcher::NativeToolDispatcher;

    #[tokio::test]
    async fn insert_stamps_monotonic_generation() {
        let store = make_store(4);
        store
            .insert(
                "a".into(),
                RpcSession::new(make_agent(), "x", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        store
            .insert(
                "b".into(),
                RpcSession::new(make_agent(), "x", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let g_a = store.get_generation("a").await.unwrap();
        let g_b = store.get_generation("b").await.unwrap();
        assert_ne!(g_a, g_b, "each insert must stamp a unique generation");

        // Replace "a" — the successor must get a new generation.
        store
            .insert(
                "a".into(),
                RpcSession::new(make_agent(), "x", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let g_a2 = store.get_generation("a").await.unwrap();
        assert_ne!(
            g_a, g_a2,
            "replacing a same-ID session must bump the generation"
        );
    }

    #[tokio::test]
    async fn set_overrides_gated_rejects_stale_generation() {
        let store = make_store(4);
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let captured_gen = store.get_generation("s").await.unwrap();

        // Replace the session so generation advances.
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        // Stale generation must be rejected — successor untouched.
        let result = store
            .set_overrides_gated(
                "s",
                captured_gen,
                SessionOverrides {
                    model: Some("intruder".into()),
                    ..Default::default()
                },
            )
            .await;
        assert!(result.is_none(), "stale generation must be rejected");

        // Successor's overrides must remain default.
        let overrides = store.get_overrides("s").await.unwrap();
        assert_eq!(overrides.model, None, "successor model must be untouched");
        assert_eq!(
            overrides.temperature, None,
            "successor temperature must be untouched"
        );
    }

    #[tokio::test]
    async fn set_overrides_gated_accepts_current_generation() {
        let store = make_store(4);
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let captured_gen = store.get_generation("s").await.unwrap();

        let result = store
            .set_overrides_gated(
                "s",
                captured_gen,
                SessionOverrides {
                    model: Some("valid".into()),
                    temperature: Some(0.7),
                    ..Default::default()
                },
            )
            .await;
        assert!(result.is_some(), "current generation must be accepted");
        let overrides = result.unwrap();
        assert_eq!(overrides.model.as_deref(), Some("valid"));
        assert_eq!(overrides.temperature, Some(0.7));
    }

    #[tokio::test]
    async fn apply_model_provider_rejects_stale_generation() {
        let store = make_store(4);
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let captured_gen = store.get_generation("s").await.unwrap();

        // Replace the session so generation advances.
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();

        let applied = store
            .apply_model_provider(
                "s",
                captured_gen,
                Box::new(StubProvider),
                "stale-provider".into(),
                "stale-model".into(),
                Box::new(NativeToolDispatcher),
                Some(Some(0.99)),
            )
            .await;
        assert!(
            !applied,
            "stale generation must be rejected by apply_model_provider"
        );

        // Successor must retain the default agent identity — the stale
        // provider refresh must not have touched it.
        let agent = store.get_agent("s").await.unwrap();
        let (_, provider_name, model_name) = agent.lock().await.attribution_fields();
        assert_eq!(
            provider_name, "<unconfigured>",
            "successor provider name untouched"
        );
        assert_eq!(model_name, "<unconfigured>", "successor model untouched");
        assert_eq!(
            agent.lock().await.temperature_for_test(),
            None,
            "successor temperature must not be overwritten by stale refresh"
        );
    }

    #[tokio::test]
    async fn apply_model_provider_applies_temperature_to_captured_agent() {
        let store = make_store(4);
        store
            .insert(
                "s".into(),
                RpcSession::new(make_agent(), "a", ".", crate::rpc::types::ChatMode::Chat),
            )
            .await
            .unwrap();
        let captured_gen = store.get_generation("s").await.unwrap();

        let applied = store
            .apply_model_provider(
                "s",
                captured_gen,
                Box::new(StubProvider),
                "new-provider".into(),
                "new-model".into(),
                Box::new(NativeToolDispatcher),
                Some(Some(0.42)),
            )
            .await;
        assert!(applied, "current generation must be accepted");

        let agent = store.get_agent("s").await.unwrap();
        let guard = agent.lock().await;
        let (_, provider_name, model_name) = guard.attribution_fields();
        assert_eq!(provider_name, "new-provider");
        assert_eq!(model_name, "new-model");
        assert_eq!(
            guard.temperature_for_test(),
            Some(0.42),
            "temperature must be set on the captured agent under the generation check"
        );
    }
}
