use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use pulldown_cmark::{Event as MdEvent, Options as MdOptions, Parser as MdParser, Tag, TagEnd};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use tokio::sync::{broadcast, mpsc};

use crate::attachment::build_attachments_json;
use crate::client::{
    ApprovalDecision, RpcClient, RpcNotification, SessionEntry, SessionUpdate, TurnEndOutcome,
    method, parse_session_update,
};
use crate::diff;
use crate::file_explorer::{ExplorerAction, FileExplorerState};
use crate::input_bar::{InputBarAction, InputBarState};
use crate::jsonrpc::RpcOutbound;
use crate::mouse;
use crate::theme;
use crate::turn_status::TurnStatus;

// Height of the approval popup anchored to the bottom of the content area.
// Used both in render_approval_overlay and to pad diffs so they aren't covered.
const APPROVAL_OVERLAY_HEIGHT: u16 = 7;

/// How often the cwd line re-polls the daemon for the current git branch.
const GIT_BRANCH_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

// ── Chat pane (tab mode) ─────────────────────────────────────────

enum ChatPhase {
    /// Showing agent picker (or loading the list).
    PickAgent {
        agents: Vec<String>,
        list_state: ListState,
        loading: bool,
    },
    /// WSS only: user picks the remote working directory before session starts.
    PickCwd {
        /// The agent alias already chosen.
        agent_alias: String,
        /// Interactive directory picker.
        explorer: FileExplorerState,
    },
    /// Active chat session.
    Active(Box<ChatState>),
    /// Unrecoverable error.
    Error(String),
}

/// Distinguishes which kind of chat pane this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneKind {
    Chat,
    Acp,
}

impl PaneKind {
    /// Short name for this pane (no padding — callers format as needed).
    pub(crate) fn name(self) -> String {
        crate::i18n::t(self.fluent_key())
    }

    /// Stable Fluent key for this pane's display name.
    pub(crate) fn fluent_key(self) -> &'static str {
        match self {
            PaneKind::Chat => "zc-chat-pane-chat",
            PaneKind::Acp => "zc-chat-pane-acp",
        }
    }
}

pub(crate) struct Chat {
    rpc: Arc<RpcClient>,
    rpc_out: Arc<RpcOutbound>,
    notif_rx: broadcast::Receiver<RpcNotification>,
    /// Background-fetched git status updates: (session_id, branch, hash).
    git_branch_tx: mpsc::Sender<GitStatusUpdate>,
    git_branch_rx: mpsc::Receiver<GitStatusUpdate>,
    /// In-flight git_branch refresh; gates repeat fetches until result arrives.
    git_branch_inflight: bool,
    phase: ChatPhase,
    pane_kind: PaneKind,
    /// One-shot session id to reattach to on the next session start, set by
    /// the app layer across a reconnect so the rebuilt pane resumes the
    /// pre-disconnect session (the daemon retains it, #7182) instead of
    /// minting a fresh one. Cleared once consumed by `start_session`.
    resume_session_id: Option<String>,
    /// The agent the resumed session belongs to. A multi-agent reconnect must
    /// reattach to this agent automatically; the resume id is only dropped when
    /// the user manually picks a different agent.
    resume_agent_alias: Option<String>,
    /// List rect of the agent picker, recorded each draw so mouse clicks in the
    /// PickAgent phase can map a row to a selection. Default until first draw.
    pick_agent_list_area: Rect,
    /// Double-click tracker for the agent picker: a second click on the same row
    /// confirms (enters the session), matching the keyboard Enter.
    pick_agent_double_click: crate::mouse::DoubleClickTracker,
}

/// Result of one background `session/git_branch` poll, routed back to the UI
/// thread over `git_branch_tx`.
struct GitStatusUpdate {
    session_id: String,
    branch: Option<String>,
    hash: Option<String>,
}

fn should_retry_on_entry(phase: &ChatPhase) -> bool {
    matches!(phase, ChatPhase::Error(_))
}

impl Chat {
    pub(crate) fn new(rpc: Arc<RpcClient>, pane_kind: PaneKind) -> Self {
        let (git_branch_tx, git_branch_rx) = mpsc::channel(4);
        Self {
            rpc: rpc.clone(),
            rpc_out: rpc.rpc.clone(),
            notif_rx: rpc.subscribe_notifications(),
            git_branch_tx,
            git_branch_rx,
            git_branch_inflight: false,
            phase: ChatPhase::PickAgent {
                agents: Vec::new(),
                list_state: ListState::default(),
                loading: true,
            },
            pane_kind,
            resume_session_id: None,
            resume_agent_alias: None,
            pick_agent_list_area: Rect::default(),
            pick_agent_double_click: crate::mouse::DoubleClickTracker::new(),
        }
    }

    /// Seed a session id to reattach to on the next session start. Used by the
    /// app layer right before `init()` on a reconnect rebuild so the new pane
    /// resumes the prior session rather than starting a new one. One-shot:
    /// consumed by the first `start_session`.
    pub(crate) fn set_resume_session_id(&mut self, sid: Option<String>) {
        self.resume_session_id = sid;
    }

    /// Seed the agent the resumed session belongs to so a multi-agent reconnect
    /// can reattach automatically instead of dropping the carried session.
    pub(crate) fn set_resume_agent_alias(&mut self, alias: Option<String>) {
        self.resume_agent_alias = alias;
    }

    /// The active session id, if a session is live. Read by the app layer
    /// before a reconnect rebuild to carry the session across.
    pub(crate) fn current_session_id(&self) -> Option<&str> {
        match &self.phase {
            ChatPhase::Active(state) => Some(state.session_id.as_str()),
            _ => None,
        }
    }

    /// The active session's agent alias, if live. Read by the app layer before a
    /// reconnect rebuild so the resumed session reattaches to its own agent.
    pub(crate) fn current_agent_alias(&self) -> Option<&str> {
        match &self.phase {
            ChatPhase::Active(state) => Some(state.agent_alias.as_str()),
            _ => None,
        }
    }

    /// Fetch agent list. If exactly one enabled agent, auto-start a session (or
    /// show the CWD picker first on WSS ACP connections).
    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        let agents = match self.rpc.agents_status().await {
            Ok(result) => result
                .agents
                .into_iter()
                .filter(|a| a.enabled)
                .map(|a| a.alias)
                .collect::<Vec<_>>(),
            Err(e) => {
                self.phase = ChatPhase::Error(crate::i18n::t_args(
                    "zc-chat-error-fetch-agents",
                    &[("error", &e.to_string())],
                ));
                return Ok(());
            }
        };

        if agents.is_empty() {
            self.phase = ChatPhase::Error(crate::i18n::t("zc-chat-no-agents"));
            return Ok(());
        }

        if agents.len() == 1 {
            self.pick_or_start_session(&agents[0]).await;
            return Ok(());
        }

        // Multi-agent reconnect: if a resumed session was carried across the
        // rebuild and its agent is still present, reattach to it automatically
        // rather than forcing the user back through the picker and minting a
        // fresh session. The resume id is consumed by `start_session`.
        if let Some(prior) = self.resume_agent_alias.take()
            && self.resume_session_id.is_some()
            && agents.iter().any(|a| a == &prior)
        {
            self.pick_or_start_session(&prior).await;
            return Ok(());
        }

        let mut list_state = ListState::default();
        list_state.select(Some(0));
        // No carried session matched: a manual pick of a different agent must
        // not bleed a stale resume id into a mismatched agent's session.
        self.resume_session_id = None;
        self.resume_agent_alias = None;
        self.phase = ChatPhase::PickAgent {
            agents,
            list_state,
            loading: false,
        };
        Ok(())
    }

    /// Decide whether to show the CWD picker (WSS ACP) or start the session
    /// immediately (Unix, or non-ACP pane).
    async fn pick_or_start_session(&mut self, agent_alias: &str) {
        // A carried resume id means we are reattaching a daemon-retained session
        // across a reconnect: it already has a cwd, so skip the picker and
        // resume directly instead of forcing the user to re-pick a directory.
        if self.resume_session_id.is_some() {
            self.start_session(agent_alias, None).await;
            return;
        }
        if self.pane_kind == PaneKind::Acp && self.rpc.transport() == crate::client::Transport::Wss
        {
            // Remote ACP: start from the daemon root, not a local path.
            let start_dir = std::path::PathBuf::from("/");
            self.phase = ChatPhase::PickCwd {
                agent_alias: agent_alias.to_string(),
                explorer: FileExplorerState::new_dir_picker_remote(
                    start_dir,
                    Arc::clone(&self.rpc),
                ),
            };
        } else {
            self.start_session(agent_alias, None).await;
        }
    }

    /// Public entry point for "start a session against this specific
    /// agent." Used by the Quickstart pane on Stage 2 to route the
    /// user into the freshly-created agent's chat.
    pub(crate) async fn focus_agent(&mut self, agent_alias: &str) {
        self.pick_or_start_session(agent_alias).await;
    }

    /// Re-check stale setup errors when the user returns to a chat-style pane.
    ///
    /// Manual setup can happen in Config while Chat is parked on a stale
    /// "no agents" error. Quickstart uses `focus_agent()` directly after
    /// creation, but manual Config setup needs this small refresh hook.
    pub(crate) async fn refresh_if_inactive(&mut self) {
        if should_retry_on_entry(&self.phase) {
            let _ = self.init().await;
        }
    }

    /// Start the session, optionally with a caller-supplied `cwd`.
    ///
    /// - Resume (carried session id): never overrides cwd; the daemon keeps the
    ///   retained session's own working directory.
    /// - Unix: always passes the local CWD (ignores `cwd_override`).
    /// - WSS: passes `cwd_override` if provided, otherwise `None`.
    async fn start_session(&mut self, agent_alias: &str, cwd_override: Option<&str>) {
        // Reattach to a carried-over session on reconnect (one-shot); else a
        // fresh session. `session_new_with_id`/`_acp` with Some(id) restores
        // the daemon-retained session, its persisted history, and its cwd.
        let resume = self.resume_session_id.take();
        // A resume must not re-point the session at the TUI's launch directory:
        // pass no cwd so the daemon keeps the retained session's own cwd. Only
        // a fresh session derives a cwd from the transport / caller.
        let cwd_str: Option<String> = if resume.is_some() {
            None
        } else if self.rpc.transport() == crate::client::Transport::Local {
            // Over Unix socket, pass local CWD so the agent works in the
            // directory the TUI was launched from.
            std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(str::to_string))
        } else {
            // Over WSS the server uses the agent's workspace dir unless the
            // user supplies one.
            cwd_override
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
        };
        let result = if self.pane_kind == PaneKind::Acp {
            self.rpc
                .session_new_acp(agent_alias, cwd_str.as_deref(), resume.as_deref())
                .await
        } else {
            self.rpc
                .session_new_with_id(agent_alias, cwd_str.as_deref(), resume.as_deref())
                .await
        };
        match result {
            Ok(session) => {
                let resumed_sid = resume.as_deref().map(|_| session.session_id.clone());
                let mut state = ChatState::new(session.session_id, agent_alias.to_string());
                // Only ACP shows the working directory above the input bar.
                if self.pane_kind == PaneKind::Acp {
                    state.cwd = session.workspace_dir;
                }
                // On a resume, replay the daemon-retained transcript so the
                // reattached pane shows the prior conversation rather than an
                // empty history. Fresh sessions have nothing to load.
                if let Some(sid) = resumed_sid
                    && let Ok(msgs) = self.rpc.session_messages(&sid).await
                {
                    state.load_history(msgs.messages);
                }
                self.phase = ChatPhase::Active(Box::new(state));
            }
            Err(e) => {
                self.phase = ChatPhase::Error(crate::i18n::t_args(
                    "zc-chat-error-create-session",
                    &[("error", &e.to_string())],
                ));
            }
        }
    }

    // ── Drain channels (called from draw) ────────────────────────

    fn drain_notifications(&mut self) {
        loop {
            match self.notif_rx.try_recv() {
                Ok(notif) if notif.method == "session/update" => {
                    if let ChatPhase::Active(ref mut state) = self.phase
                        && let Some(update) = parse_session_update(&notif.params)
                    {
                        state.apply_update(update);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                _ => break,
            }
        }
    }

    fn drain_git_branch_results(&mut self) {
        while let Ok(update) = self.git_branch_rx.try_recv() {
            self.git_branch_inflight = false;
            if let ChatPhase::Active(ref mut state) = self.phase
                && state.session_id == update.session_id
            {
                state.git_branch = update.branch;
                state.git_hash = update.hash;
                state.git_branch_last_fetch = Some(Instant::now());
            }
        }
    }

    /// Spawn a background `session/git_branch` poll when the cache is stale.
    /// Gated by `git_branch_inflight` so we never have more than one fetch
    /// outstanding per Chat — the daemon walks the filesystem each call and
    /// the user only sees one result at a time anyway.
    fn maybe_refresh_git_branch(&mut self) {
        if self.git_branch_inflight {
            return;
        }
        let ChatPhase::Active(ref state) = self.phase else {
            return;
        };
        if state.cwd.is_none() {
            return;
        }
        let due = state
            .git_branch_last_fetch
            .is_none_or(|t| t.elapsed() >= GIT_BRANCH_REFRESH_INTERVAL);
        if !due {
            return;
        }
        self.git_branch_inflight = true;
        let sid = state.session_id.clone();
        let rpc = self.rpc.clone();
        let tx = self.git_branch_tx.clone();
        tokio::spawn(async move {
            let result = rpc.session_git_branch(&sid).await.ok();
            let (branch, hash) = match result {
                Some(r) => (r.branch, r.hash),
                None => (None, None),
            };
            let _ = tx
                .send(GitStatusUpdate {
                    session_id: sid,
                    branch,
                    hash,
                })
                .await;
        });
    }

    // ── Drawing ──────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        self.drain_notifications();
        self.drain_git_branch_results();
        self.maybe_refresh_git_branch();

        match &mut self.phase {
            ChatPhase::PickAgent {
                agents,
                list_state,
                loading,
            } => {
                let list_area = draw_agent_picker(
                    frame,
                    area,
                    agents,
                    list_state,
                    *loading,
                    &self.pane_kind.name(),
                );
                self.pick_agent_list_area = list_area;
            }
            ChatPhase::PickCwd { explorer, .. } => {
                explorer.render(frame, area);
            }
            ChatPhase::Active(state) => {
                render(frame, state, area);
            }
            ChatPhase::Error(msg) => {
                draw_error(frame, area, msg, &self.pane_kind.name());
            }
        }
    }

    // ── Key handling ─────────────────────────────────────────────

    pub(crate) async fn handle_key(
        &mut self,
        key: KeyEvent,
        term: &mut crate::config_manager::Term,
    ) -> bool {
        // Determine which phase we're in without holding a borrow on self.
        // For the picker, extract what we need; for active, delegate below.
        match &mut self.phase {
            ChatPhase::PickAgent {
                agents,
                list_state,
                loading,
            } => {
                if *loading {
                    return false;
                }
                use crate::keymap::{ChatTabAction, GlobalAction, ModalAction};
                // Three action types in scope here — explicit short-circuit
                // chain instead of one mixed match.
                match ModalAction::from_chord(&key) {
                    Some(ModalAction::Confirm) => {
                        if let Some(i) = list_state.selected()
                            && let Some(alias) = agents.get(i).cloned()
                        {
                            self.pick_or_start_session(&alias).await;
                        }
                        return false;
                    }
                    Some(ModalAction::Cancel) => return true,
                    _ => {}
                }
                if GlobalAction::from_chord(&key) == Some(GlobalAction::Quit) {
                    return true;
                }
                match ChatTabAction::from_chord(&key) {
                    Some(ChatTabAction::BrowseUp) | Some(ChatTabAction::BrowseUpVim) => {
                        let i = list_state.selected().unwrap_or(0);
                        list_state.select(Some(i.saturating_sub(1)));
                    }
                    Some(ChatTabAction::BrowseDown) | Some(ChatTabAction::BrowseDownVim) => {
                        let i = list_state.selected().unwrap_or(0);
                        if i + 1 < agents.len() {
                            list_state.select(Some(i + 1));
                        }
                    }
                    _ => {}
                }
                return false;
            }
            ChatPhase::PickCwd {
                agent_alias,
                explorer,
            } => {
                let action = explorer.handle_key(key);
                match action {
                    ExplorerAction::ConfirmDir(path) => {
                        let alias = agent_alias.clone();
                        let cwd_str = path.to_str().map(str::to_string);
                        self.start_session(&alias, cwd_str.as_deref()).await;
                    }
                    ExplorerAction::Cancel => {
                        self.phase = ChatPhase::PickAgent {
                            agents: Vec::new(),
                            list_state: ListState::default(),
                            loading: true,
                        };
                        // Re-fetch agents asynchronously.
                        let _ = self.init().await;
                    }
                    ExplorerAction::Confirm(_) | ExplorerAction::None => {}
                }
                return false;
            }
            ChatPhase::Error(_) => {
                use crate::keymap::GlobalAction;
                return GlobalAction::from_chord(&key) == Some(GlobalAction::Quit)
                    || crate::keymap::Chord::char('q').matches(&key);
            }
            ChatPhase::Active(_) => { /* handled below to avoid borrow conflict */ }
        }

        // Active phase — borrow state directly to avoid double &mut self.
        let ChatPhase::Active(ref mut state) = self.phase else {
            return false;
        };

        // ── Session overlay key handling ─────────────────────────
        match &mut state.session_overlay {
            SessionOverlay::List {
                sessions,
                list_state,
            } => {
                use crate::keymap::{Chord, ModalAction};
                match ModalAction::from_chord(&key) {
                    Some(ModalAction::Cancel) => {
                        state.session_overlay = SessionOverlay::None;
                    }
                    Some(ModalAction::Confirm) => {
                        if let Some(i) = list_state.selected()
                            && let Some(s) = sessions.get(i)
                        {
                            let new_sid = s.session_id.clone();
                            let new_name = s.name.clone();
                            let agent_alias = s
                                .agent_alias
                                .clone()
                                .unwrap_or_else(|| state.agent_alias.clone());
                            let _ = self.rpc.session_close(&state.session_id).await;
                            state.session_overlay = SessionOverlay::None;
                            state.reset_for_session(new_sid.clone(), new_name);
                            state.agent_alias = agent_alias.clone();
                            // Rehydrate the session in the daemon so prompts work.
                            let rehydrate_result = if self.pane_kind == PaneKind::Acp {
                                self.rpc
                                    .session_new_acp(&agent_alias, None, Some(&new_sid))
                                    .await
                            } else {
                                self.rpc
                                    .session_new_with_id(&agent_alias, None, Some(&new_sid))
                                    .await
                            };
                            if let Ok(rehydrated) = rehydrate_result
                                && self.pane_kind == PaneKind::Acp
                            {
                                state.cwd = rehydrated.workspace_dir;
                            }
                            // Load persisted message history.
                            if let Ok(msgs) = self.rpc.session_messages(&new_sid).await {
                                state.load_history(msgs.messages);
                            }
                        }
                    }
                    _ => {
                        if Chord::key(crossterm::event::KeyCode::Up).matches(&key) {
                            let i = list_state.selected().unwrap_or(0);
                            list_state.select(Some(i.saturating_sub(1)));
                        } else if Chord::key(crossterm::event::KeyCode::Down).matches(&key) {
                            let i = list_state.selected().unwrap_or(0);
                            if i + 1 < sessions.len() {
                                list_state.select(Some(i + 1));
                            }
                        }
                    }
                }
                return false;
            }
            SessionOverlay::None => { /* handled below */ }
        }

        // ── Delegate to input bar first ─────────────────────────
        // The input bar handles: file explorer, Ctrl+A, Ctrl+V,
        // Enter (slash commands + submit), text input, cursor, backspace.
        // It does NOT handle approval, selection, session management, etc.
        if state.pending_approval().is_none() && !state.in_browse_mode() {
            let action = state.input_bar.handle_key(key, state.turn_in_flight);
            match action {
                InputBarAction::Submit { text, attachments } => {
                    let prompt = text.clone().unwrap_or_default();
                    let att_names: Vec<String> =
                        attachments.iter().map(|a| a.filename.clone()).collect();
                    state.push_user_message(text, att_names);
                    let sid = state.session_id.clone();
                    let rpc_arc = self.rpc_out.clone();
                    let transport = self.rpc.transport();
                    // Fire-and-forget. Turn end arrives via TurnComplete
                    // notification handled in apply_update.
                    tokio::spawn(async move {
                        let mut params = serde_json::json!({
                            "session_id": sid,
                            "prompt": prompt,
                        });
                        if !attachments.is_empty() {
                            match build_attachments_json(&attachments, transport) {
                                Ok(att_json) => {
                                    params["attachments"] = serde_json::Value::Array(att_json);
                                }
                                Err(_) => return,
                            }
                        }
                        rpc_arc.notify(method::SESSION_PROMPT, params).await;
                    });
                    return false;
                }
                InputBarAction::StatusMessage(msg) => {
                    state
                        .entries
                        .push(ChatEntry::SystemMessage(Arc::<str>::from(msg)));
                    state.mark_dirty_append();
                    return false;
                }
                InputBarAction::ToggleThinking => {
                    state.show_thoughts = !state.show_thoughts;
                    state.mark_dirty_full();
                    let status = if state.show_thoughts {
                        crate::i18n::t("zc-chat-thinking-visible")
                    } else {
                        crate::i18n::t("zc-chat-thinking-hidden")
                    };
                    state
                        .entries
                        .push(ChatEntry::SystemMessage(Arc::<str>::from(status)));
                    state.mark_dirty_append();
                    return false;
                }
                InputBarAction::Consumed => return false,
                InputBarAction::NotHandled => { /* fall through to chat-specific keys */ }
            }
        }

        // ── Chat-specific key handling ───────────────────────────
        use crate::keymap::{ChatTabAction, GlobalAction};
        // Quit chord wins (chat overrides conditionally on turn state below).
        if GlobalAction::from_chord(&key) == Some(GlobalAction::Quit) {
            if state.turn_in_flight {
                if !matches!(state.turn_status, TurnStatus::Cancelling) {
                    let _ = self.rpc.session_cancel(&state.session_id).await;
                    state.turn_status = TurnStatus::Cancelling;
                }
            } else {
                return true;
            }
            return false;
        }
        match ChatTabAction::from_chord(&key) {
            Some(ChatTabAction::BrowseExitSelection) => {
                if state.in_browse_mode() {
                    state.exit_browse_mode();
                } else if state.turn_in_flight
                    && !matches!(state.turn_status, TurnStatus::Cancelling)
                {
                    let _ = self.rpc.session_cancel(&state.session_id).await;
                    state.turn_status = TurnStatus::Cancelling;
                }
            }
            Some(ChatTabAction::ApprovalApprove) if state.pending_approval().is_some() => {
                if let Some(pa) = state.take_pending_approval() {
                    let _ = self
                        .rpc
                        .session_approve(
                            &state.session_id,
                            &pa.request_id,
                            ApprovalDecision::AllowOnce,
                        )
                        .await;
                }
            }
            Some(ChatTabAction::CancelTurn) if state.pending_approval().is_some() => {
                if let Some(pa) = state.take_pending_approval() {
                    let _ = self
                        .rpc
                        .session_approve(
                            &state.session_id,
                            &pa.request_id,
                            ApprovalDecision::Reject,
                        )
                        .await;
                }
            }
            Some(ChatTabAction::ApprovalApproveAll) if state.pending_approval().is_some() => {
                if let Some(pa) = state.take_pending_approval() {
                    let _ = self
                        .rpc
                        .session_approve(
                            &state.session_id,
                            &pa.request_id,
                            ApprovalDecision::AllowAlways,
                        )
                        .await;
                }
            }
            Some(ChatTabAction::ApprovalApproveEdit) if state.pending_approval().is_some() => {
                let is_edit_tool = state
                    .pending_approval()
                    .map(|pa| matches!(pa.tool_name.as_str(), "file_edit" | "file_write"))
                    .unwrap_or(false);
                if is_edit_tool && let Some(pa) = state.take_pending_approval() {
                    let initial = pa.arguments_summary.clone();
                    let edited = open_editor_for_content(&initial).await;
                    let _ = term.clear();
                    let _ = self
                        .rpc
                        .session_approve(
                            &state.session_id,
                            &pa.request_id,
                            ApprovalDecision::RejectWithEdit {
                                replacement: edited,
                            },
                        )
                        .await;
                }
            }
            Some(ChatTabAction::NewSession) if !state.turn_in_flight => {
                let alias = state.agent_alias.clone();
                if self.pane_kind == PaneKind::Acp
                    && self.rpc.transport() == crate::client::Transport::Wss
                {
                    // For WSS ACP, go through the CWD picker for new sessions too.
                    let _ = self.rpc.session_close(&state.session_id).await;
                    // Remote ACP picker must start from a path the daemon understands.
                    let start_dir = std::path::PathBuf::from("/");
                    self.phase = ChatPhase::PickCwd {
                        agent_alias: alias,
                        explorer: FileExplorerState::new_dir_picker_remote(
                            start_dir,
                            Arc::clone(&self.rpc),
                        ),
                    };
                } else {
                    let local_cwd = if self.rpc.transport() == crate::client::Transport::Local {
                        std::env::current_dir().ok()
                    } else {
                        None
                    };
                    let cwd_str = local_cwd.as_deref().and_then(|p| p.to_str());
                    let new_session = if self.pane_kind == PaneKind::Acp {
                        self.rpc.session_new_acp(&alias, cwd_str, None).await
                    } else {
                        self.rpc.session_new(&alias, cwd_str).await
                    };
                    if let Ok(s) = new_session {
                        let _ = self.rpc.session_close(&state.session_id).await;
                        state.reset_for_session(s.session_id, None);
                        if self.pane_kind == PaneKind::Acp {
                            state.cwd = s.workspace_dir;
                        }
                    }
                }
            }
            Some(ChatTabAction::SwitchSession) if !state.turn_in_flight => {
                // ACP and Chat live in separate stores and must not cross-pick:
                //  • Chat → unified session_backend (filter out channel-backed
                //    sessions; those are owned by the channels pane).
                //  • ACP  → dedicated acp-sessions.db, listed by a separate RPC.
                let picker_sessions = if self.pane_kind == PaneKind::Acp {
                    self.rpc
                        .acp_session_list()
                        .await
                        .map(|list| list.sessions)
                        .unwrap_or_default()
                } else {
                    match self.rpc.session_list(None).await {
                        Ok(list) => list
                            .sessions
                            .into_iter()
                            .filter(|s| s.channel_id.is_none())
                            .collect(),
                        Err(_) => Vec::new(),
                    }
                };

                let mut ls = ListState::default();
                if !picker_sessions.is_empty() {
                    ls.select(Some(0));
                }
                state.session_overlay = SessionOverlay::List {
                    sessions: picker_sessions,
                    list_state: ls,
                };
            }
            Some(ChatTabAction::ToggleThoughts)
                if state.input_bar.input().is_empty()
                    && state.pending_approval().is_none()
                    && !state.in_browse_mode() =>
            {
                state.show_thoughts = !state.show_thoughts;
                state.mark_dirty_full();
            }
            Some(ChatTabAction::BrowseEnter) => {
                if state.in_browse_mode() {
                    state.browse_move_up(1, false);
                } else {
                    state.enter_browse_mode();
                }
            }
            Some(ChatTabAction::BrowseExit) if state.in_browse_mode() => {
                state.exit_browse_mode();
            }
            Some(ChatTabAction::BrowseUp) => {
                if state.in_browse_mode() {
                    state.browse_move_up(1, false);
                } else if !state.pinned_to_bottom {
                    state.scroll_up(1);
                }
            }
            Some(ChatTabAction::BrowseDown) => {
                if state.in_browse_mode() {
                    state.browse_move_down(1, false);
                } else if !state.pinned_to_bottom {
                    state.scroll_down(1);
                }
            }
            Some(ChatTabAction::BrowseSelectExtend) => {
                if state.in_browse_mode() {
                    state.browse_move_up(1, true);
                } else {
                    state.scroll_up(1);
                }
            }
            Some(ChatTabAction::BrowseSelectExtendDown) => {
                if state.in_browse_mode() {
                    state.browse_move_down(1, true);
                } else {
                    state.scroll_down(1);
                }
            }
            Some(ChatTabAction::FastScrollUp) => {
                state.scroll_up(5);
            }
            Some(ChatTabAction::FastScrollDown) => {
                state.scroll_down(5);
            }
            Some(ChatTabAction::BrowseUpVim)
                if state.in_browse_mode()
                    && state.pending_approval().is_none()
                    && !state.turn_in_flight =>
            {
                state.browse_move_up(1, false);
            }
            Some(ChatTabAction::BrowseDownVim)
                if state.in_browse_mode()
                    && state.pending_approval().is_none()
                    && !state.turn_in_flight =>
            {
                state.browse_move_down(1, false);
            }
            Some(ChatTabAction::CopySelection) if state.has_selection() => {
                let text = state.yank_selection();
                if !text.is_empty() {
                    crate::mouse::copy_osc52(&text);
                }
            }
            Some(ChatTabAction::CopyAllVisible) if state.has_selection() => {
                let text = state.yank_selection();
                if !text.is_empty() {
                    crate::mouse::copy_osc52(&text);
                }
            }
            _ => {}
        }
        false
    }

    pub(crate) async fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        // Dir-picker explorer handles its own mouse events.
        if let ChatPhase::PickCwd { explorer, .. } = &mut self.phase {
            explorer.handle_mouse(mouse);
            return;
        }

        // Agent picker: click highlights a row, double-click confirms (enters
        // the session), wheel moves the selection.
        if matches!(self.phase, ChatPhase::PickAgent { loading: false, .. }) {
            let mut confirm_alias: Option<String> = None;
            if let ChatPhase::PickAgent {
                agents, list_state, ..
            } = &mut self.phase
            {
                let list_area = self.pick_agent_list_area;
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(idx) = mouse::list_click_index(
                            mouse.row,
                            list_area,
                            list_state.offset(),
                            agents.len(),
                        ) {
                            list_state.select(Some(idx));
                            if self.pick_agent_double_click.click(mouse.column, mouse.row) {
                                confirm_alias = agents.get(idx).cloned();
                            }
                        }
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                        let i = list_state.selected().unwrap_or(0);
                        list_state.select(Some(mouse::list_scroll(i, agents.len(), up, 1)));
                    }
                    _ => {}
                }
            }
            if let Some(alias) = confirm_alias {
                self.pick_or_start_session(&alias).await;
            }
            return;
        }

        if let ChatPhase::Active(ref mut state) = self.phase {
            // Let the file explorer handle mouse events first when open.
            if state.input_bar.handle_mouse(mouse) {
                return;
            }

            // Session list overlay intercepts all mouse events when open.
            if let SessionOverlay::List {
                sessions,
                list_state,
            } = &mut state.session_overlay
            {
                let col = mouse.column;
                let row = mouse.row;
                let overlay_area = session_list_overlay_area(area);

                match mouse.kind {
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        if !mouse::in_rect(col, row, overlay_area) {
                            // Click outside → close overlay.
                            state.session_overlay = SessionOverlay::None;
                        } else {
                            let count = sessions.len();
                            if let Some(idx) = mouse::list_click_index(
                                row,
                                overlay_area,
                                list_state.offset(),
                                count,
                            ) {
                                list_state.select(Some(idx));
                            }
                        }
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                        if mouse::in_rect(col, row, overlay_area) =>
                    {
                        let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                        let count = sessions.len();
                        let i = list_state.selected().unwrap_or(0);
                        list_state.select(Some(mouse::list_scroll(i, count, up, 1)));
                    }
                    _ => {}
                }
                return;
            }

            use crossterm::event::KeyModifiers as KM;
            let col = mouse.column;
            let row = mouse.row;
            match mouse.kind {
                MouseEventKind::ScrollUp => state.scroll_up(3),
                MouseEventKind::ScrollDown => state.scroll_down(3),
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(track) = state.scrollbar_track_rect
                        && mouse::in_rect(col, row, track)
                    {
                        state.scrollbar_drag = Some(ScrollbarDrag {
                            start_scroll: state.scroll_offset,
                            start_row: row,
                        });
                        let max = state
                            .last_total_rows
                            .saturating_sub(state.last_inner_height);
                        if track.height > 0 {
                            let rel = row.saturating_sub(track.y) as u32;
                            let new_off = (rel * max as u32 / track.height.max(1) as u32) as u16;
                            state.scroll_offset = new_off.min(max);
                            state.pinned_to_bottom = state.scroll_offset >= max;
                        }
                        return;
                    }
                    let hit = state
                        .entry_rects
                        .iter()
                        .find(|(_, r)| mouse::in_rect(col, row, *r))
                        .map(|(idx, _)| *idx);
                    let shift = mouse.modifiers.contains(KM::SHIFT);
                    let ctrl = mouse.modifiers.contains(KM::CONTROL);
                    if let Some(idx) = hit {
                        if ctrl {
                            if !state.browse_multi.remove(&idx) {
                                state.browse_multi.insert(idx);
                            }
                            state.mark_dirty_full();
                        } else if shift {
                            if state.browse_cursor.is_none() {
                                state.browse_cursor = Some(idx);
                            }
                            state.browse_anchor = state.browse_cursor;
                            state.browse_cursor = Some(idx);
                            state.mark_dirty_full();
                        } else {
                            state.browse_multi.clear();
                            state.browse_cursor = Some(idx);
                            state.browse_anchor = None;
                            state.mark_dirty_full();
                        }
                    } else {
                        state.browse_multi.clear();
                        state.browse_cursor = None;
                        state.browse_anchor = None;
                        state.mark_dirty_full();
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(drag) = state.scrollbar_drag {
                        let max = state
                            .last_total_rows
                            .saturating_sub(state.last_inner_height);
                        let track_h = state
                            .scrollbar_track_rect
                            .map(|r| r.height)
                            .unwrap_or(0)
                            .max(1);
                        let dy = row as i32 - drag.start_row as i32;
                        let scroll_delta = dy * max as i32 / track_h as i32;
                        let new_off =
                            (drag.start_scroll as i32 + scroll_delta).clamp(0, max as i32);
                        state.scroll_offset = new_off as u16;
                        state.pinned_to_bottom = state.scroll_offset >= max;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    state.scrollbar_drag = None;
                }
                _ => {}
            }
        }
    }

    /// Handle a bracketed paste event.
    pub(crate) fn handle_paste(&mut self, text: &str) {
        let ChatPhase::Active(state) = &mut self.phase else {
            return;
        };
        if state.turn_in_flight {
            return;
        }
        let action = state.input_bar.handle_paste(text);
        if let InputBarAction::StatusMessage(msg) = action {
            state
                .entries
                .push(ChatEntry::SystemMessage(Arc::<str>::from(msg)));
            state.mark_dirty_append();
        }
    }

    /// Returns true when the pane is accepting text input (blocks `?` help).
    ///
    /// In active chat: text input mode is on when the user has started typing
    /// (non-empty input buffer) and is not in selection mode or an overlay.
    /// When input is empty we're in "command" mode — single-char keybindings
    /// like `t`, `j`, `k`, `y`, `?` should work.
    /// Return the current context token counts for the status bar.
    pub(crate) fn ctx_tokens(&self) -> (Option<u64>, Option<u64>) {
        match &self.phase {
            ChatPhase::Active(s) => (s.context_input_tokens, s.context_max_tokens),
            _ => (None, None),
        }
    }

    /// The agent alias this pane is currently focused on, if any. Used to
    /// resolve a per-agent theme override while this pane is active. Returns
    /// `None` in the agent-picker phase, where no agent is yet chosen.
    pub(crate) fn selected_agent(&self) -> Option<&str> {
        match &self.phase {
            ChatPhase::Active(s) => Some(s.agent_alias.as_str()),
            ChatPhase::PickCwd { agent_alias, .. } => Some(agent_alias.as_str()),
            _ => None,
        }
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        match &self.phase {
            // CWD picker always captures text input.
            ChatPhase::PickCwd { .. } => true,
            ChatPhase::Active(s) => {
                if !matches!(s.session_overlay, SessionOverlay::None) {
                    return false;
                }
                // Browse mode: single-char bindings active.
                if s.in_browse_mode() {
                    return false;
                }
                // Command mode when input is empty; text mode when typing.
                s.input_bar.wants_text_input()
            }
            _ => false,
        }
    }
}

impl crate::widgets::HelpContext for Chat {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::ChatTabAction;
        use crate::widgets::{HelpEntry as E, HelpNode};
        match &self.phase {
            ChatPhase::PickAgent { loading, .. } => {
                if *loading {
                    HelpNode::entries(vec![E::key("", crate::i18n::t("zc-chat-loading-agents"))])
                } else {
                    HelpNode::entries(vec![
                        E::new(vec!["↑", "↓"], crate::i18n::t("zc-chat-help-navigate")),
                        E::key("Enter", crate::i18n::t("zc-chat-help-select-agent")),
                        E::key("q", crate::i18n::t("zc-chat-help-quit")),
                    ])
                }
            }
            ChatPhase::PickCwd { explorer, .. } => explorer.help_context(),
            ChatPhase::Error(_) => {
                HelpNode::entries(vec![E::key("q", crate::i18n::t("zc-chat-help-quit"))])
            }
            ChatPhase::Active(state) => {
                match &state.session_overlay {
                    SessionOverlay::List { .. } => {
                        return HelpNode::entries(vec![
                            E::new(vec!["↑", "↓"], crate::i18n::t("zc-chat-help-navigate")),
                            E::key("Enter", crate::i18n::t("zc-chat-help-switch-session")),
                            E::key("Esc", crate::i18n::t("zc-chat-help-close")),
                        ]);
                    }
                    SessionOverlay::None => {}
                }
                if state.pending_approval().is_some() {
                    return HelpNode::entries(vec![
                        E::key("Enter", crate::i18n::t("zc-chat-help-approve")),
                        E::key("a", crate::i18n::t("zc-chat-help-always-approve")),
                        E::key("Ctrl+D", crate::i18n::t("zc-chat-help-deny")),
                        E::key("Ctrl+C", crate::i18n::t("zc-chat-help-cancel-turn")),
                    ]);
                }
                if state.in_browse_mode() {
                    return HelpNode::entries(vec![
                        E::new(vec!["↑", "k"], crate::i18n::t("zc-chat-help-move-up")),
                        E::new(vec!["↓", "j"], crate::i18n::t("zc-chat-help-move-down")),
                        E::key("Shift+↑/↓", crate::i18n::t("zc-chat-help-extend-selection")),
                        E::key("y", crate::i18n::t("zc-chat-help-yank-selection")),
                        E::new(
                            vec!["Ctrl+↓", "Esc"],
                            crate::i18n::t("zc-chat-help-return-to-input"),
                        ),
                    ]);
                }
                if state.turn_in_flight {
                    return HelpNode::entries(vec![E::new(
                        vec!["Ctrl+C", "Esc"],
                        crate::i18n::t("zc-chat-help-cancel-turn"),
                    )]);
                }
                // Idle: compose pane-level bindings + input bar as child.
                let pane = HelpNode::entries(vec![
                    E::key("Ctrl+↑", crate::i18n::t("zc-chat-help-browse-mode")),
                    E::key(
                        "Shift+↑/↓",
                        crate::i18n::t("zc-chat-help-scroll-conversation"),
                    ),
                    E::key("t", crate::i18n::t("zc-chat-help-toggle-thoughts")),
                    E::key(
                        "/toggle-thinking",
                        crate::i18n::t("zc-chat-help-toggle-thinking-cmd"),
                    ),
                    E::spacer(),
                    E::key(
                        Box::leak(
                            ChatTabAction::NewSession.default_chords()[0]
                                .display()
                                .into_boxed_str(),
                        ),
                        crate::i18n::t("zc-chat-help-new-session"),
                    ),
                    E::key(
                        Box::leak(
                            ChatTabAction::SwitchSession.default_chords()[0]
                                .display()
                                .into_boxed_str(),
                        ),
                        crate::i18n::t("zc-chat-help-session-list"),
                    ),
                ]);
                pane.with_child(state.input_bar.help_context())
            }
        }
    }
}

// ── Agent picker rendering ───────────────────────────────────────

/// Build the agent-picker nav hint from the live keymap (browse up/down + the
/// modal confirm chord), never hardcoded literals.
fn picker_nav_keys() -> String {
    use crate::keymap::{ChatTabAction, Chord, ModalAction, RebindableActions};
    let mut parts: Vec<String> = Vec::new();
    let mut push = |c: &Chord| {
        let d = c.display();
        if !parts.contains(&d) {
            parts.push(d);
        }
    };
    for c in ChatTabAction::BrowseUp.resolved() {
        push(&c);
    }
    for c in ChatTabAction::BrowseDown.resolved() {
        push(&c);
    }
    for c in ModalAction::Confirm.resolved() {
        push(&c);
    }
    parts.join("/")
}

fn draw_agent_picker(
    frame: &mut Frame,
    area: Rect,
    agents: &[String],
    list_state: &mut ListState,
    loading: bool,
    tab_title: &str,
) -> Rect {
    let block = Block::default()
        .title(Span::styled(format!(" {tab_title} "), theme::title_style()))
        .borders(Borders::ALL)
        .border_style(theme::dim_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if loading {
        let p = Paragraph::new(crate::i18n::t("zc-chat-loading-agents-msg"))
            .alignment(Alignment::Center)
            .style(theme::dim_style());
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(inner);
        frame.render_widget(p, vert[1]);
        return Rect::default();
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{} ", crate::i18n::t("zc-chat-picker-header")),
            theme::body_style(),
        ),
        Span::styled(
            crate::i18n::t_args(
                "zc-chat-picker-header-hint",
                &[("keys", &picker_nav_keys())],
            ),
            theme::dim_style(),
        ),
    ]));
    frame.render_widget(header, chunks[0]);

    let items: Vec<ListItem> = agents
        .iter()
        .map(|a| ListItem::new(Span::styled(a.as_str(), theme::body_style())))
        .collect();
    let list = List::new(items).highlight_style(theme::list_highlight_style());
    frame.render_stateful_widget(list, chunks[1], list_state);
    // The list rect is unbordered, but `mouse::list_click_index` assumes a
    // 1-cell top border. Hand back a rect shifted up one row (and one taller) so
    // the helper's border compensation lands on the true first item.
    Rect::new(
        chunks[1].x,
        chunks[1].y.saturating_sub(1),
        chunks[1].width,
        chunks[1].height + 1,
    )
}

// ── Error rendering ──────────────────────────────────────────────

fn draw_error(frame: &mut Frame, area: Rect, msg: &str, tab_title: &str) {
    let block = Block::default()
        .title(Span::styled(format!(" {tab_title} "), theme::title_style()))
        .borders(Borders::ALL)
        .border_style(theme::dim_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);

    let p = Paragraph::new(Line::from(Span::styled(msg, theme::error_style())))
        .alignment(Alignment::Center);
    frame.render_widget(p, chunks[1]);
}

// ── Active chat rendering ────────────────────────────────────────

fn render(f: &mut Frame, state: &mut ChatState, area: Rect) {
    let show_cursor = state.pending_approval().is_none();
    let turn_status = state.turn_status.clone();
    let turn_started_at = state.turn_started_at;

    let _live_input_tokens = state.context_input_tokens;
    let input_area = area;

    let conv_area = state.input_bar.render(
        f,
        input_area,
        state.turn_in_flight,
        show_cursor,
        &turn_status,
        turn_started_at,
    );

    // Optional CWD line just above the input bar (bottom of conv_area).
    // Renders `<cwd> - (branch) (hash)`, all left-aligned; the branch and hash
    // segments are appended only when the daemon's git poll has resolved them.
    let actual_conv = if let Some(ref cwd) = state.cwd {
        if conv_area.height > 1 {
            let cwd_row = Rect::new(
                conv_area.x,
                conv_area.y + conv_area.height - 1,
                conv_area.width,
                1,
            );
            let mut line = format!(" {cwd}");
            if state.git_branch.is_some() || state.git_hash.is_some() {
                line.push_str(" -");
                if let Some(ref branch) = state.git_branch {
                    line.push_str(&format!(" ({branch})"));
                }
                if let Some(ref hash) = state.git_hash {
                    line.push_str(&format!(" ({hash})"));
                }
            }
            line.push(' ');
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(line, theme::dim_style())))
                    .alignment(Alignment::Left),
                cwd_row,
            );
            Rect::new(
                conv_area.x,
                conv_area.y,
                conv_area.width,
                conv_area.height - 1,
            )
        } else {
            conv_area
        }
    } else {
        conv_area
    };

    render_conversation(f, state, actual_conv);
    state.input_bar.render_autocomplete_popup(f);

    if state.pending_approval().is_some() {
        render_approval_overlay(f, state, area);
    }

    match &state.session_overlay {
        SessionOverlay::List {
            sessions,
            list_state,
        } => {
            render_session_list_overlay(f, area, sessions, list_state);
        }
        SessionOverlay::None => {}
    }

    state.input_bar.render_explorer_overlay(f, area);
}

/// Extract the file extension from the `"path"` field of a tool's input JSON.
fn file_ext(input: &serde_json::Value) -> Option<&str> {
    let path = input.get("path")?.as_str()?;
    std::path::Path::new(path).extension()?.to_str()
}

/// Return a prefix of `s` no longer than `max_bytes`, guaranteed to end on a
/// valid UTF-8 char boundary. Never panics on multi-byte characters.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn render_tool_entry(
    lines: &mut Vec<Line<'static>>,
    name: &str,
    input_json: &str,
    result: Option<&str>,
    is_selected: bool,
) {
    let sel_mod = if is_selected {
        Modifier::REVERSED
    } else {
        Modifier::empty()
    };
    lines.push(Line::from(vec![Span::styled(
        format!("[tool: {name}] "),
        theme::tool_label_style().add_modifier(sel_mod),
    )]));

    let parsed: Option<serde_json::Value> = match name {
        "file_edit" | "file_write" => serde_json::from_str(input_json).ok(),
        _ => None,
    };

    let body_start = lines.len();
    match name {
        "file_edit" => {
            let input = parsed.as_ref();
            let old = input
                .and_then(|v| v.get("old_string"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = input
                .and_then(|v| v.get("new_string"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = input.and_then(|v| v.get("path")).and_then(|v| v.as_str());
            let ext = input.and_then(|v| file_ext(v));
            let start_line = path
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|content| {
                    content
                        .find(old)
                        .map(|idx| content[..idx].bytes().filter(|b| *b == b'\n').count() + 1)
                })
                .unwrap_or(1);
            lines.extend(diff::diff_lines(old, new, ext, start_line));
        }
        "file_write" => {
            let input = parsed.as_ref();
            let content = input
                .and_then(|v| v.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let ext = input.and_then(|v| file_ext(v));
            lines.extend(diff::write_lines(content, ext));
        }
        _ => {
            let truncated = if input_json.len() > 120 {
                format!("{}…", truncate_utf8(input_json, 120))
            } else {
                input_json.to_string()
            };
            lines.push(Line::from(Span::styled(
                format!("  {truncated}"),
                theme::dim_style().add_modifier(sel_mod),
            )));
        }
    }

    if let Some(res) = result {
        let truncated = if res.len() > 200 {
            format!("{}…", truncate_utf8(res, 200))
        } else {
            res.to_string()
        };
        lines.push(Line::from(Span::styled(
            format!("  → {truncated}"),
            theme::dim_style().add_modifier(sel_mod),
        )));
    }

    // Apply REVERSED to body lines from diff_lines/write_lines too.
    if is_selected {
        for line in &mut lines[body_start..] {
            let spans = std::mem::take(&mut line.spans);
            line.spans = spans
                .into_iter()
                .map(|s| s.patch_style(Style::default().add_modifier(Modifier::REVERSED)))
                .collect();
        }
    }
}

/// Render a single committed entry into `lines`.
/// Extracted so both the incremental-append and full-rebuild paths in
/// `rebuild_lines` share identical rendering logic.
fn render_entry_into(
    entry: &ChatEntry,
    is_selected: bool,
    show_thoughts: bool,
    width: u16,
    lines: &mut Vec<Line<'static>>,
) {
    let sel_mod = if is_selected {
        Modifier::REVERSED
    } else {
        Modifier::empty()
    };
    match entry {
        ChatEntry::UserMessage { text, attachments } => {
            let label_span = Span::styled(
                format!("{} ", crate::i18n::t("zc-chat-label-you")),
                theme::user_label_style().add_modifier(sel_mod),
            );
            let body_style = theme::body_style().add_modifier(sel_mod);
            let mut text_lines: Vec<&str> = match text {
                Some(t) => t.split('\n').collect(),
                None => Vec::new(),
            };
            if text_lines.is_empty() {
                text_lines.push("");
            }
            for (idx, line_text) in text_lines.iter().enumerate() {
                let mut spans = Vec::new();
                if idx == 0 {
                    spans.push(label_span.clone());
                }
                spans.push(Span::styled((*line_text).to_string(), body_style));
                lines.push(Line::from(spans));
            }
            if !attachments.is_empty() {
                let label = attachments
                    .iter()
                    .map(|a| a.as_ref())
                    .collect::<Vec<&str>>()
                    .join(", ");
                lines.push(Line::from(Span::styled(
                    format!(" [{label}]"),
                    theme::warn_style().add_modifier(Modifier::ITALIC | sel_mod),
                )));
            }
        }
        ChatEntry::AgentMessage(text) => {
            lines.push(Line::from(vec![Span::styled(
                format!("{} ", crate::i18n::t("zc-chat-label-agent")),
                theme::agent_label_style().add_modifier(sel_mod),
            )]));
            let md_lines = markdown_to_lines(text.as_ref(), width);
            for mut line in md_lines {
                if is_selected {
                    line = Line::from(
                        line.spans
                            .into_iter()
                            .map(|s| {
                                s.patch_style(Style::default().add_modifier(Modifier::REVERSED))
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                lines.push(line);
            }
        }
        ChatEntry::AgentThought(text) => {
            if show_thoughts {
                lines.push(Line::from(vec![
                    Span::styled("(thinking) ", theme::thought_style().add_modifier(sel_mod)),
                    Span::styled(text.to_string(), theme::dim_style().add_modifier(sel_mod)),
                ]));
            }
        }
        ChatEntry::SystemMessage(text) => {
            for line_text in text.lines() {
                lines.push(Line::from(Span::styled(
                    line_text.to_string(),
                    theme::warn_style().add_modifier(Modifier::ITALIC | sel_mod),
                )));
            }
        }
        ChatEntry::Tool {
            name,
            input_json,
            result,
            ..
        } => {
            render_tool_entry(
                lines,
                name.as_ref(),
                input_json.as_ref(),
                result.as_deref().map(|s| s as &str),
                is_selected,
            );
        }
    }
}

fn borrow_line<'a>(line: &'a Line<'static>) -> Line<'a> {
    let spans: Vec<Span<'a>> = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.as_ref(), s.style))
        .collect();
    let mut out = Line::from(spans).style(line.style);
    if let Some(a) = line.alignment {
        out = out.alignment(a);
    }
    out
}

fn render_conversation(f: &mut Frame, state: &mut ChatState, area: Rect) {
    // Width must be computed before cache rebuild — table column budgets
    // depend on it, and a width change invalidates cached layouts.
    let inner_width = area.width.saturating_sub(2);

    // ── Rebuild cached lines only when entries changed ────────
    if state.dirty != LinesDirty::Clean || state.cached_render_width != inner_width {
        state.rebuild_lines(inner_width);
    }

    let mut lines: Vec<Line> = state.cached_lines.iter().map(borrow_line).collect();
    let mut transient = false;

    if !state.streaming_text.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("{} ", crate::i18n::t("zc-chat-label-agent")),
            theme::agent_label_style(),
        )]));
        lines.extend(markdown_to_lines(&state.streaming_text, inner_width));
        transient = true;
    }

    if state.show_thoughts && !state.streaming_thought.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("(thinking) ", theme::thought_style()),
            Span::styled(state.streaming_thought.as_str(), theme::dim_style()),
        ]));
        transient = true;
    }

    if state.pending_approval().is_some() {
        for _ in 0..APPROVAL_OVERLAY_HEIGHT {
            lines.push(Line::default());
        }
        transient = true;
    }

    // Reserve a pinned top row inside the panel for the session's first user
    // message — a recovery reminder that stays put across scroll and reload.
    let show_first = state
        .first_message
        .as_deref()
        .is_some_and(|m| !m.is_empty());
    let first_row_h: u16 = if show_first && area.height > 2 { 1 } else { 0 };

    let inner_height = area.height.saturating_sub(2).saturating_sub(first_row_h);

    let block = theme::panel_block(&format!(" {} ", state.title()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if first_row_h == 1 {
        let first_row = Rect::new(inner.x, inner.y, inner.width, 1);
        let msg = state.first_message.as_deref().unwrap_or_default();
        let line = Line::from(Span::styled(msg.to_string(), theme::dim_style()));
        f.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }), first_row);
    }

    // Conversation paragraph fills the inner area below the pinned row.
    let body_area = Rect::new(
        inner.x,
        inner.y + first_row_h,
        inner.width,
        inner.height.saturating_sub(first_row_h),
    );

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });

    let total_rows = if transient {
        p.line_count(inner_width) as u16
    } else {
        state.cached_total_rows
    };
    let max_scroll = total_rows.saturating_sub(inner_height);
    let scroll = if state.pinned_to_bottom {
        max_scroll
    } else {
        state.scroll_offset.min(max_scroll)
    };

    let p = p.scroll((scroll, 0));
    f.render_widget(p, body_area);

    state.last_total_rows = total_rows;
    state.last_inner_height = inner_height;
    state.scroll_offset = scroll;

    // Project each entry's line range into screen coords. Off-viewport
    // ranges get no rect.
    let body_x = body_area.x;
    let body_y = body_area.y;
    let body_w = inner_width;
    let body_h = inner_height;
    state.entry_rects.clear();
    for &(entry_idx, lo, hi) in &state.cached_line_ranges {
        let lo = lo as u16;
        let hi = hi as u16;
        let visible_lo = lo.max(scroll);
        let visible_hi = hi.min(scroll + body_h);
        if visible_hi <= visible_lo {
            continue;
        }
        let rect = Rect::new(
            body_x,
            body_y + (visible_lo - scroll),
            body_w,
            visible_hi - visible_lo,
        );
        state.entry_rects.push((entry_idx, rect));
    }

    let mut scrollbar_state = ScrollbarState::new(total_rows as usize)
        .position(scroll as usize)
        .viewport_content_length(inner_height as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area,
        &mut scrollbar_state,
    );
    // Scrollbar paints in `area.right() - 1`; mirror that.
    if area.height > 2 {
        state.scrollbar_track_rect = Some(Rect::new(
            area.x + area.width.saturating_sub(1),
            area.y + 1,
            1,
            area.height - 2,
        ));
    } else {
        state.scrollbar_track_rect = None;
    }
}

fn render_approval_overlay(f: &mut Frame, state: &ChatState, area: Rect) {
    let pa = match state.pending_approval() {
        Some(p) => p,
        None => return,
    };

    // Anchor to the bottom of the given area.
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(APPROVAL_OVERLAY_HEIGHT),
        ])
        .split(area);
    let overlay_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(5),
            Constraint::Min(60),
            Constraint::Percentage(5),
        ])
        .split(vert[1])[1];

    f.render_widget(Clear, overlay_area);

    let is_edit_tool = matches!(pa.tool_name.as_str(), "file_edit" | "file_write");
    let allow = crate::i18n::t("zc-chat-approval-action-allow");
    let always = crate::i18n::t("zc-chat-approval-action-always");
    let reject = crate::i18n::t("zc-chat-approval-action-reject");
    let edit = crate::i18n::t("zc-chat-approval-action-edit");
    let keys = if is_edit_tool {
        format!("Enter={allow}  a={always}  Ctrl+D={reject}  e={edit}")
    } else {
        format!("Enter={allow}  a={always}  Ctrl+D={reject}")
    };

    // For file_edit/file_write, strip the bulk content fields — the diff
    // preview in the conversation already shows old/new content.
    let summary = if is_edit_tool {
        strip_content_fields(&pa.arguments_summary)
    } else {
        pa.arguments_summary.clone()
    };

    let secs = pa.timeout_secs.to_string();
    let title = crate::i18n::t_args(
        "zc-chat-approval-title",
        &[("tool", &pa.tool_name), ("secs", &secs)],
    );
    let text = if summary.is_empty() {
        format!("{title}\n\n  {keys}")
    } else {
        format!("{title}\n\n  {summary}\n\n  {keys}")
    };

    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Approval Required ", theme::warn_style()))
                .style(theme::approval_border_style()),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, overlay_area);
}

/// Strip `old_string`, `new_string`, and `content` from an `arguments_summary`
/// string (format: `"key: val, key: val, …"`) so the approval overlay stays
/// compact when a diff preview is already shown in the conversation.
fn strip_content_fields(summary: &str) -> String {
    let mut s = summary;
    for key in &["old_string", "new_string", "content"] {
        // Key appears mid-string as ", key: …"
        if let Some(i) = s.find(&format!(", {key}:")) {
            s = &s[..i];
        } else if s.starts_with(&format!("{key}:")) {
            s = "";
        }
    }
    s.trim_end_matches([',', ' ']).to_string()
}

// ── Session overlay rendering ─────────────────────────────────────

/// Compute the overlay rect for the session list picker.
/// Kept in sync with `render_session_list_overlay` so mouse hit-testing
/// can use the same geometry without storing extra state.
fn session_list_overlay_area(area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Min(8),
            Constraint::Percentage(20),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(15),
            Constraint::Min(40),
            Constraint::Percentage(15),
        ])
        .split(vert[1])[1]
}

fn render_session_list_overlay(
    f: &mut Frame,
    area: Rect,
    sessions: &[SessionEntry],
    list_state: &ListState,
) {
    let overlay_area = session_list_overlay_area(area);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Sessions (Enter=switch, Esc=close) ",
            theme::overlay_border_style(),
        ))
        .style(theme::overlay_border_style());

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            let name = s.name.as_deref().unwrap_or(&s.session_id);
            let agent = s.agent_alias.as_deref().unwrap_or("?");
            let label = format!("{name}  ({agent}, {} msgs)", s.message_count);
            ListItem::new(Span::styled(label, theme::body_style()))
        })
        .collect();

    let list = List::new(items).highlight_style(theme::list_highlight_style());
    // Copy state to pass as mutable.
    let mut ls = *list_state;
    f.render_stateful_widget(list, inner, &mut ls);
}

/// Render a single-row context usage bar showing token consumption.
///
/// Shows: `ctx: 12,345 / 200,000  [████████░░░░░░░░░░░░]  6%`
/// When max is unknown, shows: `ctx: 12,345 tokens`
/// Render a markdown blob into terminal lines.
///
/// `width` is the available rendering width in cells (the chat-area inner
/// width). It only matters for tables, which compute their column budgets
/// from it; non-table content ignores it.
fn markdown_to_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    use pulldown_cmark::{Alignment as MdAlign, HeadingLevel};

    let mut opts = MdOptions::empty();
    opts.insert(MdOptions::ENABLE_TABLES);
    opts.insert(MdOptions::ENABLE_STRIKETHROUGH);
    opts.insert(MdOptions::ENABLE_TASKLISTS);
    let parser = MdParser::new_ext(text, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut in_bold = false;
    let mut in_italic = false;
    let mut in_strike = false;
    let mut in_code_block = false;
    let mut heading_level: Option<HeadingLevel> = None;
    let mut blockquote_depth: u32 = 0;
    let mut link_url: Option<String> = None;

    // Table state. While non-`None`, text/inline events accumulate into the
    // current cell instead of the live `current_spans` line.
    struct TableBuf {
        alignments: Vec<MdAlign>,
        rows: Vec<Vec<String>>,
        in_header: bool,
        current_row: Vec<String>,
        current_cell: Option<String>,
    }
    let mut table: Option<TableBuf> = None;

    let push_line = |lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>| {
        if !spans.is_empty() {
            lines.push(Line::from(std::mem::take(spans)));
        }
    };

    let blockquote_gutter = |depth: u32| -> Vec<Span<'static>> {
        (0..depth)
            .map(|_| Span::styled("\u{2502} ", theme::dim_style()))
            .collect()
    };

    for event in parser {
        // While inside a table cell, route inline events into the cell
        // buffer. The table only lays out at TagEnd::Table.
        if let Some(t) = table.as_mut()
            && let Some(cell) = t.current_cell.as_mut()
        {
            match &event {
                MdEvent::Text(s) | MdEvent::Code(s) => {
                    cell.push_str(s);
                    continue;
                }
                MdEvent::SoftBreak | MdEvent::HardBreak => {
                    cell.push(' ');
                    continue;
                }
                _ => {}
            }
        }

        match event {
            MdEvent::Start(Tag::Strong) => in_bold = true,
            MdEvent::End(TagEnd::Strong) => in_bold = false,
            MdEvent::Start(Tag::Emphasis) => in_italic = true,
            MdEvent::End(TagEnd::Emphasis) => in_italic = false,
            MdEvent::Start(Tag::Strikethrough) => in_strike = true,
            MdEvent::End(TagEnd::Strikethrough) => in_strike = false,
            MdEvent::Start(Tag::Heading { level, .. }) => {
                push_line(&mut lines, &mut current_spans);
                lines.push(Line::default());
                heading_level = Some(level);
                if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                    current_spans.push(Span::styled("\u{258C} ", theme::accent_style()));
                }
            }
            MdEvent::End(TagEnd::Heading(_)) => {
                push_line(&mut lines, &mut current_spans);
                lines.push(Line::default());
                heading_level = None;
            }
            MdEvent::Start(Tag::BlockQuote(_)) => {
                push_line(&mut lines, &mut current_spans);
                blockquote_depth += 1;
            }
            MdEvent::End(TagEnd::BlockQuote(_)) => {
                push_line(&mut lines, &mut current_spans);
                blockquote_depth = blockquote_depth.saturating_sub(1);
            }
            MdEvent::Start(Tag::Link { dest_url, .. }) => {
                link_url = Some(dest_url.to_string());
            }
            MdEvent::End(TagEnd::Link) => {
                if let Some(url) = link_url.take() {
                    current_spans.push(Span::styled(
                        format!(" ({url})"),
                        theme::dim_style().add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            MdEvent::Start(Tag::CodeBlock(_)) => {
                push_line(&mut lines, &mut current_spans);
                in_code_block = true;
            }
            MdEvent::End(TagEnd::CodeBlock) => {
                push_line(&mut lines, &mut current_spans);
                in_code_block = false;
            }
            MdEvent::Start(Tag::Item) => {
                push_line(&mut lines, &mut current_spans);
                current_spans.extend(blockquote_gutter(blockquote_depth));
                current_spans.push(Span::styled("  \u{2022} ", theme::dim_style()));
            }
            MdEvent::End(TagEnd::Item) if !current_spans.is_empty() => {
                push_line(&mut lines, &mut current_spans);
            }
            MdEvent::Start(Tag::Paragraph) if blockquote_depth > 0 && current_spans.is_empty() => {
                current_spans.extend(blockquote_gutter(blockquote_depth));
            }
            MdEvent::Start(Tag::Paragraph) => {}
            MdEvent::End(TagEnd::Paragraph) if !current_spans.is_empty() => {
                push_line(&mut lines, &mut current_spans);
            }
            MdEvent::TaskListMarker(checked) => {
                let glyph = if checked { "\u{2611} " } else { "\u{2610} " };
                current_spans.push(Span::styled(glyph, theme::accent_style()));
            }
            // ── Tables ──────────────────────────────────────────
            MdEvent::Start(Tag::Table(alignments)) => {
                push_line(&mut lines, &mut current_spans);
                table = Some(TableBuf {
                    alignments,
                    rows: Vec::new(),
                    in_header: false,
                    current_row: Vec::new(),
                    current_cell: None,
                });
            }
            MdEvent::Start(Tag::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.in_header = true;
                    t.current_row.clear();
                }
            }
            MdEvent::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    let row = std::mem::take(&mut t.current_row);
                    t.rows.push(row);
                    t.in_header = false;
                }
            }
            MdEvent::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.current_row.clear();
                }
            }
            MdEvent::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    let row = std::mem::take(&mut t.current_row);
                    t.rows.push(row);
                }
            }
            MdEvent::Start(Tag::TableCell) => {
                if let Some(t) = table.as_mut() {
                    t.current_cell = Some(String::new());
                }
            }
            MdEvent::End(TagEnd::TableCell) => {
                if let Some(t) = table.as_mut()
                    && let Some(cell) = t.current_cell.take()
                {
                    t.current_row.push(cell);
                }
            }
            MdEvent::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    lines.extend(render_table(t.rows, t.alignments, width));
                }
            }
            MdEvent::Text(t) => {
                let owned = t.to_string();
                if in_code_block {
                    for code_line in owned.split('\n') {
                        push_line(&mut lines, &mut current_spans);
                        current_spans.push(Span::styled(
                            format!("\u{2502} {code_line}"),
                            theme::code_block_style(),
                        ));
                    }
                } else {
                    let mut style = Style::default();
                    if let Some(level) = heading_level {
                        style = match level {
                            HeadingLevel::H1 | HeadingLevel::H2 => {
                                theme::heading_style().add_modifier(Modifier::BOLD)
                            }
                            _ => theme::heading_style(),
                        };
                    }
                    if in_bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if in_italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if in_strike {
                        style = style.add_modifier(Modifier::CROSSED_OUT);
                    }
                    if link_url.is_some() {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    current_spans.push(Span::styled(owned, style));
                }
            }
            MdEvent::Code(t) => {
                current_spans.push(Span::styled(t.to_string(), theme::code_inline_style()));
            }
            MdEvent::SoftBreak => {
                current_spans.push(Span::raw(" "));
            }
            MdEvent::HardBreak => {
                push_line(&mut lines, &mut current_spans);
                if blockquote_depth > 0 {
                    current_spans.extend(blockquote_gutter(blockquote_depth));
                }
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    // Fallback: if parsing produced nothing, return raw text.
    if lines.is_empty() && !text.is_empty() {
        lines.push(Line::from(Span::raw(text.to_string())));
    }

    lines
}

/// Render a parsed table to box-drawing terminal lines.
///
/// `width` is the total available render width. Per-column width is
/// proportional to the longest cell in that column, capped so the table
/// fits in `width`. Cells that exceed their column cap are truncated with
/// `…`. A column whose budget would force a truncation under 2 cells
/// collapses to a single `…`.
fn render_table(
    rows: Vec<Vec<String>>,
    alignments: Vec<pulldown_cmark::Alignment>,
    width: u16,
) -> Vec<Line<'static>> {
    use pulldown_cmark::Alignment as MdAlign;
    use unicode_width::UnicodeWidthStr;

    if rows.is_empty() {
        return Vec::new();
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }

    // Normalise: pad short rows so every row has `cols` cells.
    let mut grid: Vec<Vec<String>> = rows;
    for row in &mut grid {
        while row.len() < cols {
            row.push(String::new());
        }
    }

    // Natural width per column = longest cell.
    let mut natural: Vec<usize> = vec![0; cols];
    for row in &grid {
        for (i, cell) in row.iter().enumerate() {
            natural[i] = natural[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }

    // Frame budget: `│` borders (cols+1) + one-cell padding either side
    // of each cell (cols * 2).
    let frame = (cols + 1) + cols * 2;
    let avail = (width as usize).saturating_sub(frame);
    let total_natural: usize = natural.iter().sum();

    let widths: Vec<usize> = if total_natural <= avail || total_natural == 0 {
        natural.clone()
    } else {
        // Scale each column proportionally. Floor at 1 cell so columns
        // don't vanish; the renderer collapses 1–3 cell columns to `…`.
        natural
            .iter()
            .map(|n| ((*n * avail) / total_natural).max(1))
            .collect()
    };

    fn truncate_to(s: &str, budget: usize) -> String {
        use unicode_width::UnicodeWidthChar;
        if budget == 0 {
            return String::new();
        }
        let full_width = UnicodeWidthStr::width(s);
        if full_width <= budget {
            return s.to_string();
        }
        // Cell needs truncation but budget is too narrow to convey any
        // content + ellipsis — collapse to a single `…`.
        if budget < 2 {
            return "\u{2026}".to_string();
        }
        let mut acc = String::new();
        let mut used = 0usize;
        for ch in s.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w + 1 > budget {
                acc.push('\u{2026}');
                return acc;
            }
            acc.push(ch);
            used += w;
            if used == budget {
                return acc;
            }
        }
        acc
    }

    fn pad_cell(s: &str, budget: usize, align: MdAlign) -> String {
        let w = UnicodeWidthStr::width(s);
        let slack = budget.saturating_sub(w);
        match align {
            MdAlign::Right => format!("{}{}", " ".repeat(slack), s),
            MdAlign::Center => {
                let left = slack / 2;
                let right = slack - left;
                format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
            }
            MdAlign::None | MdAlign::Left => format!("{}{}", s, " ".repeat(slack)),
        }
    }

    let border = |left: &str, mid: &str, right: &str| -> Line<'static> {
        let mut s = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"\u{2500}".repeat(w + 2));
            if i + 1 < widths.len() {
                s.push_str(mid);
            }
        }
        s.push_str(right);
        Line::from(Span::styled(s, theme::dim_style()))
    };

    let render_row = |cells: &[String]| -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled("\u{2502}".to_string(), theme::dim_style()));
        for (i, cell) in cells.iter().enumerate() {
            let budget = widths[i];
            let trimmed = truncate_to(cell, budget);
            let align = alignments.get(i).copied().unwrap_or(MdAlign::None);
            let padded = pad_cell(&trimmed, budget, align);
            spans.push(Span::raw(format!(" {padded} ")));
            spans.push(Span::styled("\u{2502}".to_string(), theme::dim_style()));
        }
        Line::from(spans)
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(border("\u{250C}", "\u{252C}", "\u{2510}"));
    let mut iter = grid.into_iter();
    if let Some(header) = iter.next() {
        out.push(render_row(&header));
        out.push(border("\u{251C}", "\u{253C}", "\u{2524}"));
    }
    for row in iter {
        out.push(render_row(&row));
    }
    out.push(border("\u{2514}", "\u{2534}", "\u{2518}"));
    out
}

// ── ChatState / ChatEntry ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub arguments_summary: String,
    pub timeout_secs: u64,
}

/// One row in the chat / code-tab transcript. Heavy payloads
/// (agent messages, tool inputs, tool outputs) are refcounted via
/// `Arc<str>` so cloning is O(1) — the renderer and the
/// `cached_lines` line cache both hold cheap refs into the same
/// bytes instead of duplicating the string per render. Long
/// sessions stay flat on memory because every per-entry payload
/// has exactly one heap allocation regardless of how many places
/// borrow it.
#[derive(Debug, Clone)]
pub enum ChatEntry {
    AgentMessage(Arc<str>),
    AgentThought(Arc<str>),
    /// Local system/info message (e.g. "Attached: photo.png").
    SystemMessage(Arc<str>),
    UserMessage {
        text: Option<Arc<str>>,
        attachments: Vec<Arc<str>>,
    },
    Tool {
        tool_call_id: Arc<str>,
        name: Arc<str>,
        /// Pre-serialised JSON of the tool input. Storing the
        /// rendered string instead of a `serde_json::Value` tree
        /// drops the per-entry parsed-tree footprint (one
        /// allocation per Value node) to a single `Arc<str>`.
        input_json: Arc<str>,
        /// Tool output. `None` while the call is in flight,
        /// `Some(Arc<str>)` once the result arrives.
        result: Option<Arc<str>>,
    },
}

#[derive(Debug)]
enum SessionOverlay {
    None,
    List {
        sessions: Vec<SessionEntry>,
        list_state: ListState,
    },
}

/// Tracks what kind of update has invalidated the rendered lines cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinesDirty {
    /// Cache is up-to-date.
    Clean,
    /// New entries were appended at the tail; the render window has not shifted.
    /// `rebuild_lines` can extend `cached_lines` instead of rebuilding from scratch,
    /// avoiding re-parsing markdown for unchanged `AgentMessage` entries.
    Appended,
    /// Full rebuild required (entry mutation, selection/thoughts change, reset).
    Full,
}

/// Scrollbar drag captured on mouse-down on the track.
#[derive(Debug, Clone, Copy)]
struct ScrollbarDrag {
    start_scroll: u16,
    start_row: u16,
}

#[derive(Debug)]
pub struct ChatState {
    pub session_id: String,
    pub agent_alias: String,
    session_name: Option<String>,
    /// Working directory for this session (shown above input bar).
    pub cwd: Option<String>,
    /// Cached git branch for `cwd`, refreshed by the daemon on a polling
    /// interval (`GIT_BRANCH_REFRESH_INTERVAL`). `None` means either "not a
    /// git repo" or "not fetched yet".
    pub git_branch: Option<String>,
    /// First user message of the session, pulled from the persisted message
    /// store. Shown as a pinned recovery row at the top of the panel so the
    /// original ask stays visible across scroll and after a session reload.
    pub first_message: Option<String>,
    /// Cached short commit hash for `cwd`, refreshed alongside `git_branch`.
    /// `None` means "not a git repo", "unborn branch", or "not fetched yet".
    pub git_hash: Option<String>,
    /// Monotonic timestamp of the last completed `session/git_branch` reply,
    /// used to throttle re-fetches.
    pub git_branch_last_fetch: Option<Instant>,
    pub input_bar: InputBarState,
    entries: Vec<ChatEntry>,
    streaming_text: String,
    streaming_thought: String,
    pending_approval: Option<PendingApproval>,
    pub turn_in_flight: bool,
    /// Fine-grained label for the input-bar title while a turn is active.
    /// Lockstep with `turn_in_flight` (`Idle` ↔ `false`) but adds the
    /// thinking / responding / tool-call breakdown for the UI.
    pub turn_status: TurnStatus,
    /// Anchor for the dots animation — reset each time a turn begins so
    /// the pulse starts from phase 0.
    turn_started_at: Instant,
    show_thoughts: bool,
    /// Browse mode cursor (most-recently moved position).
    browse_cursor: Option<usize>,
    /// Anchor for range selection; set when Shift+↑/↓ is first pressed.
    /// Range is `min(anchor, cursor)..=max(anchor, cursor)`.
    browse_anchor: Option<usize>,
    /// Ctrl+click multi-select set, independent of cursor/anchor range.
    browse_multi: std::collections::BTreeSet<usize>,
    /// Per-entry hit rects from the last draw.
    entry_rects: Vec<(usize, ratatui::layout::Rect)>,
    /// Scrollbar track rect from the last draw.
    scrollbar_track_rect: Option<ratatui::layout::Rect>,
    /// Active scrollbar drag anchor.
    scrollbar_drag: Option<ScrollbarDrag>,
    session_overlay: SessionOverlay,
    scroll_offset: u16,
    pinned_to_bottom: bool,
    last_total_rows: u16,
    last_inner_height: u16,
    /// Cached rendered lines from committed entries.
    cached_lines: Vec<Line<'static>>,
    /// Per-entry unwrapped-line ranges in `cached_lines` — `(entry_idx,
    /// start, end_exclusive)`. Used by mouse hit-testing.
    cached_line_ranges: Vec<(usize, usize, usize)>,
    /// Fine-grained dirty tracking — see [`LinesDirty`].
    dirty: LinesDirty,
    /// How many entries from `entries[cached_render_start..]` are represented in
    /// `cached_lines`.  Valid only when `dirty != Full`.
    cached_entry_count: usize,
    /// The `entries` index where the render window starts for the current cache.
    cached_render_start: usize,
    /// The render width the current `cached_lines` were laid out for.
    /// A width change forces a full rebuild because tables compute their
    /// column budgets from it.
    cached_render_width: u16,
    cached_total_rows: u16,
    /// Cumulative token count for this session: every Usage event from the
    /// provider (input + cached + output) is added on arrival. Cleared on
    /// session reset only.
    pub context_input_tokens: Option<u64>,
    /// Configured context limit for this session's model.
    pub context_max_tokens: Option<u64>,
}

impl ChatState {
    pub fn new(session_id: String, agent_alias: String) -> Self {
        Self {
            session_id,
            agent_alias,
            session_name: None,
            cwd: None,
            git_branch: None,
            first_message: None,
            git_hash: None,
            git_branch_last_fetch: None,
            input_bar: InputBarState::new(),
            entries: Vec::new(),
            streaming_text: String::new(),
            streaming_thought: String::new(),
            pending_approval: None,
            turn_in_flight: false,
            turn_status: TurnStatus::Idle,
            turn_started_at: Instant::now(),
            show_thoughts: true,
            browse_cursor: None,
            browse_anchor: None,
            browse_multi: std::collections::BTreeSet::new(),
            entry_rects: Vec::new(),
            scrollbar_track_rect: None,
            scrollbar_drag: None,
            session_overlay: SessionOverlay::None,
            scroll_offset: 0,
            pinned_to_bottom: true,
            last_total_rows: 0,
            last_inner_height: 0,
            cached_lines: Vec::new(),
            cached_line_ranges: Vec::new(),
            dirty: LinesDirty::Full,
            cached_entry_count: 0,
            cached_render_start: 0,
            cached_render_width: 0,
            cached_total_rows: 0,
            context_input_tokens: None,
            context_max_tokens: None,
        }
    }

    fn mark_dirty_append(&mut self) {
        if self.dirty == LinesDirty::Clean {
            self.dirty = LinesDirty::Appended;
        }
        // Full is sticky — don't downgrade.
    }

    fn mark_dirty_full(&mut self) {
        self.dirty = LinesDirty::Full;
    }

    // ── Browse-mode helpers ───────────────────────────────────────

    /// True when browse mode is active (cursor is set).
    fn in_browse_mode(&self) -> bool {
        self.browse_cursor.is_some()
    }

    /// True when anything is selected — cursor, range, or multi.
    fn has_selection(&self) -> bool {
        self.browse_cursor.is_some() || !self.browse_multi.is_empty()
    }

    /// Build the clipboard string. Single = body. Multi = role-prefixed.
    fn yank_selection(&self) -> String {
        let sel = self.selected_entries();
        let count = sel.len();
        if count == 0 {
            return String::new();
        }
        let with_label = count > 1;
        sel.into_iter()
            .filter_map(|i| self.entries.get(i))
            .map(|e| {
                if with_label {
                    labelled_clipboard_text(e)
                } else {
                    clipboard_text(e)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Enter browse mode: jump cursor to last entry, clear anchor.
    fn enter_browse_mode(&mut self) {
        if !self.entries.is_empty() {
            self.browse_cursor = Some(self.entries.len() - 1);
            self.browse_anchor = None;
            self.mark_dirty_full();
        }
    }

    /// Leave browse mode: clear both cursor and anchor, return to input.
    fn exit_browse_mode(&mut self) {
        self.browse_cursor = None;
        self.browse_anchor = None;
        self.mark_dirty_full();
    }

    /// Move the cursor up by `n` entries.  Clamps at 0.
    /// If `extend` is true, sets/keeps the anchor for range selection.
    fn browse_move_up(&mut self, n: usize, extend: bool) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let cur = self.browse_cursor.unwrap_or(len - 1);
        if extend && self.browse_anchor.is_none() {
            self.browse_anchor = Some(cur);
        } else if !extend {
            self.browse_anchor = None;
        }
        self.browse_cursor = Some(cur.saturating_sub(n));
        self.mark_dirty_full();
    }

    /// Move the cursor down by `n` entries.  Clamps at last entry.
    /// If `extend` is true, sets/keeps the anchor for range selection.
    fn browse_move_down(&mut self, n: usize, extend: bool) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let cur = self.browse_cursor.unwrap_or(0);
        if extend && self.browse_anchor.is_none() {
            self.browse_anchor = Some(cur);
        } else if !extend {
            self.browse_anchor = None;
        }
        self.browse_cursor = Some((cur + n).min(len - 1));
        self.mark_dirty_full();
    }

    /// The selected range as `(lo, hi)` indices, inclusive.
    /// Returns `None` when not in browse mode.
    fn browse_range(&self) -> Option<(usize, usize)> {
        let cur = self.browse_cursor?;
        let anchor = self.browse_anchor.unwrap_or(cur);
        let lo = cur.min(anchor);
        let hi = cur.max(anchor);
        Some((lo, hi))
    }

    /// True when `idx` falls inside the current browse selection range.
    fn is_in_browse_range(&self, idx: usize) -> bool {
        self.browse_range()
            .is_some_and(|(lo, hi)| idx >= lo && idx <= hi)
    }

    /// True when `idx` should render highlighted: in range, in multi-select,
    /// or matches the lone cursor.
    fn is_entry_highlighted(&self, idx: usize) -> bool {
        if self.browse_multi.contains(&idx) {
            return true;
        }
        if self.is_in_browse_range(idx) {
            return true;
        }
        self.browse_cursor == Some(idx)
    }

    /// Total selection: multi-select set ∪ browse range ∪ lone cursor.
    fn selected_entries(&self) -> std::collections::BTreeSet<usize> {
        let mut out = self.browse_multi.clone();
        if let Some((lo, hi)) = self.browse_range() {
            for i in lo..=hi {
                out.insert(i);
            }
        } else if let Some(c) = self.browse_cursor {
            out.insert(c);
        }
        out
    }

    /// Rebuild (or incrementally extend) the cached rendered lines from committed entries.
    ///
    /// `width` is the chat-area inner width in cells. A change in width
    /// invalidates the table layouts inside the cached lines, so a width
    /// change forces a full rebuild.
    fn rebuild_lines(&mut self, width: u16) {
        if self.cached_render_width != width {
            self.dirty = LinesDirty::Full;
            self.cached_render_width = width;
        }
        const MAX_RENDERED_ENTRIES: usize = 1_000;
        let total = self.entries.len();
        let natural_start = total.saturating_sub(MAX_RENDERED_ENTRIES);
        let start = if let Some((lo, _hi)) = self.browse_range() {
            natural_start.min(lo)
        } else {
            natural_start
        };

        // Incremental append path.
        if self.dirty == LinesDirty::Appended && start == self.cached_render_start {
            let render_from = start + self.cached_entry_count;
            let show_thoughts = self.show_thoughts;
            let mut new_lines = Vec::new();
            let mut new_ranges = Vec::new();
            for (rel_idx, entry) in self.entries[render_from..].iter().enumerate() {
                let abs_idx = render_from + rel_idx;
                let before = new_lines.len();
                render_entry_into(
                    entry,
                    self.is_entry_highlighted(abs_idx),
                    show_thoughts,
                    width,
                    &mut new_lines,
                );
                let after = new_lines.len();
                if after > before {
                    let base = self.cached_lines.len();
                    new_ranges.push((abs_idx, base + before, base + after));
                }
            }
            let appended_rows =
                Paragraph::new(new_lines.iter().map(borrow_line).collect::<Vec<_>>())
                    .wrap(Wrap { trim: false })
                    .line_count(width) as u16;
            self.cached_lines.extend(new_lines);
            self.cached_line_ranges.extend(new_ranges);
            self.cached_entry_count = total - start;
            self.dirty = LinesDirty::Clean;
            self.cached_total_rows = self.cached_total_rows.saturating_add(appended_rows);
            return;
        }

        // Full rebuild path.
        let mut lines = Vec::new();
        let mut ranges = Vec::new();
        let show_thoughts = self.show_thoughts;
        for (rel_idx, entry) in self.entries[start..].iter().enumerate() {
            let abs_idx = start + rel_idx;
            let before = lines.len();
            render_entry_into(
                entry,
                self.is_entry_highlighted(abs_idx),
                show_thoughts,
                width,
                &mut lines,
            );
            let after = lines.len();
            if after > before {
                ranges.push((abs_idx, before, after));
            }
        }
        self.cached_lines = lines;
        self.cached_line_ranges = ranges;
        self.cached_entry_count = total - start;
        self.cached_render_start = start;
        self.dirty = LinesDirty::Clean;
        self.cached_total_rows = self.compute_cached_rows(width);
    }

    fn compute_cached_rows(&self, width: u16) -> u16 {
        Paragraph::new(
            self.cached_lines
                .iter()
                .map(borrow_line)
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false })
        .line_count(width) as u16
    }

    pub fn scroll_up(&mut self, lines: u16) {
        self.pinned_to_bottom = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: u16) {
        let max = self.last_total_rows.saturating_sub(self.last_inner_height);
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(max);
        if self.scroll_offset >= max {
            self.pinned_to_bottom = true;
        }
    }

    /// Display title: session name if set, otherwise agent alias.
    pub fn title(&self) -> String {
        let short = self.session_id.get(..7).unwrap_or(self.session_id.as_str());
        match &self.session_name {
            Some(name) => format!("{} — {}  {}", self.agent_alias, name, short),
            None => format!("{}  {}", self.agent_alias, short),
        }
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[ChatEntry] {
        &self.entries
    }

    #[cfg(test)]
    pub fn current_agent_text(&self) -> &str {
        &self.streaming_text
    }

    #[cfg(test)]
    pub fn current_thought_text(&self) -> &str {
        &self.streaming_thought
    }

    pub fn pending_approval(&self) -> Option<&PendingApproval> {
        self.pending_approval.as_ref()
    }

    pub fn take_pending_approval(&mut self) -> Option<PendingApproval> {
        self.pending_approval.take()
    }

    /// Commit any accumulated streaming thought as an entry. Called at the two
    /// natural flush points: when a tool call interrupts thinking, and when the
    /// first response text chunk arrives after a thinking phase.
    fn flush_streaming_thought(&mut self) {
        let thought = std::mem::take(&mut self.streaming_thought);
        if !thought.is_empty() {
            self.entries
                .push(ChatEntry::AgentThought(Arc::<str>::from(thought)));
            self.mark_dirty_append();
        }
    }

    /// Commit any accumulated streaming text as an `AgentMessage` entry.
    /// Called when a tool call interrupts the text stream so that pre-tool
    /// text is committed in conversation order before the `Tool` entry.
    fn flush_streaming_text(&mut self) {
        let text = std::mem::take(&mut self.streaming_text);
        if !text.is_empty() {
            self.entries
                .push(ChatEntry::AgentMessage(Arc::<str>::from(text)));
            self.mark_dirty_append();
        }
    }

    pub fn apply_update(&mut self, update: SessionUpdate) {
        // Ignore notifications that belong to a different session.
        let update_sid = match &update {
            SessionUpdate::AgentMessageChunk { session_id, .. }
            | SessionUpdate::AgentThoughtChunk { session_id, .. }
            | SessionUpdate::ToolCall { session_id, .. }
            | SessionUpdate::ToolResult { session_id, .. }
            | SessionUpdate::ApprovalRequest { session_id, .. }
            | SessionUpdate::ContextUsage { session_id, .. }
            | SessionUpdate::TurnComplete { session_id, .. } => session_id.as_str(),
        };
        if update_sid != self.session_id {
            return;
        }

        match update {
            SessionUpdate::AgentMessageChunk { text, .. } => {
                // Flush any accumulated thought before the response text begins
                // so it appears inline at the right position, not piled at the end.
                if self.streaming_text.is_empty() {
                    self.flush_streaming_thought();
                }
                self.streaming_text.push_str(&text);
                // Guard: don't mutate turn_status after commit_turn has already
                // set us back to Idle. Late-arriving notifications (broadcast
                // channel lag) can otherwise flip the input bar back to the
                // working animator even though the turn is done.
                if self.turn_in_flight {
                    self.turn_status = TurnStatus::Responding;
                }
            }
            SessionUpdate::AgentThoughtChunk { text, .. } => {
                self.streaming_thought.push_str(&text);
                if self.turn_in_flight {
                    self.turn_status = TurnStatus::Thinking;
                }
            }
            SessionUpdate::ToolCall {
                tool_call_id,
                name,
                raw_input,
                ..
            } => {
                // Flush any accumulated text and thought before the tool call
                // so that pre-tool agent text and thinking both appear in
                // conversation order before the Tool entry.
                self.flush_streaming_text();
                self.flush_streaming_thought();
                if self.turn_in_flight {
                    self.turn_status = TurnStatus::CallingTool(name.clone());
                }
                self.entries.push(ChatEntry::Tool {
                    tool_call_id: Arc::<str>::from(tool_call_id),
                    name: Arc::<str>::from(name),
                    input_json: Arc::<str>::from(
                        serde_json::to_string(&raw_input).unwrap_or_default(),
                    ),
                    result: None,
                });
                self.mark_dirty_append();
            }
            SessionUpdate::ToolResult {
                tool_call_id,
                raw_output,
                ..
            } => {
                // Cap stored output so large tool responses (bash, file reads) don't
                // accumulate unboundedly.  The renderer already truncates to 200 chars
                // for display; 16 KB gives clipboard users a generous but bounded copy.
                const MAX_RAW_OUTPUT: usize = 16 * 1024;
                let raw_output = if raw_output.len() > MAX_RAW_OUTPUT {
                    format!("{}…[truncated]", truncate_utf8(&raw_output, MAX_RAW_OUTPUT))
                } else {
                    raw_output
                };
                for entry in self.entries.iter_mut().rev() {
                    if let ChatEntry::Tool {
                        tool_call_id: id,
                        result,
                        ..
                    } = entry
                        && id.as_ref() == tool_call_id.as_str()
                    {
                        *result = Some(Arc::<str>::from(raw_output));
                        self.mark_dirty_full(); // mutation of existing entry
                        break;
                    }
                }
                // Tool finished; we're back in the model's hands. Don't clobber
                // a more specific status if one has already arrived (chunks can
                // race the result), so only step down from the matching
                // CallingTool state. Also guard against post-commit stale
                // notifications flipping us out of Idle.
                if self.turn_in_flight && matches!(self.turn_status, TurnStatus::CallingTool(_)) {
                    self.turn_status = TurnStatus::Working;
                }
            }
            SessionUpdate::ApprovalRequest {
                request_id,
                tool_name,
                arguments_summary,
                timeout_secs,
                ..
            } => {
                self.pending_approval = Some(PendingApproval {
                    request_id,
                    tool_name,
                    arguments_summary,
                    timeout_secs,
                });
                if self.turn_in_flight {
                    self.turn_status = TurnStatus::WaitingForApproval;
                }
            }
            SessionUpdate::ContextUsage {
                input_tokens,
                max_context_tokens,
                ..
            } => {
                // Replace-on-arrival: ContextUsage reports the *current* prompt
                // size for the upcoming/just-sent turn. It is an absolute
                // measurement of how full the model's context window is, not
                // an increment. Accumulating across turns produced a runaway
                // counter that quickly exceeded the window.
                if input_tokens.is_some() {
                    self.context_input_tokens = input_tokens;
                }
                if max_context_tokens.is_some() {
                    self.context_max_tokens = max_context_tokens;
                }
            }
            SessionUpdate::TurnComplete {
                outcome, content, ..
            } => {
                // Single source of truth for turn end. RPC errors on
                // session/prompt cannot reach this — only the daemon can.
                // For a cancel or failure the daemon composes the attributed
                // reason in `content` (who cancelled, and why); render it as a
                // system line. For a clean finish, `content` is the final text
                // and commit_turn handles it.
                match outcome {
                    TurnEndOutcome::Completed => {
                        self.commit_turn(content);
                    }
                    TurnEndOutcome::Cancelled | TurnEndOutcome::Failed => {
                        self.entries
                            .push(ChatEntry::SystemMessage(Arc::<str>::from(content.as_str())));
                        self.mark_dirty_append();
                        self.commit_turn(String::new());
                    }
                }
            }
        }
    }

    pub fn commit_turn(&mut self, full_text: String) {
        // Flush any remaining streaming text as a final AgentMessage.
        // `flush_streaming_text` takes the buffer, so after this call
        // `streaming_text` is empty. If the buffer was non-empty (i.e. the
        // turn ended with trailing text that was never interrupted by a tool
        // call), the entry is committed here. If the buffer was already empty
        // (all text was flushed at ToolCall boundaries mid-turn), nothing is
        // pushed and we avoid duplicating already-committed entries.
        //
        // We do NOT use `full_text` to push a final entry: the full turn text
        // is the concatenation of all chunks, which have already been
        // committed in order (pre-tool, post-tool, …). Using `full_text` here
        // would duplicate text that was flushed earlier.
        self.flush_streaming_text();
        // Flush any trailing thought not yet committed (e.g. thinking-only turn).
        self.flush_streaming_thought();
        // If the turn produced text but no tool calls interrupted it, the
        // buffer was non-empty and flush_streaming_text already committed it.
        // If the turn produced only tool calls (no trailing text) or all text
        // was flushed mid-turn, nothing more to push.
        // Legacy path: if streaming_text was empty AND full_text is non-empty
        // AND no AgentMessage was committed this turn (pure tool-only turn
        // with a final summary), push full_text.  This preserves behaviour
        // for turns that have no chunks at all (e.g. instant responses from
        // tests that call commit_turn directly without apply_update).
        let _ = full_text; // consumed by flush above; kept as parameter for API stability
        self.mark_dirty_append();
        self.turn_in_flight = false;
        self.turn_status = TurnStatus::Idle;
        self.input_bar.cleanup_temps();
    }

    pub fn push_user_message(&mut self, text: Option<String>, attachments: Vec<String>) {
        if self.first_message.is_none()
            && let Some(ref t) = text
            && !t.trim().is_empty()
        {
            self.first_message = Some(t.clone());
        }
        self.entries.push(ChatEntry::UserMessage {
            text: text.map(Arc::<str>::from),
            attachments: attachments.into_iter().map(Arc::<str>::from).collect(),
        });
        self.mark_dirty_append();
        self.turn_in_flight = true;
        // Start a fresh status + animation anchor. We're `Working` until the
        // first chunk (thought / message / tool-call) tells us otherwise.
        self.turn_status = TurnStatus::Working;
        self.turn_started_at = Instant::now();
    }

    /// Replay persisted message history into the transcript on a session resume.
    /// Mirrors the daemon-retained store into UI entries and seeds the pinned
    /// first-message recovery row, so a reconnect/reattach shows the prior
    /// conversation instead of an empty pane. Idempotent on entries: callers
    /// invoke it on a freshly reset session state.
    fn load_history(&mut self, messages: Vec<crate::client::MessageEntry>) {
        for m in messages {
            match m.role() {
                crate::client::MessageRole::User => {
                    if self.first_message.is_none() {
                        self.first_message = Some(m.content.clone());
                    }
                    self.entries.push(ChatEntry::UserMessage {
                        text: Some(Arc::<str>::from(m.content)),
                        attachments: vec![],
                    });
                }
                crate::client::MessageRole::Assistant => {
                    self.entries
                        .push(ChatEntry::AgentMessage(Arc::<str>::from(m.content)));
                }
                crate::client::MessageRole::System | crate::client::MessageRole::Other => {}
            }
        }
        self.mark_dirty_full();
    }
    /// Reset conversational state for a new or switched session.
    pub fn reset_for_session(&mut self, session_id: String, name: Option<String>) {
        self.session_id = session_id;
        self.session_name = name;
        self.input_bar.reset();
        self.entries.clear();
        self.streaming_text.clear();
        self.streaming_thought.clear();
        self.cached_lines.clear();
        self.dirty = LinesDirty::Full;
        self.cached_entry_count = 0;
        self.cached_render_start = 0;
        self.cached_render_width = 0;
        self.pending_approval = None;
        self.turn_in_flight = false;
        self.turn_status = TurnStatus::Idle;
        self.browse_cursor = None;
        self.browse_anchor = None;
        self.browse_multi.clear();
        // Reset branch cache: new session may have a different cwd.
        self.git_branch = None;
        self.first_message = None;
        self.git_hash = None;
        self.git_branch_last_fetch = None;
        // Context usage is per-session; clear so we don't show stale numbers
        // from the previous session before the first LLM call fires a new
        // ContextUsage event.
        self.context_input_tokens = None;
        self.context_max_tokens = None;
    }
}

/// Body-only clipboard text.
fn clipboard_text(entry: &ChatEntry) -> String {
    match entry {
        ChatEntry::UserMessage { text, attachments } => {
            let base = text.as_deref().unwrap_or("");
            if attachments.is_empty() {
                base.to_string()
            } else {
                let label = attachments
                    .iter()
                    .map(|a| a.as_ref())
                    .collect::<Vec<&str>>()
                    .join(", ");
                format!("{base} [{label}]")
            }
        }
        ChatEntry::AgentMessage(t) => t.to_string(),
        ChatEntry::AgentThought(t) => format!("(thinking) {t}"),
        ChatEntry::SystemMessage(t) => t.to_string(),
        ChatEntry::Tool {
            name,
            input_json,
            result,
            ..
        } => match result {
            Some(r) => format!("[tool: {name}] {input_json}\n  \u{2514}\u{2500} {r}"),
            None => format!("[tool: {name}] {input_json}"),
        },
    }
}

/// Role-prefixed clipboard text. Used when ≥2 entries are yanked.
fn labelled_clipboard_text(entry: &ChatEntry) -> String {
    match entry {
        ChatEntry::UserMessage { .. } => {
            crate::i18n::t_args("zc-chat-clipboard-you", &[("text", &clipboard_text(entry))])
        }
        ChatEntry::AgentMessage(_) => crate::i18n::t_args(
            "zc-chat-clipboard-agent",
            &[("text", &clipboard_text(entry))],
        ),
        _ => clipboard_text(entry),
    }
}

/// Suspend the TUI, open `$EDITOR` with `content`, return the edited text.
/// Restores raw mode and alternate screen before returning.
/// Falls back to `content` unchanged if `$EDITOR` is unset or the process fails.
pub async fn open_editor_for_content(content: &str) -> String {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    let tmp = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(_) => return content.to_string(),
    };
    if std::fs::write(tmp.path(), content).is_err() {
        return content.to_string();
    }

    crossterm::terminal::disable_raw_mode().ok();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::terminal::LeaveAlternateScreen
    );

    let path = tmp.path().to_owned();
    let status = tokio::process::Command::new(&editor)
        .arg(&path)
        .status()
        .await;

    crossterm::terminal::enable_raw_mode().ok();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen,);
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
    }

    if status.map(|s| s.success()).unwrap_or(false) {
        std::fs::read_to_string(&path).unwrap_or_else(|_| content.to_string())
    } else {
        content.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ChatState {
        ChatState::new("sess-1".to_string(), "myagent".to_string())
    }

    #[tokio::test]
    async fn current_session_id_reports_active_session() {
        let (tx, _rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(Arc::clone(&rpc)));
        let mut chat = Chat::new(client, PaneKind::Acp);
        // No session yet → None.
        assert_eq!(chat.current_session_id(), None);
        chat.phase = ChatPhase::Active(Box::new(state()));
        // Active → the live session id (the `state()` helper's id).
        assert!(chat.current_session_id().is_some());
    }

    #[tokio::test]
    async fn resume_session_id_dropped_when_init_lands_in_multi_agent_picker() {
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(Arc::clone(&rpc)));
        let mut chat = Chat::new(client, PaneKind::Acp);
        chat.set_resume_session_id(Some("sess-prev".to_string()));

        let init = tokio::spawn(async move {
            let _ = chat.init().await;
            chat
        });

        let line = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("init should request the agent list")
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = request["id"].as_str().unwrap().to_string();
        // Two enabled agents → multi-agent picker, no auto-start.
        rpc.dispatch_response(
            &id,
            Some(serde_json::json!({
                "agents": [
                    {"alias": "alpha", "enabled": true, "active_sessions": 0},
                    {"alias": "beta", "enabled": true, "active_sessions": 0}
                ]
            })),
            None,
        );

        let chat = tokio::time::timeout(Duration::from_secs(2), init)
            .await
            .expect("init should finish")
            .unwrap();
        // A carried resume id with no matching agent must not survive into the
        // picker, or a manual pick of a different agent would reattach a
        // mismatched session.
        assert_eq!(chat.resume_session_id, None);
        assert!(matches!(chat.phase, ChatPhase::PickAgent { .. }));
    }

    #[tokio::test]
    async fn multi_agent_reconnect_reattaches_prior_agent_session() {
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(Arc::clone(&rpc)));
        let mut chat = Chat::new(client, PaneKind::Chat);
        chat.set_resume_session_id(Some("sess-prev".to_string()));
        chat.set_resume_agent_alias(Some("beta".to_string()));

        let init = tokio::spawn(async move {
            let _ = chat.init().await;
            chat
        });

        // First request: the agent list.
        let line = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("init should request the agent list")
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = request["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(serde_json::json!({
                "agents": [
                    {"alias": "alpha", "enabled": true, "active_sessions": 0},
                    {"alias": "beta", "enabled": true, "active_sessions": 1}
                ]
            })),
            None,
        );

        // Second request must be session_new_with_id carrying the prior id for
        // the prior agent — NOT a fresh pick / fresh session. This is the whole
        // fix: a multi-agent reconnect reattaches instead of minting fresh.
        let line = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("reconnect should reattach the prior session")
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "session/new");
        let params = &request["params"];
        assert_eq!(params["agent_alias"], "beta");
        assert_eq!(params["session_id"], "sess-prev");

        init.abort();
    }

    #[tokio::test]
    async fn agent_picker_click_selects_row() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (tx, _rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(Arc::clone(&rpc)));
        let mut chat = Chat::new(client, PaneKind::Chat);
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        chat.phase = ChatPhase::PickAgent {
            agents: vec!["alpha".into(), "beta".into(), "gamma".into()],
            list_state,
            loading: false,
        };
        // Stored rect is the draw's shifted form: list_click_index treats (y+1)
        // as the first item. With y=1, first item maps to row 2.
        chat.pick_agent_list_area = Rect::new(1, 1, 20, 6);
        // Click the third item → row 2 + 2 = 4.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        chat.handle_mouse(click, Rect::new(0, 0, 40, 10)).await;
        if let ChatPhase::PickAgent { list_state, .. } = &chat.phase {
            assert_eq!(
                list_state.selected(),
                Some(2),
                "click selects the clicked row"
            );
        } else {
            panic!("expected PickAgent phase");
        }
    }

    fn authoritative_rows(s: &ChatState, width: u16) -> u16 {
        Paragraph::new(s.cached_lines.iter().map(borrow_line).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .line_count(width) as u16
    }

    #[test]
    fn cached_total_rows_matches_full_line_count() {
        let width: u16 = 40;
        let mut s = state();

        for i in 0..50 {
            s.push_user_message(Some(format!("message number {i} with enough text to wrap across the forty column width budget")), Vec::new());
        }
        s.rebuild_lines(width);
        assert_eq!(
            s.cached_total_rows,
            authoritative_rows(&s, width),
            "full-rebuild row total must match line_count"
        );

        for i in 50..60 {
            s.push_user_message(
                Some(format!(
                    "appended message {i} also long enough to wrap somewhere in the middle of a row"
                )),
                Vec::new(),
            );
        }
        s.rebuild_lines(width);
        assert_eq!(
            s.cached_total_rows,
            authoritative_rows(&s, width),
            "incremental-append row total must match line_count"
        );

        let narrower: u16 = 20;
        s.rebuild_lines(narrower);
        assert_eq!(
            s.cached_total_rows,
            authoritative_rows(&s, narrower),
            "width change must force a recompute that still matches line_count"
        );
    }

    #[tokio::test]
    async fn chat_entry_refresh_reloads_agents_from_error_phase() {
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(Arc::clone(&rpc)));
        let mut chat = Chat::new(client, PaneKind::Chat);
        chat.phase = ChatPhase::Error("No enabled agents yet.".to_string());

        let refresh = tokio::spawn(async move {
            chat.refresh_if_inactive().await;
            chat
        });

        let line = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("refresh should request the agent list")
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], method::AGENTS_STATUS);

        let id = request["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(serde_json::json!({
                "agents": [
                    {"alias": "alpha", "enabled": true, "active_sessions": 0},
                    {"alias": "beta", "enabled": true, "active_sessions": 0}
                ]
            })),
            None,
        );

        let chat = tokio::time::timeout(Duration::from_secs(2), refresh)
            .await
            .expect("refresh should finish after agents/status response")
            .unwrap();
        let ChatPhase::PickAgent {
            agents, loading, ..
        } = chat.phase
        else {
            panic!("refresh should leave stale error state");
        };
        assert_eq!(agents, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(!loading);
    }

    #[tokio::test]
    async fn apply_update_during_turn_in_flight() {
        let mut s = state();
        s.turn_in_flight = true;
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "streaming...".to_string(),
        });
        assert_eq!(s.current_agent_text(), "streaming...");
    }

    #[test]
    fn input_append_and_clear() {
        let mut s = state();
        s.input_bar.push_input_char('h');
        s.input_bar.push_input_char('i');
        assert_eq!(s.input_bar.input(), "hi");
        let taken = s.input_bar.take_input();
        assert_eq!(taken, "hi");
        assert_eq!(s.input_bar.input(), "");
    }

    #[test]
    fn text_chunk_accumulates() {
        let mut s = state();
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Hello".to_string(),
        });
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: " world".to_string(),
        });
        assert_eq!(s.current_agent_text(), "Hello world");
    }

    #[test]
    fn tool_call_followed_by_result_is_one_entry() {
        let mut s = state();
        s.apply_update(SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command":"ls"}),
        });
        s.apply_update(SessionUpdate::ToolResult {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            raw_output: "file.txt\n".to_string(),
        });
        let entries = s.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ChatEntry::Tool {
                result: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn approval_request_sets_pending_approval() {
        let mut s = state();
        s.apply_update(SessionUpdate::ApprovalRequest {
            session_id: "sess-1".to_string(),
            request_id: "req-1".to_string(),
            tool_name: "shell".to_string(),
            arguments_summary: "rm -rf /".to_string(),
            timeout_secs: 30,
        });
        assert!(s.pending_approval().is_some());
        let pa = s.pending_approval().unwrap();
        assert_eq!(pa.request_id, "req-1");
        assert_eq!(pa.tool_name, "shell");
    }

    #[test]
    fn thought_chunk_visible_before_commit() {
        let mut s = state();
        s.turn_in_flight = true;
        s.apply_update(SessionUpdate::AgentThoughtChunk {
            session_id: "sess-1".to_string(),
            text: "reasoning...".to_string(),
        });
        assert_eq!(s.current_thought_text(), "reasoning...");
        assert!(
            s.entries().is_empty(),
            "thought must not become an entry mid-turn"
        );
    }

    #[test]
    fn thought_flushed_as_entry_before_tool_call() {
        let mut s = state();
        s.turn_in_flight = true;
        s.apply_update(SessionUpdate::AgentThoughtChunk {
            session_id: "sess-1".to_string(),
            text: "plan: run ls".to_string(),
        });
        s.apply_update(SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command": "ls"}),
        });
        // Thought must be committed as an entry before the tool entry.
        assert_eq!(s.entries().len(), 2);
        assert!(
            matches!(&s.entries()[0], ChatEntry::AgentThought(t) if t.as_ref() == "plan: run ls")
        );
        assert!(matches!(&s.entries()[1], ChatEntry::Tool { .. }));
        // streaming_thought is now clear.
        assert!(s.current_thought_text().is_empty());
    }

    #[test]
    fn thought_flushed_as_entry_before_first_response_chunk() {
        let mut s = state();
        s.turn_in_flight = true;
        s.apply_update(SessionUpdate::AgentThoughtChunk {
            session_id: "sess-1".to_string(),
            text: "thinking".to_string(),
        });
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Here is".to_string(),
        });
        // Thought entry committed before streaming text starts.
        assert_eq!(s.entries().len(), 1);
        assert!(matches!(&s.entries()[0], ChatEntry::AgentThought(t) if t.as_ref() == "thinking"));
        assert_eq!(s.current_agent_text(), "Here is");
        assert!(s.current_thought_text().is_empty());
    }

    #[test]
    fn subsequent_message_chunks_do_not_re_flush_thought() {
        let mut s = state();
        s.turn_in_flight = true;
        s.apply_update(SessionUpdate::AgentThoughtChunk {
            session_id: "sess-1".to_string(),
            text: "thinking".to_string(),
        });
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Hello".to_string(),
        });
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: " world".to_string(),
        });
        // Only one AgentThought entry, not two.
        assert_eq!(s.entries().len(), 1);
        assert_eq!(s.current_agent_text(), "Hello world");
    }

    // ── Interleaving regression tests ────────────────────────────

    /// Core interleaving scenario:
    /// text chunk → tool call → tool result → text chunk → commit
    /// Expected committed order: AgentMessage | Tool | AgentMessage
    #[test]
    fn text_before_tool_call_is_flushed_as_separate_agent_message() {
        let mut s = state();
        s.turn_in_flight = true;

        // Pre-tool text chunk.
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "I will run ls.".to_string(),
        });

        // Tool call interrupts the text stream.
        s.apply_update(SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command": "ls"}),
        });

        // At this point the pre-tool text must be committed as its own entry.
        assert_eq!(
            s.entries().len(),
            2,
            "expected AgentMessage + Tool entries, got {:?}",
            s.entries()
        );
        assert!(
            matches!(&s.entries()[0], ChatEntry::AgentMessage(t) if t.as_ref() == "I will run ls."),
            "first entry must be AgentMessage with pre-tool text"
        );
        assert!(
            matches!(&s.entries()[1], ChatEntry::Tool { .. }),
            "second entry must be Tool"
        );
        // streaming_text must be cleared after the flush.
        assert!(
            s.current_agent_text().is_empty(),
            "streaming_text must be empty after tool-call flush"
        );
    }

    /// After a tool call, post-tool text chunks accumulate in streaming_text
    /// as normal and are committed by commit_turn.
    #[test]
    fn text_after_tool_call_commits_separately() {
        let mut s = state();
        s.turn_in_flight = true;

        // Pre-tool text.
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Running ls.".to_string(),
        });
        // Tool call flushes pre-tool text.
        s.apply_update(SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command": "ls"}),
        });
        // Tool result.
        s.apply_update(SessionUpdate::ToolResult {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            raw_output: "file.txt\n".to_string(),
        });
        // Post-tool text.
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Done.".to_string(),
        });
        assert_eq!(s.current_agent_text(), "Done.");

        // commit_turn: only the post-tool text should become a new AgentMessage.
        s.commit_turn("Done.".to_string());

        // Final order: AgentMessage("Running ls.") | Tool | AgentMessage("Done.")
        assert_eq!(
            s.entries().len(),
            3,
            "expected 3 entries: pre-tool AgentMessage, Tool, post-tool AgentMessage"
        );
        assert!(
            matches!(&s.entries()[0], ChatEntry::AgentMessage(t) if t.as_ref() == "Running ls."),
            "first entry must be pre-tool AgentMessage"
        );
        assert!(
            matches!(
                &s.entries()[1],
                ChatEntry::Tool {
                    result: Some(_),
                    ..
                }
            ),
            "second entry must be Tool with result"
        );
        assert!(
            matches!(&s.entries()[2], ChatEntry::AgentMessage(t) if t.as_ref() == "Done."),
            "third entry must be post-tool AgentMessage"
        );
    }

    /// If there is NO pre-tool text, no spurious empty AgentMessage is inserted.
    #[test]
    fn no_spurious_agent_message_when_no_pre_tool_text() {
        let mut s = state();
        s.turn_in_flight = true;

        // Tool call with no preceding text chunk.
        s.apply_update(SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command": "ls"}),
        });

        // Only the Tool entry should exist — no empty AgentMessage.
        assert_eq!(s.entries().len(), 1);
        assert!(matches!(&s.entries()[0], ChatEntry::Tool { .. }));
    }

    /// commit_turn must not push a duplicate AgentMessage for text already
    /// flushed as a pre-tool entry.
    #[test]
    fn commit_turn_does_not_duplicate_already_flushed_text() {
        let mut s = state();
        s.turn_in_flight = true;

        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Before tool.".to_string(),
        });
        s.apply_update(SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command": "ls"}),
        });
        // No post-tool text; commit_turn receives the full text but streaming_text is empty.
        s.commit_turn("Before tool.".to_string());

        // Must be exactly: AgentMessage("Before tool.") | Tool
        // NOT: AgentMessage | Tool | AgentMessage (duplicate)
        assert_eq!(
            s.entries().len(),
            2,
            "commit_turn must not add a duplicate AgentMessage for already-flushed text"
        );
        assert!(
            matches!(&s.entries()[0], ChatEntry::AgentMessage(t) if t.as_ref() == "Before tool.")
        );
        assert!(matches!(&s.entries()[1], ChatEntry::Tool { .. }));
    }

    #[test]
    fn turn_commit_flushes_streaming_buffer() {
        let mut s = state();
        s.apply_update(SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Done".to_string(),
        });
        s.commit_turn("Done".to_string());
        assert_eq!(s.current_agent_text(), "");
        assert!(
            s.entries()
                .iter()
                .any(|e| matches!(e, ChatEntry::AgentMessage(t) if t.as_ref() == "Done"))
        );
    }

    // ── markdown_to_lines ──────────────────────────────────────────

    fn rendered(input: &str, width: u16) -> String {
        markdown_to_lines(input, width)
            .into_iter()
            .map(|l| {
                l.spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn md_table_renders_box_drawing_borders() {
        let out = rendered("| A | B |\n|---|---|\n| 1 | 2 |\n", 40);
        assert!(out.contains('\u{250C}'), "missing top-left corner: {out}");
        assert!(
            out.contains('\u{2514}'),
            "missing bottom-left corner: {out}"
        );
        assert!(out.contains('\u{2502}'), "missing vertical: {out}");
        assert!(out.contains('A'));
        assert!(out.contains('1'));
    }

    #[test]
    fn md_table_truncates_when_width_is_tight() {
        let out = rendered(
            "| col |\n|-----|\n| this cell is far too long for a tiny width |\n",
            20,
        );
        assert!(out.contains('\u{2026}'), "expected ellipsis: {out}");
    }

    #[test]
    fn md_heading_emits_gutter_for_h1() {
        let out = rendered("# Title\n", 80);
        assert!(out.contains('\u{258C}'), "expected H1 gutter: {out}");
        assert!(out.contains("Title"));
    }

    #[test]
    fn md_blockquote_prefixes_each_line() {
        let out = rendered("> quoted text\n", 80);
        assert!(
            out.contains('\u{2502}'),
            "expected blockquote gutter: {out}"
        );
        assert!(out.contains("quoted text"));
    }

    #[test]
    fn md_link_appends_url_inline() {
        let out = rendered("[click](https://example.com)\n", 80);
        assert!(out.contains("click"));
        assert!(out.contains("https://example.com"));
    }

    #[test]
    fn md_strikethrough_passes_text_through() {
        // Style flag isn't visible in plain text join, but the text must
        // still render — proves the parser option is enabled.
        let out = rendered("~~gone~~\n", 80);
        assert!(out.contains("gone"));
    }

    #[test]
    fn md_task_list_renders_checkbox_glyphs() {
        let out = rendered("- [x] done\n- [ ] todo\n", 80);
        assert!(out.contains('\u{2611}'), "expected checked glyph: {out}");
        assert!(out.contains('\u{2610}'), "expected unchecked glyph: {out}");
    }

    #[test]
    fn md_table_with_no_width_still_emits_lines() {
        // Defensive: zero width must not panic and must not emit infinite
        // padding. The truncation rule collapses every column to `…`.
        let out = markdown_to_lines("| A |\n|---|\n| 1 |\n", 0);
        assert!(!out.is_empty());
    }

    #[test]
    fn title_includes_short_session_hash() {
        let s = ChatState::new("40be7731122334455".to_string(), "personal_code".to_string());
        assert_eq!(s.title(), "personal_code  40be773");
    }

    #[test]
    fn title_with_session_name_keeps_hash() {
        let mut s = ChatState::new("40be7731122334455".to_string(), "personal_code".to_string());
        s.session_name = Some("my work".to_string());
        assert_eq!(s.title(), "personal_code — my work  40be773");
    }

    #[test]
    fn first_message_captures_first_user_message_only() {
        let mut s = state();
        assert!(s.first_message.is_none());
        s.push_user_message(Some("the original ask".to_string()), Vec::new());
        s.push_user_message(Some("a follow up".to_string()), Vec::new());
        assert_eq!(s.first_message.as_deref(), Some("the original ask"));
    }

    #[test]
    fn first_message_ignores_empty_text() {
        let mut s = state();
        s.push_user_message(Some("   ".to_string()), Vec::new());
        assert!(s.first_message.is_none());
        s.push_user_message(Some("real".to_string()), Vec::new());
        assert_eq!(s.first_message.as_deref(), Some("real"));
    }

    #[test]
    fn reset_for_session_clears_first_message() {
        let mut s = state();
        s.push_user_message(Some("ask".to_string()), Vec::new());
        s.reset_for_session("sess-2".to_string(), None);
        assert!(s.first_message.is_none());
    }

    #[test]
    fn load_history_replays_transcript_and_seeds_first_message() {
        use crate::client::MessageEntry;
        let mut s = state();
        s.reset_for_session("sess-resume".to_string(), None);
        let before = s.entries.len();
        s.load_history(vec![
            MessageEntry {
                role: "user".to_string(),
                content: "first ask".to_string(),
            },
            MessageEntry {
                role: "assistant".to_string(),
                content: "reply".to_string(),
            },
            MessageEntry {
                role: "system".to_string(),
                content: "ignored".to_string(),
            },
            MessageEntry {
                role: "user".to_string(),
                content: "second ask".to_string(),
            },
        ]);
        // User + assistant + user replayed; system dropped.
        assert_eq!(s.entries.len(), before + 3);
        // First user message seeds the pinned recovery row.
        assert_eq!(s.first_message.as_deref(), Some("first ask"));
    }
}
