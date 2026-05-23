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
use crate::theme;

/// How often the UI redraws when no input arrives (for live panes).
const TICK: Duration = Duration::from_millis(200);

/// Mode bar label table. Shared between drawing and click detection.
const MODE_LABELS: [(&str, &str, Mode); 5] = [
    ("F1", " Dashboard ", Mode::Dashboard),
    ("F2", " Config ", Mode::Config),
    ("F3", " ACP ", Mode::ACP),
    ("F4", " Chat ", Mode::Chat),
    ("F5", " Logs ", Mode::Logs),
];

// ── Mode enum ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dashboard,
    Config,
    ACP,
    Chat,
    Logs,
}

// ── Top-level entry point ────────────────────────────────────────

/// Run the TUI event loop. Returns `true` if the daemon disconnected
/// (caller should attempt reconnection), `false` if the user quit normally.
pub async fn run(
    rpc: &RpcClient,
    mut term: &mut config_manager::Term,
    connect_label: &str,
) -> Result<bool> {
    let mut mode = Mode::Dashboard;
    let mut show_help = false;
    let mut bar_area = Rect::default();
    let mut content_area = Rect::default();
    let mut disconnect_since: Option<std::time::Instant> = None;

    let mut dashboard_pane = dashboard::Dashboard::new(rpc, connect_label);
    dashboard_pane.init().await?;
    let mut config_app = config_manager::App::new(rpc);
    config_app.init().await?;
    let mut acp_pane = acp::Acp::new(rpc);
    acp_pane.init().await?;
    let mut chat_pane = chat::Chat::new(rpc, " Chat ");
    chat_pane.init().await?;
    let mut logs_pane = logs::Logs::new(rpc);
    logs_pane.init().await?;

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
                Mode::ACP => acp_pane.draw(frame, chunks[1]),
                Mode::Chat => chat_pane.draw(frame, chunks[1]),
                Mode::Logs => logs_pane.draw(frame, chunks[1]),
            }

            draw_status_bar(frame, chunks[2], &conn_state, rpc.tui_id());

            // Help modal overlay (drawn last so it sits on top).
            if show_help {
                let help = match mode {
                    Mode::Dashboard => dashboard_pane.help_lines(),
                    Mode::Config => config_app.help_lines(),
                    Mode::ACP => vec![("?", "This help")],
                    Mode::Chat => chat_pane.help_lines(),
                    Mode::Logs => logs_pane.help_lines(),
                };
                // Global keys always shown.
                let mut lines = vec![("F1–F5", "Switch mode"), ("Ctrl+C", "Quit")];
                lines.push(("", ""));
                lines.extend(help);
                draw_help_modal(frame, frame.area(), &lines);
            }
        })?;

        // Poll for input with a timeout so live panes refresh periodically.
        if !event::poll(TICK)? {
            // Re-read live connection state — the snapshot from draw time
            // may be stale if the read task detected EOF since then.
            let live_state = rpc.connection_state();
            if matches!(live_state, ConnectionState::Disconnected { .. }) {
                // Keep the UI alive for a few seconds so the user sees the
                // disconnect reason, then hand off to the caller to reconnect.
                // RPC calls are skipped — they'd hang on the dead socket.
                let since = *disconnect_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= Duration::from_secs(2) {
                    return Ok(true);
                }
                continue;
            }
            if mode == Mode::Dashboard {
                dashboard_pane.tick().await;
            }
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Ctrl+C always quits
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }

                // Help modal: any key dismisses it
                if show_help {
                    show_help = false;
                    continue;
                }

                // Global keys: F1–F5 switch modes
                match key.code {
                    KeyCode::F(1) => {
                        mode = Mode::Dashboard;
                        continue;
                    }
                    KeyCode::F(2) => {
                        mode = Mode::Config;
                        continue;
                    }
                    KeyCode::F(3) => {
                        mode = Mode::ACP;
                        continue;
                    }
                    KeyCode::F(4) => {
                        mode = Mode::Chat;
                        continue;
                    }
                    KeyCode::F(5) => {
                        mode = Mode::Logs;
                        continue;
                    }
                    _ => {}
                }

                // `?` opens help unless pane is in text-input mode.
                if key.code == KeyCode::Char('?') {
                    let in_text_input = match mode {
                        Mode::Dashboard => dashboard_pane.wants_text_input(),
                        Mode::Config => config_app.wants_text_input(),
                        Mode::ACP => acp_pane.wants_text_input(),
                        Mode::Chat => chat_pane.wants_text_input(),
                        Mode::Logs => logs_pane.wants_text_input(),
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
                    Mode::Config => config_app.handle_key(key, &mut term).await?,
                    Mode::ACP => acp_pane.handle_key(key).await,
                    Mode::Chat => chat_pane.handle_key(key).await,
                    Mode::Logs => logs_pane.handle_key(key).await,
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
                    let labels: Vec<(&str, &str)> =
                        MODE_LABELS.iter().map(|(k, l, _)| (*k, *l)).collect();
                    if let Some(n) =
                        mouse::mode_bar_click(mouse.column, mouse.row, bar_area, &labels)
                    {
                        mode = MODE_LABELS[(n - 1) as usize].2;
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
                            config_app
                                .handle_mouse(mouse, content_area, &mut term)
                                .await?;
                        }
                        Mode::Logs => {
                            logs_pane.handle_mouse(mouse, content_area);
                        }
                        Mode::ACP => {}
                        Mode::Chat => {}
                    }
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
    for (key, label, m) in &MODE_LABELS {
        let key_style = theme::dim_style();
        let label_style = if *m == active {
            theme::selected_style().add_modifier(Modifier::BOLD)
        } else {
            theme::body_style()
        };
        spans.push(Span::styled(*key, key_style));
        spans.push(Span::styled(*label, label_style));
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
    let text_len = id_len + 1 + label.len(); // id + dot + label
    let padding = (area.width as usize).saturating_sub(text_len);

    let mut spans = vec![Span::raw(" ".repeat(padding))];
    if let Some(id) = id_span {
        spans.push(id);
    }
    spans.push(Span::styled(dot, style));
    spans.push(Span::styled(label, style));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Help modal ───────────────────────────────────────────────────

fn draw_help_modal(frame: &mut ratatui::Frame, area: Rect, lines: &[(&str, &str)]) {
    // Compute minimum dimensions.
    let key_width = lines.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let val_width = lines.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
    // key column + "  " gap + value column + border padding (2 each side)
    let inner_w = key_width + 2 + val_width;
    let box_w = (inner_w + 4) as u16; // 2 border + 2 padding
    let box_h = (lines.len() + 4) as u16; // 2 border + 1 title + 1 footer hint

    // Center in the terminal area.
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let modal_rect = Rect::new(x, y, box_w.min(area.width), box_h.min(area.height));

    // Clear the area behind the modal.
    frame.render_widget(Clear, modal_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim_style())
        .title(Span::styled(" Keybindings ", theme::heading_style()));

    let inner = block.inner(modal_rect);
    frame.render_widget(block, modal_rect);

    // Render keybinding lines.
    let mut text_lines: Vec<Line> = Vec::new();
    for (key, desc) in lines {
        if key.is_empty() && desc.is_empty() {
            text_lines.push(Line::from(""));
        } else {
            text_lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>width$}", key, width = key_width),
                    theme::accent_style(),
                ),
                Span::styled("  ", theme::dim_style()),
                Span::styled(*desc, theme::body_style()),
            ]));
        }
    }
    // Footer hint
    text_lines.push(Line::from(""));
    text_lines.push(Line::from(Span::styled(
        "Press any key to close",
        theme::dim_style(),
    )));

    frame.render_widget(Paragraph::new(text_lines), inner);
}
