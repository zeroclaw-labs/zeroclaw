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
