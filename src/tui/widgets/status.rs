//! Single-line status bar.

use crate::tui::state::{TuiState, TuiStatus};
use ratatui::{prelude::*, widgets::Paragraph};

pub fn render(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let status_text = match state.status {
        TuiStatus::Idle => "idle",
        TuiStatus::Thinking => "thinking",
        TuiStatus::ToolRunning => "tool-running",
    };
    let line = format!(
        "provider={} | model={} | status={} | mode={:?}",
        state.provider_id, state.model_id, status_text, state.mode
    );
    let widget = Paragraph::new(line).style(Style::default().fg(Color::Gray));
    frame.render_widget(widget, area);
}
