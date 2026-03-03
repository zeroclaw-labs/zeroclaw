//! Tool progress output panel.

use crate::tui::state::TuiState;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn render(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let content = state
        .progress_block
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("No active tool execution.");
    let clean = crate::tui::widgets::chat::sanitize_display(content);
    let panel = Paragraph::new(clean)
        .block(Block::default().title("Tools").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(panel, area);
}
