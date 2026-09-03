use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use tokio::sync::mpsc;

use crate::acp;
use crate::chat;
use crate::client::{ConnectionState, RpcClient, StatusResult};
use crate::config;
use crate::config_manager;
use crate::dashboard;
use crate::doctor;
use crate::keymap::{GlobalAction, ModalAction, SearchBoxAction};
use crate::logs;
use crate::mouse;
use crate::quickstart_pane;
use crate::sop_pane;
use crate::theme;
use crate::widgets::{CtxBar, HelpContext, HelpEntry, HelpNode};

/// Pending Quickstart chat transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingQuickstartChat {
    /// Open the created agent after the daemon reconnects.
    AfterReconnect(String),
    /// Open the created agent on the current live connection.
    Immediate(String),
}

/// State that must survive a reconnect — used by Quickstart's
/// Stage-2 flow to route the user into the freshly-created agent's
/// chat after the daemon comes back up.
#[derive(Debug, Default)]
pub struct CrossReconnectState {
    /// The single pending handoff target for Quickstart-created agents.
    pub pending_quickstart_chat: Option<PendingQuickstartChat>,
}

pub type SharedReconnectState = Arc<Mutex<CrossReconnectState>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickstartChatDrain {
    Immediate,
    AfterReconnect,
}

/// How often the UI redraws when no input arrives (for live panes).
const TICK: Duration = Duration::from_millis(200);
const CHROME_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_COALESCED_MOUSE_DRAGS: usize = 64;
const SGR_MOUSE_SEQUENCE_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_SGR_MOUSE_SEQUENCE_EVENTS: usize = 32;

/// Reassembles SGR mouse sequences that crossterm exposed as individual key
/// events after an input read split the sequence at the escape byte.
///
/// A complete sequence is converted back into the same `MouseEvent` that
/// crossterm would have produced. Invalid or incomplete sequences are replayed
/// in order, so ordinary Escape-prefixed keyboard input is not discarded.
#[derive(Debug, Default)]
struct SgrMouseEventDecoder {
    pending: VecDeque<Event>,
    candidate: Vec<Event>,
    candidate_started_at: Option<Instant>,
}

impl SgrMouseEventDecoder {
    fn feed(&mut self, event: Event) {
        let output = if self.candidate.is_empty() {
            if is_sgr_mouse_start(&event) {
                self.candidate.push(event);
                self.candidate_started_at = Some(Instant::now());
                Vec::new()
            } else {
                vec![event]
            }
        } else {
            self.candidate.push(event);
            self.decode_candidate()
        };
        self.pending.extend(output);
    }

    fn next(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }

    fn push_front(&mut self, event: Event) {
        self.pending.push_front(event);
    }

    fn poll_timeout(&self) -> Duration {
        self.candidate_started_at
            .map(|started| SGR_MOUSE_SEQUENCE_TIMEOUT.saturating_sub(started.elapsed()))
            .unwrap_or(TICK)
    }

    fn flush_candidate(&mut self) -> bool {
        if self.candidate.is_empty() {
            return false;
        }
        self.pending.extend(self.candidate.drain(..));
        self.candidate_started_at = None;
        true
    }

    fn flush_timed_out_candidate(&mut self) -> bool {
        let timed_out = self
            .candidate_started_at
            .is_some_and(|started| started.elapsed() >= SGR_MOUSE_SEQUENCE_TIMEOUT);
        timed_out && self.flush_candidate()
    }

    fn read_ready(&mut self) -> Result<Option<Event>> {
        self.read_ready_with(|| {
            if event::poll(Duration::ZERO)? {
                Ok(Some(event::read()?))
            } else {
                Ok(None)
            }
        })
    }

    fn read_ready_with<F>(&mut self, mut read_event: F) -> Result<Option<Event>>
    where
        F: FnMut() -> Result<Option<Event>>,
    {
        loop {
            if let Some(event) = self.next() {
                return Ok(Some(event));
            }
            let Some(event) = read_event()? else {
                return Ok(None);
            };
            self.feed(event);
        }
    }

    fn decode_candidate(&mut self) -> Vec<Event> {
        if self.candidate.len() > MAX_SGR_MOUSE_SEQUENCE_EVENTS {
            return self.replay_candidate();
        }

        let Some(chars) = self.candidate[1..]
            .iter()
            .map(key_event_char)
            .collect::<Option<Vec<_>>>()
        else {
            return self.replay_candidate();
        };

        let prefix = ['[', '<'];
        for (index, expected) in prefix.iter().enumerate() {
            let Some(actual) = chars.get(index) else {
                return Vec::new();
            };
            if actual != expected {
                return self.replay_candidate();
            }
        }

        let Some(final_char) = chars.last().copied() else {
            return Vec::new();
        };
        if !matches!(final_char, 'M' | 'm') {
            if chars[2..]
                .iter()
                .all(|character| character.is_ascii_digit() || *character == ';')
            {
                return Vec::new();
            }
            return self.replay_candidate();
        }

        let Some(mouse) = parse_sgr_mouse(&chars) else {
            return self.replay_candidate();
        };
        self.candidate.clear();
        self.candidate_started_at = None;
        vec![Event::Mouse(mouse)]
    }

    fn replay_candidate(&mut self) -> Vec<Event> {
        self.candidate_started_at = None;
        std::mem::take(&mut self.candidate)
    }
}

fn is_sgr_mouse_start(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.code == KeyCode::Esc // keyguard: recognize terminal protocol escape, not an app chord
                && key.kind == KeyEventKind::Press
                && key.modifiers == KeyModifiers::NONE
    )
}

fn key_event_char(event: &Event) -> Option<char> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(character), // keyguard: extract terminal protocol byte
            kind: KeyEventKind::Press,
            ..
        }) => Some(*character),
        _ => None,
    }
}

fn parse_sgr_mouse(chars: &[char]) -> Option<MouseEvent> {
    let final_char = *chars.last()?;
    let payload: String = chars[2..chars.len().checked_sub(1)?].iter().collect();
    let payload = payload.strip_suffix(';').unwrap_or(payload.as_str());
    let fields: Vec<_> = payload.split(';').collect();
    if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
        return None;
    }
    let cb = fields[0].parse::<u8>().ok()?;
    let column = fields[1].parse::<u16>().ok()?;
    let row = fields[2].parse::<u16>().ok()?;
    if column == 0 || row == 0 {
        return None;
    }

    let (mut kind, modifiers) = parse_sgr_mouse_button(cb)?;
    if final_char == 'm'
        && let MouseEventKind::Down(button) = kind
    {
        kind = MouseEventKind::Up(button);
    }

    Some(MouseEvent {
        kind,
        column: column - 1,
        row: row - 1,
        modifiers,
    })
}

fn parse_sgr_mouse_button(cb: u8) -> Option<(MouseEventKind, KeyModifiers)> {
    let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    let dragging = cb & 0b0010_0000 == 0b0010_0000;
    let kind = match (button_number, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3, true) | (4, true) | (5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };

    let mut modifiers = KeyModifiers::NONE;
    if cb & 0b0000_0100 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if cb & 0b0000_1000 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if cb & 0b0001_0000 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    Some((kind, modifiers))
}

/// Returns whether an application-level confirmation modal owns an input event.
///
/// Confirmation dialogs sit above every pane and intentionally consume paste
/// events so text or file paths cannot mutate a hidden composer.
fn confirmation_modal_owns_event(event: &Event, reload_confirm: bool, quit_confirm: bool) -> bool {
    matches!(event, Event::Paste(_)) && (reload_confirm || quit_confirm)
}

fn mouse_drag_button(event: &Event) -> Option<crossterm::event::MouseButton> {
    match event {
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Drag(button) => Some(button),
            _ => None,
        },
        _ => None,
    }
}

fn coalesce_mouse_drag<F>(mut current: Event, mut read_queued: F) -> Result<(Event, Option<Event>)>
where
    F: FnMut() -> Result<Option<Event>>,
{
    let Some(button) = mouse_drag_button(&current) else {
        return Ok((current, None));
    };

    let mut coalesced = 1;
    while coalesced < MAX_COALESCED_MOUSE_DRAGS {
        let Some(next) = read_queued()? else {
            return Ok((current, None));
        };
        if mouse_drag_button(&next) == Some(button) {
            current = next;
            coalesced += 1;
        } else {
            return Ok((current, Some(next)));
        }
    }
    Ok((current, None))
}

/// Ephemeral interaction state for the keybinding overlay. Keybinding
/// metadata itself stays in the action/help registry and is resolved on draw.
#[derive(Debug, Default)]
struct HelpOverlayState {
    query: String,
    scroll: usize,
}

impl HelpOverlayState {
    /// Handle a key while the overlay has focus. Returns `true` when it
    /// should close.
    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        match SearchBoxAction::from_chord(key) {
            Some(SearchBoxAction::Cancel) if self.query.is_empty() => return true,
            Some(SearchBoxAction::Cancel) => {
                self.query.clear();
                self.scroll = 0;
            }
            Some(SearchBoxAction::Backspace) => {
                self.query.pop();
                self.scroll = 0;
            }
            Some(SearchBoxAction::Up) => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            Some(SearchBoxAction::Down) => {
                self.scroll = self.scroll.saturating_add(1);
            }
            Some(SearchBoxAction::Accept) => {}
            None => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.query.push(c);
                    self.scroll = 0;
                }
            }
        }
        false
    }
}

/// Mode bar entries. Shared between drawing and click detection.
const MODES: &[Mode] = &[
    Mode::Dashboard,
    Mode::Config,
    Mode::Acp,
    Mode::Chat,
    Mode::Logs,
    Mode::Doctor,
    Mode::Quickstart,
    Mode::Sop,
];

// ── Mode enum ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Dashboard,
    Config,
    Doctor,
    Acp, // displayed as "Code" in the UI
    Chat,
    Logs,
    Quickstart,
    Sop,
}

#[derive(Debug, Clone)]
struct ModeBarEntry {
    mode: Mode,
    title: String,
    hit_rect: Rect,
}

/// Exact geometry produced for the most recently rendered mode bar.
///
/// Drawing and mouse dispatch both consume this value, so a clipped or hidden
/// tab can never retain a larger synthetic click target.
#[derive(Debug, Clone, Default)]
struct ModeBarLayout {
    tab_area: Rect,
    summary_area: Option<Rect>,
    entries: Vec<ModeBarEntry>,
}

impl ModeBarLayout {
    fn mode_at(&self, column: u16, row: u16) -> Option<Mode> {
        if self
            .summary_area
            .is_some_and(|area| mouse::in_rect(column, row, area))
            || !mouse::in_rect(column, row, self.tab_area)
        {
            return None;
        }
        self.entries
            .iter()
            .find(|entry| mouse::in_rect(column, row, entry.hit_rect))
            .map(|entry| entry.mode)
    }
}

#[derive(Default)]
struct ChromeStatus {
    status: Option<StatusResult>,
    health: Option<serde_json::Value>,
    last_poll: Option<Instant>,
    refresh_in_flight: bool,
    refresh_rx: Option<mpsc::UnboundedReceiver<ChromeStatusSnapshot>>,
}

struct ChromeStatusSnapshot {
    status: Option<StatusResult>,
    health: Option<serde_json::Value>,
}

impl ChromeStatus {
    fn tick(&mut self, rpc: &Arc<RpcClient>) {
        self.drain_completed_refresh();
        let due = self
            .last_poll
            .map(|t| t.elapsed() >= CHROME_STATUS_POLL_INTERVAL)
            .unwrap_or(true);
        if due && !self.refresh_in_flight {
            self.start_poll(rpc);
        }
    }

    fn start_poll(&mut self, rpc: &Arc<RpcClient>) {
        self.last_poll = Some(Instant::now());
        self.refresh_in_flight = true;
        let (tx, rx) = mpsc::unbounded_channel();
        self.refresh_rx = Some(rx);
        let rpc = Arc::clone(rpc);
        tokio::spawn(async move {
            let status = rpc.status().await.ok();
            let health = rpc.health().await.ok();
            let _ = tx.send(ChromeStatusSnapshot { status, health });
        });
    }

    fn drain_completed_refresh(&mut self) {
        let Some(rx) = self.refresh_rx.as_mut() else {
            return;
        };

        match rx.try_recv() {
            Ok(snapshot) => {
                if let Some(status) = snapshot.status {
                    self.status = Some(status);
                }
                if let Some(health) = snapshot.health {
                    self.health = Some(health);
                }
                self.refresh_in_flight = false;
                self.refresh_rx = None;
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.refresh_in_flight = false;
                self.refresh_rx = None;
            }
        }
    }

    fn clear(&mut self) {
        self.status = None;
        self.health = None;
        self.last_poll = None;
        self.refresh_in_flight = false;
        self.refresh_rx = None;
    }

    fn summary_line(&self) -> Option<Line<'static>> {
        let status = self.status.as_ref()?;
        let mut text = format!(
            " v{} {}:{}",
            status.server_version,
            crate::i18n::t("zc-chrome-summary-sessions"),
            status.active_sessions
        );
        text.push_str(&process_stats_summary(self.health.as_ref()));
        text.push(' ');
        Some(Line::from(Span::styled(text, theme::dim_style())))
    }
}

impl Mode {
    fn fluent_key(self) -> &'static str {
        match self {
            Mode::Dashboard => "zc-pane-dashboard",
            Mode::Config => "zc-pane-config",
            Mode::Doctor => "zc-pane-doctor",
            Mode::Acp => "zc-pane-code",
            Mode::Chat => "zc-pane-chat",
            Mode::Logs => "zc-pane-logs",
            Mode::Quickstart => "zc-pane-quickstart",
            Mode::Sop => "zc-pane-sop",
        }
    }

    fn cycle(self, offset: isize) -> Mode {
        let len = MODES.len() as isize;
        let cur = MODES
            .iter()
            .position(|m| *m == self)
            .expect("mode missing from MODES") as isize;
        let next = ((cur + offset).rem_euclid(len)) as usize;
        MODES[next]
    }
}

async fn switch_mode(
    mode: &mut Mode,
    next: Mode,
    conn_state: &ConnectionState,
    dashboard_pane: &mut dashboard::Dashboard,
    quickstart: &mut quickstart_pane::QuickstartPane,
    acp_pane: &mut acp::Acp,
    chat_pane: &mut chat::Chat,
    sop_pane: &mut sop_pane::SopPane,
) {
    if *mode == Mode::Dashboard && next != Mode::Dashboard {
        dashboard_pane.on_pane_blur();
    }
    if *mode == Mode::Quickstart && next != Mode::Quickstart {
        quickstart.dismiss_beacon();
    }
    if *mode == Mode::Sop && next != Mode::Sop {
        sop_pane.on_pane_blur();
    }
    if !matches!(conn_state, ConnectionState::Disconnected { .. }) {
        match next {
            Mode::Acp => acp_pane.refresh_if_inactive().await,
            Mode::Chat => chat_pane.refresh_if_inactive().await,
            Mode::Sop => sop_pane.refresh(),
            _ => {}
        }
    }
    *mode = next;
}

fn take_pending_quickstart_chat(
    reconnect_state: &SharedReconnectState,
    drain: QuickstartChatDrain,
) -> Option<String> {
    let Ok(mut guard) = reconnect_state.lock() else {
        return None;
    };
    let pending = guard.pending_quickstart_chat.take()?;
    match (drain, pending) {
        (QuickstartChatDrain::Immediate, PendingQuickstartChat::Immediate(alias))
        | (QuickstartChatDrain::AfterReconnect, PendingQuickstartChat::AfterReconnect(alias)) => {
            Some(alias)
        }
        (_, other) => {
            guard.pending_quickstart_chat = Some(other);
            None
        }
    }
}

async fn consume_pending_quickstart_chat(
    conn_state: &ConnectionState,
    reconnect_state: &SharedReconnectState,
    mode: &mut Mode,
    chat_pane: &mut chat::Chat,
) {
    if matches!(conn_state, ConnectionState::Disconnected { .. }) {
        return;
    }
    let Some(alias) = take_pending_quickstart_chat(reconnect_state, QuickstartChatDrain::Immediate)
    else {
        return;
    };
    chat_pane.focus_agent(&alias).await;
    *mode = Mode::Chat;
}

// ── Top-level entry point ────────────────────────────────────────

/// Run the TUI event loop. Owns the full session lifecycle: when the
/// daemon disconnects it reconnects in-loop (keeping the cached UI alive
/// and responsive) and rebuilds its panes against the recovered client.
/// Returns when the user quits.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    rpc: Arc<RpcClient>,
    term: &mut config_manager::Term,
    connect_label: &str,
    insecure_tls: bool,
    reconnect_state: SharedReconnectState,
    config_dir: &std::path::Path,
    target: &crate::ConnectTarget,
    owns_ephemeral: bool,
    initial_leg: crate::ActiveLeg,
) -> Result<()> {
    let mut mode = Mode::Dashboard;
    theme::set_agent_overrides(resolve_agent_overrides(config_dir));
    let mut help_overlay: Option<HelpOverlayState> = None;
    let mut reload_confirm = false;
    let mut quit_confirm = false;
    let mut reload_status: Option<String> = None;
    let mut mode_bar_layout = ModeBarLayout::default();
    let mut content_area = Rect::default();
    let mut reconnect_last_attempt: Option<std::time::Instant> = None;
    let mut ephemeral_respawn_done = false;
    let mut needs_intervention = false;

    // Which transport leg the live connection sits on, and when the direct path
    // was last re-probed. While on the relay leg of a route that also has a
    // direct address, the loop periodically retries direct and migrates back.
    let mut active_leg = initial_leg;
    let mut reprobe_last_attempt: Option<std::time::Instant> = None;

    // The live client handle. Reassigned in place on a successful
    // reconnect so every rebuilt pane talks to the recovered daemon.
    let mut rpc = rpc;

    macro_rules! build_panes {
        ($resume_chat:expr, $resume_acp:expr) => {
            async {
                let mut dashboard_pane =
                    dashboard::Dashboard::new(rpc.clone(), connect_label, insecure_tls);
                dashboard_pane.init().await?;
                let mut config_app = config_manager::App::new(rpc.clone(), config_dir);
                config_app.init().await?;
                let doctor_pane = doctor::Doctor::new(rpc.clone());
                let mut acp_pane = acp::Acp::new(rpc.clone());
                // Carry the pre-disconnect session across a reconnect rebuild so
                // the rebuilt pane resumes the daemon-retained session
                // instead of minting a fresh one. None on first build.
                acp_pane.set_resume_session_id($resume_acp.0);
                acp_pane.set_resume_agent_alias($resume_acp.1);
                acp_pane.init().await?;
                let mut chat_pane = chat::Chat::new(rpc.clone(), chat::PaneKind::Chat);
                chat_pane.set_resume_session_id($resume_chat.0);
                chat_pane.set_resume_agent_alias($resume_chat.1);
                chat_pane.init().await?;
                let pending_start_chat = take_pending_quickstart_chat(
                    &reconnect_state,
                    QuickstartChatDrain::AfterReconnect,
                );
                let mut logs_pane = logs::Logs::new(rpc.clone());
                logs_pane.init().await?;
                let mut quickstart =
                    quickstart_pane::QuickstartPane::new(rpc.clone(), Arc::clone(&reconnect_state));
                quickstart.init().await?;
                let sop_pane = sop_pane::SopPane::new(rpc.clone());
                if let Some(alias) = pending_start_chat {
                    chat_pane.focus_agent(&alias).await;
                    mode = Mode::Chat;
                }
                anyhow::Ok((
                    dashboard_pane,
                    config_app,
                    doctor_pane,
                    acp_pane,
                    chat_pane,
                    logs_pane,
                    quickstart,
                    sop_pane,
                ))
            }
            .await
        };
    }

    let (
        mut dashboard_pane,
        mut config_app,
        mut doctor_pane,
        mut acp_pane,
        mut chat_pane,
        mut logs_pane,
        mut quickstart,
        mut sop_pane,
    ) = build_panes!(
        (None::<String>, None::<String>),
        (None::<String>, None::<String>)
    )?;
    let mut chrome_status = ChromeStatus::default();
    chrome_status.tick(&rpc);
    let mut input_decoder = SgrMouseEventDecoder::default();

    // Adopt a freshly-connected client: rebuild every pane against it (a kept
    // pane would still hold the dead client's notification receiver) while
    // carrying the live sessions + agent aliases across the rebuild. Evaluates to
    // `true` when the rebuild succeeded, `false` when the daemon flapped mid-init
    // (the caller stays in its current state and retries). Shared by the
    // disconnect-recovery path and the relay->direct re-probe migration.
    macro_rules! adopt_client {
        ($new_client:expr) => {{
            // Transactional: `rpc` and the panes must always describe the SAME
            // connection. The new client has to be installed first because the
            // panes are built against it, so a failed rebuild puts the previous
            // one back rather than leaving a live `rpc` beside panes that still
            // hold the old, possibly dead, connection.
            let previous = Arc::clone(&rpc);
            rpc = Arc::new($new_client);
            let resume_chat = (
                chat_pane.current_session_id().map(String::from),
                chat_pane.current_agent_alias().map(String::from),
            );
            let resume_acp = (
                acp_pane.current_session_id().map(String::from),
                acp_pane.current_agent_alias().map(String::from),
            );
            match build_panes!(resume_chat, resume_acp) {
                Ok(mut panes) => {
                    refresh_visible_sop_after_reconnect(mode, &mut panes.7).await;
                    // Assigned as one tuple: every pane the builder produces is
                    // adopted, and a pane added to `build_panes!` later cannot
                    // be left behind on the old client without failing to
                    // compile here.
                    (
                        dashboard_pane,
                        config_app,
                        doctor_pane,
                        acp_pane,
                        chat_pane,
                        logs_pane,
                        quickstart,
                        sop_pane,
                    ) = panes;
                    // No pane holds the replaced connection any more. Nothing
                    // else would ever stop it: its reader, writer, and relay
                    // pump are detached tasks with no destructor to reach them.
                    previous.shutdown();
                    true
                }
                Err(_) => {
                    let abandoned = std::mem::replace(&mut rpc, previous);
                    abandoned.shutdown();
                    false
                }
            }
        }};
    }

    'event_loop: loop {
        // Draw
        let conn_state = rpc.connection_state();
        if matches!(conn_state, ConnectionState::Disconnected { .. }) {
            chrome_status.clear();
        } else {
            chrome_status.tick(&rpc);
        }
        let chrome_summary = chrome_status.summary_line();
        doctor_pane.poll_refresh().await;
        if mode == Mode::Doctor && !matches!(conn_state, ConnectionState::Disconnected { .. }) {
            doctor_pane.refresh_if_inactive();
        }
        let base_theme = theme::active_raw();
        let frame_theme = match mode {
            Mode::Acp => acp_pane.selected_agent().and_then(theme::agent_override),
            Mode::Chat => chat_pane.selected_agent().and_then(theme::agent_override),
            _ => None,
        };
        if let Some(t) = frame_theme {
            theme::set_active(t);
        }

        term.draw(|frame| {
            // Theme backdrop: paint the whole screen with the active
            // theme's background first so every pane inherits it. The
            // `terminal` theme returns None and the user's own shell
            // colours show through.
            if let Some(style) = theme::backdrop_style() {
                frame.render_widget(
                    ratatui::widgets::Block::default().style(style),
                    frame.area(),
                );
            }
            // The info bar appears as a dedicated row between the content and
            // the status bar, only while the active pane has a message to show.
            let info_message = match mode {
                Mode::Chat => chat_pane.info_message().cloned(),
                _ => None,
            };
            let has_info = info_message.is_some();
            let constraints: Vec<Constraint> = if has_info {
                vec![
                    Constraint::Length(1), // mode bar
                    Constraint::Min(0),    // content
                    Constraint::Length(1), // info bar
                    Constraint::Length(1), // status bar
                ]
            } else {
                vec![
                    Constraint::Length(1), // mode bar
                    Constraint::Min(0),    // content
                    Constraint::Length(1), // status bar
                ]
            };
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(frame.area());

            mode_bar_layout = draw_mode_bar(frame, chunks[0], mode, chrome_summary.as_ref());
            content_area = chunks[1];

            match mode {
                Mode::Dashboard => dashboard_pane.draw(
                    frame,
                    chunks[1],
                    chrome_status.status.as_ref(),
                    chrome_status.health.as_ref(),
                    acp_pane.current_cwd(),
                    chat_pane.current_cwd(),
                ),
                Mode::Config => config_app.draw_into(frame, chunks[1]),
                Mode::Doctor => doctor_pane.draw(frame, chunks[1]),
                Mode::Acp => acp_pane.draw(frame, chunks[1]),
                Mode::Chat => chat_pane.draw(frame, chunks[1]),
                Mode::Logs => logs_pane.draw(frame, chunks[1]),
                Mode::Quickstart => quickstart.draw(frame, chunks[1]),
                Mode::Sop => sop_pane.render(frame, chunks[1]),
            }

            let status_idx = if has_info {
                // Render the info bar in its own row above the status bar.
                let info_area = chunks[2];
                let bar = crate::widgets::InfoBar::new(info_message.as_ref());
                if let Some(widget) = bar.widget(info_area.width as usize) {
                    frame.render_widget(widget, info_area);
                }
                3
            } else {
                2
            };

            let (ctx_input, ctx_max) = match mode {
                Mode::Chat => chat_pane.ctx_tokens(),
                Mode::Acp => acp_pane.ctx_tokens(),
                _ => (None, None),
            };
            let browse_mode = match mode {
                Mode::Chat => chat_pane.in_browse_mode(),
                Mode::Acp => acp_pane.in_browse_mode(),
                _ => false,
            };
            draw_status_bar(
                frame,
                chunks[status_idx],
                &conn_state,
                rpc.tui_id(),
                CtxBar::new(ctx_input, ctx_max),
                needs_intervention,
                browse_mode,
            );

            // Help modal overlay (drawn last so it sits on top).
            if let Some(state) = help_overlay.as_mut() {
                let mut node = HelpNode::entries(global_help_entries());
                let pane_node = match mode {
                    Mode::Dashboard => dashboard_pane.help_context(),
                    Mode::Config => config_app.help_context(),
                    Mode::Doctor => doctor_pane.help_context(),
                    Mode::Acp => acp_pane.help_context(),
                    Mode::Chat => chat_pane.help_context(),
                    Mode::Logs => logs_pane.help_context(),
                    Mode::Quickstart => quickstart.help_context(),
                    Mode::Sop => sop_pane.help_context(),
                };
                node.children.push(pane_node);
                draw_help_modal(frame, frame.area(), &node, state);
            }

            if reload_confirm {
                draw_reload_confirm_modal(frame, frame.area());
            }
            if quit_confirm {
                draw_quit_confirm_modal(frame, frame.area());
            }
            if let Some(msg) = &reload_status {
                draw_reload_status_toast(frame, frame.area(), msg);
            }
        })?;

        // Restore the base palette so the override never leaks into the next
        // frame, a different pane, or live theme changes from the Config pane.
        if frame_theme.is_some() {
            theme::set_active(base_theme);
        }

        // Recovery stays inside the responsive event loop. During each disconnected
        // episode an owned ephemeral daemon is respawned at most once, attached daemons
        // are never spawned, and both modes keep polling for manual recovery.
        if matches!(rpc.connection_state(), ConnectionState::Disconnected { .. }) {
            if owns_ephemeral && !ephemeral_respawn_done {
                ephemeral_respawn_done = true;
                if let crate::ConnectTarget::LocalSocket(socket) = target {
                    let _ = crate::spawn_ephemeral_daemon(config_dir, socket);
                }
            }

            {
                let now = std::time::Instant::now();
                let due = reconnect_last_attempt
                    .map(|t| now.duration_since(t) >= Duration::from_secs(1))
                    .unwrap_or(true);
                if due {
                    reconnect_last_attempt = Some(now);
                    // Reclaim the same TUI identity so the daemon restores
                    // our UID via HMAC signature verification.
                    let prev_id = rpc.tui_id().map(String::from);
                    let prev_sig = rpc.tui_sig().map(String::from);
                    // The connect prefers the direct path and falls back to the
                    // relay, so a reconnect lands on whichever leg is reachable.
                    if let Ok((new_client, leg)) = target
                        .connect(prev_id.as_deref(), prev_sig.as_deref())
                        .await
                    {
                        // A reconnect may land on a DIFFERENT leg than the one
                        // that dropped: direct can fall back to the relay, and a
                        // relay session can come back direct. The leg is
                        // committed only once the panes run on that client.
                        let adopted = adopt_client!(new_client);
                        active_leg = leg_after_adoption(adopted, active_leg, leg);
                        if adopted {
                            chrome_status.clear();
                            chrome_status.tick(&rpc);
                            reconnect_last_attempt = None;
                            ephemeral_respawn_done = false;
                            needs_intervention = false;
                        }
                        // Adopted or not, go round again: a failed rebuild means
                        // the daemon flapped mid-init, and the previous client is
                        // back in place for the next throttle window.
                        continue;
                    } else if owns_ephemeral && ephemeral_respawn_done {
                        // The one permitted respawn did not come back — flag
                        // for the user. We keep polling above, so a manual
                        // daemon restart still recovers.
                        needs_intervention = true;
                    }
                }
            }
        }

        // Migrate back to the direct path once it becomes reachable again. Only
        // runs while live on the relay leg of a route that also has a direct
        // address; a successful probe adopts the direct client and rebuilds the
        // panes against it. The probe is throttled and short-timeout, so the
        // event loop keeps drawing between attempts.
        if matches!(rpc.connection_state(), ConnectionState::Connected)
            && active_leg == crate::ActiveLeg::WssRelay
            && let crate::ConnectTarget::Wss(route) = target
            && route.reprobe_secs > 0
            && route.direct_url.is_some()
        {
            let now = std::time::Instant::now();
            let due = reprobe_last_attempt
                .map(|t| now.duration_since(t) >= Duration::from_secs(route.reprobe_secs))
                .unwrap_or(true);
            if due {
                reprobe_last_attempt = Some(now);
                let prev_id = rpc.tui_id().map(String::from);
                let prev_sig = rpc.tui_sig().map(String::from);
                if let Ok(direct) = route
                    .connect_direct(prev_id.as_deref(), prev_sig.as_deref())
                    .await
                {
                    // Committed after the rebuild, not before: a probe that
                    // connects but cannot be adopted leaves the session on the
                    // relay, and claiming the direct leg there would stop the
                    // re-probe that is still the only way back.
                    let adopted = adopt_client!(direct);
                    active_leg =
                        leg_after_adoption(adopted, active_leg, crate::ActiveLeg::WssDirect);
                    continue;
                }
            }
        }

        let input_event = loop {
            if let Some(event) = input_decoder.next() {
                break event;
            }

            // Poll for input with a timeout so live panes refresh periodically.
            // A shorter deadline while an Escape-prefixed sequence is being
            // assembled keeps an ordinary Escape key responsive.
            if !event::poll(input_decoder.poll_timeout())? {
                if input_decoder.flush_timed_out_candidate() {
                    continue;
                }
                if matches!(conn_state, ConnectionState::Disconnected { .. }) {
                    continue 'event_loop;
                }
                if mode == Mode::Dashboard {
                    dashboard_pane.tick().await;
                }
                if mode == Mode::Logs {
                    logs_pane.tick().await;
                }
                if mode == Mode::Quickstart {
                    quickstart.tick().await;
                }
                if mode == Mode::Sop {
                    sop_pane.tick();
                }
                consume_pending_quickstart_chat(
                    &conn_state,
                    &reconnect_state,
                    &mut mode,
                    &mut chat_pane,
                )
                .await;
                continue 'event_loop;
            }
            input_decoder.feed(event::read()?);
        };
        let (input_event, next_pending) =
            coalesce_mouse_drag(input_event, || input_decoder.read_ready())?;
        if let Some(next_pending) = next_pending {
            input_decoder.push_front(next_pending);
        }

        if confirmation_modal_owns_event(&input_event, reload_confirm, quit_confirm) {
            // The visible confirmation modal is the authoritative input
            // owner; discard paste instead of forwarding it underneath.
            continue;
        }

        match input_event {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }

                let in_text_input = match mode {
                    Mode::Dashboard => dashboard_pane.wants_text_input(),
                    Mode::Config => config_app.wants_text_input(),
                    Mode::Doctor => doctor_pane.wants_text_input(),
                    Mode::Acp => acp_pane.wants_text_input(),
                    Mode::Chat => chat_pane.wants_text_input(),
                    Mode::Logs => logs_pane.wants_text_input(),
                    Mode::Quickstart => quickstart.wants_text_input(),
                    Mode::Sop => sop_pane.wants_text_input(),
                };
                let global = GlobalAction::from_chord(&key);

                // Quit-confirm modal. The first exit chord closes any open
                // transient widgets and arms the modal; a second exit chord —
                // or an explicit confirm — actually quits. Cancel dismisses.
                if quit_confirm {
                    match ModalAction::from_chord(&key) {
                        Some(ModalAction::Confirm) => break,
                        Some(ModalAction::Cancel) => {
                            quit_confirm = false;
                        }
                        _ => {
                            if global == Some(GlobalAction::Quit) {
                                break;
                            }
                        }
                    }
                    continue;
                }

                let pane_wants_quit_chord = match mode {
                    Mode::Chat => chat_pane.wants_quit_chord(),
                    Mode::Acp => acp_pane.wants_quit_chord(),
                    _ => false,
                };
                if global == Some(GlobalAction::Quit) && !pane_wants_quit_chord {
                    // First Ctrl+C: clear input bar text, clear transient
                    // state (browse mode, overlay, …) and arm the confirm modal.
                    match mode {
                        Mode::Chat => {
                            chat_pane.exit_browse_mode();
                            chat_pane.clear_input();
                        }
                        Mode::Acp => {
                            acp_pane.exit_browse_mode();
                            acp_pane.clear_input();
                        }
                        _ => {}
                    }
                    help_overlay = None;
                    reload_confirm = false;
                    reload_status = None;
                    quit_confirm = true;
                    continue;
                }

                // Reload-daemon confirmation modal — intercepts all keys
                // while open. Mirrors the web dashboard's
                // `ReloadDaemonButton` confirm flow.
                if reload_confirm {
                    match ModalAction::from_chord(&key) {
                        Some(ModalAction::Confirm) => {
                            reload_confirm = false;
                            reload_status = Some(match rpc.config_reload().await {
                                Ok(_) => crate::i18n::t("zc-app-reload-status-signalled"),
                                Err(e) => format!("Reload requested ({e})"),
                            });
                        }
                        Some(ModalAction::Cancel) => {
                            reload_confirm = false;
                        }
                        _ => {}
                    }
                    continue;
                }

                // Any pending reload-status toast clears on the next key.
                if reload_status.is_some() {
                    reload_status = None;
                }

                if global == Some(GlobalAction::ReloadDaemon) && !in_text_input {
                    reload_confirm = true;
                    continue;
                }

                // The help overlay owns keyboard focus while open. Its opening
                // chord toggles it closed; Esc clears a filter before closing.
                if let Some(state) = help_overlay.as_mut() {
                    if global == Some(GlobalAction::Help) || state.handle_key(&key) {
                        help_overlay = None;
                    }
                    continue;
                }

                let editor_claims_pane_navigation = matches!(
                    global,
                    Some(GlobalAction::PaneNavLeft | GlobalAction::PaneNavRight)
                ) && match mode {
                    Mode::Config => config_app.claims_pane_navigation(&key),
                    Mode::Acp => acp_pane.claims_pane_navigation(&key),
                    Mode::Chat => chat_pane.claims_pane_navigation(&key),
                    Mode::Sop => sop_pane.claims_pane_navigation(&key),
                    _ => false,
                };
                // Disconnected panes are skipped below to avoid dead-socket RPCs,
                // so a retained editor cannot consume its local cursor chord.
                let pane_can_receive_editor_chord =
                    !matches!(conn_state, ConnectionState::Disconnected { .. });
                let switch_to = pane_switch_delta(
                    global,
                    editor_claims_pane_navigation,
                    pane_can_receive_editor_chord,
                )
                .map(|delta| mode.cycle(delta));
                if let Some(next) = switch_to {
                    switch_mode(
                        &mut mode,
                        next,
                        &conn_state,
                        &mut dashboard_pane,
                        &mut quickstart,
                        &mut acp_pane,
                        &mut chat_pane,
                        &mut sop_pane,
                    )
                    .await;
                    continue;
                }

                if global == Some(GlobalAction::Help)
                    && (!in_text_input || crate::keymap::help_bypasses_text_input(&key))
                {
                    help_overlay = Some(HelpOverlayState::default());
                    continue;
                }

                // Skip pane key handlers when disconnected — they may
                // issue RPC calls that hang on the dead socket.
                if matches!(conn_state, ConnectionState::Disconnected { .. }) {
                    continue;
                }

                let quit = match mode {
                    Mode::Dashboard => dashboard_pane.handle_key(key).await,
                    Mode::Config => config_app.handle_key(key, term).await?,
                    Mode::Doctor => doctor_pane.handle_key(key).await,
                    Mode::Acp => acp_pane.handle_key(key, term).await,
                    Mode::Chat => chat_pane.handle_key(key, term).await,
                    Mode::Logs => logs_pane.handle_key(key).await,
                    Mode::Quickstart => quickstart.handle_key(key).await,
                    Mode::Sop => sop_pane.handle_key(key).await,
                };
                if quit {
                    break;
                }
                match mode {
                    Mode::Acp if acp_pane.take_help_request() => {
                        help_overlay = Some(HelpOverlayState::default());
                    }
                    Mode::Chat if chat_pane.take_help_request() => {
                        help_overlay = Some(HelpOverlayState::default());
                    }
                    _ => {}
                }
                if mode == Mode::Quickstart && quickstart.take_leave_request() {
                    switch_mode(
                        &mut mode,
                        Mode::Dashboard,
                        &conn_state,
                        &mut dashboard_pane,
                        &mut quickstart,
                        &mut acp_pane,
                        &mut chat_pane,
                        &mut sop_pane,
                    )
                    .await;
                }
                consume_pending_quickstart_chat(
                    &conn_state,
                    &reconnect_state,
                    &mut mode,
                    &mut chat_pane,
                )
                .await;
            }
            Event::Mouse(mouse) => {
                if let Some(state) = help_overlay.as_mut() {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            state.scroll = state.scroll.saturating_sub(3);
                        }
                        MouseEventKind::ScrollDown => {
                            state.scroll = state.scroll.saturating_add(3);
                        }
                        MouseEventKind::Down(_) => {
                            help_overlay = None;
                        }
                        _ => {}
                    }
                    continue;
                }
                // Mode bar clicks
                if matches!(mouse.kind, MouseEventKind::Down(_))
                    && let Some(next) = mode_bar_layout.mode_at(mouse.column, mouse.row)
                {
                    switch_mode(
                        &mut mode,
                        next,
                        &conn_state,
                        &mut dashboard_pane,
                        &mut quickstart,
                        &mut acp_pane,
                        &mut chat_pane,
                        &mut sop_pane,
                    )
                    .await;
                    continue;
                }
                // Help-hint click: every pane renders the `?=help` indicator at
                // the bottom-left of the content area; clicking it opens help,
                // mirroring the `?` key.
                if matches!(mouse.kind, MouseEventKind::Down(_))
                    && mouse::help_hint_click(mouse.column, mouse.row, content_area)
                {
                    help_overlay = Some(HelpOverlayState::default());
                    continue;
                }
                // Forward to active pane (skip when disconnected).
                if !matches!(conn_state, ConnectionState::Disconnected { .. }) {
                    match mode {
                        Mode::Dashboard => {
                            dashboard_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::Config => {
                            config_app.handle_mouse(mouse, content_area, term).await?;
                        }
                        Mode::Doctor => {
                            doctor_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::Logs => {
                            logs_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::Acp => {
                            acp_pane.handle_mouse(mouse, content_area).await;
                        }
                        Mode::Chat => {
                            chat_pane.handle_mouse(mouse, content_area).await;
                        }
                        Mode::Quickstart => {
                            quickstart.handle_mouse(mouse, content_area).await;
                        }
                        Mode::Sop => {
                            sop_pane.handle_mouse(mouse).await;
                        }
                    }
                    consume_pending_quickstart_chat(
                        &conn_state,
                        &reconnect_state,
                        &mut mode,
                        &mut chat_pane,
                    )
                    .await;
                }
            }
            Event::Paste(text) if help_overlay.is_some() => {
                if let Some(state) = help_overlay.as_mut() {
                    state
                        .query
                        .extend(text.chars().filter(|character| !character.is_control()));
                    state.scroll = 0;
                }
            }
            Event::Paste(text) if !matches!(conn_state, ConnectionState::Disconnected { .. }) => {
                match mode {
                    Mode::Chat => chat_pane.handle_paste(&text),
                    Mode::Acp => acp_pane.handle_paste(&text),
                    Mode::Config => config_app.handle_paste(&text),
                    Mode::Doctor => doctor_pane.handle_paste(&text),
                    Mode::Quickstart => quickstart.handle_paste(&text),
                    Mode::Dashboard => dashboard_pane.handle_paste(&text),
                    Mode::Logs => logs_pane.handle_paste(&text),
                    Mode::Sop => sop_pane.handle_paste(&text),
                }
                consume_pending_quickstart_chat(
                    &conn_state,
                    &reconnect_state,
                    &mut mode,
                    &mut chat_pane,
                )
                .await;
            }
            _ => {} // Resize, etc. — just redraw on next iteration
        }
    }

    Ok(())
}

/// A reconnect rebuilds every pane around the new RPC client. The SOP list is
/// loaded on focus rather than at construction, so an already-visible SOP pane
/// must run that same canonical refresh before replacing the disconnected pane.
/// The transport leg to run with after an adoption attempt.
///
/// `connected` is the leg the routing source of truth actually dialled, which is
/// not necessarily the one that dropped: a direct session reconnects onto the
/// relay when the direct address is still down, and a relay session can come
/// back direct. It is committed ONLY when the panes were rebuilt against that
/// client, because `active_leg` names the connection the panes hold and the
/// re-probe and failback loops read it to decide what to try next.
///
/// Discarding it leaves a direct-to-relay reconnect believing it is still
/// direct, so the failback loop that would migrate it back never runs; the
/// mirror case leaves a relay-to-direct reconnect believing it is on the relay,
/// which re-probes a direct path it is already using.
fn leg_after_adoption(
    adopted: bool,
    current: crate::ActiveLeg,
    connected: crate::ActiveLeg,
) -> crate::ActiveLeg {
    if adopted { connected } else { current }
}

async fn refresh_visible_sop_after_reconnect(mode: Mode, pane: &mut sop_pane::SopPane) {
    if mode == Mode::Sop {
        pane.refresh();
    }
}

fn global_help_entries() -> Vec<HelpEntry> {
    use crate::keymap::{GlobalAction, action_key_labels};

    let cycle_keys = action_key_labels(GlobalAction::PaneNavLeft)
        .into_iter()
        .chain(action_key_labels(GlobalAction::PaneNavRight));
    vec![
        HelpEntry::new(cycle_keys, crate::i18n::t("zc-app-help-cycle-mode")),
        HelpEntry::new(
            action_key_labels(GlobalAction::Help),
            crate::i18n::t("zc-app-help-help"),
        ),
        HelpEntry::new(
            action_key_labels(GlobalAction::ReloadDaemon),
            crate::i18n::t("zc-app-help-reload"),
        ),
        HelpEntry::new(
            action_key_labels(GlobalAction::Quit),
            crate::i18n::t("zc-app-help-quit"),
        ),
        HelpEntry::spacer(),
    ]
}

fn pane_switch_delta(
    global: Option<GlobalAction>,
    editor_claims_chord: bool,
    pane_can_receive_editor_chord: bool,
) -> Option<isize> {
    if editor_claims_chord && pane_can_receive_editor_chord {
        return None;
    }
    match global {
        Some(GlobalAction::PaneNavLeft) => Some(-1),
        Some(GlobalAction::PaneNavRight) => Some(1),
        _ => None,
    }
}

fn resolve_agent_overrides(
    config_dir: &std::path::Path,
) -> std::collections::HashMap<String, theme::Theme> {
    let mut out = std::collections::HashMap::new();
    let Ok(cfg) = config::ensure_and_load(config_dir) else {
        return out;
    };
    for alias in cfg.agent_override_aliases() {
        if let Ok(Some(t)) = cfg.resolve_agent_theme(alias) {
            out.insert(alias.to_string(), t);
        }
    }
    out
}

// ── Mode bar ─────────────────────────────────────────────────────

fn draw_mode_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    active: Mode,
    chrome_summary: Option<&Line<'static>>,
) -> ModeBarLayout {
    use ratatui::widgets::Tabs;

    let active_idx = MODES.iter().position(|m| *m == active).unwrap_or(0);
    let base_titles: Vec<String> = MODES
        .iter()
        .map(|mode| format!(" {} ", crate::i18n::t(mode.fluent_key())))
        .collect();

    // Chrome is informative; the selected navigation target is interactive.
    // Keep the full summary only when it leaves enough room for the active tab.
    let active_width = crate::display_width::display_width(&base_titles[active_idx]) as u16;
    let summary_width = chrome_summary
        .map(Line::width)
        .filter(|width| usize::from(area.width) >= width.saturating_add(active_width.into()))
        .map(|width| width.min(usize::from(u16::MAX)) as u16)
        .unwrap_or(0);
    let tab_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(summary_width),
        area.height,
    );
    let summary_area = (summary_width > 0)
        .then(|| Rect::new(tab_area.right(), area.y, summary_width, area.height));

    let (start, end, show_overflow_markers) =
        visible_mode_window(&base_titles, active_idx, usize::from(tab_area.width));
    let mut visible: Vec<(Mode, String)> = MODES[start..end]
        .iter()
        .copied()
        .zip(base_titles[start..end].iter().cloned())
        .collect();
    if show_overflow_markers && start > 0 {
        visible[0].1.insert(0, '‹');
    }
    if show_overflow_markers && end < MODES.len() {
        visible
            .last_mut()
            .expect("active mode is visible")
            .1
            .push('›');
    }

    let mut x = tab_area.x;
    let mut entries = Vec::with_capacity(visible.len());
    for (index, (mode, title)) in visible.into_iter().enumerate() {
        let remaining = tab_area.right().saturating_sub(x);
        if remaining == 0 {
            break;
        }
        let title_width = crate::display_width::display_width(&title) as u16;
        let rendered_width = title_width.min(remaining);
        entries.push(ModeBarEntry {
            mode,
            title,
            hit_rect: Rect::new(x, tab_area.y, rendered_width, tab_area.height),
        });
        x = x.saturating_add(rendered_width);
        if rendered_width < title_width {
            break;
        }
        if index + 1 < end - start && x < tab_area.right() {
            // Ratatui renders the one-column divider after each non-final title.
            x = x.saturating_add(1);
        }
    }

    let selected = entries.iter().position(|entry| entry.mode == active);
    let titles: Vec<ratatui::text::Line> = entries
        .iter()
        .map(|entry| {
            ratatui::text::Line::from(ratatui::text::Span::styled(
                entry.title.clone(),
                theme::body_style(),
            ))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(selected)
        .style(theme::bar_style())
        .highlight_style(theme::selected_style().add_modifier(Modifier::BOLD))
        .divider("│")
        .padding("", "");

    frame.render_widget(tabs, tab_area);
    if let (Some(summary), Some(summary_area)) = (chrome_summary, summary_area) {
        frame.render_widget(Paragraph::new(summary.clone()), summary_area);
    }

    ModeBarLayout {
        tab_area,
        summary_area,
        entries,
    }
}

/// Select a contiguous localized-title window containing `active_idx`.
/// Hidden neighbors are signaled with edge markers when those markers fit.
fn visible_mode_window(
    titles: &[String],
    active_idx: usize,
    available_width: usize,
) -> (usize, usize, bool) {
    let mut start = active_idx.min(titles.len().saturating_sub(1));
    let mut end = (start + 1).min(titles.len());

    loop {
        let mut expanded = false;
        if start > 0 && mode_window_width(titles, start - 1, end) <= available_width {
            start -= 1;
            expanded = true;
        }
        if end < titles.len() && mode_window_width(titles, start, end + 1) <= available_width {
            end += 1;
            expanded = true;
        }
        if !expanded {
            break;
        }
    }

    let markers_fit = mode_window_width(titles, start, end) <= available_width;
    (start, end, markers_fit)
}

fn mode_window_width(titles: &[String], start: usize, end: usize) -> usize {
    let title_width: usize = titles[start..end]
        .iter()
        .map(|title| crate::display_width::display_width(title))
        .sum();
    let dividers = end.saturating_sub(start + 1);
    let overflow_markers = usize::from(start > 0) + usize::from(end < titles.len());
    title_width + dividers + overflow_markers
}

// ── Status bar ───────────────────────────────────────────────────

const HEALTHY_GREEN: Color = Color::Rgb(80, 220, 120);
const DEAD_RED: Color = Color::Rgb(255, 80, 80);

fn draw_status_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &ConnectionState,
    tui_id: Option<&str>,
    ctx: CtxBar,
    needs_intervention: bool,
    browse_mode: bool,
) {
    let (dot, label, style) = match state {
        ConnectionState::Connected => (
            "\u{25cf}",
            " Connected".to_string(),
            Style::default().fg(HEALTHY_GREEN),
        ),
        ConnectionState::Disconnected { reason } if needs_intervention => (
            "\u{25cf}",
            format!(" Daemon unavailable — restart required ({reason})"),
            Style::default().fg(DEAD_RED),
        ),
        ConnectionState::Disconnected { reason } => (
            "\u{25cf}",
            format!(" Reconnecting… (reason: {reason})"),
            Style::default().fg(DEAD_RED),
        ),
    };

    // Show TUI ID prefix when connected and assigned.
    let id_span = match (state, tui_id) {
        (ConnectionState::Connected, Some(id)) => Some(Span::styled(
            format!("{id} "),
            Style::default().fg(HEALTHY_GREEN),
        )),
        _ => None,
    };

    let id_len = id_span.as_ref().map(|s| s.width()).unwrap_or(0);
    let conn_text_len = (id_len + 1 + label.len()) as u16; // id + dot + label

    // Split the row: ctx bar on the left, connection status on the right.
    // Right column is sized to exactly fit the conn text; left gets the rest.
    let right_w = conn_text_len.min(area.width);
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w)])
        .split(area);
    let left_area = chunks[0];
    let right_area = chunks[1];

    // Right: connection status, no leading padding (column is exact width).
    let mut spans = Vec::with_capacity(3);
    if let Some(id) = id_span {
        spans.push(id);
    }
    spans.push(Span::styled(dot, style));
    spans.push(Span::styled(label, style));
    frame.render_widget(Paragraph::new(Line::from(spans)), right_area);

    // Left: ctx bar, possibly preceded by a browse-mode badge.
    // The ctx bar is held back until the context-accounting feature is
    // ready to show; there is no user-facing switch — the gate flips
    // when the work lands.
    const SHOW_CTX_BAR: bool = true;
    // If browse mode is active, split off a fixed-width badge first.
    let left_area = if browse_mode {
        let badge_w = "  BROWSE  ".len() as u16 + 1;
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(badge_w), Constraint::Min(0)])
            .split(left_area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " BROWSE ",
                Style::default()
                    .fg(HEALTHY_GREEN)
                    .add_modifier(Modifier::REVERSED),
            )])),
            chunks[0],
        );
        chunks[1]
    } else {
        left_area
    };
    if SHOW_CTX_BAR && let Some(w) = ctx.widget() {
        frame.render_widget(w, left_area);
    }
}

fn process_stats_summary(health: Option<&serde_json::Value>) -> String {
    let cpu_label = crate::i18n::t("zc-chrome-summary-cpu");
    let loading_label = crate::i18n::t("zc-chrome-summary-loading");
    let Some(h) = health else {
        return format!(" {cpu_label}:{loading_label}");
    };
    let Some(process) = h.get("process") else {
        return format!(" {cpu_label}:{loading_label}");
    };
    let mut parts = Vec::new();
    if let Some(rss) = process.get("rss_bytes").and_then(|v| v.as_u64())
        && rss > 0
    {
        let total = process
            .get("system_ram_total_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let rss_str = format_bytes(rss);
        let ram_label = crate::i18n::t("zc-chrome-summary-ram");
        if total > 0 {
            let pct = (rss as f64 / total as f64) * 100.0;
            parts.push(format!(" {ram_label}:{rss_str}({pct:.0}%)"));
        } else {
            parts.push(format!(" {ram_label}:{rss_str}"));
        }
    }
    if let Some(cpu) = process.get("cpu_percent").and_then(|v| v.as_f64()) {
        parts.push(format!(" {cpu_label}:{cpu:.1}%"));
    } else {
        parts.push(format!(" {cpu_label}:{loading_label}"));
    }
    parts.join("")
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}G", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

// ── Help modal ───────────────────────────────────────────────────

/// Flatten a `HelpNode` tree into renderable lines, depth-first.
/// Returns `(key_string, action)` pairs; both empty = spacer; action empty +
/// key non-empty = section header; key == "\x01" = dim rule separator.
fn flatten_help_node(node: &HelpNode, out: &mut Vec<(String, String)>, inner_width: usize) {
    // Section title → dim header line.
    if let Some(title) = &node.title {
        out.push(("\x01".into(), title.to_string())); // sentinel = separator/header
    }

    // Description prose → soft-wrapped plain lines, no key column.
    if let Some(desc) = &node.description {
        let wrap_at = inner_width.saturating_sub(2).max(20);
        for line in soft_wrap(desc, wrap_at) {
            out.push(("".into(), line));
        }
        out.push(("".into(), "".into())); // blank after prose
    }

    // Keybinding entries.
    for entry in &node.entries {
        let k = entry.key_str();
        out.push((k, entry.action.to_string()));
    }

    // Children with a dim rule before each.
    for child in &node.children {
        out.push(("\x01".into(), "".into())); // dim rule
        flatten_help_node(child, out, inner_width);
    }
}

/// Naive soft-wrap: split `text` into lines no longer than `width`.
/// Breaks on word boundaries where possible.
fn soft_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.len() + 1 + word.len() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current.clone());
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

fn filter_help_node(node: &HelpNode, query: &str) -> Option<HelpNode> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Some(node.clone());
    }

    let node_matches = node
        .title
        .as_deref()
        .is_some_and(|title| title.to_lowercase().contains(&needle))
        || node
            .description
            .as_deref()
            .is_some_and(|description| description.to_lowercase().contains(&needle));
    if node_matches {
        return Some(node.clone());
    }

    let entries: Vec<HelpEntry> = node
        .entries
        .iter()
        .filter(|entry| {
            entry.key_str().to_lowercase().contains(&needle)
                || entry.action.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    let children: Vec<HelpNode> = node
        .children
        .iter()
        .filter_map(|child| filter_help_node(child, &needle))
        .collect();

    if entries.is_empty() && children.is_empty() {
        return None;
    }

    Some(HelpNode {
        title: node.title.clone(),
        description: None,
        entries,
        children,
    })
}

fn help_control_hint() -> String {
    use crate::keymap::action_key_labels;

    let up = action_key_labels(SearchBoxAction::Up).join("/");
    let down = action_key_labels(SearchBoxAction::Down).join("/");
    let cancel = action_key_labels(SearchBoxAction::Cancel).join("/");
    crate::i18n::t_args(
        "zc-app-help-controls",
        &[("up", &up), ("down", &down), ("cancel", &cancel)],
    )
}

fn draw_help_modal(
    frame: &mut ratatui::Frame,
    area: Rect,
    node: &HelpNode,
    state: &mut HelpOverlayState,
) {
    // We need inner_width to soft-wrap descriptions. Use a generous default
    // first pass, then clamp to terminal width.
    let max_inner_w = (area.width as usize).saturating_sub(6).max(30);

    let mut all_flat: Vec<(String, String)> = Vec::new();
    flatten_help_node(node, &mut all_flat, max_inner_w);
    let mut flat: Vec<(String, String)> = Vec::new();
    if let Some(filtered) = filter_help_node(node, &state.query) {
        flatten_help_node(&filtered, &mut flat, max_inner_w);
    }

    // Compute key column width (skip sentinels and prose-only lines).
    let key_width = all_flat
        .iter()
        .filter(|(k, _)| k != "\x01")
        .map(|(k, _)| crate::display_width::display_width(k))
        .max()
        .unwrap_or(0);
    let val_width = all_flat
        .iter()
        .filter(|(k, _)| k != "\x01")
        .map(|(_, v)| crate::display_width::display_width(v))
        .max()
        .unwrap_or(0);

    let title = format!(" {} ", crate::i18n::t("zc-app-keybindings-title"));
    let filter_label = crate::i18n::t("zc-app-help-filter-label");
    let filter_placeholder = crate::i18n::t("zc-app-help-filter-placeholder");
    let filter_display = if state.query.is_empty() {
        filter_placeholder.as_str()
    } else {
        &state.query
    };
    let control_hint = help_control_hint();
    let chrome_width = [
        crate::display_width::display_width(&title),
        crate::display_width::display_width(&filter_label)
            + 2
            + crate::display_width::display_width(filter_display),
        crate::display_width::display_width(&control_hint),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let inner_w = (key_width + 2 + val_width).max(chrome_width);
    let box_w = (inner_w + 4).min(area.width as usize) as u16;
    // +4: 2 border + 1 filter row + 1 footer row.
    let box_h = (all_flat.len() + 4).min(area.height as usize) as u16;

    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let modal_rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, modal_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim_style())
        .style(theme::fill_style())
        .title(Span::styled(title, theme::heading_style()));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    let filter_value = if state.query.is_empty() {
        Span::styled(filter_placeholder, theme::dim_style())
    } else {
        Span::styled(state.query.clone(), theme::accent_style())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{filter_label}: "), theme::body_style()),
            filter_value,
        ]))
        .style(theme::fill_style()),
        chunks[0],
    );

    let rule_width = chunks[1].width as usize;
    let mut text_lines: Vec<Line> = Vec::new();

    if flat.is_empty() {
        text_lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-app-help-no-matches"),
            theme::dim_style(),
        )));
    }
    for (key, val) in &flat {
        if key == "\x01" {
            // Dim horizontal rule, optionally with a label.
            if val.is_empty() {
                let rule = "─".repeat(rule_width);
                text_lines.push(Line::from(Span::styled(rule, theme::dim_style())));
            } else {
                // "── Label ──"
                let label = format!(" {} ", val);
                let sides = rule_width.saturating_sub(crate::display_width::display_width(&label));
                let left = "─".repeat(sides / 2);
                let right = "─".repeat(sides - sides / 2);
                text_lines.push(Line::from(vec![
                    Span::styled(left, theme::dim_style()),
                    Span::styled(label, theme::dim_style()),
                    Span::styled(right, theme::dim_style()),
                ]));
            }
        } else if key.is_empty() && val.is_empty() {
            text_lines.push(Line::from(""));
        } else if key.is_empty() {
            // Prose line — no key column, full width.
            text_lines.push(Line::from(Span::styled(val.clone(), theme::body_style())));
        } else {
            let padding =
                " ".repeat(key_width.saturating_sub(crate::display_width::display_width(key)));
            text_lines.push(Line::from(vec![
                Span::styled(format!("{padding}{key}"), theme::accent_style()),
                Span::styled("  ", theme::dim_style()),
                Span::styled(val.clone(), theme::body_style()),
            ]));
        }
    }

    let max_scroll = text_lines.len().saturating_sub(chunks[1].height as usize);
    state.scroll = state.scroll.min(max_scroll);
    let scroll = state.scroll.min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(text_lines)
            .style(theme::fill_style())
            .scroll((scroll, 0)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(control_hint, theme::dim_style())).style(theme::fill_style()),
        chunks[2],
    );
}

fn draw_reload_confirm_modal(frame: &mut ratatui::Frame, area: Rect) {
    let body_lines: Vec<Line> = vec![
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-line-1"),
            theme::body_style(),
        )),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-line-2"),
            theme::body_style(),
        )),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-line-3"),
            theme::body_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-bullet-gateway"),
            theme::body_style(),
        )),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-bullet-channels"),
            theme::body_style(),
        )),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-bullet-mcp"),
            theme::body_style(),
        )),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-bullet-provider"),
            theme::body_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-reload-socket-note"),
            theme::dim_style(),
        )),
    ];

    let box_w = area.width.saturating_sub(8).min(64);
    let box_h = (body_lines.len() as u16 + 4).min(area.height.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::warn_style())
        .style(theme::fill_style())
        .title(Span::styled(
            " Reload daemon? ",
            theme::warn_style().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body = Paragraph::new(body_lines)
        .style(theme::fill_style())
        .wrap(ratatui::widgets::Wrap { trim: false });
    let body_rect = Rect::new(
        inner.x.saturating_add(1),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(1),
    );
    frame.render_widget(body, body_rect);

    let footer_rect = Rect::new(
        inner.x.saturating_add(1),
        inner.y + inner.height.saturating_sub(1),
        inner.width.saturating_sub(2),
        1,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            crate::i18n::t_args(
                "zc-app-reload-confirm-row",
                &[("confirm_chord", "Enter / y"), ("cancel_chord", "Esc / n")],
            ),
            theme::dim_style(),
        ))
        .style(theme::fill_style()),
        footer_rect,
    );
}

fn draw_quit_confirm_modal(frame: &mut ratatui::Frame, area: Rect) {
    let body_lines: Vec<Line> = vec![
        Line::from(Span::styled(
            crate::i18n::t("zc-app-quit-prompt"),
            theme::heading_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            crate::i18n::t("zc-app-quit-explainer"),
            theme::dim_style(),
        )),
    ];

    let box_w = area.width.saturating_sub(8).min(60);
    let box_h = (body_lines.len() as u16 + 4).min(area.height.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, rect);
    let block = theme::modal_block(" Quit? ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body = Paragraph::new(body_lines)
        .style(theme::fill_style())
        .wrap(ratatui::widgets::Wrap { trim: false });
    let body_rect = Rect::new(
        inner.x.saturating_add(1),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(1),
    );
    frame.render_widget(body, body_rect);

    let footer_rect = Rect::new(
        inner.x.saturating_add(1),
        inner.y + inner.height.saturating_sub(1),
        inner.width.saturating_sub(2),
        1,
    );
    let footer = format!(
        "{} = {confirm}   {} = {quit}   {} = {cancel}",
        chords_for(ModalAction::bindings(), ModalAction::Confirm),
        chords_for(GlobalAction::bindings(), GlobalAction::Quit),
        chords_for(ModalAction::bindings(), ModalAction::Cancel),
        confirm = ModalAction::Confirm.label(),
        quit = GlobalAction::Quit.label(),
        cancel = ModalAction::Cancel.label(),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(footer, theme::dim_style())).style(theme::fill_style()),
        footer_rect,
    );
}

/// Render every chord bound to `action` from its `bindings()` table as a
/// `a/b` display string. Surfaces read the harness; no key literals.
/// Display strings are deduplicated — chords that render identically
/// (e.g. `'y'` and `'Y'` both render as `Y`) collapse to one slot.
fn chords_for<ActionType: PartialEq>(
    bindings: Vec<(crate::keymap::Chord, ActionType)>,
    action: ActionType,
) -> String {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for (chord, bound_action) in bindings {
        if bound_action != action {
            continue;
        }
        let label = chord.display();
        if seen.insert(label.clone()) {
            out.push(label);
        }
    }
    out.join("/")
}

fn draw_reload_status_toast(frame: &mut ratatui::Frame, area: Rect, msg: &str) {
    let text = format!(" {msg} ");
    let box_w = (text.chars().count() as u16 + 2).min(area.width);
    let box_h = 3u16.min(area.height);
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h).saturating_sub(1);
    let rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::warn_style())
        .style(theme::fill_style());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(Span::styled(text, theme::body_style())).style(theme::fill_style()),
        inner,
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> Event {
        mouse_event_with_modifiers(kind, column, row, KeyModifiers::NONE)
    }

    fn mouse_event_with_modifiers(
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers,
        })
    }

    fn key_event(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn decode_sgr_sequence(sequence: &str) -> Vec<Event> {
        let mut decoder = SgrMouseEventDecoder::default();
        decoder.feed(key_event(KeyCode::Esc));
        for character in sequence.chars() {
            decoder.feed(key_event(KeyCode::Char(character)));
        }
        std::iter::from_fn(|| decoder.next()).collect()
    }

    fn split_sgr_events(sequence: &str) -> VecDeque<Event> {
        sequence
            .chars()
            .map(|character| key_event(KeyCode::Char(character)))
            .collect()
    }

    #[test]
    fn coalesces_contiguous_mouse_drags_to_latest_position() {
        let first = mouse_event(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            1,
            2,
        );
        let mut queued = VecDeque::from([
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                4,
                5,
            ),
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                8,
                9,
            ),
        ]);

        let (current, pending) =
            coalesce_mouse_drag(first, || Ok(queued.pop_front())).expect("coalesce drag events");

        assert_eq!(
            current,
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                8,
                9
            )
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn coalescing_preserves_first_non_drag_event() {
        let first = mouse_event(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            1,
            2,
        );
        let mouse_up = mouse_event(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            8,
            9,
        );
        let mut queued = VecDeque::from([
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                8,
                9,
            ),
            mouse_up.clone(),
        ]);

        let (current, pending) =
            coalesce_mouse_drag(first, || Ok(queued.pop_front())).expect("coalesce drag events");

        assert_eq!(
            current,
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                8,
                9
            )
        );
        assert_eq!(pending, Some(mouse_up));
    }

    #[test]
    fn coalescing_bounds_each_drag_batch_and_preserves_following_input() {
        let first = mouse_event(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            0,
            0,
        );
        let mouse_up = mouse_event(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            MAX_COALESCED_MOUSE_DRAGS as u16,
            0,
        );
        let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let mut queued: VecDeque<Event> = (1..=MAX_COALESCED_MOUSE_DRAGS)
            .map(|column| {
                mouse_event(
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                    column as u16,
                    0,
                )
            })
            .chain([mouse_up.clone(), key.clone()])
            .collect();

        let (current, pending) =
            coalesce_mouse_drag(first, || Ok(queued.pop_front())).expect("coalesce first batch");

        assert_eq!(
            current,
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                MAX_COALESCED_MOUSE_DRAGS.saturating_sub(1) as u16,
                0,
            )
        );
        assert_eq!(pending, None);

        let next = queued.pop_front().expect("next drag remains queued");
        let (current, pending) =
            coalesce_mouse_drag(next, || Ok(queued.pop_front())).expect("coalesce next batch");

        assert_eq!(
            current,
            mouse_event(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                MAX_COALESCED_MOUSE_DRAGS as u16,
                0,
            )
        );
        assert_eq!(pending, Some(mouse_up));
        assert_eq!(queued.pop_front(), Some(key));
    }

    #[test]
    fn non_drag_event_passes_through_without_reading_ahead() {
        let first = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let mut read_ahead = false;

        let (current, pending) = coalesce_mouse_drag(first.clone(), || {
            read_ahead = true;
            Ok(None)
        })
        .expect("leave non-drag event unchanged");

        assert_eq!(current, first);
        assert_eq!(pending, None);
        assert!(!read_ahead);
    }

    #[test]
    fn split_sgr_mouse_decoder_decodes_vertical_wheel_events() {
        for (sequence, kind) in [
            ("[<64;32;17M", MouseEventKind::ScrollUp),
            ("[<65;32;17M", MouseEventKind::ScrollDown),
        ] {
            assert_eq!(
                decode_sgr_sequence(sequence),
                vec![mouse_event(kind, 31, 16)]
            );
        }
    }

    #[test]
    fn split_sgr_mouse_decoder_decodes_click_and_release_events() {
        for (sequence, kind) in [
            ("[<0;3;4M", MouseEventKind::Down(MouseButton::Left)),
            ("[<1;3;4M", MouseEventKind::Down(MouseButton::Middle)),
            ("[<2;3;4M", MouseEventKind::Down(MouseButton::Right)),
            ("[<3;3;4M", MouseEventKind::Up(MouseButton::Left)),
            ("[<0;3;4m", MouseEventKind::Up(MouseButton::Left)),
        ] {
            assert_eq!(decode_sgr_sequence(sequence), vec![mouse_event(kind, 2, 3)]);
        }
    }

    #[test]
    fn split_sgr_mouse_decoder_decodes_drag_and_movement_events() {
        for (sequence, kind) in [
            ("[<32;5;6M", MouseEventKind::Drag(MouseButton::Left)),
            ("[<33;5;6M", MouseEventKind::Drag(MouseButton::Middle)),
            ("[<34;5;6M", MouseEventKind::Drag(MouseButton::Right)),
            ("[<35;5;6M", MouseEventKind::Moved),
        ] {
            assert_eq!(decode_sgr_sequence(sequence), vec![mouse_event(kind, 4, 5)]);
        }
    }

    #[test]
    fn split_sgr_mouse_decoder_decodes_horizontal_wheel_events() {
        for (sequence, kind) in [
            ("[<66;10;11M", MouseEventKind::ScrollLeft),
            ("[<67;10;11M", MouseEventKind::ScrollRight),
        ] {
            assert_eq!(
                decode_sgr_sequence(sequence),
                vec![mouse_event(kind, 9, 10)]
            );
        }
    }

    #[test]
    fn split_sgr_mouse_decoder_preserves_modifiers_and_coordinates() {
        assert_eq!(
            decode_sgr_sequence("[<93;1;2M"),
            vec![mouse_event_with_modifiers(
                MouseEventKind::ScrollDown,
                0,
                1,
                KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
            )]
        );
    }

    #[test]
    fn split_sgr_mouse_decoder_accepts_optional_delimiter_and_lowercase_release() {
        assert_eq!(
            decode_sgr_sequence("[<64;32;17;M"),
            vec![mouse_event(MouseEventKind::ScrollUp, 31, 16)]
        );
        assert_eq!(
            decode_sgr_sequence("[<0;32;17;m"),
            vec![mouse_event(MouseEventKind::Up(MouseButton::Left), 31, 16)]
        );
    }

    #[test]
    fn split_sgr_mouse_decoder_rejects_multiple_empty_trailing_fields() {
        let sequence = "[<64;32;17;;M";
        let expected: Vec<Event> = std::iter::once(key_event(KeyCode::Esc))
            .chain(
                sequence
                    .chars()
                    .map(|character| key_event(KeyCode::Char(character))),
            )
            .collect();

        assert_eq!(decode_sgr_sequence(sequence), expected);
    }

    #[test]
    fn drag_read_ahead_reassembles_split_sgr_drag_without_leaking_escape() {
        let first_drag = mouse_event(MouseEventKind::Drag(MouseButton::Left), 4, 5);
        let latest_drag = mouse_event(MouseEventKind::Drag(MouseButton::Left), 7, 8);
        let following_key = key_event(KeyCode::Char('x'));
        let mut queued = VecDeque::from([key_event(KeyCode::Esc)]);
        queued.extend(split_sgr_events("[<32;8;9M"));
        queued.push_back(following_key.clone());
        let mut decoder = SgrMouseEventDecoder::default();

        let (current, pending) = coalesce_mouse_drag(first_drag, || {
            decoder.read_ready_with(|| Ok(queued.pop_front()))
        })
        .expect("reassemble SGR drag during read-ahead");

        assert_eq!(current, latest_drag);
        assert_eq!(pending, Some(following_key.clone()));
        assert!(decoder.candidate.is_empty());
        assert!(queued.is_empty());

        decoder.push_front(pending.expect("preserve following input"));
        assert_eq!(decoder.next(), Some(following_key));
    }

    #[test]
    fn invalid_escape_prefix_is_replayed_in_order() {
        let original = vec![
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        ];
        let mut decoder = SgrMouseEventDecoder::default();
        for event in original.iter().cloned() {
            decoder.feed(event);
        }

        let decoded: Vec<Event> = std::iter::from_fn(|| decoder.next()).collect();

        assert_eq!(decoded, original);
    }

    #[test]
    fn incomplete_escape_flush_preserves_the_escape_key() {
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut decoder = SgrMouseEventDecoder::default();
        decoder.feed(escape.clone());

        assert_eq!(decoder.next(), None);
        assert!(decoder.flush_candidate());
        assert_eq!(decoder.next(), Some(escape));
    }

    #[test]
    fn delayed_input_flushes_an_expired_escape_before_the_next_tick() {
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut decoder = SgrMouseEventDecoder::default();
        decoder.feed(escape.clone());
        decoder.candidate_started_at = Some(Instant::now() - SGR_MOUSE_SEQUENCE_TIMEOUT);

        assert_eq!(decoder.poll_timeout(), Duration::ZERO);
        assert!(decoder.flush_timed_out_candidate());
        assert_eq!(decoder.next(), Some(escape));
    }

    #[test]
    fn confirmation_modals_consume_text_and_path_paste_before_pane_dispatch() {
        let paste_payloads = ["hidden composer text", "/tmp/hidden-attachment.txt"];
        let modal_states = [(true, false), (false, true), (true, true)];

        for (reload_confirm, quit_confirm) in modal_states {
            for payload in paste_payloads {
                // Both plain text and path-shaped paste must stop at the
                // application modal boundary before any pane sees the value.
                let event = Event::Paste(payload.to_owned());
                assert!(confirmation_modal_owns_event(
                    &event,
                    reload_confirm,
                    quit_confirm
                ));
            }
        }

        assert!(!confirmation_modal_owns_event(
            &Event::Paste("visible composer text".to_owned()),
            false,
            false
        ));
        assert!(!confirmation_modal_owns_event(
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            true,
            false
        ));
    }

    #[test]
    fn chrome_process_summary_shows_cpu_loading_without_health() {
        let cpu = crate::i18n::t("zc-chrome-summary-cpu");
        let loading = crate::i18n::t("zc-chrome-summary-loading");

        assert_eq!(process_stats_summary(None), format!(" {cpu}:{loading}"));
    }

    #[test]
    fn chrome_process_summary_shows_ram_and_cpu_values() {
        let ram = crate::i18n::t("zc-chrome-summary-ram");
        let cpu = crate::i18n::t("zc-chrome-summary-cpu");
        let health = serde_json::json!({
            "process": {
                "rss_bytes": 1_048_576_u64,
                "system_ram_total_bytes": 4_194_304_u64,
                "cpu_percent": 12.345_f64
            }
        });

        assert_eq!(
            process_stats_summary(Some(&health)),
            format!(" {ram}:1.0M(25%) {cpu}:12.3%")
        );
    }

    #[test]
    fn chrome_process_summary_keeps_cpu_loading_until_sample_exists() {
        let ram = crate::i18n::t("zc-chrome-summary-ram");
        let cpu = crate::i18n::t("zc-chrome-summary-cpu");
        let loading = crate::i18n::t("zc-chrome-summary-loading");
        let health = serde_json::json!({
            "process": {
                "rss_bytes": 1_048_576_u64,
                "system_ram_total_bytes": 4_194_304_u64
            }
        });

        assert_eq!(
            process_stats_summary(Some(&health)),
            format!(" {ram}:1.0M(25%) {cpu}:{loading}")
        );
    }

    #[tokio::test]
    async fn chrome_status_tick_starts_refresh_without_waiting_for_rpc_response() {
        let (tx, mut rx) = mpsc::channel::<String>(1);
        let rpc = Arc::new(RpcClient::with_rpc(Arc::new(
            crate::jsonrpc::RpcOutbound::new(tx),
        )));
        let mut chrome_status = ChromeStatus::default();

        let start = Instant::now();
        chrome_status.tick(&rpc);

        assert!(
            start.elapsed() < Duration::from_millis(50),
            "tick must not wait for the status response"
        );
        assert!(
            chrome_status.refresh_in_flight,
            "tick should record that the background refresh is still pending"
        );

        let raw = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("status refresh should send a request")
            .expect("request channel should stay open");
        let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(request["method"], crate::client::method::STATUS);
    }

    #[tokio::test]
    async fn mode_switch_to_sop_returns_before_a_withheld_list_response() {
        let (tx, mut rx) = mpsc::channel::<String>(1);
        let outbound = Arc::new(crate::jsonrpc::RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let reconnect_state = Arc::new(Mutex::new(CrossReconnectState::default()));
        let mut mode = Mode::Config;
        let conn_state = ConnectionState::Connected;
        let mut dashboard_pane = dashboard::Dashboard::new(Arc::clone(&rpc), "test", false);
        let mut quickstart =
            quickstart_pane::QuickstartPane::new(Arc::clone(&rpc), reconnect_state);
        let mut acp_pane = acp::Acp::new(Arc::clone(&rpc));
        let mut chat_pane = chat::Chat::new(Arc::clone(&rpc), chat::PaneKind::Chat);
        let mut sop_pane = sop_pane::SopPane::new(rpc);

        tokio::time::timeout(
            Duration::from_millis(50),
            switch_mode(
                &mut mode,
                Mode::Sop,
                &conn_state,
                &mut dashboard_pane,
                &mut quickstart,
                &mut acp_pane,
                &mut chat_pane,
                &mut sop_pane,
            ),
        )
        .await
        .expect("entering SOP mode must not await the list response");
        assert_eq!(mode, Mode::Sop);

        let raw = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("mode entry should send a list request")
            .expect("RPC writer should remain connected");
        let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(request["method"], crate::client::method::SOPS_LIST);
    }

    #[tokio::test]
    async fn sop_reentry_discards_pre_blur_list_and_requests_one_fresh_result() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let outbound = Arc::new(crate::jsonrpc::RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let reconnect_state = Arc::new(Mutex::new(CrossReconnectState::default()));
        let mut mode = Mode::Config;
        let conn_state = ConnectionState::Connected;
        let mut dashboard_pane = dashboard::Dashboard::new(Arc::clone(&rpc), "test", false);
        let mut quickstart =
            quickstart_pane::QuickstartPane::new(Arc::clone(&rpc), reconnect_state);
        let mut acp_pane = acp::Acp::new(Arc::clone(&rpc));
        let mut chat_pane = chat::Chat::new(Arc::clone(&rpc), chat::PaneKind::Chat);
        let mut sop_pane = sop_pane::SopPane::new(rpc);

        switch_mode(
            &mut mode,
            Mode::Sop,
            &conn_state,
            &mut dashboard_pane,
            &mut quickstart,
            &mut acp_pane,
            &mut chat_pane,
            &mut sop_pane,
        )
        .await;
        let first_raw = rx.recv().await.expect("first list request");
        let first_request: serde_json::Value = serde_json::from_str(&first_raw).unwrap();
        let first_id = first_request["id"].as_str().unwrap().to_string();

        switch_mode(
            &mut mode,
            Mode::Config,
            &conn_state,
            &mut dashboard_pane,
            &mut quickstart,
            &mut acp_pane,
            &mut chat_pane,
            &mut sop_pane,
        )
        .await;
        switch_mode(
            &mut mode,
            Mode::Sop,
            &conn_state,
            &mut dashboard_pane,
            &mut quickstart,
            &mut acp_pane,
            &mut chat_pane,
            &mut sop_pane,
        )
        .await;
        assert!(
            rx.try_recv().is_err(),
            "single-flight re-entry must wait for the older request to settle"
        );

        outbound.dispatch_response(
            &first_id,
            Some(serde_json::json!([{ "name": "old" }])),
            None,
        );
        let mut fresh_id = None;
        for _ in 0..100 {
            tokio::task::yield_now().await;
            sop_pane.tick();
            while let Ok(raw) = rx.try_recv() {
                let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
                if request["method"] == crate::client::method::SOPS_LIST {
                    fresh_id = request["id"].as_str().map(String::from);
                }
            }
            if fresh_id.is_some() {
                break;
            }
        }
        assert_eq!(
            sop_pane.selected_name(),
            None,
            "the pre-blur response must not become authoritative after re-entry"
        );
        let fresh_id = fresh_id.expect("fresh list request after re-entry");
        assert_ne!(fresh_id, first_id);

        outbound.dispatch_response(
            &fresh_id,
            Some(serde_json::json!([{ "name": "fresh" }])),
            None,
        );
        for _ in 0..100 {
            tokio::task::yield_now().await;
            sop_pane.tick();
            if sop_pane.selected_name() == Some("fresh") {
                break;
            }
        }
        assert_eq!(sop_pane.selected_name(), Some("fresh"));
        while let Ok(raw) = rx.try_recv() {
            let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_ne!(
                request["method"],
                crate::client::method::SOPS_LIST,
                "only one authoritative follow-up may be sent"
            );
        }
    }

    #[tokio::test]
    async fn mode_switch_leaves_quickstart_for_sop_before_withheld_responses() {
        let (tx, mut rx) = mpsc::channel::<String>(1);
        let outbound = Arc::new(crate::jsonrpc::RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let reconnect_state = Arc::new(Mutex::new(CrossReconnectState::default()));
        let mut mode = Mode::Quickstart;
        let conn_state = ConnectionState::Connected;
        let mut dashboard_pane = dashboard::Dashboard::new(Arc::clone(&rpc), "test", false);
        let mut quickstart =
            quickstart_pane::QuickstartPane::new(Arc::clone(&rpc), reconnect_state);
        let mut acp_pane = acp::Acp::new(Arc::clone(&rpc));
        let mut chat_pane = chat::Chat::new(Arc::clone(&rpc), chat::PaneKind::Chat);
        let mut sop_pane = sop_pane::SopPane::new(rpc);

        tokio::time::timeout(
            Duration::from_millis(50),
            switch_mode(
                &mut mode,
                Mode::Sop,
                &conn_state,
                &mut dashboard_pane,
                &mut quickstart,
                &mut acp_pane,
                &mut chat_pane,
                &mut sop_pane,
            ),
        )
        .await
        .expect("leaving Quickstart must not await dismissal or SOP refresh");
        assert_eq!(mode, Mode::Sop);

        let mut methods = Vec::new();
        for _ in 0..2 {
            let raw = tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .expect("mode switch should send both background requests")
                .expect("RPC writer should remain connected");
            let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let method = request["method"].as_str().unwrap().to_string();
            if method == crate::client::method::QUICKSTART_DISMISS {
                assert_eq!(request["params"]["surface"], "tui");
            }
            methods.push(method);
        }
        assert!(
            methods
                .iter()
                .any(|method| method == crate::client::method::QUICKSTART_DISMISS),
            "mode switch should send dismissal telemetry: {methods:?}"
        );
        assert!(
            methods
                .iter()
                .any(|method| method == crate::client::method::SOPS_LIST),
            "mode switch should refresh SOPs: {methods:?}"
        );
        assert_eq!(outbound.pending_count(), 2, "both responses are withheld");
    }

    #[tokio::test]
    async fn reconnect_refreshes_the_sop_list_when_it_remains_visible() {
        let (tx, mut rx) = mpsc::channel::<String>(1);
        let outbound = Arc::new(crate::jsonrpc::RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let mut pane = sop_pane::SopPane::new(rpc);

        tokio::time::timeout(
            Duration::from_millis(50),
            refresh_visible_sop_after_reconnect(Mode::Sop, &mut pane),
        )
        .await
        .expect("visible reconnect must not await the list response");

        let raw = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("visible reconnect should request the SOP list")
            .expect("RPC request channel should stay open");
        let request: serde_json::Value =
            serde_json::from_str(&raw).expect("RPC request should be JSON");
        assert_eq!(request["method"], crate::client::method::SOPS_LIST);
        let id = request["id"]
            .as_str()
            .expect("RPC request should carry an id");
        outbound.dispatch_response(id, Some(serde_json::json!([{ "name": "deploy" }])), None);
        for _ in 0..100 {
            tokio::task::yield_now().await;
            pane.tick();
            if pane.selected_name().is_some() {
                break;
            }
        }

        assert_eq!(pane.selected_name(), Some("deploy"));
    }

    #[test]
    fn narrow_mode_bar_keeps_selected_sop_visible_and_summary_non_clickable() {
        use ratatui::{Terminal, backend::TestBackend};

        let summary = Line::from(" status");
        let backend = TestBackend::new(24, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut layout = ModeBarLayout::default();

        terminal
            .draw(|frame| {
                layout = draw_mode_bar(frame, frame.area(), Mode::Sop, Some(&summary));
            })
            .expect("draw narrow mode bar");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("SOPs"), "rendered bar: {rendered:?}");
        assert!(
            rendered.contains('‹'),
            "hidden leading modes should be signaled: {rendered:?}"
        );

        let sop = layout
            .entries
            .iter()
            .find(|entry| entry.mode == Mode::Sop)
            .expect("selected SOP tab must have rendered geometry");
        assert_eq!(
            layout.mode_at(sop.hit_rect.x, sop.hit_rect.y),
            Some(Mode::Sop)
        );

        let summary_area = layout.summary_area.expect("full summary should fit");
        assert_eq!(
            layout.mode_at(summary_area.x, summary_area.y),
            None,
            "chrome summary must not share a tab hit target"
        );
        assert!(
            layout
                .entries
                .iter()
                .all(|entry| entry.hit_rect.right() <= layout.tab_area.right()),
            "every click target must stay within the rendered tab chunk"
        );
    }

    #[test]
    fn global_help_entries_include_live_help_binding() {
        use crate::keymap::{GlobalAction, action_key_labels};

        let entries = global_help_entries();
        let help = entries
            .iter()
            .find(|entry| entry.action == crate::i18n::t("zc-app-help-help"))
            .expect("global Help section should include its own opening binding");
        let expected = action_key_labels(GlobalAction::Help);

        assert_eq!(help.keys, expected);
    }

    #[test]
    fn active_text_editor_can_claim_global_pane_navigation() {
        assert_eq!(
            pane_switch_delta(Some(GlobalAction::PaneNavLeft), false, true),
            Some(-1)
        );
        assert_eq!(
            pane_switch_delta(Some(GlobalAction::PaneNavRight), true, true),
            None
        );
    }

    #[test]
    fn disconnected_editor_claim_keeps_global_pane_navigation() {
        assert_eq!(
            pane_switch_delta(Some(GlobalAction::PaneNavRight), true, false),
            Some(1)
        );
    }

    #[test]
    fn help_filter_matches_actions_case_insensitively() {
        let node = HelpNode::titled(
            "Chat",
            vec![
                HelpEntry::key("Ctrl+N", "New session"),
                HelpEntry::key("Ctrl+D", "Cancel turn"),
            ],
        );

        let filtered = filter_help_node(&node, "NEW SESSION").expect("action should match");

        assert_eq!(filtered.title.as_deref(), Some("Chat"));
        assert_eq!(filtered.entries.len(), 1);
        assert_eq!(filtered.entries[0].action, "New session");
    }

    #[test]
    fn help_filter_matches_live_registry_key_labels() {
        use crate::keymap::{GlobalAction, action_key_labels};

        let live_key = action_key_labels(GlobalAction::ReloadDaemon)
            .into_iter()
            .next()
            .expect("reload action should have a binding");
        let node = HelpNode::entries(global_help_entries());

        let filtered =
            filter_help_node(&node, &live_key).expect("rendered registry key should match");

        assert_eq!(filtered.entries.len(), 1);
        assert_eq!(
            filtered.entries[0].action,
            crate::i18n::t("zc-app-help-reload")
        );
    }

    #[test]
    fn help_filter_keeps_every_entry_when_section_title_matches() {
        let child = HelpNode::titled(
            "Sessions",
            vec![
                HelpEntry::key("n", "New session"),
                HelpEntry::key("d", "Delete session"),
            ],
        );
        let root = HelpNode::default().with_child(child);

        let filtered = filter_help_node(&root, "sessions").expect("section should match");

        assert_eq!(filtered.children.len(), 1);
        assert_eq!(filtered.children[0].entries.len(), 2);
    }

    #[test]
    fn help_overlay_typing_scroll_and_escape_are_stateful() {
        let mut state = HelpOverlayState::default();

        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)));
        assert_eq!(state.query, "r");

        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(state.scroll, 1);

        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(state.query.is_empty());
        assert_eq!(state.scroll, 0);

        assert!(state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    #[test]
    fn help_control_hint_uses_live_search_box_bindings() {
        use crate::keymap::action_key_labels;

        let hint = help_control_hint();
        for label in action_key_labels(SearchBoxAction::Up)
            .into_iter()
            .chain(action_key_labels(SearchBoxAction::Down))
            .chain(action_key_labels(SearchBoxAction::Cancel))
        {
            assert!(
                hint.contains(&label),
                "control hint should contain live binding {label:?}"
            );
        }
    }

    #[test]
    fn help_modal_renders_filtered_results_and_keeps_controls_visible() {
        use ratatui::{Terminal, backend::TestBackend};

        let node = HelpNode::entries(vec![
            HelpEntry::key("r", "Reload daemon"),
            HelpEntry::key("q", "Quit"),
        ]);
        let mut state = HelpOverlayState {
            query: "reload".into(),
            scroll: 0,
        };
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| draw_help_modal(frame, frame.area(), &node, &mut state))
            .expect("draw help modal");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("reload"));
        assert!(rendered.contains("Reload daemon"));
        assert!(!rendered.contains("Quit"));
        assert!(rendered.contains(&help_control_hint()));
    }

    #[test]
    fn help_modal_scrolls_results_without_scrolling_filter_row() {
        use ratatui::{Terminal, backend::TestBackend};

        let node = HelpNode::entries(
            (0..10)
                .map(|index| HelpEntry::key(index.to_string(), format!("Action {index}")))
                .collect(),
        );
        let mut state = HelpOverlayState {
            query: String::new(),
            scroll: usize::MAX,
        };
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| draw_help_modal(frame, frame.area(), &node, &mut state))
            .expect("draw help modal");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let cancel = crate::keymap::action_key_labels(SearchBoxAction::Cancel)
            .into_iter()
            .next()
            .expect("cancel action should have a binding");
        assert_eq!(state.scroll, 6);
        assert!(rendered.contains(&crate::i18n::t("zc-app-help-filter-label")));
        assert!(rendered.contains(&cancel));
        assert!(!rendered.contains("Action 0"));
        assert!(rendered.contains("Action 9"));
    }

    #[test]
    fn quickstart_chat_handoff_consumes_immediate_target() {
        let state = SharedReconnectState::default();
        {
            let mut guard = state.lock().unwrap();
            guard.pending_quickstart_chat = Some(PendingQuickstartChat::Immediate("scout".into()));
        }

        assert_eq!(
            take_pending_quickstart_chat(&state, QuickstartChatDrain::Immediate),
            Some("scout".into())
        );
        assert!(state.lock().unwrap().pending_quickstart_chat.is_none());
    }

    #[test]
    fn quickstart_chat_handoff_immediate_drain_preserves_after_reconnect_target() {
        let state = SharedReconnectState::default();
        {
            let mut guard = state.lock().unwrap();
            guard.pending_quickstart_chat =
                Some(PendingQuickstartChat::AfterReconnect("scout".into()));
        }

        assert_eq!(
            take_pending_quickstart_chat(&state, QuickstartChatDrain::Immediate),
            None
        );
        assert_eq!(
            state.lock().unwrap().pending_quickstart_chat,
            Some(PendingQuickstartChat::AfterReconnect("scout".into()))
        );
    }

    #[test]
    fn quickstart_chat_handoff_consumes_after_reconnect_target() {
        let state = SharedReconnectState::default();
        {
            let mut guard = state.lock().unwrap();
            guard.pending_quickstart_chat =
                Some(PendingQuickstartChat::AfterReconnect("scout".into()));
        }

        assert_eq!(
            take_pending_quickstart_chat(&state, QuickstartChatDrain::AfterReconnect),
            Some("scout".into())
        );
        assert!(state.lock().unwrap().pending_quickstart_chat.is_none());
    }
}

#[cfg(test)]
mod transport_leg_tests {
    //! What the reconnect and failback paths do with the leg the routing source
    //! of truth returns.
    //!
    //! `active_leg` is not decoration: the failback loop runs only while it says
    //! `WssRelay`, so a leg that is dropped or committed early silently disables
    //! the migration back to the direct path.

    use super::leg_after_adoption;
    use crate::ActiveLeg;

    /// A direct session whose address is still down reconnects onto the relay.
    /// Keeping `WssDirect` here is what stops the failback loop from ever
    /// running, stranding the session on the relay for the rest of its life.
    #[test]
    fn a_direct_session_that_reconnects_on_the_relay_commits_the_relay_leg() {
        let leg = leg_after_adoption(true, ActiveLeg::WssDirect, ActiveLeg::WssRelay);

        assert_eq!(leg, ActiveLeg::WssRelay);
        assert!(
            leg == ActiveLeg::WssRelay,
            "the failback loop's precondition must now hold"
        );
    }

    /// The mirror case: a relay session that comes back on the direct path must
    /// stop re-probing a direct address it is already using.
    #[test]
    fn a_relay_session_that_reconnects_direct_commits_the_direct_leg() {
        let leg = leg_after_adoption(true, ActiveLeg::WssRelay, ActiveLeg::WssDirect);

        assert_eq!(leg, ActiveLeg::WssDirect);
        assert!(
            leg != ActiveLeg::WssRelay,
            "a direct session must not keep re-probing the path it is on"
        );
    }

    /// A connection that could not be adopted is not the connection the panes
    /// hold, so its leg must not be committed: the session is still on the old
    /// one and must keep behaving that way.
    #[test]
    fn a_failed_adoption_keeps_the_leg_it_is_still_running_on() {
        assert_eq!(
            leg_after_adoption(false, ActiveLeg::WssRelay, ActiveLeg::WssDirect),
            ActiveLeg::WssRelay,
            "a failed direct migration must leave the session on the relay"
        );
        assert_eq!(
            leg_after_adoption(false, ActiveLeg::WssDirect, ActiveLeg::WssRelay),
            ActiveLeg::WssDirect
        );
    }

    /// The local socket has no legs to migrate between; adoption must not
    /// invent one.
    #[test]
    fn a_local_session_keeps_its_leg() {
        assert_eq!(
            leg_after_adoption(true, ActiveLeg::Local, ActiveLeg::Local),
            ActiveLeg::Local
        );
    }
}
