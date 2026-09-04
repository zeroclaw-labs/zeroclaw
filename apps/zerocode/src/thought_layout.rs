// Adapted from Ratatui's MIT-licensed `reflow::WordWrapper` at
// 0a2a7c0363a4806b0cf05c1915bf7cdd438f756c. Keeping the row boundaries here
// avoids rerunning its private wrapper for an unchanged long thought.
//
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2025 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use std::collections::VecDeque;
use std::mem;
use std::ops::Range;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, StyledGrapheme};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::text_selection::{TextRowBreak, row_breaks_for_line};

const PREFIX: &str = "(thinking) ";

#[derive(Clone, Debug)]
struct Symbol {
    source: Range<usize>,
    width: u16,
    whitespace: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ThoughtLayout {
    text: Arc<str>,
    width: u16,
    pieces: Vec<Range<usize>>,
    row_ends: Vec<usize>,
    row_breaks: Vec<TextRowBreak>,
    #[cfg(test)]
    generation: usize,
}

impl ThoughtLayout {
    pub(crate) fn new(text: Arc<str>, width: u16) -> Self {
        let symbols = symbols(&text);
        let (pieces, row_ends) = wrap_symbols(symbols, width);
        // Keep the existing selection/copy separator contract; layout changes
        // must not turn soft wrapping into newlines in copied text.
        let row_breaks = row_breaks_for_line(
            &Line::from(vec![Span::raw(PREFIX), Span::raw(text.to_string())]),
            width,
        );
        #[cfg(test)]
        static GENERATION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        Self {
            text,
            width,
            pieces,
            row_ends,
            row_breaks,
            #[cfg(test)]
            generation: GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub(crate) fn matches(&self, text: &Arc<str>, width: u16) -> bool {
        self.width == width && Arc::ptr_eq(&self.text, text)
    }

    pub(crate) fn row_count(&self) -> u16 {
        u16::try_from(self.row_ends.len()).unwrap_or(u16::MAX)
    }

    pub(crate) fn width(&self) -> u16 {
        self.width
    }

    pub(crate) fn row_breaks(&self) -> &[TextRowBreak] {
        &self.row_breaks
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> usize {
        self.generation
    }

    pub(crate) fn render(
        &self,
        rows: Range<usize>,
        prefix_style: Style,
        body_style: Style,
        area: Rect,
        buffer: &mut Buffer,
    ) {
        for (row, line) in self
            .lines(rows, prefix_style, body_style)
            .iter()
            .enumerate()
        {
            let mut column = 0;
            for grapheme in line.styled_graphemes(Style::default()) {
                let width = grapheme.symbol.width() as u16;
                if width == 0 {
                    continue;
                }
                if column >= area.width {
                    break;
                }
                // Match Paragraph's wrapped painter: a wide symbol starting
                // in the last column is retained, not dropped by LineTruncator.
                buffer[(area.x + column, area.y + row as u16)]
                    .set_symbol(grapheme.symbol)
                    .set_style(grapheme.style);
                column = column.saturating_add(width);
            }
        }
    }

    fn lines(
        &self,
        rows: Range<usize>,
        prefix_style: Style,
        body_style: Style,
    ) -> Vec<Line<'static>> {
        rows.map(|row| {
            let start = if row == 0 { 0 } else { self.row_ends[row - 1] };
            let pieces = &self.pieces[start..self.row_ends[row]];
            Line::from(
                pieces
                    .iter()
                    .flat_map(|piece| split_piece(&self.text, piece, prefix_style, body_style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
    }
}

fn symbols(text: &str) -> Vec<Symbol> {
    // Ratatui segments each styled span independently, including a thought
    // whose first grapheme combines with the prefix's final space.
    [(0, PREFIX), (PREFIX.len(), text)]
        .into_iter()
        .flat_map(|(offset, part)| {
            UnicodeSegmentation::grapheme_indices(part, true)
                .map(move |(start, symbol)| (offset + start, symbol))
        })
        .filter_map(|(start, symbol)| {
            if symbol.contains(char::is_control) {
                return None;
            }
            let styled = StyledGrapheme {
                symbol,
                style: Style::default(),
            };
            Some(Symbol {
                source: start..start + symbol.len(),
                width: symbol.width() as u16,
                whitespace: styled.is_whitespace(),
            })
        })
        .collect()
}

fn wrap_symbols(symbols: Vec<Symbol>, width: u16) -> (Vec<Range<usize>>, Vec<usize>) {
    if width == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut pieces = Vec::new();
    let mut row_ends = Vec::new();
    let mut pending_line: Vec<Symbol> = Vec::new();
    let mut pending_word: Vec<Symbol> = Vec::new();
    let mut pending_whitespace: VecDeque<Symbol> = VecDeque::new();
    let mut line_width = 0u16;
    let mut word_width = 0u16;
    let mut whitespace_width = 0u16;
    let mut non_whitespace_previous = false;

    for symbol in symbols {
        if symbol.width > width {
            continue;
        }
        let word_found = non_whitespace_previous && symbol.whitespace;
        let untrimmed_overflow = pending_line.is_empty()
            && word_width
                .saturating_add(whitespace_width)
                .saturating_add(symbol.width)
                > width;
        if word_found || untrimmed_overflow {
            pending_line.extend(pending_whitespace.drain(..));
            line_width = line_width.saturating_add(whitespace_width);
            pending_line.append(&mut pending_word);
            line_width = line_width.saturating_add(word_width);
            whitespace_width = 0;
            word_width = 0;
        }

        let line_full = line_width >= width;
        let pending_word_overflow = symbol.width > 0
            && line_width
                .saturating_add(whitespace_width)
                .saturating_add(word_width)
                >= width;
        if line_full || pending_word_overflow {
            let mut remaining_width = width.saturating_sub(line_width);
            append_row(mem::take(&mut pending_line), &mut pieces, &mut row_ends);
            line_width = 0;
            while let Some(candidate) = pending_whitespace.front() {
                if candidate.width > remaining_width {
                    break;
                }
                whitespace_width = whitespace_width.saturating_sub(candidate.width);
                remaining_width = remaining_width.saturating_sub(candidate.width);
                pending_whitespace.pop_front();
            }
            if symbol.whitespace && pending_whitespace.is_empty() {
                continue;
            }
        }

        non_whitespace_previous = !symbol.whitespace;
        if symbol.whitespace {
            whitespace_width = whitespace_width.saturating_add(symbol.width);
            pending_whitespace.push_back(symbol);
        } else {
            word_width = word_width.saturating_add(symbol.width);
            pending_word.push(symbol);
        }
    }

    pending_line.extend(pending_whitespace);
    pending_line.append(&mut pending_word);
    if !pending_line.is_empty() {
        append_row(pending_line, &mut pieces, &mut row_ends);
    }
    if row_ends.is_empty() {
        row_ends.push(0);
    }
    (pieces, row_ends)
}

fn append_row(symbols: Vec<Symbol>, pieces: &mut Vec<Range<usize>>, row_ends: &mut Vec<usize>) {
    let start = pieces.len();
    for symbol in symbols {
        if pieces.len() > start
            && let Some(last) = pieces.last_mut()
            && last.end == symbol.source.start
        {
            last.end = symbol.source.end;
        } else {
            pieces.push(symbol.source);
        }
    }
    row_ends.push(pieces.len());
}

fn split_piece(
    text: &str,
    piece: &Range<usize>,
    prefix_style: Style,
    body_style: Style,
) -> Vec<Span<'static>> {
    if piece.end <= PREFIX.len() {
        return vec![Span::styled(PREFIX[piece.clone()].to_owned(), prefix_style)];
    }
    if piece.start >= PREFIX.len() {
        return vec![Span::styled(
            text[piece.start - PREFIX.len()..piece.end - PREFIX.len()].to_owned(),
            body_style,
        )];
    }
    vec![
        Span::styled(PREFIX[piece.start..].to_owned(), prefix_style),
        Span::styled(text[..piece.end - PREFIX.len()].to_owned(), body_style),
    ]
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Paragraph, Wrap};

    use super::*;

    fn symbols_from(terminal: &Terminal<TestBackend>) -> Vec<Vec<(String, Style)>> {
        let area = terminal.backend().buffer().area;
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| {
                        let cell = &terminal.backend().buffer()[(column, row)];
                        (cell.symbol().to_owned(), cell.style())
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn cached_rows_match_paragraph_cells() {
        let prefix_style = Style::default().bold();
        let body_style = Style::default().italic();
        for text in [
            "alpha beta  gamma delta",
            "wide 界 and é Unicode",
            "alpha beta  界 é trailing words alpha beta  界 é trailing words alpha beta  界 é trailing words",
            "averylongunbrokenword",
            "\u{301}combining at the style boundary",
            "  \t\nspaces\u{200b}and\u{a0}nonbreaking\u{200b}space   ",
            "",
        ] {
            for width in [1, 2, 5, 7, 8, 19, 20, 80] {
                let original = Line::from(vec![
                    Span::styled(PREFIX, prefix_style),
                    Span::styled(text, body_style),
                ]);
                let layout = ThoughtLayout::new(Arc::<str>::from(text), width);
                let height = layout.row_count();
                assert_eq!(
                    usize::from(height),
                    Paragraph::new(original.clone())
                        .wrap(Wrap { trim: false })
                        .line_count(width)
                );
                let mut expected = Terminal::new(TestBackend::new(width, height)).unwrap();
                expected
                    .draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(original.clone()).wrap(Wrap { trim: false }),
                            frame.area(),
                        );
                    })
                    .unwrap();
                let mut actual = Terminal::new(TestBackend::new(width, height)).unwrap();
                actual
                    .draw(|frame| {
                        let area = frame.area();
                        layout.render(
                            0..usize::from(height),
                            prefix_style,
                            body_style,
                            area,
                            frame.buffer_mut(),
                        );
                    })
                    .unwrap();
                assert_eq!(
                    symbols_from(&actual),
                    symbols_from(&expected),
                    "{text:?} at {width}"
                );
            }
        }
    }
}
