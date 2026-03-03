//! Chat history panel.

use crate::tui::state::{TuiRole, TuiState};
use crate::tui::widgets::sanitize::sanitize_text;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub fn sanitize_display(text: &str) -> String {
    sanitize_text(text)
}

pub fn render(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (idx, message) in state.messages.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::raw(String::new()));
        }

        let (label, label_style) = match message.role {
            TuiRole::User => ("user", Style::default().fg(Color::Cyan)),
            TuiRole::Assistant => ("assistant", Style::default().fg(Color::Green)),
            TuiRole::System => ("system", Style::default().fg(Color::Yellow)),
            TuiRole::Error => ("error", Style::default().fg(Color::Red)),
        };

        let clean = sanitize_display(&message.content);
        let mut clean_lines = clean.lines();
        if let Some(first) = clean_lines.next() {
            lines.push(Line::from(vec![
                Span::styled(format!("{label}: "), label_style),
                Span::raw(first.to_string()),
            ]));
            for extra in clean_lines {
                lines.push(Line::from(Span::raw(format!("  {extra}"))));
            }
        } else {
            lines.push(Line::from(Span::styled(format!("{label}: "), label_style)));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::raw(
            "Start typing in the input box to chat with ZeroClaw.",
        )));
    }

    let viewport_lines = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(viewport_lines.max(1));
    let scroll_from_bottom = state.scroll_offset.min(max_scroll);
    let scroll_y = max_scroll.saturating_sub(scroll_from_bottom) as u16;

    let paragraph = Paragraph::new(lines)
        .block(Block::default().title("Chat").borders(Borders::ALL))
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::sanitize_display;

    #[test]
    fn strips_ansi_escape_sequences() {
        let input = "safe\x1b[2Jtext\x1b]0;title\x07";
        let clean = sanitize_display(input);
        assert_eq!(clean, "safetext");
    }

    #[test]
    fn removes_control_chars_except_newline_and_tab() {
        let input = "a\u{0000}b\nc\td\u{0085}e";
        let clean = sanitize_display(input);
        assert_eq!(clean, "ab\nc\tde");
    }
}
