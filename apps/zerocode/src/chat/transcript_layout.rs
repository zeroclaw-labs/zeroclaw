use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use ratatui::text::Line;

use super::{
    CachedCodeBlock, ChatEntry, MAX_RENDERED_ENTRIES, ToolDisclosure, TranscriptRowBreak,
    UrlLineRegion, fenced_text, header_fence_lang, label_cells, offset_url_line_regions,
    render_entry_into, row_breaks_for_lines, url_line_regions_for_lines, wrapped_rows,
};

/// Tracks which committed entries need to be rendered again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinesDirty {
    /// Committed layout is current.
    Clean,
    /// Extend the unchanged render window without reparsing earlier entries.
    Appended,
    /// Replace the final cached entry when its render window still matches.
    TailChanged(usize),
    /// Entry mutation, presentation change, width change, or reset requires all indexes.
    Full,
}

/// Borrowed canonical entry and its presentation for this rebuild only.
pub(super) struct EntryLayoutInput<'a> {
    pub index: usize,
    pub entry: &'a ChatEntry,
    pub highlighted: bool,
    pub disclosure: ToolDisclosure,
    pub local_file_diff: Option<&'a zeroclaw_api::local_file_diff::LocalFileDiff>,
}

/// Coupled committed layout indexes. Production consumers only borrow this view.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(super) struct TranscriptLayoutView {
    pub cached_lines: Vec<Line<'static>>,
    /// Source-derived separator before each wrapped screen row.
    pub cached_row_breaks: Vec<TranscriptRowBreak>,
    /// Absolute entry index and its unwrapped line range, excluding omitted entries.
    pub cached_line_ranges: Vec<(usize, usize, usize)>,
    /// Absolute disclosure-footer line index for each file-tool entry.
    pub cached_tool_footer_lines: BTreeMap<usize, usize>,
    /// Wrapped row spans for individual lines and committed entries.
    pub cached_line_screen_ranges: Vec<(u16, u16)>,
    /// Entry index, wrapped row range, and width used to exclude adjacent blank cells.
    pub cached_screen_ranges: Vec<(usize, u16, u16, u16)>,
    /// Full copy text shared by visible fence targets without steady-state rescanning.
    pub cached_code_blocks: Vec<CachedCodeBlock>,
    /// Tagged logical URL lines with transcript-relative row extents.
    pub cached_url_regions: Vec<UrlLineRegion>,
    pub dirty: LinesDirty,
    /// Number of source entries in the cached window, including hidden thoughts.
    pub cached_entry_count: usize,
    pub cached_render_start: usize,
    /// Layout width; changing it invalidates tables and every screen index.
    pub cached_render_width: u16,
    pub cached_total_rows: u16,
}

/// Owns all derived layout and its invalidation on the synchronous UI thread.
#[derive(Debug)]
pub(super) struct TranscriptLayoutCache {
    layout: TranscriptLayoutView,
}

impl TranscriptLayoutCache {
    pub(super) fn new() -> Self {
        Self {
            layout: TranscriptLayoutView {
                cached_lines: Vec::new(),
                cached_row_breaks: Vec::new(),
                cached_line_ranges: Vec::new(),
                cached_tool_footer_lines: BTreeMap::new(),
                cached_line_screen_ranges: Vec::new(),
                cached_screen_ranges: Vec::new(),
                cached_code_blocks: Vec::new(),
                cached_url_regions: Vec::new(),
                dirty: LinesDirty::Full,
                cached_entry_count: 0,
                cached_render_start: 0,
                cached_render_width: 0,
                cached_total_rows: 0,
            },
        }
    }

    pub(super) fn view(&self) -> &TranscriptLayoutView {
        &self.layout
    }

    #[cfg(test)]
    pub(super) fn fixture_mut(&mut self) -> &mut TranscriptLayoutView {
        &mut self.layout
    }

    pub(super) fn invalidate_append(&mut self) {
        match self.layout.dirty {
            LinesDirty::Clean => self.layout.dirty = LinesDirty::Appended,
            LinesDirty::TailChanged(_) => self.layout.dirty = LinesDirty::Full,
            LinesDirty::Appended | LinesDirty::Full => {}
        }
        // Full invalidation is sticky.
    }

    pub(super) fn invalidate_tail(&mut self, entry_index: usize) {
        match self.layout.dirty {
            LinesDirty::Clean => self.layout.dirty = LinesDirty::TailChanged(entry_index),
            LinesDirty::TailChanged(index) if index == entry_index => {}
            LinesDirty::Appended => {
                // An intervening append does not make previously cached text fresh.
                if (self.layout.cached_render_start
                    ..self
                        .layout
                        .cached_render_start
                        .saturating_add(self.layout.cached_entry_count))
                    .contains(&entry_index)
                {
                    self.invalidate_full();
                }
            }
            LinesDirty::TailChanged(_) | LinesDirty::Full => self.invalidate_full(),
        }
    }

    pub(super) fn invalidate_full(&mut self) {
        self.layout.dirty = LinesDirty::Full;
    }

    pub(super) fn reset(&mut self) {
        self.layout.cached_lines.clear();
        self.layout.cached_row_breaks.clear();
        self.layout.cached_line_ranges.clear();
        self.layout.cached_tool_footer_lines.clear();
        self.layout.cached_line_screen_ranges.clear();
        self.layout.cached_screen_ranges.clear();
        self.layout.cached_code_blocks.clear();
        self.layout.cached_url_regions.clear();
        self.layout.dirty = LinesDirty::Full;
        self.layout.cached_entry_count = 0;
        self.layout.cached_render_start = 0;
        self.layout.cached_render_width = 0;
        self.layout.cached_total_rows = 0;
    }

    pub(super) fn set_render_start(&mut self, start: usize) {
        self.layout.cached_render_start = start;
        self.invalidate_full();
    }

    pub(super) fn render_range(
        &self,
        total: usize,
        pinned_to_bottom: bool,
        browse_cursor: Option<usize>,
        width: u16,
    ) -> Range<usize> {
        let natural_start = total.saturating_sub(MAX_RENDERED_ENTRIES);
        let mut start = if pinned_to_bottom || width == 0 {
            natural_start
        } else {
            self.layout.cached_render_start.min(natural_start)
        };
        if let Some(cursor) = browse_cursor {
            if cursor < start {
                start = cursor;
            } else if cursor >= start.saturating_add(MAX_RENDERED_ENTRIES) {
                start = cursor
                    .saturating_add(1)
                    .saturating_sub(MAX_RENDERED_ENTRIES);
            }
        }
        start = start.min(natural_start);
        start..start.saturating_add(MAX_RENDERED_ENTRIES).min(total)
    }

    pub(super) fn rebuild<'a>(
        &mut self,
        width: u16,
        range: Range<usize>,
        show_thoughts: bool,
        mut inputs: impl Iterator<Item = EntryLayoutInput<'a>>,
    ) {
        if self.layout.cached_render_width != width {
            self.invalidate_full();
            self.layout.cached_render_width = width;
        }

        // A prompt-response fallback may settle just before its final chunks arrive.
        if let LinesDirty::TailChanged(entry_index) = self.layout.dirty
            && range.start == self.layout.cached_render_start
            && entry_index + 1 == range.end
            && let Some(range_pos) = self
                .layout
                .cached_line_ranges
                .iter()
                .position(|&(index, _, _)| index == entry_index)
            && range_pos + 1 == self.layout.cached_line_ranges.len()
        {
            let line_start = self.layout.cached_line_ranges[range_pos].1;
            let row_start = self.layout.cached_line_screen_ranges[line_start].0;
            self.layout.cached_lines.truncate(line_start);
            self.layout.cached_line_ranges.truncate(range_pos);
            self.layout.cached_tool_footer_lines.remove(&entry_index);
            if let Some(input) = inputs.nth(entry_index - range.start) {
                self.append_entry(input, show_thoughts, width);
            }
            self.layout.cached_row_breaks = row_breaks_for_lines(&self.layout.cached_lines, width);
            if row_start == u16::MAX {
                // Saturated offsets cannot distinguish the unchanged prefix.
                self.layout.cached_url_regions =
                    url_line_regions_for_lines(&self.layout.cached_lines, width);
            } else {
                self.layout
                    .cached_url_regions
                    .retain(|region| region.row < row_start);
                let mut regions =
                    url_line_regions_for_lines(&self.layout.cached_lines[line_start..], width);
                offset_url_line_regions(&mut regions, row_start);
                self.layout.cached_url_regions.extend(regions);
            }
        } else if self.layout.dirty == LinesDirty::Appended
            && range.start == self.layout.cached_render_start
        {
            let line_start = self.layout.cached_lines.len();
            for input in inputs.skip(self.layout.cached_entry_count) {
                self.append_entry(input, show_thoughts, width);
            }
            let new_lines = &self.layout.cached_lines[line_start..];
            self.layout
                .cached_row_breaks
                .extend(row_breaks_for_lines(new_lines, width));
            let mut regions = url_line_regions_for_lines(new_lines, width);
            offset_url_line_regions(&mut regions, self.layout.cached_total_rows);
            self.layout.cached_url_regions.extend(regions);
        } else {
            self.layout.cached_lines.clear();
            self.layout.cached_line_ranges.clear();
            self.layout.cached_tool_footer_lines.clear();
            for input in inputs {
                self.append_entry(input, show_thoughts, width);
            }
            self.layout.cached_row_breaks = row_breaks_for_lines(&self.layout.cached_lines, width);
            self.layout.cached_url_regions =
                url_line_regions_for_lines(&self.layout.cached_lines, width);
        }

        self.layout.cached_entry_count = range.len();
        self.layout.cached_render_start = range.start;
        self.rebuild_screen_ranges(width);
        self.layout.dirty = LinesDirty::Clean;
    }

    /// Every rebuild mode publishes identical absolute line/footer offsets and
    /// omits entries that render no lines, such as hidden thoughts.
    fn append_entry(&mut self, input: EntryLayoutInput<'_>, show_thoughts: bool, width: u16) {
        let before = self.layout.cached_lines.len();
        let footer_line = render_entry_into(
            input.entry,
            input.highlighted,
            show_thoughts,
            input.disclosure,
            width,
            input.local_file_diff,
            &mut self.layout.cached_lines,
        );
        let after = self.layout.cached_lines.len();
        if after > before {
            self.layout
                .cached_line_ranges
                .push((input.index, before, after));
        }
        if let Some(footer_line) = footer_line {
            self.layout
                .cached_tool_footer_lines
                .insert(input.index, footer_line);
        }
    }

    /// Recompute all screen and copy indexes together after committed lines change.
    fn rebuild_screen_ranges(&mut self, width: u16) {
        self.layout.cached_line_screen_ranges.clear();
        self.layout.cached_screen_ranges.clear();
        self.layout.cached_code_blocks.clear();
        let mut screen_cursor = 0u16;
        let mut pending_fence: Option<(u16, u16, u16, usize, Option<String>, String)> = None;

        for line in &self.layout.cached_lines {
            let line_start = screen_cursor;
            screen_cursor = screen_cursor.saturating_add(wrapped_rows(line, width));
            self.layout
                .cached_line_screen_ranges
                .push((line_start, screen_cursor));

            let first = line.spans.first().map(|s| s.content.as_ref()).unwrap_or("");
            if first.starts_with('\u{250c}') {
                let lang = header_fence_lang(line);
                pending_fence = label_cells(line, " [Copy] ").map(|(col, cells)| {
                    (
                        line_start,
                        col,
                        cells,
                        line_start as usize,
                        lang,
                        String::new(),
                    )
                });
            } else if first.starts_with('\u{2514}') {
                if let Some((header_row, header_col, header_cells, group, lang, body)) =
                    pending_fence.take()
                {
                    self.layout.cached_code_blocks.push(CachedCodeBlock {
                        header_row,
                        block_end: screen_cursor,
                        header_label: (header_col, header_cells),
                        footer_row: line_start,
                        footer_label: label_cells(line, " [Copy] "),
                        text: Arc::<str>::from(fenced_text(lang.as_deref(), &body)),
                        group,
                    });
                }
            } else if let Some((_, _, _, _, _, body)) = pending_fence.as_mut() {
                let full: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let body_text = full.strip_prefix("  ").unwrap_or(&full);
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(body_text);
            }
        }

        self.layout.cached_total_rows = screen_cursor;
        for &(entry_idx, lo, hi) in &self.layout.cached_line_ranges {
            if lo >= hi {
                continue;
            }
            // Wrapping fills the viewport; short entries exclude adjacent blank cells.
            let content_width = self.layout.cached_lines[lo..hi]
                .iter()
                .map(|l| l.width() as u16)
                .max()
                .unwrap_or(0)
                .min(width);
            let Some(&(screen_lo, _)) = self.layout.cached_line_screen_ranges.get(lo) else {
                continue;
            };
            let Some(&(_, screen_hi)) = self.layout.cached_line_screen_ranges.get(hi - 1) else {
                continue;
            };
            self.layout
                .cached_screen_ranges
                .push((entry_idx, screen_lo, screen_hi, content_width));
        }
    }
}
