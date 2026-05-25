use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
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
    ApprovalDecision, RpcClient, RpcNotification, SessionEntry, SessionPromptResult, SessionUpdate,
    method, parse_session_update,
};
use crate::diff;
use crate::input_bar::{InputBarAction, InputBarState};
use crate::mouse;
use crate::theme;
use crate::turn_status::TurnStatus;
use zeroclaw_api::jsonrpc::RpcOutbound;

// Height of the approval popup anchored to the bottom of the content area.
// Used both in render_approval_overlay and to pad diffs so they aren't covered.
const APPROVAL_OVERLAY_HEIGHT: u16 = 7;

// ── Chat pane (tab mode) ─────────────────────────────────────────

enum ChatPhase {
    /// Showing agent picker (or loading the list).
    PickAgent {
        agents: Vec<String>,
        list_state: ListState,
        loading: bool,
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
    pub(crate) fn name(self) -> &'static str {
        match self {
            PaneKind::Chat => "Chat",
            PaneKind::Acp => "ACP",
        }
    }
}

pub(crate) struct Chat<'a> {
    rpc: &'a RpcClient,
    rpc_out: Arc<RpcOutbound>,
    notif_rx: broadcast::Receiver<RpcNotification>,
    turn_result_tx: mpsc::Sender<anyhow::Result<SessionPromptResult>>,
    turn_result_rx: mpsc::Receiver<anyhow::Result<SessionPromptResult>>,
    phase: ChatPhase,
    pane_kind: PaneKind,
}

impl<'a> Chat<'a> {
    pub(crate) fn new(rpc: &'a RpcClient, pane_kind: PaneKind) -> Self {
        let (turn_result_tx, turn_result_rx) = mpsc::channel(4);
        Self {
            rpc,
            rpc_out: rpc.rpc.clone(),
            notif_rx: rpc.subscribe_notifications(),
            turn_result_tx,
            turn_result_rx,
            phase: ChatPhase::PickAgent {
                agents: Vec::new(),
                list_state: ListState::default(),
                loading: true,
            },
            pane_kind,
        }
    }

    /// Fetch agent list. If exactly one enabled agent, auto-start a session.
    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        let agents = match self.rpc.agents_status().await {
            Ok(result) => result
                .agents
                .into_iter()
                .filter(|a| a.enabled)
                .map(|a| a.alias)
                .collect::<Vec<_>>(),
            Err(e) => {
                self.phase = ChatPhase::Error(format!("Failed to fetch agents: {e}"));
                return Ok(());
            }
        };

        if agents.is_empty() {
            self.phase = ChatPhase::Error(
                "No enabled agents. Configure an agent in the Config tab.".to_string(),
            );
            return Ok(());
        }

        if agents.len() == 1 {
            self.start_session(&agents[0]).await;
            return Ok(());
        }

        let mut list_state = ListState::default();
        list_state.select(Some(0));
        self.phase = ChatPhase::PickAgent {
            agents,
            list_state,
            loading: false,
        };
        Ok(())
    }

    async fn start_session(&mut self, agent_alias: &str) {
        // Over Unix socket, pass local CWD so the agent works in the
        // directory the TUI was launched from.  Over WSS the server
        // ignores this and uses the agent's workspace dir.
        let local_cwd = if self.rpc.transport() == crate::client::Transport::Unix {
            std::env::current_dir().ok()
        } else {
            None
        };
        let cwd_str = local_cwd.as_deref().and_then(|p| p.to_str());
        let result = if self.pane_kind == PaneKind::Acp {
            self.rpc.session_new_acp(agent_alias, cwd_str, None).await
        } else {
            self.rpc.session_new(agent_alias, cwd_str).await
        };
        match result {
            Ok(session) => {
                let mut state = ChatState::new(session.session_id, agent_alias.to_string());
                // Only ACP shows the working directory above the input bar.
                if self.pane_kind == PaneKind::Acp {
                    state.cwd = session.workspace_dir;
                }
                self.phase = ChatPhase::Active(Box::new(state));
            }
            Err(e) => {
                self.phase = ChatPhase::Error(format!("Failed to create session: {e}"));
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

    fn drain_turn_results(&mut self) {
        while let Ok(result) = self.turn_result_rx.try_recv() {
            if let ChatPhase::Active(ref mut state) = self.phase {
                match result {
                    Ok(r) => state.commit_turn(r.content),
                    Err(e) => state.commit_turn(format!("[error: {e}]")),
                }
            }
        }
    }

    // ── Drawing ──────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        self.drain_notifications();
        self.drain_turn_results();

        match &mut self.phase {
            ChatPhase::PickAgent {
                agents,
                list_state,
                loading,
            } => {
                draw_agent_picker(
                    frame,
                    area,
                    agents,
                    list_state,
                    *loading,
                    self.pane_kind.name(),
                );
            }
            ChatPhase::Active(state) => {
                render(frame, state, area);
            }
            ChatPhase::Error(msg) => {
                draw_error(frame, area, msg, self.pane_kind.name());
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
                match key.code {
                    KeyCode::Up => {
                        let i = list_state.selected().unwrap_or(0);
                        list_state.select(Some(i.saturating_sub(1)));
                    }
                    KeyCode::Down => {
                        let i = list_state.selected().unwrap_or(0);
                        if i + 1 < agents.len() {
                            list_state.select(Some(i + 1));
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(i) = list_state.selected()
                            && let Some(alias) = agents.get(i).cloned()
                        {
                            self.start_session(&alias).await;
                        }
                    }
                    KeyCode::Char('q') => return true,
                    _ => {}
                }
                return false;
            }
            ChatPhase::Error(_) => {
                return matches!(key.code, KeyCode::Char('q'));
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
                match key.code {
                    KeyCode::Up => {
                        let i = list_state.selected().unwrap_or(0);
                        list_state.select(Some(i.saturating_sub(1)));
                    }
                    KeyCode::Down => {
                        let i = list_state.selected().unwrap_or(0);
                        if i + 1 < sessions.len() {
                            list_state.select(Some(i + 1));
                        }
                    }
                    KeyCode::Enter => {
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
                                for m in msgs.messages {
                                    match m.role.as_str() {
                                        "user" => {
                                            state.entries.push(ChatEntry::UserMessage {
                                                text: Some(m.content),
                                                attachments: vec![],
                                            });
                                        }
                                        "assistant" => {
                                            state.entries.push(ChatEntry::AgentMessage(m.content));
                                        }
                                        _ => {}
                                    }
                                }
                                state.mark_dirty_full(); // bulk session load
                            }
                        }
                    }
                    KeyCode::Esc => {
                        state.session_overlay = SessionOverlay::None;
                    }
                    _ => {}
                }
                return false;
            }
            SessionOverlay::Rename { buf } => {
                match key.code {
                    KeyCode::Enter => {
                        let name = std::mem::take(buf);
                        if !name.is_empty()
                            && self
                                .rpc
                                .session_rename(&state.session_id, &name)
                                .await
                                .is_ok()
                        {
                            state.session_name = Some(name);
                        }
                        state.session_overlay = SessionOverlay::None;
                    }
                    KeyCode::Esc => {
                        state.session_overlay = SessionOverlay::None;
                    }
                    KeyCode::Char(c) => {
                        buf.push(c);
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    _ => {}
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
                    let tx = self.turn_result_tx.clone();
                    let transport = self.rpc.transport();
                    zeroclaw_spawn::spawn!(async move {
                        let mut params = serde_json::json!({
                            "session_id": sid,
                            "prompt": prompt,
                        });
                        if !attachments.is_empty() {
                            match build_attachments_json(&attachments, transport) {
                                Ok(att_json) => {
                                    params["attachments"] = serde_json::Value::Array(att_json);
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(e)).await;
                                    return;
                                }
                            }
                        }
                        let result = RpcClient::call_static::<SessionPromptResult>(
                            &rpc_arc,
                            method::SESSION_PROMPT,
                            params,
                        )
                        .await;
                        let _ = tx.send(result).await;
                    });
                    return false;
                }
                InputBarAction::StatusMessage(msg) => {
                    state.entries.push(ChatEntry::SystemMessage(msg));
                    state.mark_dirty_append();
                    return false;
                }
                InputBarAction::ToggleThinking => {
                    state.show_thoughts = !state.show_thoughts;
                    state.mark_dirty_full();
                    let status = if state.show_thoughts {
                        "Thinking output: visible"
                    } else {
                        "Thinking output: hidden"
                    };
                    state
                        .entries
                        .push(ChatEntry::SystemMessage(status.to_string()));
                    state.mark_dirty_append();
                    return false;
                }
                InputBarAction::Consumed => return false,
                InputBarAction::NotHandled => { /* fall through to chat-specific keys */ }
            }
        }

        // ── Chat-specific key handling ───────────────────────────
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if state.turn_in_flight {
                    let _ = self.rpc.session_cancel(&state.session_id).await;
                    state.turn_in_flight = false;
                    state.turn_status = TurnStatus::Idle;
                } else {
                    return true;
                }
            }
            KeyCode::Esc => {
                if state.in_browse_mode() {
                    // Return to input mode from browse mode.
                    state.exit_browse_mode();
                } else if state.turn_in_flight {
                    let _ = self.rpc.session_cancel(&state.session_id).await;
                    state.turn_in_flight = false;
                    state.turn_status = TurnStatus::Idle;
                }
                // Esc never quits the TUI — use q or Ctrl+C for that.
            }
            KeyCode::Enter if state.pending_approval().is_some() => {
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
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
            KeyCode::Char('a') if state.pending_approval().is_some() => {
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
            KeyCode::Char('e') if state.pending_approval().is_some() => {
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
            // ── Session management ───────────────────────────────
            KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !state.turn_in_flight =>
            {
                let local_cwd = if self.rpc.transport() == crate::client::Transport::Unix {
                    std::env::current_dir().ok()
                } else {
                    None
                };
                let cwd_str = local_cwd.as_deref().and_then(|p| p.to_str());
                let new_session = if self.pane_kind == PaneKind::Acp {
                    self.rpc.session_new_acp(&state.agent_alias, cwd_str, None).await
                } else {
                    self.rpc.session_new(&state.agent_alias, cwd_str).await
                };
                if let Ok(s) = new_session {
                    let _ = self.rpc.session_close(&state.session_id).await;
                    state.reset_for_session(s.session_id, None);
                    if self.pane_kind == PaneKind::Acp {
                        state.cwd = s.workspace_dir;
                    }
                }
            }
            KeyCode::Char('s')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !state.turn_in_flight =>
            {
                if let Ok(list) = self.rpc.session_list(None).await {
                    let chat_sessions: Vec<_> = list
                        .sessions
                        .into_iter()
                        .filter(|s| s.channel_id.is_none())
                        .collect();
                    let mut ls = ListState::default();
                    if !chat_sessions.is_empty() {
                        ls.select(Some(0));
                    }
                    state.session_overlay = SessionOverlay::List {
                        sessions: chat_sessions,
                        list_state: ls,
                    };
                }
            }
            KeyCode::Char('r')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !state.turn_in_flight =>
            {
                state.session_overlay = SessionOverlay::Rename { buf: String::new() };
            }
            // ── Thought toggle ───────────────────────────────────
            KeyCode::Char('t')
                if state.input_bar.input().is_empty()
                    && state.pending_approval().is_none()
                    && !state.in_browse_mode() =>
            {
                state.show_thoughts = !state.show_thoughts;
                state.mark_dirty_full();
            }
            // ── Browse mode: enter (Ctrl+↑) ──────────────────────
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !state.in_browse_mode() {
                    state.enter_browse_mode();
                } else {
                    state.browse_move_up(1, false);
                }
            }
            // ── Browse mode: exit (Ctrl+↓) ───────────────────────
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if state.in_browse_mode() {
                    state.exit_browse_mode();
                }
            }
            // ── Browse mode: navigate ↑/↓ ────────────────────────
            KeyCode::Up if state.in_browse_mode() => {
                state.browse_move_up(1, false);
            }
            KeyCode::Down if state.in_browse_mode() => {
                state.browse_move_down(1, false);
            }
            // ── Browse mode: range extend (Shift+↑/↓) ───────────
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if state.in_browse_mode() {
                    state.browse_move_up(1, true);
                } else {
                    state.scroll_up(1);
                }
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if state.in_browse_mode() {
                    state.browse_move_down(1, true);
                } else {
                    state.scroll_down(1);
                }
            }
            // ── Browse mode: fast scroll (Ctrl+Shift+↑/↓) ───────
            KeyCode::Up
                if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) =>
            {
                state.scroll_up(5);
            }
            KeyCode::Down
                if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) =>
            {
                state.scroll_down(5);
            }
            // ── Browse mode: vim-style cursor (j/k, input empty) ─
            KeyCode::Char('k')
                if state.in_browse_mode()
                    && state.pending_approval().is_none()
                    && !state.turn_in_flight =>
            {
                state.browse_move_up(1, false);
            }
            KeyCode::Char('j')
                if state.in_browse_mode()
                    && state.pending_approval().is_none()
                    && !state.turn_in_flight =>
            {
                state.browse_move_down(1, false);
            }
            // ── Browse mode: yank selection ──────────────────────
            KeyCode::Char('y') if state.in_browse_mode() => {
                if let Some((lo, hi)) = state.browse_range() {
                    let text = state.entries[lo..=hi]
                        .iter()
                        .map(clipboard_text)
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    crate::mouse::copy_osc52(&text);
                }
            }
            _ => {}
        }
        false
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
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
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        if mouse::in_rect(col, row, overlay_area) {
                            let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                            let count = sessions.len();
                            let i = list_state.selected().unwrap_or(0);
                            list_state.select(Some(mouse::list_scroll(i, count, up, 1)));
                        }
                    }
                    _ => {}
                }
                return;
            }

            match mouse.kind {
                MouseEventKind::ScrollUp => state.scroll_up(3),
                MouseEventKind::ScrollDown => state.scroll_down(3),
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
            state.entries.push(ChatEntry::SystemMessage(msg));
            state.mark_dirty_append();
        }
    }

    /// Returns true when the pane is accepting text input (blocks `?` help).
    ///
    /// In active chat: text input mode is on when the user has started typing
    /// (non-empty input buffer) and is not in selection mode or an overlay.
    /// When input is empty we're in "command" mode — single-char keybindings
    /// like `t`, `j`, `k`, `y`, `?` should work.
    pub(crate) fn wants_text_input(&self) -> bool {
        match &self.phase {
            ChatPhase::Active(s) => {
                // Overlay has its own key handling (Rename captures chars).
                if matches!(s.session_overlay, SessionOverlay::Rename { .. }) {
                    return true;
                }
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

impl<'a> crate::widgets::HelpContext for Chat<'a> {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::{HelpEntry as E, HelpNode};
        match &self.phase {
            ChatPhase::PickAgent { loading, .. } => {
                if *loading {
                    HelpNode::entries(vec![E::key("", "Loading agents…")])
                } else {
                    HelpNode::entries(vec![
                        E::new(vec!["↑", "↓"], "Navigate"),
                        E::key("Enter", "Select agent"),
                        E::key("q", "Quit"),
                    ])
                }
            }
            ChatPhase::Error(_) => HelpNode::entries(vec![E::key("q", "Quit")]),
            ChatPhase::Active(state) => {
                match &state.session_overlay {
                    SessionOverlay::List { .. } => {
                        return HelpNode::entries(vec![
                            E::new(vec!["↑", "↓"], "Navigate"),
                            E::key("Enter", "Switch session"),
                            E::key("Esc", "Close"),
                        ]);
                    }
                    SessionOverlay::Rename { .. } => {
                        return HelpNode::entries(vec![
                            E::key("Enter", "Submit name"),
                            E::key("Esc", "Cancel"),
                        ]);
                    }
                    SessionOverlay::None => {}
                }
                if state.pending_approval().is_some() {
                    return HelpNode::entries(vec![
                        E::key("Enter", "Approve"),
                        E::key("a", "Always approve"),
                        E::key("Ctrl+D", "Deny"),
                        E::key("Ctrl+C", "Cancel turn"),
                    ]);
                }
                if state.in_browse_mode() {
                    return HelpNode::entries(vec![
                        E::new(vec!["↑", "k"], "Move cursor up"),
                        E::new(vec!["↓", "j"], "Move cursor down"),
                        E::key("Shift+↑/↓", "Extend selection"),
                        E::key("y", "Yank selection"),
                        E::new(vec!["Ctrl+↓", "Esc"], "Return to input"),
                    ]);
                }
                if state.turn_in_flight {
                    return HelpNode::entries(vec![E::new(vec!["Ctrl+C", "Esc"], "Cancel turn")]);
                }
                // Idle: compose pane-level bindings + input bar as child.
                let pane = HelpNode::entries(vec![
                    E::key("Ctrl+↑", "Browse mode"),
                    E::key("Shift+↑/↓", "Scroll conversation"),
                    E::key("t", "Toggle thoughts"),
                    E::key("/toggle-thinking", "Toggle thinking visibility"),
                    E::spacer(),
                    E::key("Ctrl+N", "New session"),
                    E::key("Ctrl+S", "Session list"),
                    E::key("Ctrl+R", "Rename session"),
                ]);
                pane.with_child(state.input_bar.help_context())
            }
        }
    }
}

// ── Agent picker rendering ───────────────────────────────────────

fn draw_agent_picker(
    frame: &mut Frame,
    area: Rect,
    agents: &[String],
    list_state: &mut ListState,
    loading: bool,
    tab_title: &str,
) {
    let block = Block::default()
        .title(Span::styled(format!(" {tab_title} "), theme::title_style()))
        .borders(Borders::ALL)
        .border_style(theme::dim_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if loading {
        let p = Paragraph::new("Loading agents...")
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
        return;
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
        Span::styled("Select an agent ", theme::body_style()),
        Span::styled("(Up/Down, Enter)", theme::dim_style()),
    ]));
    frame.render_widget(header, chunks[0]);

    let items: Vec<ListItem> = agents
        .iter()
        .map(|a| ListItem::new(Span::styled(a.as_str(), theme::body_style())))
        .collect();
    let list = List::new(items).highlight_style(theme::list_highlight_style());
    frame.render_stateful_widget(list, chunks[1], list_state);
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

    let p = Paragraph::new(Line::from(Span::styled(
        msg,
        theme::error_style(),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(p, chunks[1]);
}

// ── Active chat rendering ────────────────────────────────────────

fn render(f: &mut Frame, state: &mut ChatState, area: Rect) {
    let show_cursor = state.pending_approval().is_none();
    let turn_status = state.turn_status.clone();
    let turn_started_at = state.turn_started_at;
    let conv_area = state.input_bar.render(
        f,
        area,
        state.turn_in_flight,
        show_cursor,
        &turn_status,
        turn_started_at,
    );

    // Optional CWD line just above the input bar (bottom of conv_area).
    let actual_conv = if let Some(ref cwd) = state.cwd {
        if conv_area.height > 1 {
            let cwd_row = Rect::new(
                conv_area.x,
                conv_area.y + conv_area.height - 1,
                conv_area.width,
                1,
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {} ", cwd),
                    theme::dim_style(),
                ))),
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
        SessionOverlay::Rename { buf } => {
            render_rename_overlay(f, area, buf);
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
    input: &serde_json::Value,
    result: Option<&str>,
) {
    lines.push(Line::from(vec![Span::styled(
        format!("[tool: {name}] "),
        theme::tool_label_style(),
    )]));

    match name {
        "file_edit" => {
            let old = input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let ext = file_ext(input);
            lines.extend(diff::diff_lines(old, new, ext));
        }
        "file_write" => {
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let ext = file_ext(input);
            lines.extend(diff::write_lines(content, ext));
        }
        _ => {
            let summary = serde_json::to_string(input).unwrap_or_default();
            let truncated = if summary.len() > 120 {
                format!("{}…", truncate_utf8(&summary, 120))
            } else {
                summary
            };
            lines.push(Line::from(Span::styled(
                format!("  {truncated}"),
                theme::dim_style(),
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
            theme::dim_style(),
        )));
    }
}

/// Render a single committed entry into `lines`.
/// Extracted so both the incremental-append and full-rebuild paths in
/// `rebuild_lines` share identical rendering logic.
fn render_entry_into(
    entry: &ChatEntry,
    is_selected: bool,
    show_thoughts: bool,
    lines: &mut Vec<Line<'static>>,
) {
    let sel_mod = if is_selected {
        Modifier::REVERSED
    } else {
        Modifier::empty()
    };
    match entry {
        ChatEntry::UserMessage { text, attachments } => {
            let mut spans = vec![Span::styled(
                "You: ",
                theme::user_label_style().add_modifier(sel_mod),
            )];
            if let Some(t) = text {
                spans.push(Span::styled(
                    t.clone(),
                    theme::body_style().add_modifier(sel_mod),
                ));
            }
            if !attachments.is_empty() {
                let label = attachments.join(", ");
                spans.push(Span::styled(
                    format!(" [{label}]"),
                    theme::warn_style().add_modifier(Modifier::ITALIC | sel_mod),
                ));
            }
            lines.push(Line::from(spans));
        }
        ChatEntry::AgentMessage(text) => {
            lines.push(Line::from(vec![Span::styled(
                "Agent: ",
                theme::agent_label_style().add_modifier(sel_mod),
            )]));
            let md_lines = markdown_to_lines(text);
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
                    Span::styled(
                        "(thinking) ",
                        theme::thought_style().add_modifier(sel_mod),
                    ),
                    Span::styled(
                        text.clone(),
                        theme::dim_style().add_modifier(sel_mod),
                    ),
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
            input,
            result,
            ..
        } => {
            render_tool_entry(lines, name, input, result.as_deref());
        }
    }
}

fn render_conversation(f: &mut Frame, state: &mut ChatState, area: Rect) {
    // ── Rebuild cached lines only when entries changed ────────
    if state.dirty != LinesDirty::Clean {
        state.rebuild_lines();
    }

    // ── Assemble final lines: cached + streaming ─────────────
    let mut lines: Vec<Line> = state.cached_lines.clone();

    // Streaming text (in-flight agent response).
    if !state.streaming_text.is_empty() {
        let prefix = Line::from(vec![Span::styled(
            "Agent: ",
            theme::agent_label_style(),
        )]);
        lines.push(prefix);
        lines.extend(markdown_to_lines(&state.streaming_text));
    }

    // Streaming thought (in-flight).
    if state.show_thoughts && !state.streaming_thought.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "(thinking) ",
                theme::thought_style(),
            ),
            Span::styled(
                state.streaming_thought.clone(),
                theme::dim_style(),
            ),
        ]));
    }

    // When the approval overlay is visible it covers the bottom
    // APPROVAL_OVERLAY_HEIGHT rows.  Push blank lines so the diff/content
    // above it stays readable and the user can scroll to see it.
    if state.pending_approval().is_some() {
        for _ in 0..APPROVAL_OVERLAY_HEIGHT {
            lines.push(Line::default());
        }
    }

    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", state.title()));

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    // Use ratatui's own line_count for accurate wrapped row total.
    let total_rows = p.line_count(inner_width) as u16;
    let max_scroll = total_rows.saturating_sub(inner_height);
    let scroll = if state.pinned_to_bottom {
        max_scroll
    } else {
        state.scroll_offset.min(max_scroll)
    };

    let p = p.scroll((scroll, 0));
    f.render_widget(p, area);

    state.last_total_rows = total_rows;
    state.last_inner_height = inner_height;
    state.scroll_offset = scroll;

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
    let keys = if is_edit_tool {
        "Enter=Allow  a=Always  Ctrl+D=Reject  e=Edit"
    } else {
        "Enter=Allow  a=Always  Ctrl+D=Reject"
    };

    // For file_edit/file_write, strip the bulk content fields — the diff
    // preview in the conversation already shows old/new content.
    let summary = if is_edit_tool {
        strip_content_fields(&pa.arguments_summary)
    } else {
        pa.arguments_summary.clone()
    };

    let text = if summary.is_empty() {
        format!(
            "Approve tool call: {}  [{}s]\n\n  {keys}",
            pa.tool_name, pa.timeout_secs
        )
    } else {
        format!(
            "Approve tool call: {}  [{}s]\n\n  {summary}\n\n  {keys}",
            pa.tool_name, pa.timeout_secs
        )
    };

    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Approval Required ")
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
        .title(" Sessions (Enter=switch, Esc=close) ")
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

fn render_rename_overlay(f: &mut Frame, area: Rect, buf: &str) {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Length(5),
            Constraint::Min(0),
        ])
        .split(area);
    let overlay_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Min(30),
            Constraint::Percentage(20),
        ])
        .split(vert[1])[1];

    f.render_widget(Clear, overlay_area);

    let text = format!("New name: {buf}\u{2588}\n\nEnter=submit  Esc=cancel");
    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Rename Session ")
                .style(theme::overlay_border_style()),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, overlay_area);
}

// ── Markdown rendering ───────────────────────────────────────────

fn markdown_to_lines(text: &str) -> Vec<Line<'static>> {
    let opts = MdOptions::empty();
    let parser = MdParser::new_ext(text, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut in_bold = false;
    let mut in_italic = false;
    let mut in_code_block = false;
    for event in parser {
        match event {
            MdEvent::Start(Tag::Strong) => in_bold = true,
            MdEvent::End(TagEnd::Strong) => in_bold = false,
            MdEvent::Start(Tag::Emphasis) => in_italic = true,
            MdEvent::End(TagEnd::Emphasis) => in_italic = false,
            MdEvent::Start(Tag::CodeBlock(_)) => {
                // Flush current line.
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                in_code_block = true;
            }
            MdEvent::End(TagEnd::CodeBlock) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                in_code_block = false;
            }
            MdEvent::Start(Tag::Item) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                current_spans.push(Span::styled(
                    "  \u{2022} ",
                    theme::dim_style(),
                ));
            }
            MdEvent::End(TagEnd::Item) if !current_spans.is_empty() => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
            MdEvent::Start(Tag::Paragraph) => {}
            MdEvent::End(TagEnd::Paragraph) if !current_spans.is_empty() => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
            MdEvent::Text(t) => {
                let owned = t.to_string();
                if in_code_block {
                    for code_line in owned.split('\n') {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(std::mem::take(&mut current_spans)));
                        }
                        current_spans.push(Span::styled(
                            format!("\u{2502} {code_line}"),
                            theme::code_block_style(),
                        ));
                    }
                } else {
                    let mut style = Style::default();
                    if in_bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if in_italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    current_spans.push(Span::styled(owned, style));
                }
            }
            MdEvent::Code(t) => {
                current_spans.push(Span::styled(
                    t.to_string(),
                    theme::code_inline_style(),
                ));
            }
            MdEvent::SoftBreak => {
                current_spans.push(Span::raw(" "));
            }
            MdEvent::HardBreak if !current_spans.is_empty() => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
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

// ── ChatState / ChatEntry ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub arguments_summary: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub enum ChatEntry {
    AgentMessage(String),
    AgentThought(String),
    /// Local system/info message (e.g. "Attached: photo.png").
    SystemMessage(String),
    UserMessage {
        text: Option<String>,
        attachments: Vec<String>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        result: Option<String>,
    },
}

#[derive(Debug)]
enum SessionOverlay {
    None,
    List {
        sessions: Vec<SessionEntry>,
        list_state: ListState,
    },
    Rename {
        buf: String,
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

#[derive(Debug)]
pub struct ChatState {
    pub session_id: String,
    pub agent_alias: String,
    session_name: Option<String>,
    /// Working directory for this session (shown above input bar).
    pub cwd: Option<String>,
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
    session_overlay: SessionOverlay,
    scroll_offset: u16,
    pinned_to_bottom: bool,
    last_total_rows: u16,
    last_inner_height: u16,
    /// Cached rendered lines from committed entries.
    cached_lines: Vec<Line<'static>>,
    /// Fine-grained dirty tracking — see [`LinesDirty`].
    dirty: LinesDirty,
    /// How many entries from `entries[cached_render_start..]` are represented in
    /// `cached_lines`.  Valid only when `dirty != Full`.
    cached_entry_count: usize,
    /// The `entries` index where the render window starts for the current cache.
    cached_render_start: usize,
}

impl ChatState {
    pub fn new(session_id: String, agent_alias: String) -> Self {
        Self {
            session_id,
            agent_alias,
            session_name: None,
            cwd: None,
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
            session_overlay: SessionOverlay::None,
            scroll_offset: 0,
            pinned_to_bottom: true,
            last_total_rows: 0,
            last_inner_height: 0,
            cached_lines: Vec::new(),
            dirty: LinesDirty::Full,
            cached_entry_count: 0,
            cached_render_start: 0,
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
        if len == 0 { return; }
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
        if len == 0 { return; }
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
        self.browse_range().map_or(false, |(lo, hi)| idx >= lo && idx <= hi)
    }

    /// Rebuild (or incrementally extend) the cached rendered lines from committed entries.
    fn rebuild_lines(&mut self) {
        // Cap the render window so cached_lines (and its per-frame clone) stays
        // bounded regardless of conversation length.  Selected entries are always
        // included even if they fall before the window.
        const MAX_RENDERED_ENTRIES: usize = 1_000;
        let total = self.entries.len();
        let natural_start = total.saturating_sub(MAX_RENDERED_ENTRIES);
        // Ensure the browse selection range is always visible.
        let start = if let Some((lo, _hi)) = self.browse_range() {
            natural_start.min(lo)
        } else {
            natural_start
        };

        // ── Incremental append path ───────────────────────────────
        // When only new tail entries were appended and the window hasn't shifted,
        // extend the existing cache instead of rebuilding from scratch.  This avoids
        // re-running markdown_to_lines on every prior AgentMessage.
        if self.dirty == LinesDirty::Appended && start == self.cached_render_start {
            let render_from = start + self.cached_entry_count;
            let show_thoughts = self.show_thoughts;
            let mut new_lines = Vec::new();
            for (rel_idx, entry) in self.entries[render_from..].iter().enumerate() {
                let abs_idx = render_from + rel_idx;
                render_entry_into(
                    entry,
                    self.is_in_browse_range(abs_idx),
                    show_thoughts,
                    &mut new_lines,
                );
            }
            self.cached_lines.extend(new_lines);
            self.cached_entry_count = total - start;
            self.dirty = LinesDirty::Clean;
            return;
        }

        // ── Full rebuild path ─────────────────────────────────────
        let mut lines = Vec::new();
        let show_thoughts = self.show_thoughts;
        for (rel_idx, entry) in self.entries[start..].iter().enumerate() {
            let abs_idx = start + rel_idx;
            render_entry_into(entry, self.is_in_browse_range(abs_idx), show_thoughts, &mut lines);
        }
        self.cached_lines = lines;
        self.cached_entry_count = total - start;
        self.cached_render_start = start;
        self.dirty = LinesDirty::Clean;
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
        match &self.session_name {
            Some(name) => format!("{} — {}", self.agent_alias, name),
            None => self.agent_alias.clone(),
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
            self.entries.push(ChatEntry::AgentThought(thought));
            self.mark_dirty_append();
        }
    }

    /// Commit any accumulated streaming text as an `AgentMessage` entry.
    /// Called when a tool call interrupts the text stream so that pre-tool
    /// text is committed in conversation order before the `Tool` entry.
    fn flush_streaming_text(&mut self) {
        let text = std::mem::take(&mut self.streaming_text);
        if !text.is_empty() {
            self.entries.push(ChatEntry::AgentMessage(text));
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
            | SessionUpdate::ApprovalRequest { session_id, .. } => session_id.as_str(),
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
                self.turn_status = TurnStatus::Responding;
            }
            SessionUpdate::AgentThoughtChunk { text, .. } => {
                self.streaming_thought.push_str(&text);
                self.turn_status = TurnStatus::Thinking;
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
                self.turn_status = TurnStatus::CallingTool(name.clone());
                self.entries.push(ChatEntry::Tool {
                    tool_call_id,
                    name,
                    input: raw_input,
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
                        && id == &tool_call_id
                    {
                        *result = Some(raw_output);
                        self.mark_dirty_full(); // mutation of existing entry
                        break;
                    }
                }
                // Tool finished; we're back in the model's hands. Don't clobber
                // a more specific status if one has already arrived (chunks can
                // race the result), so only step down from the matching
                // CallingTool state.
                if matches!(self.turn_status, TurnStatus::CallingTool(_)) {
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
                self.turn_status = TurnStatus::WaitingForApproval;
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
        self.entries
            .push(ChatEntry::UserMessage { text, attachments });
        self.mark_dirty_append();
        self.turn_in_flight = true;
        // Start a fresh status + animation anchor. We're `Working` until the
        // first chunk (thought / message / tool-call) tells us otherwise.
        self.turn_status = TurnStatus::Working;
        self.turn_started_at = Instant::now();
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
        self.pending_approval = None;
        self.turn_in_flight = false;
        self.turn_status = TurnStatus::Idle;
        self.browse_cursor = None;
        self.browse_anchor = None;
    }
}

fn clipboard_text(entry: &ChatEntry) -> String {
    match entry {
        ChatEntry::UserMessage { text, attachments } => {
            let base = text.as_deref().unwrap_or("");
            if attachments.is_empty() {
                format!("You: {base}")
            } else {
                format!("You: {base} [{}]", attachments.join(", "))
            }
        }
        ChatEntry::AgentMessage(t) => format!("Agent: {t}"),
        ChatEntry::AgentThought(t) => format!("(thinking) {t}"),
        ChatEntry::SystemMessage(t) => t.clone(),
        ChatEntry::Tool {
            name,
            input,
            result,
            ..
        } => {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            match result {
                Some(r) => format!("[tool: {name}] {input_str}\n  \u{2514}\u{2500} {r}"),
                None => format!("[tool: {name}] {input_str}"),
            }
        }
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
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    );

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

    #[tokio::test]
    async fn launch_editor_returns_original_on_empty_write() {
        // Use `true` as the editor — it exits immediately without modifying the file.
        // SAFETY: test-only, single-threaded context.
        unsafe { std::env::set_var("EDITOR", "true") };
        let original = "let x = 1;".to_string();
        let result = open_editor_for_content(&original).await;
        // `true` writes nothing, so the original is returned unchanged.
        assert_eq!(result, original);
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
        assert!(matches!(&s.entries()[0], ChatEntry::AgentThought(t) if t == "plan: run ls"));
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
        assert!(matches!(&s.entries()[0], ChatEntry::AgentThought(t) if t == "thinking"));
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
            matches!(&s.entries()[0], ChatEntry::AgentMessage(t) if t == "I will run ls."),
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
            matches!(&s.entries()[0], ChatEntry::AgentMessage(t) if t == "Running ls."),
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
            matches!(&s.entries()[2], ChatEntry::AgentMessage(t) if t == "Done."),
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
        assert!(matches!(&s.entries()[0], ChatEntry::AgentMessage(t) if t == "Before tool."));
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
                .any(|e| matches!(e, ChatEntry::AgentMessage(t) if t == "Done"))
        );
    }
}
