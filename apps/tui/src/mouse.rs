//! Reusable mouse interaction helpers for the TUI.
//!
//! Pure geometry + timing utilities. No pane-specific logic lives here.

use std::io::Write;
use std::time::Instant;

use ratatui::layout::Rect;

// ── Hit testing ──────────────────────────────────────────────────

/// Check whether `(col, row)` is inside `rect`.
pub(crate) fn in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

// ── List helpers ─────────────────────────────────────────────────

/// Map a mouse click row to the item index in a bordered `List` widget.
///
/// Returns `None` if the click lands on a border or outside the item
/// range. `scroll_offset` is `ListState::offset()` (the index of the
/// first visible item).
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

/// Map a click column to the tab index in a tab bar.
///
/// Each tab is rendered as a span of `label.len()` chars, separated
/// by `sep_width` characters (typically `" │ "` = 3). Pass the raw
/// label strings; the function walks their widths to determine hit
/// regions.
pub(crate) fn tab_click_index(
    mouse_col: u16,
    mouse_row: u16,
    tab_area: Rect,
    labels: &[&str],
    sep_width: usize,
) -> Option<usize> {
    if !in_rect(mouse_col, mouse_row, tab_area) {
        return None;
    }
    let mut x = tab_area.x as usize;
    for (i, label) in labels.iter().enumerate() {
        let w = label.len();
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

/// Map a click column to a mode (F-key number 1–5) in the app mode bar.
///
/// The mode bar renders each tab as: `key` + `label` + `" "`.
/// E.g. `"F1"` + `" Dashboard "` + `" "`.
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
        let w = key.len() + label.len() + 1; // +1 for trailing " "
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
pub(crate) fn copy_osc52(text: &str) {
    let encoded = base64_encode(text.as_bytes());
    // OSC 52 ; c ; <base64> ST
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let _ = std::io::stdout().write_all(seq.as_bytes());
    let _ = std::io::stdout().flush();
}

/// Minimal base64 encoder. Standard alphabet, with padding.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
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
