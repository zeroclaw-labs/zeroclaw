//! Multiline input widget with sanitization helpers.

use crate::tui::state::{InputMode, TuiState};
use crate::tui::widgets::sanitize::sanitize_text;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub const MAX_INPUT_BYTES: usize = 64 * 1024;

pub fn sanitize_input(raw: &str) -> String {
    truncate_to_max_bytes(sanitize_text(raw), MAX_INPUT_BYTES)
}

pub fn append_sanitized_input(buffer: &mut String, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let chunk = sanitize_input(raw);
    if chunk.is_empty() {
        return;
    }
    buffer.push_str(&chunk);
    if buffer.len() > MAX_INPUT_BYTES {
        *buffer = truncate_to_max_bytes(std::mem::take(buffer), MAX_INPUT_BYTES);
    }
}

pub fn preferred_height(buffer: &str) -> u16 {
    let lines = buffer.lines().count().max(1).min(6) as u16;
    lines + 2 // borders/padding
}

pub fn render(frame: &mut Frame<'_>, area: Rect, state: &TuiState) -> Option<Position> {
    let title = match state.mode {
        InputMode::Editing => "Input (editing)",
        InputMode::Normal => "Input (normal, press i to edit)",
    };
    let block_style = if state.mode == InputMode::Editing {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input = Paragraph::new(state.input_buffer.as_str())
        .style(block_style)
        .block(Block::default().title(title).borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, area);

    if state.mode == InputMode::Editing {
        return Some(cursor_position(area, &state.input_buffer));
    }
    None
}

fn truncate_to_max_bytes(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut cut = max_bytes;
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    value[..cut].to_string()
}

fn cursor_position(area: Rect, content: &str) -> Position {
    let lines: Vec<&str> = content.split('\n').collect();
    let last_line = lines.last().copied().unwrap_or("");

    let max_content_rows = area.height.saturating_sub(2);
    let row = (lines.len().saturating_sub(1) as u16).min(max_content_rows.saturating_sub(1));
    let max_content_cols = area.width.saturating_sub(2);
    let col = (last_line.chars().count() as u16).min(max_content_cols.saturating_sub(1));

    Position::new(area.x + 1 + col, area.y + 1 + row)
}

#[cfg(test)]
mod tests {
    use super::{append_sanitized_input, sanitize_input, MAX_INPUT_BYTES};

    #[test]
    fn sanitize_input_strips_ansi() {
        let raw = "hello\x1b[31m-red\x1b[0m";
        let clean = sanitize_input(raw);
        assert_eq!(clean, "hello-red");
    }

    #[test]
    fn sanitize_input_enforces_max_size() {
        let raw = "x".repeat(MAX_INPUT_BYTES + 128);
        let clean = sanitize_input(&raw);
        assert_eq!(clean.len(), MAX_INPUT_BYTES);
    }

    #[test]
    fn append_sanitized_input_ignores_controls() {
        let mut buffer = String::new();
        append_sanitized_input(&mut buffer, "a\u{0000}b\x1b[2J");
        assert_eq!(buffer, "ab");
    }
}
