//! ZeroClaw TUI colour palette and style helpers.
//!
//! Shared between the onboarding UI (lib target) and the main chat TUI (binary
//! target). Not every helper is used by both targets.
#![allow(dead_code)]

use std::sync::RwLock;

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
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

const ICY_BLUE: Theme = Theme {
    title: Color::Rgb(100, 200, 255),
    heading: Color::Rgb(140, 230, 255),
    body: Color::Rgb(220, 240, 255),
    dim: Color::Rgb(80, 130, 170),
    accent: Color::Rgb(255, 100, 80),
    warn: Color::Rgb(255, 220, 80),
    selection_bg: Color::Rgb(30, 60, 100),
    tool: Color::Rgb(180, 140, 255),
    background: Color::Rgb(8, 14, 24),
};

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

// The named preset palettes (icy_blue plus the dashboard registry themes) are
// generated at build time from `web/src/contexts/themes.json`, the single
// source of truth shared with the React dashboard and mdBook docs. See
// `build.rs` for the var→role mapping. `TERMINAL` and `ICY_BLUE` are authored
// here because they have no registry entry: `terminal` is the inherit-shell
// sentinel, and `icy_blue` is the non-macOS default.
include!(concat!(env!("OUT_DIR"), "/theme_presets.rs"));

pub(crate) const DEFAULT_THEME_NAME: &str = if cfg!(target_os = "macos") {
    "terminal"
} else {
    "icy_blue"
};

const DEFAULT_THEME: Theme = if cfg!(target_os = "macos") {
    TERMINAL
} else {
    ICY_BLUE
};

/// The two authored presets with no dashboard-registry entry: the
/// inherit-shell sentinel and the non-macOS default. All other presets come
/// from `GENERATED_THEMES`.
const AUTHORED_THEMES: &[(&str, Theme)] = &[("terminal", TERMINAL), ("icy_blue", ICY_BLUE)];

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

static ACTIVE: RwLock<Theme> = RwLock::new(DEFAULT_THEME);

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
    ACTIVE.read().map(|g| *g).unwrap_or(DEFAULT_THEME)
}

pub(crate) fn default_theme() -> Theme {
    DEFAULT_THEME
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
        set_active(theme_by_name("nord_dark").unwrap());
        assert_eq!(
            theme_by_name("nord_dark").unwrap().title,
            Color::Rgb(136, 192, 208)
        );
        set_active(theme_by_name("icy_blue").unwrap());
        assert_eq!(
            theme_by_name("icy_blue").unwrap().title,
            Color::Rgb(100, 200, 255)
        );
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
    fn default_theme_is_platform_conditional() {
        let expected = if cfg!(target_os = "macos") {
            "terminal"
        } else {
            "icy_blue"
        };
        assert_eq!(DEFAULT_THEME_NAME, expected);
        assert!(theme_by_name(DEFAULT_THEME_NAME).is_some());
    }
}
