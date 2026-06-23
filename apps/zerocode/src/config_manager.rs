use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::wire::{ConfigFieldEntry, ConfigTab, PropKind, SectionShape};
use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags, MouseEvent, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};

use crate::client::{ConfigSectionEntry, ConfigTemplateEntry, RpcClient};
use crate::theme;

pub(crate) type Term = Terminal<CrosstermBackend<Stdout>>;

fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
}

pub(crate) fn init_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
    )?;
    // Keyboard progressive enhancement (Kitty protocol) is optional — it
    // enables key-release/repeat reporting on capable terminals. Legacy
    // Windows consoles (conhost) don't support it and return an error; treat
    // it as best-effort so an unsupported console degrades gracefully instead
    // of aborting startup.
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        );
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

pub(crate) fn restore_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    // Pop the enhancement flags best-effort — if they were never pushed (or the
    // terminal doesn't support them), popping is a harmless no-op we ignore.
    let _ = execute!(term.backend_mut(), PopKeyboardEnhancementFlags);
    execute!(
        term.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    Ok(())
}

// ── Screen stack ─────────────────────────────────────────────────

enum Screen {
    SectionList,
    TypeList {
        section_idx: usize,
    },
    AliasList {
        section_idx: usize,
        /// For TypedFamilyMap: the family path (e.g. "providers.models.anthropic").
        /// For OneTierAliasMap: the section key itself (e.g. "agents").
        map_path: String,
        breadcrumb: Vec<String>,
    },
    AliasCreate {
        section_idx: usize,
        map_path: String,
        breadcrumb: Vec<String>,
    },
    FieldList {
        section_idx: usize,
        prefix: String,
        breadcrumb: Vec<String>,
    },
    FieldEdit {
        section_idx: usize,
        prefix: String,
        breadcrumb: Vec<String>,
        field_idx: usize,
    },
}

enum FilterAction {
    /// Key was consumed by the filter (typed, navigated, dismissed).
    Consumed,
    /// Key was not handled — caller should process it normally.
    Passthrough,
    /// Enter pressed — caller should act on the currently-selected filtered item.
    Accept,
}

enum FilterEditAction {
    Cancel,
    Accept,
    Backspace,
    CursorUp,
    CursorDown,
}

// ── Config section sub-tabs ──────────────────────────────────────

/// Which pane of the zeroclaw split holds keyboard focus. The section list
/// (left) eagerly loads the highlighted section into the right pane for a live
/// preview; focus moves to the detail (right) on the inward chord.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ZeroclawPane {
    Sections,
    Detail,
}

/// Top-level Config sub-tab: the daemon RPC editor (`zeroclaw`) first,
/// the local client config (`zerocode`) second.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigSection {
    Zeroclaw,
    Zerocode,
}

const CONFIG_SECTIONS: [ConfigSection; 2] = [ConfigSection::Zeroclaw, ConfigSection::Zerocode];

impl ConfigSection {
    fn label(self) -> &'static str {
        match self {
            Self::Zeroclaw => "zeroclaw",
            Self::Zerocode => "zerocode",
        }
    }
}

// ── Keymap-derived chord glyphs (footers + help) ─────────────────
//
// Footer hints and the help overlay must show the live, possibly-overridden
// chord for each action — never a hardcoded glyph.

/// First display chord for a `ConfigTabAction`, or its fallback label.
fn tab_key(action: crate::keymap::ConfigTabAction) -> String {
    use crate::keymap::RebindableActions;
    action
        .resolved()
        .first()
        .map(crate::keymap::Chord::display)
        .unwrap_or_default()
}

/// All display chords for a `ConfigTabAction`, joined for help rows.
fn tab_keys(action: crate::keymap::ConfigTabAction) -> Vec<String> {
    use crate::keymap::RebindableActions;
    action
        .resolved()
        .iter()
        .map(crate::keymap::Chord::display)
        .collect()
}

/// First display chord for a `ConfigEditorAction` (Enter/Esc/Ctrl+S in edits).
fn editor_key(action: crate::keymap::ConfigEditorAction) -> String {
    use crate::keymap::RebindableActions;
    action
        .resolved()
        .first()
        .map(crate::keymap::Chord::display)
        .unwrap_or_default()
}

/// Joined up/down display chords for list navigation footers.
fn nav_keys() -> String {
    use crate::keymap::ConfigTabAction as A;
    tab_keys(A::Up)
        .into_iter()
        .chain(tab_keys(A::Down))
        .collect::<Vec<_>>()
        .join("/")
}

/// Up+down display chords as a vec, for help rows that render each chord.
fn nav_keys_split() -> Vec<String> {
    use crate::keymap::ConfigTabAction as A;
    tab_keys(A::Up)
        .into_iter()
        .chain(tab_keys(A::Down))
        .collect()
}

/// Left+right display chords, used by composite-tab switch help rows.
fn switch_tabs_keys() -> Vec<String> {
    use crate::keymap::ConfigTabAction as A;
    tab_keys(A::TabLeft)
        .into_iter()
        .chain(tab_keys(A::TabRight))
        .collect()
}

/// Display form of a config directory. Replaces the user's home prefix with
/// "~" so a long path like "/home/alice/.zeroclaw" reads as
/// "~/.zeroclaw" in the Config header. Falls back to the original path
/// representation when the path is not under the current home directory or
/// when the home directory cannot be resolved.
fn shorten_home(path: &Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let Some(home) = home else {
        return path.display().to_string();
    };
    // Canonicalize the home directory so the comparison is symmetric with the
    // canonicalized config path. This handles symlinked $HOME setups common on
    // macOS, NixOS, and container images.
    let home_path = PathBuf::from(&home)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(home));
    let to_tilde = |p: &Path| -> String {
        match p.strip_prefix(&home_path) {
            Ok(rel) => format!("~/{}", rel.to_string_lossy().trim_end_matches('/')),
            Err(_) => p.display().to_string(),
        }
    };
    if let Ok(canon) = path.canonicalize() {
        return to_tilde(&canon);
    }
    to_tilde(path)
}

// ── App state ────────────────────────────────────────────────────

pub(crate) struct App {
    rpc: Arc<RpcClient>,
    /// Cached display string for the active config directory, computed once at
    /// construction so the tab bar does not re-stat the filesystem on every
    /// render.
    config_dir_display: String,
    section: ConfigSection,
    zerocode: crate::zerocode_pane::ZerocodePane,
    section_tab_area: Option<Rect>,
    screen: Screen,
    zeroclaw_pane: ZeroclawPane,
    /// Section index currently loaded into the right pane, so re-previewing the
    /// same section preserves its cursor instead of resetting to the top.
    loaded_section: Option<usize>,
    /// Per-section top-level cursor, so switching the section-list highlight away
    /// and back restores each section's own previewed position.
    section_top_cursor: std::collections::HashMap<usize, usize>,
    sections: Vec<ConfigSectionEntry>,
    templates: Vec<ConfigTemplateEntry>,
    section_cursor: usize,
    // Type list (TypedFamilyMap families)
    types: Vec<ConfigTemplateEntry>,
    type_alias_counts: Vec<usize>,
    type_cursor: usize,
    // Alias list
    aliases: Vec<String>,
    alias_enabled: Vec<Option<bool>>,
    alias_cursor: usize,
    // Aliases/Costs tab on cost-bearing provider alias lists.
    alias_tab: usize,
    cost_resources: Vec<String>,
    cost_cursor: usize,
    // Field list
    fields: Vec<ConfigFieldEntry>,
    field_cursor: usize,
    // Edit state
    edit_buf: String,
    // Enum/bool select state
    select_cursor: usize,
    select_items: Vec<String>,
    status_msg: Option<String>,
    // Filter state: None = inactive, Some(buf) = active filter
    filter: Option<String>,
    filter_cursor: usize,
    // Tab state for field list
    active_tab: usize,
    tab_names: Vec<ConfigTab>,
    // Personality editor state (composite tab on agents)
    personality_files: Vec<crate::client::PersonalityFileEntry>,
    personality_cursor: usize,
    personality_agent: String,
    personality_content: String,
    personality_loaded: String,
    personality_active_file: Option<String>,
    personality_max_chars: usize,
    // Skills editor state (composite tab on skill-bundles)
    skills_list: Vec<crate::client::SkillListEntry>,
    skills_cursor: usize,
    skills_bundle: String,
    skills_active: Option<String>,
    skills_body: String,
    skills_body_loaded: String,
    skills_frontmatter: crate::client::SkillFrontmatter,
    skills_frontmatter_loaded: crate::client::SkillFrontmatter,
    // Mouse support
    last_main_area: Rect,
    last_section_pane_area: Rect,
    last_section_list_area: Rect,
    last_list_offset: usize,
    /// Draw-time map of section-pane display rows to `sections` indices.
    /// `None` rows are group headers; mouse clicks resolve through this
    /// so headers are dead zones instead of off-by-N selections.
    last_section_rows: Vec<Option<usize>>,
    /// Scroll offset of the section-pane list at last draw. Dedicated to
    /// the sections pane (not the shared `last_list_offset`, which the
    /// right-pane draws overwrite) so a click on a scrolled section list
    /// maps to the right row in any screen.
    last_section_list_offset: usize,
    last_tab_area: Option<Rect>,
    double_click: crate::mouse::DoubleClickTracker,
}

impl App {
    pub(crate) fn new(rpc: Arc<RpcClient>, config_dir: &Path) -> Self {
        Self {
            rpc,
            config_dir_display: shorten_home(config_dir),
            section: ConfigSection::Zeroclaw,
            zerocode: crate::zerocode_pane::ZerocodePane::new(config_dir),
            section_tab_area: None,
            screen: Screen::SectionList,
            zeroclaw_pane: ZeroclawPane::Sections,
            loaded_section: None,
            section_top_cursor: std::collections::HashMap::new(),
            sections: Vec::new(),
            templates: Vec::new(),
            section_cursor: 0,
            types: Vec::new(),
            type_alias_counts: Vec::new(),
            type_cursor: 0,
            aliases: Vec::new(),
            alias_enabled: Vec::new(),
            alias_cursor: 0,
            alias_tab: 0,
            cost_resources: Vec::new(),
            cost_cursor: 0,
            fields: Vec::new(),
            field_cursor: 0,
            edit_buf: String::new(),
            select_cursor: 0,
            select_items: Vec::new(),
            status_msg: None,
            filter: None,
            filter_cursor: 0,
            active_tab: 0,
            tab_names: Vec::new(),
            personality_files: Vec::new(),
            personality_cursor: 0,
            personality_agent: String::new(),
            personality_content: String::new(),
            personality_loaded: String::new(),
            personality_active_file: None,
            personality_max_chars: 20_000,
            skills_list: Vec::new(),
            skills_cursor: 0,
            skills_bundle: String::new(),
            skills_active: None,
            skills_body: String::new(),
            skills_body_loaded: String::new(),
            skills_frontmatter: Default::default(),
            skills_frontmatter_loaded: Default::default(),
            last_main_area: Rect::default(),
            last_section_pane_area: Rect::default(),
            last_section_list_area: Rect::default(),
            last_list_offset: 0,
            last_section_rows: Vec::new(),
            last_section_list_offset: 0,
            last_tab_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
        }
    }

    /// Load initial data from the daemon. Call once before draw/handle_key.
    pub(crate) async fn init(&mut self) -> Result<()> {
        self.sections = self.rpc.config_sections().await?;
        // Group the section list for display: stable sort by group rank
        // keeps the canonical (dependency-correct) order within each
        // group. Daemons that predate group plumbing send "" for every
        // entry — all ranks tie, the sort is a no-op, and the pane
        // renders the flat list exactly as before.
        self.sections.sort_by_key(|s| Self::group_rank(&s.group));
        self.templates = self.rpc.config_templates().await?;
        // Eagerly load the first section so the right pane previews content on
        // first paint, matching the zerocode Config tab.
        if !self.sections.is_empty() {
            self.load_section_content(self.section_cursor).await?;
        }
        Ok(())
    }

    pub(crate) async fn open_agent_config(&mut self, alias: &str) -> Result<()> {
        self.section = ConfigSection::Zeroclaw;
        self.zeroclaw_pane = ZeroclawPane::Detail;
        self.deactivate_filter();

        let Some(section_idx) = self.sections.iter().position(|s| s.key == "agents") else {
            return Ok(());
        };

        self.section_cursor = section_idx;
        self.loaded_section = Some(section_idx);
        self.load_aliases("agents").await?;

        let Some(alias_idx) = self.aliases.iter().position(|a| a == alias) else {
            self.alias_cursor = 0;
            self.screen = Screen::AliasList {
                section_idx,
                map_path: "agents".to_string(),
                breadcrumb: vec!["agents".to_string()],
            };
            self.status_msg = None;
            return Ok(());
        };

        self.alias_cursor = alias_idx;
        let prefix = format!("agents.{alias}");
        self.load_fields(&prefix).await?;
        self.screen = Screen::FieldList {
            section_idx,
            prefix,
            breadcrumb: vec!["agents".to_string(), alias.to_string()],
        };
        self.status_msg = None;
        Ok(())
    }

    /// Draw the current screen into the given area, beneath the Config
    /// section sub-tab bar (`zeroclaw` / `zerocode`).
    pub(crate) fn draw_into(&mut self, frame: &mut Frame, area: Rect) {
        use ratatui::layout::{Constraint, Direction, Layout};
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        self.draw_section_tab_bar(frame, chunks[0]);
        self.section_tab_area = Some(chunks[0]);
        let body = chunks[1];

        if self.section == ConfigSection::Zerocode {
            self.zerocode.draw(frame, body);
            return;
        }

        // Unified bottom-left help indicator, matching the Dashboard/Logs panes.
        frame.render_widget(
            Paragraph::new(Span::styled(" ?=help", theme::dim_style())),
            chunks[2],
        );

        // Split-pane: the section list stays pinned on the left; the highlighted
        // section's content is eagerly loaded and embedded on the right for a
        // live preview. Focus is on the left until the inward chord; the right
        // pane always renders the loaded content.
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(0)])
            .split(body);
        let left = panes[0];
        let right = panes[1];

        let on_sections = self.zeroclaw_pane == ZeroclawPane::Sections;
        self.draw_sections_pane(frame, left, on_sections);
        self.last_section_pane_area = left;

        // Clone values out of `screen` so draw methods can take `&mut self`.
        // The right pane renders the loaded section content whether focus is on
        // the sections list (preview) or the detail (active).
        match &self.screen {
            Screen::SectionList => self.draw_section_detail_hint(frame, right),
            Screen::TypeList { section_idx } => {
                let si = *section_idx;
                self.draw_type_list(frame, right, si);
            }
            Screen::AliasList {
                section_idx,
                breadcrumb,
                ..
            } => {
                let si = *section_idx;
                let bc = breadcrumb.clone();
                self.draw_alias_list(frame, right, si, &bc);
            }
            Screen::AliasCreate { breadcrumb, .. } => {
                let bc = breadcrumb.clone();
                self.draw_alias_create(frame, right, &bc);
            }
            Screen::FieldList {
                section_idx,
                breadcrumb,
                ..
            } => {
                let si = *section_idx;
                let bc = breadcrumb.clone();
                self.draw_field_list(frame, right, si, &bc);
            }
            Screen::FieldEdit {
                breadcrumb,
                field_idx,
                ..
            } => {
                let bc = breadcrumb.clone();
                let fi = *field_idx;
                self.draw_field_edit(frame, right, &bc, fi);
            }
        }
    }

    /// Highlight style + symbol for the right (detail) pane lists: active while
    /// focus is in the detail, dimmed "you are here" while the section list holds
    /// focus and the detail is just a preview.
    fn detail_highlight(&self) -> (ratatui::style::Style, &'static str) {
        if self.zeroclaw_pane == ZeroclawPane::Detail {
            (theme::selected_style(), "› ")
        } else {
            (theme::selected_inactive_style(), "  ")
        }
    }

    fn draw_section_tab_bar(&self, frame: &mut Frame, area: Rect) {
        let mut spans = Vec::new();
        for (i, sec) in CONFIG_SECTIONS.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", theme::dim_style()));
            }
            let style = if *sec == self.section {
                theme::accent_style().add_modifier(Modifier::BOLD)
            } else {
                theme::dim_style()
            };
            spans.push(Span::styled(sec.label(), style));
        }
        // Surface the zerocode pane's last status inline on the bar.
        if self.section == ConfigSection::Zerocode
            && let Some(msg) = self.zerocode.status()
        {
            spans.push(Span::styled("   ", theme::dim_style()));
            spans.push(Span::styled(msg.to_string(), theme::warn_style()));
        }
        // Surface the active config directory so the user can tell which
        // on-disk state the displayed values came from when running with
        // --config-dir, $ZEROCLAW_CONFIG_DIR, or a daemon backed by a
        // different config source.
        spans.push(Span::styled("   ", theme::dim_style()));
        spans.push(Span::styled(
            format!("config: {}", self.config_dir_display),
            theme::dim_style(),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Handle a key event. Returns `Ok(true)` when the user wants to
    /// quit the entire TUI (never triggered from Config; use Ctrl+C at the app level).
    pub(crate) async fn handle_key(&mut self, key: KeyEvent, term: &mut Term) -> Result<bool> {
        self.status_msg = None;

        // Tab / Shift+Tab cycle the outer Config section (zeroclaw ↔
        // zerocode) from anywhere — neither is bound inside the daemon
        // editor or the zerocode pane, so there is no shadowing.
        if key.code == KeyCode::Tab && key.modifiers == KeyModifiers::NONE {
            self.cycle_section(1);
            self.sync_zerocode_locales().await;
            return Ok(false);
        }
        if key.code == KeyCode::BackTab {
            self.cycle_section(-1);
            self.sync_zerocode_locales().await;
            return Ok(false);
        }

        if self.section == ConfigSection::Zerocode {
            self.zerocode.handle_key(key);
            self.sync_zerocode_locales().await;
            return Ok(false);
        }

        // Focus on the section list: keys drive the list (which eagerly loads
        // the highlighted section into the right pane for preview). Focus on the
        // detail: keys drive whatever drill screen is loaded.
        if self.zeroclaw_pane == ZeroclawPane::Sections {
            return self.handle_section_list(key).await;
        }

        // At a section's top drill level, Left/Back returns focus to the section
        // list without tearing down the loaded screen, so its cursor is kept.
        // Deeper levels fall through to the drill handlers' one-level pop.
        {
            use crate::keymap::ConfigTabAction;
            if matches!(
                ConfigTabAction::from_chord(&key),
                Some(ConfigTabAction::Back | ConfigTabAction::TabLeft)
            ) && self.at_section_top_level()
            {
                self.zeroclaw_pane = ZeroclawPane::Sections;
                return Ok(false);
            }
        }

        match &self.screen {
            Screen::SectionList => {
                return self.handle_section_list(key).await;
            }
            Screen::TypeList { .. } => self.handle_type_list(key).await?,
            Screen::AliasList { .. } => self.handle_alias_list(key).await?,
            Screen::AliasCreate { .. } => self.handle_alias_create(key).await?,
            Screen::FieldList { .. } => self.handle_field_list(key, term).await?,
            Screen::FieldEdit { .. } => self.handle_field_edit(key).await?,
        }
        // A drill handler that walked all the way out resets to SectionList;
        // translate that into "focus returns to the left pane" and reload the
        // highlighted section so the right pane keeps previewing it.
        if matches!(self.screen, Screen::SectionList) {
            self.zeroclaw_pane = ZeroclawPane::Sections;
            self.load_section_content(self.section_cursor).await?;
        }
        Ok(false)
    }

    fn cycle_section(&mut self, delta: isize) {
        let i = CONFIG_SECTIONS
            .iter()
            .position(|s| *s == self.section)
            .unwrap_or(0) as isize;
        let n = CONFIG_SECTIONS.len() as isize;
        self.section = CONFIG_SECTIONS[(((i + delta) % n + n) % n) as usize];
    }

    /// Handle a mouse event forwarded from the app event loop.
    pub(crate) async fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        _area: Rect,
        term: &mut Term,
    ) -> Result<()> {
        use crate::mouse;

        // Section tab-bar click switches sub-tab in either section.
        if let MouseEventKind::Down(crossterm::event::MouseButton::Left) = mouse.kind
            && let Some(bar) = self.section_tab_area
            && mouse::in_rect(mouse.column, mouse.row, bar)
        {
            let labels: Vec<&str> = CONFIG_SECTIONS.iter().map(|s| s.label()).collect();
            if let Some(idx) = mouse::tab_click_index(mouse.column, mouse.row, bar, &labels, 3) {
                self.section = CONFIG_SECTIONS[idx];
                return Ok(());
            }
        }

        // The zerocode pane owns its own mouse handling. Drain the locale
        // sync afterward so a mouse-driven "Download locale file" (or a
        // click into the Locale tab) triggers the lazy list/fetch RPC the
        // same way the key path does — otherwise the request is queued and
        // never sent, leaving the tab stuck on "loading locales…".
        if self.section == ConfigSection::Zerocode {
            self.zerocode.handle_mouse(mouse);
            self.sync_zerocode_locales().await;
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // Aliases/Costs tab bar click on a cost-bearing provider list.
                if self.alias_list_has_tabs()
                    && let Some(tab_rect) = self.last_tab_area
                    && mouse::in_rect(mouse.column, mouse.row, tab_rect)
                {
                    let labels = [ConfigTab::Aliases.label(), ConfigTab::Costs.label()];
                    let display: Vec<String> = labels
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            if i == self.alias_tab {
                                format!("▸ {l}")
                            } else {
                                l.to_string()
                            }
                        })
                        .collect();
                    let display_refs: Vec<&str> = display.iter().map(|s| s.as_str()).collect();
                    if let Some(idx) =
                        mouse::tab_click_index(mouse.column, mouse.row, tab_rect, &display_refs, 3)
                        && idx != self.alias_tab
                        && idx < labels.len()
                    {
                        self.alias_tab = idx;
                        self.deactivate_filter();
                        if idx == 1 {
                            self.load_cost_resources().await?;
                        }
                    }
                    return Ok(());
                }

                // Tab bar click (FieldList only).
                if let Some(tab_rect) = self.last_tab_area
                    && mouse::in_rect(mouse.column, mouse.row, tab_rect)
                {
                    let labels: Vec<&str> = self.tab_names.iter().map(|t| t.label()).collect();
                    // Each rendered label is "▸ <label>" (active, +2 chars) or
                    // "<label>" (inactive). For hit testing we use the plain
                    // label width + 2 for the active tab's prefix. However
                    // `tab_click_index` just walks fixed widths, so build
                    // display labels matching what draw_field_list renders.
                    let display: Vec<String> = labels
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            if i == self.active_tab {
                                format!("▸ {l}")
                            } else {
                                l.to_string()
                            }
                        })
                        .collect();
                    let display_refs: Vec<&str> = display.iter().map(|s| s.as_str()).collect();
                    if let Some(idx) = mouse::tab_click_index(
                        mouse.column,
                        mouse.row,
                        tab_rect,
                        &display_refs,
                        3, // " │ " separator
                    ) && idx != self.active_tab
                        && idx < self.tab_names.len()
                    {
                        self.active_tab = idx;
                        self.field_cursor = self.tab_field_indices().first().copied().unwrap_or(0);
                        self.deactivate_filter();
                        self.on_tab_switched(term).await?;
                    }
                    return Ok(());
                }

                // Section pane click: select the clicked section, return focus to
                // the left pane, and preview that section on the right. Display
                // rows resolve through the draw-time `last_section_rows` map so
                // group headers are dead zones rather than off-by-N selections.
                if mouse::in_rect(mouse.column, mouse.row, self.last_section_pane_area) {
                    if let Some(pos) = mouse::list_click_index(
                        mouse.row,
                        self.last_section_list_area,
                        self.last_section_list_offset,
                        self.last_section_rows.len(),
                    ) && let Some(&Some(orig)) = self.last_section_rows.get(pos)
                    {
                        self.section_cursor = orig;
                    }
                    self.zeroclaw_pane = ZeroclawPane::Sections;
                    self.preview_section(self.section_cursor).await?;
                    self.status_msg = None;
                    return Ok(());
                }

                // List area click.
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) {
                    let count = self.visible_count();
                    if let Some(pos) = mouse::list_click_index(
                        mouse.row,
                        self.last_main_area,
                        self.last_list_offset,
                        count,
                    ) {
                        let is_double = self.double_click.click(mouse.column, mouse.row);
                        self.set_visible_cursor(pos);
                        if is_double {
                            self.activate_mouse(term).await?;
                        }
                    }
                }
            }

            // Scroll over the pinned section list moves the section highlight and
            // re-previews — works whether focus is on the sections or the detail.
            MouseEventKind::ScrollUp
                if self.section_cursor > 0
                    && mouse::in_rect(mouse.column, mouse.row, self.last_section_pane_area) =>
            {
                self.section_cursor -= 1;
                self.preview_section(self.section_cursor).await?;
            }
            MouseEventKind::ScrollDown
                if self.section_cursor + 1 < self.sections.len()
                    && mouse::in_rect(mouse.column, mouse.row, self.last_section_pane_area) =>
            {
                self.section_cursor += 1;
                self.preview_section(self.section_cursor).await?;
            }

            MouseEventKind::ScrollUp
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) =>
            {
                let cur = self.visible_cursor();
                let count = self.visible_count();
                let next = mouse::list_scroll(cur, count, true, 3);
                self.set_visible_cursor(next);
            }

            MouseEventKind::ScrollDown
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) =>
            {
                let cur = self.visible_cursor();
                let count = self.visible_count();
                let next = mouse::list_scroll(cur, count, false, 3);
                self.set_visible_cursor(next);
            }

            _ => {}
        }
        Ok(())
    }

    // ── Mouse helper methods ─────────────────────────────────────

    /// Number of visible items for the current screen (respecting filters).
    fn visible_count(&self) -> usize {
        match &self.screen {
            Screen::SectionList => {
                let labels: Vec<String> = self.sections.iter().map(|s| s.label.clone()).collect();
                self.filtered_indices(&labels).len()
            }
            Screen::TypeList { .. } => {
                let names: Vec<String> = self
                    .types
                    .iter()
                    .map(|t| t.path.rsplit('.').next().unwrap_or(&t.path).to_string())
                    .collect();
                self.filtered_indices(&names).len()
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    self.cost_resources.len() + 1 // +1 for [+ Add]
                } else {
                    let vis = self.filtered_indices(&self.aliases);
                    // +1 for [+ Add] when not filtering
                    if self.filter.is_none() {
                        vis.len() + 1
                    } else {
                        vis.len()
                    }
                }
            }
            Screen::AliasCreate { .. } => 0,
            Screen::FieldList { .. } => {
                if self.is_composite_tab() {
                    match self.tab_names[self.active_tab] {
                        ConfigTab::Personality => {
                            if self.personality_active_file.is_some() {
                                0
                            } else {
                                self.personality_files.len()
                            }
                        }
                        ConfigTab::Skills => {
                            if self.skills_active.is_some() {
                                0
                            } else {
                                self.skills_list.len()
                            }
                        }
                        _ => self.visible_field_count(),
                    }
                } else {
                    self.visible_field_count()
                }
            }
            Screen::FieldEdit { .. } => {
                if self.is_select_edit() {
                    self.filtered_indices(&self.select_items).len()
                } else {
                    0
                }
            }
        }
    }

    /// Compute the display label for each tab-visible field. Labels are paths
    /// relative to the current screen prefix so nested fields stay distinct
    /// (e.g. `tool_receipts.enabled` instead of just `enabled`).
    fn field_labels_for_tab(&self, tab_indices: &[usize]) -> Vec<String> {
        let screen_prefix: &str = match &self.screen {
            Screen::FieldList { prefix, .. } => prefix.as_str(),
            _ => "",
        };
        tab_indices
            .iter()
            .map(|&i| {
                let path = self.fields[i].path.as_str();
                let rel = if !screen_prefix.is_empty() {
                    path.strip_prefix(screen_prefix)
                        .and_then(|s| s.strip_prefix('.'))
                        .unwrap_or(path)
                } else {
                    path
                };
                if rel.is_empty() {
                    path.rsplit('.').next().unwrap_or(path).to_string()
                } else {
                    rel.to_string()
                }
            })
            .collect()
    }

    /// Helper: visible field count for the regular (non-composite) field list.
    fn visible_field_count(&self) -> usize {
        let tab_indices = self.tab_field_indices();
        let tab_names = self.field_labels_for_tab(&tab_indices);
        let filter_vis = self.filtered_indices(&tab_names);
        filter_vis.len()
    }

    /// Current cursor position in visible (filtered) coordinates.
    fn visible_cursor(&self) -> usize {
        match &self.screen {
            Screen::SectionList => {
                if self.filter.is_some() {
                    self.filter_cursor
                } else {
                    let labels: Vec<String> =
                        self.sections.iter().map(|s| s.label.clone()).collect();
                    self.filtered_indices(&labels)
                        .iter()
                        .position(|&i| i == self.section_cursor)
                        .unwrap_or(0)
                }
            }
            Screen::TypeList { .. } => {
                if self.filter.is_some() {
                    self.filter_cursor
                } else {
                    let names: Vec<String> = self
                        .types
                        .iter()
                        .map(|t| t.path.rsplit('.').next().unwrap_or(&t.path).to_string())
                        .collect();
                    self.filtered_indices(&names)
                        .iter()
                        .position(|&i| i == self.type_cursor)
                        .unwrap_or(0)
                }
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    self.cost_cursor
                } else if self.filter.is_some() {
                    self.filter_cursor
                } else {
                    self.alias_cursor
                }
            }
            Screen::AliasCreate { .. } => 0,
            Screen::FieldList { .. } => {
                if self.is_composite_tab() {
                    match self.tab_names[self.active_tab] {
                        ConfigTab::Personality => self.personality_cursor,
                        ConfigTab::Skills => self.skills_cursor,
                        _ => self.visible_field_cursor(),
                    }
                } else {
                    self.visible_field_cursor()
                }
            }
            Screen::FieldEdit { .. } => {
                if self.filter.is_some() {
                    self.filter_cursor
                } else {
                    self.select_cursor
                }
            }
        }
    }

    /// Helper: current field cursor in visible coordinates.
    fn visible_field_cursor(&self) -> usize {
        if self.filter.is_some() {
            return self.filter_cursor;
        }
        let tab_indices = self.tab_field_indices();
        let tab_names = self.field_labels_for_tab(&tab_indices);
        let filter_vis = self.filtered_indices(&tab_names);
        let visible: Vec<usize> = filter_vis.iter().map(|&fi| tab_indices[fi]).collect();
        visible
            .iter()
            .position(|&i| i == self.field_cursor)
            .unwrap_or(0)
    }

    /// Set the cursor from a visible (filtered) position.
    fn set_visible_cursor(&mut self, pos: usize) {
        match &self.screen {
            Screen::SectionList => {
                let labels: Vec<String> = self.sections.iter().map(|s| s.label.clone()).collect();
                let visible = self.filtered_indices(&labels);
                if self.filter.is_some() {
                    self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                } else if let Some(&orig) = visible.get(pos) {
                    self.section_cursor = orig;
                }
            }
            Screen::TypeList { .. } => {
                let names: Vec<String> = self
                    .types
                    .iter()
                    .map(|t| t.path.rsplit('.').next().unwrap_or(&t.path).to_string())
                    .collect();
                let visible = self.filtered_indices(&names);
                if self.filter.is_some() {
                    self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                } else if let Some(&orig) = visible.get(pos) {
                    self.type_cursor = orig;
                }
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    let total = self.cost_resources.len() + 1; // +1 for [+ Add]
                    self.cost_cursor = pos.min(total.saturating_sub(1));
                } else if self.filter.is_some() {
                    let visible = self.filtered_indices(&self.aliases);
                    self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                } else {
                    let total = if self.filter.is_none() {
                        self.aliases.len() + 1 // +1 for [+ Add]
                    } else {
                        self.aliases.len()
                    };
                    self.alias_cursor = pos.min(total.saturating_sub(1));
                }
            }
            Screen::AliasCreate { .. } => {}
            Screen::FieldList { .. } => {
                if self.is_composite_tab() {
                    match self.tab_names[self.active_tab] {
                        ConfigTab::Personality => {
                            self.personality_cursor =
                                pos.min(self.personality_files.len().saturating_sub(1));
                        }
                        ConfigTab::Skills => {
                            self.skills_cursor = pos.min(self.skills_list.len().saturating_sub(1));
                        }
                        _ => self.set_visible_field_cursor(pos),
                    }
                } else {
                    self.set_visible_field_cursor(pos);
                }
            }
            Screen::FieldEdit { .. } => {
                if self.is_select_edit() {
                    let visible = self.filtered_indices(&self.select_items);
                    if self.filter.is_some() {
                        self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                    } else if pos < visible.len() {
                        self.select_cursor = pos;
                    }
                }
            }
        }
    }

    /// Helper: set field cursor from visible position.
    fn set_visible_field_cursor(&mut self, pos: usize) {
        let tab_indices = self.tab_field_indices();
        let tab_names = self.field_labels_for_tab(&tab_indices);
        let filter_vis = self.filtered_indices(&tab_names);
        let visible: Vec<usize> = filter_vis.iter().map(|&fi| tab_indices[fi]).collect();
        if self.filter.is_some() {
            self.filter_cursor = pos.min(filter_vis.len().saturating_sub(1));
        } else if let Some(&orig) = visible.get(pos) {
            self.field_cursor = orig;
        }
    }

    /// Activate the currently selected item (double-click equivalent of Enter).
    async fn activate_mouse(&mut self, term: &mut Term) -> Result<()> {
        match &self.screen {
            Screen::SectionList => {
                let idx = self.section_cursor;
                self.enter_section(idx).await?;
            }
            Screen::TypeList { .. } => {
                let idx = self.type_cursor;
                self.enter_type(idx).await?;
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    if self.cost_cursor < self.cost_resources.len() {
                        let idx = self.cost_cursor;
                        self.enter_cost_resource(idx).await?;
                    }
                } else if self.alias_cursor < self.aliases.len() {
                    let idx = self.alias_cursor;
                    self.enter_alias(idx).await?;
                }
                // If on [+ Add], double-click does nothing — use keyboard.
            }
            Screen::AliasCreate { .. } => {}
            Screen::FieldList { .. } => {
                if self.is_composite_tab() {
                    // Double-click on personality file or skill opens editor —
                    // that requires async loading which mirrors the Enter key
                    // handler. For now, no-op on composite tabs.
                } else if self.field_cursor < self.fields.len() {
                    self.enter_field_edit(self.field_cursor, term).await;
                }
            }
            Screen::FieldEdit { .. } => {
                if self.is_select_edit() {
                    let visible = self.filtered_indices(&self.select_items);
                    let cursor = if self.filter.is_some() {
                        self.filter_cursor
                    } else {
                        self.select_cursor
                    };
                    if let Some(&orig) = visible.get(cursor) {
                        self.commit_select(orig).await?;
                    }
                }
            }
        }
        Ok(())
    }

    // ── Data loading ─────────────────────────────────────────────

    fn types_for_section(&self, section_key: &str) -> Vec<ConfigTemplateEntry> {
        let prefix = format!("{}.", section_key);
        self.templates
            .iter()
            .filter(|t| t.path.starts_with(&prefix))
            .cloned()
            .collect()
    }

    async fn load_type_alias_counts(&mut self) -> Result<()> {
        self.type_alias_counts.clear();
        for tmpl in &self.types {
            let count = self
                .rpc
                .config_map_keys(&tmpl.path)
                .await
                .map(|k| k.len())
                .unwrap_or(0);
            self.type_alias_counts.push(count);
        }
        Ok(())
    }

    /// Bridge the sync zerocode pane to the async RPC client: lazily load the
    /// locale registry when the Locale tab needs it, and drain a queued
    /// "download locale file" request. Errors surface to the pane status line —
    /// no crash, no orphaned request.
    async fn sync_zerocode_locales(&mut self) {
        if self.section != ConfigSection::Zerocode {
            return;
        }
        if self.zerocode.locale_needs_list() {
            match self.rpc.locales_list().await {
                Ok(locales) => self.zerocode.set_locales(locales),
                // Surface the failure instead of silently retrying forever on
                // every keypress with the tab stuck on "loading locales…".
                Err(e) => self.zerocode.report_list_error(&e.to_string()),
            }
        }
        if let Some(locale) = self.zerocode.take_pending_fetch() {
            match self.rpc.locales_fetch(&locale, &[]).await {
                Ok(res) => self
                    .zerocode
                    .apply_fetched(&locale, &res.catalogs, &res.skipped),
                Err(e) => self.zerocode.report_fetch_error(&locale, &e.to_string()),
            }
        }
        // Agent theme overrides: feed the same enabled-agent list the Code/Chat
        // pickers walk, fetched lazily when the AgentTheme tab is first focused.
        if self.zerocode.agents_needs_list() {
            match self.rpc.agents_status().await {
                Ok(res) => {
                    let aliases = res
                        .agents
                        .into_iter()
                        .filter(|a| a.enabled)
                        .map(|a| a.alias)
                        .collect();
                    self.zerocode.set_agents(aliases);
                }
                Err(e) => self.zerocode.report_agents_error(&e.to_string()),
            }
        }
    }

    async fn load_aliases(&mut self, map_path: &str) -> Result<()> {
        self.aliases = self.rpc.config_map_keys(map_path).await?;
        self.alias_enabled.clear();
        for alias in &self.aliases {
            let enabled_path = format!("{}.{}.enabled", map_path, alias);
            let fields = self
                .rpc
                .config_list(Some(&enabled_path))
                .await
                .unwrap_or_default();
            let status = fields.first().and_then(|f| {
                f.value
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .map(|s| s == "true")
            });
            self.alias_enabled.push(status);
        }
        self.alias_cursor = 0;
        self.alias_tab = 0;
        self.cost_resources.clear();
        self.cost_cursor = 0;
        Ok(())
    }

    fn alias_list_cost_target(&self) -> Option<(String, String)> {
        if let Screen::AliasList {
            section_idx,
            breadcrumb,
            ..
        } = &self.screen
            && breadcrumb.len() >= 2
        {
            let category = &self.sections[*section_idx].cost_category;
            if !category.is_empty() {
                return Some((category.clone(), breadcrumb[1].clone()));
            }
        }
        None
    }

    /// Base map path for the cost-rates resource list on the active provider
    /// AliasList: `cost.rates.providers.<category>.<type>`.
    fn cost_base_path(&self) -> Option<String> {
        self.alias_list_cost_target()
            .map(|(category, provider_type)| {
                format!("cost.rates.providers.{category}.{provider_type}")
            })
    }

    /// Whether the active AliasList carries the Aliases/Costs tab pair.
    fn alias_list_has_tabs(&self) -> bool {
        self.alias_list_cost_target().is_some()
    }

    async fn load_cost_resources(&mut self) -> Result<()> {
        if let Some(base) = self.cost_base_path() {
            self.cost_resources = self.rpc.config_map_keys(&base).await.unwrap_or_default();
        } else {
            self.cost_resources.clear();
        }
        self.cost_cursor = 0;
        Ok(())
    }

    /// Pre-fill a freshly created cost-rate resource from the live provider
    /// catalog, matching the web Costs editor. Reuses the existing
    /// `catalog_models` RPC (same payload the gateway serves the dashboard);
    /// only the `models` category carries token pricing, so other categories
    /// are left for manual entry. Best-effort: any miss leaves the sheet
    /// empty rather than surfacing an error.
    async fn prefill_cost_rates_from_catalog(&self, base_path: &str, resource: &str) {
        let Some(provider_type) = base_path.strip_prefix("cost.rates.providers.models.") else {
            return;
        };
        let Ok(catalog) = self.rpc.catalog_models(provider_type).await else {
            return;
        };
        let Some(pricing) = catalog.pricing.as_ref().and_then(|p| p.get(resource)) else {
            return;
        };
        // Catalog rates are USD per token; cost sheets store USD per million.
        let per_mtok = |s: &Option<String>| -> Option<f64> {
            s.as_ref()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|v| v * 1_000_000.0)
        };
        let fields = [
            ("input_per_mtok", per_mtok(&pricing.prompt)),
            ("output_per_mtok", per_mtok(&pricing.completion)),
            ("cached_input_per_mtok", per_mtok(&pricing.input_cache_read)),
        ];
        for (field, value) in fields {
            if let Some(v) = value {
                let prop = format!("{base_path}.{resource}.{field}");
                let _ = self.rpc.config_set(&prop, serde_json::json!(v)).await;
            }
        }
    }

    async fn load_fields(&mut self, prefix: &str) -> Result<()> {
        self.fields = self.rpc.config_list(Some(prefix)).await?;
        self.field_cursor = 0;
        // Compute distinct tab names in field-declaration order.
        let mut tabs = Vec::new();
        for f in &self.fields {
            if !f.tab.is_none() && !tabs.contains(&f.tab) {
                tabs.push(f.tab);
            }
        }
        // Append composite tabs for agents and skill-bundles.
        let mut has_composite = false;
        if prefix.starts_with("agents.") {
            tabs.push(ConfigTab::Personality);
            has_composite = true;
            // Extract agent alias from prefix (agents.<alias>).
            let agent = prefix.strip_prefix("agents.").unwrap_or("").to_string();
            self.personality_agent = agent;
            self.personality_active_file = None;
            self.personality_files.clear();
            self.personality_cursor = 0;
        }
        if prefix.starts_with("skill-bundles.") {
            tabs.push(ConfigTab::Skills);
            has_composite = true;
            let bundle = prefix
                .strip_prefix("skill-bundles.")
                .unwrap_or("")
                .to_string();
            self.skills_bundle = bundle;
            self.skills_active = None;
            self.skills_list.clear();
            self.skills_cursor = 0;
        }
        // When composite tabs exist and some fields have no tab annotation,
        // prepend a "Settings" tab so those fields remain accessible.
        if has_composite && self.fields.iter().any(|f| f.tab == ConfigTab::None) {
            tabs.insert(0, ConfigTab::Settings);
            // Re-tag un-annotated fields so tab_field_indices() finds them.
            for f in &mut self.fields {
                if f.tab == ConfigTab::None {
                    f.tab = ConfigTab::Settings;
                }
            }
        }
        self.tab_names = tabs;
        self.active_tab = 0;
        // Eagerly load composite-tab data so it's ready when the user
        // switches to that tab (avoids showing an empty list).
        if has_composite {
            if prefix.starts_with("agents.") {
                let _ = self.load_personality_files().await;
            }
            if prefix.starts_with("skill-bundles.") {
                let _ = self.load_skills_list().await;
            }
        }
        Ok(())
    }

    /// Refresh field values from the server WITHOUT disturbing UI state
    /// (active tab, cursor, scroll, filter). Used on tab/pane transitions
    /// so values stay current after out-of-band edits. Silent on failure —
    /// retains the previously loaded data so the user sees no flicker.
    async fn reload_fields_silent(&mut self, prefix: &str) {
        let Ok(new_fields) = self.rpc.config_list(Some(prefix)).await else {
            return;
        };
        // Preserve the synthesised Settings/composite tab promotion logic
        // from load_fields(): if a Settings tab exists, retag un-annotated
        // fields so tab_field_indices() keeps finding them.
        let has_settings_tab = self.tab_names.contains(&ConfigTab::Settings);
        let mut new_fields = new_fields;
        if has_settings_tab {
            for f in &mut new_fields {
                if f.tab == ConfigTab::None {
                    f.tab = ConfigTab::Settings;
                }
            }
        }
        self.fields = new_fields;
        // Clamp cursor in case fields shrank.
        if !self.fields.is_empty() && self.field_cursor >= self.fields.len() {
            self.field_cursor = self.fields.len() - 1;
        }
    }

    /// Convenience: silently reload whichever prefix the current FieldList
    /// is displaying. No-op when the current screen is not a FieldList.
    async fn reload_current_field_list_silent(&mut self) {
        let prefix = match &self.screen {
            Screen::FieldList { prefix, .. } => prefix.clone(),
            _ => return,
        };
        self.reload_fields_silent(&prefix).await;
    }

    /// Indices of fields visible under the active tab (all fields when no tabs).
    fn tab_field_indices(&self) -> Vec<usize> {
        if self.tab_names.is_empty() {
            return (0..self.fields.len()).collect();
        }
        let active = &self.tab_names[self.active_tab];
        self.fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.tab == *active)
            .map(|(i, _)| i)
            .collect()
    }

    /// Whether the active tab is a composite (custom-rendered) tab.
    fn is_composite_tab(&self) -> bool {
        if self.tab_names.is_empty() {
            return false;
        }
        matches!(
            self.tab_names[self.active_tab],
            ConfigTab::Personality | ConfigTab::Skills
        )
    }

    async fn load_personality_files(&mut self) -> Result<()> {
        let result = self
            .rpc
            .personality_list(Some(&self.personality_agent))
            .await?;
        self.personality_files = result.files;
        self.personality_max_chars = result.max_chars;
        self.personality_cursor = 0;
        self.personality_active_file = None;
        self.personality_content.clear();
        self.personality_loaded.clear();
        Ok(())
    }

    async fn load_personality_file(&mut self, filename: &str) -> Result<()> {
        let result = self
            .rpc
            .personality_get(&self.personality_agent, filename)
            .await?;
        let content = result.content.unwrap_or_default();
        self.personality_loaded = content.clone();
        self.personality_content = content;
        self.personality_active_file = Some(filename.to_string());
        Ok(())
    }

    async fn load_skills_list(&mut self) -> Result<()> {
        let result = self.rpc.skills_list(Some(&self.skills_bundle)).await?;
        self.skills_list = result.skills;
        self.skills_cursor = 0;
        self.skills_active = None;
        self.skills_body.clear();
        self.skills_body_loaded.clear();
        self.skills_frontmatter = Default::default();
        self.skills_frontmatter_loaded = Default::default();
        Ok(())
    }

    async fn load_skill(&mut self, name: &str) -> Result<()> {
        let result = self.rpc.skills_read(&self.skills_bundle, name).await?;
        self.skills_body_loaded = result.body.clone();
        self.skills_body = result.body;
        self.skills_frontmatter_loaded = result.frontmatter.clone();
        self.skills_frontmatter = result.frontmatter;
        self.skills_active = Some(name.to_string());
        Ok(())
    }

    // ── Section list ─────────────────────────────────────────────

    async fn handle_section_list(&mut self, key: KeyEvent) -> Result<bool> {
        let labels: Vec<String> = self.sections.iter().map(|s| s.label.clone()).collect();
        let visible = self.filtered_indices(&labels);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => {
                // The filter edit may have changed the filtered list or moved
                // `filter_cursor`; resolve the highlighted filtered row back to
                // its underlying section so the right-pane preview matches what
                // `draw_sections_pane` highlights and what Enter will open.
                let visible = self.filtered_indices(&labels);
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.section_cursor = orig;
                }
                self.preview_section(self.section_cursor).await?;
                return Ok(false);
            }
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.section_cursor = orig;
                    self.deactivate_filter();
                    return self.open_section(orig).await;
                }
                return Ok(false);
            }
            FilterAction::Passthrough => {}
        }

        use crate::keymap::ConfigTabAction;
        let action = ConfigTabAction::from_chord(&key);
        match action {
            // Left at the section list is home — no-op (no tab jump).
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => return Ok(false),
            Some(ConfigTabAction::Up) => {
                self.section_cursor = self.section_cursor.saturating_sub(1);
                self.preview_section(self.section_cursor).await?;
            }
            Some(ConfigTabAction::Down) if self.section_cursor + 1 < self.sections.len() => {
                self.section_cursor += 1;
                self.preview_section(self.section_cursor).await?;
            }
            // Enter/Right move focus into the detail pane (content already loaded
            // by the live preview).
            Some(ConfigTabAction::Enter | ConfigTabAction::TabRight) => {
                return self.open_section(self.section_cursor).await;
            }
            _ => {}
        }
        Ok(false)
    }

    /// The right-pane top-level cursor for the currently loaded section, keyed by
    /// the loaded screen shape.
    fn current_top_cursor(&self) -> usize {
        match &self.screen {
            Screen::TypeList { .. } => self.type_cursor,
            Screen::AliasList { breadcrumb, .. } if breadcrumb.len() <= 1 => self.alias_cursor,
            Screen::FieldList { breadcrumb, .. } if breadcrumb.len() <= 1 => self.field_cursor,
            _ => 0,
        }
    }

    /// Restore a remembered top-level cursor onto the freshly loaded section,
    /// clamped to the loaded list length.
    fn restore_top_cursor(&mut self, pos: usize) {
        match &self.screen {
            Screen::TypeList { .. } => {
                self.type_cursor = pos.min(self.types.len().saturating_sub(1));
            }
            Screen::AliasList { breadcrumb, .. } if breadcrumb.len() <= 1 => {
                self.alias_cursor = pos.min(self.aliases.len().saturating_sub(1));
            }
            Screen::FieldList { breadcrumb, .. } if breadcrumb.len() <= 1 => {
                self.field_cursor = pos.min(self.fields.len().saturating_sub(1));
            }
            _ => {}
        }
    }

    /// Load the highlighted section's content into the right pane for preview,
    /// without moving keyboard focus off the section list. Re-previewing the
    /// already-loaded section is a no-op so its cursor is preserved; switching to
    /// a different section saves the outgoing cursor and restores the incoming
    /// section's remembered position.
    async fn preview_section(&mut self, idx: usize) -> Result<()> {
        if self.loaded_section == Some(idx) {
            return Ok(());
        }
        if let Some(prev) = self.loaded_section {
            let pos = self.current_top_cursor();
            self.section_top_cursor.insert(prev, pos);
        }
        self.load_section_content(idx).await?;
        if let Some(&pos) = self.section_top_cursor.get(&idx) {
            self.restore_top_cursor(pos);
        }
        Ok(())
    }

    /// Move focus into the detail pane for the highlighted section, loading its
    /// content first if needed.
    async fn open_section(&mut self, idx: usize) -> Result<bool> {
        if self.loaded_section != Some(idx) {
            self.load_section_content(idx).await?;
        }
        if !matches!(self.screen, Screen::SectionList) {
            self.zeroclaw_pane = ZeroclawPane::Detail;
        }
        Ok(false)
    }

    /// True when the detail pane is at the section's top drill level — the first
    /// breadcrumb level, and on the leftmost field sub-tab if any — so Left/Back
    /// should return focus to the section list rather than pop a level or switch
    /// a sub-tab.
    fn at_section_top_level(&self) -> bool {
        let depth_one = match &self.screen {
            Screen::TypeList { .. } => true,
            Screen::AliasList { breadcrumb, .. } | Screen::FieldList { breadcrumb, .. } => {
                breadcrumb.len() <= 1
            }
            _ => false,
        };
        let on_left_subtab = self.tab_names.is_empty() || self.active_tab == 0;
        depth_one && on_left_subtab
    }

    async fn load_section_content(&mut self, idx: usize) -> Result<()> {
        if let Some(section) = self.sections.get(idx) {
            let section_key = section.key.clone();
            match section.shape {
                Some(SectionShape::TypedFamilyMap) => {
                    self.types = self.types_for_section(&section_key);
                    self.type_cursor = 0;
                    self.load_type_alias_counts().await?;
                    self.screen = Screen::TypeList { section_idx: idx };
                }
                Some(SectionShape::OneTierAliasMap) => {
                    self.load_aliases(&section_key).await?;
                    self.screen = Screen::AliasList {
                        section_idx: idx,
                        map_path: section_key.clone(),
                        breadcrumb: vec![section_key],
                    };
                }
                Some(SectionShape::DirectForm) | Some(SectionShape::BackendPicker) | None => {
                    self.load_fields(&section_key).await?;
                    self.screen = Screen::FieldList {
                        section_idx: idx,
                        prefix: section_key.clone(),
                        breadcrumb: vec![section_key],
                    };
                }
            }
            self.loaded_section = Some(idx);
            self.status_msg = None;
        }
        Ok(())
    }

    async fn enter_section(&mut self, idx: usize) -> Result<bool> {
        self.load_section_content(idx).await?;
        if !matches!(self.screen, Screen::SectionList) {
            self.zeroclaw_pane = ZeroclawPane::Detail;
        }
        Ok(false)
    }

    // ── Type list (TypedFamilyMap) ───────────────────────────────

    async fn handle_type_list(&mut self, key: KeyEvent) -> Result<()> {
        let type_names: Vec<String> = self
            .types
            .iter()
            .map(|t| t.path.rsplit('.').next().unwrap_or(&t.path).to_string())
            .collect();
        let visible = self.filtered_indices(&type_names);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.deactivate_filter();
                    return self.enter_type(orig).await;
                }
                return Ok(());
            }
            FilterAction::Passthrough => {}
        }

        use crate::keymap::ConfigTabAction;
        let action = ConfigTabAction::from_chord(&key);
        match action {
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => {
                self.screen = Screen::SectionList;
                self.status_msg = None;
            }
            Some(ConfigTabAction::Up) => {
                self.type_cursor = self.type_cursor.saturating_sub(1);
            }
            Some(ConfigTabAction::Down) if self.type_cursor + 1 < self.types.len() => {
                self.type_cursor += 1;
            }
            Some(ConfigTabAction::Enter | ConfigTabAction::TabRight) => {
                self.enter_type(self.type_cursor).await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn enter_type(&mut self, orig_idx: usize) -> Result<()> {
        if let (Some(tmpl), Screen::TypeList { section_idx }) =
            (self.types.get(orig_idx), &self.screen)
        {
            let section_idx = *section_idx;
            let map_path = tmpl.path.clone();
            let type_name = map_path.rsplit('.').next().unwrap_or(&map_path).to_string();
            let section_key = self.sections[section_idx].key.clone();
            self.load_aliases(&map_path).await?;
            self.screen = Screen::AliasList {
                section_idx,
                map_path,
                breadcrumb: vec![section_key, type_name],
            };
            self.status_msg = None;
        }
        Ok(())
    }

    // ── Alias list ───────────────────────────────────────────────

    async fn handle_alias_list(&mut self, key: KeyEvent) -> Result<()> {
        use crate::keymap::ConfigTabAction;
        let has_tabs = self.alias_list_has_tabs();

        // Aliases/Costs tab switching reuses the same TabLeft/TabRight chords
        // the FieldList tab bar uses. On these screens those chords no longer
        // mean back/into; Back is the only way out to the type list.
        if has_tabs && let Some(action) = ConfigTabAction::from_chord(&key) {
            match action {
                ConfigTabAction::TabLeft => {
                    if self.alias_tab > 0 {
                        self.alias_tab -= 1;
                        self.deactivate_filter();
                    }
                    return Ok(());
                }
                ConfigTabAction::TabRight => {
                    if self.alias_tab == 0 {
                        self.alias_tab = 1;
                        self.deactivate_filter();
                        self.load_cost_resources().await?;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }

        if has_tabs && self.alias_tab == 1 {
            return self.handle_cost_tab(key).await;
        }

        let visible = self.filtered_indices(&self.aliases);
        // +1 for [+ Add] (only when not filtering)
        let has_add = self.filter.is_none();
        let visible_total = if has_add {
            visible.len() + 1
        } else {
            visible.len()
        };

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.deactivate_filter();
                    return self.enter_alias(orig).await;
                }
                return Ok(());
            }
            FilterAction::Passthrough => {}
        }

        let add_pos = visible.len(); // position of [+ Add] in the rendered list
        let action = ConfigTabAction::from_chord(&key);
        // With tabs present, TabLeft no longer doubles as Back.
        let back = if has_tabs {
            matches!(action, Some(ConfigTabAction::Back))
        } else {
            matches!(
                action,
                Some(ConfigTabAction::Back | ConfigTabAction::TabLeft)
            )
        };
        let into = if has_tabs {
            matches!(action, Some(ConfigTabAction::Enter))
        } else {
            matches!(
                action,
                Some(ConfigTabAction::Enter | ConfigTabAction::TabRight)
            )
        };
        match action {
            _ if back => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::AliasList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                    && breadcrumb.len() >= 2
                {
                    self.types = self.types_for_section(&self.sections[section_idx].key);
                    self.screen = Screen::TypeList { section_idx };
                }
                self.status_msg = None;
            }
            Some(ConfigTabAction::Up) => {
                self.alias_cursor = self.alias_cursor.saturating_sub(1);
            }
            Some(ConfigTabAction::Down) if self.alias_cursor + 1 < visible_total => {
                self.alias_cursor += 1;
            }
            _ if into => {
                if has_add && self.alias_cursor == add_pos {
                    if let Screen::AliasList {
                        section_idx,
                        map_path,
                        breadcrumb,
                        ..
                    } = &self.screen
                    {
                        self.edit_buf.clear();
                        self.screen = Screen::AliasCreate {
                            section_idx: *section_idx,
                            map_path: map_path.clone(),
                            breadcrumb: breadcrumb.clone(),
                        };
                    }
                } else if self.alias_cursor < self.aliases.len() {
                    self.enter_alias(self.alias_cursor).await?;
                }
            }
            Some(ConfigTabAction::ToggleSecret) if self.alias_cursor < self.aliases.len() => {
                if let Screen::AliasList { map_path, .. } = &self.screen {
                    let alias = self.aliases[self.alias_cursor].clone();
                    let map_path = map_path.clone();
                    match self.rpc.config_map_key_delete(&map_path, &alias).await {
                        Ok(()) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-alias-deleted",
                                &[("alias", &alias)],
                            ));
                            self.load_aliases(&map_path).await?;
                            if self.alias_cursor > 0 && self.alias_cursor >= self.aliases.len() {
                                self.alias_cursor = self.aliases.len().saturating_sub(1);
                            }
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-delete-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Costs-tab key handling on a cost-bearing provider AliasList. Mirrors
    /// the Aliases tab: a resource list with [+ Add], Enter to open the rate
    /// sheet, delete-row to remove a resource. The map path is the
    /// `cost.rates.providers.<category>.<type>` subtree.
    async fn handle_cost_tab(&mut self, key: KeyEvent) -> Result<()> {
        use crate::keymap::ConfigTabAction;
        let Some(base) = self.cost_base_path() else {
            return Ok(());
        };
        let add_pos = self.cost_resources.len();
        let total = add_pos + 1; // [+ Add] always present on this tab
        let action = ConfigTabAction::from_chord(&key);
        match action {
            Some(ConfigTabAction::Back) => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::AliasList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                    && breadcrumb.len() >= 2
                {
                    self.types = self.types_for_section(&self.sections[section_idx].key);
                    self.screen = Screen::TypeList { section_idx };
                }
                self.status_msg = None;
            }
            Some(ConfigTabAction::Up) => {
                self.cost_cursor = self.cost_cursor.saturating_sub(1);
            }
            Some(ConfigTabAction::Down) if self.cost_cursor + 1 < total => {
                self.cost_cursor += 1;
            }
            Some(ConfigTabAction::Enter) => {
                if self.cost_cursor == add_pos {
                    if let Screen::AliasList {
                        section_idx,
                        breadcrumb,
                        ..
                    } = &self.screen
                    {
                        self.edit_buf.clear();
                        let mut bc = breadcrumb.clone();
                        bc.push(ConfigTab::Costs.label().to_string());
                        self.screen = Screen::AliasCreate {
                            section_idx: *section_idx,
                            map_path: base,
                            breadcrumb: bc,
                        };
                    }
                } else if self.cost_cursor < self.cost_resources.len() {
                    self.enter_cost_resource(self.cost_cursor).await?;
                }
            }
            Some(ConfigTabAction::DeleteRow | ConfigTabAction::ToggleSecret)
                if self.cost_cursor < self.cost_resources.len() =>
            {
                let resource = self.cost_resources[self.cost_cursor].clone();
                match self.rpc.config_map_key_delete(&base, &resource).await {
                    Ok(()) => {
                        self.status_msg = Some(crate::i18n::t_args(
                            "zc-config-status-alias-deleted",
                            &[("alias", &resource)],
                        ));
                        self.load_cost_resources().await?;
                        if self.cost_cursor > 0 && self.cost_cursor >= self.cost_resources.len() {
                            self.cost_cursor = self.cost_resources.len().saturating_sub(1);
                        }
                    }
                    Err(e) => {
                        self.status_msg = Some(crate::i18n::t_args(
                            "zc-config-status-delete-failed",
                            &[("err", &e.to_string())],
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn enter_cost_resource(&mut self, idx: usize) -> Result<()> {
        let Some(base) = self.cost_base_path() else {
            return Ok(());
        };
        if let Some(resource) = self.cost_resources.get(idx).cloned()
            && let Screen::AliasList {
                section_idx,
                breadcrumb,
                ..
            } = &self.screen
        {
            let prefix = format!("{base}.{resource}");
            let mut bc = breadcrumb.clone();
            bc.push(ConfigTab::Costs.label().to_string());
            bc.push(resource);
            let si = *section_idx;
            self.load_fields(&prefix).await?;
            self.screen = Screen::FieldList {
                section_idx: si,
                prefix,
                breadcrumb: bc,
            };
            self.status_msg = None;
        }
        Ok(())
    }

    /// Restore the provider AliasList when backing out of a FieldList. Pops
    /// the trailing segment; if a `Costs` sentinel remains it strips that too
    /// and reopens the AliasList on the Costs tab with its resource list
    /// loaded. Otherwise it falls back to the alias-tier list as before.
    async fn restore_alias_list_from_field_back(
        &mut self,
        section_idx: usize,
        breadcrumb: Vec<String>,
    ) -> Result<()> {
        let mut bc = breadcrumb;
        bc.pop();
        let costs_label = ConfigTab::Costs.label();
        let on_costs = bc.last().map(|s| s.as_str()) == Some(costs_label);
        if on_costs {
            bc.pop();
        }
        let section_key = &self.sections[section_idx].key;
        let map_path = if bc.len() == 1 {
            section_key.clone()
        } else {
            format!("{}.{}", section_key, bc[1..].join("."))
        };
        self.load_aliases(&map_path).await?;
        self.screen = Screen::AliasList {
            section_idx,
            map_path,
            breadcrumb: bc,
        };
        if on_costs {
            self.alias_tab = 1;
            self.load_cost_resources().await?;
        }
        Ok(())
    }

    /// Restore the provider AliasList when cancelling or failing a create.
    /// A `cost.rates.` map path means the create targeted the Costs tab; the
    /// breadcrumb ends in the `Costs` sentinel, so strip it and reopen on the
    /// Costs tab. Otherwise reopen the alias-tier list at the map path.
    async fn restore_alias_list_from_create_back(
        &mut self,
        section_idx: usize,
        map_path: String,
        breadcrumb: Vec<String>,
    ) -> Result<()> {
        let costs_label = ConfigTab::Costs.label();
        let on_costs = breadcrumb.last().map(|s| s.as_str()) == Some(costs_label);
        if on_costs {
            let mut bc = breadcrumb;
            bc.pop();
            let section_key = &self.sections[section_idx].key;
            let provider_path = if bc.len() == 1 {
                section_key.clone()
            } else {
                format!("{}.{}", section_key, bc[1..].join("."))
            };
            self.load_aliases(&provider_path).await?;
            self.screen = Screen::AliasList {
                section_idx,
                map_path: provider_path,
                breadcrumb: bc,
            };
            self.alias_tab = 1;
            self.load_cost_resources().await?;
        } else {
            self.load_aliases(&map_path).await?;
            self.screen = Screen::AliasList {
                section_idx,
                map_path,
                breadcrumb,
            };
        }
        Ok(())
    }

    async fn enter_alias(&mut self, orig_idx: usize) -> Result<()> {
        if let Some(alias) = self.aliases.get(orig_idx)
            && let Screen::AliasList {
                section_idx,
                map_path,
                breadcrumb,
                ..
            } = &self.screen
        {
            let prefix = format!("{}.{}", map_path, alias);
            let mut bc = breadcrumb.clone();
            bc.push(alias.clone());
            let si = *section_idx;
            self.load_fields(&prefix).await?;
            self.screen = Screen::FieldList {
                section_idx: si,
                prefix,
                breadcrumb: bc,
            };
            self.status_msg = None;
        }
        Ok(())
    }

    // ── Alias creation ───────────────────────────────────────────

    async fn handle_alias_create(&mut self, key: KeyEvent) -> Result<()> {
        use crate::keymap::ConfigEditorAction;
        let action = ConfigEditorAction::from_chord(&key);
        match action {
            Some(ConfigEditorAction::Cancel) => {
                if let Screen::AliasCreate {
                    section_idx,
                    map_path,
                    breadcrumb,
                    ..
                } = std::mem::replace(&mut self.screen, Screen::SectionList)
                {
                    self.restore_alias_list_from_create_back(section_idx, map_path, breadcrumb)
                        .await?;
                }
            }
            Some(ConfigEditorAction::Confirm) => {
                let name = self.edit_buf.trim().to_string();
                if name.is_empty() {
                    self.status_msg = Some(crate::i18n::t("zc-config-status-alias-empty"));
                    return Ok(());
                }
                if let Screen::AliasCreate {
                    section_idx,
                    map_path,
                    breadcrumb,
                    ..
                } = std::mem::replace(&mut self.screen, Screen::SectionList)
                {
                    match self.rpc.config_map_key_create(&map_path, &name).await {
                        Ok(()) => {
                            self.prefill_cost_rates_from_catalog(&map_path, &name).await;
                            let prefix = format!("{}.{}", map_path, name);
                            let mut bc = breadcrumb;
                            bc.push(name);
                            self.load_fields(&prefix).await?;
                            self.screen = Screen::FieldList {
                                section_idx,
                                prefix,
                                breadcrumb: bc,
                            };
                            self.status_msg = None;
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-alias-create-failed",
                                &[("err", &e.to_string())],
                            ));
                            self.restore_alias_list_from_create_back(
                                section_idx,
                                map_path,
                                breadcrumb,
                            )
                            .await?;
                        }
                    }
                }
            }
            Some(ConfigEditorAction::Backspace) => {
                self.edit_buf.pop();
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.edit_buf.push(c);
                }
            }
        }
        Ok(())
    }

    // ── Field list ───────────────────────────────────────────────

    async fn handle_field_list(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        // Composite tabs get their own handler; only ←/→/Esc fall through.
        if self.is_composite_tab() {
            match self.tab_names[self.active_tab] {
                ConfigTab::Personality => {
                    return self.handle_personality_tab(key, term).await;
                }
                ConfigTab::Skills => return self.handle_skills_tab(key, term).await,
                _ => {}
            }
        }

        // Fields visible under active tab, then filtered by `/` query.
        let tab_indices = self.tab_field_indices();
        let tab_names = self.field_labels_for_tab(&tab_indices);
        let filter_vis = self.filtered_indices(&tab_names);
        // Map back to original field indices.
        let visible: Vec<usize> = filter_vis.iter().map(|&fi| tab_indices[fi]).collect();

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.deactivate_filter();
                    self.field_cursor = orig;
                    self.enter_field_edit(orig, term).await;
                }
                return Ok(());
            }
            FilterAction::Passthrough => {}
        }

        use crate::keymap::ConfigTabAction;
        let action = ConfigTabAction::from_chord(&key);
        match action {
            // Left switches the field sub-tab leftward; once on the leftmost
            // sub-tab (or when the section has none) it walks back one breadcrumb
            // level, so Left is a continuous "out" gesture like the zerocode tab.
            Some(ConfigTabAction::TabLeft) if !self.tab_names.is_empty() && self.active_tab > 0 => {
                self.active_tab = self.active_tab.saturating_sub(1);
                self.field_cursor = self.tab_field_indices().first().copied().unwrap_or(0);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            Some(ConfigTabAction::TabRight) if !self.tab_names.is_empty() => {
                if self.active_tab + 1 < self.tab_names.len() {
                    self.active_tab += 1;
                }
                self.field_cursor = self.tab_field_indices().first().copied().unwrap_or(0);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                    && breadcrumb.len() >= 2
                {
                    self.restore_alias_list_from_field_back(section_idx, breadcrumb)
                        .await?;
                }
                self.status_msg = None;
            }
            Some(ConfigTabAction::Up) => {
                if let Some(pos) = visible.iter().position(|&i| i == self.field_cursor) {
                    if pos > 0 {
                        self.field_cursor = visible[pos - 1];
                    }
                } else if let Some(&first) = visible.first() {
                    self.field_cursor = first;
                }
            }
            Some(ConfigTabAction::Down) => {
                if let Some(pos) = visible.iter().position(|&i| i == self.field_cursor) {
                    if pos + 1 < visible.len() {
                        self.field_cursor = visible[pos + 1];
                    }
                } else if let Some(&first) = visible.first() {
                    self.field_cursor = first;
                }
            }
            Some(ConfigTabAction::Enter) if visible.contains(&self.field_cursor) => {
                self.enter_field_edit(self.field_cursor, term).await;
            }
            Some(ConfigTabAction::DeleteRow) => {
                if let Some(field) = self.fields.get(self.field_cursor) {
                    let prop = field.path.clone();
                    let saved_cursor = self.field_cursor;
                    if let Screen::FieldList { prefix, .. } = &self.screen {
                        let prefix = prefix.clone();
                        match self.rpc.config_delete(&prop).await {
                            Ok(()) => {
                                self.status_msg = Some(crate::i18n::t_args(
                                    "zc-config-status-field-reset",
                                    &[("prop", &prop)],
                                ));
                                self.load_fields(&prefix).await?;
                                self.field_cursor =
                                    saved_cursor.min(self.fields.len().saturating_sub(1));
                            }
                            Err(e) => {
                                self.status_msg = Some(crate::i18n::t_args(
                                    "zc-config-status-delete-failed",
                                    &[("err", &e.to_string())],
                                ));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ── Composite tab helpers ──────────────────────────────────────

    /// Called after ←/→ tab switch — loads data for composite tabs.
    async fn on_tab_switched(&mut self, term: &mut Term) -> Result<()> {
        // Silent refresh of the underlying field list so values stay
        // current after out-of-band edits — no flicker, no status churn.
        self.reload_current_field_list_silent().await;

        if !self.is_composite_tab() {
            return Ok(());
        }
        match self.tab_names[self.active_tab] {
            ConfigTab::Personality if self.personality_files.is_empty() => {
                self.status_msg = Some(crate::i18n::t("zc-config-status-loading-personality"));
                let _ = self.draw(term);
                match self.load_personality_files().await {
                    Ok(()) => self.status_msg = None,
                    Err(e) => {
                        self.status_msg = Some(crate::i18n::t_args(
                            "zc-config-status-load-failed",
                            &[("err", &e.to_string())],
                        ));
                    }
                }
            }
            ConfigTab::Skills if self.skills_list.is_empty() => {
                self.status_msg = Some(crate::i18n::t("zc-config-status-loading-skills"));
                let _ = self.draw(term);
                match self.load_skills_list().await {
                    Ok(()) => self.status_msg = None,
                    Err(e) => {
                        self.status_msg = Some(crate::i18n::t_args(
                            "zc-config-status-load-failed",
                            &[("err", &e.to_string())],
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ── Personality tab handler ──────────────────────────────────

    async fn handle_personality_tab(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        // Two modes: file picker (no active file) or editor (active file).
        if self.personality_active_file.is_some() {
            return self.handle_personality_editor(key, term).await;
        }

        // Tab navigation still works on composite tabs.
        use crate::keymap::ConfigTabAction;
        let action = ConfigTabAction::from_chord(&key);
        match action {
            Some(ConfigTabAction::TabLeft) if self.active_tab > 0 => {
                self.active_tab = self.active_tab.saturating_sub(1);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            Some(ConfigTabAction::TabRight) if self.active_tab + 1 < self.tab_names.len() => {
                self.active_tab += 1;
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => {
                // Back to alias list (reuse the normal Esc logic).
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                    && breadcrumb.len() >= 2
                {
                    let mut bc = breadcrumb;
                    bc.pop();
                    let section_key = &self.sections[section_idx].key;
                    let map_path = if bc.len() == 1 {
                        section_key.clone()
                    } else {
                        format!("{}.{}", section_key, bc[1..].join("."))
                    };
                    self.load_aliases(&map_path).await?;
                    self.screen = Screen::AliasList {
                        section_idx,
                        map_path,
                        breadcrumb: bc,
                    };
                }
                self.status_msg = None;
                return Ok(());
            }
            Some(ConfigTabAction::Up) => {
                self.personality_cursor = self.personality_cursor.saturating_sub(1);
            }
            Some(ConfigTabAction::Down)
                if self.personality_cursor + 1 < self.personality_files.len() =>
            {
                self.personality_cursor += 1;
            }
            Some(ConfigTabAction::Enter) => {
                if let Some(file) = self.personality_files.get(self.personality_cursor) {
                    let filename = file.filename.clone();
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-personality-loading-file",
                        &[("filename", &filename)],
                    ));
                    let _ = self.draw(term);
                    match self.load_personality_file(&filename).await {
                        Ok(()) => {
                            // Try $EDITOR first; fall back to inline editor.
                            match edit_in_external_editor(
                                term,
                                &self.personality_content,
                                &filename,
                            ) {
                                Ok(edited) => {
                                    self.personality_content = edited;
                                    if self.personality_content != self.personality_loaded {
                                        // Auto-save after $EDITOR.
                                        let agent = self.personality_agent.clone();
                                        let content = self.personality_content.clone();
                                        match self
                                            .rpc
                                            .personality_put(&agent, &filename, &content)
                                            .await
                                        {
                                            Ok(_) => {
                                                self.personality_loaded =
                                                    self.personality_content.clone();
                                                self.status_msg = Some(crate::i18n::t_args(
                                                    "zc-config-status-personality-saved-file",
                                                    &[("filename", &filename)],
                                                ));
                                                let _ = self.load_personality_files().await;
                                            }
                                            Err(e) => {
                                                self.status_msg = Some(crate::i18n::t_args(
                                                    "zc-config-status-save-failed",
                                                    &[("err", &e.to_string())],
                                                ));
                                            }
                                        }
                                    } else {
                                        self.status_msg = None;
                                    }
                                    self.personality_active_file = None;
                                }
                                Err(_) => {
                                    self.status_msg = None;
                                }
                            }
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-load-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            Some(ConfigTabAction::ApplyTemplate) => {
                // Fill selected file from default template.
                if let Some(file) = self.personality_files.get(self.personality_cursor) {
                    let filename = file.filename.clone();
                    let agent = self.personality_agent.clone();
                    self.status_msg = Some(crate::i18n::t("zc-config-status-fetching-templates"));
                    let _ = self.draw(term);
                    match self.rpc.personality_templates(Some(&agent)).await {
                        Ok(result) => {
                            if let Some(tmpl) = result.files.iter().find(|f| f.filename == filename)
                            {
                                self.personality_content = tmpl.content.clone();
                                self.personality_loaded.clear();
                                self.personality_active_file = Some(filename.clone());

                                // Try $EDITOR, fall back to inline.
                                match edit_in_external_editor(
                                    term,
                                    &self.personality_content,
                                    &filename,
                                ) {
                                    Ok(edited) => {
                                        self.personality_content = edited;
                                        if !self.personality_content.is_empty() {
                                            let content = self.personality_content.clone();
                                            match self
                                                .rpc
                                                .personality_put(&agent, &filename, &content)
                                                .await
                                            {
                                                Ok(_) => {
                                                    self.personality_loaded =
                                                        self.personality_content.clone();
                                                    self.status_msg =
                                                        Some(format!("Saved {filename}"));
                                                    let _ = self.load_personality_files().await;
                                                }
                                                Err(e) => {
                                                    self.status_msg =
                                                        Some(format!("Save failed: {e}"));
                                                }
                                            }
                                        } else {
                                            self.status_msg = None;
                                        }
                                        self.personality_active_file = None;
                                    }
                                    Err(_) => {
                                        self.status_msg = Some(crate::i18n::t_args(
                                            "zc-config-status-template-loaded",
                                            &[("filename", &filename)],
                                        ));
                                    }
                                }
                            } else {
                                self.status_msg = Some(crate::i18n::t_args(
                                    "zc-config-status-template-missing",
                                    &[("filename", &filename)],
                                ));
                            }
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-template-fetch-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_personality_editor(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        use crate::keymap::ConfigEditorAction;
        let action = ConfigEditorAction::from_chord(&key);
        match action {
            Some(ConfigEditorAction::Cancel) => {
                // Back to file picker. Warn if dirty.
                if self.personality_content != self.personality_loaded {
                    self.status_msg = Some(crate::i18n::t("zc-config-status-unsaved-discarded"));
                }
                self.personality_active_file = None;
            }
            Some(ConfigEditorAction::Save) => {
                if let Some(filename) = &self.personality_active_file {
                    let filename = filename.clone();
                    let agent = self.personality_agent.clone();
                    let content = self.personality_content.clone();
                    if content.chars().count() > self.personality_max_chars {
                        self.status_msg = Some(crate::i18n::t_args(
                            "zc-config-personality-over-limit",
                            &[("limit", &self.personality_max_chars.to_string())],
                        ));
                        return Ok(());
                    }
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-personality-saving-file",
                        &[("filename", &filename)],
                    ));
                    let _ = self.draw(term);
                    match self.rpc.personality_put(&agent, &filename, &content).await {
                        Ok(_) => {
                            self.personality_loaded = self.personality_content.clone();
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-personality-saved-file",
                                &[("filename", &filename)],
                            ));
                            let _ = self.load_personality_files().await;
                            self.personality_active_file = Some(filename);
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-save-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            Some(ConfigEditorAction::Confirm) => {
                self.personality_content.push('\n');
            }
            Some(ConfigEditorAction::Backspace) => {
                self.personality_content.pop();
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.personality_content.push(c);
                }
            }
        }
        Ok(())
    }

    // ── Skills tab handler ───────────────────────────────────────

    async fn handle_skills_tab(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        // Two modes: skill picker (no active skill) or editor (active skill).
        if self.skills_active.is_some() {
            return self.handle_skills_editor(key, term).await;
        }

        use crate::keymap::ConfigTabAction;
        let action = ConfigTabAction::from_chord(&key);
        match action {
            Some(ConfigTabAction::TabLeft) if self.active_tab > 0 => {
                self.active_tab = self.active_tab.saturating_sub(1);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            Some(ConfigTabAction::TabRight) => {
                if self.active_tab + 1 < self.tab_names.len() {
                    self.active_tab += 1;
                }
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                    && breadcrumb.len() >= 2
                {
                    let mut bc = breadcrumb;
                    bc.pop();
                    let section_key = &self.sections[section_idx].key;
                    let map_path = if bc.len() == 1 {
                        section_key.clone()
                    } else {
                        format!("{}.{}", section_key, bc[1..].join("."))
                    };
                    self.load_aliases(&map_path).await?;
                    self.screen = Screen::AliasList {
                        section_idx,
                        map_path,
                        breadcrumb: bc,
                    };
                }
                self.status_msg = None;
                return Ok(());
            }
            Some(ConfigTabAction::Up) => {
                self.skills_cursor = self.skills_cursor.saturating_sub(1);
            }
            Some(ConfigTabAction::Down) if self.skills_cursor + 1 < self.skills_list.len() => {
                self.skills_cursor += 1;
            }
            Some(ConfigTabAction::Enter) => {
                if let Some(skill) = self.skills_list.get(self.skills_cursor) {
                    let name = skill.name.clone();
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-skill-loading",
                        &[("name", &name)],
                    ));
                    let _ = self.draw(term);
                    match self.load_skill(&name).await {
                        Ok(()) => {
                            let hint = format!("{name}.SKILL.md");
                            match edit_in_external_editor(term, &self.skills_body, &hint) {
                                Ok(edited) => {
                                    self.skills_body = edited;
                                    if self.skills_body != self.skills_body_loaded {
                                        let bundle = self.skills_bundle.clone();
                                        let fm = self.skills_frontmatter.clone();
                                        let body = self.skills_body.clone();
                                        match self
                                            .rpc
                                            .skills_write(&bundle, &name, &fm, &body)
                                            .await
                                        {
                                            Ok(_) => {
                                                self.skills_body_loaded = self.skills_body.clone();
                                                self.status_msg = Some(crate::i18n::t_args(
                                                    "zc-config-status-skill-saved",
                                                    &[("name", &name)],
                                                ));
                                            }
                                            Err(e) => {
                                                self.status_msg = Some(crate::i18n::t_args(
                                                    "zc-config-status-save-failed",
                                                    &[("err", &e.to_string())],
                                                ));
                                            }
                                        }
                                    } else {
                                        self.status_msg = None;
                                    }
                                    self.skills_active = None;
                                }
                                Err(_) => {
                                    self.status_msg = None;
                                    // $EDITOR unavailable — falls into inline editor.
                                }
                            }
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-load-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            Some(ConfigTabAction::ToggleSecret) => {
                if let Some(skill) = self.skills_list.get(self.skills_cursor) {
                    let name = skill.name.clone();
                    let bundle = self.skills_bundle.clone();
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-skill-deleting",
                        &[("name", &name)],
                    ));
                    let _ = self.draw(term);
                    match self.rpc.skills_delete(&bundle, &name).await {
                        Ok(_) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-skill-archived",
                                &[("name", &name)],
                            ));
                            let _ = self.load_skills_list().await;
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-delete-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_skills_editor(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        use crate::keymap::ConfigEditorAction;
        let action = ConfigEditorAction::from_chord(&key);
        match action {
            Some(ConfigEditorAction::Cancel) => {
                if self.skills_body != self.skills_body_loaded {
                    self.status_msg = Some(crate::i18n::t("zc-config-status-unsaved-discarded"));
                }
                self.skills_active = None;
            }
            Some(ConfigEditorAction::Save) => {
                if let Some(name) = &self.skills_active {
                    let name = name.clone();
                    let bundle = self.skills_bundle.clone();
                    let frontmatter = self.skills_frontmatter.clone();
                    let body = self.skills_body.clone();
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-skill-saving",
                        &[("name", &name)],
                    ));
                    let _ = self.draw(term);
                    match self
                        .rpc
                        .skills_write(&bundle, &name, &frontmatter, &body)
                        .await
                    {
                        Ok(_) => {
                            self.skills_body_loaded = self.skills_body.clone();
                            self.skills_frontmatter_loaded = self.skills_frontmatter.clone();
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-skill-saved",
                                &[("name", &name)],
                            ));
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-save-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            Some(ConfigEditorAction::Confirm) => {
                self.skills_body.push('\n');
            }
            Some(ConfigEditorAction::Backspace) => {
                self.skills_body.pop();
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.skills_body.push(c);
                }
            }
        }
        Ok(())
    }

    async fn enter_field_edit(&mut self, idx: usize, term: &mut Term) {
        self.prepare_edit_at(idx);

        // Model field inside a provider alias → fetch available models.
        let field_path = self.fields[idx].path.clone();
        let field_current = self.fields[idx]
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if field_path.ends_with(".model") && field_path.starts_with("providers.models.") {
            // providers.models.<family>.<alias>.model → segment at index 2
            let segments: Vec<&str> = field_path.split('.').collect();
            if segments.len() >= 4 {
                let family = segments[2].to_string();

                // Show loading indicator before the blocking RPC call.
                self.status_msg = Some(crate::i18n::t_args(
                    "zc-config-status-fetching-models",
                    &[("family", &family)],
                ));
                let _ = self.draw(term);

                match self.rpc.catalog_models(&family).await {
                    Ok(res) if !res.models.is_empty() => {
                        self.select_cursor = res
                            .models
                            .iter()
                            .position(|m| m == &field_current)
                            .unwrap_or(0);
                        self.select_items = res.models;
                        self.status_msg = None;
                    }
                    Ok(_) => {
                        self.status_msg = Some(crate::i18n::t("zc-config-status-no-models"));
                    }
                    Err(_) => {
                        self.status_msg =
                            Some(crate::i18n::t("zc-config-status-model-fetch-failed"));
                    }
                }
            }
        }

        // Alias-reference fields resolve their picker list generically from
        // the field's `alias_source` — no per-path special-casing.
        if let Some(source) = self.fields[idx].alias_source {
            self.status_msg = Some(crate::i18n::t("zc-config-status-loading-aliases"));
            let _ = self.draw(term);
            match self.rpc.config_resolve_alias_source(source).await {
                Ok(values) if !values.is_empty() => {
                    self.select_cursor =
                        values.iter().position(|v| v == &field_current).unwrap_or(0);
                    self.select_items = values;
                    self.status_msg = None;
                }
                Ok(_) => {
                    self.status_msg = Some(crate::i18n::t("zc-config-status-no-aliases"));
                }
                Err(_) => {
                    self.status_msg = Some(crate::i18n::t("zc-config-status-alias-fetch-failed"));
                }
            }
        }

        if let Screen::FieldList {
            section_idx,
            prefix,
            breadcrumb,
            ..
        } = &self.screen
        {
            self.screen = Screen::FieldEdit {
                section_idx: *section_idx,
                prefix: prefix.clone(),
                breadcrumb: breadcrumb.clone(),
                field_idx: idx,
            };
        }
    }

    fn prepare_edit_at(&mut self, idx: usize) {
        let kind = self.fields[idx].kind;
        let value = self.fields[idx]
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let variants = self.fields[idx].enum_variants.clone();

        match kind {
            PropKind::Bool => {
                self.select_items = vec!["true".into(), "false".into()];
                self.select_cursor = match value.as_deref() {
                    Some("true") => 0,
                    Some("false") => 1,
                    _ => 0,
                };
            }
            PropKind::Enum => {
                self.select_items = variants;
                let current = value.as_deref().unwrap_or("");
                self.select_cursor = self
                    .select_items
                    .iter()
                    .position(|v| v == current)
                    .unwrap_or(0);
            }
            PropKind::StringArray => {
                // Deserialize the JSON array into one entry-per-line for editing.
                self.select_items.clear();
                let raw = value.unwrap_or_default();
                let entries: Vec<String> =
                    serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default();
                self.edit_buf = entries.join("\n");
            }
            _ => {
                self.select_items.clear();
                self.edit_buf = value.unwrap_or_default();
            }
        }
    }

    fn is_select_edit(&self) -> bool {
        !self.select_items.is_empty()
    }

    // ── Filter helpers ───────────────────────────────────────────

    fn activate_filter(&mut self) {
        self.filter = Some(String::new());
        self.filter_cursor = 0;
    }

    fn deactivate_filter(&mut self) {
        self.filter = None;
    }

    fn filtered_indices<S: AsRef<str>>(&self, items: &[S]) -> Vec<usize> {
        match &self.filter {
            None => (0..items.len()).collect(),
            Some(buf) if buf.is_empty() => (0..items.len()).collect(),
            Some(buf) => {
                let needle = buf.to_lowercase();
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.as_ref().to_lowercase().contains(&needle))
                    .map(|(i, _)| i)
                    .collect()
            }
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent, filtered_len: usize) -> FilterAction {
        use crate::keymap::Chord;
        if self.filter.is_none() {
            return match key.code {
                KeyCode::Char('/') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.activate_filter();
                    FilterAction::Consumed
                }
                _ => FilterAction::Passthrough,
            };
        }
        let editor_chord = if Chord::key(KeyCode::Esc).matches(&key) {
            Some(FilterEditAction::Cancel)
        } else if Chord::key(KeyCode::Enter).matches(&key) {
            Some(FilterEditAction::Accept)
        } else if Chord::key(KeyCode::Backspace).matches(&key) {
            Some(FilterEditAction::Backspace)
        } else if Chord::key(KeyCode::Up).matches(&key) {
            Some(FilterEditAction::CursorUp)
        } else if Chord::key(KeyCode::Down).matches(&key) {
            Some(FilterEditAction::CursorDown)
        } else {
            None
        };
        match editor_chord {
            Some(FilterEditAction::Cancel) => {
                self.deactivate_filter();
                FilterAction::Consumed
            }
            Some(FilterEditAction::Accept) => FilterAction::Accept,
            Some(FilterEditAction::Backspace) => {
                if let Some(buf) = &mut self.filter {
                    buf.pop();
                    if self.filter_cursor >= filtered_len {
                        self.filter_cursor = filtered_len.saturating_sub(1);
                    }
                }
                FilterAction::Consumed
            }
            Some(FilterEditAction::CursorUp) => {
                self.filter_cursor = self.filter_cursor.saturating_sub(1);
                FilterAction::Consumed
            }
            Some(FilterEditAction::CursorDown) => {
                if self.filter_cursor + 1 < filtered_len {
                    self.filter_cursor += 1;
                }
                FilterAction::Consumed
            }
            None => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && let Some(buf) = &mut self.filter
                {
                    buf.push(c);
                    self.filter_cursor = 0;
                }
                FilterAction::Consumed
            }
        }
    }

    // ── Field edit ───────────────────────────────────────────────

    async fn handle_field_edit(&mut self, key: KeyEvent) -> Result<()> {
        if self.is_select_edit() {
            return self.handle_select_edit(key).await;
        }
        // For StringArray fields, Enter adds a new line (new entry), Ctrl+S saves.
        let is_string_array = matches!(&self.screen, Screen::FieldEdit { field_idx, .. }
            if self.fields[*field_idx].kind == PropKind::StringArray);

        use crate::keymap::ConfigEditorAction;
        let action = ConfigEditorAction::from_chord(&key);

        if is_string_array {
            match action {
                Some(ConfigEditorAction::Cancel) => {
                    self.pop_to_field_list().await?;
                }
                Some(ConfigEditorAction::Confirm) => {
                    self.edit_buf.push('\n');
                }
                Some(ConfigEditorAction::Backspace) => {
                    self.edit_buf.pop();
                }
                Some(ConfigEditorAction::Save) => {
                    if let Screen::FieldEdit {
                        prefix, field_idx, ..
                    } = &self.screen
                    {
                        let prop = self.fields[*field_idx].path.clone();
                        let prefix = prefix.clone();
                        let entries: Vec<String> = self
                            .edit_buf
                            .lines()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        let value = serde_json::Value::Array(
                            entries.into_iter().map(serde_json::Value::String).collect(),
                        );
                        match self.rpc.config_set(&prop, value).await {
                            Ok(()) => {
                                self.status_msg = Some(crate::i18n::t_args(
                                    "zc-config-status-field-set",
                                    &[("prop", &prop)],
                                ));
                                self.load_fields(&prefix).await?;
                                self.pop_to_field_list_keep_cursor().await?;
                            }
                            Err(e) => {
                                self.status_msg = Some(crate::i18n::t_args(
                                    "zc-config-status-set-failed",
                                    &[("err", &e.to_string())],
                                ));
                            }
                        }
                    }
                }
                _ => {
                    if let KeyCode::Char(c) = key.code
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.edit_buf.push(c);
                    }
                }
            }
            return Ok(());
        }

        match action {
            Some(ConfigEditorAction::Cancel) => {
                self.pop_to_field_list().await?;
            }
            Some(ConfigEditorAction::Confirm) => {
                if let Screen::FieldEdit {
                    prefix, field_idx, ..
                } = &self.screen
                {
                    let field = &self.fields[*field_idx];
                    let prop = field.path.clone();
                    let value = serde_json::Value::String(self.edit_buf.clone());
                    let prefix = prefix.clone();
                    match self.rpc.config_set(&prop, value).await {
                        Ok(()) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-field-set",
                                &[("prop", &prop)],
                            ));
                            self.load_fields(&prefix).await?;
                            self.pop_to_field_list_keep_cursor().await?;
                        }
                        Err(e) => {
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-set-failed",
                                &[("err", &e.to_string())],
                            ));
                        }
                    }
                }
            }
            Some(ConfigEditorAction::Backspace) => {
                self.edit_buf.pop();
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.edit_buf.push(c);
                }
            }
        }
        Ok(())
    }

    async fn handle_select_edit(&mut self, key: KeyEvent) -> Result<()> {
        let visible = self.filtered_indices(&self.select_items);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.deactivate_filter();
                    return self.commit_select(orig).await;
                }
                return Ok(());
            }
            FilterAction::Passthrough => {}
        }

        use crate::keymap::ConfigTabAction;
        let action = ConfigTabAction::from_chord(&key);
        match action {
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => {
                self.deactivate_filter();
                self.pop_to_field_list().await?;
            }
            Some(ConfigTabAction::Up) => {
                self.select_cursor = self.select_cursor.saturating_sub(1);
            }
            Some(ConfigTabAction::Down) if self.select_cursor + 1 < visible.len() => {
                self.select_cursor += 1;
            }
            Some(ConfigTabAction::Enter) => {
                if let Some(&orig) = visible.get(self.select_cursor) {
                    return self.commit_select(orig).await;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn commit_select(&mut self, orig_idx: usize) -> Result<()> {
        if let Some(chosen) = self.select_items.get(orig_idx)
            && let Screen::FieldEdit {
                prefix, field_idx, ..
            } = &self.screen
        {
            let prop = self.fields[*field_idx].path.clone();
            let value = serde_json::Value::String(chosen.clone());
            let prefix = prefix.clone();
            match self.rpc.config_set(&prop, value).await {
                Ok(()) => {
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-field-set",
                        &[("prop", &prop)],
                    ));
                    self.load_fields(&prefix).await?;
                    self.pop_to_field_list_keep_cursor().await?;
                }
                Err(e) => {
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-set-failed",
                        &[("err", &e.to_string())],
                    ));
                }
            }
        }
        Ok(())
    }

    async fn pop_to_field_list(&mut self) -> Result<()> {
        if let Screen::FieldEdit {
            section_idx,
            prefix,
            breadcrumb,
            ..
        } = std::mem::replace(&mut self.screen, Screen::SectionList)
        {
            // Silent refresh so values reflect any saves while editing.
            self.reload_fields_silent(&prefix).await;
            self.screen = Screen::FieldList {
                section_idx,
                prefix,
                breadcrumb,
            };
        }
        Ok(())
    }

    async fn pop_to_field_list_keep_cursor(&mut self) -> Result<()> {
        if let Screen::FieldEdit {
            section_idx,
            prefix,
            breadcrumb,
            field_idx,
        } = std::mem::replace(&mut self.screen, Screen::SectionList)
        {
            // Silent refresh — preserves cursor below.
            self.reload_fields_silent(&prefix).await;
            self.field_cursor = field_idx.min(self.fields.len().saturating_sub(1));
            self.screen = Screen::FieldList {
                section_idx,
                prefix,
                breadcrumb,
            };
        }
        Ok(())
    }

    // ── Drawing ──────────────────────────────────────────────────

    fn draw(&mut self, term: &mut Term) -> Result<()> {
        term.draw(|frame| {
            let area = frame.area();
            self.draw_into(frame, area);
        })?;
        Ok(())
    }

    /// Persistent left pane: the section list. `active` is true while the
    /// SectionList screen holds focus (bright highlight); once a section is
    /// entered the list dims to a "you are here" marker.
    /// Display rank of a section-group label. Mirror of
    /// `zeroclaw_config::sections::SECTION_GROUPS` — zerocode talks to
    /// remote daemons over the wire, so like the dashboard's
    /// `GROUP_ORDER` (web/src/pages/Config.tsx) it carries its own copy
    /// of the order instead of linking the config crate. Unknown and
    /// empty labels rank with "Other" so nothing ever vanishes.
    fn group_rank(label: &str) -> usize {
        const ORDER: &[&str] = &[
            "Foundation",
            "Agent",
            "Multi-agent",
            "Tools",
            "Integrations",
            "Network",
            "Storage",
            "Operations",
            "Other",
        ];
        ORDER
            .iter()
            .position(|g| *g == label)
            .unwrap_or(ORDER.len() - 1)
    }

    fn draw_sections_pane(&mut self, frame: &mut Frame, area: Rect, active: bool) {
        use ratatui::layout::{Constraint, Direction, Layout};
        // Reserve one line at the top for the filter bar when filtering.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        if let Some(buf) = &self.filter {
            render_filter_bar(frame, rows[0], buf);
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("ZeroClaw v{}", self.rpc.server_version),
                    theme::dim_style(),
                )),
                rows[0],
            );
        }
        let list_area = rows[1];

        let labels: Vec<String> = self.sections.iter().map(|s| s.label.clone()).collect();
        let visible = self.filtered_indices(&labels);

        // Grouped display: dim header rows between groups, sections
        // beneath. Active only when the daemon sent group labels and no
        // filter narrows the list — filtering and old daemons render the
        // flat all-sections list unchanged. `row_map` records what each
        // display row is so the cursor and mouse hit-testing resolve
        // through it instead of assuming row == section position.
        let grouped = self.filter.is_none() && self.sections.iter().any(|s| !s.group.is_empty());
        let mut row_map: Vec<Option<usize>> = Vec::with_capacity(visible.len());
        let mut items: Vec<ListItem> = Vec::with_capacity(visible.len());
        let mut last_group: Option<&str> = None;
        for &i in &visible {
            let s = &self.sections[i];
            if grouped {
                let group = if s.group.is_empty() {
                    "Other"
                } else {
                    s.group.as_str()
                };
                if last_group != Some(group) {
                    items.push(ListItem::new(Line::from(Span::styled(
                        group.to_string(),
                        theme::dim_style().add_modifier(Modifier::BOLD),
                    ))));
                    row_map.push(None);
                    last_group = Some(group);
                }
            }
            let badge = if s.completed { " ✓" } else { "" };
            let indent = if grouped { " " } else { "" };
            items.push(ListItem::new(Line::from(Span::styled(
                format!("{indent}{}{badge}", s.label),
                theme::body_style(),
            ))));
            row_map.push(Some(i));
        }

        let cursor = if self.filter.is_some() {
            self.filter_cursor
        } else {
            row_map
                .iter()
                .position(|r| *r == Some(self.section_cursor))
                .unwrap_or(0)
        };

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(cursor.min(items.len().saturating_sub(1))));
        }

        let (style, symbol) = if active {
            (theme::selected_style(), "› ")
        } else {
            (theme::selected_inactive_style(), "  ")
        };

        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" Sections "))
                .highlight_style(style)
                .highlight_symbol(symbol),
            list_area,
            &mut state,
        );
        self.last_main_area = list_area;
        self.last_section_list_area = list_area;
        self.last_section_rows = row_map;
        self.last_list_offset = state.offset();
        self.last_section_list_offset = state.offset();
        self.last_tab_area = None;
    }

    /// Right pane shown while focus is on the section list: the highlighted
    /// section's description, so the content updates as the cursor moves — and a
    /// trailing line with the inward chords resolved from the keymap.
    fn draw_section_detail_hint(&self, frame: &mut Frame, area: Rect) {
        use ratatui::layout::{Constraint, Direction, Layout};
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        let help = self
            .sections
            .get(self.section_cursor)
            .map(|s| s.help.clone())
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Span::styled(help, theme::body_style()))
                .wrap(ratatui::widgets::Wrap { trim: true })
                .block(theme::panel_block(" ")),
            rows[0],
        );

        let line = crate::i18n::t_args(
            "zc-config-section-detail-hint",
            &[
                ("open", &tab_key(crate::keymap::ConfigTabAction::Enter)),
                ("into", &tab_key(crate::keymap::ConfigTabAction::TabRight)),
            ],
        );
        frame.render_widget(
            Paragraph::new(Span::styled(line, theme::dim_style())),
            rows[1],
        );
    }

    fn draw_type_list(&mut self, frame: &mut Frame, area: Rect, section_idx: usize) {
        let r = regions(area);
        let section = &self.sections[section_idx];

        render_breadcrumb(frame, r.breadcrumb, std::slice::from_ref(&section.label));

        if let Some(buf) = &self.filter {
            render_filter_bar(frame, r.help, buf);
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(&section.help, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
        }

        let type_names: Vec<String> = self
            .types
            .iter()
            .map(|t| t.path.rsplit('.').next().unwrap_or(&t.path).to_string())
            .collect();
        let visible = self.filtered_indices(&type_names);

        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let name = &type_names[i];
                let count = self.type_alias_counts.get(i).copied().unwrap_or(0);
                let mut spans = vec![Span::styled(name.to_string(), theme::body_style())];
                if count > 0 {
                    spans.push(Span::styled(format!("  ({count})"), theme::accent_style()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let cursor = if self.filter.is_some() {
            self.filter_cursor
        } else {
            visible
                .iter()
                .position(|&i| i == self.type_cursor)
                .unwrap_or(0)
        };

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(cursor.min(items.len().saturating_sub(1))));
        }

        let (dstyle, dsym) = self.detail_highlight();
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&format!(" {} ", section.label)))
                .highlight_style(dstyle)
                .highlight_symbol(dsym),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = None;

        let hints = if self.filter.is_some() {
            format!(
                "{}  {}=open  {}=clear filter",
                nav_keys(),
                tab_key(crate::keymap::ConfigTabAction::Enter),
                tab_key(crate::keymap::ConfigTabAction::Back),
            )
        } else {
            "?=help".to_string()
        };
        self.draw_footer(frame, r, &hints);
    }

    fn draw_alias_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        section_idx: usize,
        breadcrumb: &[String],
    ) {
        let mut r = regions(area);
        let section = &self.sections[section_idx];

        let mut bc: Vec<String> = Vec::new();
        bc.extend(breadcrumb.iter().cloned());
        render_breadcrumb(frame, r.breadcrumb, &bc);

        // Aliases/Costs tab bar on cost-bearing provider types. Reuses the
        // same two-row help split the FieldList tab bar uses.
        let has_tabs = self.alias_list_has_tabs();
        let tab_area = if has_tabs {
            use ratatui::layout::{Constraint, Direction, Layout};
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(r.help);
            r.help = split[1];
            Some(split[0])
        } else {
            None
        };
        if let Some(tab_rect) = tab_area {
            let labels = [ConfigTab::Aliases.label(), ConfigTab::Costs.label()];
            let mut spans = Vec::new();
            for (i, label) in labels.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", theme::dim_style()));
                }
                if i == self.alias_tab {
                    spans.push(Span::styled(
                        format!("▸ {label}"),
                        theme::accent_style().add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(*label, theme::dim_style()));
                }
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), tab_rect);
        }

        if has_tabs && self.alias_tab == 1 {
            self.draw_cost_resource_list(frame, r, tab_area);
            return;
        }

        if let Some(buf) = &self.filter {
            render_filter_bar(frame, r.help, buf);
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(&section.help, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
        }

        let visible = self.filtered_indices(&self.aliases);

        let mut items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let a = &self.aliases[i];
                let mut spans = vec![Span::styled(a.clone(), theme::body_style())];
                match self.alias_enabled.get(i).copied().flatten() {
                    Some(true) => spans.push(Span::styled("  ✓", theme::accent_style())),
                    Some(false) => spans.push(Span::styled("  disabled", theme::dim_style())),
                    None => {}
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        // Only show [+ Add] when not filtering
        if self.filter.is_none() {
            items.push(ListItem::new(Line::from(Span::styled(
                "[+ Add]",
                theme::accent_style(),
            ))));
        }

        let cursor = if self.filter.is_some() {
            self.filter_cursor
        } else {
            self.alias_cursor
        };

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(cursor.min(items.len().saturating_sub(1))));
        }

        let (dstyle, dsym) = self.detail_highlight();
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" Aliases "))
                .highlight_style(dstyle)
                .highlight_symbol(dsym),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = tab_area;

        let hints = if self.filter.is_some() {
            format!(
                "{}  {}=open  {}=clear filter",
                nav_keys(),
                tab_key(crate::keymap::ConfigTabAction::Enter),
                tab_key(crate::keymap::ConfigTabAction::Back),
            )
        } else {
            "?=help".to_string()
        };
        self.draw_footer(frame, r, &hints);
    }

    /// Costs-tab body: the resource rate sheets under
    /// `cost.rates.providers.<category>.<type>`, with an [+ Add] affordance.
    fn draw_cost_resource_list(&mut self, frame: &mut Frame, r: Regions, tab_area: Option<Rect>) {
        let base = self.cost_base_path().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("{base}.<resource>"),
                theme::dim_style(),
            )),
            r.help,
        );

        let mut items: Vec<ListItem> = self
            .cost_resources
            .iter()
            .map(|res| ListItem::new(Line::from(Span::styled(res.clone(), theme::body_style()))))
            .collect();
        items.push(ListItem::new(Line::from(Span::styled(
            "[+ Add]",
            theme::accent_style(),
        ))));

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.cost_cursor.min(items.len().saturating_sub(1))));
        }

        let (dstyle, dsym) = self.detail_highlight();
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&format!(
                    " {} ",
                    ConfigTab::Costs.label()
                )))
                .highlight_style(dstyle)
                .highlight_symbol(dsym),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = tab_area;
        self.draw_footer(frame, r, "?=help");
    }

    fn draw_alias_create(&mut self, frame: &mut Frame, area: Rect, breadcrumb: &[String]) {
        let r = regions(area);

        let mut bc: Vec<String> = Vec::new();
        bc.extend(breadcrumb.iter().cloned());
        bc.push(crate::i18n::t("zc-config-breadcrumb-new"));
        render_breadcrumb(frame, r.breadcrumb, &bc);

        frame.render_widget(
            Paragraph::new(Span::styled(
                crate::i18n::t("zc-config-alias-create-hint"),
                theme::dim_style(),
            )),
            r.help,
        );

        let input_display = format!("{}{}", self.edit_buf, "█");
        let input = Paragraph::new(Line::from(Span::styled(
            input_display,
            theme::input_style(),
        )))
        .block(theme::panel_block(" Alias name "));
        frame.render_widget(input, r.main);

        let footer = format!(
            "{}={}  {}={}",
            editor_key(crate::keymap::ConfigEditorAction::Confirm),
            crate::i18n::t("zc-config-footer-action-create"),
            editor_key(crate::keymap::ConfigEditorAction::Cancel),
            crate::i18n::t("zc-config-footer-action-cancel"),
        );
        self.draw_footer(frame, r, &footer);
    }

    fn draw_field_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        _section_idx: usize,
        breadcrumb: &[String],
    ) {
        let has_tabs = !self.tab_names.is_empty();

        // Breadcrumb first, then optional tab bar, then the rest.
        let mut r = regions(area);

        let mut bc: Vec<String> = Vec::new();
        bc.extend(breadcrumb.iter().cloned());
        render_breadcrumb(frame, r.breadcrumb, &bc);

        // When tabs are present, split the help row into tab bar + help.
        // The help area is 2 rows: use the first for tabs, second for help.
        let tab_area = if has_tabs {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(r.help);
            r.help = split[1];
            Some(split[0])
        } else {
            None
        };

        // Tab bar
        if let Some(tab_rect) = tab_area {
            let mut spans = Vec::new();
            for (i, name) in self.tab_names.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", theme::dim_style()));
                }
                let label = name.label();
                if i == self.active_tab {
                    spans.push(Span::styled(
                        format!("▸ {label}"),
                        theme::accent_style().add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(label, theme::dim_style()));
                }
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), tab_rect);
        }

        // Composite tabs get custom rendering.
        if self.is_composite_tab() {
            self.last_tab_area = tab_area;
            match self.tab_names[self.active_tab] {
                ConfigTab::Personality => {
                    self.draw_personality_tab(frame, r);
                    return;
                }
                ConfigTab::Skills => {
                    self.draw_skills_tab(frame, r);
                    return;
                }
                _ => {}
            }
        }

        // Fields visible under active tab, then filtered by `/` query.
        let tab_indices = self.tab_field_indices();
        let tab_names = self.field_labels_for_tab(&tab_indices);
        let filter_vis = self.filtered_indices(&tab_names);
        let visible: Vec<usize> = filter_vis.iter().map(|&fi| tab_indices[fi]).collect();

        if let Some(buf) = &self.filter {
            render_filter_bar(frame, r.help, buf);
        } else if let Some(field) = self.fields.get(self.field_cursor) {
            frame.render_widget(
                Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
        }

        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let f = &self.fields[i];
                let short_name =
                    &tab_names[tab_indices.iter().position(|&ti| ti == i).unwrap_or(0)];
                let val_display = if f.is_secret {
                    "••••••".to_string()
                } else {
                    f.value
                        .as_ref()
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_else(|| "<unset>".to_string())
                };

                let env_marker = if f.is_env_overridden { " [env]" } else { "" };
                let line = format!("{short_name} = {val_display}{env_marker}");

                let style = if f.populated {
                    theme::body_style()
                } else {
                    theme::dim_style()
                };
                ListItem::new(Line::from(Span::styled(line, style)))
            })
            .collect();

        let cursor = if self.filter.is_some() {
            self.filter_cursor
        } else {
            visible
                .iter()
                .position(|&i| i == self.field_cursor)
                .unwrap_or(0)
        };

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(cursor.min(items.len().saturating_sub(1))));
        }

        let (dstyle, dsym) = self.detail_highlight();
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(" Fields "))
                .highlight_style(dstyle)
                .highlight_symbol(dsym),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = tab_area;

        let hints = if self.filter.is_some() {
            format!(
                "{}  {}=edit  {}=clear filter",
                nav_keys(),
                tab_key(crate::keymap::ConfigTabAction::Enter),
                tab_key(crate::keymap::ConfigTabAction::Back),
            )
        } else {
            "?=help".to_string()
        };
        self.draw_footer(frame, r, &hints);
    }

    // ── Composite tab draw methods ──────────────────────────────

    fn draw_personality_tab(&mut self, frame: &mut Frame, r: Regions) {
        if let Some(filename) = &self.personality_active_file {
            // Editor mode: show file content as editable text.
            let dirty = self.personality_content != self.personality_loaded;
            let char_count = self.personality_content.chars().count();
            let status = format!(
                "{filename}  {char_count}/{} chars{}",
                self.personality_max_chars,
                if dirty { "  [modified]" } else { "" },
            );
            frame.render_widget(
                Paragraph::new(Span::styled(status, theme::dim_style())),
                r.help,
            );

            // Show last ~N lines that fit the area, with a cursor block.
            let height = r.main.height.saturating_sub(2) as usize; // border eats 2
            let lines: Vec<&str> = self.personality_content.split('\n').collect();
            let start = lines.len().saturating_sub(height);
            let mut visible_lines: Vec<Line> = lines[start..]
                .iter()
                .map(|l| Line::from(Span::styled(*l, theme::body_style())))
                .collect();
            // Append cursor to last line.
            if let Some(last) = visible_lines.last_mut() {
                let mut spans = last.spans.clone();
                spans.push(Span::styled("█", theme::input_style()));
                *last = Line::from(spans);
            }

            frame.render_widget(
                Paragraph::new(visible_lines).block(theme::panel_block(&format!(" {filename} "))),
                r.main,
            );

            let footer = format!(
                "{}={}  {}={}",
                editor_key(crate::keymap::ConfigEditorAction::Save),
                crate::i18n::t("zc-config-footer-action-save"),
                editor_key(crate::keymap::ConfigEditorAction::Cancel),
                crate::i18n::t("zc-config-footer-action-back-to-files"),
            );
            self.draw_footer(frame, r, &footer);
        } else {
            // File picker mode.
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-config-personality-help-blurb"),
                    theme::dim_style(),
                ))
                .wrap(Wrap { trim: false }),
                r.help,
            );

            let items: Vec<ListItem> = self
                .personality_files
                .iter()
                .map(|f| {
                    let dot = if f.exists { "●" } else { "○" };
                    let size = if f.exists {
                        format!("  ({} B)", f.size)
                    } else {
                        String::new()
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{dot} "),
                            if f.exists {
                                theme::accent_style()
                            } else {
                                theme::dim_style()
                            },
                        ),
                        Span::styled(f.filename.clone(), theme::body_style()),
                        Span::styled(size, theme::dim_style()),
                    ]))
                })
                .collect();

            let mut state = ListState::default();
            if !items.is_empty() {
                state.select(Some(
                    self.personality_cursor.min(items.len().saturating_sub(1)),
                ));
            }

            frame.render_stateful_widget(
                List::new(items)
                    .block(theme::panel_block(" Personality Files "))
                    .highlight_style(self.detail_highlight().0)
                    .highlight_symbol(self.detail_highlight().1),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();

            let footer = format!("?={}", crate::i18n::t("zc-config-footer-action-help"));
            self.draw_footer(frame, r, &footer);
        }
    }

    fn draw_skills_tab(&mut self, frame: &mut Frame, r: Regions) {
        if let Some(name) = &self.skills_active {
            // Editor mode.
            let dirty = self.skills_body != self.skills_body_loaded;
            let status = format!(
                "{}  {}{}",
                name,
                self.skills_frontmatter.description,
                if dirty { "  [modified]" } else { "" },
            );
            frame.render_widget(
                Paragraph::new(Span::styled(status, theme::dim_style())).wrap(Wrap { trim: false }),
                r.help,
            );

            let height = r.main.height.saturating_sub(2) as usize;
            let lines: Vec<&str> = self.skills_body.split('\n').collect();
            let start = lines.len().saturating_sub(height);
            let mut visible_lines: Vec<Line> = lines[start..]
                .iter()
                .map(|l| Line::from(Span::styled(*l, theme::body_style())))
                .collect();
            if let Some(last) = visible_lines.last_mut() {
                let mut spans = last.spans.clone();
                spans.push(Span::styled("█", theme::input_style()));
                *last = Line::from(spans);
            }

            frame.render_widget(
                Paragraph::new(visible_lines)
                    .block(theme::panel_block(&format!(" SKILL.md — {name} "))),
                r.main,
            );

            let footer = format!(
                "{}={}  {}={}",
                editor_key(crate::keymap::ConfigEditorAction::Save),
                crate::i18n::t("zc-config-footer-action-save"),
                editor_key(crate::keymap::ConfigEditorAction::Cancel),
                crate::i18n::t("zc-config-footer-action-back-to-skills"),
            );
            self.draw_footer(frame, r, &footer);
        } else {
            // Skill picker mode.
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t_args(
                        "zc-config-skills-help-blurb",
                        &[
                            (
                                "enter_chord",
                                &tab_key(crate::keymap::ConfigTabAction::Enter),
                            ),
                            (
                                "archive_chord",
                                &tab_key(crate::keymap::ConfigTabAction::ToggleSecret),
                            ),
                        ],
                    ),
                    theme::dim_style(),
                ))
                .wrap(Wrap { trim: false }),
                r.help,
            );

            let items: Vec<ListItem> = self
                .skills_list
                .iter()
                .map(|s| {
                    ListItem::new(Line::from(Span::styled(
                        s.name.clone(),
                        theme::body_style(),
                    )))
                })
                .collect();

            let mut state = ListState::default();
            if !items.is_empty() {
                state.select(Some(self.skills_cursor.min(items.len().saturating_sub(1))));
            }

            frame.render_stateful_widget(
                List::new(items)
                    .block(theme::panel_block(" Skills "))
                    .highlight_style(self.detail_highlight().0)
                    .highlight_symbol(self.detail_highlight().1),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();

            let footer = format!("?={}", crate::i18n::t("zc-config-footer-action-help"));
            self.draw_footer(frame, r, &footer);
        }
    }

    fn draw_field_edit(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        breadcrumb: &[String],
        field_idx: usize,
    ) {
        let r = regions(area);
        let field = &self.fields[field_idx];
        let short_name = field.path.rsplit('.').next().unwrap_or(&field.path);

        let mut bc: Vec<String> = Vec::new();
        bc.extend(breadcrumb.iter().cloned());
        bc.push(short_name.to_string());
        render_breadcrumb(frame, r.breadcrumb, &bc);

        if self.is_select_edit() {
            // Enum, Bool, or model select — with optional `/` filter.
            if let Some(buf) = &self.filter {
                render_filter_bar(frame, r.help, buf);
            } else {
                frame.render_widget(
                    Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                        .wrap(Wrap { trim: false }),
                    r.help,
                );
            }

            let visible = self.filtered_indices(&self.select_items);
            let items: Vec<ListItem> = visible
                .iter()
                .map(|&i| {
                    ListItem::new(Line::from(Span::styled(
                        self.select_items[i].clone(),
                        theme::body_style(),
                    )))
                })
                .collect();

            let cursor = if self.filter.is_some() {
                self.filter_cursor
            } else {
                self.select_cursor
            };

            let mut state = ListState::default();
            if !items.is_empty() {
                state.select(Some(cursor.min(items.len().saturating_sub(1))));
            }

            let title = match field.kind {
                PropKind::Bool => format!(" {short_name} (toggle) "),
                PropKind::Enum | PropKind::AliasRef => format!(" {short_name} (select) "),
                _ => format!(" {short_name} "),
            };

            frame.render_stateful_widget(
                List::new(items)
                    .block(theme::panel_block(&title))
                    .highlight_style(theme::selected_style())
                    .highlight_symbol("› "),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();
            self.last_tab_area = None;

            let hints = if self.filter.is_some() {
                format!(
                    "{}  {}=save  {}=clear filter",
                    nav_keys(),
                    tab_key(crate::keymap::ConfigTabAction::Enter),
                    tab_key(crate::keymap::ConfigTabAction::Back),
                )
            } else {
                "?=help".to_string()
            };
            self.draw_footer(frame, r, &hints);
        } else {
            // Text input (masked for secrets) — help text always visible.
            frame.render_widget(
                Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
            let type_prefix = crate::i18n::t("zc-config-field-type-prefix");
            let kind_hint = if field.is_secret {
                let suffix = crate::i18n::t("zc-config-field-type-secret-suffix");
                format!("{type_prefix} {} {suffix}", field.kind.wire_name())
            } else if field.kind == PropKind::StringArray {
                let suffix = crate::i18n::t_args(
                    "zc-config-field-type-string-array-suffix",
                    &[("newline_chord", "Enter"), ("save_chord", "Ctrl+S")],
                );
                format!("{type_prefix} {} {suffix}", field.kind.wire_name())
            } else {
                format!("{type_prefix} {}", field.kind.wire_name())
            };

            if field.kind == PropKind::StringArray {
                // Multi-line display: each array entry on its own line.
                let mut lines: Vec<Line> = vec![Line::from(Span::styled(
                    kind_hint.clone(),
                    theme::dim_style(),
                ))];
                let buf_lines: Vec<&str> = self.edit_buf.split('\n').collect();
                for (i, l) in buf_lines.iter().enumerate() {
                    let is_last = i + 1 == buf_lines.len();
                    let text = if is_last {
                        format!("{l}█")
                    } else {
                        l.to_string()
                    };
                    lines.push(Line::from(Span::styled(text, theme::input_style())));
                }
                frame.render_widget(
                    Paragraph::new(lines).block(theme::panel_block(&format!(
                        " {short_name} (string_array) "
                    ))),
                    r.main,
                );
                let footer = format!(
                    "{}={}  {}={}  {}={}",
                    editor_key(crate::keymap::ConfigEditorAction::Confirm),
                    crate::i18n::t("zc-config-footer-action-new-line"),
                    editor_key(crate::keymap::ConfigEditorAction::Save),
                    crate::i18n::t("zc-config-footer-action-save"),
                    editor_key(crate::keymap::ConfigEditorAction::Cancel),
                    crate::i18n::t("zc-config-footer-action-cancel"),
                );
                self.draw_footer(frame, r, &footer);
                return;
            }

            let input_display = if field.is_secret {
                format!("{}█", "•".repeat(self.edit_buf.len()))
            } else {
                format!("{}█", self.edit_buf)
            };

            let input = Paragraph::new(vec![
                Line::from(Span::styled(&kind_hint, theme::dim_style())),
                Line::from(Span::styled(input_display, theme::input_style())),
            ])
            .block(theme::panel_block(&format!(" {short_name} ")));

            frame.render_widget(input, r.main);

            let footer = format!(
                "{}={}  {}={}",
                editor_key(crate::keymap::ConfigEditorAction::Confirm),
                crate::i18n::t("zc-config-footer-action-save"),
                editor_key(crate::keymap::ConfigEditorAction::Cancel),
                crate::i18n::t("zc-config-footer-action-cancel"),
            );
            self.draw_footer(frame, r, &footer);
        }
    }

    fn draw_footer(&self, frame: &mut Frame, r: Regions, _hints: &str) {
        // The help indicator is unified at the pane bottom (draw_into); per-screen
        // hint strings are no longer rendered here. Only the transient status
        // message remains inline.
        if let Some(msg) = &self.status_msg {
            frame.render_widget(
                Paragraph::new(Span::styled(msg.as_str(), theme::warn_style())),
                r.status,
            );
        }
    }

    /// Handle a bracketed-paste payload. Routes pasted text into whichever
    /// text-input surface is currently active (filter, edit buffer, alias
    /// create, personality/skills editor). Filters out the bracket-paste
    /// terminator bytes and normalises CRLF.
    pub(crate) fn handle_paste(&mut self, text: &str) {
        // Normalise line endings — bracketed paste can deliver \r, \r\n,
        // or \n depending on terminal.
        let cleaned: String = text.replace("\r\n", "\n").replace('\r', "\n");

        // Filter active: paste goes into the filter buffer.
        if let Some(buf) = self.filter.as_mut() {
            for c in cleaned.chars() {
                if c == '\n' {
                    continue;
                } // filter is single-line
                buf.push(c);
            }
            return;
        }

        match &self.screen {
            Screen::AliasCreate { .. } => {
                // Aliases are single-line identifiers.
                for c in cleaned.chars() {
                    if c == '\n' {
                        continue;
                    }
                    self.edit_buf.push(c);
                }
            }
            Screen::FieldEdit { field_idx, .. } => {
                if self.is_select_edit() {
                    return; // No text input on select screens.
                }
                let is_string_array = self
                    .fields
                    .get(*field_idx)
                    .map(|f| f.kind == PropKind::StringArray)
                    .unwrap_or(false);
                if is_string_array {
                    // Preserve newlines so each pasted line becomes a new entry.
                    self.edit_buf.push_str(&cleaned);
                } else {
                    // Scalar fields: strip newlines.
                    for c in cleaned.chars() {
                        if c == '\n' {
                            continue;
                        }
                        self.edit_buf.push(c);
                    }
                }
            }
            Screen::FieldList { .. } => {
                if self.personality_active_file.is_some() {
                    self.personality_content.push_str(&cleaned);
                } else if self.skills_active.is_some() {
                    self.skills_body.push_str(&cleaned);
                }
            }
            _ => {}
        }
    }

    /// Whether the pane is in a text-input mode (filter, edit buf, alias create, editors).
    pub(crate) fn wants_text_input(&self) -> bool {
        if self.section == ConfigSection::Zerocode {
            return self.zerocode.wants_text_input();
        }
        if self.filter.is_some() {
            return true;
        }
        match &self.screen {
            Screen::AliasCreate { .. } => true,
            Screen::FieldEdit { .. } if !self.is_select_edit() => true,
            Screen::FieldList { .. } => {
                self.personality_active_file.is_some() || self.skills_active.is_some()
            }
            _ => false,
        }
    }
}

impl crate::widgets::HelpContext for App {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::HelpEntry as E;
        // Section switch is available in either sub-tab.
        let section_nav = E::new(
            vec!["Tab", "Shift+Tab"],
            crate::i18n::t("zc-config-help-switch-section"),
        );
        if self.section == ConfigSection::Zerocode {
            let mut node = self.zerocode.help_context();
            node.entries.insert(0, section_nav);
            return node;
        }
        let mut node = self.zeroclaw_help_context();
        node.entries.insert(0, section_nav);
        node
    }
}

impl App {
    fn zeroclaw_help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::ConfigTabAction as A;
        use crate::widgets::{HelpEntry as E, HelpNode};

        // All chords resolve from the live keymap so overrides/vim/emacs show.
        let nav = || E::new(nav_keys_split(), crate::i18n::t("zc-config-help-navigate"));
        let k = |a: A, label: &str| E::new(tab_keys(a), crate::i18n::t(label));
        let help = || E::key("?", crate::i18n::t("zc-config-help-this-help"));
        let filter = || E::key("/", crate::i18n::t("zc-config-help-filter"));
        let clear_filter = || k(A::Back, "zc-config-help-clear-filter");
        let back = || k(A::Back, "zc-config-help-back");
        let mouse_open = || E::key("Mouse", crate::i18n::t("zc-config-help-mouse-open"));

        match &self.screen {
            Screen::SectionList => {
                if self.filter.is_some() {
                    HelpNode::entries(vec![
                        nav(),
                        k(A::Enter, "zc-config-help-open-section"),
                        clear_filter(),
                        help(),
                    ])
                } else {
                    let open = [tab_keys(A::Enter), tab_keys(A::TabRight)].concat();
                    HelpNode::entries(vec![
                        nav(),
                        E::new(open, crate::i18n::t("zc-config-help-open-section")),
                        filter(),
                        k(A::Back, "zc-config-help-quit"),
                        help(),
                        E::spacer(),
                        mouse_open(),
                    ])
                }
            }
            Screen::TypeList { .. } => {
                if self.filter.is_some() {
                    HelpNode::entries(vec![
                        nav(),
                        k(A::Enter, "zc-config-help-open-type"),
                        clear_filter(),
                        help(),
                    ])
                } else {
                    let open = [tab_keys(A::Enter), tab_keys(A::TabRight)].concat();
                    HelpNode::entries(vec![
                        nav(),
                        E::new(open, crate::i18n::t("zc-config-help-open-type")),
                        filter(),
                        back(),
                        help(),
                        E::spacer(),
                        mouse_open(),
                    ])
                }
            }
            Screen::AliasList { .. } => {
                if self.filter.is_some() {
                    HelpNode::entries(vec![
                        nav(),
                        k(A::Enter, "zc-config-help-open-alias"),
                        clear_filter(),
                        help(),
                    ])
                } else if self.alias_list_has_tabs() {
                    HelpNode::entries(vec![
                        nav(),
                        E::new(
                            switch_tabs_keys(),
                            crate::i18n::t("zc-config-help-switch-tabs"),
                        ),
                        k(A::Enter, "zc-config-help-open-alias"),
                        k(A::ToggleSecret, "zc-config-help-delete-alias"),
                        back(),
                        help(),
                        E::spacer(),
                        mouse_open(),
                    ])
                } else {
                    let open = [tab_keys(A::Enter), tab_keys(A::TabRight)].concat();
                    HelpNode::entries(vec![
                        nav(),
                        E::new(open, crate::i18n::t("zc-config-help-open-alias")),
                        k(A::ToggleSecret, "zc-config-help-delete-alias"),
                        filter(),
                        back(),
                        help(),
                        E::spacer(),
                        mouse_open(),
                    ])
                }
            }
            Screen::AliasCreate { .. } => HelpNode::entries(vec![
                E::new(
                    vec![editor_key(crate::keymap::ConfigEditorAction::Confirm)],
                    crate::i18n::t("zc-config-help-create-alias"),
                ),
                E::new(
                    vec![editor_key(crate::keymap::ConfigEditorAction::Cancel)],
                    crate::i18n::t("zc-config-help-cancel"),
                ),
                help(),
            ]),
            Screen::FieldList { .. } => {
                if self.filter.is_some() {
                    HelpNode::entries(vec![
                        nav(),
                        k(A::Enter, "zc-config-help-edit-field"),
                        clear_filter(),
                        help(),
                    ])
                } else if self.is_composite_tab() {
                    match self.tab_names.get(self.active_tab) {
                        Some(ConfigTab::Personality) => {
                            if self.personality_active_file.is_some() {
                                HelpNode::entries(vec![
                                    E::new(
                                        vec![editor_key(crate::keymap::ConfigEditorAction::Save)],
                                        crate::i18n::t("zc-config-help-save"),
                                    ),
                                    E::new(
                                        vec![editor_key(crate::keymap::ConfigEditorAction::Cancel)],
                                        crate::i18n::t("zc-config-help-back-to-files"),
                                    ),
                                    help(),
                                ])
                            } else {
                                HelpNode::entries(vec![
                                    E::new(
                                        switch_tabs_keys(),
                                        crate::i18n::t("zc-config-help-switch-tabs"),
                                    ),
                                    nav(),
                                    k(A::Enter, "zc-config-help-edit-file"),
                                    k(A::ApplyTemplate, "zc-config-help-fill-from-template"),
                                    back(),
                                    help(),
                                    E::spacer(),
                                    E::key("Mouse", crate::i18n::t("zc-config-help-mouse-tabs")),
                                ])
                            }
                        }
                        Some(ConfigTab::Skills) => {
                            if self.skills_active.is_some() {
                                HelpNode::entries(vec![
                                    E::new(
                                        vec![editor_key(crate::keymap::ConfigEditorAction::Save)],
                                        crate::i18n::t("zc-config-help-save"),
                                    ),
                                    E::new(
                                        vec![editor_key(crate::keymap::ConfigEditorAction::Cancel)],
                                        crate::i18n::t("zc-config-help-back-to-skills"),
                                    ),
                                    help(),
                                ])
                            } else {
                                HelpNode::entries(vec![
                                    E::new(
                                        switch_tabs_keys(),
                                        crate::i18n::t("zc-config-help-switch-tabs"),
                                    ),
                                    nav(),
                                    k(A::Enter, "zc-config-help-edit-skill"),
                                    k(A::ToggleSecret, "zc-config-help-archive-skill"),
                                    back(),
                                    help(),
                                    E::spacer(),
                                    E::key("Mouse", crate::i18n::t("zc-config-help-mouse-tabs")),
                                ])
                            }
                        }
                        _ => self.field_list_context(),
                    }
                } else {
                    self.field_list_context()
                }
            }
            Screen::FieldEdit { field_idx, .. } => {
                let is_string_array = self
                    .fields
                    .get(*field_idx)
                    .map(|f| f.kind == PropKind::StringArray)
                    .unwrap_or(false);
                if self.is_select_edit() {
                    if self.filter.is_some() {
                        HelpNode::entries(vec![
                            nav(),
                            k(A::Enter, "zc-config-help-save-selection"),
                            clear_filter(),
                            help(),
                        ])
                    } else {
                        HelpNode::entries(vec![
                            nav(),
                            k(A::Enter, "zc-config-help-save-selection"),
                            filter(),
                            k(A::Back, "zc-config-help-cancel"),
                            help(),
                            E::spacer(),
                            E::key("Mouse", crate::i18n::t("zc-config-help-mouse-save")),
                        ])
                    }
                } else if is_string_array {
                    HelpNode::entries(vec![
                        E::new(
                            vec![editor_key(crate::keymap::ConfigEditorAction::Confirm)],
                            crate::i18n::t("zc-config-help-new-line-entry"),
                        ),
                        E::new(
                            vec![editor_key(crate::keymap::ConfigEditorAction::Save)],
                            crate::i18n::t("zc-config-help-save-array"),
                        ),
                        E::new(
                            vec![editor_key(crate::keymap::ConfigEditorAction::Cancel)],
                            crate::i18n::t("zc-config-help-cancel"),
                        ),
                        help(),
                    ])
                } else {
                    HelpNode::entries(vec![
                        E::new(
                            vec![editor_key(crate::keymap::ConfigEditorAction::Confirm)],
                            crate::i18n::t("zc-config-help-save-value"),
                        ),
                        E::new(
                            vec![editor_key(crate::keymap::ConfigEditorAction::Cancel)],
                            crate::i18n::t("zc-config-help-cancel"),
                        ),
                        help(),
                    ])
                }
            }
        }
    }
}

impl App {
    fn field_list_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::ConfigTabAction as A;
        use crate::widgets::{HelpEntry as E, HelpNode};
        let has_tabs = !self.tab_names.is_empty();
        let mut entries = Vec::new();
        if has_tabs {
            entries.push(E::new(
                switch_tabs_keys(),
                crate::i18n::t("zc-config-help-switch-tabs"),
            ));
        }
        entries.push(E::new(
            nav_keys_split(),
            crate::i18n::t("zc-config-help-navigate"),
        ));
        entries.push(E::new(
            tab_keys(A::Enter),
            crate::i18n::t("zc-config-help-edit-field"),
        ));
        entries.push(E::new(
            tab_keys(A::DeleteRow),
            crate::i18n::t("zc-config-help-reset-default"),
        ));
        entries.push(E::key("/", crate::i18n::t("zc-config-help-filter")));
        entries.push(E::new(
            tab_keys(A::Back),
            crate::i18n::t("zc-config-help-back"),
        ));
        entries.push(E::key("?", crate::i18n::t("zc-config-help-this-help")));
        entries.push(E::spacer());
        let mouse = if has_tabs {
            crate::i18n::t("zc-config-help-mouse-tabs-edit")
        } else {
            crate::i18n::t("zc-config-help-mouse-edit")
        };
        entries.push(E::key("Mouse", mouse));
        HelpNode::entries(entries)
    }
}

// ── Layout ───────────────────────────────────────────────────────

struct Regions {
    breadcrumb: Rect,
    help: Rect,
    main: Rect,
    status: Rect,
}

fn regions(area: Rect) -> Regions {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // breadcrumb
            Constraint::Length(2), // help
            Constraint::Min(4),    // main
            Constraint::Length(1), // status
        ])
        .split(area);

    Regions {
        breadcrumb: chunks[0],
        help: chunks[1],
        main: chunks[2],
        status: chunks[3],
    }
}

fn render_filter_bar(frame: &mut Frame, area: Rect, buf: &str) {
    let display = format!("/{buf}█");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(display, theme::input_style()))),
        area,
    );
}

fn render_breadcrumb(frame: &mut Frame, area: Rect, segments: &[String]) {
    let mut spans: Vec<Span<'_>> = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ›  ", theme::dim_style()));
        }
        let style = if i == segments.len() - 1 {
            theme::accent_style().add_modifier(Modifier::BOLD)
        } else {
            theme::heading_style()
        };
        spans.push(Span::styled(seg.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── $EDITOR helper ───────────────────────────────────────────────

/// Open `content` in `$EDITOR` (or `$VISUAL`). Returns `Ok(edited)` on
/// success, or `Err(reason)` if the editor could not be launched / exited
/// non-zero. The caller falls back to the inline TUI editor on `Err`.
fn edit_in_external_editor(
    term: &mut Term,
    content: &str,
    filename_hint: &str,
) -> Result<String, String> {
    let Some(editor) = crate::editor::editor_from_env_or_path() else {
        return Err("no external editor found; set VISUAL or EDITOR".to_string());
    };

    // Write content to a temp file with the right extension.
    let dir = std::env::temp_dir();
    let tmp_path = dir.join(filename_hint);
    std::fs::write(&tmp_path, content).map_err(|e| format!("tmp write: {e}"))?;

    // Suspend TUI: leave alternate screen + disable raw mode so the
    // child process gets a normal terminal.
    let _ = execute!(
        term.backend_mut(),
        PopKeyboardEnhancementFlags,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();

    // Launch via `sh -c` so $EDITOR values with flags (e.g. "vim -u NONE",
    // "code --wait") work correctly.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} \"{}\"", editor, tmp_path.display()))
        .status();

    // Restore TUI.
    let _ = enable_raw_mode();
    let _ = execute!(term.backend_mut(), EnterAlternateScreen);
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            term.backend_mut(),
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        );
    }
    // Force a full redraw so ratatui repaints everything.
    let _ = term.clear();

    match status {
        Ok(s) if s.success() => {
            let edited =
                std::fs::read_to_string(&tmp_path).map_err(|e| format!("tmp read: {e}"))?;
            let _ = std::fs::remove_file(&tmp_path);
            Ok(edited)
        }
        Ok(s) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(format!("{editor} exited with {s}"))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(format!("failed to launch {editor}: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_enhancement_flags_disambiguate_modified_enter() {
        assert!(
            keyboard_enhancement_flags()
                .contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            "Shift+Enter reaches crossterm as plain Enter on common terminals unless keyboard enhancement asks for modified-key disambiguation"
        );
    }
}
