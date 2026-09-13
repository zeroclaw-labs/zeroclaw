//! Rendered-text selection primitives shared by interactive ZeroCode panes.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::mouse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellPoint {
    pub(crate) column: u16,
    pub(crate) row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextSelection {
    pub(crate) anchor: CellPoint,
    pub(crate) head: CellPoint,
    pub(crate) dragged: bool,
}

impl TextSelection {
    fn normalized(self) -> (CellPoint, CellPoint) {
        if (self.anchor.row, self.anchor.column) <= (self.head.row, self.head.column) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextCell {
    pub(crate) symbol: String,
    pub(crate) span_start: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextRowBreak {
    Hard,
    SoftSpace,
    SoftConcat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSnapshot {
    pub(crate) area: Rect,
    pub(crate) cells: Vec<TextCell>,
    pub(crate) row_breaks: Vec<TextRowBreak>,
}

impl TextSnapshot {
    pub(crate) fn capture(frame: &mut Frame, area: Rect, row_breaks: Vec<TextRowBreak>) -> Self {
        use unicode_width::UnicodeWidthStr;

        let buffer = frame.buffer_mut();
        let mut cells = Vec::with_capacity(usize::from(area.width) * usize::from(area.height));
        for y in area.y..area.y.saturating_add(area.height) {
            let mut column = 0;
            while column < area.width {
                let symbol = buffer[(area.x + column, y)].symbol().to_string();
                let width = (UnicodeWidthStr::width(symbol.as_str()) as u16)
                    .max(1)
                    .min(area.width - column);
                cells.push(TextCell {
                    symbol,
                    span_start: column,
                });
                for _ in 1..width {
                    cells.push(TextCell {
                        symbol: String::new(),
                        span_start: column,
                    });
                }
                column += width;
            }
        }
        Self {
            area,
            cells,
            row_breaks,
        }
    }

    pub(crate) fn point_at(&self, column: u16, row: u16) -> Option<CellPoint> {
        if !mouse::in_rect(column, row, self.area) {
            return None;
        }
        Some(CellPoint {
            column: column - self.area.x,
            row: row - self.area.y,
        })
    }

    fn cell(&self, point: CellPoint) -> Option<&TextCell> {
        if point.column >= self.area.width || point.row >= self.area.height {
            return None;
        }
        let index =
            usize::from(point.row) * usize::from(self.area.width) + usize::from(point.column);
        self.cells.get(index)
    }

    pub(crate) fn has_text_at(&self, point: CellPoint) -> bool {
        let Some(cell) = self.cell(point) else {
            return false;
        };
        self.cell(CellPoint {
            column: cell.span_start,
            row: point.row,
        })
        .is_some_and(|origin| !origin.symbol.chars().all(char::is_whitespace))
    }

    pub(crate) fn row_text_bounds(&self, row: u16) -> Option<(u16, u16)> {
        let first =
            (0..self.area.width).find(|&column| self.has_text_at(CellPoint { column, row }))?;
        let last = (0..self.area.width)
            .rev()
            .find(|&column| self.has_text_at(CellPoint { column, row }))?;
        Some((first, last))
    }

    fn clamp_outer_whitespace(&self, mut point: CellPoint) -> CellPoint {
        if let Some((first, last)) = self.row_text_bounds(point.row) {
            point.column = point.column.clamp(first, last);
        }
        point
    }

    pub(crate) fn selection_bounds(
        &self,
        selection: TextSelection,
    ) -> Option<(CellPoint, CellPoint)> {
        if !selection.dragged {
            return None;
        }
        let (mut start, mut end) = selection.normalized();
        start = self.clamp_outer_whitespace(start);
        end = self.clamp_outer_whitespace(end);
        start.column = self.cell(start)?.span_start;
        let end_cell = self.cell(end)?;
        let origin = self.cell(CellPoint {
            column: end_cell.span_start,
            row: end.row,
        })?;
        end.column = end_cell
            .span_start
            .saturating_add(
                (unicode_width::UnicodeWidthStr::width(origin.symbol.as_str()) as u16)
                    .max(1)
                    .saturating_sub(1),
            )
            .min(self.area.width.saturating_sub(1));
        Some((start, end))
    }

    pub(crate) fn bounds_contain(start: CellPoint, end: CellPoint, point: CellPoint) -> bool {
        (point.row, point.column) >= (start.row, start.column)
            && (point.row, point.column) <= (end.row, end.column)
    }

    pub(crate) fn selected_text(&self, selection: TextSelection) -> Option<String> {
        if self.cells.is_empty() {
            return None;
        }

        let (start, end) = self.selection_bounds(selection)?;
        let start_row = usize::from(start.row);
        let end_row = usize::from(end.row);
        let mut text = String::new();

        for row_idx in start_row..=end_row {
            let first_col = if row_idx == start_row {
                start.column
            } else {
                0
            };
            let last_col = if row_idx == end_row {
                end.column
            } else {
                self.area.width.saturating_sub(1)
            };

            let mut row_text = String::new();
            for column in first_col..=last_col {
                let point = CellPoint {
                    column,
                    row: row_idx as u16,
                };
                let Some(cell) = self.cell(point) else {
                    continue;
                };
                if cell.span_start == column {
                    row_text.push_str(&cell.symbol);
                }
            }
            let row_text = row_text.trim_end_matches(' ');
            if row_idx > start_row {
                match self
                    .row_breaks
                    .get(row_idx)
                    .copied()
                    .unwrap_or(TextRowBreak::Hard)
                {
                    TextRowBreak::Hard => text.push('\n'),
                    TextRowBreak::SoftSpace => text.push(' '),
                    TextRowBreak::SoftConcat => {}
                }
            }
            text.push_str(row_text);
        }

        text.chars().any(|ch| !ch.is_whitespace()).then_some(text)
    }

    pub(crate) fn selection_anchor_rect(&self, selection: TextSelection) -> Option<Rect> {
        if !selection.dragged {
            return None;
        }
        let (start, end) = selection.normalized();
        let y = self.area.y.saturating_add(start.row);
        let height = end.row.saturating_sub(start.row).saturating_add(1);
        Some(Rect::new(self.area.x, y, self.area.width, height))
    }

    pub(crate) fn render_selection(
        &self,
        frame: &mut Frame,
        selection: TextSelection,
        style: Style,
    ) {
        let Some((start, end)) = self.selection_bounds(selection) else {
            return;
        };
        let buffer = frame.buffer_mut();
        for row in 0..self.area.height {
            for column in 0..self.area.width {
                if Self::bounds_contain(start, end, CellPoint { column, row }) {
                    buffer[(self.area.x + column, self.area.y + row)].set_style(style);
                }
            }
        }
    }
}

pub(crate) fn borrow_line<'a>(line: &'a Line<'static>) -> Line<'a> {
    let spans: Vec<Span<'a>> = line
        .spans
        .iter()
        .map(|span| Span::styled(span.content.as_ref(), span.style))
        .collect();
    let mut out = Line::from(spans).style(line.style);
    if let Some(alignment) = line.alignment {
        out = out.alignment(alignment);
    }
    out
}

pub(crate) fn wrapped_rows(line: &Line<'static>, width: u16) -> u16 {
    Paragraph::new(vec![borrow_line(line)])
        .wrap(Wrap { trim: false })
        .line_count(width) as u16
}

pub(crate) fn row_breaks_for_line(line: &Line<'static>, width: u16) -> Vec<TextRowBreak> {
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let visual_lines = crate::input_bar::wrap_visual_lines(&text, width);
    let expected_rows = usize::from(wrapped_rows(line, width));
    if visual_lines.len() != expected_rows {
        return vec![TextRowBreak::Hard; expected_rows];
    }

    visual_lines
        .iter()
        .enumerate()
        .map(|(index, current)| {
            let Some(previous) = index.checked_sub(1).and_then(|i| visual_lines.get(i)) else {
                return TextRowBreak::Hard;
            };
            let gap = &text[previous.end..current.start];
            if !gap.is_empty() && !gap.chars().all(|ch| ch == '\u{200b}') {
                TextRowBreak::SoftSpace
            } else {
                TextRowBreak::SoftConcat
            }
        })
        .collect()
}

pub(crate) fn row_breaks_for_lines(lines: &[Line<'static>], width: u16) -> Vec<TextRowBreak> {
    lines
        .iter()
        .flat_map(|line| row_breaks_for_line(line, width))
        .collect()
}
