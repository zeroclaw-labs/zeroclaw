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
    pub action: &'static str,
}

impl HelpEntry {
    pub fn new(keys: Vec<&'static str>, action: &'static str) -> Self {
        Self { keys, action }
    }

    /// Convenience: single key.
    pub fn key(key: &'static str, action: &'static str) -> Self {
        Self { keys: vec![key], action }
    }

    /// Blank spacer row.
    pub fn spacer() -> Self {
        Self { keys: vec![], action: "" }
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
    pub title: Option<&'static str>,
    /// Prose description shown above the keybindings, soft-wrapped to modal width.
    pub description: Option<&'static str>,
    /// Keybinding entries for this level.
    pub entries: Vec<HelpEntry>,
    /// Child nodes (tab-level, widget-level, etc.).
    pub children: Vec<HelpNode>,
}

impl HelpNode {
    /// Leaf node with just keybindings.
    pub fn entries(entries: Vec<HelpEntry>) -> Self {
        Self { entries, ..Default::default() }
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
