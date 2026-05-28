use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::acp;
use crate::chat;
use crate::client::{ConnectionState, RpcClient};
use crate::config_manager;
use crate::dashboard;
use crate::logs;
use crate::mouse;
use crate::quickstart_pane;
use crate::theme;
use crate::widgets::{CtxBar, HelpContext, HelpEntry, HelpNode};

/// State that must survive a reconnect — used by Quickstart's
/// Stage-2 flow to route the user into the freshly-created agent's
/// chat after the daemon comes back up.
#[derive(Debug, Default)]
pub struct CrossReconnectState {
    /// Agent alias the next `run()` invocation should switch the
    /// Chat tab onto. Consumed (cleared) after the first read.
    pub start_chat_with: Option<String>,
}

pub type SharedReconnectState = Arc<Mutex<CrossReconnectState>>;

/// How often the UI redraws when no input arrives (for live panes).
const TICK: Duration = Duration::from_millis(200);

/// Mode bar entries. Shared between drawing and click detection.
const MODES: [Mode; 6] = [
    Mode::Dashboard,
    Mode::Config,
    Mode::Acp,
    Mode::Chat,
    Mode::Logs,
    Mode::Quickstart,
];

// ── Mode enum ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dashboard,
    Config,
    Acp, // displayed as "Code" in the UI
    Chat,
    Logs,
    Quickstart,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Dashboard => "Dashboard",
            Mode::Config => "Config",
            Mode::Acp => "Code",
            Mode::Chat => "Chat",
            Mode::Logs => "Logs",
            Mode::Quickstart => "Quickstart",
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

// ── Top-level entry point ────────────────────────────────────────

/// Run the TUI event loop. Returns `true` if the daemon disconnected
/// (caller should attempt reconnection), `false` if the user quit normally.
pub async fn run(
    rpc: Arc<RpcClient>,
    term: &mut config_manager::Term,
    connect_label: &str,
    reconnect_state: SharedReconnectState,
) -> Result<bool> {
    let mut mode = Mode::Dashboard;
    let mut show_help = false;
    let mut reload_confirm = false;
    let mut reload_status: Option<String> = None;
    let mut bar_area = Rect::default();
    let mut content_area = Rect::default();
    let mut disconnect_since: Option<std::time::Instant> = None;

    let mut dashboard_pane = dashboard::Dashboard::new(&rpc, connect_label);
    dashboard_pane.init().await?;
    let mut config_app = config_manager::App::new(&rpc);
    config_app.init().await?;
    let rpc_arc = rpc.clone();
    let mut acp_pane = acp::Acp::new(Arc::clone(&rpc_arc));
    acp_pane.init().await?;
    let mut chat_pane = chat::Chat::new(Arc::clone(&rpc_arc), chat::PaneKind::Chat);
    chat_pane.init().await?;
    // Consume any post-reconnect intent — Quickstart's Stage 2 sets
    // this before triggering disconnect/reconnect so the next run
    // lands the user directly in the freshly-created agent's chat.
    let pending_start_chat = {
        let mut guard = reconnect_state.lock().expect("reconnect state poisoned");
        guard.start_chat_with.take()
    };
    let mut logs_pane = logs::Logs::new(&rpc);
    logs_pane.init().await?;
    let mut quickstart =
        quickstart_pane::QuickstartPane::new(Arc::clone(&rpc_arc), Arc::clone(&reconnect_state));
    quickstart.init().await?;

    // Apply any pending Stage-2 intent from the previous run.
    if let Some(alias) = pending_start_chat {
        chat_pane.focus_agent(&alias).await;
        mode = Mode::Chat;
    }

    loop {
        // Draw
        let conn_state = rpc.connection_state();
        term.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // mode bar
                    Constraint::Min(0),    // content
                    Constraint::Length(1), // status bar
                ])
                .split(frame.area());

            bar_area = chunks[0];
            draw_mode_bar(frame, chunks[0], mode);
            content_area = chunks[1];

            match mode {
                Mode::Dashboard => dashboard_pane.draw(frame, chunks[1]),
                Mode::Config => config_app.draw_into(frame, chunks[1]),
                Mode::Acp => acp_pane.draw(frame, chunks[1]),
                Mode::Chat => chat_pane.draw(frame, chunks[1]),
                Mode::Logs => logs_pane.draw(frame, chunks[1]),
                Mode::Quickstart => quickstart.draw(frame, chunks[1]),
            }

            let (ctx_input, ctx_max) = match mode {
                Mode::Chat => chat_pane.ctx_tokens(),
                Mode::Acp => acp_pane.ctx_tokens(),
                _ => (None, None),
            };
            draw_status_bar(
                frame,
                chunks[2],
                &conn_state,
                rpc.tui_id(),
                CtxBar::new(ctx_input, ctx_max),
            );

            // Help modal overlay (drawn last so it sits on top).
            if show_help {
                let mut node = HelpNode::entries(vec![
                    HelpEntry::new(vec!["Ctrl+←", "Ctrl+→"], "Cycle mode"),
                    HelpEntry::key("Ctrl+R", "Reload daemon"),
                    HelpEntry::key("Ctrl+C", "Quit"),
                    HelpEntry::spacer(),
                ]);
                let pane_node = match mode {
                    Mode::Dashboard => dashboard_pane.help_context(),
                    Mode::Config => config_app.help_context(),
                    Mode::Acp => acp_pane.help_context(),
                    Mode::Chat => chat_pane.help_context(),
                    Mode::Logs => logs_pane.help_context(),
                    Mode::Quickstart => quickstart.help_context(),
                };
                node.children.push(pane_node);
                draw_help_modal(frame, frame.area(), &node);
            }

            if reload_confirm {
                draw_reload_confirm_modal(frame, frame.area());
            }
            if let Some(msg) = &reload_status {
                draw_reload_status_toast(frame, frame.area(), msg);
            }
        })?;

        // Disconnect handoff runs every iteration, not just when the input
        // poll times out. A steady stream of events (mouse scroll, resize,
        // focus) would otherwise keep `event::poll` returning true and the
        // grace timer would never start — the UI would sit frozen on the
        // red "Disconnected" status bar indefinitely.
        if matches!(rpc.connection_state(), ConnectionState::Disconnected { .. }) {
            let since = *disconnect_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= Duration::from_secs(2) {
                return Ok(true);
            }
        }

        // Poll for input with a timeout so live panes refresh periodically.
        if !event::poll(TICK)? {
            if matches!(conn_state, ConnectionState::Disconnected { .. }) {
                continue;
            }
            if mode == Mode::Dashboard {
                dashboard_pane.tick().await;
            }
            if mode == Mode::Logs {
                logs_pane.tick().await;
            }
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }

                // Ctrl+C always quits
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }

                // Reload-daemon confirmation modal — intercepts all keys
                // while open. Mirrors the web dashboard's
                // `ReloadDaemonButton` confirm flow.
                if reload_confirm {
                    match key.code {
                        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                            reload_confirm = false;
                            reload_status = Some(match rpc.config_reload().await {
                                Ok(_) => "Daemon reload signalled — reconnecting…".into(),
                                Err(e) => format!("Reload requested ({e})"),
                            });
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
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

                // Ctrl+R opens the reload-daemon confirmation modal,
                // unless the active pane is in text-input mode (so it
                // doesn't hijack an edit field).
                if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    let in_text_input = match mode {
                        Mode::Dashboard => dashboard_pane.wants_text_input(),
                        Mode::Config => config_app.wants_text_input(),
                        Mode::Acp => acp_pane.wants_text_input(),
                        Mode::Chat => chat_pane.wants_text_input(),
                        Mode::Logs => logs_pane.wants_text_input(),
                        Mode::Quickstart => quickstart.wants_text_input(),
                    };
                    if !in_text_input {
                        reload_confirm = true;
                        continue;
                    }
                }

                // Help modal: any key dismisses it
                if show_help {
                    show_help = false;
                    continue;
                }

                let switch_to: Option<Mode> = if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Left => Some(mode.cycle(-1)),
                        KeyCode::Right => Some(mode.cycle(1)),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(next) = switch_to {
                    if mode == Mode::Quickstart && next != Mode::Quickstart {
                        quickstart.dismiss_beacon().await;
                    }
                    mode = next;
                    continue;
                }

                // `?` opens help unless pane is in text-input mode.
                if key.code == KeyCode::Char('?') {
                    let in_text_input = match mode {
                        Mode::Dashboard => dashboard_pane.wants_text_input(),
                        Mode::Config => config_app.wants_text_input(),
                        Mode::Acp => acp_pane.wants_text_input(),
                        Mode::Chat => chat_pane.wants_text_input(),
                        Mode::Logs => logs_pane.wants_text_input(),
                        Mode::Quickstart => quickstart.wants_text_input(),
                    };
                    if !in_text_input {
                        show_help = true;
                        continue;
                    }
                }

                // Skip pane key handlers when disconnected — they may
                // issue RPC calls that hang on the dead socket.
                if matches!(conn_state, ConnectionState::Disconnected { .. }) {
                    continue;
                }

                let quit = match mode {
                    Mode::Dashboard => dashboard_pane.handle_key(key).await,
                    Mode::Config => config_app.handle_key(key, term).await?,
                    Mode::Acp => acp_pane.handle_key(key, term).await,
                    Mode::Chat => chat_pane.handle_key(key, term).await,
                    Mode::Logs => logs_pane.handle_key(key).await,
                    Mode::Quickstart => quickstart.handle_key(key).await,
                };
                if quit {
                    break;
                }
            }
            Event::Mouse(mouse) => {
                // Dismiss help on any click
                if show_help {
                    if matches!(mouse.kind, MouseEventKind::Down(_)) {
                        show_help = false;
                    }
                    continue;
                }
                // Mode bar clicks
                if matches!(mouse.kind, MouseEventKind::Down(_)) {
                    let labels: Vec<(&str, String)> = MODES
                        .iter()
                        .map(|m| ("", format!(" {} ", m.name())))
                        .collect();
                    let label_refs: Vec<(&str, &str)> =
                        labels.iter().map(|(k, l)| (*k, l.as_str())).collect();
                    if let Some(n) =
                        mouse::mode_bar_click(mouse.column, mouse.row, bar_area, &label_refs)
                    {
                        let next = MODES[(n - 1) as usize];
                        if mode == Mode::Quickstart && next != Mode::Quickstart {
                            quickstart.dismiss_beacon().await;
                        }
                        mode = next;
                        continue;
                    }
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
                        Mode::Logs => {
                            logs_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::Acp => {
                            acp_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::Chat => {
                            chat_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::Quickstart => {
                            quickstart.handle_mouse(mouse, content_area).await;
                        }
                    }
                }
            }
            Event::Paste(text) if !matches!(conn_state, ConnectionState::Disconnected { .. }) => {
                match mode {
                    Mode::Chat => chat_pane.handle_paste(&text),
                    Mode::Acp => acp_pane.handle_paste(&text),
                    Mode::Config => config_app.handle_paste(&text),
                    _ => {}
                }
            }
            _ => {} // Resize, etc. — just redraw on next iteration
        }
    }

    Ok(false)
}

// ── Mode bar ─────────────────────────────────────────────────────

fn draw_mode_bar(frame: &mut ratatui::Frame, area: Rect, active: Mode) {
    let mut spans = Vec::new();
    for m in &MODES {
        let label_style = if *m == active {
            theme::selected_style().add_modifier(Modifier::BOLD)
        } else {
            theme::body_style()
        };
        spans.push(Span::styled(format!(" {} ", m.name()), label_style));
        spans.push(Span::raw(" "));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
) {
    let (dot, label, style) = match state {
        ConnectionState::Connected => (
            "\u{25cf}",
            " Connected".to_string(),
            Style::default().fg(HEALTHY_GREEN),
        ),
        ConnectionState::Disconnected { reason } => (
            "\u{25cf}",
            format!(" Disconnected (reason: {reason})"),
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

    // Left: ctx bar, left-aligned in its own column.
    if let Some(w) = ctx.widget() {
        frame.render_widget(w, left_area);
    }
}

// ── Help modal ───────────────────────────────────────────────────

/// Flatten a `HelpNode` tree into renderable lines, depth-first.
/// Returns `(key_string, action)` pairs; both empty = spacer; action empty +
/// key non-empty = section header; key == "\x01" = dim rule separator.
fn flatten_help_node(node: &HelpNode, out: &mut Vec<(String, String)>, inner_width: usize) {
    // Section title → dim header line.
    if let Some(title) = node.title {
        out.push(("\x01".into(), title.to_string())); // sentinel = separator/header
    }

    // Description prose → soft-wrapped plain lines, no key column.
    if let Some(desc) = node.description {
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

fn draw_help_modal(frame: &mut ratatui::Frame, area: Rect, node: &HelpNode) {
    // We need inner_width to soft-wrap descriptions. Use a generous default
    // first pass, then clamp to terminal width.
    let max_inner_w = (area.width as usize).saturating_sub(6).max(30);

    let mut flat: Vec<(String, String)> = Vec::new();
    flatten_help_node(node, &mut flat, max_inner_w);

    // Compute key column width (skip sentinels and prose-only lines).
    let key_width = flat
        .iter()
        .filter(|(k, _)| k != "\x01")
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0);
    let val_width = flat
        .iter()
        .filter(|(k, _)| k != "\x01")
        .map(|(_, v)| v.len())
        .max()
        .unwrap_or(0);

    let inner_w = key_width + 2 + val_width;
    let box_w = (inner_w + 4).min(area.width as usize) as u16;
    // +4: 2 border + 1 title + 1 footer + 1 blank
    let box_h = (flat.len() + 5).min(area.height as usize) as u16;

    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let modal_rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, modal_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim_style())
        .title(Span::styled(" Keybindings ", theme::heading_style()));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    let rule_width = inner.width as usize;
    let mut text_lines: Vec<Line> = Vec::new();

    for (key, val) in &flat {
        if key == "\x01" {
            // Dim horizontal rule, optionally with a label.
            if val.is_empty() {
                let rule = "─".repeat(rule_width);
                text_lines.push(Line::from(Span::styled(rule, theme::dim_style())));
            } else {
                // "── Label ──"
                let label = format!(" {} ", val);
                let sides = rule_width.saturating_sub(label.len());
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
            text_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>width$}", key, width = key_width),
                    theme::accent_style(),
                ),
                Span::styled("  ", theme::dim_style()),
                Span::styled(val.clone(), theme::body_style()),
            ]));
        }
    }

    text_lines.push(Line::from(""));
    text_lines.push(Line::from(Span::styled(
        "Press any key to close",
        theme::dim_style(),
    )));

    frame.render_widget(Paragraph::new(text_lines), inner);
}

fn draw_reload_confirm_modal(frame: &mut ratatui::Frame, area: Rect) {
    let body_lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "The daemon process stays running (same PID), but every",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "subsystem tears down and re-initializes from the on-disk",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled("config:", Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(Span::styled(
            "  • Gateway listener stops and rebinds",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  • Channel listeners (Matrix, Slack, etc.) respawn",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  • MCP servers, scheduler, heartbeat re-init",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  • Provider clients pick up new API keys / model defaults",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "The RPC socket will briefly drop. The TUI will reconnect.",
            Style::default().fg(Color::DarkGray),
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
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " Reload daemon? ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body = Paragraph::new(body_lines).wrap(ratatui::widgets::Wrap { trim: false });
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
            "Enter / y = reload   Esc / n = cancel",
            Style::default().fg(Color::DarkGray),
        )),
        footer_rect,
    );
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
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(Color::White))),
        inner,
    );
}
