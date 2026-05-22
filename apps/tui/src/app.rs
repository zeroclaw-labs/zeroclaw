use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::acp;
use crate::chat;
use crate::client::RpcClient;
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

pub async fn run(rpc: &RpcClient) -> Result<()> {
    let mut term = config_manager::init_terminal()?;

    let mut mode = Mode::Dashboard;
    let mut show_help = false;
    let mut bar_area = Rect::default();
    let mut content_area = Rect::default();

    let mut dashboard_pane = dashboard::Dashboard::new(rpc);
    dashboard_pane.init().await?;
    let mut config_app = config_manager::App::new(rpc);
    config_app.init().await?;
    let mut acp_pane = acp::Acp::new(rpc);
    let mut chat_pane = chat::Chat::new(rpc);
    let mut logs_pane = logs::Logs::new(rpc);
    logs_pane.init().await?;

    loop {
        // Draw
        term.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
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

            // Help modal overlay (drawn last so it sits on top).
            if show_help {
                let help = match mode {
                    Mode::Dashboard => dashboard_pane.help_lines(),
                    Mode::Config => config_app.help_lines(),
                    Mode::Logs => logs_pane.help_lines(),
                    _ => vec![("?", "This help")],
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
                        Mode::Logs => logs_pane.wants_text_input(),
                        _ => false,
                    };
                    if !in_text_input {
                        show_help = true;
                        continue;
                    }
                }

                let quit = match mode {
                    Mode::Dashboard => dashboard_pane.handle_key(key).await,
                    Mode::Config => config_app.handle_key(key, &mut term).await?,
                    Mode::ACP => acp_pane.handle_key(key),
                    Mode::Chat => chat_pane.handle_key(key),
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
                // Forward to active pane
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
                    _ => {}
                }
            }
            _ => {} // Resize, etc. — just redraw on next iteration
        }
    }

    config_manager::restore_terminal(&mut term)?;
    Ok(())
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
