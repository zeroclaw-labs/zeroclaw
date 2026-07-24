//! Reusable mouse interaction helpers for the TUI.
//! Pure geometry + timing utilities. No pane-specific logic lives here.

#[cfg(not(test))]
use std::io::Write;
use std::time::Instant;

use ratatui::layout::Rect;

// ── Hit testing ──────────────────────────────────────────────────

/// Check whether `(col, row)` is inside `rect`.
pub(crate) fn in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// The bottom-left help indicator text shared by every pane footer.
pub(crate) const HELP_HINT: &str = " ?=help";

/// Returns `true` when `(col, row)` falls on the help hint that panes render
/// at the bottom-left of `content_area`. The clickable zone spans the hint
/// width on the area's last row, tolerant of the leading space / border cell.
pub(crate) fn help_hint_click(col: u16, row: u16, content_area: Rect) -> bool {
    use unicode_width::UnicodeWidthStr;
    if content_area.height == 0 {
        return false;
    }
    let row_y = content_area.y + content_area.height - 1;
    let hint = Rect {
        x: content_area.x,
        y: row_y,
        width: UnicodeWidthStr::width(HELP_HINT) as u16,
        height: 1,
    };
    in_rect(col, row, hint)
}

// ── List helpers ─────────────────────────────────────────────────

pub(crate) fn list_click_index(
    mouse_row: u16,
    list_area: Rect,
    scroll_offset: usize,
    item_count: usize,
) -> Option<usize> {
    // The List block has a 1-cell top border.
    let inner_top = list_area.y + 1;
    let inner_bottom = list_area.y + list_area.height.saturating_sub(1);
    if mouse_row < inner_top || mouse_row >= inner_bottom {
        return None;
    }
    let row_in_list = (mouse_row - inner_top) as usize;
    let idx = scroll_offset + row_in_list;
    if idx < item_count { Some(idx) } else { None }
}

/// Compute a new selection index after a scroll event, clamped to
/// `[0, item_count - 1]`.
pub(crate) fn list_scroll(
    current: usize,
    item_count: usize,
    scroll_up: bool,
    amount: usize,
) -> usize {
    if item_count == 0 {
        return 0;
    }
    if scroll_up {
        current.saturating_sub(amount)
    } else {
        (current + amount).min(item_count - 1)
    }
}

// ── Tab bar helpers ──────────────────────────────────────────────

pub(crate) fn tab_click_index(
    mouse_col: u16,
    mouse_row: u16,
    tab_area: Rect,
    labels: &[&str],
    sep_width: usize,
) -> Option<usize> {
    use unicode_width::UnicodeWidthStr;
    if !in_rect(mouse_col, mouse_row, tab_area) {
        return None;
    }
    let mut x = tab_area.x as usize;
    for (i, label) in labels.iter().enumerate() {
        let w = UnicodeWidthStr::width(*label);
        if (mouse_col as usize) >= x && (mouse_col as usize) < x + w {
            return Some(i);
        }
        x += w;
        if i + 1 < labels.len() {
            x += sep_width;
        }
    }
    None
}

pub(crate) fn mode_bar_click(
    mouse_col: u16,
    mouse_row: u16,
    bar_area: Rect,
    labels: &[(&str, &str)],
) -> Option<u8> {
    if !in_rect(mouse_col, mouse_row, bar_area) {
        return None;
    }
    let mut x = bar_area.x as usize;
    for (i, (key, label)) in labels.iter().enumerate() {
        let w = crate::display_width::display_width(key)
            + crate::display_width::display_width(label)
            + 1; // +1 for trailing " "
        if (mouse_col as usize) >= x && (mouse_col as usize) < x + w {
            return Some((i + 1) as u8);
        }
        x += w;
    }
    None
}

// ── Double-click tracker ─────────────────────────────────────────

const DOUBLE_CLICK_MS: u128 = 400;

pub(crate) struct DoubleClickTracker {
    last_col: u16,
    last_row: u16,
    last_time: Instant,
}

impl DoubleClickTracker {
    pub(crate) fn new() -> Self {
        Self {
            last_col: u16::MAX,
            last_row: u16::MAX,
            last_time: Instant::now(),
        }
    }

    /// Record a click. Returns `true` if it forms a double-click
    /// (same cell, within 400ms of the previous click).
    pub(crate) fn click(&mut self, col: u16, row: u16) -> bool {
        let now = Instant::now();
        let is_double = col == self.last_col
            && row == self.last_row
            && now.duration_since(self.last_time).as_millis() < DOUBLE_CLICK_MS;
        self.last_col = col;
        self.last_row = row;
        self.last_time = now;
        if is_double {
            // Reset so a third click doesn't count as another double.
            self.last_col = u16::MAX;
            true
        } else {
            false
        }
    }
}

// ── Clipboard (OSC 52) ──────────────────────────────────────────

/// Copy `text` to the system clipboard via OSC 52.
///
/// Works in most modern terminals (iTerm2, kitty, alacritty, WezTerm,
/// foot, tmux with `set-clipboard on`). Terminals that don't support
/// OSC 52 silently ignore the sequence.
#[cfg(not(test))]
pub(crate) fn copy_osc52(text: &str) {
    let encoded = base64_encode(text.as_bytes());
    // OSC 52 ; c ; <base64> ST
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let _ = std::io::stdout().write_all(seq.as_bytes());
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
pub(crate) fn copy_osc52(_text: &str) {}

/// Minimal base64 encoder. Standard alphabet, with padding.
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{help_hint_click, mode_bar_click, tab_click_index};
    use ratatui::layout::Rect;

    fn bar(width: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height: 1,
        }
    }

    // Regression: tab hit-testing must use display columns, not byte length.
    // CJK labels are 3 bytes but 2 columns each; byte math mapped clicks to
    // the wrong tab. "代码" renders in 4 columns.
    #[test]
    fn tab_click_index_uses_display_width_for_cjk() {
        // labels: "代码" (4 cols) │ "聊天" (4 cols), sep " │ " = 3 cols.
        // Layout: cols 0..4 = tab0, 4..7 = sep, 7..11 = tab1.
        let labels = ["代码", "聊天"];
        assert_eq!(tab_click_index(0, 0, bar(20), &labels, 3), Some(0));
        assert_eq!(tab_click_index(3, 0, bar(20), &labels, 3), Some(0));
        // Separator columns 4,5,6 hit nothing.
        assert_eq!(tab_click_index(5, 0, bar(20), &labels, 3), None);
        // Second tab starts at column 7.
        assert_eq!(tab_click_index(7, 0, bar(20), &labels, 3), Some(1));
        assert_eq!(tab_click_index(10, 0, bar(20), &labels, 3), Some(1));
    }

    #[test]
    fn tab_click_index_ascii_unchanged() {
        let labels = ["Code", "Chat"];
        // "Code" cols 0..4, sep 4..7, "Chat" cols 7..11.
        assert_eq!(tab_click_index(0, 0, bar(20), &labels, 3), Some(0));
        assert_eq!(tab_click_index(7, 0, bar(20), &labels, 3), Some(1));
        assert_eq!(tab_click_index(5, 0, bar(20), &labels, 3), None);
    }

    // Regression: mode bar hit-testing must use display columns too. Each
    // entry is `key` + `label` + a trailing space.
    #[test]
    fn mode_bar_click_uses_display_width_for_cjk() {
        // entry0: key "" + label " 仪表板 " (3 CJK = 6 cols + 2 spaces = 8) + 1
        //         trailing space = 9 cols -> covers 0..9.
        // entry1: key "" + label " 聊天 " (2 CJK = 4 + 2 spaces = 6) + 1 = 7
        //         cols -> covers 9..16.
        let labels = [("", " 仪表板 "), ("", " 聊天 ")];
        assert_eq!(mode_bar_click(0, 0, bar(30), &labels), Some(1));
        assert_eq!(mode_bar_click(8, 0, bar(30), &labels), Some(1));
        assert_eq!(mode_bar_click(9, 0, bar(30), &labels), Some(2));
        assert_eq!(mode_bar_click(15, 0, bar(30), &labels), Some(2));
    }

    #[test]
    fn help_hint_click_hits_bottom_left() {
        let area = Rect {
            x: 4,
            y: 2,
            width: 40,
            height: 10,
        };
        let bottom = area.y + area.height - 1;
        assert!(help_hint_click(area.x, bottom, area), "left edge");
        assert!(help_hint_click(area.x + 5, bottom, area), "within hint");
        assert!(!help_hint_click(area.x + 20, bottom, area), "past hint");
        assert!(!help_hint_click(area.x, bottom - 1, area), "wrong row");
    }
}
