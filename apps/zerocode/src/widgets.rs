#![allow(dead_code)]

/// A single help entry: one or more keys that trigger the same action.
///
/// The renderer joins keys with " / " so you don't have to format manually.
/// An entry with all-empty keys/action renders as a blank spacer row.
#[derive(Debug, Clone, Default)]
pub struct HelpEntry {
    /// Keys that trigger this action, e.g. ["↑", "k"].
    pub keys: Vec<&'static str>,
    /// Human-readable description of the action.
    pub action: String,
}

impl HelpEntry {
    pub fn new(keys: Vec<&'static str>, action: impl Into<String>) -> Self {
        Self {
            keys,
            action: action.into(),
        }
    }

    /// Convenience: single key.
    pub fn key(key: &'static str, action: impl Into<String>) -> Self {
        Self {
            keys: vec![key],
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

    /// Format keys as "↑ / k" etc.
    pub fn key_str(&self) -> String {
        self.keys.join(" / ")
    }
}

/// A node in the help context tree.
///
/// The help system cascades: Pane → Tab → Widget (or Screen → Tab → Widget
/// for the config pane). Each level produces one `HelpNode`. The modal renders
/// them depth-first:
///
///   [title]
///   [description, soft-wrapped]
///   key   action
///   key   action
///   ── dim separator ──
///   [child title]
///   ...
///
/// Any field may be empty/None — the renderer skips it cleanly.
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
///
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
