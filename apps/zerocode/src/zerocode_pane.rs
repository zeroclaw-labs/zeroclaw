//! The local `zerocode` config pane: theme selector, keybinding list,
//! and preset picker, plus the chord-capture modal for per-action
//! rebinding. All surfaces walk the canonical registries (`theme_names`,
//! `KEY_PRESETS`, each action enum's `variants()`) — nothing is
//! hardcoded here.

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
use crate::config::WssSection;
use crate::keymap::{Chord, overrides, reserved_reason};
use crate::theme;

/// Which sub-pane of the zerocode tab is focused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Theme,
    AgentTheme,
    Presets,
    Bindings,
    Locale,
    Connection,
}

const FOCI: [Focus; 6] = [
    Focus::Theme,
    Focus::AgentTheme,
    Focus::Presets,
    Focus::Bindings,
    Focus::Locale,
    Focus::Connection,
];

impl Focus {
    fn fluent_key(self) -> &'static str {
        match self {
            Self::Theme => "zc-zerocode-tab-theme",
            Self::AgentTheme => "zc-zerocode-tab-agent-theme",
            Self::Presets => "zc-zerocode-tab-presets",
            Self::Bindings => "zc-zerocode-tab-bindings",
            Self::Locale => "zc-zerocode-tab-locale",
            Self::Connection => "zc-zerocode-tab-connection",
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
    // Theme
    themes: Vec<String>,
    theme_cursor: usize,
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
}

struct ConnEdit {
    field: ConnField,
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
        let mut pane = Self {
            config_dir: config_dir.to_path_buf(),
            focus: Focus::Theme,
            themes,
            theme_cursor,
            theme_target_agent: None,
            agents: Vec::new(),
            agent_cursor: 0,
            agent_overrides,
            agents_error: None,
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
        self.conn_edit.is_some()
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
            Focus::AgentTheme => self.draw_agent_theme(frame, cols[1]),
            Focus::Presets => self.draw_presets(frame, cols[1]),
            Focus::Bindings => self.draw_bindings(frame, cols[1]),
            Focus::Locale => self.draw_locale(frame, cols[1]),
            Focus::Connection => self.draw_connection(frame, cols[1]),
        }

        if self.capture.is_some() {
            self.draw_capture_modal(frame, area);
        }
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
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" zerocode "))
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
            area,
            &mut state,
        );
    }

    fn draw_theme(&self, frame: &mut Frame, area: Rect) {
        let selected = self.theme_cursor.min(self.themes.len().saturating_sub(1));
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
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&title))
                // A fg-less highlight (bg + bold only) so the per-swatch colours
                // on the highlighted row survive — a full `selected_style` would
                // patch every span's fg and flatten the palette preview.
                .highlight_style(theme::selected_bg_style())
                .highlight_symbol("› "),
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
            frame.render_widget(
                ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                    crate::i18n::t("zc-zerocode-agent-theme-loading"),
                    theme::dim_style(),
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
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
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
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
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
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
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

        // Free-entry fallback row.
        // Status line for the registry load (loading / error). Only shown
        // when there are no locales yet; it is informational, never a
        // selectable row, so there is no "type a locale" affordance that
        // implies users can invent locales the build does not ship.
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
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
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
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
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
        if !self.agents.is_empty() && self.agent_cursor >= self.agents.len() {
            self.agent_cursor = self.agents.len() - 1;
        }
    }

    /// True if the AgentTheme tab is focused and the agent list hasn't loaded —
    /// config_manager uses this to know when to call `agents/status`. Stops
    /// re-requesting once an attempt has failed.
    pub(crate) fn agents_needs_list(&self) -> bool {
        self.focus == Focus::AgentTheme && self.agents.is_empty() && self.agents_error.is_none()
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

    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        self.status = None;
        if self.capture.is_some() {
            self.handle_capture_key(key);
            return;
        }
        if self.conn_edit.is_some() {
            self.handle_conn_edit_key(key);
            return;
        }
        use crate::keymap::ConfigTabAction;
        match ConfigTabAction::from_chord(&key) {
            Some(ConfigTabAction::Up) => self.move_cursor(-1),
            Some(ConfigTabAction::Down) => self.move_cursor(1),
            Some(ConfigTabAction::TabLeft) => self.cycle_focus(-1),
            Some(ConfigTabAction::TabRight) => self.cycle_focus(1),
            Some(ConfigTabAction::Enter) => self.activate(),
            Some(ConfigTabAction::DeleteRow) if self.focus == Focus::Bindings => {
                self.reset_row();
            }
            Some(ConfigTabAction::DeleteRow) if self.focus == Focus::AgentTheme => {
                self.clear_agent_override();
            }
            _ => {}
        }
    }

    /// Begin assigning a theme to the highlighted agent: point the reusable
    /// theme list at that agent's override and move focus there. Preselect the
    /// list cursor on the agent's current override if it has one.
    fn begin_agent_assign(&mut self) {
        let Some(alias) = self.agents.get(self.agent_cursor).cloned() else {
            self.status = Some(crate::i18n::t("zc-zerocode-agent-theme-no-agents"));
            return;
        };
        if let Some(name) = self.agent_overrides.get(&alias)
            && let Some(pos) = self.themes.iter().position(|t| t == name)
        {
            self.theme_cursor = pos;
        }
        self.theme_target_agent = Some(alias);
        self.focus = Focus::Theme;
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

    fn cycle_focus(&mut self, delta: isize) {
        // Leaving the Theme section abandons any pending agent assignment so the
        // list reverts to global-theme mode.
        if self.focus == Focus::Theme {
            self.theme_target_agent = None;
        }
        let i = FOCI.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let n = FOCI.len() as isize;
        self.focus = FOCI[(((i + delta) % n + n) % n) as usize];
    }

    fn move_cursor(&mut self, delta: isize) {
        let (cursor, len) = match self.focus {
            Focus::Theme => (&mut self.theme_cursor, self.themes.len()),
            Focus::AgentTheme => (&mut self.agent_cursor, self.agents.len()),
            Focus::Presets => (&mut self.preset_cursor, self.presets.len()),
            Focus::Bindings => (&mut self.binding_cursor, self.rows.len()),
            Focus::Locale => (&mut self.locale_cursor, self.locales.len() + 1),
            Focus::Connection => (&mut self.conn_cursor, CONN_FIELDS.len()),
        };
        if len == 0 {
            return;
        }
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1);
        *cursor = next as usize;
    }

    fn activate(&mut self) {
        match self.focus {
            Focus::Theme => self.apply_theme(),
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

    fn apply_theme(&mut self) {
        let Some(name) = self.themes.get(self.theme_cursor).cloned() else {
            return;
        };
        // Assign-to-agent mode: write the override and return focus to the
        // AgentTheme section instead of touching the global theme.
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
            self.focus = Focus::AgentTheme;
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

    fn reset_row(&mut self) {
        let Some(row) = self.rows.get(self.binding_cursor) else {
            return;
        };
        let action_key = row.action_key.clone();
        // Reset = restore compile-time default for this single action by
        // persisting its default chords, then re-resolving.
        let defaults = default_chords_for(&action_key);
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
        // Esc with no modifiers cancels the capture itself.
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE {
            self.capture = None;
            return;
        }
        let chord = Chord {
            code: key.code,
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

        // Render the live chords bound to an action, never a hardcoded literal,
        // so the help reflects the actual (possibly overridden) keymap.
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
        let mut entries = vec![
            E::new(
                [keys(A::TabLeft), keys(A::TabRight)].concat(),
                crate::i18n::t("zc-zerocode-help-switch-pane"),
            ),
            E::new(
                [keys(A::Up), keys(A::Down)].concat(),
                crate::i18n::t("zc-zerocode-help-navigate"),
            ),
        ];
        match self.focus {
            Focus::Theme => {
                let label = if self.theme_target_agent.is_some() {
                    "zc-zerocode-help-assign-agent-theme"
                } else {
                    "zc-zerocode-help-apply-theme"
                };
                entries.push(E::new(keys(A::Enter), crate::i18n::t(label)));
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
        }
        entries.push(E::spacer());
        entries.push(E::desc(format!(
            "{}: {}",
            crate::i18n::t("zc-zerocode-help-mouse-label"),
            crate::i18n::t("zc-zerocode-help-mouse-desc"),
        )));
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
                // Focus column click selects the pane.
                if mouse::in_rect(mouse.column, mouse.row, self.focus_area) {
                    if let Some(idx) =
                        mouse::list_click_index(mouse.row, self.focus_area, 0, FOCI.len())
                    {
                        self.focus = FOCI[idx.min(FOCI.len() - 1)];
                    }
                    return;
                }
                // Content list click selects (double-click activates).
                if mouse::in_rect(mouse.column, mouse.row, self.content_area) {
                    let len = self.current_len();
                    if let Some(idx) = mouse::list_click_index(mouse.row, self.content_area, 0, len)
                    {
                        self.set_current_cursor(idx);
                        if self.double_click.click(mouse.column, mouse.row) {
                            self.activate();
                        }
                    }
                }
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
        match self.focus {
            Focus::Theme => self.themes.len(),
            Focus::AgentTheme => self.agents.len(),
            Focus::Presets => self.presets.len(),
            Focus::Bindings => self.rows.len(),
            Focus::Locale => self.locales.len() + 1,
            Focus::Connection => CONN_FIELDS.len(),
        }
    }

    fn set_current_cursor(&mut self, idx: usize) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let idx = idx.min(len - 1);
        match self.focus {
            Focus::Theme => self.theme_cursor = idx,
            Focus::AgentTheme => self.agent_cursor = idx,
            Focus::Presets => self.preset_cursor = idx,
            Focus::Bindings => self.binding_cursor = idx,
            Focus::Locale => self.locale_cursor = idx,
            Focus::Connection => self.conn_cursor = idx,
        }
    }
}

/// Number of representative roles previewed per theme (canvas, title, heading,
/// body, warn, tool). The swatch strip is this many blocks plus a trailing
/// space; every row reserves that width so names stay aligned.
const SWATCH_ROLE_COUNT: usize = 6;
const SWATCH_STRIP_WIDTH: usize = SWATCH_ROLE_COUNT + 1;

/// Inline palette swatches for a theme row: one block per representative role,
/// in the theme's own colours, followed by a trailing space before the name.
/// The `terminal` (inherit) theme has every role as `Color::Reset`, so it gets
/// blank swatches — there is no fixed palette to preview, but the width is kept
/// so its name aligns with the others.
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
        ChatTabAction, ConfigTabAction, DashboardTabAction, FileExplorerAction, GlobalAction,
        InputBarAction, LogsTabAction, QuickstartTabAction,
    };

    let mut rows = Vec::new();
    rows_from::<GlobalAction>(&mut rows);
    rows_from::<ChatTabAction>(&mut rows);
    rows_from::<LogsTabAction>(&mut rows);
    rows_from::<DashboardTabAction>(&mut rows);
    rows_from::<ConfigTabAction>(&mut rows);
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
        ChatTabAction, ConfigTabAction, DashboardTabAction, FileExplorerAction, GlobalAction,
        InputBarAction, LogsTabAction, QuickstartTabAction,
    };
    let mut found = None;
    defaults_in::<GlobalAction>(action_key, &mut found);
    defaults_in::<ChatTabAction>(action_key, &mut found);
    defaults_in::<LogsTabAction>(action_key, &mut found);
    defaults_in::<DashboardTabAction>(action_key, &mut found);
    defaults_in::<ConfigTabAction>(action_key, &mut found);
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
    use crossterm::event::{KeyCode, KeyEvent};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // The Locale tab is a pick-from-list surface with no free-entry, so the
    // pane never claims text input — typing a locale code by hand was removed
    // because it implied users could conjure locales the build does not ship.
    #[test]
    fn locale_tab_never_claims_text_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = ZerocodePane::new(dir.path());
        while pane.focus != Focus::Locale {
            pane.handle_key(key(KeyCode::Right));
        }
        // Pressing Enter on the (empty) list must not open any text buffer.
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
            pane.handle_key(key(KeyCode::Right));
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
}
