use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::acp;
use crate::chat;
use crate::client::RpcClient;
use crate::config_manager;
use crate::dashboard;
use crate::logs;
use crate::theme;

/// How often the UI redraws when no input arrives (for live panes).
const TICK: Duration = Duration::from_millis(200);

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
    let mut content_area = Rect::default();

    let mut dashboard_pane = dashboard::Dashboard::new(rpc);
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

            draw_mode_bar(frame, chunks[0], mode);
            content_area = chunks[1];

            match mode {
                Mode::Dashboard => dashboard_pane.draw(frame, chunks[1]),
                Mode::Config => config_app.draw_into(frame, chunks[1]),
                Mode::ACP => acp_pane.draw(frame, chunks[1]),
                Mode::Chat => chat_pane.draw(frame, chunks[1]),
                Mode::Logs => logs_pane.draw(frame, chunks[1]),
            }
        })?;

        // Poll for input with a timeout so live panes refresh periodically.
        if !event::poll(TICK)? {
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let key = if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
                } else {
                    key
                };

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

                let quit = match mode {
                    Mode::Dashboard => dashboard_pane.handle_key(key),
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
                if mode == Mode::Logs {
                    logs_pane.handle_mouse(mouse, content_area);
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
    let tabs = [
        ("F1", " Dashboard ", Mode::Dashboard),
        ("F2", " Config ", Mode::Config),
        ("F3", " ACP ", Mode::ACP),
        ("F4", " Chat ", Mode::Chat),
        ("F5", " Logs ", Mode::Logs),
    ];

    let mut spans = Vec::new();
    for (key, label, m) in &tabs {
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
