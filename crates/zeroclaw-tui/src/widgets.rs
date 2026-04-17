use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use super::theme;

/// Bordered info panel (like the OpenClaw security/config/channel panels).
pub struct InfoPanel<'a> {
    pub title: &'a str,
    pub lines: Vec<Line<'a>>,
}

impl Widget for InfoPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_style())
            .title(Span::styled(
                format!(" {} ", self.title),
                theme::heading_style(),
            ));

        let inner = block.inner(area);
        block.render(area, buf);

        let paragraph = Paragraph::new(Text::from(self.lines))
            .wrap(Wrap { trim: false })
            .style(theme::body_style());
        paragraph.render(inner, buf);
    }
}

/// Selectable list item for channel/option menus.
pub struct SelectableList<'a> {
    pub title: &'a str,
    pub items: &'a [SelectableItem],
    pub selected: usize,
    pub scroll_offset: usize,
}

pub struct SelectableItem {
    pub label: String,
    pub hint: String,
    pub is_active: bool,
    pub installed: bool,
}

impl Widget for SelectableList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_style())
            .title(Span::styled(
                format!(" {} ", self.title),
                theme::heading_style(),
            ));

        let inner = block.inner(area);
        block.render(area, buf);

        let visible_items = inner.height as usize;
        let start = self.scroll_offset;
        let end = (start + visible_items).min(self.items.len());

        for (i, item) in self.items[start..end].iter().enumerate() {
            let abs_idx = start + i;
            let y = inner.y + u16::try_from(i).unwrap_or(u16::MAX);
            if y >= inner.y + inner.height {
                break;
            }

            let row_area = Rect::new(inner.x, y, inner.width, 1);

            let is_cursor = abs_idx == self.selected;

            let (marker, marker_style) = if is_cursor {
                if item.is_active {
                    ("\u{25cf} ", theme::accent_style()) // ● filled (active + cursor)
                } else {
                    ("\u{203a} ", theme::selected_style()) // › arrow cursor
                }
            } else if item.is_active {
                ("\u{25cf} ", theme::accent_style()) // ● filled (active, no cursor)
            } else {
                ("\u{25cb} ", theme::unselected_style()) // ○ hollow
            };

            let label_style = if is_cursor {
                theme::selected_style()
            } else if item.installed {
                theme::success_style()
            } else {
                theme::body_style()
            };

            let hint_style = if item.installed {
                theme::success_style().add_modifier(Modifier::DIM)
            } else {
                theme::dim_style()
            };

            // Build the line — skip hint parens if hint is empty
            let mut spans = vec![
                Span::styled(marker, marker_style),
                Span::styled(&item.label, label_style),
            ];

            if !item.hint.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!("({})", item.hint), hint_style));
            }

            if item.installed && !is_cursor {
                spans.push(Span::styled(" \u{2713}", theme::success_style()));
            }

            Paragraph::new(Line::from(spans)).render(row_area, buf);
        }

        // Scroll indicators
        if self.scroll_offset > 0 {
            let indicator = Rect::new(inner.x + inner.width.saturating_sub(3), inner.y, 3, 1);
            Paragraph::new(Span::styled(" \u{25b2}", theme::dim_style())).render(indicator, buf);
        }
        if end < self.items.len() {
            let indicator = Rect::new(
                inner.x + inner.width.saturating_sub(3),
                inner.y + inner.height.saturating_sub(1),
                3,
                1,
            );
            Paragraph::new(Span::styled(" \u{25bc}", theme::dim_style())).render(indicator, buf);
        }
    }
}

/// Progress step indicator (e.g., [1/3] Preparing environment).
pub struct StepIndicator<'a> {
    pub current: u8,
    pub total: u8,
    pub label: &'a str,
    pub status: StepStatus,
}

#[allow(dead_code)] // TUI widget variants for step progress display
pub enum StepStatus {
    Pending,
    Active,
    Complete,
    Error,
}

impl Widget for StepIndicator<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (icon, style) = match self.status {
            StepStatus::Pending => (" ", theme::dim_style()),
            StepStatus::Active => ("\u{25b6}", theme::heading_style()), // ▶
            StepStatus::Complete => ("\u{2713}", theme::success_style()), // ✓
            StepStatus::Error => ("\u{2717}", Style::default().fg(theme::ERR_RED)), // ✗
        };

        let line = Line::from(vec![
            Span::styled(
                format!("[{}/{}] ", self.current, self.total),
                theme::dim_style(),
            ),
            Span::styled(format!("{icon} "), style),
            Span::styled(self.label, style),
        ]);

        Paragraph::new(line).render(area, buf);
    }
}

/// ASCII art banner widget — spells ZEROCLAW in block characters.
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
        let mut lines: Vec<Line> = vec![Line::from("")];

        for line in BANNER_ART.lines() {
            if !line.is_empty() {
                lines.push(Line::from(Span::styled(line, theme::title_style())));
            }
        }

        lines.push(Line::from(Span::styled(
            "\u{1f980} ZEROCLAW \u{1f980}",
            theme::accent_style(),
        )));
        lines.push(Line::from(""));

        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .render(area, buf);
    }
}

/// Confirmed step line (checkmark + text).
pub struct ConfirmedLine<'a> {
    pub label: &'a str,
    pub value: &'a str,
}

impl Widget for ConfirmedLine<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(vec![
            Span::styled("\u{25c7}  ", theme::success_style()), // ◇
            Span::styled(self.label, theme::body_style()),
            Span::raw("  "),
            Span::styled(self.value, theme::heading_style()),
        ]);
        Paragraph::new(line).render(area, buf);
    }
}

/// Prompt line with current input buffer.
pub struct InputPrompt<'a> {
    pub label: &'a str,
    pub input: &'a str,
    pub masked: bool,
}

impl Widget for InputPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let display = if self.masked {
            "\u{2022}".repeat(self.input.len()) // •
        } else {
            self.input.to_string()
        };

        let line = Line::from(vec![
            Span::styled("\u{25c6}  ", theme::accent_style()), // ◆
            Span::styled(self.label, theme::heading_style()),
            Span::raw("  "),
            Span::styled(display, theme::input_style()),
            Span::styled("\u{2588}", theme::accent_style()), // cursor block
        ]);
        Paragraph::new(line).render(area, buf);
    }
}
