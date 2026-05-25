use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
};

use crate::widgets::{HelpNode};

/// A read-only onboarding / welcome pane shown on first launch.
pub struct OnboardPane {
    scroll: u16,
}

impl OnboardPane {
    pub fn new() -> Self {
        Self { scroll: 0 }
    }

    pub fn draw(&self, frame: &mut ratatui::Frame, area: Rect) {
        let title_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let heading_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let normal_style = Style::default().fg(Color::White);
        let dim_style = Style::default().fg(Color::DarkGray);
        let key_style = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);

        let lines: Vec<Line> = vec![
            Line::from(vec![Span::styled(
                " Welcome to ZeroClaw ",
                title_style,
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "What is ZeroClaw?",
                heading_style,
            )]),
            Line::from(vec![Span::styled(
                "ZeroClaw is an autonomous AI agent platform. It runs persistent \
                 AI agents that can browse the web, write and run code, manage \
                 files, and communicate across channels like Telegram, Discord, \
                 and Slack — all from a single daemon.",
                normal_style,
            )]),
            Line::from(""),
            Line::from(vec![Span::styled("Quick-start", heading_style)]),
            Line::from(vec![
                Span::styled("  1. ", dim_style),
                Span::styled("Dashboard (F1)", key_style),
                Span::styled(" — view connected agents and system status.", normal_style),
            ]),
            Line::from(vec![
                Span::styled("  2. ", dim_style),
                Span::styled("Config (F2)  ", key_style),
                Span::styled(" — create or edit agent configurations.", normal_style),
            ]),
            Line::from(vec![
                Span::styled("  3. ", dim_style),
                Span::styled("ACP (F3)     ", key_style),
                Span::styled(" — inspect live agent/channel protocol traffic.", normal_style),
            ]),
            Line::from(vec![
                Span::styled("  4. ", dim_style),
                Span::styled("Chat (F4)    ", key_style),
                Span::styled(" — send messages to an agent interactively.", normal_style),
            ]),
            Line::from(vec![
                Span::styled("  5. ", dim_style),
                Span::styled("Logs (F5)    ", key_style),
                Span::styled(" — stream live log output from the daemon.", normal_style),
            ]),
            Line::from(""),
            Line::from(vec![Span::styled("Global keys", heading_style)]),
            Line::from(vec![
                Span::styled("  ?        ", key_style),
                Span::styled("Toggle context-sensitive help overlay.", normal_style),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl+C   ", key_style),
                Span::styled("Quit the TUI (daemon keeps running).", normal_style),
            ]),
            Line::from(vec![
                Span::styled("  ↑ / ↓   ", key_style),
                Span::styled("Scroll this pane.", normal_style),
            ]),
            Line::from(""),
            Line::from(vec![Span::styled("Next steps", heading_style)]),
            Line::from(vec![Span::styled(
                "Press F2 to open the Config editor and create your first agent, \
                 or press F1 to check the Dashboard for any agents already running.",
                normal_style,
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "This pane is accessible at any time via F6.",
                dim_style,
            )]),
        ];

        let block = Block::default()
            .title(" Onboarding ")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .padding(Padding::horizontal(2));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(para, inner);
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
            }
            _ => {}
        }
        false
    }

    pub fn wants_text_input(&self) -> bool {
        false
    }

    pub fn help_context(&self) -> HelpNode {
        use crate::widgets::{HelpEntry};
        HelpNode::entries(vec![
            HelpEntry::new(vec!["↑ / k", "↓ / j"], "Scroll up / down"),
            HelpEntry::new(vec!["PgUp", "PgDn"], "Scroll by 10 lines"),
            HelpEntry::key("F1", "Go to Dashboard"),
        ])
    }
}
