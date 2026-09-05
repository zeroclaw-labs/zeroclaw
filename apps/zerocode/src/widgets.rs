#![allow(dead_code)]

#[derive(Debug, Clone, Default)]
pub struct HelpEntry {
    /// Keys that trigger this action, e.g. ["↑", "k"]. Rendered labels,
    /// owned so registry-derived chord labels (`Chord::display`) can be
    /// used alongside static literals.
    pub keys: Vec<String>,
    /// Human-readable description of the action.
    pub action: String,
}

impl HelpEntry {
    pub fn new<K: Into<String>>(
        keys: impl IntoIterator<Item = K>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            keys: keys.into_iter().map(Into::into).collect(),
            action: action.into(),
        }
    }

    /// Convenience: single key.
    pub fn key(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            keys: vec![key.into()],
            action: action.into(),
        }
    }

    /// Blank spacer row.
    pub fn spacer() -> Self {
        Self {
            keys: vec![],
            action: String::new(),
        }
    }

    /// Keyless description row (no key column, just text).
    pub fn desc(action: impl Into<String>) -> Self {
        Self {
            keys: vec![],
            action: action.into(),
        }
    }

    /// Format keys as "↑ / k" etc.
    pub fn key_str(&self) -> String {
        self.keys.join(" / ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct HelpNode {
    /// Short label shown as a dim section header (e.g. "Tab", "Widget"). None = no header.
    pub title: Option<String>,
    /// Prose description shown above the keybindings, soft-wrapped to modal width.
    pub description: Option<String>,
    /// Keybinding entries for this level.
    pub entries: Vec<HelpEntry>,
    /// Child nodes (tab-level, widget-level, etc.).
    pub children: Vec<HelpNode>,
}

impl HelpNode {
    /// Leaf node with just keybindings.
    pub fn entries(entries: Vec<HelpEntry>) -> Self {
        Self {
            entries,
            ..Default::default()
        }
    }

    pub fn titled(title: impl Into<String>, entries: Vec<HelpEntry>) -> Self {
        Self {
            title: Some(title.into()),
            entries,
            ..Default::default()
        }
    }

    /// Consume self and append a child node, returning the modified node.
    pub fn with_child(mut self, child: HelpNode) -> Self {
        self.children.push(child);
        self
    }
}

/// Implement this on any struct that can contribute to the help modal.
pub trait HelpContext {
    fn help_context(&self) -> HelpNode;
}

// ── CtxBar ────────────────────────────────────────────────────────────────────

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// A one-row context-window usage bar.
/// Renders left-aligned into whatever `Rect` you hand it.
/// Returns `None` from `widget()` when there is nothing to show.
pub struct CtxBar {
    pub input_tokens: Option<u64>,
    pub max_tokens: Option<u64>,
}

impl CtxBar {
    pub fn new(input_tokens: Option<u64>, max_tokens: Option<u64>) -> Self {
        Self {
            input_tokens,
            max_tokens,
        }
    }

    /// `true` when there is something worth rendering.
    pub fn has_content(&self) -> bool {
        self.input_tokens.is_some() || self.max_tokens.is_some()
    }

    /// Build a `Paragraph` widget, or `None` if there is nothing to show.
    pub fn widget(&self) -> Option<Paragraph<'static>> {
        let (text, pct_opt) = match (self.input_tokens, self.max_tokens) {
            (Some(used), Some(max)) if max > 0 => {
                let pct = (used as f64 / max as f64 * 100.0).min(100.0);
                let bar_width: usize = 16;
                let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
                let empty = bar_width.saturating_sub(filled);
                let bar = format!(
                    "[{}{}]",
                    "\u{2588}".repeat(filled),
                    "\u{2591}".repeat(empty)
                );
                let label = format!(
                    " ctx: {:>7} / {:>7}  {}  {:.0}%",
                    fmt_tokens(used),
                    fmt_tokens(max),
                    bar,
                    pct,
                );
                (label, Some(pct))
            }
            (Some(used), None) => {
                let label = format!(" ctx: {} tokens", fmt_tokens(used));
                (label, None)
            }
            _ => return None,
        };

        let color = match pct_opt {
            Some(p) if p >= 90.0 => Color::Red,
            Some(p) if p >= 75.0 => Color::Yellow,
            _ => Color::DarkGray,
        };

        Some(Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(color),
        ))))
    }
}

fn fmt_tokens(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

// ── InfoBar ─────────────────────────────────────────────────────────────────

use std::time::{Duration, Instant};

/// How long an info message stays on the bar before it auto-clears. Named so
/// the timeout is not a bare literal at the clear site.
pub const INFO_BAR_TTL: Duration = Duration::from_secs(10);

/// Severity of an info-bar message. Drives the colour; never matched on as a
/// string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoKind {
    /// Neutral operational note (e.g. "Fetching models for anthropic…").
    Info,
    /// A completed action worth confirming (e.g. "Model switched to …").
    Note,
    /// A failure the user should see (e.g. an RPC error).
    Error,
}

/// A single user-facing message shown on the info bar. Owned by the app layer
/// as `Option<InfoMessage>`; `None` means the bar is hidden. `set_at` drives the
/// [`INFO_BAR_TTL`] auto-clear in the app tick loop.
#[derive(Debug, Clone)]
pub struct InfoMessage {
    pub kind: InfoKind,
    pub text: String,
    pub set_at: Instant,
}

impl InfoMessage {
    pub fn new(kind: InfoKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            set_at: Instant::now(),
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self::new(InfoKind::Info, text)
    }

    pub fn note(text: impl Into<String>) -> Self {
        Self::new(InfoKind::Note, text)
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self::new(InfoKind::Error, text)
    }

    /// `true` once the message has been visible for at least [`INFO_BAR_TTL`].
    pub fn is_expired(&self) -> bool {
        self.set_at.elapsed() >= INFO_BAR_TTL
    }
}

/// A one-row, single-line info bar. Renders the current message truncated to the
/// available width; stores the full text untruncated so a wider window shows
/// more without any state change.
pub struct InfoBar<'a> {
    message: Option<&'a InfoMessage>,
}

impl<'a> InfoBar<'a> {
    pub fn new(message: Option<&'a InfoMessage>) -> Self {
        Self { message }
    }

    pub fn has_content(&self) -> bool {
        self.message.is_some()
    }

    /// Build the `Paragraph`, or `None` when there is no message. `width` is the
    /// available column count; the text is truncated (with an ellipsis) to fit.
    pub fn widget(&self, width: usize) -> Option<Paragraph<'static>> {
        let msg = self.message?;
        let palette = crate::theme::active();
        let color = match msg.kind {
            InfoKind::Info => palette.dim,
            InfoKind::Note => palette.accent,
            InfoKind::Error => palette.warn,
        };
        let text = truncate_to_width(&msg.text, width);
        Some(Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(color),
        ))))
    }
}

/// Truncate `s` to at most `width` display columns, appending an ellipsis when
/// it overflows. Approximates width by `char` count — adequate for the
/// single-line status text the info bar carries.
fn truncate_to_width(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width == 1 {
        return "\u{2026}".to_string();
    }
    let keep = width - 1;
    let mut out: String = s.chars().take(keep).collect();
    out.push('\u{2026}');
    out
}

// ── PickerModal ─────────────────────────────────────────────────────────────

use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};
use unicode_width::UnicodeWidthStr;

/// Cells between a row label and its `current` suffix.
const CURRENT_SUFFIX_GAP: usize = 2;

pub struct PickerModal<'a> {
    title: &'a str,
    items: &'a [String],
    cursor: usize,
    /// Row whose value is currently in force, with the localized suffix it
    /// is drawn with.
    current: Option<(usize, &'a str)>,
}

impl<'a> PickerModal<'a> {
    pub fn new(title: &'a str, items: &'a [String], cursor: usize) -> Self {
        Self {
            title,
            items,
            cursor,
            current: None,
        }
    }

    /// Tag `row` as the value currently in force by appending a dim `label`
    /// suffix. A word rather than a glyph: East Asian labels already mix
    /// single- and double-width cells, and a marker glyph would misalign the
    /// column. `None` draws every row plain.
    pub fn with_current(mut self, row: Option<usize>, label: &'a str) -> Self {
        self.current = row.map(|row| (row, label));
        self
    }

    /// Modal rect for a picker without a current-row suffix.
    pub fn area_for(title: &str, items: &[String], area: Rect) -> Option<Rect> {
        PickerModal::new(title, items, 0).area(area)
    }

    /// The rect this modal renders into, centered within `area`; `None` when
    /// there are no rows. Keep this geometry in sync with `render` so mouse
    /// hit-testing lands on the same rows the user sees.
    pub fn area(&self, area: Rect) -> Option<Rect> {
        if self.items.is_empty() {
            return None;
        }

        let longest = self
            .items
            .iter()
            .enumerate()
            .map(|(row, label)| self.row_width(row, label))
            .max()
            .unwrap_or(0)
            .max(UnicodeWidthStr::width(self.title));
        let inner_w = longest + 2; // 1 col padding each side
        let box_w = (inner_w + 2).clamp(12, area.width as usize) as u16;
        let box_h = (self.items.len() + 2).clamp(3, area.height as usize) as u16;

        let x = area.x + area.width.saturating_sub(box_w) / 2;
        let y = area.y + area.height.saturating_sub(box_h) / 2;
        Some(Rect::new(x, y, box_w, box_h))
    }

    fn current_suffix(&self, row: usize) -> Option<&'a str> {
        self.current
            .and_then(|(current, label)| (current == row).then_some(label))
    }

    fn row_width(&self, row: usize, label: &str) -> usize {
        let base = UnicodeWidthStr::width(label);
        match self.current_suffix(row) {
            Some(suffix) => base + CURRENT_SUFFIX_GAP + UnicodeWidthStr::width(suffix),
            None => base,
        }
    }

    fn row_line(&self, row: usize, label: &str, style: ratatui::style::Style) -> Line<'static> {
        let mut spans = vec![Span::styled(label.to_string(), style)];
        if let Some(suffix) = self.current_suffix(row) {
            spans.push(Span::styled(
                format!("{}{suffix}", " ".repeat(CURRENT_SUFFIX_GAP)),
                crate::theme::dim_style(),
            ));
        }
        Line::from(spans)
    }

    /// Render the modal centered within `area`. No-op when there are no items.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let Some(modal_rect) = self.area(area) else {
            return;
        };

        frame.render_widget(Clear, modal_rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(crate::theme::overlay_border_style())
            .style(crate::theme::fill_style())
            .title(Span::styled(
                format!(" {} ", self.title),
                crate::theme::heading_style(),
            ));

        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let style = if i == self.cursor {
                    crate::theme::selected_style()
                } else {
                    crate::theme::body_style()
                };
                ListItem::new(self.row_line(i, label, style))
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.cursor.min(self.items.len().saturating_sub(1))));

        let list = List::new(items)
            .block(block)
            .highlight_style(crate::theme::selected_style());

        frame.render_stateful_widget(list, modal_rect, &mut list_state);
    }
}

/// Owned state for a [`PickerModal`]: the items and the current cursor. Caller
/// holds this and drives it with the movement helpers; the widget borrows it for
/// rendering. Reusable by any surface that needs a string picker.
#[derive(Debug, Clone, Default)]
pub struct PickerState {
    pub items: Vec<String>,
    pub cursor: usize,
    /// Row holding the value currently in force, drawn with the `current`
    /// suffix. `None` when that value is not among the rows.
    pub current: Option<usize>,
}

impl PickerState {
    /// Build a picker over `items`. `default` is the value currently in
    /// force: when present it is pre-selected and marked as the current row,
    /// else the cursor starts on the first row and no row is marked.
    pub fn new(items: Vec<String>, default: Option<&str>) -> Self {
        let current = default.and_then(|d| items.iter().position(|i| i == d));
        Self {
            items,
            cursor: current.unwrap_or(0),
            current,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.items.len() {
            self.cursor += 1;
        }
    }

    /// The currently highlighted value, if any.
    pub fn selected(&self) -> Option<&str> {
        self.items.get(self.cursor).map(String::as_str)
    }
}

#[cfg(test)]
mod info_bar_tests {
    use super::*;

    #[test]
    fn truncate_shorter_than_width_is_unchanged() {
        assert_eq!(truncate_to_width("model", 10), "model");
    }

    #[test]
    fn truncate_exact_width_is_unchanged() {
        assert_eq!(truncate_to_width("model", 5), "model");
    }

    #[test]
    fn truncate_overflow_appends_ellipsis() {
        assert_eq!(truncate_to_width("anthropic", 5), "anth\u{2026}");
    }

    #[test]
    fn truncate_zero_width_is_empty() {
        assert_eq!(truncate_to_width("anything", 0), "");
    }

    #[test]
    fn truncate_width_one_is_ellipsis() {
        assert_eq!(truncate_to_width("anything", 1), "\u{2026}");
    }

    #[test]
    fn fresh_message_is_not_expired() {
        let m = InfoMessage::info("hi");
        assert!(!m.is_expired());
    }

    #[test]
    fn ttl_aged_message_is_expired() {
        let mut m = InfoMessage::error("boom");
        m.set_at = Instant::now() - INFO_BAR_TTL - Duration::from_secs(1);
        assert!(m.is_expired());
    }

    #[test]
    fn no_message_renders_nothing() {
        let bar = InfoBar::new(None);
        assert!(!bar.has_content());
        assert!(bar.widget(80).is_none());
    }

    #[test]
    fn message_renders_widget() {
        let m = InfoMessage::note("switched");
        let bar = InfoBar::new(Some(&m));
        assert!(bar.has_content());
        assert!(bar.widget(80).is_some());
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;

    #[test]
    fn area_for_uses_display_width_for_wide_items() {
        let items = vec!["界界界界界界".to_string()];

        let area = PickerModal::area_for("Pick", &items, Rect::new(0, 0, 80, 24)).unwrap();

        assert_eq!(area.width, 16);
    }

    #[test]
    fn area_for_uses_display_width_for_wide_title() {
        let items = vec!["one".to_string()];

        let area =
            PickerModal::area_for("界界界界界界界", &items, Rect::new(0, 0, 80, 24)).unwrap();

        assert_eq!(area.width, 18);
    }

    #[test]
    fn new_defaults_to_first_when_no_default() {
        let p = PickerState::new(vec!["a".into(), "b".into()], None);
        assert_eq!(p.cursor, 0);
        assert_eq!(p.selected(), Some("a"));
    }

    #[test]
    fn new_preselects_default_when_present() {
        let p = PickerState::new(vec!["a".into(), "b".into(), "c".into()], Some("b"));
        assert_eq!(p.cursor, 1);
        assert_eq!(p.selected(), Some("b"));
    }

    #[test]
    fn new_default_absent_falls_back_to_first() {
        let p = PickerState::new(vec!["a".into(), "b".into()], Some("zzz"));
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn movement_clamps_at_bounds() {
        let mut p = PickerState::new(vec!["a".into(), "b".into()], None);
        p.move_up(); // already at top
        assert_eq!(p.cursor, 0);
        p.move_down();
        assert_eq!(p.cursor, 1);
        p.move_down(); // already at bottom
        assert_eq!(p.cursor, 1);
    }

    #[test]
    fn empty_picker_has_no_selection() {
        let p = PickerState::default();
        assert!(p.is_empty());
        assert_eq!(p.selected(), None);
    }

    #[test]
    fn new_marks_the_default_as_the_current_row() {
        let p = PickerState::new(vec!["a".into(), "b".into()], Some("b"));
        assert_eq!(p.current, Some(1));

        let p = PickerState::new(vec!["a".into(), "b".into()], None);
        assert_eq!(p.current, None);

        let p = PickerState::new(vec!["a".into(), "b".into()], Some("zzz"));
        assert_eq!(p.current, None);
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn area_widens_for_the_current_row_suffix() {
        let items = vec!["low".to_string(), "high".to_string()];
        let area = Rect::new(0, 0, 80, 24);

        let plain = PickerModal::area_for("Pick", &items, area).unwrap();
        assert_eq!(plain.width, 12, "short rows sit at the minimum width");

        let marked = PickerModal::new("Pick", &items, 0)
            .with_current(Some(1), "current")
            .area(area)
            .unwrap();
        // "high" + two cells + "current" is the longest row (13), plus one
        // cell of padding and one border on each side.
        assert_eq!(marked.width, 17);
        assert_eq!(marked.height, plain.height);

        let unmarked = PickerModal::new("Pick", &items, 0)
            .with_current(None, "current")
            .area(area)
            .unwrap();
        assert_eq!(unmarked, plain, "no current row leaves the geometry alone");
    }

    #[test]
    fn current_suffix_geometry_uses_display_width() {
        let items = vec!["界界界界界界".to_string()];

        let marked = PickerModal::new("P", &items, 0)
            .with_current(Some(0), "現在")
            .area(Rect::new(0, 0, 80, 24))
            .unwrap();

        // 12 cells of label + 2 gap + 4 cells of suffix, then padding and border.
        assert_eq!(marked.width, 22);
    }

    #[test]
    fn current_row_renders_the_suffix_only_on_that_row() {
        use ratatui::{Terminal, backend::TestBackend};

        let items = vec!["low".to_string(), "high".to_string()];
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| {
                PickerModal::new("Pick", &items, 0)
                    .with_current(Some(1), "current")
                    .render(frame, frame.area());
            })
            .expect("draw picker");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let rows: Vec<String> = buffer
            .content()
            .chunks(width)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect())
            .collect();
        assert!(
            rows.iter().any(|row| row.contains("high  current")),
            "the current row carries the suffix: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("low  current")),
            "other rows stay plain: {rows:?}"
        );
    }
}
