//! ZeroClaw TUI colour palette and style helpers.
//!
//! Shared between the onboarding UI (lib target) and the main chat TUI (binary
//! target). Not every helper is used by both targets.
#![allow(dead_code)]

use std::sync::{LazyLock, RwLock};

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    pub title: Color,
    pub heading: Color,
    pub body: Color,
    pub dim: Color,
    pub accent: Color,
    pub warn: Color,
    pub selection_bg: Color,
    pub tool: Color,
    pub background: Color,
}

/// "Inherit shell" — uses the terminal's own default colours. Every
/// role is `Color::Reset`, and the app-level backdrop skips painting
/// when `background` is `Reset`, so a user's tuned terminal palette
/// shows through untouched.
const TERMINAL: Theme = Theme {
    title: Color::Reset,
    heading: Color::Reset,
    body: Color::Reset,
    dim: Color::Reset,
    accent: Color::Reset,
    warn: Color::Reset,
    selection_bg: Color::Reset,
    tool: Color::Reset,
    background: Color::Reset,
};

// The named preset palettes are generated at build time from
// `web/src/contexts/themes.json`, the single source of truth shared with the
// React dashboard and mdBook docs. See `build.rs` for the var→role mapping.
// `TERMINAL` is authored here because it is the inherit-shell sentinel, not a
// real palette.
include!(concat!(env!("OUT_DIR"), "/theme_presets.rs"));

pub(crate) const DEFAULT_THEME_NAME: &str = "icy_blue";

/// The authored inherit-shell sentinel. Real palettes come from
/// `GENERATED_THEMES`.
const AUTHORED_THEMES: &[(&str, Theme)] = &[("terminal", TERMINAL)];

/// Every named preset: the authored pair followed by the generated registry
/// themes. The single iteration point both lookup helpers walk.
fn all_themes() -> impl Iterator<Item = &'static (&'static str, Theme)> {
    AUTHORED_THEMES.iter().chain(GENERATED_THEMES.iter())
}

pub(crate) fn theme_by_name(name: &str) -> Option<Theme> {
    all_themes().find_map(|(n, t)| (*n == name).then_some(*t))
}

pub(crate) fn theme_names() -> impl Iterator<Item = &'static str> {
    all_themes().map(|(n, _)| *n)
}

static ACTIVE: LazyLock<RwLock<Theme>> = LazyLock::new(|| RwLock::new(default_theme()));

#[cfg(test)]
static ACTIVE_TEST_LOCK: LazyLock<std::sync::Mutex<()>> =
    LazyLock::new(|| std::sync::Mutex::new(()));

/// Per-agent theme overrides, keyed by agent alias. A process-global registry
/// mirroring `ACTIVE`: the Config pane writes here on assign/clear (live, no
/// restart), and the app loop reads it each frame to tint the Code/Chat pane
/// for the focused agent. Lazily created so the static stays const-initialised.
static AGENT_OVERRIDES: RwLock<Option<std::collections::HashMap<String, Theme>>> =
    RwLock::new(None);

/// Replace the whole agent-override registry (loaded once at startup).
pub(crate) fn set_agent_overrides(map: std::collections::HashMap<String, Theme>) {
    if let Ok(mut guard) = AGENT_OVERRIDES.write() {
        *guard = Some(map);
    }
}

/// Insert or replace one agent's override (live assign from the Config pane).
pub(crate) fn set_agent_override(alias: &str, theme: Theme) {
    if let Ok(mut guard) = AGENT_OVERRIDES.write() {
        guard
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(alias.to_string(), theme);
    }
}

/// Remove one agent's override (live clear from the Config pane).
pub(crate) fn clear_agent_override(alias: &str) {
    if let Ok(mut guard) = AGENT_OVERRIDES.write()
        && let Some(map) = guard.as_mut()
    {
        map.remove(alias);
    }
}

/// The override palette for `alias`, if any. Read each frame by the app loop.
pub(crate) fn agent_override(alias: &str) -> Option<Theme> {
    AGENT_OVERRIDES
        .read()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(alias).copied()))
}

pub(crate) fn set_active(theme: Theme) {
    if let Ok(mut guard) = ACTIVE.write() {
        *guard = theme;
    }
}

pub(crate) fn active() -> Theme {
    let raw = active_raw();
    Theme {
        title: crate::color_depth::downgrade(raw.title),
        heading: crate::color_depth::downgrade(raw.heading),
        body: crate::color_depth::downgrade(raw.body),
        dim: crate::color_depth::downgrade(raw.dim),
        accent: crate::color_depth::downgrade(raw.accent),
        warn: crate::color_depth::downgrade(raw.warn),
        selection_bg: crate::color_depth::downgrade(raw.selection_bg),
        tool: crate::color_depth::downgrade(raw.tool),
        background: crate::color_depth::downgrade(raw.background),
    }
}

/// The stored palette without colour-depth downgrade. Used to snapshot and
/// restore the base theme around a per-frame override swap: `set_active` stores
/// raw RGB, so save/restore must round-trip the raw value, not the downgraded
/// one `active()` returns.
pub(crate) fn active_raw() -> Theme {
    ACTIVE
        .read()
        .map(|g| *g)
        .unwrap_or_else(|_| default_theme())
}

#[cfg(test)]
pub(crate) struct ActiveThemeTestGuard {
    previous: Theme,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for ActiveThemeTestGuard {
    fn drop(&mut self) {
        set_active(self.previous);
    }
}

#[cfg(test)]
pub(crate) fn set_active_for_test(theme: Theme) -> ActiveThemeTestGuard {
    let lock = ACTIVE_TEST_LOCK
        .lock()
        .expect("active theme test lock poisoned");
    let previous = active_raw();
    set_active(theme);
    ActiveThemeTestGuard {
        previous,
        _lock: lock,
    }
}

pub(crate) fn default_theme() -> Theme {
    theme_by_name(DEFAULT_THEME_NAME).expect("default theme must be present in theme registry")
}

/// The graceful-fallback palette for an unknown theme name: the inherit-shell
/// `terminal` theme. Always present in the registry, so resolution never fails
/// just because a config names a theme this build doesn't have.
pub(crate) fn fallback_theme() -> Theme {
    TERMINAL
}

pub(crate) fn fg_primary() -> Color {
    active().body
}

pub(crate) fn selection_bg() -> Color {
    active().selection_bg
}

/// The active theme's canvas colour. `Color::Reset` means "inherit the
/// terminal" — the app-level backdrop skips painting in that case.
pub(crate) fn background() -> Color {
    active().background
}

/// Full-screen backdrop style painting the theme background. Returns
/// `None` when the theme inherits the terminal (`background == Reset`),
/// so the caller can skip the backdrop entirely.
pub(crate) fn backdrop_style() -> Option<Style> {
    let bg = active().background;
    if bg == Color::Reset {
        None
    } else {
        Some(Style::default().bg(bg))
    }
}

pub(crate) fn title_style() -> Style {
    Style::default()
        .fg(active().title)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn heading_style() -> Style {
    Style::default()
        .fg(active().heading)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn body_style() -> Style {
    Style::default().fg(active().body)
}

pub(crate) fn dim_style() -> Style {
    Style::default().fg(active().dim)
}

pub(crate) fn accent_style() -> Style {
    Style::default()
        .fg(active().accent)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn warn_style() -> Style {
    Style::default().fg(active().warn)
}

pub(crate) fn selected_style() -> Style {
    let t = active();
    Style::default()
        .fg(t.title)
        .bg(t.selection_bg)
        .add_modifier(Modifier::BOLD)
}

/// Selection highlight without a foreground override: only the selection
/// background and bold. Use where row spans carry their own meaningful colours
/// (e.g. the theme list's palette swatches) that a full `selected_style` would
/// otherwise patch away.
pub(crate) fn selected_bg_style() -> Style {
    Style::default()
        .bg(active().selection_bg)
        .add_modifier(Modifier::BOLD)
}

/// Retained ("you are here") selection for a pane that does NOT currently hold
/// the cursor. Distinct from `selected_style` (the active cursor): no bold and a
/// dim foreground so the row reads as a remembered position, not the live focus.
pub(crate) fn selected_inactive_style() -> Style {
    let t = active();
    Style::default().fg(t.dim).bg(t.selection_bg)
}

/// Inactive ("you are here") selection without a foreground override: the
/// selection background only, for rows whose spans carry their own meaningful
/// colours (theme swatches) that a dim fg would flatten.
pub(crate) fn selected_inactive_bg_style() -> Style {
    Style::default().bg(active().selection_bg)
}

/// Canonical selection highlight resolver for every split-pane detail list.
///
/// `focused` is true when the cursor lives in the pane being drawn (active
/// selection); false renders the dim "you are here" marker for the pane that
/// has stepped back. `preserve_fg` is true for rows whose own span colours must
/// survive (theme swatches), suppressing the foreground override.
pub(crate) fn selection_highlight(focused: bool, preserve_fg: bool) -> Style {
    match (focused, preserve_fg) {
        (true, false) => selected_style(),
        (true, true) => selected_bg_style(),
        (false, false) => selected_inactive_style(),
        (false, true) => selected_inactive_bg_style(),
    }
}

pub(crate) fn input_style() -> Style {
    Style::default().fg(active().body)
}

/// "You:" label in the chat conversation.
pub(crate) fn user_label_style() -> Style {
    Style::default()
        .fg(active().heading)
        .add_modifier(Modifier::BOLD)
}

/// "Agent:" label in the chat conversation.
pub(crate) fn agent_label_style() -> Style {
    Style::default()
        .fg(active().title)
        .add_modifier(Modifier::BOLD)
}

/// Error messages (error phase, etc.).
pub(crate) fn error_style() -> Style {
    Style::default().fg(active().accent)
}

/// Tool call label `[tool: name]`.
pub(crate) fn tool_label_style() -> Style {
    Style::default()
        .fg(active().tool)
        .add_modifier(Modifier::BOLD)
}

/// Inline code spans in markdown.
pub(crate) fn code_inline_style() -> Style {
    Style::default().fg(active().warn)
}

/// Code block body lines.
pub(crate) fn code_block_style() -> Style {
    Style::default().fg(active().body)
}

/// A syntax-highlight token category. Tree-sitter emits dotted scope strings
/// (`keyword.function`, `string.special`, …); [`SyntaxScope::classify`] folds
/// those into this fixed set so every colour decision keys off a typed variant,
/// and the colours themselves come from the active [`Theme`] rather than a
/// hardcoded palette — so highlighting tracks the theme (and per-agent
/// overrides) live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyntaxScope {
    Keyword,
    StorageType,
    StringLit,
    Constant,
    Type,
    Function,
    Variable,
    Comment,
    Operator,
    Punctuation,
    Attribute,
    DiffPlus,
    DiffMinus,
    Plain,
}

impl SyntaxScope {
    /// Fold a tree-sitter highlight name into a [`SyntaxScope`]. This is the
    /// single place inkjet's string scopes cross into the typed world; every
    /// downstream colour choice matches on the enum, never the raw string.
    pub(crate) fn classify(name: &str) -> Self {
        let head = name.split('.').next().unwrap_or(name);
        let storage = name.starts_with("keyword.storage");
        match head {
            "keyword" if storage => Self::StorageType,
            "keyword" => Self::Keyword,
            "string" | "escape" => Self::StringLit,
            "constant" => Self::Constant,
            "type" | "constructor" => Self::Type,
            "function" => Self::Function,
            "variable" => Self::Variable,
            "comment" => Self::Comment,
            "operator" => Self::Operator,
            "punctuation" => Self::Punctuation,
            "attribute" | "tag" | "label" | "namespace" | "special" | "markup" => Self::Attribute,
            "diff" if name.starts_with("diff.plus") => Self::DiffPlus,
            "diff" if name.starts_with("diff.minus") => Self::DiffMinus,
            _ => Self::Plain,
        }
    }

    /// The active-theme foreground colour for this scope. Maps each token
    /// category onto one of the nine theme roles so the palette follows the
    /// theme registry instead of a second hardcoded colour set.
    pub(crate) fn color(self) -> Color {
        let t = active();
        match self {
            Self::Keyword => t.tool,
            Self::StorageType => t.warn,
            Self::StringLit => t.heading,
            Self::Constant => t.accent,
            Self::Type => t.title,
            Self::Function => t.title,
            Self::Variable => t.body,
            Self::Comment => t.dim,
            Self::Operator => t.body,
            Self::Punctuation => t.dim,
            Self::Attribute => t.warn,
            Self::DiffPlus => t.heading,
            Self::DiffMinus => t.accent,
            Self::Plain => t.body,
        }
    }
}

/// Build the highlight-colour table indexed by inkjet's `Highlight.0`, mapping
/// each `HIGHLIGHT_NAMES` scope through [`SyntaxScope`] onto a themed colour.
/// Rebuilt per call so a live theme swap is reflected on the next render.
pub(crate) fn syntax_colors(names: &[&str]) -> Vec<Color> {
    names
        .iter()
        .map(|n| SyntaxScope::classify(n).color())
        .collect()
}

/// Thought / thinking output.
pub(crate) fn thought_style() -> Style {
    Style::default()
        .fg(active().dim)
        .add_modifier(Modifier::ITALIC)
}

/// Overlay border/title accent (session list, rename, approval).
pub(crate) fn overlay_border_style() -> Style {
    Style::default().fg(active().heading)
}

/// Approval overlay border (warning tone).
pub(crate) fn approval_border_style() -> Style {
    Style::default().fg(active().warn)
}

/// Highlight style for list items (agent picker, session list).
pub(crate) fn list_highlight_style() -> Style {
    Style::default()
        .fg(active().heading)
        .add_modifier(Modifier::BOLD)
}

/// A bordered content panel with a themed border and an optional themed
/// title. The single source of truth for pane chrome so borders never
/// drift back to the terminal default.
pub(crate) fn panel_block(title: &str) -> ratatui::widgets::Block<'static> {
    let mut block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(dim_style());
    if !title.is_empty() {
        block = block.title(ratatui::text::Span::styled(
            title.to_string(),
            title_style(),
        ));
    }
    block
}

/// A modal/overlay panel: themed accent border, bold accent title, and a
/// solid theme-background fill so the modal interior never shows through
/// to the terminal default after a `Clear`.
pub(crate) fn modal_block(title: &str) -> ratatui::widgets::Block<'static> {
    let mut block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(accent_style())
        .style(fill_style());
    if !title.is_empty() {
        block = block.title(ratatui::text::Span::styled(
            title.to_string(),
            accent_style(),
        ));
    }
    block
}

/// Solid panel fill: theme body foreground on the theme background. Used
/// to back modals so their interior matches the active palette instead of
/// the terminal default. Falls back to body-only when the theme inherits
/// the terminal (`background == Reset`).
pub(crate) fn fill_style() -> Style {
    let t = active();
    let s = Style::default().fg(t.body);
    if t.background == Color::Reset {
        s
    } else {
        s.bg(t.background)
    }
}

/// Bottom bar / status bar background: dim foreground on theme background.
/// Used by the mode tab bar, status bar, and info bar so they share a
/// consistent muted look that grounds the chrome without competing with
/// the content area.
pub(crate) fn bar_style() -> Style {
    let t = active();
    let s = Style::default().fg(t.dim);
    if t.background == Color::Reset {
        s
    } else {
        s.bg(t.background)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icy_blue_rgb_unchanged() {
        let t = theme_by_name("icy_blue").expect("icy_blue registered");
        assert_eq!(t.title, Color::Rgb(100, 200, 255));
        assert_eq!(t.heading, Color::Rgb(140, 230, 255));
        assert_eq!(t.body, Color::Rgb(220, 240, 255));
        assert_eq!(t.dim, Color::Rgb(80, 130, 170));
        assert_eq!(t.accent, Color::Rgb(255, 100, 80));
        assert_eq!(t.warn, Color::Rgb(255, 220, 80));
        assert_eq!(t.selection_bg, Color::Rgb(30, 60, 100));
        assert_eq!(t.tool, Color::Rgb(180, 140, 255));
    }

    #[test]
    fn unknown_theme_is_none() {
        assert!(theme_by_name("no-such-theme").is_none());
    }

    #[test]
    fn default_is_registered() {
        assert!(theme_by_name(DEFAULT_THEME_NAME).is_some());
    }

    #[test]
    fn set_active_swaps_palette() {
        // `active()` routes through the colour-depth downgrade; assert on the
        // stored palette via the registry lookup so the test is independent of
        // the terminal depth detected in the test environment.
        let _theme_guard = set_active_for_test(theme_by_name("nord_dark").unwrap());
        assert_eq!(active_raw().title, Color::Rgb(136, 192, 208));
        set_active(theme_by_name("icy_blue").unwrap());
        assert_eq!(active_raw().title, Color::Rgb(100, 200, 255));
    }

    #[test]
    fn registry_themes_are_present() {
        // Parity guard: the generated table mirrors the dashboard registry.
        // A representative spread of registry ids must resolve, proving the
        // build-time generation ran and the kebab→snake mapping applied.
        for name in [
            "default_dark",
            "default_light",
            "dracula",
            "nord_dark",
            "rose_pine_moon",
            "everforest_dark",
            "material_light",
            "hacker_green",
        ] {
            assert!(
                theme_by_name(name).is_some(),
                "registry theme '{name}' missing"
            );
        }
    }

    #[test]
    fn theme_names_are_snake_case() {
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !s.starts_with('_')
                && !s.ends_with('_')
        };
        for name in theme_names() {
            assert!(ok(name), "theme name '{name}' is not snake_case");
        }
        assert!(ok(DEFAULT_THEME_NAME), "default theme name not snake_case");
    }

    #[test]
    fn default_theme_is_icy_blue() {
        assert_eq!(DEFAULT_THEME_NAME, "icy_blue");
        assert_eq!(
            default_theme(),
            theme_by_name(DEFAULT_THEME_NAME).expect("default registered")
        );
    }
}
