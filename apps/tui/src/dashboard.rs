use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::client::RpcClient;
use crate::theme;

pub(crate) struct Dashboard<'a> {
    rpc: &'a RpcClient,
}

impl<'a> Dashboard<'a> {
    pub(crate) fn new(rpc: &'a RpcClient) -> Self {
        Self { rpc }
    }

    pub(crate) fn draw(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Dashboard ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(inner);

        let hint = Paragraph::new(Line::from(vec![
            Span::styled("Coming soon ", theme::body_style()),
            Span::styled("— press ", theme::dim_style()),
            Span::styled("F2", theme::accent_style()),
            Span::styled(" to switch to Config", theme::dim_style()),
        ]))
        .alignment(Alignment::Center);

        let version = Paragraph::new(Line::from(Span::styled(
            format!("daemon v{}", self.rpc.server_version),
            theme::dim_style(),
        )))
        .alignment(Alignment::Center);

        frame.render_widget(hint, chunks[1]);
        frame.render_widget(version, chunks[2]);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
    }
}
