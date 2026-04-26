use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::theme;

/// ZeroClaw ASCII banner rendered at the top of the TUI onboarding session.
pub struct Banner;

const BANNER_ART: &str = r"
 ███████╗███████╗██████╗  ██████╗  ██████╗██╗      █████╗ ██╗    ██╗
 ╚══███╔╝██╔════╝██╔══██╗██╔═══██╗██╔════╝██║     ██╔══██╗██║    ██║
   ███╔╝ █████╗  ██████╔╝██║   ██║██║     ██║     ███████║██║ █╗ ██║
  ███╔╝  ██╔══╝  ██╔══██╗██║   ██║██║     ██║     ██╔══██║██║███╗██║
 ███████╗███████╗██║  ██║╚██████╔╝╚██████╗███████╗██║  ██║╚███╔███╔╝
 ╚══════╝╚══════╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝╚══════╝╚═╝  ╚═╝ ╚══╝╚══╝
";

impl Widget for Banner {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line<'_>> = Vec::new();
        for art_line in BANNER_ART.lines() {
            if art_line.is_empty() {
                continue;
            }
            lines.push(Line::from(Span::styled(art_line, theme::title_style())));
        }
        lines.push(Line::from(Span::styled(
            "\u{1f980} ZEROCLAW \u{1f980}",
            theme::accent_style(),
        )));

        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .render(area, buf);
    }
}

/// Fixed height the banner widget wants (7 ASCII-art rows + 1 tagline).
pub const BANNER_HEIGHT: u16 = 7;

/// Single-line prompt with a label and the current input buffer. Masks the
/// input when `masked` is true (for secrets).
pub struct InputPrompt<'a> {
    pub label: &'a str,
    pub input: &'a str,
    pub masked: bool,
}

impl Widget for InputPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let display = if self.masked {
            "\u{2022}".repeat(self.input.len())
        } else {
            self.input.to_string()
        };

        let line = Line::from(vec![
            Span::styled("\u{25c6}  ", theme::accent_style()),
            Span::styled(self.label, theme::heading_style()),
            Span::raw("  "),
            Span::styled(display, theme::input_style()),
            Span::styled("\u{2588}", theme::accent_style()),
        ]);
        Paragraph::new(line).render(area, buf);
    }
}
