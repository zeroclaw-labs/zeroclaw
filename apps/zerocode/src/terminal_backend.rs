//! Terminal backend that cleans up wide trailing cells.
//!
//! Ratatui 0.30's Crossterm backend positions each changed cell relative to
//! the previously written cell: it only emits an absolute `MoveTo` when the
//! new cell is not exactly one column to the right of the last one. That
//! contiguity assumption breaks for wide grapheme clusters.
//!
//! Two distinct failure modes are handled here:
//!
//! 1. **Ordinary wide glyphs** (e.g. CJK `中`, width 2): after the terminal
//!    prints the glyph the hardware cursor advances by the glyph's display
//!    width. Ratatui's buffer diff normally omits the covered continuation
//!    cell, but when a stale continuation update sneaks into the stream it is
//!    contiguous (`x == prev.x + 1`) and would be printed on top of the wide
//!    glyph's second half. We suppress those covered updates entirely.
//!
//! 2. **Emoji presentation sequences** containing VS16 (`U+FE0F`, e.g.
//!    `⌨️`): Ratatui deliberately emits an explicit update for the trailing
//!    cell because some terminals fail to blank it. That trailing update is
//!    contiguous with the glyph, so the Crossterm backend would print it
//!    relying on relative cursor advancement. On terminals whose cursor lands
//!    at an ambiguous column after a VS16 glyph, that relative positioning is
//!    wrong.
//!
//!    We fix this without fabricating anything. The VS16 glyph is drawn as a
//!    normal changed cell in the current batch. When the *next* incoming
//!    changed cell lands on a column that glyph covers, we flush the current
//!    batch (finalizing the glyph) and start a **fresh** batch whose first
//!    element is that actual trailing cell — untouched, straight from the
//!    incoming iterator. Because it is the first cell of a fresh delegated
//!    `draw`, Ratatui's Crossterm backend emits an absolute `MoveTo` for it
//!    while preserving its real symbol and style. No `Print(" ")`, no
//!    synthesized columns, no double-emit, and the actual cell is never lost.
//!
//! No persistent shadow buffer is kept: the wrapper is a pure transform over
//! the changed-cell stream Ratatui already computes.

use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

use crate::display_width::display_width;

/// Wraps [`ratatui::backend::CrosstermBackend`] and rewrites the changed-cell
/// stream so wide grapheme continuation cells are positioned correctly.
pub(crate) struct WideCellCleanupBackend<W: Write> {
    inner: ratatui::backend::CrosstermBackend<W>,
}

impl<W: Write> WideCellCleanupBackend<W> {
    /// Creates a new backend wrapping a Crossterm backend over `writer`.
    pub(crate) fn new(writer: W) -> Self {
        Self {
            inner: ratatui::backend::CrosstermBackend::new(writer),
        }
    }
}

/// Display width of a cell's symbol, saturated into `u16`.
fn cell_width_u16(cell: &Cell) -> u16 {
    u16::try_from(display_width(cell.symbol())).unwrap_or(u16::MAX)
}

/// Whether a cell's symbol carries an emoji-presentation selector (VS16).
fn contains_vs16(cell: &Cell) -> bool {
    cell.symbol().chars().any(|c| c == '\u{FE0F}')
}

impl<W: Write> Write for WideCellCleanupBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for WideCellCleanupBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        // Buffered references to hand to the delegated draw in order.
        let mut batch: Vec<(u16, u16, &'a Cell)> = Vec::new();
        // After an ordinary (non-VS16) wide glyph, the column(s) it covers.
        // Any changed cell landing on a covered column is suppressed because
        // printing the wide glyph already painted it.
        let mut suppress_until: Option<(u16, u16)> = None;
        // After a VS16 wide glyph, the column(s) it covers. The FIRST incoming
        // changed cell that lands on one of those columns must begin a fresh
        // delegated batch so Ratatui emits an absolute `MoveTo` for it (rather
        // than a relative advance off the glyph's ambiguous cursor position),
        // while its real symbol and style are preserved verbatim.
        let mut vs16_cover: Option<(u16, u16)> = None;

        for (x, y, cell) in content {
            if let Some((cover_y, cover_x_end)) = suppress_until {
                if y == cover_y && x < cover_x_end {
                    // Covered continuation cell of a preceding ordinary wide
                    // glyph: skip it entirely.
                    continue;
                }
                suppress_until = None;
            }

            if let Some((cover_y, cover_x_end)) = vs16_cover {
                if y == cover_y && x < cover_x_end {
                    // Actual changed cell on a column the preceding VS16 glyph
                    // covers. Flush the current batch (finalizing the glyph),
                    // then let this cell start a fresh batch: as the first
                    // element of the next delegated `draw`, the backend emits
                    // an absolute `MoveTo(x, y)` for it while printing its real
                    // symbol and style. It is not dropped or rewritten.
                    if !batch.is_empty() {
                        self.inner.draw(batch.drain(..))?;
                    }
                }
                // Whether or not it fell in range, the VS16 boundary is spent
                // once we pass the covered columns.
                vs16_cover = None;
            }

            let width = cell_width_u16(cell);

            if width > 1 && contains_vs16(cell) {
                // Emoji presentation sequence. Draw it as a normal changed cell
                // in the current batch, but remember the columns it covers so
                // the next covered update opens a fresh batch (above).
                let right = x.saturating_add(width);
                vs16_cover = Some((y, right));
                batch.push((x, y, cell));
            } else if width > 1 {
                // Ordinary wide glyph: keep it, and suppress any covered
                // continuation cells that follow in this stream.
                let right = x.saturating_add(width);
                suppress_until = Some((y, right));
                batch.push((x, y, cell));
            } else {
                batch.push((x, y, cell));
            }
        }

        if !batch.is_empty() {
            self.inner.draw(batch.drain(..))?;
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Cell;

    /// Absolute cursor move that Crossterm emits for `MoveTo(x, y)`:
    /// `ESC [ (y+1) ; (x+1) H`.
    fn move_to(x: u16, y: u16) -> String {
        format!("\x1b[{};{}H", y + 1, x + 1)
    }

    /// Render `content` through the backend into a byte buffer and return the
    /// bytes emitted to the underlying writer.
    fn render(content: Vec<(u16, u16, Cell)>) -> Vec<u8> {
        let mut sink: Vec<u8> = Vec::new();
        {
            let refs: Vec<(u16, u16, &Cell)> =
                content.iter().map(|(x, y, c)| (*x, *y, c)).collect();
            let mut backend = WideCellCleanupBackend::new(&mut sink);
            backend
                .draw(refs.into_iter())
                .expect("draw must succeed into an in-memory writer");
        }
        sink
    }

    fn cell(symbol: &str) -> Cell {
        let mut c = Cell::default();
        c.set_symbol(symbol);
        c
    }

    /// Position of the first byte-substring `needle` in `haystack`, if any.
    fn find(haystack: &[u8], needle: &str) -> Option<usize> {
        let n = needle.as_bytes();
        if n.is_empty() {
            return Some(0);
        }
        haystack.windows(n.len()).position(|w| w == n)
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        find(haystack, needle).is_some()
    }

    /// Count non-overlapping occurrences of `needle` in `haystack`.
    fn count(haystack: &[u8], needle: &str) -> usize {
        let n = needle.as_bytes();
        if n.is_empty() {
            return 0;
        }
        let mut i = 0;
        let mut total = 0;
        while i + n.len() <= haystack.len() {
            if &haystack[i..i + n.len()] == n {
                total += 1;
                i += n.len();
            } else {
                i += 1;
            }
        }
        total
    }

    /// A cell carrying an explicit symbol and a bold modifier, so byte
    /// inspection can prove the delegated draw preserved the actual cell's
    /// style rather than fabricating a blank space.
    fn bold_cell(symbol: &str) -> Cell {
        let mut c = Cell::default();
        c.set_symbol(symbol);
        c.modifier = ratatui::style::Modifier::BOLD;
        c
    }

    #[test]
    fn vs16_trailing_cell_retained_with_absolute_move_and_style() {
        // ⌨️ (U+2328 U+FE0F, width 2) at x=0 covering x=0..=1. The actual
        // changed trailing cell at x=1 carries a NON-space symbol `z` with a
        // bold modifier; a further ASCII `z` follows at x=2.
        //
        // Contract:
        //   * exactly one MoveTo(1,0) for the trailing cell,
        //   * NO MoveTo(2,0) — the x=2 update is contiguous within the same
        //     delegated batch as x=1 and rides its relative advance,
        //   * the actual `z` symbol survives (hardcoded Print(" ") would lose
        //     it), appearing exactly once,
        //   * the trailing cell's bold style survives.
        let glyph = "\u{2328}\u{FE0F}";
        let out = render(vec![
            (0, 0, cell(glyph)),
            (1, 0, bold_cell("z")),
            (2, 0, cell("x")),
        ]);

        // Glyph start gets its own absolute move.
        assert!(
            contains(&out, &move_to(0, 0)),
            "expected MoveTo(0,0) for glyph start"
        );
        assert!(contains(&out, glyph), "glyph must be printed");

        // The trailing covered cell at x=1 begins a fresh delegated batch, so
        // the backend emits exactly one absolute MoveTo(1,0) for it.
        assert_eq!(
            count(&out, &move_to(1, 0)),
            1,
            "expected exactly one MoveTo(1,0) for the trailing cell"
        );

        // The x=2 update rides the relative advance from x=1 — no absolute
        // move to column 2 is emitted.
        assert!(
            !contains(&out, &move_to(2, 0)),
            "x=2 update must not emit MoveTo(2,0)"
        );

        // The actual changed symbol is retained (not replaced by a space) and
        // emitted exactly once.
        assert_eq!(
            count(&out, "z"),
            1,
            "actual trailing symbol `z` must be retained exactly once"
        );

        // Style preservation: the bold modifier for the trailing cell reaches
        // the wire (`ESC [ 1 m`).
        assert!(
            contains(&out, "\x1b[1m"),
            "trailing cell bold style must be preserved on the wire"
        );

        // Ordering: glyph emitted before its trailing cell's absolute move.
        let g = find(&out, glyph).expect("glyph present");
        let trailing = find(&out, &move_to(1, 0)).expect("trailing move present");
        assert!(
            g < trailing,
            "glyph must be emitted before its trailing cell"
        );
    }

    #[test]
    fn cjk_wide_glyph_omits_covered_space_retains_glyph_and_x() {
        // 中 (width 2) at x=1 covering x=1..=2, a stale covered space at x=2,
        // then `x` at x=3. The covered space must be omitted; glyph and x kept.
        let glyph = "中";
        let out = render(vec![
            (1, 0, cell(glyph)),
            (2, 0, cell(" ")),
            (3, 0, cell("x")),
        ]);

        assert!(contains(&out, glyph), "CJK glyph must be printed");
        assert!(contains(&out, "x"), "x must be retained");

        // The covered continuation cell at x=2 must NOT produce a MoveTo(2,0):
        // it is suppressed, so no absolute move to column 2 is emitted.
        assert!(
            !contains(&out, &move_to(2, 0)),
            "covered continuation cell must be omitted (no MoveTo(2,0))"
        );
    }

    #[test]
    fn ascii_contiguous_run_uses_only_initial_move() {
        // a, b, x contiguous starting at x=0 => single MoveTo(0,0), then the
        // relative contiguity keeps subsequent cells move-free.
        let out = render(vec![
            (0, 0, cell("a")),
            (1, 0, cell("b")),
            (2, 0, cell("x")),
        ]);

        assert!(contains(&out, "a"));
        assert!(contains(&out, "b"));
        assert!(contains(&out, "x"));

        // Only the initial MoveTo(0,0); no per-cell absolute moves.
        assert!(
            contains(&out, &move_to(0, 0)),
            "initial MoveTo(0,0) expected"
        );
        assert!(
            !contains(&out, &move_to(1, 0)),
            "contiguous ASCII must not emit MoveTo(1,0)"
        );
        assert!(
            !contains(&out, &move_to(2, 0)),
            "contiguous ASCII must not emit MoveTo(2,0)"
        );
    }
}
