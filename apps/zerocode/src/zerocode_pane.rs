//! The local `zerocode` config pane: theme selector, keybinding list,
//! and preset picker, plus the chord-capture modal for per-action
//! rebinding. All surfaces walk the canonical registries (`theme_names`,
//! `KEY_PRESETS`, each action enum's `variants()`) — nothing is

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::config;
use crate::config::{TodoTrackerSection, WssSection};
use crate::keymap::{Chord, overrides, reserved_reason};
use crate::theme;

/// Which sub-pane of the zerocode tab is focused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Theme,
    AgentTheme,
    Presets,
    Bindings,
    Locale,
    Connection,
    // ── UI heading ─────────────────────────────────────────────────
    TodoTracker,
}

const FOCI: [Focus; 7] = [
    Focus::Theme,
    Focus::AgentTheme,
    Focus::Presets,
    Focus::Bindings,
    Focus::Locale,
    Focus::Connection,
    Focus::TodoTracker,
];

/// Which side of the split holds the live cursor. `Sections` is the left list
/// of section names; `Detail` is the right pane for the highlighted section. The
/// inactive side keeps a dimmed "you are here" highlight so the user never loses
/// their place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PaneCursor {
    Sections,
    Detail,
}

impl Focus {
    fn fluent_key(self) -> &'static str {
        match self {
            Self::Theme => "zc-zerocode-tab-theme",
            Self::AgentTheme => "zc-zerocode-tab-agent-theme",
            Self::Presets => "zc-zerocode-tab-presets",
            Self::Bindings => "zc-zerocode-tab-bindings",
            Self::Locale => "zc-zerocode-tab-locale",
            Self::Connection => "zc-zerocode-tab-connection",
            Self::TodoTracker => "zc-zerocode-tab-todo-tracker",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnField {
    Uri,
    SkipVerify,
    SkipVerifyRoutes,
}

const CONN_FIELDS: [ConnField; 3] = [
    ConnField::Uri,
    ConnField::SkipVerify,
    ConnField::SkipVerifyRoutes,
];

impl ConnField {
    fn fluent_key(self) -> &'static str {
        match self {
            Self::Uri => "zc-zerocode-conn-uri",
            Self::SkipVerify => "zc-zerocode-conn-skip-verify",
            Self::SkipVerifyRoutes => "zc-zerocode-conn-skip-verify-routes",
        }
    }

    fn leaf_path(self) -> &'static str {
        match self {
            Self::Uri => "uri",
            Self::SkipVerify => "tls.skip_verify",
            Self::SkipVerifyRoutes => "tls.skip_verify_routes",
        }
    }
}

// ── Todo tracker fields (Task 7) ────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackerField {
    Enabled,
    EnabledAtStart,
    Location,
    Width,
    MaxHeight,
}

const TRACKER_FIELDS: [TrackerField; 5] = [
    TrackerField::Enabled,
    TrackerField::EnabledAtStart,
    TrackerField::Location,
    TrackerField::Width,
    TrackerField::MaxHeight,
];

impl TrackerField {
    fn fluent_key(self) -> &'static str {
        match self {
            Self::Enabled => "zc-zerocode-tracker-enabled",
            Self::EnabledAtStart => "zc-zerocode-tracker-enabled-at-start",
            Self::Location => "zc-zerocode-tracker-location",
            Self::Width => "zc-zerocode-tracker-width",
            Self::MaxHeight => "zc-zerocode-tracker-max-height",
        }
    }
}

/// One rebindable action row, materialised from the registries so the
/// surface never hardcodes a variant list.
#[derive(Clone)]
struct BindingRow {
    action_key: String,
    label: String,
    chords: Vec<Chord>,
}

/// Capture-modal state: armed for a given row, holding any rejection
/// reason to show inline.
struct Capture {
    row: usize,
    error: Option<String>,
}

pub(crate) struct ZerocodePane {
    config_dir: PathBuf,
    focus: Focus,
    /// Which split side holds the live cursor. Section navigation is on the left
    /// (Sections); entering a section moves the cursor to the right (Detail).
    cursor: PaneCursor,
    // Theme
    themes: Vec<String>,
    theme_cursor: usize,
    /// Separate cursor for the assign-to-agent flow so picking a theme for an
    /// agent never moves the global Theme tab's selection.
    assign_cursor: usize,
    /// When `Some(alias)`, the theme list assigns to that agent's override
    /// rather than the global theme. Cleared after the assignment or on cancel.
    theme_target_agent: Option<String>,
    // Agent theme overrides
    /// Configured agent aliases from the daemon (agents/status), fed by
    /// config_manager — the same registry the Code/Chat agent pickers walk.
    agents: Vec<String>,
    agent_cursor: usize,
    /// alias -> override theme name, loaded from the local config.
    agent_overrides: HashMap<String, String>,
    /// Last `agents/status` error, distinguishing a genuine failure from the
    /// transient "loading…" state.
    agents_error: Option<String>,
    /// True once an `agents/status` response has been applied, so an empty
    /// `agents` list reads as "loaded, none enabled" rather than "still
    /// loading" — otherwise a config with no enabled agents would re-request
    /// forever and never show the terminal "no agents" message.
    agents_loaded: bool,
    // Presets
    presets: Vec<String>,
    preset_cursor: usize,
    // Bindings
    rows: Vec<BindingRow>,
    binding_cursor: usize,
    capture: Option<Capture>,
    // Locale: registry from the daemon (locales/list), fed by config_manager.
    locales: Vec<crate::client::LocaleOption>,
    locale_cursor: usize,
    /// Selected locale persisted to zerocode-config.toml (the active one).
    active_locale: Option<String>,
    /// Set when the user requests "Download locale file"; config_manager (which
    /// holds the RpcClient) drains this, performs the async fetch, and writes.
    pending_fetch: Option<String>,
    status: Option<String>,
    /// Last `locales/list` error, if the registry fetch failed. Distinguishes
    /// a genuine failure from the transient "loading…" state so the Locale tab
    /// does not sit on "loading locales…" forever when the daemon errors.
    list_error: Option<String>,
    last_area: Rect,
    focus_area: Rect,
    content_area: Rect,
    double_click: crate::mouse::DoubleClickTracker,
    conn: WssSection,
    conn_cursor: usize,
    conn_edit: Option<ConnEdit>,
    // ── UI heading (Task 7) ────────────────────────────────────────
    tracker: TodoTrackerSection,
    tracker_cursor: usize,
    tracker_edit: Option<TrackerEdit>,
    /// Set when the persisted `[todotracker]` section exists but does not
    /// parse. `load_persisted` is deliberately tolerant and substitutes
    /// defaults, so without this the pane would edit a phantom default section
    /// and write it over the user's unparseable canonical data on the next
    /// action. While set, tracker edits are refused and the error is surfaced,
    /// leaving the file untouched for the user to repair by hand.
    tracker_load_error: Option<String>,
}

/// Truncate `s` to at most `width` terminal cells, marking elision with `…`.
///
/// Status/banner surfaces are a fixed number of rows, so an over-long string
/// must be cut rather than wrapped: wrapping silently steals rows from the
/// widgets below it.
///
/// Measured in display cells via [`crate::display_width`], not scalars: a wide
/// glyph (CJK, or an emoji presentation sequence) occupies two cells while
/// counting as one `char`, so a scalar-based cut would still overflow the row.
/// Truncation advances by grapheme cluster, so a multi-scalar sequence is never
/// split down the middle.
fn truncate_to_width(s: &str, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    if crate::display_width::display_width(s) <= width {
        return s.to_string();
    }
    // Reserve one cell for the ellipsis.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for (_, grapheme, w) in crate::display_width::grapheme_widths(s) {
        if used + w > budget {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('…');
    out
}

/// The innermost cause of an error chain, as a display string.
///
/// Outer `anyhow` context is written for logs, where repeating the section and
/// path is helpful. On a truncated one-line banner it is pure padding that
/// hides the diagnosis, so the UI shows only the root cause.
fn root_cause_of(error: &anyhow::Error) -> String {
    error
        .chain()
        .last()
        .map_or_else(|| format!("{error}"), std::string::ToString::to_string)
}

/// Flatten a multi-line error into a single line for status/banner display.
///
/// `toml` deserialization errors embed newlines (for example
/// `expected u16\nin `width``), which render as a run-together artifact
/// (`u16in `width``) once a single-line surface strips the break. Collapsing
/// every whitespace run to one space keeps the detail readable wherever it is
/// shown.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct ConnEdit {
    field: ConnField,
    buf: String,
}

struct TrackerEdit {
    field: TrackerField,
    buf: String,
}

impl ZerocodePane {
    pub(crate) fn new(config_dir: &Path) -> Self {
        let themes: Vec<String> = theme::theme_names().map(str::to_string).collect();
        let presets: Vec<String> = config::keybindings::preset_names()
            .map(str::to_string)
            .collect();
        let active = theme::active();
        let theme_cursor = themes
            .iter()
            .position(|n| theme::theme_by_name(n).map(|t| t.title) == Some(active.title))
            .unwrap_or(0);
        let agent_overrides: HashMap<String, String> = config::ensure_and_load(config_dir)
            .ok()
            .map(|c| {
                c.agent_override_aliases()
                    .filter_map(|a| {
                        c.agent_override_name(a)
                            .map(|n| (a.to_string(), n.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Strict parse of the persisted tracker section; see the field docs on
        // `tracker_load_error`.
        let tracker_loaded = config::load_persisted_todotracker_strict(config_dir);
        let mut pane = Self {
            config_dir: config_dir.to_path_buf(),
            focus: Focus::Theme,
            cursor: PaneCursor::Sections,
            themes,
            theme_cursor,
            assign_cursor: 0,
            theme_target_agent: None,
            agents: Vec::new(),
            agent_cursor: 0,
            agent_overrides,
            agents_error: None,
            agents_loaded: false,
            presets,
            preset_cursor: 0,
            rows: Vec::new(),
            binding_cursor: 0,
            capture: None,
            locales: Vec::new(),
            locale_cursor: 0,
            active_locale: config::ensure_and_load(config_dir)
                .ok()
                .and_then(|c| c.resolve_locale()),
            pending_fetch: None,
            status: None,
            list_error: None,
            last_area: Rect::default(),
            focus_area: Rect::default(),
            content_area: Rect::default(),
            double_click: crate::mouse::DoubleClickTracker::new(),
            conn: config::ensure_and_load(config_dir)
                .ok()
                .map(|c| c.connection.wss)
                .unwrap_or_default(),
            conn_cursor: 0,
            conn_edit: None,
            // The editable copy is the *persisted* section: env overrides are
            // transient, and saving one field rewrites the whole section, so an
            // env-injected value must never become the on-disk value.
            //
            // Parsed strictly: a malformed section must not silently become an
            // editable default that a later save would write over the user's
            // canonical text. On error the pane keeps defaults for *display*
            // but records the error and refuses edits.
            tracker: tracker_loaded
                .as_ref()
                .ok()
                .and_then(|s| s.clone())
                .unwrap_or_default(),
            tracker_cursor: 0,
            tracker_edit: None,
            // Only the *root* cause is displayed. `{e:#}` would render the
            // whole anyhow chain, whose outer context repeats the section name
            // and the file path that the surrounding message already states —
            // and that padding pushes the actual diagnosis past the end of a
            // truncated one-line banner, which is the only part the user needs.
            tracker_load_error: tracker_loaded
                .err()
                .map(|e| collapse_whitespace(&root_cause_of(&e))),
        };
        pane.rebuild_rows();
        pane
    }

    /// Materialise the binding rows from every rebindable action enum's
    /// resolved bindings — defaults merged with any active override.
    fn rebuild_rows(&mut self) {
        self.rows = collect_binding_rows();
        if self.binding_cursor >= self.rows.len() {
            self.binding_cursor = self.rows.len().saturating_sub(1);
        }
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        self.conn_edit.is_some() || self.tracker_edit.is_some()
    }

    // ── Draw ─────────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut Frame, area: Rect) {
        use ratatui::layout::{Constraint, Direction, Layout};
        self.last_area = area;

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(22), Constraint::Min(0)])
            .split(area);

        self.focus_area = cols[0];
        self.content_area = cols[1];
        self.draw_focus_list(frame, cols[0]);

        match self.focus {
            Focus::Theme => self.draw_theme(frame, cols[1]),
            // While assigning, Agent Themes borrows the theme list as its detail
            // surface; the agent picker returns once the assignment ends.
            Focus::AgentTheme if self.assigning_theme() => self.draw_theme(frame, cols[1]),
            Focus::AgentTheme => self.draw_agent_theme(frame, cols[1]),
            Focus::Presets => self.draw_presets(frame, cols[1]),
            Focus::Bindings => self.draw_bindings(frame, cols[1]),
            Focus::Locale => self.draw_locale(frame, cols[1]),
            Focus::Connection => self.draw_connection(frame, cols[1]),
            Focus::TodoTracker => self.draw_todo_tracker(frame, cols[1]),
        }

        if self.capture.is_some() {
            self.draw_capture_modal(frame, area);
        }
    }

    /// Highlight style + symbol for a detail-pane list: active (full) when the
    /// cursor is in the detail, dimmed "you are here" when it has stepped back to
    /// the section list. `preserve_fg` keeps row span colours (theme swatches).
    fn detail_highlight(&self) -> (ratatui::style::Style, &'static str) {
        self.list_highlight(self.cursor == PaneCursor::Detail, false)
    }

    /// Canonical highlight resolver shared by every list in this pane: the
    /// themed selection style plus the gutter arrow. `focused` is whether the
    /// list being drawn currently holds the cursor; `preserve_fg` is set for
    /// rows whose own colours must survive (theme swatches).
    fn list_highlight(
        &self,
        focused: bool,
        preserve_fg: bool,
    ) -> (ratatui::style::Style, &'static str) {
        let symbol = if focused { "\u{203a} " } else { "  " };
        (theme::selection_highlight(focused, preserve_fg), symbol)
    }

    fn draw_focus_list(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = FOCI
            .iter()
            .map(|f| {
                ListItem::new(Line::from(Span::styled(
                    crate::i18n::t(f.fluent_key()),
                    theme::body_style(),
                )))
            })
            .collect();
        let mut state = ListState::default();
        state.select(FOCI.iter().position(|f| *f == self.focus));
        // The section list is the active surface when the cursor lives in it;
        // a dimmed "you are here" highlight when the cursor has stepped into the
        // detail.
        let (style, symbol) = self.list_highlight(self.cursor == PaneCursor::Sections, false);
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" zerocode "))
                .highlight_style(style)
                .highlight_symbol(symbol),
            area,
            &mut state,
        );
    }

    /// The cursor the theme list is currently driving: the agent-assign cursor
    /// while assigning to an agent, the global-theme cursor otherwise. Keeping
    /// them distinct stops an agent pick from moving the global Theme selection.
    fn theme_list_cursor(&self) -> usize {
        if self.theme_target_agent.is_some() {
            self.assign_cursor
        } else {
            self.theme_cursor
        }
    }

    fn theme_list_cursor_mut(&mut self) -> &mut usize {
        if self.theme_target_agent.is_some() {
            &mut self.assign_cursor
        } else {
            &mut self.theme_cursor
        }
    }

    fn draw_theme(&self, frame: &mut Frame, area: Rect) {
        let selected = self
            .theme_list_cursor()
            .min(self.themes.len().saturating_sub(1));
        let items: Vec<ListItem> = self
            .themes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                // Swatches only on the highlighted row; other rows reserve the
                // same width in blanks so the name indent never shifts.
                let mut spans = if i == selected {
                    theme_swatch_spans(n)
                } else {
                    theme_swatch_blank()
                };
                spans.push(Span::styled(n.clone(), theme::body_style()));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(selected));
        }
        // In assign-to-agent mode the same list writes the agent's override; the
        // title makes the target unmistakable.
        let title = match &self.theme_target_agent {
            Some(alias) => format!(" Theme → {alias} "),
            None => " Theme ".to_string(),
        };
        let (hstyle, hsym) = self.list_highlight(self.cursor == PaneCursor::Detail, true);
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&title))
                // A fg-less highlight so the per-swatch colours on the
                // highlighted row survive — a full fg override would patch every
                // span's fg and flatten the palette preview.
                .highlight_style(hstyle)
                .highlight_symbol(hsym),
            area,
            &mut state,
        );
    }

    fn draw_agent_theme(&self, frame: &mut Frame, area: Rect) {
        if let Some(err) = &self.agents_error {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                    err.clone(),
                    theme::warn_style(),
                )))
                .block(theme::panel_block(" Agent Themes ")),
                area,
            );
            return;
        }
        if self.agents.is_empty() {
            // Distinguish "still loading" from "loaded, none enabled": the
            // latter is terminal and must not read as a spinner.
            let (msg_key, style) = if self.agents_loaded {
                ("zc-zerocode-agent-theme-no-agents", theme::dim_style())
            } else {
                ("zc-zerocode-agent-theme-loading", theme::dim_style())
            };
            frame.render_widget(
                ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                    crate::i18n::t(msg_key),
                    style,
                )))
                .block(theme::panel_block(" Agent Themes ")),
                area,
            );
            return;
        }
        let items: Vec<ListItem> = self
            .agents
            .iter()
            .map(|alias| {
                let over = self
                    .agent_overrides
                    .get(alias)
                    .map(String::as_str)
                    .unwrap_or("—");
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{alias:<24}"), theme::body_style()),
                    Span::styled(over.to_string(), theme::accent_style()),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.agent_cursor.min(items.len() - 1)));

        // Reserve a one-line hint footer inside the panel so the key actions
        // are visible without opening the help modal.
        use ratatui::layout::{Constraint, Direction, Layout};
        let block = theme::panel_block(" Agent Themes ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(inner);
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(self.detail_highlight().0)
                .highlight_symbol(self.detail_highlight().1),
            rows[0],
            &mut state,
        );
        frame.render_widget(
            ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                self.agent_theme_hint(),
                theme::dim_style(),
            ))),
            rows[1],
        );
    }

    /// One-line key hint for the Agent Themes section, with key labels derived
    /// from the keymap (assign / clear) rather than hardcoded.
    fn agent_theme_hint(&self) -> String {
        use crate::keymap::{ConfigTabAction as A, RebindableActions};
        let label = |a: A| -> String {
            a.resolved()
                .iter()
                .map(Chord::display)
                .collect::<Vec<_>>()
                .join("/")
        };
        crate::i18n::t_args(
            "zc-zerocode-agent-theme-hint",
            &[
                ("assign", &label(A::Enter)),
                ("clear", &label(A::DeleteRow)),
            ],
        )
    }

    fn draw_presets(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .presets
            .iter()
            .map(|n| ListItem::new(Line::from(Span::styled(n.clone(), theme::body_style()))))
            .collect();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.preset_cursor.min(items.len() - 1)));
        }
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" Keybinding Presets "))
                .highlight_style(self.detail_highlight().0)
                .highlight_symbol(self.detail_highlight().1),
            area,
            &mut state,
        );
    }

    fn draw_bindings(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let chords = if r.chords.is_empty() {
                    "(unbound)".to_string()
                } else {
                    r.chords
                        .iter()
                        .map(Chord::display)
                        .collect::<Vec<_>>()
                        .join("  ")
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<28}", r.action_key), theme::dim_style()),
                    Span::styled(format!("{:<22}", r.label), theme::body_style()),
                    Span::styled(chords, theme::accent_style()),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.binding_cursor.min(items.len() - 1)));
        }
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" Keybindings (Enter to rebind) "))
                .highlight_style(self.detail_highlight().0)
                .highlight_symbol(self.detail_highlight().1),
            area,
            &mut state,
        );
    }

    /// Total selectable rows on the Locale tab: one per registry locale, plus
    /// the download action row.
    fn locale_row_count(&self) -> usize {
        self.locales.len() + 1
    }

    fn locale_download_row(&self) -> usize {
        self.locales.len()
    }

    fn draw_locale(&self, frame: &mut Frame, area: Rect) {
        let active = self.active_locale.as_deref();
        let mut items: Vec<ListItem> = self
            .locales
            .iter()
            .map(|o| {
                let mark = if active == Some(o.code.as_str()) {
                    "● "
                } else {
                    "  "
                };
                ListItem::new(Line::from(vec![
                    Span::styled(mark.to_string(), theme::accent_style()),
                    Span::styled(format!("{:<8}", o.code), theme::dim_style()),
                    Span::styled(o.label.clone(), theme::body_style()),
                ]))
            })
            .collect();

        if self.locales.is_empty() {
            let (msg, style) = if let Some(err) = &self.list_error {
                (
                    crate::i18n::t_args("zc-zerocode-locale-list-failed", &[("err", err)]),
                    theme::error_style(),
                )
            } else {
                (
                    crate::i18n::t("zc-zerocode-locale-loading"),
                    theme::dim_style(),
                )
            };
            items.push(ListItem::new(Line::from(Span::styled(msg, style))));
        }

        // Download action row.
        items.push(ListItem::new(Line::from(Span::styled(
            crate::i18n::t("zc-zerocode-locale-download"),
            theme::accent_style().add_modifier(Modifier::BOLD),
        ))));

        let mut state = ListState::default();
        state.select(Some(self.locale_cursor.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" Locale (Enter to select / download) "))
                .highlight_style(self.detail_highlight().0)
                .highlight_symbol(self.detail_highlight().1),
            area,
            &mut state,
        );
    }

    fn conn_field_value(&self, field: ConnField) -> String {
        match field {
            ConnField::Uri => self
                .conn
                .uri
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| crate::i18n::t("zc-zerocode-conn-unset")),
            ConnField::SkipVerify => if self.conn.tls.skip_verify {
                "true"
            } else {
                "false"
            }
            .to_string(),
            ConnField::SkipVerifyRoutes => {
                if self.conn.tls.skip_verify_routes.is_empty() {
                    crate::i18n::t("zc-zerocode-conn-no-routes")
                } else {
                    self.conn.tls.skip_verify_routes.join(", ")
                }
            }
        }
    }

    fn draw_connection(&self, frame: &mut Frame, area: Rect) {
        if let Some(edit) = &self.conn_edit {
            use ratatui::layout::{Constraint, Direction, Layout};
            let title = format!(" {} ", crate::i18n::t(edit.field.fluent_key()));
            let hint = match edit.field {
                ConnField::SkipVerify => crate::i18n::t("zc-zerocode-conn-edit-bool"),
                ConnField::SkipVerifyRoutes => crate::i18n::t("zc-zerocode-conn-edit-routes"),
                ConnField::Uri => crate::i18n::t("zc-zerocode-conn-edit-text"),
            };
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(area);

            let buf_lines: Vec<&str> = edit.buf.split('\n').collect();
            let lines: Vec<Line> = buf_lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let text = if i + 1 == buf_lines.len() {
                        format!("{l}█")
                    } else {
                        (*l).to_string()
                    };
                    Line::from(Span::styled(text, theme::input_style()))
                })
                .collect();
            frame.render_widget(
                Paragraph::new(lines)
                    .block(theme::panel_block(&title))
                    .wrap(Wrap { trim: false }),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(Span::styled(hint, theme::dim_style())),
                rows[1],
            );
            return;
        }

        let items: Vec<ListItem> = CONN_FIELDS
            .iter()
            .map(|f| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<22}", crate::i18n::t(f.fluent_key())),
                        theme::dim_style(),
                    ),
                    Span::styled(self.conn_field_value(*f), theme::body_style()),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.conn_cursor.min(CONN_FIELDS.len() - 1)));
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&crate::i18n::t(
                    "zc-zerocode-conn-title",
                )))
                .highlight_style(self.detail_highlight().0)
                .highlight_symbol(self.detail_highlight().1),
            area,
            &mut state,
        );
    }

    // ── Todo tracker section (Task 7) ───────────────────────────────

    fn tracker_field_value(&self, field: TrackerField) -> String {
        match field {
            TrackerField::Enabled => if self.tracker.enabled {
                "true"
            } else {
                "false"
            }
            .to_string(),
            TrackerField::EnabledAtStart => if self.tracker.enabled_at_start {
                "true"
            } else {
                "false"
            }
            .to_string(),
            TrackerField::Location => match self.tracker.location {
                config::TodoTrackerLocation::Bottom => "bottom",
                config::TodoTrackerLocation::Left => "left",
                config::TodoTrackerLocation::Right => "right",
            }
            .to_string(),
            TrackerField::Width => self.tracker.width.to_string(),
            TrackerField::MaxHeight => self.tracker.max_height.to_string(),
        }
    }

    fn draw_todo_tracker(&self, frame: &mut Frame, area: Rect) {
        if let Some(edit) = &self.tracker_edit {
            use ratatui::layout::{Constraint, Direction, Layout};
            let title = format!(" {} ", crate::i18n::t(edit.field.fluent_key()));
            let hint = match edit.field {
                TrackerField::Enabled | TrackerField::EnabledAtStart => {
                    crate::i18n::t("zc-zerocode-tracker-edit-bool")
                }
                TrackerField::Location => crate::i18n::t("zc-zerocode-tracker-edit-location"),
                TrackerField::Width | TrackerField::MaxHeight => {
                    crate::i18n::t("zc-zerocode-tracker-edit-number")
                }
            };
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(area);

            let buf_lines: Vec<&str> = edit.buf.split('\n').collect();
            let lines: Vec<Line> = buf_lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let text = if i + 1 == buf_lines.len() {
                        format!("{l}█")
                    } else {
                        (*l).to_string()
                    };
                    Line::from(Span::styled(text, theme::input_style()))
                })
                .collect();
            frame.render_widget(
                Paragraph::new(lines)
                    .block(theme::panel_block(&title))
                    .wrap(Wrap { trim: false }),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(Span::styled(hint, theme::dim_style())),
                rows[1],
            );
            return;
        }

        let items: Vec<ListItem> = TRACKER_FIELDS
            .iter()
            .map(|f| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<22}", crate::i18n::t(f.fluent_key())),
                        theme::dim_style(),
                    ),
                    Span::styled(self.tracker_field_value(*f), theme::body_style()),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.tracker_cursor.min(TRACKER_FIELDS.len() - 1)));

        // A malformed persisted section means the values above are defaults
        // standing in for unreadable data, and edits are refused. Say so
        // up front rather than letting the list imply it is editable.
        if self.tracker_load_error.is_some() {
            use ratatui::layout::{Constraint, Direction, Layout};
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(area);
            // Deliberately one line, hard-truncated: this banner shares the
            // pane's area, so wrapping it would push the section list and the
            // field panel off their rows. The full detail stays available in
            // the config file itself.
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&self.tracker_load_banner(), rows[0].width),
                    theme::warn_style(),
                )),
                rows[0],
            );
            frame.render_stateful_widget(
                List::new(items)
                    .block(theme::panel_block(&crate::i18n::t(
                        "zc-zerocode-tracker-title",
                    )))
                    .highlight_style(self.detail_highlight().0)
                    .highlight_symbol(self.detail_highlight().1),
                rows[1],
                &mut state,
            );
            return;
        }

        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&crate::i18n::t(
                    "zc-zerocode-tracker-title",
                )))
                .highlight_style(self.detail_highlight().0)
                .highlight_symbol(self.detail_highlight().1),
            area,
            &mut state,
        );
    }

    // ── RPC bridge (config_manager holds the RpcClient) ──────────

    /// Feed the locale registry fetched via `locales/list`.
    pub(crate) fn set_locales(&mut self, locales: Vec<crate::client::LocaleOption>) {
        self.locales = locales;
        self.list_error = None;
        if self.locale_cursor >= self.locale_row_count() {
            self.locale_cursor = self.locale_row_count().saturating_sub(1);
        }
    }

    /// Feed the configured agent aliases (daemon `agents/status`), supplied by
    /// config_manager which holds the RpcClient. Mirrors `set_locales`.
    pub(crate) fn set_agents(&mut self, agents: Vec<String>) {
        self.agents = agents;
        self.agents_error = None;
        self.agents_loaded = true;
        if !self.agents.is_empty() && self.agent_cursor >= self.agents.len() {
            self.agent_cursor = self.agents.len() - 1;
        }
    }

    /// True if the AgentTheme tab is focused and the agent list hasn't loaded —
    /// config_manager uses this to know when to call `agents/status`. Once a
    /// response has been applied (even an empty one) or an attempt has failed,
    /// it stops re-requesting so an all-disabled config does not spin forever.
    pub(crate) fn agents_needs_list(&self) -> bool {
        self.focus == Focus::AgentTheme && !self.agents_loaded && self.agents_error.is_none()
    }

    /// Record an `agents/status` failure so the tab shows the error instead of
    /// spinning on "loading…" forever.
    pub(crate) fn report_agents_error(&mut self, msg: &str) {
        self.agents_error = Some(format!("agents unavailable: {msg}"));
    }

    /// True if the Locale tab is focused and the registry hasn't loaded yet —
    /// config_manager uses this to know when to call `locales/list`. Once a
    /// list attempt has failed, stop re-requesting on every keypress; the user
    /// sees the error and can retry explicitly.
    pub(crate) fn locale_needs_list(&self) -> bool {
        self.focus == Focus::Locale && self.locales.is_empty() && self.list_error.is_none()
    }

    /// Drain a pending "download locale file" request (the locale code).
    pub(crate) fn take_pending_fetch(&mut self) -> Option<String> {
        self.pending_fetch.take()
    }

    /// Write fetched catalogue bytes into this config dir's FTL store and report.
    pub(crate) fn apply_fetched(
        &mut self,
        locale: &str,
        catalogs: &[crate::client::FetchedCatalog],
        skipped: &[String],
    ) {
        let dir = self.config_dir.join("data").join("ftl").join(locale);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.status = Some(format!("locale write failed: {e}"));
            return;
        }
        let mut written: Vec<&str> = Vec::new();
        for cat in catalogs {
            if std::fs::write(dir.join(&cat.filename), &cat.content).is_ok() {
                written.push(cat.name.as_str());
            }
        }
        self.status = Some(crate::i18n::t_args(
            "zc-zerocode-locale-downloaded",
            &[
                ("written", &written.join(", ")),
                ("locale", locale),
                ("skipped", &skipped.join(", ")),
            ],
        ));
    }

    /// Surface a failed `locales/fetch` (network/daemon error) to the user
    /// without crashing or orphaning the request.
    pub(crate) fn report_fetch_error(&mut self, locale: &str, err: &str) {
        self.status = Some(crate::i18n::t_args(
            "zc-zerocode-locale-fetch-failed",
            &[("locale", locale), ("err", err)],
        ));
    }

    /// Surface a failed `locales/list` so the Locale tab shows the error
    /// instead of hanging on "loading locales…". Stored separately from the
    /// transient empty state so `draw_locale` can render it.
    pub(crate) fn report_list_error(&mut self, err: &str) {
        self.list_error = Some(err.to_string());
        self.status = Some(crate::i18n::t_args(
            "zc-zerocode-locale-list-failed",
            &[("err", err)],
        ));
    }

    fn select_locale_row(&mut self) {
        let cursor = self.locale_cursor;
        if cursor < self.locales.len() {
            // Persist the chosen registry locale.
            let code = self.locales[cursor].code.clone();
            self.set_active_locale(&code);
        } else if cursor == self.locale_download_row() {
            // Queue a fetch for the active (or selected) locale.
            let target = self
                .active_locale
                .clone()
                .or_else(|| self.locales.first().map(|o| o.code.clone()));
            match target {
                Some(code) => {
                    self.pending_fetch = Some(code.clone());
                    self.status = Some(crate::i18n::t_args(
                        "zc-zerocode-locale-fetching",
                        &[("locale", &code)],
                    ));
                }
                None => self.status = Some(crate::i18n::t("zc-zerocode-locale-pick-first")),
            }
        }
    }

    fn set_active_locale(&mut self, code: &str) {
        match config::persist_locale(&self.config_dir, code) {
            Ok(()) => {
                self.active_locale = Some(code.to_string());
                self.status = Some(crate::i18n::t_args(
                    "zc-zerocode-locale-set",
                    &[("locale", code)],
                ));
            }
            Err(e) => self.status = Some(format!("locale save failed: {e}")),
        }
    }

    fn draw_capture_modal(&self, frame: &mut Frame, area: Rect) {
        use ratatui::layout::{Constraint, Direction, Layout};
        let Some(cap) = &self.capture else { return };
        let row = &self.rows[cap.row];

        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Length(7),
                Constraint::Percentage(40),
            ])
            .split(area);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(60),
                Constraint::Percentage(20),
            ])
            .split(v[1]);
        let modal = h[1];

        let mut lines = vec![
            Line::from(Span::styled(
                format!("Rebind: {}", row.action_key),
                theme::heading_style(),
            )),
            Line::from(Span::styled(
                crate::i18n::t("zc-zerocode-capture-prompt"),
                theme::body_style(),
            )),
        ];
        if let Some(err) = &cap.error {
            lines.push(Line::from(Span::styled(err.clone(), theme::warn_style())));
        }
        lines.push(Line::from(Span::styled(
            crate::i18n::t_args("zc-zerocode-hint-cancel", &[("keys", "Esc")]),
            theme::dim_style(),
        )));

        frame.render_widget(ratatui::widgets::Clear, modal);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::approval_border_style())
                    .title(Span::styled(
                        format!(" {} ", crate::i18n::t("zc-zerocode-capture-modal-title")),
                        theme::title_style(),
                    )),
            ),
            modal,
        );
    }

    // ── Key handling ─────────────────────────────────────────────

    /// Returns `true` when the key was consumed. Left/Back at the section
    /// level is intentionally *not* consumed so the outer config pane can
    /// cross back to the left (zeroclaw) pane instead of dead-ending here.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.status = None;
        if self.capture.is_some() {
            self.handle_capture_key(key);
            return true;
        }
        if self.conn_edit.is_some() {
            self.handle_conn_edit_key(key);
            return true;
        }
        if self.tracker_edit.is_some() {
            self.handle_tracker_edit_key(key);
            return true;
        }
        use crate::keymap::ConfigTabAction;
        match ConfigTabAction::from_chord(&key) {
            // Up/Down move within whichever side holds the cursor: the section
            // list on the left, or the detail rows on the right.
            Some(ConfigTabAction::Up) => match self.cursor {
                PaneCursor::Sections => self.cycle_focus(-1),
                PaneCursor::Detail => self.move_cursor(-1),
            },
            Some(ConfigTabAction::Down) => match self.cursor {
                PaneCursor::Sections => self.cycle_focus(1),
                PaneCursor::Detail => self.move_cursor(1),
            },
            // Right enters the detail pane; at the detail level it is a no-op
            // (deepest level — cross-tab nav stays on the global PaneNav chord).
            Some(ConfigTabAction::TabRight) => self.enter_detail(),
            // Left walks back to the section list; at the section level it does
            // not consume so the outer pane crosses to the left (zeroclaw) pane.
            Some(ConfigTabAction::TabLeft) => {
                if self.cursor == PaneCursor::Sections {
                    return false;
                }
                self.leave_detail();
            }
            // Enter: from Sections steps into the detail; from Detail activates
            // the highlighted row.
            Some(ConfigTabAction::Enter) => match self.cursor {
                PaneCursor::Sections => self.enter_detail(),
                PaneCursor::Detail => self.activate(),
            },
            // Back walks one level toward home: Detail -> Sections; at Sections
            // it does not consume so the outer pane can cross left.
            Some(ConfigTabAction::Back) => {
                if self.cursor == PaneCursor::Sections {
                    return false;
                }
                self.leave_detail();
            }
            Some(ConfigTabAction::DeleteRow)
                if self.cursor == PaneCursor::Detail && self.focus == Focus::Bindings =>
            {
                self.reset_row();
            }
            Some(ConfigTabAction::DeleteRow) if self.focus == Focus::AgentTheme => {
                self.clear_agent_override();
            }
            _ => {}
        }
        true
    }

    fn begin_agent_assign(&mut self) {
        let Some(alias) = self.agents.get(self.agent_cursor).cloned() else {
            self.status = Some(crate::i18n::t("zc-zerocode-agent-theme-no-agents"));
            return;
        };
        if let Some(name) = self.agent_overrides.get(&alias)
            && let Some(pos) = self.themes.iter().position(|t| t == name)
        {
            self.assign_cursor = pos;
        } else {
            self.assign_cursor = 0;
        }
        self.theme_target_agent = Some(alias);
    }

    /// True while assigning a theme to an agent: the detail surface is the
    /// reusable theme list even though focus stays on Agent Themes.
    fn assigning_theme(&self) -> bool {
        self.theme_target_agent.is_some()
    }

    /// Remove the highlighted agent's override (DeleteRow in the AgentTheme
    /// section).
    fn clear_agent_override(&mut self) {
        let Some(alias) = self.agents.get(self.agent_cursor).cloned() else {
            return;
        };
        if !self.agent_overrides.contains_key(&alias) {
            self.status = Some(crate::i18n::t("zc-zerocode-agent-theme-none"));
            return;
        }
        match config::persist_agent_theme_clear(&self.config_dir, &alias) {
            Ok(()) => {
                self.agent_overrides.remove(&alias);
                theme::clear_agent_override(&alias);
                self.status = Some(crate::i18n::t_args(
                    "zc-zerocode-agent-theme-cleared",
                    &[("agent", &alias)],
                ));
            }
            Err(e) => self.status = Some(format!("Clear failed: {e}")),
        }
    }

    /// Move the cursor into the detail pane for the highlighted section.
    fn enter_detail(&mut self) {
        self.cursor = PaneCursor::Detail;
    }

    /// Move the cursor back to the section list. No-op if already there (home).
    /// Walking out of the detail pane also ends any pending agent-theme
    /// assignment so the borrowed theme list does not outlive the detail focus.
    fn leave_detail(&mut self) {
        if self.cursor == PaneCursor::Detail {
            self.theme_target_agent = None;
        }
        self.cursor = PaneCursor::Sections;
    }

    fn cycle_focus(&mut self, delta: isize) {
        // Moving off Agent Themes drops any pending assignment defensively;
        // assignment normally lives in the detail pane, so this rarely fires.
        if self.focus == Focus::AgentTheme {
            self.theme_target_agent = None;
        }
        let i = FOCI.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let n = FOCI.len() as isize;
        self.focus = FOCI[(((i + delta) % n + n) % n) as usize];
    }

    fn move_cursor(&mut self, delta: isize) {
        // While assigning, Agent Themes drives the borrowed theme list.
        let len = if self.focus == Focus::AgentTheme && self.assigning_theme() {
            self.themes.len()
        } else {
            match self.focus {
                Focus::Theme => self.themes.len(),
                Focus::AgentTheme => self.agents.len(),
                Focus::Presets => self.presets.len(),
                Focus::Bindings => self.rows.len(),
                Focus::Locale => self.locales.len() + 1,
                Focus::Connection => CONN_FIELDS.len(),
                Focus::TodoTracker => TRACKER_FIELDS.len(),
            }
        };
        if len == 0 {
            return;
        }
        let cursor = if self.focus == Focus::AgentTheme && self.assigning_theme() {
            self.theme_list_cursor_mut()
        } else {
            match self.focus {
                Focus::Theme => self.theme_list_cursor_mut(),
                Focus::AgentTheme => &mut self.agent_cursor,
                Focus::Presets => &mut self.preset_cursor,
                Focus::Bindings => &mut self.binding_cursor,
                Focus::Locale => &mut self.locale_cursor,
                Focus::Connection => &mut self.conn_cursor,
                Focus::TodoTracker => &mut self.tracker_cursor,
            }
        };
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1);
        *cursor = next as usize;
    }

    fn activate(&mut self) {
        match self.focus {
            Focus::Theme => self.apply_theme(),
            // Enter on Agent Themes: pick an agent (start assign) or, while the
            // theme list is borrowed, commit the highlighted theme as the
            // agent's override.
            Focus::AgentTheme if self.assigning_theme() => self.apply_theme(),
            Focus::AgentTheme => self.begin_agent_assign(),
            Focus::Presets => self.apply_preset(),
            Focus::Bindings => {
                if !self.rows.is_empty() {
                    self.capture = Some(Capture {
                        row: self.binding_cursor,
                        error: None,
                    });
                }
            }
            Focus::Locale => self.select_locale_row(),
            Focus::Connection => self.activate_connection(),
            Focus::TodoTracker => self.activate_tracker(),
        }
    }

    fn activate_connection(&mut self) {
        let Some(field) = CONN_FIELDS.get(self.conn_cursor).copied() else {
            return;
        };
        if field == ConnField::SkipVerify {
            self.conn.tls.skip_verify = !self.conn.tls.skip_verify;
            self.persist_conn_field(field);
            return;
        }
        let buf = match field {
            ConnField::Uri => self.conn.uri.clone().unwrap_or_default(),
            ConnField::SkipVerifyRoutes => self.conn.tls.skip_verify_routes.join("\n"),
            ConnField::SkipVerify => String::new(),
        };
        self.conn_edit = Some(ConnEdit { field, buf });
    }

    fn persist_conn_field(&mut self, field: ConnField) {
        let value = match field {
            ConnField::Uri => toml::Value::String(self.conn.uri.clone().unwrap_or_default()),
            ConnField::SkipVerify => toml::Value::Boolean(self.conn.tls.skip_verify),
            ConnField::SkipVerifyRoutes => toml::Value::Array(
                self.conn
                    .tls
                    .skip_verify_routes
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        };
        match config::persist_connection_field(&self.config_dir, field.leaf_path(), value) {
            Ok(()) => self.status = Some(crate::i18n::t("zc-zerocode-conn-saved")),
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    fn commit_conn_edit(&mut self) {
        let Some(edit) = self.conn_edit.take() else {
            return;
        };
        match edit.field {
            ConnField::Uri => {
                let trimmed = edit.buf.trim();
                self.conn.uri = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
            ConnField::SkipVerifyRoutes => {
                self.conn.tls.skip_verify_routes = edit
                    .buf
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            ConnField::SkipVerify => {}
        }
        self.persist_conn_field(edit.field);
    }

    fn handle_conn_edit_key(&mut self, key: KeyEvent) {
        use crate::keymap::ConfigEditorAction;
        let is_routes = self
            .conn_edit
            .as_ref()
            .is_some_and(|e| e.field == ConnField::SkipVerifyRoutes);
        match ConfigEditorAction::from_chord(&key) {
            Some(ConfigEditorAction::Cancel) => {
                self.conn_edit = None;
            }
            Some(ConfigEditorAction::Save) => {
                self.commit_conn_edit();
            }
            Some(ConfigEditorAction::Confirm) => {
                if is_routes {
                    if let Some(e) = self.conn_edit.as_mut() {
                        e.buf.push('\n');
                    }
                } else {
                    self.commit_conn_edit();
                }
            }
            Some(ConfigEditorAction::Backspace) => {
                if let Some(e) = self.conn_edit.as_mut() {
                    e.buf.pop();
                }
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && let Some(e) = self.conn_edit.as_mut()
                {
                    e.buf.push(c);
                }
            }
        }
    }

    // ── Todo tracker activate / edit (Task 7) ───────────────────────

    fn activate_tracker(&mut self) {
        let Some(field) = TRACKER_FIELDS.get(self.tracker_cursor).copied() else {
            return;
        };
        // A malformed persisted section makes every field a phantom default;
        // refuse before opening an editor or toggling so the user sees the
        // repair prompt rather than an edit that cannot be saved.
        if self.tracker_load_error.is_some() {
            self.set_tracker_load_error_status();
            return;
        }
        // Booleans toggle on Enter without opening the editor.
        if field == TrackerField::Enabled || field == TrackerField::EnabledAtStart {
            let mut candidate = self.tracker.clone();
            match field {
                TrackerField::Enabled => candidate.enabled = !candidate.enabled,
                TrackerField::EnabledAtStart => {
                    candidate.enabled_at_start = !candidate.enabled_at_start
                }
                _ => {}
            }
            self.persist_tracker_candidate(candidate);
            return;
        }
        // Location cycles on Enter.
        if field == TrackerField::Location {
            let mut candidate = self.tracker.clone();
            candidate.location = match candidate.location {
                config::TodoTrackerLocation::Bottom => config::TodoTrackerLocation::Left,
                config::TodoTrackerLocation::Left => config::TodoTrackerLocation::Right,
                config::TodoTrackerLocation::Right => config::TodoTrackerLocation::Bottom,
            };
            self.persist_tracker_candidate(candidate);
            return;
        }
        // Numbers and text open the editor.
        let buf = match field {
            TrackerField::Width => self.tracker.width.to_string(),
            TrackerField::MaxHeight => self.tracker.max_height.to_string(),
            _ => String::new(),
        };
        self.tracker_edit = Some(TrackerEdit { field, buf });
    }

    /// The one-line banner text for an unreadable tracker section.
    fn tracker_load_banner(&self) -> String {
        crate::i18n::t_args(
            "zc-zerocode-tracker-load-error",
            &[("error", self.tracker_load_error.as_deref().unwrap_or(""))],
        )
    }

    /// Surface the retained malformed-section error, including the parser
    /// detail so the user can see which field is wrong. The persisted text is
    /// left untouched; they repair it by hand (or delete the section to get
    /// defaults back), and the pane picks it up on the next open.
    fn set_tracker_load_error_status(&mut self) {
        // The pane already renders the full explanation as a banner, and the
        // status is echoed on the shared section tab bar, so repeating the
        // whole sentence here would print it twice on one screen. Point at the
        // banner instead.
        self.status = Some(crate::i18n::t("zc-zerocode-tracker-edit-refused"));
    }

    fn set_ui_validation_error(&mut self, error: config::UiSectionValidationError) {
        let key = match error {
            config::UiSectionValidationError::PositiveRequired => {
                "zc-zerocode-config-positive-required"
            }
        };
        self.status = Some(crate::i18n::t(key));
    }

    fn set_ui_save_error(&mut self, error: &anyhow::Error) {
        self.status = Some(crate::i18n::t_args(
            "zc-zerocode-config-save-failed",
            &[("error", &error.to_string())],
        ));
    }

    fn persist_tracker_candidate(&mut self, candidate: TodoTrackerSection) {
        if let Err(error) = candidate.validate() {
            self.set_ui_validation_error(error);
            return;
        }
        self.persist_tracker_candidate_with_intent(
            candidate,
            config::TrackerWriteIntent::PreserveInvalid,
        );
    }

    /// Write a tracker candidate and report the outcome the user will actually
    /// get. Under a field-scoped repair the writer rebases onto the latest
    /// on-disk section, so what lands may differ from `candidate`; the status
    /// is always derived from the reloaded file, never from the proposal.
    fn persist_tracker_candidate_with_intent(
        &mut self,
        candidate: TodoTrackerSection,
        intent: config::TrackerWriteIntent,
    ) {
        // The persisted section is present but unparseable: the in-memory
        // `tracker` is a default stand-in, not the user's data. Writing it
        // would destroy the canonical text they need in order to repair it, so
        // refuse the edit and re-surface the error instead.
        if self.tracker_load_error.is_some() {
            self.set_tracker_load_error_status();
            return;
        }
        // Both callers pre-validate the value they are writing (a whole
        // candidate for an ordinary edit, the typed number for a repair), so a
        // writer refusal here is always about *on-disk* state: a current
        // section that is malformed or invalid. Report it verbatim — its
        // context names the offending section and file, which a generic
        // validation message would hide.
        let written =
            match config::persist_todotracker_with_intent(&self.config_dir, &candidate, intent) {
                Ok(written) => written,
                Err(error) => {
                    self.set_ui_save_error(&error);
                    return;
                }
            };
        // Verify against the persisted file (not the env-overridden view) so
        // the success status reflects what was actually written to disk.
        // The comparison is against `written`, the section the writer really
        // stored — a field-scoped repair rebases onto the latest document, so
        // the proposed `candidate` is not authoritative here.
        match config::load_persisted(&self.config_dir) {
            Ok(loaded) => {
                let persisted_resolved = loaded.resolve_todo_tracker();
                if loaded.todotracker != written || persisted_resolved != written.resolve() {
                    self.tracker = loaded.todotracker;
                    self.status = Some(crate::i18n::t("zc-zerocode-config-save-mismatch"));
                    return;
                }
                self.tracker = written;
                // The write to disk is correct, but new sessions resolve
                // through `ensure_and_load`, which layers `ZEROCODE_todotracker__*`
                // environment overrides on top. Report what the next session
                // will actually see:
                //   - resolves to the saved value  -> plain success
                //   - resolves to a different value -> an override shadows it
                //   - resolution fails              -> the value may not apply
                // so the ordinary "sessions will use this" is never shown when
                // the effective outcome does not match the saved value.
                // A repair that has not finished (the other dimension is still
                // invalid) landed on disk, but must not report the ordinary
                // success message: the section is not yet usable.
                if loaded.todotracker.validate().is_err() {
                    self.status = Some(crate::i18n::t("zc-zerocode-tracker-saved-still-invalid"));
                    return;
                }
                let key = match config::ensure_and_load(&self.config_dir) {
                    // An effective section that a session boundary would
                    // reject (e.g. `ZEROCODE_todotracker__width=0`) must not
                    // be reported as a mere shadowing override — the next
                    // session keeps its current settings instead.
                    Ok(effective) if effective.validate_todo_tracker().is_err() => {
                        "zc-zerocode-tracker-saved-resolve-error"
                    }
                    Ok(effective) if effective.resolve_todo_tracker() != persisted_resolved => {
                        "zc-zerocode-tracker-saved-env-override"
                    }
                    Ok(_) => "zc-zerocode-tracker-saved",
                    Err(_) => "zc-zerocode-tracker-saved-resolve-error",
                };
                self.status = Some(crate::i18n::t(key));
            }
            Err(error) => self.set_ui_save_error(&error),
        }
    }

    fn commit_tracker_edit(&mut self) {
        let Some(edit) = self.tracker_edit.take() else {
            return;
        };
        let parsed = match edit.buf.trim().parse::<u16>() {
            Ok(value) => value,
            Err(_) => {
                self.status = Some(crate::i18n::t("zc-zerocode-config-invalid-number"));
                return;
            }
        };
        // The value the user just typed must itself be valid, whatever the
        // rest of the section looks like. Field-scoped repair intent below
        // tolerates *other* fields still being invalid; it must never let a
        // fresh zero in through the field being edited.
        if parsed == 0 {
            self.set_ui_validation_error(config::UiSectionValidationError::PositiveRequired);
            return;
        }
        let mut candidate = self.tracker.clone();
        let field = match edit.field {
            TrackerField::Width => {
                candidate.width = parsed;
                config::TrackerNumericField::Width
            }
            TrackerField::MaxHeight => {
                candidate.max_height = parsed;
                config::TrackerNumericField::MaxHeight
            }
            _ => return,
        };
        // An explicit numeric edit of a dimension is the repair path for an
        // invalid stored value, so it may replace one — but only *that*
        // dimension. Authority is scoped to the edited field so the pane's
        // snapshot, which may be as old as pane construction, cannot carry a
        // stale value over another field the user never touched. An unrelated
        // toggle (which routes through `persist_tracker_candidate` directly)
        // may not replace an invalid value at all.
        self.persist_tracker_candidate_with_intent(
            candidate,
            config::TrackerWriteIntent::RepairField(field),
        );
    }

    fn handle_tracker_edit_key(&mut self, key: KeyEvent) {
        use crate::keymap::ConfigEditorAction;
        match ConfigEditorAction::from_chord(&key) {
            Some(ConfigEditorAction::Cancel) => {
                self.tracker_edit = None;
            }
            Some(ConfigEditorAction::Save) => {
                self.commit_tracker_edit();
            }
            Some(ConfigEditorAction::Confirm) => {
                self.commit_tracker_edit();
            }
            Some(ConfigEditorAction::Backspace) => {
                if let Some(e) = self.tracker_edit.as_mut() {
                    e.buf.pop();
                }
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && let Some(e) = self.tracker_edit.as_mut()
                {
                    e.buf.push(c);
                }
            }
        }
    }

    fn apply_theme(&mut self) {
        let Some(name) = self.themes.get(self.theme_list_cursor()).cloned() else {
            return;
        };
        // Assign-to-agent mode: write the override and end the assignment so the
        // detail surface reverts to the agent picker, without touching the
        // global theme.
        if let Some(alias) = self.theme_target_agent.take() {
            if theme::theme_by_name(&name).is_none() {
                return;
            }
            match config::persist_agent_theme(&self.config_dir, &alias, &name) {
                Ok(()) => {
                    self.agent_overrides.insert(alias.clone(), name.clone());
                    // Live-apply, exactly like the global theme: update the
                    // process-global override registry so the Code/Chat pane
                    // picks it up on the next frame without an app restart.
                    if let Some(t) = theme::theme_by_name(&name) {
                        theme::set_agent_override(&alias, t);
                    }
                    self.status = Some(crate::i18n::t_args(
                        "zc-zerocode-agent-theme-set",
                        &[("agent", &alias), ("theme", &name)],
                    ));
                }
                Err(e) => self.status = Some(format!("Override save failed: {e}")),
            }
            return;
        }
        let Some(t) = theme::theme_by_name(&name) else {
            return;
        };
        theme::set_active(t);
        match config::persist_theme(&self.config_dir, &name) {
            Ok(()) => self.status = Some(format!("Theme set to {name}")),
            Err(e) => self.status = Some(format!("Theme set (save failed: {e})")),
        }
    }

    fn apply_preset(&mut self) {
        let Some(name) = self.presets.get(self.preset_cursor).cloned() else {
            return;
        };
        let Some(preset) = config::keybindings::preset_by_name(&name) else {
            return;
        };
        match preset.resolve() {
            Ok(table) => {
                overrides::set_active(table.clone());
                match config::persist_keybindings(&self.config_dir, &table) {
                    Ok(()) => self.status = Some(format!("Preset '{name}' applied")),
                    Err(e) => self.status = Some(format!("Applied (save failed: {e})")),
                }
                self.rebuild_rows();
            }
            Err(e) => self.status = Some(format!("Preset invalid: {e}")),
        }
    }

    /// Why installing `chords` for `action_key` would leave the keymap
    /// ambiguous, or `None` when it is safe.
    ///
    /// Both editor writes go through this. A config file loaded whole is
    /// already checked by `build_override_table`, but these two install one row
    /// against a table nobody re-validates, and a chord owned by two explicit
    /// rows is arbitrated nowhere: dispatch silently follows enum declaration
    /// order while Help advertises the chord for both actions. Refusing names
    /// the other action so the operator can rebind it first, which is a worse
    /// outcome than succeeding but a better one than a binding that does
    /// something else.
    fn collision_reason(action_key: &str, chords: &[Chord]) -> Option<String> {
        let (tag, variant) = action_key.split_once('.')?;
        let (chord, other) = overrides::conflicting_row(tag, variant, chords)?;
        Some(format!(
            "'{}' is already bound to {tag}.{other}; rebind that first",
            chord.display()
        ))
    }

    fn reset_row(&mut self) {
        let Some(row) = self.rows.get(self.binding_cursor) else {
            return;
        };
        let action_key = row.action_key.clone();
        // Reset = restore compile-time default for this single action by
        // persisting its default chords, then re-resolving.
        let defaults = default_chords_for(&action_key);
        if let Some(reason) = Self::collision_reason(&action_key, &defaults) {
            self.status = Some(format!("Reset refused: {reason}"));
            return;
        }
        if let Err(e) = config::persist_keybind_row(&self.config_dir, &action_key, defaults.clone())
        {
            self.status = Some(format!("Reset failed: {e}"));
            return;
        }
        if let Some((tag, variant)) = action_key.split_once('.') {
            overrides::set_row(tag, variant, defaults);
        }
        self.rebuild_rows();
        self.status = Some(format!("Reset {action_key}"));
    }

    fn handle_capture_key(&mut self, key: KeyEvent) {
        // Cancel resolves through its own single-binding event so the
        // capture widget never tests a raw keycode. The widget still
        // records any other chord verbatim below.
        if crate::keymap::CaptureAction::from_chord(&key)
            == Some(crate::keymap::CaptureAction::Cancel)
        {
            self.capture = None;
            return;
        }
        let chord = Chord {
            code: key.code, // keyguard: capture widget records the pressed chord verbatim
            modifiers: key.modifiers,
        };
        if let Some(reason) = reserved_reason(&chord) {
            if let Some(cap) = &mut self.capture {
                cap.error = Some(format!("'{}' is {reason}", chord.display()));
            }
            return;
        }
        let Some(cap) = self.capture.take() else {
            return;
        };
        let action_key = self.rows[cap.row].action_key.clone();
        if let Some(reason) = Self::collision_reason(&action_key, std::slice::from_ref(&chord)) {
            self.status = Some(format!("Save refused: {reason}"));
            return;
        }
        if let Err(e) =
            config::persist_keybind_row(&self.config_dir, &action_key, vec![chord.clone()])
        {
            self.status = Some(format!("Save failed: {e}"));
            return;
        }
        if let Some((tag, variant)) = action_key.split_once('.') {
            overrides::set_row(tag, variant, vec![chord.clone()]);
        }
        self.rebuild_rows();
        self.status = Some(format!("{action_key} -> {}", chord.display()));
    }

    pub(crate) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    // ── Contextual help ──────────────────────────────────────────

    pub(crate) fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::ConfigTabAction as A;
        use crate::widgets::{HelpEntry as E, HelpNode};
        // Render the live chords for an action, never a hardcoded glyph, so the
        // help tracks the actual (possibly overridden) keymap.
        let keys = |a: A| -> Vec<String> {
            use crate::keymap::RebindableActions;
            a.resolved().iter().map(Chord::display).collect()
        };

        if self.capture.is_some() {
            return HelpNode::entries(vec![
                E::key("any key", crate::i18n::t("zc-zerocode-capture-assign")),
                E::new(keys(A::Back), crate::i18n::t("zc-zerocode-capture-cancel")),
            ]);
        }

        let mouse = || {
            E::new(
                Vec::<String>::new(),
                format!(
                    "{}: {}",
                    crate::i18n::t("zc-zerocode-help-mouse-label"),
                    crate::i18n::t("zc-zerocode-help-mouse-desc"),
                ),
            )
        };

        // Cursor in the section list: navigate sections and step into one.
        if self.cursor == PaneCursor::Sections {
            return HelpNode::entries(vec![
                E::new(
                    [keys(A::Up), keys(A::Down)].concat(),
                    crate::i18n::t("zc-zerocode-help-choose-section"),
                ),
                E::new(
                    [keys(A::TabRight), keys(A::Enter)].concat(),
                    crate::i18n::t("zc-zerocode-help-open-section"),
                ),
                E::spacer(),
                mouse(),
            ]);
        }

        // Cursor in the detail pane: navigate rows, act, walk back.
        let mut entries = vec![E::new(
            [keys(A::Up), keys(A::Down)].concat(),
            crate::i18n::t("zc-zerocode-help-navigate-rows"),
        )];
        match self.focus {
            Focus::Theme => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-apply-theme"),
                ));
            }
            Focus::AgentTheme if self.assigning_theme() => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-assign-agent-theme"),
                ));
            }
            Focus::AgentTheme => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-pick-agent"),
                ));
                entries.push(E::new(
                    keys(A::DeleteRow),
                    crate::i18n::t("zc-zerocode-help-clear-agent-theme"),
                ));
            }
            Focus::Presets => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-apply-preset"),
                ));
            }
            Focus::Bindings => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-rebind"),
                ));
                entries.push(E::new(
                    keys(A::DeleteRow),
                    crate::i18n::t("zc-zerocode-help-reset-default"),
                ));
            }
            Focus::Locale => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-locale"),
                ));
            }
            Focus::Connection => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-conn"),
                ));
            }
            Focus::TodoTracker => {
                entries.push(E::new(
                    keys(A::Enter),
                    crate::i18n::t("zc-zerocode-help-todo-tracker"),
                ));
            }
        }
        entries.push(E::new(
            [keys(A::TabLeft), keys(A::Back)].concat(),
            crate::i18n::t("zc-zerocode-help-back-to-sections"),
        ));
        entries.push(E::spacer());
        entries.push(mouse());
        HelpNode::entries(entries)
    }

    // ── Mouse ────────────────────────────────────────────────────

    /// Handle a mouse event already known to fall within the pane body.
    pub(crate) fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crate::mouse;
        use crossterm::event::{MouseButton, MouseEventKind};

        // The capture modal swallows mouse input — keyboard only.
        if self.capture.is_some() {
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Focus column click selects the section and parks the cursor on
                // the left list.
                if mouse::in_rect(mouse.column, mouse.row, self.focus_area) {
                    if let Some(idx) =
                        mouse::list_click_index(mouse.row, self.focus_area, 0, FOCI.len())
                    {
                        // A section click ends any pending assignment so focus,
                        // the detail surface, and the cursor stay consistent.
                        self.theme_target_agent = None;
                        self.focus = FOCI[idx.min(FOCI.len() - 1)];
                        self.cursor = PaneCursor::Sections;
                    }
                    return;
                }
                // Content list click moves the cursor into the detail pane and
                // selects (double-click activates).
                if mouse::in_rect(mouse.column, mouse.row, self.content_area) {
                    let len = self.current_len();
                    if let Some(idx) = mouse::list_click_index(mouse.row, self.content_area, 0, len)
                    {
                        self.cursor = PaneCursor::Detail;
                        self.set_current_cursor(idx);
                        if self.double_click.click(mouse.column, mouse.row) {
                            self.activate();
                        }
                    }
                }
            }
            // Scroll over the left section list cycles the focused section.
            MouseEventKind::ScrollDown
                if mouse::in_rect(mouse.column, mouse.row, self.focus_area) =>
            {
                self.cycle_focus(1);
            }
            MouseEventKind::ScrollUp
                if mouse::in_rect(mouse.column, mouse.row, self.focus_area) =>
            {
                self.cycle_focus(-1);
            }
            MouseEventKind::ScrollDown
                if mouse::in_rect(mouse.column, mouse.row, self.content_area) =>
            {
                self.move_cursor(1);
            }
            MouseEventKind::ScrollUp
                if mouse::in_rect(mouse.column, mouse.row, self.content_area) =>
            {
                self.move_cursor(-1);
            }
            _ => {}
        }
    }

    fn current_len(&self) -> usize {
        if self.focus == Focus::AgentTheme && self.assigning_theme() {
            return self.themes.len();
        }
        match self.focus {
            Focus::Theme => self.themes.len(),
            Focus::AgentTheme => self.agents.len(),
            Focus::Presets => self.presets.len(),
            Focus::Bindings => self.rows.len(),
            Focus::Locale => self.locales.len() + 1,
            Focus::Connection => CONN_FIELDS.len(),
            Focus::TodoTracker => TRACKER_FIELDS.len(),
        }
    }

    fn set_current_cursor(&mut self, idx: usize) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let idx = idx.min(len - 1);
        if self.focus == Focus::AgentTheme && self.assigning_theme() {
            *self.theme_list_cursor_mut() = idx;
            return;
        }
        match self.focus {
            Focus::Theme => *self.theme_list_cursor_mut() = idx,
            Focus::AgentTheme => self.agent_cursor = idx,
            Focus::Presets => self.preset_cursor = idx,
            Focus::Bindings => self.binding_cursor = idx,
            Focus::Locale => self.locale_cursor = idx,
            Focus::Connection => self.conn_cursor = idx,
            Focus::TodoTracker => self.tracker_cursor = idx,
        }
    }
}

/// Number of representative roles previewed per theme (canvas, title, heading,
/// body, warn, tool). The swatch strip is this many blocks plus a trailing
/// space; every row reserves that width so names stay aligned.
const SWATCH_ROLE_COUNT: usize = 6;
const SWATCH_STRIP_WIDTH: usize = SWATCH_ROLE_COUNT + 1;

fn theme_swatch_spans(name: &str) -> Vec<Span<'static>> {
    let Some(roles) = theme_swatch_roles(name) else {
        return vec![Span::raw(" ".repeat(SWATCH_STRIP_WIDTH))];
    };
    let mut spans: Vec<Span<'static>> = roles
        .iter()
        .map(|c| {
            // Route through the colour-depth downgrade so swatches stay faithful
            // on 256/16-colour terminals instead of emitting raw truecolor.
            let c = crate::color_depth::downgrade(*c);
            Span::styled("█", ratatui::style::Style::default().fg(c))
        })
        .collect();
    spans.push(Span::raw(" "));
    spans
}

/// A blank placeholder the same width as the swatch strip, so an unhighlighted
/// row keeps the name at the same indent as the highlighted one.
fn theme_swatch_blank() -> Vec<Span<'static>> {
    vec![Span::raw(" ".repeat(SWATCH_STRIP_WIDTH))]
}

/// The representative role colours previewed for a theme, or `None` when the
/// theme has no fixed palette (the `terminal` inherit theme).
fn theme_swatch_roles(name: &str) -> Option<[ratatui::style::Color; SWATCH_ROLE_COUNT]> {
    use ratatui::style::Color;
    let t = theme::theme_by_name(name)?;
    // Representative spread: canvas, title/accent, heading, body, warn, tool.
    let roles = [t.background, t.title, t.heading, t.body, t.warn, t.tool];
    if roles.iter().all(|c| *c == Color::Reset) {
        None
    } else {
        Some(roles)
    }
}

/// Build the binding rows by walking every rebindable action enum's
/// resolved bindings (defaults merged with active overrides). One row
/// per `(tag, variant)`, chords grouped.
fn collect_binding_rows() -> Vec<BindingRow> {
    use crate::keymap::{
        ChatTabAction, ConfigTabAction, DashboardTabAction, DoctorTabAction, FileExplorerAction,
        GlobalAction, InputBarAction, LogsTabAction, QuickstartTabAction,
    };

    let mut rows = Vec::new();
    rows_from::<GlobalAction>(&mut rows);
    rows_from::<ChatTabAction>(&mut rows);
    rows_from::<LogsTabAction>(&mut rows);
    rows_from::<DashboardTabAction>(&mut rows);
    rows_from::<ConfigTabAction>(&mut rows);
    rows_from::<DoctorTabAction>(&mut rows);
    rows_from::<QuickstartTabAction>(&mut rows);
    rows_from::<InputBarAction>(&mut rows);
    rows_from::<FileExplorerAction>(&mut rows);
    rows
}

/// Append a row for every variant of one action enum, resolved through
/// the override layer.
fn rows_from<A: crate::keymap::RebindableActions>(out: &mut Vec<BindingRow>) {
    for v in A::all() {
        out.push(BindingRow {
            action_key: v.key(),
            label: v.human_label().to_string(),
            chords: v.resolved(),
        });
    }
}

/// Resolve the compile-time default chords for a single `"tag.variant"`
/// by walking the enums for a matching action key.
fn default_chords_for(action_key: &str) -> Vec<Chord> {
    use crate::keymap::{
        ChatTabAction, ConfigTabAction, DashboardTabAction, DoctorTabAction, FileExplorerAction,
        GlobalAction, InputBarAction, LogsTabAction, QuickstartTabAction,
    };
    let mut found = None;
    defaults_in::<GlobalAction>(action_key, &mut found);
    defaults_in::<ChatTabAction>(action_key, &mut found);
    defaults_in::<LogsTabAction>(action_key, &mut found);
    defaults_in::<DashboardTabAction>(action_key, &mut found);
    defaults_in::<ConfigTabAction>(action_key, &mut found);
    defaults_in::<DoctorTabAction>(action_key, &mut found);
    defaults_in::<QuickstartTabAction>(action_key, &mut found);
    defaults_in::<InputBarAction>(action_key, &mut found);
    defaults_in::<FileExplorerAction>(action_key, &mut found);
    found.unwrap_or_default()
}

fn defaults_in<A: crate::keymap::RebindableActions>(
    action_key: &str,
    found: &mut Option<Vec<Chord>>,
) {
    if found.is_some() {
        return;
    }
    // Skip enums whose tag can't prefix this action key.
    if !action_key.starts_with(A::tag()) {
        return;
    }
    for v in A::all() {
        if v.key() == action_key {
            *found = Some(v.defaults());
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::InputBarAction;
    use crossterm::event::{KeyCode, KeyEvent};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // Park the section cursor on `target` within the left section list, leaving
    // the cursor in the Sections pane (the split-pane model navigates sections
    // with Up/Down while the cursor is on the left).
    fn focus_section(pane: &mut ZerocodePane, target: Focus) {
        while pane.focus != target {
            pane.handle_key(key(KeyCode::Down));
        }
    }

    /// Park the binding cursor on `action_key`, returning its row index.
    fn focus_binding(pane: &mut ZerocodePane, action_key: &str) -> usize {
        pane.rebuild_rows();
        let idx = pane
            .rows
            .iter()
            .position(|r| r.action_key == action_key)
            .unwrap_or_else(|| panic!("no binding row for {action_key}"));
        pane.binding_cursor = idx;
        idx
    }

    /// Install one explicit override row, the way a config load or an earlier
    /// editor save would have left it.
    fn given_explicit_row(action_key: &str, chords: Vec<Chord>) {
        let (tag, variant) = action_key.split_once('.').expect("dotted action key");
        overrides::set_row(tag, variant, chords);
    }

    /// The bot's round-2 finding, at both call sites. An operator already owning
    /// `alt+backspace` on a *different* input-bar action must not end up with two
    /// explicit owners of it, because nothing arbitrates that pair: dispatch
    /// falls to enum declaration order and Help advertises the chord twice.
    ///
    /// Reset is the path this PR newly reaches, because `alt+backspace` is now
    /// one of the chords `default_chords_for("input_bar.delete_previous_word")`
    /// writes. Capture is reachable without this PR at all.
    #[test]
    fn binding_editor_refuses_a_chord_another_explicit_row_owns() {
        let _g = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let alt_backspace = Chord::with(KeyCode::Backspace, KeyModifiers::ALT);

        for capture in [false, true] {
            crate::keymap::overrides::reset();
            given_explicit_row("input_bar.clear_input", vec![alt_backspace.clone()]);

            let dir = tempfile::tempdir().unwrap();
            let mut pane = ZerocodePane::new(dir.path());
            let row = focus_binding(&mut pane, "input_bar.delete_previous_word");

            if capture {
                pane.capture = Some(Capture { row, error: None });
                pane.handle_capture_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
            } else {
                pane.reset_row();
            }

            let status = pane.status().unwrap_or_default().to_string();
            assert!(
                status.contains("refused") && status.contains("clear_input"),
                "the write must be refused and name the other owner, got: {status:?}"
            );

            // The refusal is only worth anything if it left the keymap alone.
            let ev = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
            assert_eq!(
                InputBarAction::from_chord(&ev),
                Some(InputBarAction::ClearInput),
                "the operator's existing binding must still own the chord"
            );
            assert!(
                !crate::keymap::action_key_labels(InputBarAction::DeletePreviousWord)
                    .contains(&alt_backspace.display()),
                "a refused write must not advertise the chord in Help"
            );
        }
        crate::keymap::overrides::reset();
    }

    /// The editor half of the darwin normalization case. `ctrl+a` and `super+a`
    /// are one chord at dispatch there, so capturing `super+a` for an action
    /// while another explicit row owns `ctrl+a` would install two rows that
    /// only the dispatcher can tell apart.
    #[cfg(target_os = "macos")]
    #[test]
    fn binding_editor_refuses_a_normalized_collision_on_darwin() {
        let _g = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        given_explicit_row("input_bar.clear_input", vec![Chord::ctrl('a')]);

        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        let row = focus_binding(&mut pane, "input_bar.delete_previous_word");
        pane.capture = Some(Capture { row, error: None });
        pane.handle_capture_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));

        let status = pane.status().unwrap_or_default().to_string();
        assert!(
            status.contains("refused") && status.contains("clear_input"),
            "a chord that only differs on the wire must still collide, got: {status:?}"
        );
        assert_eq!(
            InputBarAction::from_chord(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER)),
            Some(InputBarAction::ClearInput),
            "the existing binding must keep the key"
        );
        crate::keymap::overrides::reset();
    }

    /// The inverse, so the guard cannot pass by refusing everything: a chord
    /// nobody else owns still installs.
    #[test]
    fn binding_editor_still_saves_an_unclaimed_chord() {
        let _g = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();

        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        let row = focus_binding(&mut pane, "input_bar.delete_previous_word");
        pane.capture = Some(Capture { row, error: None });
        // alt+d is bound by no default in this repo.
        pane.handle_capture_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));

        let status = pane.status().unwrap_or_default().to_string();
        assert!(
            !status.contains("refused"),
            "an unclaimed chord must save, got: {status:?}"
        );
        let ev = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(
            InputBarAction::from_chord(&ev),
            Some(InputBarAction::DeletePreviousWord)
        );
        crate::keymap::overrides::reset();
    }

    fn edit_tracker_number(pane: &mut ZerocodePane, field: TrackerField, value: &str) {
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|candidate| *candidate == field)
            .expect("tracker field is registered");
        pane.activate_tracker();
        pane.tracker_edit
            .as_mut()
            .expect("numeric tracker field opens an editor")
            .buf = value.to_string();
        pane.handle_tracker_edit_key(key(KeyCode::Enter));
    }

    // Current-head smoke of the Config-pane Todo-tracker save path, driven
    // through the real edit/persist/resolve functions (no interactive TUI is
    // available in CI). Run with:
    //   cargo test -p zerocode --bin zerocode -- --ignored --nocapture smoke_config_pane_save
    // Prints the observable status/disk/effective values for each scenario so
    // the user-facing contract can be eyeballed. Serializes on the env lock
    // because it mutates process env.
    #[test]
    #[ignore = "current-head smoke; run explicitly with --ignored --nocapture"]
    fn smoke_config_pane_save_tracker_flow() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();

        let disk = |d: &std::path::Path| config::load_persisted(d).unwrap().todotracker.width;
        let effective = |d: &std::path::Path| {
            config::ensure_and_load(d)
                .unwrap()
                .resolve_todo_tracker()
                .width
        };

        eprintln!("── SMOKE: Config-pane Todo-tracker save (current head) ──");

        // Scenario A: no override — a saved value is exactly what sessions use.
        {
            let mut pane = ZerocodePane::new(dir.path());
            edit_tracker_number(&mut pane, TrackerField::Width, "48");
            eprintln!(
                "A. no override      -> saved width 48 | disk={} | effective={} | status={:?}",
                disk(dir.path()),
                effective(dir.path()),
                pane.status.as_deref(),
            );
            assert_eq!(disk(dir.path()), 48);
            assert_eq!(effective(dir.path()), 48);
            assert_eq!(
                pane.status.as_deref(),
                Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
            );
        }

        // Scenario B: an active ZEROCODE_todotracker__width override shadows the
        // save — the pane must not claim sessions will use the saved value.
        {
            let _v = crate::test_support::EnvVarGuard::set("ZEROCODE_todotracker__width", "77");
            let mut pane = ZerocodePane::new(dir.path());
            edit_tracker_number(&mut pane, TrackerField::Width, "52");
            eprintln!(
                "B. width override=77 -> saved width 52 | disk={} | effective={} | status={:?}",
                disk(dir.path()),
                effective(dir.path()),
                pane.status.as_deref(),
            );
            assert_eq!(
                disk(dir.path()),
                52,
                "the edit still lands on disk verbatim"
            );
            assert_eq!(effective(dir.path()), 77, "sessions still see the override");
            assert_eq!(
                pane.status.as_deref(),
                Some(crate::i18n::t("zc-zerocode-tracker-saved-env-override").as_str())
            );
        }

        // Scenario C: a malformed edit is rejected, not silently saved.
        {
            let mut pane = ZerocodePane::new(dir.path());
            let before = disk(dir.path());
            edit_tracker_number(&mut pane, TrackerField::Width, "not-a-number");
            eprintln!(
                "C. malformed edit    -> disk unchanged ({}={}) | status={:?}",
                before,
                disk(dir.path()),
                pane.status.as_deref(),
            );
            assert_eq!(disk(dir.path()), before, "malformed edit must not persist");
            assert_eq!(
                pane.status.as_deref(),
                Some(crate::i18n::t("zc-zerocode-config-invalid-number").as_str())
            );
        }

        // Scenario D: an invalid override makes the effective resolution fail —
        // the disk write succeeds but the pane must not promise sessions will
        // use it; it reports the resolve-error status.
        {
            let _v = crate::test_support::EnvVarGuard::set("ZEROCODE_todotracker__nope", "1");
            let mut pane = ZerocodePane::new(dir.path());
            edit_tracker_number(&mut pane, TrackerField::Width, "40");
            eprintln!(
                "D. resolve error     -> saved width 40 | disk={} | ensure_and_load=Err | status={:?}",
                disk(dir.path()),
                pane.status.as_deref(),
            );
            assert_eq!(disk(dir.path()), 40, "the edit still lands on disk");
            assert!(config::ensure_and_load(dir.path()).is_err());
            assert_eq!(
                pane.status.as_deref(),
                Some(crate::i18n::t("zc-zerocode-tracker-saved-resolve-error").as_str())
            );
        }

        eprintln!("── SMOKE OK ──");
    }

    #[test]
    fn tracker_malformed_edit_does_not_persist_or_report_success() {
        // Drives the pane's save path, which resolves the effective view
        // through `ensure_and_load`; `std::env` is process-global, so this
        // must serialize with every other env-reading test.
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let original = TodoTrackerSection {
            width: 40,
            ..TodoTrackerSection::default()
        };
        config::persist_todotracker(dir.path(), &original).unwrap();
        let mut pane = ZerocodePane::new(dir.path());

        edit_tracker_number(&mut pane, TrackerField::Width, "not-a-number");

        // Asserts what is *persisted*, so load without env overrides: this
        // contract is about the file, and reading through `ensure_and_load`
        // would let a concurrent `ZEROCODE_todotracker__*` test change the
        // observed value.
        let reloaded = config::load_persisted(dir.path()).unwrap();
        assert_eq!(reloaded.todotracker, original);
        assert_eq!(reloaded.todotracker.width, 40);
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-config-invalid-number").as_str())
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
        );
    }

    // A zero is parseable but not a usable panel dimension. The pane must
    // reject it at the edit boundary rather than persisting a value the
    // resolver would silently floor to `1`, so a reported save always matches
    // what the next session resolves.
    #[test]
    fn tracker_zero_edit_does_not_persist_or_report_success() {
        // Drives the pane's save path, which resolves the effective view
        // through `ensure_and_load`; `std::env` is process-global, so this
        // must serialize with every other env-reading test.
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let original = TodoTrackerSection {
            width: 40,
            ..TodoTrackerSection::default()
        };
        config::persist_todotracker(dir.path(), &original).unwrap();
        let mut pane = ZerocodePane::new(dir.path());

        edit_tracker_number(&mut pane, TrackerField::Width, "0");

        // Persisted-only: the contract is that nothing was written to disk.
        let reloaded = config::load_persisted(dir.path()).unwrap();
        assert_eq!(reloaded.todotracker, original);
        assert_eq!(reloaded.todotracker.width, 40);
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-config-positive-required").as_str())
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
        );
    }

    // A valid edit must land on disk verbatim and resolve to the same value,
    // so the success status is only shown when the stored value is exactly
    // what the next session will consume.
    #[test]
    fn valid_tracker_edit_persists_exact_session_consumed_value() {
        // Drives the pane's save path, which resolves the effective view
        // through `ensure_and_load`; `std::env` is process-global, so this
        // must serialize with every other env-reading test.
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();
        let mut pane = ZerocodePane::new(dir.path());

        edit_tracker_number(&mut pane, TrackerField::Width, "52");

        // Persisted-only: this asserts the saved value, not an env-resolved
        // one, so it must not read through the process-global environment.
        let reloaded = config::load_persisted(dir.path()).unwrap();
        assert_eq!(reloaded.todotracker.width, 52);
        assert_eq!(reloaded.resolve_todo_tracker().width, 52);
        assert_eq!(pane.tracker.width, 52);
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
        );
    }

    // A malformed persisted `[todotracker]` section must survive an unrelated
    // tracker action. `load_persisted` is deliberately tolerant (one bad block
    // must not blank unrelated config), so the pane used to initialize from a
    // *default* section and then write that default over the user's malformed
    // canonical data on the next toggle — silently destroying the very text
    // they need to repair, on the supported manual-upgrade path. The pane must
    // instead retain the error, refuse the edit, and leave the file byte-identical.
    /// Render the pane and return its rows as separate strings.
    ///
    /// Row-wise, not a flattened blob: concatenating every cell hides exactly
    /// the defects that matter on a shared surface — a banner that wraps and
    /// pushes widgets off their rows, or a message printed twice on one
    /// screen. Both are invisible to a `contains()` check over joined cells.
    fn render_rows(pane: &mut ZerocodePane, w: u16, h: u16) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| pane.draw(f, Rect::new(0, 0, w, h))).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn malformed_pane(dir: &std::path::Path) -> ZerocodePane {
        std::fs::write(
            config::config_path(dir),
            "[todotracker]\nwidth = \"oops\"\n",
        )
        .unwrap();
        let mut pane = ZerocodePane::new(dir);
        pane.focus = Focus::TodoTracker;
        pane
    }

    // The repair warning must be legible *and* stay inside its own row. An
    // interactive smoke found this wrapping across three rows and painting
    // over the section list and the field panel, which a flattened-buffer
    // assertion could not see.
    #[test]
    fn malformed_tracker_warning_occupies_exactly_one_row_at_every_width() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut pane = malformed_pane(dir.path());

        for width in [80u16, 120, 200] {
            let rows = render_rows(&mut pane, width, 12);
            let hits: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.contains("[todotracker] unreadable:"))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                hits,
                vec![0],
                "at {width} cols the warning must occupy exactly row 0, got rows {hits:?}"
            );
            for (i, row) in rows.iter().enumerate() {
                assert!(
                    crate::display_width::display_width(row) <= width as usize,
                    "row {i} overflows {width} cols (display width \
                     {}): {row}",
                    crate::display_width::display_width(row)
                );
            }
            // The widgets below must keep their rows: the section list starts
            // at row 0 and the tracker panel border sits on row 1.
            assert!(
                rows[0].contains("zerocode"),
                "the section list header must still be on row 0 at {width} cols"
            );
            assert!(
                rows[1].contains("Todo tracker"),
                "the tracker panel must start on row 1 at {width} cols, got: {}",
                rows[1]
            );
        }
    }

    // Truncation must never cost the user the diagnosis. An interactive smoke
    // showed the banner cut at "...zerocode-config.to…", because the parser
    // detail sat behind ~190 characters of boilerplate that repeated the
    // section name and the file path. The actionable part must come early
    // enough to survive a narrow terminal.
    #[test]
    fn malformed_tracker_warning_keeps_the_diagnosis_when_truncated() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut pane = malformed_pane(dir.path());

        for width in [80u16, 100, 120] {
            let rows = render_rows(&mut pane, width, 12);
            let banner = &rows[0];
            assert!(
                banner.contains("oops"),
                "at {width} cols the offending value must survive truncation: {banner}"
            );
        }

        // The retained detail is the *root* cause only: outer context that
        // repeats the section and path is what pushed the diagnosis off-screen.
        let detail = pane
            .tracker_load_error
            .as_deref()
            .expect("a malformed section must record its parser detail");
        assert!(
            !detail.contains("is malformed") && !detail.contains(".toml"),
            "the detail must be the root parse error, not the wrapped chain: {detail}"
        );
        assert!(
            detail.contains("expected u16") && detail.contains("width"),
            "the detail must still identify the type error and field: {detail}"
        );
    }

    // Wide glyphs must not overflow the fixed-width banner. A CJK character
    // is one `char` but two terminal cells, so scalar-based truncation
    // silently overruns the row; and a multi-scalar sequence must never be
    // split. Exercised directly on the helper so the assertion is exact.
    #[test]
    fn truncate_to_width_measures_terminal_cells_not_scalars() {
        use crate::display_width::display_width;

        // 10 CJK chars = 20 cells, but only 10 `char`s.
        let wide = "世界世界世界世界世界";
        assert_eq!(wide.chars().count(), 10);
        assert_eq!(display_width(wide), 20);

        for budget in [1u16, 2, 5, 8, 12, 19, 20, 30] {
            let out = truncate_to_width(wide, budget);
            assert!(
                display_width(&out) <= budget as usize,
                "truncating to {budget} cells produced {} cells: {out:?}",
                display_width(&out)
            );
        }

        // An emoji presentation sequence (base + U+FE0F) is two scalars and
        // must not be cut between them.
        let seq = "⚠️⚠️⚠️";
        for budget in [1u16, 2, 3, 4, 5, 6] {
            let out = truncate_to_width(seq, budget);
            assert!(
                display_width(&out) <= budget as usize,
                "sequence truncation to {budget} produced {} cells: {out:?}",
                display_width(&out)
            );
            assert!(
                !out.contains('\u{fe0f}') || out.contains('⚠'),
                "a variation selector must never be orphaned: {out:?}"
            );
        }

        // ASCII is unchanged when it already fits.
        assert_eq!(truncate_to_width("abc", 10), "abc");
        assert_eq!(truncate_to_width("", 10), "");
        assert_eq!(truncate_to_width("abc", 0), "");
    }

    // The same screen must not say the same thing twice. The banner carries
    // the explanation; the tab-bar status is a short pointer to it.
    #[test]
    fn malformed_tracker_warning_is_not_duplicated_on_screen() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut pane = malformed_pane(dir.path());
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Enabled)
            .expect("tracker field is registered");
        pane.activate_tracker();

        let rows = render_rows(&mut pane, 200, 12);
        let count = rows
            .iter()
            .filter(|r| r.contains("[todotracker] unreadable:"))
            .count();
        assert_eq!(
            count, 1,
            "the warning must appear once, found {count} times"
        );

        let status = pane.status.as_deref().expect("a refusal must set a status");
        assert!(
            !status.contains("[todotracker] unreadable:"),
            "the status must point at the banner, not repeat it: {status}"
        );
        assert!(
            status.len() < 80,
            "the status shares a one-line bar, so it must stay short: {status}"
        );
    }

    // The warning must be legible: translated, and free of the run-together
    // artifact produced when a multi-line parser error is flattened.
    #[test]
    fn malformed_tracker_warning_is_translated_and_readable() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut pane = malformed_pane(dir.path());
        let rows = render_rows(&mut pane, 200, 12);
        let banner = &rows[0];

        assert!(
            banner.contains("[todotracker]") && banner.contains("zerocode-config.toml"),
            "the banner must name the section and the file: {banner}"
        );
        assert!(
            !banner.contains("zc-zerocode-tracker-load-error"),
            "the warning must be translated, not a raw Fluent key: {banner}"
        );
        // `toml` embeds a newline before "in `width`"; flattening it without
        // normalizing whitespace produced the unreadable "u16in `width`".
        let detail = pane
            .tracker_load_error
            .as_deref()
            .expect("a malformed section must record its parser detail");
        assert!(
            !detail.contains('\n') && !detail.contains("u16in"),
            "the parser detail must be collapsed to readable single-line text: {detail}"
        );
    }

    // The repair path is deliberately narrow. Carrying repair intent must not
    // become a general bypass: a numeric edit still cannot overwrite a section
    // that is *unparseable*, because the pane never showed the user that text,
    // so they cannot knowingly be replacing it.
    #[test]
    fn repair_intent_still_refuses_an_unparseable_current_section() {
        let dir = tempfile::tempdir().unwrap();
        let malformed = "[todotracker]\nwidth = \"oops\"\n";
        std::fs::write(config::config_path(dir.path()), malformed).unwrap();

        let err = config::persist_todotracker_with_intent(
            dir.path(),
            &TodoTrackerSection::default(),
            config::TrackerWriteIntent::RepairField(config::TrackerNumericField::Width),
        )
        .expect_err("repair intent must not bypass the unparseable-section refusal");
        assert!(
            format!("{err:#}").contains("malformed"),
            "the error must identify the malformed section, got: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(config::config_path(dir.path())).unwrap(),
            malformed,
            "the unparseable file must be left byte-identical even under repair intent"
        );
    }

    // A zero dimension is *syntactically* valid TOML and parses cleanly as a
    // `u16`, so a strict type re-read alone lets it through. It is still
    // explicitly invalid configuration, and the preservation contract makes no
    // distinction between "unparseable" and "parses but invalid": neither may
    // be silently replaced, and neither may report a successful save.
    #[test]
    fn tracker_save_preserves_zero_width_written_after_pane_open() {
        assert_external_zero_section_survives_unrelated_save("width = 0\nmax_height = 5\n");
    }

    #[test]
    fn tracker_save_preserves_zero_max_height_written_after_pane_open() {
        assert_external_zero_section_survives_unrelated_save("width = 32\nmax_height = 0\n");
    }

    /// Open the pane on valid data, let an external writer install
    /// `[todotracker]` with `fields`, then perform an *unrelated* tracker
    /// action and assert the file is byte-identical with no success status.
    fn assert_external_zero_section_survives_unrelated_save(fields: &str) {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        assert!(
            pane.tracker_load_error.is_none(),
            "precondition: the pane must open cleanly on valid data"
        );

        let external = format!("[theme]\nname = \"nord\"\n\n[todotracker]\n{fields}");
        std::fs::write(config::config_path(dir.path()), &external).unwrap();

        // Unrelated action: toggle a boolean, nothing to do with dimensions.
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Enabled)
            .expect("tracker field is registered");
        pane.activate_tracker();

        assert_eq!(
            std::fs::read_to_string(config::config_path(dir.path())).unwrap(),
            external,
            "an unrelated save must not replace an explicitly invalid current section"
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "a refused write must never report a successful save"
        );
    }

    // Repair must work field-by-field even when *both* dimensions are invalid.
    // Validating the whole candidate before the writer means fixing one field
    // still leaves the other zero, so the candidate is rejected and neither
    // field can ever be repaired from the pane. Both edit orders must work.
    #[test]
    fn both_zero_dimensions_are_repairable_in_either_order() {
        for first in [TrackerField::Width, TrackerField::MaxHeight] {
            let _guard = crate::test_support::env_test_lock();
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                config::config_path(dir.path()),
                "[todotracker]\nwidth = 0\nmax_height = 0\n",
            )
            .unwrap();

            let mut pane = ZerocodePane::new(dir.path());

            // Repair the first field. The other is still zero at this point.
            let (a, b) = match first {
                TrackerField::Width => (TrackerField::Width, TrackerField::MaxHeight),
                _ => (TrackerField::MaxHeight, TrackerField::Width),
            };
            edit_tracker_number(&mut pane, a, "44");
            let after_first = config::load_persisted(dir.path()).unwrap().todotracker;
            let first_value = match a {
                TrackerField::Width => after_first.width,
                _ => after_first.max_height,
            };
            assert_eq!(
                first_value,
                44,
                "editing {} first must land even while the other dimension is still zero",
                a.fluent_key()
            );

            // Repair the second field; the section is now fully valid.
            edit_tracker_number(&mut pane, b, "9");
            let repaired = config::load_persisted(dir.path()).unwrap().todotracker;
            let second_value = match b {
                TrackerField::Width => repaired.width,
                _ => repaired.max_height,
            };
            assert_eq!(
                second_value,
                9,
                "editing {} second must land",
                b.fluent_key()
            );
            repaired
                .validate()
                .expect("the repaired section must be valid");
            assert_eq!(
                pane.status.as_deref(),
                Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
                "a completed repair must report success"
            );
        }
    }

    // A half-finished repair lands on disk but is not yet usable, so it must
    // not claim the ordinary success. Telling the user "saved, new sessions
    // will use this" while the section is still invalid is exactly the
    // false-success reporting the preservation contract forbids.
    #[test]
    fn partial_repair_does_not_report_ordinary_success() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nwidth = 0\nmax_height = 0\n",
        )
        .unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        edit_tracker_number(&mut pane, TrackerField::Width, "44");

        // The edit landed...
        assert_eq!(
            config::load_persisted(dir.path())
                .unwrap()
                .todotracker
                .width,
            44
        );
        // ...but max_height is still zero, so this is not a success yet.
        let status = pane.status.as_deref().expect("a save must set a status");
        assert_ne!(
            Some(status),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "a partial repair must not report the ordinary success"
        );
        assert_eq!(
            status,
            crate::i18n::t("zc-zerocode-tracker-saved-still-invalid").as_str(),
            "the user must be told the section is still invalid"
        );
    }

    // Refusing to overwrite an invalid section must not lock the user out of
    // fixing it: an explicit numeric edit of the offending field is the repair
    // path and must still land.
    #[test]
    fn explicit_numeric_edit_repairs_a_zero_width_section() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nwidth = 0\nmax_height = 5\n",
        )
        .unwrap();

        // The pane opens: the section parses, so this is not the type-malformed
        // path; the user edits width directly to a usable value.
        let mut pane = ZerocodePane::new(dir.path());
        edit_tracker_number(&mut pane, TrackerField::Width, "44");

        let repaired = config::load_persisted(dir.path()).unwrap();
        assert_eq!(
            repaired.todotracker.width, 44,
            "an explicit edit of the invalid field must repair it"
        );
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "a successful repair must report success"
        );
    }

    // The pane's constructor snapshot is not enough on its own: it can be held
    // for the whole life of the pane. If the file is valid at open and an
    // external editor later makes `[todotracker]` malformed, the cached
    // "no error" state would let the next unrelated save replace that
    // externally authored text with the pane's stale candidate and report
    // success. The write boundary itself must re-check what is on disk.
    // Repair authority is scoped to the field the user actually typed into.
    // The pane's snapshot can be as old as construction, so a numeric edit
    // must not carry that stale value over a *different* field an external
    // editor made invalid in the meantime. Open on `width = 32, max_height =
    // 5`; externally write `width = 0, max_height = 9`; then edit only
    // `max_height`. The externally authored invalid `width = 0` must survive.
    #[test]
    fn numeric_edit_does_not_overwrite_a_different_externally_invalid_field() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(
            dir.path(),
            &TodoTrackerSection {
                width: 32,
                max_height: 5,
                ..TodoTrackerSection::default()
            },
        )
        .unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        assert!(
            pane.tracker_load_error.is_none(),
            "precondition: the pane must open cleanly on valid data"
        );

        // An external editor invalidates `width` and changes `max_height`.
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nenabled = true\nenabled_at_start = false\nlocation = \"right\"\nwidth = 0\nmax_height = 9\n",
        )
        .unwrap();

        // The user edits only `max_height` in the still-open pane.
        edit_tracker_number(&mut pane, TrackerField::MaxHeight, "10");

        let after = config::load_persisted(dir.path()).unwrap().todotracker;
        assert_eq!(
            after.width, 0,
            "the externally authored invalid width must not be replaced by the stale snapshot"
        );
        assert_eq!(
            after.max_height, 10,
            "the field the user explicitly edited must land"
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "the section is still invalid, so ordinary success must not be reported"
        );
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved-still-invalid").as_str()),
            "the user must be told the section is still invalid"
        );
    }

    // The mirror case: the same stale-snapshot hazard through the other field.
    #[test]
    fn width_edit_does_not_overwrite_an_externally_invalid_max_height() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(
            dir.path(),
            &TodoTrackerSection {
                width: 32,
                max_height: 5,
                ..TodoTrackerSection::default()
            },
        )
        .unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nenabled = true\nenabled_at_start = false\nlocation = \"right\"\nwidth = 44\nmax_height = 0\n",
        )
        .unwrap();

        edit_tracker_number(&mut pane, TrackerField::Width, "48");

        let after = config::load_persisted(dir.path()).unwrap().todotracker;
        assert_eq!(
            after.max_height, 0,
            "the externally authored invalid max_height must survive a width repair"
        );
        assert_eq!(after.width, 48, "the edited field must land");
    }

    // Repair authority is scoped to the *numeric* field only: every other
    // field, including booleans and the location, is rebased from the latest
    // document rather than taken from the caller's snapshot.
    #[test]
    fn numeric_repair_preserves_externally_changed_non_numeric_fields() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nenabled = false\nenabled_at_start = true\nlocation = \"left\"\nwidth = 0\nmax_height = 5\n",
        )
        .unwrap();

        edit_tracker_number(&mut pane, TrackerField::Width, "44");

        let after = config::load_persisted(dir.path()).unwrap().todotracker;
        assert_eq!(after.width, 44, "the repaired field must land");
        assert!(
            !after.enabled && after.enabled_at_start,
            "externally changed booleans must be preserved, got {after:?}"
        );
        assert_eq!(
            after.location,
            config::TodoTrackerLocation::Left,
            "the externally changed location must be preserved"
        );
    }

    // The pane's in-memory snapshot must be refreshed to what the writer
    // actually stored, not to the candidate it proposed. Otherwise the next
    // edit would rebase from an already-stale view a second time.
    #[test]
    fn pane_snapshot_follows_what_was_written_not_what_was_proposed() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(
            dir.path(),
            &TodoTrackerSection {
                width: 32,
                max_height: 5,
                ..TodoTrackerSection::default()
            },
        )
        .unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nenabled = true\nenabled_at_start = false\nlocation = \"right\"\nwidth = 77\nmax_height = 9\n",
        )
        .unwrap();

        edit_tracker_number(&mut pane, TrackerField::MaxHeight, "10");

        assert_eq!(
            pane.tracker.width, 77,
            "the pane must adopt the on-disk width the write rebased onto, not its stale 32"
        );
        assert_eq!(pane.tracker.max_height, 10);
    }

    // Same invariant at the owning boundary, with no pane involved: repair
    // intent replaces only its own field and leaves the rest of the latest
    // section alone, and it reports back what it actually wrote.
    #[test]
    fn repair_intent_returns_the_rebased_section_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nenabled = true\nenabled_at_start = false\nlocation = \"right\"\nwidth = 0\nmax_height = 9\n",
        )
        .unwrap();

        // A stale candidate that disagrees with disk on *both* dimensions.
        let stale = TodoTrackerSection {
            width: 32,
            max_height: 5,
            ..TodoTrackerSection::default()
        };
        let written = config::persist_todotracker_with_intent(
            dir.path(),
            &stale,
            config::TrackerWriteIntent::RepairField(config::TrackerNumericField::MaxHeight),
        )
        .expect("repairing max_height must succeed");

        assert_eq!(
            written.max_height, 5,
            "the repaired field comes from the candidate"
        );
        assert_eq!(
            written.width, 0,
            "every other field comes from the latest document, not the candidate"
        );
        assert_eq!(
            config::load_persisted(dir.path()).unwrap().todotracker,
            written,
            "the returned section must be exactly what landed on disk"
        );
    }

    // A zero in the field being repaired is still rejected: repair authority
    // tolerates other fields being invalid, never a fresh zero in its own.
    #[test]
    fn repair_intent_rejects_a_zero_in_the_field_it_repairs() {
        let dir = tempfile::tempdir().unwrap();
        let original = "[todotracker]\nwidth = 0\nmax_height = 9\n";
        std::fs::write(config::config_path(dir.path()), original).unwrap();

        let err = config::persist_todotracker_with_intent(
            dir.path(),
            &TodoTrackerSection {
                width: 0,
                max_height: 9,
                ..TodoTrackerSection::default()
            },
            config::TrackerWriteIntent::RepairField(config::TrackerNumericField::Width),
        )
        .expect_err("a zero in the repaired field must be rejected");
        assert!(
            err.downcast_ref::<config::UiSectionValidationError>()
                .is_some(),
            "the refusal must be the numeric validation error, got: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(config::config_path(dir.path())).unwrap(),
            original,
            "a rejected repair must leave the file byte-identical"
        );
    }

    #[test]
    fn tracker_save_preserves_section_made_malformed_after_pane_open() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        // Pane opens on a perfectly valid file, so no load error is recorded.
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        assert!(
            pane.tracker_load_error.is_none(),
            "precondition: the pane must open cleanly on valid data"
        );

        // An external editor rewrites the section into something unparseable.
        let malformed = "[theme]\nname = \"nord\"\n\n[todotracker]\nwidth = \"clobbered\"\n";
        std::fs::write(config::config_path(dir.path()), malformed).unwrap();

        // The user now toggles an unrelated field in the still-open pane.
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Enabled)
            .expect("tracker field is registered");
        pane.activate_tracker();

        let after = std::fs::read_to_string(config::config_path(dir.path())).unwrap();
        assert_eq!(
            after, malformed,
            "a save must not replace a section that became malformed after pane open"
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "a refused write must never report a successful save"
        );
        // The refusal must be legible, naming the section and the reason,
        // rather than failing silently or with a bare generic message.
        let status = pane
            .status
            .as_deref()
            .expect("a refused write must set a status");
        assert!(
            status.contains("todotracker"),
            "the status must name the offending section, got: {status}"
        );
    }

    // Same invariant at the owning boundary, independent of any pane state:
    // `persist_todotracker` must refuse to overwrite a malformed current
    // section even when handed a perfectly valid candidate.
    #[test]
    fn persist_todotracker_refuses_to_replace_a_malformed_current_section() {
        let dir = tempfile::tempdir().unwrap();
        let malformed = "[todotracker]\nmax_height = \"nope\"\n";
        std::fs::write(config::config_path(dir.path()), malformed).unwrap();

        let err = config::persist_todotracker(dir.path(), &TodoTrackerSection::default())
            .expect_err("replacing a malformed current section must fail");
        assert!(
            format!("{err:#}").contains("todotracker"),
            "the error must name the offending section, got: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(config::config_path(dir.path())).unwrap(),
            malformed,
            "the malformed file must be left byte-identical"
        );
    }

    #[test]
    fn tracker_toggle_preserves_malformed_persisted_section() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let malformed = "[theme]\nname = \"nord\"\n\n[todotracker]\nwidth = \"oops\"\n";
        std::fs::write(config::config_path(dir.path()), malformed).unwrap();

        let mut pane = ZerocodePane::new(dir.path());

        // An unrelated edit: toggle `enabled`, nothing to do with `width`.
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Enabled)
            .expect("tracker field is registered");
        pane.activate_tracker();

        let after = std::fs::read_to_string(config::config_path(dir.path())).unwrap();
        assert_eq!(
            after, malformed,
            "an unrelated toggle must not overwrite a malformed canonical section"
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "a refused edit must never report a successful save"
        );
    }

    // The same contract for the numeric editor: a malformed section must not be
    // silently replaced by a default-derived one carrying only the new width.
    #[test]
    fn tracker_number_edit_preserves_malformed_persisted_section() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let malformed = "[todotracker]\nmax_height = \"nope\"\n";
        std::fs::write(config::config_path(dir.path()), malformed).unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        // The editor must refuse to open at all: there is no real persisted
        // value to edit, so offering a prefilled default would invite a save
        // that silently replaces the malformed section.
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Width)
            .expect("tracker field is registered");
        pane.activate_tracker();
        assert!(
            pane.tracker_edit.is_none(),
            "a malformed section must not open a numeric editor"
        );

        let after = std::fs::read_to_string(config::config_path(dir.path())).unwrap();
        assert_eq!(
            after, malformed,
            "a numeric edit must not overwrite a malformed canonical section"
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str()),
            "a refused edit must never report a successful save"
        );
    }

    // The refusal must be actionable, not silent: the user gets the repair
    // prompt, and once they fix the file by hand the pane edits normally again.
    #[test]
    fn malformed_tracker_section_surfaces_repair_prompt_then_recovers() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nwidth = \"oops\"\n",
        )
        .unwrap();

        let mut pane = ZerocodePane::new(dir.path());
        pane.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Enabled)
            .expect("tracker field is registered");
        pane.activate_tracker();
        let status = pane.status.as_deref().expect("a refusal must set a status");
        assert!(
            status.contains("[todotracker]"),
            "the user must be told which section needs repair, got: {status}"
        );
        // The parser detail lives in the banner rather than the status: the
        // status shares a one-line bar, so duplicating the full explanation
        // there printed it twice on one screen.
        let detail = pane
            .tracker_load_error
            .as_deref()
            .expect("a malformed section must record its parser detail");
        assert!(
            detail.contains("max_height") || detail.contains("width"),
            "the retained detail must name the bad field, got: {detail}"
        );

        // The user repairs the file by hand and reopens the pane.
        std::fs::write(
            config::config_path(dir.path()),
            "[todotracker]\nwidth = 32\n",
        )
        .unwrap();
        let mut repaired = ZerocodePane::new(dir.path());
        repaired.tracker_cursor = TRACKER_FIELDS
            .iter()
            .position(|c| *c == TrackerField::Enabled)
            .expect("tracker field is registered");
        let before = repaired.tracker.enabled;
        repaired.activate_tracker();
        assert_eq!(
            config::load_persisted(dir.path())
                .unwrap()
                .todotracker
                .enabled,
            !before,
            "a repaired section must be editable again"
        );
    }

    // A save writes to disk correctly, but runtime sessions resolve through
    // `ensure_and_load`, which layers `ZEROCODE_todotracker__*` overrides on
    // top. When such an override shadows the saved field, the pane must not
    // promise that new sessions will use the just-saved value — it reports the
    // env-override status instead so the feedback stays truthful.
    #[test]
    fn tracker_save_reports_env_override_when_active() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();
        let mut pane = ZerocodePane::new(dir.path());

        // An override pins width to 77 for every resolved session.
        let _v = crate::test_support::EnvVarGuard::set("ZEROCODE_todotracker__width", "77");

        // The user saves width 52. It lands on disk verbatim...
        edit_tracker_number(&mut pane, TrackerField::Width, "52");
        assert_eq!(
            config::load_persisted(dir.path())
                .unwrap()
                .todotracker
                .width,
            52
        );
        // ...but the next session still resolves 77 via the override, so the
        // feedback must say so rather than claim sessions will use 52.
        assert_eq!(
            config::ensure_and_load(dir.path())
                .unwrap()
                .resolve_todo_tracker()
                .width,
            77
        );
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved-env-override").as_str())
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
        );
    }

    // Without an active override the ordinary success message stands: the
    // saved value is exactly what the next session resolves.
    #[test]
    fn tracker_save_reports_plain_success_without_override() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();
        let mut pane = ZerocodePane::new(dir.path());

        edit_tracker_number(&mut pane, TrackerField::Width, "52");

        assert_eq!(
            config::ensure_and_load(dir.path())
                .unwrap()
                .resolve_todo_tracker()
                .width,
            52
        );
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
        );
    }

    // When the effective resolution itself fails (here: a bogus, hard-erroring
    // ZEROCODE_todotracker__* override), a disk write still succeeds — but the
    // pane must not claim "New Code sessions will use this", because the next
    // session's resolution errors. It reports the distinct resolve-error status.
    #[test]
    fn tracker_save_reports_resolve_error_when_effective_resolution_fails() {
        let _guard = crate::test_support::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        config::persist_todotracker(dir.path(), &TodoTrackerSection::default()).unwrap();
        let mut pane = ZerocodePane::new(dir.path());

        // An unknown override makes ensure_and_load hard-error.
        let _v = crate::test_support::EnvVarGuard::set("ZEROCODE_todotracker__nope", "1");
        assert!(
            config::ensure_and_load(dir.path()).is_err(),
            "precondition: the bogus override should make effective resolution fail"
        );

        edit_tracker_number(&mut pane, TrackerField::Width, "52");

        // The edit still lands on disk...
        assert_eq!(
            config::load_persisted(dir.path())
                .unwrap()
                .todotracker
                .width,
            52
        );
        // ...but the status reflects the resolution failure, not plain success.
        assert_eq!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved-resolve-error").as_str())
        );
        assert_ne!(
            pane.status.as_deref(),
            Some(crate::i18n::t("zc-zerocode-tracker-saved").as_str())
        );
    }

    #[test]
    fn locale_tab_never_claims_text_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        // Down moves the cursor through the section list (cursor starts left).
        while pane.focus != Focus::Locale {
            pane.handle_key(key(KeyCode::Down));
        }
        // Enter steps into the detail; a second Enter on the (empty) list must
        // not open any text buffer.
        pane.handle_key(key(KeyCode::Enter));
        pane.handle_key(key(KeyCode::Enter));
        assert!(!pane.wants_text_input());
    }

    // Regression: once a `locales/list` attempt fails, the pane must stop
    // requesting on every keypress (else it hammers the daemon and sits on
    // "loading…"); the error is surfaced instead.
    #[test]
    fn list_error_stops_needing_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        while pane.focus != Focus::Locale {
            pane.handle_key(key(KeyCode::Down));
        }
        assert!(pane.locale_needs_list(), "empty list should need a fetch");
        pane.report_list_error("daemon unreachable");
        assert!(
            !pane.locale_needs_list(),
            "a failed list must not keep re-requesting"
        );
    }

    #[test]
    fn wants_text_input_false_when_locale_buffer_closed() {
        let dir = tempfile::tempdir().unwrap();
        let pane = ZerocodePane::new(dir.path());
        assert!(!pane.wants_text_input());
    }

    #[test]
    fn agent_assign_preserves_global_theme_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        pane.set_agents(vec!["coder".to_string()]);

        // Park the global theme selection on a known, non-zero row.
        focus_section(&mut pane, Focus::Theme);
        pane.handle_key(key(KeyCode::Enter)); // into the Theme detail list
        pane.handle_key(key(KeyCode::Down));
        pane.handle_key(key(KeyCode::Down));
        pane.handle_key(key(KeyCode::Down));
        let global = pane.theme_cursor;
        assert!(global > 0, "global cursor should have moved off row 0");
        pane.handle_key(key(KeyCode::Left)); // back to the section list

        // Enter assign mode for the agent and pick a different row.
        focus_section(&mut pane, Focus::AgentTheme);
        pane.handle_key(key(KeyCode::Enter)); // into the Agent Themes detail
        pane.handle_key(key(KeyCode::Enter)); // begin assign (borrow theme list)
        assert_eq!(pane.focus, Focus::AgentTheme);
        assert!(pane.theme_target_agent.is_some());
        // Move the assign cursor; the global cursor must not follow.
        pane.handle_key(key(KeyCode::Down));
        assert_eq!(
            pane.theme_cursor, global,
            "assign navigation moved the global cursor"
        );
        pane.handle_key(key(KeyCode::Enter)); // commit the override

        // Assignment done; global selection intact, override recorded.
        assert_eq!(pane.focus, Focus::AgentTheme);
        assert!(pane.theme_target_agent.is_none());
        assert_eq!(
            pane.theme_cursor, global,
            "applying an agent override changed the global cursor"
        );
        assert!(
            pane.agent_overrides.contains_key("coder"),
            "agent override was not recorded"
        );
    }

    // Regression: focus stays on Agent Themes during assignment so the left
    // rail and mouse keep treating it as the active section.
    #[test]
    fn assign_mode_keeps_agent_themes_focus() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        pane.set_agents(vec!["coder".to_string()]);
        focus_section(&mut pane, Focus::AgentTheme);
        pane.handle_key(key(KeyCode::Enter)); // into detail
        pane.handle_key(key(KeyCode::Enter)); // begin assign
        assert_eq!(pane.focus, Focus::AgentTheme);
        assert!(pane.theme_target_agent.is_some());
    }

    // Regression: leaving the Agent Themes detail ends the pending assignment
    // so the borrowed theme list does not leak into another section.
    #[test]
    fn leaving_agent_themes_ends_assign() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        pane.set_agents(vec!["coder".to_string()]);
        focus_section(&mut pane, Focus::AgentTheme);
        pane.handle_key(key(KeyCode::Enter)); // into detail
        pane.handle_key(key(KeyCode::Enter)); // begin assign
        assert!(pane.theme_target_agent.is_some());
        // Walking back out of the detail pane drops the pending assignment.
        pane.handle_key(key(KeyCode::Left));
        assert!(
            pane.theme_target_agent.is_none(),
            "leaving Agent Themes did not end the assignment"
        );
    }

    #[test]
    fn right_enters_detail_left_returns_to_sections() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        assert_eq!(pane.cursor, PaneCursor::Sections);
        let start = pane.focus;
        assert!(pane.handle_key(key(KeyCode::Right)));
        assert_eq!(pane.cursor, PaneCursor::Detail);
        assert_eq!(pane.focus, start);
        assert!(pane.handle_key(key(KeyCode::Left)));
        assert_eq!(pane.cursor, PaneCursor::Sections);
        // Left at the section list does not consume: the cursor stays home
        // and the unconsumed key lets the outer pane cross left.
        assert!(!pane.handle_key(key(KeyCode::Left)));
        assert_eq!(pane.cursor, PaneCursor::Sections);
        assert_eq!(pane.focus, start);
        // Back (Esc/q) behaves identically at the section level.
        assert!(!pane.handle_key(key(KeyCode::Esc)));
        assert_eq!(pane.cursor, PaneCursor::Sections);
    }

    #[test]
    fn esc_walks_back_to_sections() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        pane.handle_key(key(KeyCode::Right));
        assert_eq!(pane.cursor, PaneCursor::Detail);
        pane.handle_key(key(KeyCode::Esc));
        assert_eq!(pane.cursor, PaneCursor::Sections);
        pane.handle_key(key(KeyCode::Esc));
        assert_eq!(pane.cursor, PaneCursor::Sections);
    }

    #[test]
    fn up_down_navigate_sections_when_cursor_in_sections() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        let first = pane.focus;
        pane.handle_key(key(KeyCode::Down));
        assert_ne!(
            pane.focus, first,
            "Down in Sections moves to the next section"
        );
        assert_eq!(pane.cursor, PaneCursor::Sections);
    }
}
