use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::wire::{ConfigFieldEntry, ConfigTab, PropKind, SectionShape};
use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEvent,
        MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    style::Print,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span, Text},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::client::{ConfigSectionEntry, ConfigTemplateEntry, RpcClient};
use crate::terminal_backend::WideCellCleanupBackend;
use crate::theme;

pub(crate) type Term = Terminal<WideCellCleanupBackend<Stdout>>;

fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
}

/// Ask terminals implementing XTSHIFTESCAPE to report Shift-modified mouse
/// events to the application instead of reserving Shift for local selection.
pub(crate) fn mouse_shift_capture_sequence(capture: bool) -> &'static str {
    if capture { "\x1b[>1s" } else { "\x1b[>0s" }
}

fn type_template_label(template: &ConfigTemplateEntry) -> String {
    template
        .path
        .rsplit('.')
        .next()
        .unwrap_or(&template.path)
        .to_string()
}

fn is_direct_child_path(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('.'))
        .is_some_and(|relative| !relative.is_empty() && !relative.contains('.'))
}

fn model_field_catalog_reference(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('.').collect();
    match segments.as_slice() {
        ["providers", "models", family, alias, "model"]
            if !family.is_empty() && !alias.is_empty() =>
        {
            Some(format!("{family}.{alias}"))
        }
        _ => None,
    }
}

fn retain_direct_children(fields: &mut Vec<ConfigFieldEntry>, prefix: &str) {
    fields.retain(|field| is_direct_child_path(&field.path, prefix));
}

pub(crate) fn init_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        Print(mouse_shift_capture_sequence(true)),
        EnableBracketedPaste,
    )?;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        );
    }
    Ok(Terminal::new(WideCellCleanupBackend::new(stdout))?)
}

pub(crate) fn restore_terminal(term: &mut Term) -> Result<()> {
    // Terminal status is not screen content: leaving the alternate screen does
    // not clear it, so hand it back explicitly. First, because every step below
    // can fail and return, and a tab left reading as busy outlives the process.
    crate::osc_status::release();
    disable_raw_mode()?;
    // Pop the enhancement flags best-effort — if they were never pushed (or the
    // terminal doesn't support them), popping is a harmless no-op we ignore.
    let _ = execute!(term.backend_mut(), PopKeyboardEnhancementFlags);
    execute!(
        term.backend_mut(),
        DisableBracketedPaste,
        Print(mouse_shift_capture_sequence(false)),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    Ok(())
}

// ── Screen stack ─────────────────────────────────────────────────

#[derive(Clone)]
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
    AliasRename {
        section_idx: usize,
        map_path: String,
        breadcrumb: Vec<String>,
        /// Capture the selected alias before entering the name editor.
        from: String,
        confirming: bool,
        return_to_fields: bool,
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
    /// Cancel restores the saved section instead of selecting an unfiltered row.
    Cancel,
}

struct FilterOrigin {
    pane: ZeroclawPane,
    section_cursor: usize,
    active_tab: usize,
    /// Detail page open when a sidebar search started. Previewing other
    /// sections replaces it, so cancel reopens this page rather than the
    /// section root.
    detail: Option<DetailOrigin>,
}

struct DetailOrigin {
    screen: Screen,
    loaded_section: Option<usize>,
    type_cursor: usize,
    alias_cursor: usize,
    alias_tab: usize,
    cost_cursor: usize,
    field_cursor: usize,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZeroclawPane {
    Sections,
    Detail,
}

/// Top-level Config sub-tab: the daemon RPC editor (`zeroclaw`) first,
/// the local client config (`zerocode`) second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

fn scalar_validation_status_key(kind: PropKind, value: &str) -> Option<&'static str> {
    match kind {
        PropKind::Integer => value
            .parse::<i64>()
            .err()
            .map(|_| "zc-config-status-invalid-integer"),
        PropKind::Float => value
            .parse::<f64>()
            .err()
            .map(|_| "zc-config-status-invalid-float"),
        _ => None,
    }
}

/// Validate a new secret-map entry key (env var name, header name, …).
/// Returns the Fluent key of the rejection message, or `None` when valid.
/// Kept deliberately loose: the same row type fronts env vars, HTTP headers,
/// and plugin settings, so only what can never be a valid key is refused.
fn secret_map_key_validation_key(key: &str) -> Option<&'static str> {
    if key.is_empty() {
        Some("zc-config-secret-map-key-empty")
    } else if key
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '=')
    {
        Some("zc-config-secret-map-key-invalid")
    } else {
        None
    }
}

fn scalar_validation_status(kind: PropKind, value: &str, prop: &str) -> Option<String> {
    scalar_validation_status_key(kind, value).map(|key| crate::i18n::t_args(key, &[("prop", prop)]))
}

fn config_i18n_key(kind: &str, stable_key: &str, suffix: Option<&str>) -> String {
    let normalized = stable_key.replace(['.', '_'], "-");
    match suffix {
        Some(suffix) => format!("zc-config-{kind}-{normalized}-{suffix}"),
        None => format!("zc-config-{kind}-{normalized}"),
    }
}

fn translated_config_value(
    key: String,
    fallback: &str,
    translate: &impl Fn(&str) -> Option<String>,
) -> String {
    translate(&key).unwrap_or_else(|| fallback.to_string())
}

const LEGACY_GROUPS: &[(&str, &str)] = &[
    ("Foundation", "foundation"),
    ("Agent", "agent"),
    ("Multi-agent", "multi_agent"),
    ("Tools", "tools"),
    ("Integrations", "integrations"),
    ("Network", "network"),
    ("Storage", "storage"),
    ("Operations", "operations"),
    ("Other", "other"),
];

fn legacy_group_key(label: &str) -> Option<&'static str> {
    LEGACY_GROUPS
        .iter()
        .find_map(|(legacy, key)| (*legacy == label).then_some(*key))
}

fn section_group_key(section: &ConfigSectionEntry) -> Option<&str> {
    if section.group_key.is_empty() {
        legacy_group_key(&section.group)
    } else {
        Some(&section.group_key)
    }
}

fn localized_group_label(
    section: &ConfigSectionEntry,
    translate: &impl Fn(&str) -> Option<String>,
) -> String {
    let fallback = if section.group.is_empty() {
        "Other"
    } else {
        &section.group
    };
    section_group_key(section).map_or_else(
        || fallback.to_string(),
        |key| translated_config_value(config_i18n_key("group", key, None), fallback, translate),
    )
}

fn localized_section_label(
    section: &ConfigSectionEntry,
    translate: &impl Fn(&str) -> Option<String>,
) -> String {
    translated_config_value(
        config_i18n_key("section", &section.key, Some("label")),
        &section.label,
        translate,
    )
}

fn localized_section_help(
    section: &ConfigSectionEntry,
    translate: &impl Fn(&str) -> Option<String>,
) -> String {
    translated_config_value(
        config_i18n_key("section", &section.key, Some("help")),
        &section.help,
        translate,
    )
}

fn localize_section_metadata_with(
    section: &mut ConfigSectionEntry,
    translate: &impl Fn(&str) -> Option<String>,
) {
    if section.group_key.is_empty()
        && let Some(key) = legacy_group_key(&section.group)
    {
        section.group_key = key.to_string();
    }
    if !section.group.is_empty() || !section.group_key.is_empty() {
        section.group = localized_group_label(section, translate);
    }
    section.label = localized_section_label(section, translate);
    section.help = localized_section_help(section, translate);
}

fn localize_section_metadata(section: &mut ConfigSectionEntry) {
    localize_section_metadata_with(section, &crate::i18n::try_t_locale);
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
    edit_cursor: usize,
    /// Two-step add on a `PropKind::SecretMap` row: `None` while the user
    /// types the entry key, `Some(key)` while they type its (masked) value.
    secret_map_entry_key: Option<String>,
    // Enum/bool select state
    select_cursor: usize,
    select_items: Vec<String>,
    status_msg: Option<String>,
    application_feedback: crate::config_application::ApplicationFeedback,
    // Filter state: None = inactive, Some(buf) = active filter
    filter: Option<String>,
    filter_cursor: usize,
    filter_origin: Option<FilterOrigin>,
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
    /// Segment hit areas from the actual breadcrumb renderer; separators are inert.
    last_breadcrumb_areas: Vec<Rect>,
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
            edit_cursor: 0,
            secret_map_entry_key: None,
            select_cursor: 0,
            select_items: Vec::new(),
            status_msg: None,
            application_feedback: crate::config_application::ApplicationFeedback::new(),
            filter: None,
            filter_cursor: 0,
            filter_origin: None,
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
            last_breadcrumb_areas: Vec::new(),
            last_tab_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
        }
    }

    /// Refresh application feedback without blocking terminal input.
    pub(crate) fn tick_application_status(&mut self, active: bool) {
        if active && self.section == ConfigSection::Zeroclaw {
            self.application_feedback.tick(&self.rpc);
        } else {
            self.application_feedback.deactivate();
        }
    }

    /// Load initial data from the daemon. Call once before draw/handle_key.
    pub(crate) async fn init(&mut self) -> Result<()> {
        self.sections = self.rpc.config_sections().await?;
        for section in &mut self.sections {
            localize_section_metadata(section);
        }
        self.sections.sort_by_key(Self::group_rank);
        self.templates = self.rpc.config_templates().await?;
        // Eagerly load the first section so the right pane previews content on
        // first paint, matching the zerocode Config tab.
        if !self.sections.is_empty() {
            self.load_section_content(self.section_cursor).await?;
        }
        Ok(())
    }

    /// Draw the current screen into the given area, beneath the Config
    /// section sub-tab bar (`zeroclaw` / `zerocode`).
    pub(crate) fn draw_into(&mut self, frame: &mut Frame, area: Rect) {
        use ratatui::layout::{Constraint, Direction, Layout};
        self.last_breadcrumb_areas.clear();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(if self.section == ConfigSection::Zeroclaw {
                    2
                } else {
                    0
                }),
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

        let mut hint = self.bottom_hint();
        if self.can_view_application_status() {
            hint.push_str(&format!(
                "  {}={}",
                tab_key(crate::keymap::ConfigTabAction::ApplicationStatus),
                crate::i18n::t("zc-config-application-view")
            ));
        }
        frame.render_widget(
            Paragraph::new(Span::styled(hint, theme::dim_style())),
            chunks[3],
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                self.application_feedback.summary(),
                theme::warn_style(),
            ))
            .wrap(Wrap { trim: false }),
            chunks[2],
        );
        if self.application_feedback.details_open() {
            self.application_feedback.draw_details(frame, body);
            return;
        }

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

        if self.section_filter_has_no_matches() {
            self.last_main_area = Rect::default();
            self.last_tab_area = None;
            render_no_matches(frame, right);
            return;
        }
        if self.loaded_section != Some(self.section_cursor) {
            self.last_main_area = Rect::default();
            self.last_tab_area = None;
            self.draw_section_detail_hint(frame, right);
            self.draw_status(frame, regions(right));
            return;
        }

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
            Screen::AliasRename {
                map_path,
                breadcrumb,
                from,
                confirming,
                ..
            } => {
                let path = map_path.clone();
                let bc = breadcrumb.clone();
                let from = from.clone();
                let confirming = *confirming;
                self.draw_alias_rename(frame, right, &path, &bc, &from, confirming);
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

    fn can_view_application_status(&self) -> bool {
        self.section == ConfigSection::Zeroclaw
            && self.application_feedback.has_content()
            && !matches!(
                self.screen,
                Screen::FieldEdit { .. } | Screen::AliasCreate { .. }
            )
            && self.filter.is_none()
            && self.personality_active_file.is_none()
            && self.skills_active.is_none()
    }

    fn bottom_hint(&self) -> String {
        use crate::keymap::{ConfigEditorAction as E, ConfigTabAction as T};

        if self.application_feedback.details_open() {
            return format!(
                "{}={}",
                tab_key(T::Back),
                crate::i18n::t("zc-config-help-back")
            );
        }
        let default = || format!(" ?={}", crate::i18n::t("zc-config-footer-action-help"));

        match &self.screen {
            Screen::AliasList { .. }
                if self.zeroclaw_pane == ZeroclawPane::Detail && self.alias_add_selected() =>
            {
                format!(
                    " {}={}  ?={}",
                    tab_key(T::Enter),
                    crate::i18n::t("zc-config-footer-action-add"),
                    crate::i18n::t("zc-config-footer-action-help"),
                )
            }
            Screen::AliasList { .. }
                if self.zeroclaw_pane == ZeroclawPane::Detail
                    && self.provider_alias_rename_available()
                    && self.detail_filter().is_none() =>
            {
                format!(
                    " {}={}  ?={}",
                    tab_key(T::RenameAlias),
                    crate::i18n::t("zc-config-footer-action-rename"),
                    crate::i18n::t("zc-config-footer-action-help"),
                )
            }
            Screen::FieldList { .. } if self.zeroclaw_pane == ZeroclawPane::Detail => {
                if self.detail_filter().is_some() {
                    let help = crate::i18n::t("zc-config-footer-action-help");
                    format!(
                        " {}  {}={}  {}={}  ?={}",
                        nav_keys(),
                        tab_key(T::Enter),
                        crate::i18n::t("zc-config-footer-action-edit"),
                        tab_key(T::Back),
                        crate::i18n::t("zc-config-footer-action-clear-filter"),
                        help,
                    )
                } else if self.is_composite_tab() {
                    match self.tab_names.get(self.active_tab) {
                        Some(ConfigTab::Personality) if self.personality_active_file.is_some() => {
                            format!(
                                " {}={}  {}={}",
                                editor_key(E::Save),
                                crate::i18n::t("zc-config-footer-action-save"),
                                editor_key(E::Cancel),
                                crate::i18n::t("zc-config-footer-action-back-to-files"),
                            )
                        }
                        Some(ConfigTab::Skills) if self.skills_active.is_some() => {
                            format!(
                                " {}={}  {}={}",
                                editor_key(E::Save),
                                crate::i18n::t("zc-config-footer-action-save"),
                                editor_key(E::Cancel),
                                crate::i18n::t("zc-config-footer-action-back-to-skills"),
                            )
                        }
                        _ => default(),
                    }
                } else {
                    let help = crate::i18n::t("zc-config-footer-action-help");
                    let rename = if self.provider_alias_rename_available() {
                        format!(
                            "  {}={}",
                            tab_key(T::RenameAlias),
                            crate::i18n::t("zc-config-footer-action-rename"),
                        )
                    } else {
                        String::new()
                    };
                    format!(
                        " {}={}  {}={}{}  ?={}",
                        tab_key(T::Enter),
                        crate::i18n::t("zc-config-footer-action-edit"),
                        tab_key(T::DeleteRow),
                        crate::i18n::t("zc-config-footer-action-reset"),
                        rename,
                        help,
                    )
                }
            }
            Screen::AliasCreate { .. } => {
                format!(
                    " {}={}  {}={}",
                    editor_key(E::Confirm),
                    crate::i18n::t("zc-config-footer-action-create"),
                    editor_key(E::Cancel),
                    crate::i18n::t("zc-config-footer-action-cancel"),
                )
            }
            Screen::AliasRename { confirming, .. } => {
                format!(
                    " {}={}  {}={}",
                    editor_key(E::Confirm),
                    crate::i18n::t(if *confirming {
                        "zc-config-footer-action-rename"
                    } else {
                        "zc-config-footer-action-review-rename"
                    }),
                    editor_key(E::Cancel),
                    crate::i18n::t("zc-config-footer-action-cancel"),
                )
            }
            Screen::FieldEdit { field_idx, .. } => {
                if self.is_select_edit() {
                    if self.detail_filter().is_some() {
                        let help = crate::i18n::t("zc-config-footer-action-help");
                        format!(
                            " {}  {}={}  {}={}  ?={}",
                            nav_keys(),
                            tab_key(T::Enter),
                            crate::i18n::t("zc-config-footer-action-save"),
                            tab_key(T::Back),
                            crate::i18n::t("zc-config-footer-action-clear-filter"),
                            help,
                        )
                    } else {
                        format!(
                            " {}={}  {}={}",
                            tab_key(T::Enter),
                            crate::i18n::t("zc-config-footer-action-save"),
                            tab_key(T::Back),
                            crate::i18n::t("zc-config-footer-action-cancel"),
                        )
                    }
                } else if self.fields[*field_idx].kind == PropKind::StringArray {
                    format!(
                        " {}={}  {}={}  {}={}",
                        editor_key(E::Confirm),
                        crate::i18n::t("zc-config-footer-action-new-line"),
                        editor_key(E::Save),
                        crate::i18n::t("zc-config-footer-action-save"),
                        editor_key(E::Cancel),
                        crate::i18n::t("zc-config-footer-action-cancel"),
                    )
                } else {
                    format!(
                        " {}={}  {}={}",
                        editor_key(E::Confirm),
                        crate::i18n::t("zc-config-footer-action-save"),
                        editor_key(E::Cancel),
                        crate::i18n::t("zc-config-footer-action-cancel"),
                    )
                }
            }
            _ => default(),
        }
    }

    /// Highlight style + symbol for the right (detail) pane lists, mirroring
    /// `zerocode_pane::ZerocodePane::detail_highlight`: the active selection
    /// style and "› " gutter when the detail pane holds focus, the dim "you
    /// are here" marker when focus has stepped back to the section list.
    fn detail_highlight(&self) -> (ratatui::style::Style, &'static str) {
        let focused = matches!(
            (&self.screen, self.zeroclaw_pane),
            (Screen::FieldEdit { .. }, _) | (_, ZeroclawPane::Detail)
        );
        let symbol = if focused { "\u{203a} " } else { "  " };
        (theme::selection_highlight(focused, false), symbol)
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
        let result = self.handle_key_inner(key, term).await;
        self.settle_handler_result(result, false)
    }

    /// Shared exit path of `handle_key` / `handle_mouse`: an RPC timeout is
    /// absorbed into the status line and replaced by `absorbed` (for keys,
    /// `false` = do not quit); every other result passes through unchanged.
    fn settle_handler_result<T>(&mut self, result: Result<T>, absorbed: T) -> Result<T> {
        match result {
            Err(error) if self.absorb_rpc_timeout(&error) => Ok(absorbed),
            other => other,
        }
    }

    /// A per-call RPC deadline (`DaemonRpcTimeout`) means the daemon is busy
    /// on that one method, not that the connection is gone. Surfacing it as
    /// a status line keeps the TUI alive; letting it unwind through
    /// `app::run` used to exit the whole process (observed on `config/list`
    /// while a route-affecting `config/set` was queued behind a running
    /// turn). Returns `true` when the error was absorbed.
    fn absorb_rpc_timeout(&mut self, error: &anyhow::Error) -> bool {
        if crate::client::DaemonRpcTimeout::from_anyhow(error).is_none() {
            return false;
        }
        self.status_msg = Some(crate::i18n::t_args(
            "zc-config-status-load-failed",
            &[("err", &format!("{error:#}"))],
        ));
        true
    }

    async fn handle_key_inner(&mut self, key: KeyEvent, term: &mut Term) -> Result<bool> {
        if self.section == ConfigSection::Zeroclaw {
            if self.application_feedback.handle_key(key) {
                return Ok(false);
            }
            if self.can_view_application_status()
                && crate::keymap::ConfigTabAction::from_chord(&key)
                    == Some(crate::keymap::ConfigTabAction::ApplicationStatus)
            {
                self.application_feedback.toggle_details();
                return Ok(false);
            }
        }
        self.status_msg = None;

        // Tab / Shift+Tab cycle the outer Config section (zeroclaw ↔
        // zerocode) from anywhere — neither is bound inside the daemon
        // editor or the zerocode pane, so there is no shadowing.
        if let Some(action) = crate::keymap::ConfigTabAction::from_chord(&key) {
            use crate::keymap::ConfigTabAction;
            if action == ConfigTabAction::SectionNext {
                self.cycle_section(1);
                self.sync_zerocode_locales().await;
                return Ok(false);
            }
            if action == ConfigTabAction::SectionPrev {
                self.cycle_section(-1);
                self.sync_zerocode_locales().await;
                return Ok(false);
            }
        }

        if self.section == ConfigSection::Zerocode {
            if !self.zerocode.handle_key(key) {
                // Left/Back at the zerocode section level was not consumed:
                // cross back to the outer left (zeroclaw) pane.
                self.cycle_section(-1);
            }
            self.sync_zerocode_locales().await;
            return Ok(false);
        }

        // Search cancellation takes precedence over drill/back navigation.
        if self.filter.is_some()
            && crate::keymap::SearchBoxAction::from_chord(&key)
                == Some(crate::keymap::SearchBoxAction::Cancel)
        {
            self.cancel_filter().await?;
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
                && self.filter.is_none()
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
            Screen::AliasRename { .. } => self.handle_alias_rename(key).await?,
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
        self.cancel_alias_rename();
        self.deactivate_filter();
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
        area: Rect,
        term: &mut Term,
    ) -> Result<()> {
        let result = self.handle_mouse_inner(mouse, area, term).await;
        self.settle_handler_result(result, ())
    }

    async fn handle_mouse_inner(
        &mut self,
        mouse: MouseEvent,
        _area: Rect,
        term: &mut Term,
    ) -> Result<()> {
        use crate::mouse;
        if self.application_feedback.details_open() {
            return Ok(());
        }

        // Section tab-bar click switches sub-tab in either section.
        if let MouseEventKind::Down(crossterm::event::MouseButton::Left) = mouse.kind
            && let Some(bar) = self.section_tab_area
            && mouse::in_rect(mouse.column, mouse.row, bar)
        {
            let labels: Vec<&str> = CONFIG_SECTIONS.iter().map(|s| s.label()).collect();
            if let Some(idx) = mouse::tab_click_index(mouse.column, mouse.row, bar, &labels, 3) {
                if self.section != CONFIG_SECTIONS[idx] {
                    self.cancel_alias_rename();
                }
                self.deactivate_filter();
                self.section = CONFIG_SECTIONS[idx];
                return Ok(());
            }
        }

        if self.section == ConfigSection::Zerocode {
            self.zerocode.handle_mouse(mouse);
            self.sync_zerocode_locales().await;
            return Ok(());
        }

        if self.section_filter_has_no_matches()
            || (self.loaded_section != Some(self.section_cursor)
                && !mouse::in_rect(mouse.column, mouse.row, self.last_section_pane_area))
        {
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                if let Some(idx) = self
                    .last_breadcrumb_areas
                    .iter()
                    .position(|&area| mouse::in_rect(mouse.column, mouse.row, area))
                {
                    self.open_breadcrumb(idx).await?;
                    return Ok(());
                }
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
                        && idx < labels.len()
                    {
                        self.zeroclaw_pane = ZeroclawPane::Detail;
                        if idx != self.alias_tab {
                            self.alias_tab = idx;
                            self.deactivate_filter();
                            if idx == 1 {
                                self.load_cost_resources().await?;
                            }
                        }
                    }
                    return Ok(());
                }

                // Tab bar click (FieldList only).
                if matches!(self.screen, Screen::FieldList { .. })
                    && let Some(tab_rect) = self.last_tab_area
                    && mouse::in_rect(mouse.column, mouse.row, tab_rect)
                {
                    let labels: Vec<&str> = self.tab_names.iter().map(|t| t.label()).collect();
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
                    ) && idx < self.tab_names.len()
                    {
                        self.zeroclaw_pane = ZeroclawPane::Detail;
                        if idx != self.active_tab {
                            self.active_tab = idx;
                            self.field_cursor =
                                self.tab_field_indices().first().copied().unwrap_or(0);
                            self.deactivate_filter();
                            self.on_tab_switched(term).await?;
                        }
                    }
                    return Ok(());
                }

                // Section pane click: select the clicked section, return focus to
                // the left pane, and reset ordinary navigation to the section root. Display
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
                        if self.detail_filter().is_some() {
                            self.deactivate_filter();
                        }
                        if self.filter_for(ZeroclawPane::Sections).is_some() {
                            let labels: Vec<&str> =
                                self.sections.iter().map(|s| s.label.as_str()).collect();
                            let visible = self.filtered_section_indices(&labels);
                            self.filter_cursor =
                                visible.iter().position(|&i| i == orig).unwrap_or(0);
                        }
                        self.section_cursor = orig;
                        self.zeroclaw_pane = ZeroclawPane::Sections;
                        if self.filter_for(ZeroclawPane::Sections).is_some() {
                            self.preview_section(orig).await?;
                        } else {
                            // Search previews retain their origin; ordinary clicks
                            // discard unsaved input and reopen the section root.
                            if matches!(
                                self.screen,
                                Screen::TypeList { .. } | Screen::AliasList { .. }
                            ) {
                                self.active_tab = 0;
                            }
                            if self.loaded_section == Some(orig) && !self.at_section_top_level() {
                                self.load_section_content(orig).await?;
                            } else {
                                self.preview_section(orig).await?;
                            }
                            self.edit_buf.clear();
                            self.edit_cursor = 0;
                            self.active_tab = 0;
                            self.alias_tab = 0;
                            self.last_tab_area = None;
                            self.last_breadcrumb_areas.clear();
                        }
                        self.status_msg = None;
                    }
                    return Ok(());
                }

                // List area click.
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area)
                    && mouse.column > self.last_main_area.x
                    && mouse.column < self.last_main_area.right().saturating_sub(1)
                {
                    let count = self.visible_count();
                    if let Some(pos) = mouse::list_click_index(
                        mouse.row,
                        self.last_main_area,
                        self.last_list_offset,
                        count,
                    ) {
                        if self.filter_for(ZeroclawPane::Sections).is_some() {
                            self.deactivate_filter();
                        }
                        self.zeroclaw_pane = ZeroclawPane::Detail;
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
                if mouse::in_rect(mouse.column, mouse.row, self.last_section_pane_area) =>
            {
                self.scroll_sections(true).await?;
            }
            MouseEventKind::ScrollDown
                if mouse::in_rect(mouse.column, mouse.row, self.last_section_pane_area) =>
            {
                self.scroll_sections(false).await?;
            }

            MouseEventKind::ScrollUp
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) =>
            {
                if self.filter_for(ZeroclawPane::Sections).is_some() {
                    self.deactivate_filter();
                }
                let cur = self.visible_cursor();
                let count = self.visible_count();
                let next = mouse::list_scroll(cur, count, true, 3);
                self.set_visible_cursor(next);
            }

            MouseEventKind::ScrollDown
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) =>
            {
                if self.filter_for(ZeroclawPane::Sections).is_some() {
                    self.deactivate_filter();
                }
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

    async fn scroll_sections(&mut self, up: bool) -> Result<()> {
        let labels: Vec<&str> = self.sections.iter().map(|s| s.label.as_str()).collect();
        let visible = self.filtered_section_indices(&labels);
        let cursor = visible
            .iter()
            .position(|&i| i == self.section_cursor)
            .unwrap_or(0);
        let next = crate::mouse::list_scroll(cursor, visible.len(), up, 1);
        if let Some(&orig) = visible.get(next) {
            if self.detail_filter().is_some() {
                self.deactivate_filter();
            }
            if self.filter_for(ZeroclawPane::Sections).is_some() {
                self.filter_cursor = next;
            }
            self.section_cursor = orig;
            self.preview_section(orig).await?;
        }
        Ok(())
    }

    /// Number of visible items for the current screen (respecting filters).
    fn visible_count(&self) -> usize {
        match &self.screen {
            Screen::SectionList => {
                let labels: Vec<String> = self.sections.iter().map(|s| s.label.clone()).collect();
                self.filtered_section_indices(&labels).len()
            }
            Screen::TypeList { section_idx } => {
                let names = self.type_list_labels(*section_idx);
                self.filtered_indices(&names).len()
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    self.cost_resources.len() + 1 // +1 for [+ Add]
                } else {
                    let vis = self.filtered_indices(&self.aliases);
                    // +1 for [+ Add] when not filtering
                    if self.detail_filter().is_none() {
                        vis.len() + 1
                    } else {
                        vis.len()
                    }
                }
            }
            Screen::AliasCreate { .. } | Screen::AliasRename { .. } => 0,
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
                if self.filter_for(ZeroclawPane::Sections).is_some() {
                    self.filter_cursor
                } else {
                    let labels: Vec<String> =
                        self.sections.iter().map(|s| s.label.clone()).collect();
                    self.filtered_section_indices(&labels)
                        .iter()
                        .position(|&i| i == self.section_cursor)
                        .unwrap_or(0)
                }
            }
            Screen::TypeList { section_idx } => {
                if self.detail_filter().is_some() {
                    self.filter_cursor
                } else {
                    let names = self.type_list_labels(*section_idx);
                    self.filtered_indices(&names)
                        .iter()
                        .position(|&i| i == self.type_cursor)
                        .unwrap_or(0)
                }
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    self.cost_cursor
                } else if self.detail_filter().is_some() {
                    self.filter_cursor
                } else {
                    self.alias_cursor
                }
            }
            Screen::AliasCreate { .. } | Screen::AliasRename { .. } => 0,
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
                if self.detail_filter().is_some() {
                    self.filter_cursor
                } else {
                    self.select_cursor
                }
            }
        }
    }

    /// Helper: current field cursor in visible coordinates.
    fn visible_field_cursor(&self) -> usize {
        if self.detail_filter().is_some() {
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
                let visible = self.filtered_section_indices(&labels);
                if self.filter_for(ZeroclawPane::Sections).is_some() {
                    self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                } else if let Some(&orig) = visible.get(pos) {
                    self.section_cursor = orig;
                }
            }
            Screen::TypeList { section_idx } => {
                let names = self.type_list_labels(*section_idx);
                let visible = self.filtered_indices(&names);
                if self.detail_filter().is_some() {
                    self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                } else if let Some(&orig) = visible.get(pos) {
                    self.type_cursor = orig;
                }
            }
            Screen::AliasList { .. } => {
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    let total = self.cost_resources.len() + 1; // +1 for [+ Add]
                    self.cost_cursor = pos.min(total.saturating_sub(1));
                } else if self.detail_filter().is_some() {
                    let visible = self.filtered_indices(&self.aliases);
                    self.filter_cursor = pos.min(visible.len().saturating_sub(1));
                } else {
                    let total = if self.detail_filter().is_none() {
                        self.aliases.len() + 1 // +1 for [+ Add]
                    } else {
                        self.aliases.len()
                    };
                    self.alias_cursor = pos.min(total.saturating_sub(1));
                }
            }
            Screen::AliasCreate { .. } | Screen::AliasRename { .. } => {}
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
                    if self.detail_filter().is_some() {
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
        if self.detail_filter().is_some() {
            self.filter_cursor = pos.min(filter_vis.len().saturating_sub(1));
        } else if let Some(&orig) = visible.get(pos) {
            self.field_cursor = orig;
        }
    }

    fn selected_type_row(&self) -> Option<usize> {
        let Screen::TypeList { section_idx } = &self.screen else {
            return None;
        };
        let names = self.type_list_labels(*section_idx);
        let visible = self.filtered_indices(&names);
        let cursor = if self.detail_filter().is_some() {
            self.filter_cursor
        } else {
            visible.iter().position(|&i| i == self.type_cursor)?
        };
        visible.get(cursor).copied()
    }

    /// Activate the currently selected item (double-click equivalent of Enter).
    async fn activate_mouse(&mut self, term: &mut Term) -> Result<()> {
        match &self.screen {
            Screen::SectionList => {
                let idx = self.section_cursor;
                self.enter_section(idx).await?;
            }
            Screen::TypeList { .. } => {
                if let Some(idx) = self.selected_type_row() {
                    self.deactivate_filter();
                    self.enter_type(idx).await?;
                }
            }
            Screen::AliasList { .. } => {
                self.activate_alias_list().await?;
            }
            Screen::AliasCreate { .. } | Screen::AliasRename { .. } => {}
            Screen::FieldList { .. } => {
                if self.is_composite_tab() {
                    // Double-click on personality file or skill opens editor —
                    // that requires async loading which mirrors the Enter key
                    // handler. For now, no-op on composite tabs.
                } else {
                    let tab_indices = self.tab_field_indices();
                    let labels = self.field_labels_for_tab(&tab_indices);
                    let visible = self.filtered_indices(&labels);
                    if let Some(&pos) = visible.get(self.visible_field_cursor()) {
                        let idx = tab_indices[pos];
                        self.deactivate_filter();
                        self.field_cursor = idx;
                        self.enter_field_edit(idx, term).await;
                    }
                }
            }
            Screen::FieldEdit { .. } => {
                if self.is_select_edit() {
                    let visible = self.filtered_indices(&self.select_items);
                    let cursor = if self.detail_filter().is_some() {
                        self.filter_cursor
                    } else {
                        self.select_cursor
                    };
                    if let Some(&orig) = visible.get(cursor) {
                        self.deactivate_filter();
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

    fn section_has_root_settings_row(&self, section_idx: usize) -> bool {
        self.sections.get(section_idx).is_some_and(|section| {
            section.shape == Some(SectionShape::TypedFamilyMap) && section.key == "channels"
        })
    }

    fn type_list_labels(&self, section_idx: usize) -> Vec<String> {
        let has_root_row = self.section_has_root_settings_row(section_idx);
        let root_count = usize::from(has_root_row);
        let mut labels = Vec::with_capacity(self.types.len() + root_count);
        if has_root_row {
            labels.push(format!("[{}]", self.sections[section_idx].key));
        }
        labels.extend(self.types.iter().map(type_template_label));
        labels
    }

    fn type_list_len(&self, section_idx: usize) -> usize {
        self.types.len() + usize::from(self.section_has_root_settings_row(section_idx))
    }

    fn template_idx_for_type_row(&self, section_idx: usize, row_idx: usize) -> Option<usize> {
        if self.section_has_root_settings_row(section_idx) {
            row_idx.checked_sub(1).filter(|&idx| idx < self.types.len())
        } else if row_idx < self.types.len() {
            Some(row_idx)
        } else {
            None
        }
    }

    fn is_typed_family_root_field_list(&self, section_idx: usize, prefix: &str) -> bool {
        self.sections.get(section_idx).is_some_and(|section| {
            section.shape == Some(SectionShape::TypedFamilyMap) && section.key == prefix
        })
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

    fn provider_alias_rename_target(&self) -> Option<(usize, &str, &str)> {
        let (section_idx, map_path, from) = match &self.screen {
            Screen::AliasList {
                section_idx,
                map_path,
                ..
            } if self.alias_tab == 0 => (
                *section_idx,
                map_path.as_str(),
                self.aliases.get(self.alias_cursor)?.as_str(),
            ),
            Screen::FieldList {
                section_idx,
                prefix,
                ..
            } => {
                let (map_path, from) = prefix.rsplit_once('.')?;
                (*section_idx, map_path, from)
            }
            _ => return None,
        };
        let section = self.sections.get(section_idx)?;
        (matches!(
            section.key.as_str(),
            "providers.models" | "providers.tts" | "providers.transcription"
        ) && is_direct_child_path(map_path, &section.key)
            && !from.is_empty())
        .then_some((section_idx, map_path, from))
    }

    fn provider_alias_rename_available(&self) -> bool {
        self.provider_alias_rename_target().is_some()
    }

    fn start_alias_rename(&mut self) {
        let Some((section_idx, map_path, from)) = self.provider_alias_rename_target() else {
            return;
        };
        let map_path = map_path.to_string();
        let from = from.to_string();
        let (mut breadcrumb, return_to_fields) = match &self.screen {
            Screen::AliasList { breadcrumb, .. } => (breadcrumb.clone(), false),
            Screen::FieldList { breadcrumb, .. } => (breadcrumb.clone(), true),
            _ => return,
        };
        if return_to_fields {
            breadcrumb.pop();
        }
        self.edit_buf = from.clone();
        self.status_msg = None;
        self.screen = Screen::AliasRename {
            section_idx,
            map_path,
            breadcrumb,
            from,
            confirming: false,
            return_to_fields,
        };
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

    async fn prefill_cost_rates_from_catalog(&mut self, base_path: &str, resource: &str) {
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
                if let Ok(revision) = self.rpc.config_set(&prop, serde_json::json!(v)).await {
                    self.application_feedback.record_saved(prop, revision);
                }
            }
        }
    }

    async fn load_fields(&mut self, prefix: &str) -> Result<()> {
        self.fields = self.rpc.config_list(Some(prefix)).await?;
        if prefix == "channels" {
            retain_direct_children(&mut self.fields, prefix);
        }
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
        if prefix == "channels" {
            retain_direct_children(&mut new_fields, prefix);
        }
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
        let visible = self.filtered_section_indices(&labels);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => {
                // The filter edit may have changed the filtered list or moved
                // `filter_cursor`; resolve the highlighted filtered row back to
                // its underlying section so the right-pane preview matches what
                // `draw_sections_pane` highlights and what Enter will open.
                let visible = self.filtered_section_indices(&labels);
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.section_cursor = orig;
                }
                if !visible.is_empty() {
                    self.preview_section(self.section_cursor).await?;
                }
                return Ok(false);
            }
            FilterAction::Cancel => {
                self.cancel_filter().await?;
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
            Screen::TypeList { section_idx } => {
                self.type_cursor = pos.min(self.type_list_len(*section_idx).saturating_sub(1));
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

    async fn open_breadcrumb(&mut self, idx: usize) -> Result<()> {
        let (section_idx, current_idx) = match &self.screen {
            Screen::SectionList => return Ok(()),
            Screen::TypeList { section_idx } => (*section_idx, 0),
            Screen::AliasList {
                section_idx,
                breadcrumb,
                ..
            }
            | Screen::FieldList {
                section_idx,
                breadcrumb,
                ..
            } => (*section_idx, breadcrumb.len().saturating_sub(1)),
            Screen::AliasCreate {
                section_idx,
                breadcrumb,
                ..
            }
            | Screen::FieldEdit {
                section_idx,
                breadcrumb,
                ..
            } => (*section_idx, breadcrumb.len()),
            Screen::AliasRename {
                section_idx,
                breadcrumb,
                ..
            } => (*section_idx, breadcrumb.len() + 1),
        };
        self.zeroclaw_pane = ZeroclawPane::Detail;
        if idx >= current_idx {
            return Ok(());
        }
        if idx == 0 {
            self.load_section_content(section_idx).await?;
        } else {
            match &self.screen {
                Screen::AliasCreate {
                    map_path,
                    breadcrumb,
                    ..
                } => {
                    self.restore_alias_list_from_create_back(
                        section_idx,
                        map_path.clone(),
                        breadcrumb.clone(),
                    )
                    .await?;
                }
                Screen::AliasRename {
                    map_path,
                    breadcrumb,
                    from,
                    ..
                } if idx == breadcrumb.len() => {
                    let prefix = format!("{map_path}.{from}");
                    let mut bc = breadcrumb.clone();
                    bc.push(from.clone());
                    self.load_fields(&prefix).await?;
                    self.screen = Screen::FieldList {
                        section_idx,
                        prefix,
                        breadcrumb: bc,
                    };
                }
                Screen::AliasRename { .. } => self.cancel_alias_rename(),
                Screen::FieldEdit { .. } => self.pop_to_field_list().await?,
                _ => {}
            }
            if let Screen::FieldList { breadcrumb, .. } = &self.screen
                && breadcrumb.len() > idx + 1
            {
                self.restore_alias_list_from_field_back(section_idx, breadcrumb.clone())
                    .await?;
            }
            // Costs is a breadcrumb only inside a resource form. Its parent
            // family still opens Aliases; the Costs crumb keeps the Costs tab.
            if matches!(self.screen, Screen::AliasList { .. }) {
                self.active_tab = 0;
                if idx == 1 {
                    self.alias_tab = 0;
                }
            }
        }
        self.deactivate_filter();
        self.edit_buf.clear();
        self.edit_cursor = 0;
        self.last_tab_area = None;
        self.last_breadcrumb_areas.clear();
        self.status_msg = None;
        Ok(())
    }

    async fn preview_section(&mut self, idx: usize) -> Result<()> {
        self.cancel_alias_rename();
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
        self.cancel_alias_rename();
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
        let section_idx = match &self.screen {
            Screen::TypeList { section_idx } => *section_idx,
            _ => return Ok(()),
        };
        let type_names = self.type_list_labels(section_idx);
        let visible = self.filtered_indices(&type_names);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Cancel => return self.cancel_filter().await,
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
            Some(ConfigTabAction::Down)
                if self.type_cursor + 1 < self.type_list_len(section_idx) =>
            {
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
        if let Screen::TypeList { section_idx } = &self.screen {
            let section_idx = *section_idx;
            let section_key = self.sections[section_idx].key.clone();
            if self.section_has_root_settings_row(section_idx) && orig_idx == 0 {
                self.load_fields(&section_key).await?;
                self.screen = Screen::FieldList {
                    section_idx,
                    prefix: section_key.clone(),
                    breadcrumb: vec![section_key.clone(), format!("[{section_key}]")],
                };
                self.status_msg = None;
                return Ok(());
            }

            let Some(template_idx) = self.template_idx_for_type_row(section_idx, orig_idx) else {
                return Ok(());
            };
            let Some(tmpl) = self.types.get(template_idx) else {
                return Ok(());
            };
            let map_path = tmpl.path.clone();
            let type_name = type_template_label(tmpl);
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
        // the FieldList tab bar uses. TabRight steps into Costs; TabLeft steps
        // back toward Aliases, then on the leftmost tab walks out to the type
        // list (the opposite of "into"), mirroring the FieldList sub-tab gesture.
        if has_tabs && let Some(action) = ConfigTabAction::from_chord(&key) {
            match action {
                ConfigTabAction::TabLeft if self.alias_tab > 0 => {
                    self.alias_tab -= 1;
                    self.deactivate_filter();
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
        let has_add = self.detail_filter().is_none();
        let visible_total = if has_add {
            visible.len() + 1
        } else {
            visible.len()
        };

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Cancel => return self.cancel_filter().await,
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.deactivate_filter();
                    return self.enter_alias(orig).await;
                }
                return Ok(());
            }
            FilterAction::Passthrough => {}
        }

        let action = ConfigTabAction::from_chord(&key);
        // With tabs, TabLeft is consumed for tab switching while alias_tab > 0;
        // it only reaches here on the leftmost tab, where it walks out like Back.
        let back = matches!(
            action,
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft)
        );
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
            Some(ConfigTabAction::RenameAlias) => self.start_alias_rename(),
            _ if into => {
                self.activate_alias_list().await?;
            }
            Some(ConfigTabAction::ToggleSecret) if self.alias_cursor < self.aliases.len() => {
                if let Screen::AliasList { map_path, .. } = &self.screen {
                    let alias = self.aliases[self.alias_cursor].clone();
                    let map_path = map_path.clone();
                    match self.rpc.config_map_key_delete(&map_path, &alias).await {
                        Ok(result) => {
                            if result.changed {
                                self.application_feedback
                                    .record_saved(format!("{map_path}.{alias}"), result.revision);
                            }
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

    fn alias_add_selected(&self) -> bool {
        if self.alias_list_has_tabs() && self.alias_tab == 1 {
            self.cost_cursor == self.cost_resources.len()
        } else {
            self.detail_filter().is_none() && self.alias_cursor == self.aliases.len()
        }
    }

    /// Keyboard and mouse activation share the same Add and open behavior.
    async fn activate_alias_list(&mut self) -> Result<()> {
        if self.alias_add_selected() {
            if let Screen::AliasList {
                section_idx,
                map_path,
                breadcrumb,
            } = &self.screen
            {
                let mut map_path = map_path.clone();
                let mut breadcrumb = breadcrumb.clone();
                if self.alias_list_has_tabs() && self.alias_tab == 1 {
                    let Some(base) = self.cost_base_path() else {
                        return Ok(());
                    };
                    map_path = base;
                    breadcrumb.push(ConfigTab::Costs.label().to_string());
                }
                self.edit_buf.clear();
                self.screen = Screen::AliasCreate {
                    section_idx: *section_idx,
                    map_path,
                    breadcrumb,
                };
            }
        } else if self.alias_list_has_tabs() && self.alias_tab == 1 {
            if self.cost_cursor < self.cost_resources.len() {
                self.enter_cost_resource(self.cost_cursor).await?;
            }
        } else {
            let visible = self.filtered_indices(&self.aliases);
            let cursor = if self.detail_filter().is_some() {
                self.filter_cursor
            } else {
                self.alias_cursor
            };
            if let Some(&idx) = visible.get(cursor) {
                self.deactivate_filter();
                self.enter_alias(idx).await?;
            }
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
                self.activate_alias_list().await?;
            }
            Some(ConfigTabAction::DeleteRow | ConfigTabAction::ToggleSecret)
                if self.cost_cursor < self.cost_resources.len() =>
            {
                let resource = self.cost_resources[self.cost_cursor].clone();
                match self.rpc.config_map_key_delete(&base, &resource).await {
                    Ok(result) => {
                        if result.changed {
                            self.application_feedback
                                .record_saved(format!("{base}.{resource}"), result.revision);
                        }
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

    /// Focus setup only after creation; ordinary opens and refreshes preserve navigation.
    fn focus_missing_setup_field(&mut self) {
        let Some(index) = self
            .fields
            .iter()
            .position(|field| field.setup.as_ref().is_some_and(|setup| setup.missing))
        else {
            return;
        };
        self.field_cursor = index;
        if let Some(tab) = self
            .tab_names
            .iter()
            .position(|tab| *tab == self.fields[index].tab)
        {
            self.active_tab = tab;
        }
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
                        Ok(result) => {
                            if result.changed {
                                self.application_feedback
                                    .record_saved(format!("{map_path}.{name}"), result.revision);
                            }
                            self.prefill_cost_rates_from_catalog(&map_path, &name).await;
                            let prefix = format!("{}.{}", map_path, name);
                            let mut bc = breadcrumb;
                            bc.push(name);
                            self.load_fields(&prefix).await?;
                            self.focus_missing_setup_field();
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
            None => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.edit_buf.push(c);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn cancel_alias_rename(&mut self) {
        if !matches!(self.screen, Screen::AliasRename { .. }) {
            return;
        }
        if let Screen::AliasRename {
            section_idx,
            map_path,
            mut breadcrumb,
            from,
            return_to_fields,
            ..
        } = std::mem::replace(&mut self.screen, Screen::SectionList)
        {
            self.screen = if return_to_fields {
                let prefix = format!("{map_path}.{from}");
                breadcrumb.push(from);
                Screen::FieldList {
                    section_idx,
                    prefix,
                    breadcrumb,
                }
            } else {
                Screen::AliasList {
                    section_idx,
                    map_path,
                    breadcrumb,
                }
            };
        }
        self.edit_buf.clear();
        self.status_msg = None;
    }

    fn finish_alias_rename(&mut self) {
        if let Screen::AliasRename {
            return_to_fields, ..
        } = &mut self.screen
        {
            *return_to_fields = false;
        }
        self.cancel_alias_rename();
    }

    async fn handle_alias_rename(&mut self, key: KeyEvent) -> Result<()> {
        use crate::keymap::ConfigEditorAction;
        let Screen::AliasRename {
            section_idx,
            from,
            confirming,
            ..
        } = &self.screen
        else {
            return Ok(());
        };
        let section_idx = *section_idx;
        let from = from.clone();
        let confirming = *confirming;
        match ConfigEditorAction::from_chord(&key) {
            Some(ConfigEditorAction::Cancel) => self.cancel_alias_rename(),
            Some(ConfigEditorAction::Confirm) if key.kind != KeyEventKind::Press => {}
            Some(ConfigEditorAction::Confirm) if !confirming => {
                let to = self.edit_buf.trim().to_string();
                if to.is_empty() {
                    self.status_msg = Some(crate::i18n::t("zc-config-status-alias-empty"));
                } else if to == from {
                    self.status_msg =
                        Some(crate::i18n::t("zc-config-status-alias-rename-unchanged"));
                } else {
                    self.edit_buf = to;
                    if let Screen::AliasRename { confirming, .. } = &mut self.screen {
                        *confirming = true;
                    }
                }
            }
            Some(ConfigEditorAction::Confirm) => {
                let Screen::AliasRename { map_path, .. } = &self.screen else {
                    return Ok(());
                };
                let map_path = map_path.clone();
                let to = self.edit_buf.clone();
                match self.rpc.config_map_key_rename(&map_path, &from, &to).await {
                    Ok(result) => {
                        let mut status = if result.renamed {
                            // The rename has committed. Leave the editor before refreshing
                            // so a failed read cannot cause the mutation to be retried.
                            self.finish_alias_rename();
                            match self.load_aliases(&map_path).await {
                                Ok(()) => {
                                    self.alias_cursor = self
                                        .aliases
                                        .iter()
                                        .position(|alias| alias == &to)
                                        .unwrap_or(0);
                                    crate::i18n::t_args(
                                        "zc-config-status-alias-renamed",
                                        &[("from", &from), ("to", &to)],
                                    )
                                }
                                Err(e) => {
                                    self.aliases.clear();
                                    self.alias_enabled.clear();
                                    self.alias_cursor = 0;
                                    self.loaded_section = None;
                                    crate::i18n::t_args(
                                        "zc-config-status-alias-renamed-refresh-failed",
                                        &[("from", &from), ("to", &to), ("err", &e.to_string())],
                                    )
                                }
                            }
                        } else {
                            if let Screen::AliasRename { confirming, .. } = &mut self.screen {
                                *confirming = false;
                            }
                            crate::i18n::t("zc-config-status-alias-rename-not-applied")
                        };
                        if !result.warnings.is_empty() {
                            status.push(' ');
                            status.push_str(&crate::i18n::t_args(
                                "zc-config-status-alias-rename-warnings",
                                &[("warnings", &result.warnings.join("; "))],
                            ));
                        }
                        self.status_msg = Some(status);
                    }
                    Err(e) => {
                        use crate::jsonrpc::{JsonRpcError, error_codes};
                        // Only method/parameter refusals establish that no mutation ran.
                        // A timeout, transport loss or internal error may follow a save.
                        let refused = e.downcast_ref::<JsonRpcError>().is_some_and(|error| {
                            matches!(
                                error.code,
                                error_codes::METHOD_NOT_FOUND | error_codes::INVALID_PARAMS
                            )
                        });
                        if refused {
                            if let Screen::AliasRename { confirming, .. } = &mut self.screen {
                                *confirming = false;
                            }
                            self.status_msg = Some(crate::i18n::t_args(
                                "zc-config-status-alias-rename-failed",
                                &[("err", &e.to_string())],
                            ));
                        } else {
                            // Retire the stale source before reading. Failed reconciliation
                            // must not leave a retryable mutation or cached aliases behind.
                            self.finish_alias_rename();
                            self.aliases.clear();
                            self.alias_enabled.clear();
                            self.alias_cursor = 0;
                            self.loaded_section = None;
                            self.status_msg = Some(match self.load_aliases(&map_path).await {
                                Ok(()) => {
                                    self.loaded_section = Some(section_idx);
                                    crate::i18n::t_args(
                                        "zc-config-status-alias-rename-unknown",
                                        &[("err", &e.to_string())],
                                    )
                                }
                                Err(refresh_error) => crate::i18n::t_args(
                                    "zc-config-status-alias-rename-unknown-refresh-failed",
                                    &[
                                        ("err", &e.to_string()),
                                        ("refresh_err", &refresh_error.to_string()),
                                    ],
                                ),
                            });
                        }
                    }
                }
            }
            Some(ConfigEditorAction::Backspace) if !confirming => {
                self.edit_buf.pop();
            }
            None if !confirming => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.edit_buf.push(c);
                }
            }
            _ => {}
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
            FilterAction::Cancel => return self.cancel_filter().await,
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
            Some(ConfigTabAction::RenameAlias) => self.start_alias_rename(),
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
                    prefix,
                    breadcrumb,
                    ..
                } = screen
                {
                    if self.is_typed_family_root_field_list(section_idx, &prefix) {
                        self.screen = Screen::TypeList { section_idx };
                    } else if breadcrumb.len() >= 2 {
                        self.restore_alias_list_from_field_back(section_idx, breadcrumb)
                            .await?;
                    }
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
            Some(ConfigTabAction::DeleteRow)
                if self
                    .fields
                    .get(self.field_cursor)
                    .is_some_and(|field| field.kind == PropKind::SecretMap) =>
            {
                // The container row is not itself a value; entries are
                // removed from their own `<path>.<KEY>` rows.
                self.status_msg = Some(crate::i18n::t("zc-config-secret-map-delete-hint"));
            }
            Some(ConfigTabAction::DeleteRow) => {
                if let Some(field) = self.fields.get(self.field_cursor) {
                    let prop = field.path.clone();
                    let saved_cursor = self.field_cursor;
                    if let Screen::FieldList { prefix, .. } = &self.screen {
                        let prefix = prefix.clone();
                        match self.rpc.config_delete(&prop).await {
                            Ok(revision) => {
                                self.application_feedback
                                    .record_saved(prop.clone(), revision);
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

    /// Drop the cached composite-tab data for a section prefix, without
    /// issuing any request. A save inside a composite section can retarget
    /// the data its Skills or Personality tab shows (a bundle's directory,
    /// an agent's workspace.path); clearing the cached list lets
    /// on_tab_switched refetch it on the next tab entry, so no request is
    /// issued unless the user goes there. Invalidate on every successful
    /// save in the section rather than only when the saved field is the
    /// retargeting one: a field-name check is brittle, and the cost of a
    /// wrong guess is one lazy list request.
    fn invalidate_composite_state(&mut self, prefix: &str) {
        if prefix.starts_with("agents.") {
            self.personality_files.clear();
            self.personality_active_file = None;
            self.personality_cursor = 0;
            self.personality_content.clear();
            self.personality_loaded.clear();
        } else if prefix.starts_with("skill-bundles.") {
            self.skills_list.clear();
            self.skills_active = None;
            self.skills_cursor = 0;
            self.skills_body.clear();
            self.skills_body_loaded.clear();
            self.skills_frontmatter = Default::default();
            self.skills_frontmatter_loaded = Default::default();
        }
    }

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
            None => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.personality_content.push(c);
                }
            }
            _ => {}
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
            None => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.skills_body.push(c);
                }
            }
            _ => {}
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
        if let Some(provider_ref) = model_field_catalog_reference(&field_path) {
            let family = provider_ref
                .split_once('.')
                .map_or(provider_ref.as_str(), |(family, _)| family)
                .to_string();

            // Show loading indicator before the blocking RPC call.
            self.status_msg = Some(crate::i18n::t_args(
                "zc-config-status-fetching-models",
                &[("family", &family)],
            ));
            let _ = self.draw(term);

            match self.rpc.catalog_models(&provider_ref).await {
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
                    self.status_msg = Some(crate::i18n::t("zc-config-status-model-fetch-failed"));
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
        self.secret_map_entry_key = None;
        let value = if self.fields[idx].populated {
            self.fields[idx]
                .value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(str::to_string)
        } else {
            None
        };
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
        self.edit_cursor = self.edit_buf.len();
    }

    fn is_select_edit(&self) -> bool {
        !self.select_items.is_empty()
    }

    // ── Filter helpers ───────────────────────────────────────────

    fn activate_filter(&mut self, filtered_len: usize) {
        let cursor = if self.zeroclaw_pane == ZeroclawPane::Sections {
            self.section_cursor
        } else {
            self.visible_cursor()
        };
        let detail = (self.zeroclaw_pane == ZeroclawPane::Sections).then(|| DetailOrigin {
            screen: self.screen.clone(),
            loaded_section: self.loaded_section,
            type_cursor: self.type_cursor,
            alias_cursor: self.alias_cursor,
            alias_tab: self.alias_tab,
            cost_cursor: self.cost_cursor,
            field_cursor: self.field_cursor,
        });
        self.filter_origin = Some(FilterOrigin {
            pane: self.zeroclaw_pane,
            section_cursor: self.section_cursor,
            active_tab: self.active_tab,
            detail,
        });
        self.filter = Some(String::new());
        self.filter_cursor = cursor.min(filtered_len.saturating_sub(1));
    }

    fn deactivate_filter(&mut self) {
        self.filter = None;
        self.filter_origin = None;
    }

    async fn cancel_filter(&mut self) -> Result<()> {
        let origin = self.filter_origin.take();
        self.deactivate_filter();
        if let Some(origin) = origin {
            self.zeroclaw_pane = origin.pane;
            self.section_cursor = origin.section_cursor;
            if origin.pane == ZeroclawPane::Sections {
                let restored = match origin.detail {
                    Some(detail) if detail.loaded_section != self.loaded_section => {
                        self.reopen_detail(detail, origin.active_tab).await
                    }
                    _ => self.preview_section(origin.section_cursor).await,
                };
                if let Err(err) = restored {
                    self.status_msg = Some(crate::i18n::t_args(
                        "zc-config-status-load-failed",
                        &[("err", &err.to_string())],
                    ));
                }
                self.active_tab = origin
                    .active_tab
                    .min(self.tab_names.len().saturating_sub(1));
            }
        }
        Ok(())
    }

    /// Reload the detail page saved when a sidebar search started, with its
    /// cursors and tab. Pages that hold unsaved input (an open editor or a
    /// create form) fall back to their section root.
    async fn reopen_detail(&mut self, detail: DetailOrigin, active_tab: usize) -> Result<()> {
        let Some(section_idx) = detail.loaded_section else {
            return Ok(());
        };
        match detail.screen {
            Screen::AliasList {
                section_idx,
                map_path,
                breadcrumb,
            } => {
                self.load_aliases(&map_path).await?;
                self.screen = Screen::AliasList {
                    section_idx,
                    map_path,
                    breadcrumb,
                };
                if detail.alias_tab == 1 {
                    self.alias_tab = 1;
                    self.load_cost_resources().await?;
                }
                self.alias_cursor = detail.alias_cursor.min(self.aliases.len());
                self.cost_cursor = detail.cost_cursor.min(self.cost_resources.len());
            }
            Screen::FieldList {
                section_idx,
                prefix,
                breadcrumb,
            } => {
                self.load_fields(&prefix).await?;
                self.screen = Screen::FieldList {
                    section_idx,
                    prefix,
                    breadcrumb,
                };
                self.field_cursor = detail.field_cursor.min(self.fields.len().saturating_sub(1));
                self.active_tab = active_tab.min(self.tab_names.len().saturating_sub(1));
            }
            _ => {
                self.load_section_content(section_idx).await?;
                self.type_cursor = detail
                    .type_cursor
                    .min(self.type_list_len(section_idx).saturating_sub(1));
            }
        }
        self.loaded_section = Some(section_idx);
        self.status_msg = None;
        Ok(())
    }

    fn filter_for(&self, pane: ZeroclawPane) -> Option<&String> {
        self.filter_origin
            .as_ref()
            .filter(|origin| origin.pane == pane)
            .and(self.filter.as_ref())
    }

    fn detail_filter(&self) -> Option<&String> {
        self.filter_for(ZeroclawPane::Detail)
    }

    fn filtered_section_indices<S: AsRef<str>>(&self, items: &[S]) -> Vec<usize> {
        Self::indices_matching(items, self.filter_for(ZeroclawPane::Sections))
    }

    fn section_filter_has_no_matches(&self) -> bool {
        self.filter_for(ZeroclawPane::Sections).is_some()
            && self
                .filtered_section_indices(
                    &self.sections.iter().map(|s| &s.label).collect::<Vec<_>>(),
                )
                .is_empty()
    }

    fn filtered_indices<S: AsRef<str>>(&self, items: &[S]) -> Vec<usize> {
        Self::indices_matching(items, self.detail_filter())
    }

    fn indices_matching<S: AsRef<str>>(items: &[S], query: Option<&String>) -> Vec<usize> {
        match query {
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
        use crate::keymap::{ConfigTabAction, SearchBoxAction};
        if self.filter.is_none() {
            if ConfigTabAction::from_chord(&key) == Some(ConfigTabAction::BeginSearch) {
                self.activate_filter(filtered_len);
                return FilterAction::Consumed;
            }
            return FilterAction::Passthrough;
        }
        let editor_chord = match SearchBoxAction::from_chord(&key) {
            Some(SearchBoxAction::Cancel) => Some(FilterEditAction::Cancel),
            Some(SearchBoxAction::Accept) => Some(FilterEditAction::Accept),
            Some(SearchBoxAction::Backspace) => Some(FilterEditAction::Backspace),
            Some(SearchBoxAction::Up) => Some(FilterEditAction::CursorUp),
            Some(SearchBoxAction::Down) => Some(FilterEditAction::CursorDown),
            None => None,
        };
        match editor_chord {
            Some(FilterEditAction::Cancel) => FilterAction::Cancel,
            Some(FilterEditAction::Accept) => FilterAction::Accept,
            Some(FilterEditAction::Backspace) => {
                if let Some(buf) = &mut self.filter {
                    buf.pop();
                    self.filter_cursor = 0;
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

    fn insert_scalar_edit_text(&mut self, text: &str) {
        self.edit_buf.insert_str(self.edit_cursor, text);
        self.edit_cursor += text.len();
        self.edit_cursor =
            crate::text_navigation::normalize_grapheme_cursor(&self.edit_buf, self.edit_cursor);
    }

    fn backspace_scalar_edit(&mut self) {
        let previous =
            crate::text_navigation::previous_grapheme_boundary(&self.edit_buf, self.edit_cursor);
        self.edit_buf.replace_range(previous..self.edit_cursor, "");
        self.edit_cursor =
            crate::text_navigation::normalize_grapheme_cursor(&self.edit_buf, previous);
    }

    fn move_scalar_edit_cursor(&mut self, action: crate::keymap::ConfigEditorAction) {
        use crate::keymap::ConfigEditorAction;
        self.edit_cursor = match action {
            ConfigEditorAction::CursorLeft => {
                crate::text_navigation::previous_grapheme_boundary(&self.edit_buf, self.edit_cursor)
            }
            ConfigEditorAction::CursorRight => {
                crate::text_navigation::next_grapheme_boundary(&self.edit_buf, self.edit_cursor)
            }
            ConfigEditorAction::CursorWordLeft => {
                crate::text_navigation::previous_word_boundary(&self.edit_buf, self.edit_cursor)
            }
            ConfigEditorAction::CursorWordRight => {
                crate::text_navigation::next_word_boundary(&self.edit_buf, self.edit_cursor)
            }
            ConfigEditorAction::CursorStart => 0,
            ConfigEditorAction::CursorEnd => self.edit_buf.len(),
            _ => self.edit_cursor,
        };
    }

    /// Advance the two-step "add entry" flow on a `PropKind::SecretMap`
    /// row. Step 1 takes the entry key; step 2 takes its value and writes
    /// `<map_path>.<key>`. An existing key is simply replaced. Empty values
    /// are refused here because the daemon treats "" as "remove entry".
    async fn confirm_secret_map_step(&mut self, map_path: &str) -> Result<()> {
        let Some(key) = self.secret_map_entry_key.clone() else {
            let key = self.edit_buf.trim().to_string();
            if let Some(status_key) = secret_map_key_validation_key(&key) {
                self.status_msg = Some(crate::i18n::t(status_key));
                return Ok(());
            }
            self.secret_map_entry_key = Some(key);
            self.edit_buf.clear();
            self.edit_cursor = 0;
            self.status_msg = None;
            return Ok(());
        };
        if self.edit_buf.is_empty() {
            self.status_msg = Some(crate::i18n::t("zc-config-secret-map-value-empty"));
            return Ok(());
        }
        let prop = format!("{map_path}.{key}");
        let value = serde_json::Value::String(self.edit_buf.clone());
        match self.rpc.config_set(&prop, value).await {
            Ok(revision) => {
                self.application_feedback
                    .record_saved(prop.clone(), revision);
                self.secret_map_entry_key = None;
                self.edit_buf.clear();
                self.edit_cursor = 0;
                // The application summary below names the saved field.
                self.status_msg = None;
                self.pop_to_field_list_keep_cursor().await?;
            }
            Err(e) => {
                self.status_msg = Some(crate::i18n::t_args(
                    "zc-config-status-set-failed",
                    &[("err", &e.to_string())],
                ));
            }
        }
        Ok(())
    }

    fn is_scalar_field_edit(&self) -> bool {
        matches!(&self.screen, Screen::FieldEdit { field_idx, .. }
            if !self.is_select_edit()
                && self.fields.get(*field_idx).is_some_and(|field| field.kind != PropKind::StringArray))
    }

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
                    if let Screen::FieldEdit { field_idx, .. } = &self.screen {
                        let prop = self.fields[*field_idx].path.clone();
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
                            Ok(revision) => {
                                self.application_feedback
                                    .record_saved(prop.clone(), revision);
                                // The application summary below names the saved field.
                                self.status_msg = None;
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
                None => {
                    if let KeyCode::Char(c) = key.code
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.edit_buf.push(c);
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        match action {
            Some(ConfigEditorAction::Cancel) => {
                self.pop_to_field_list().await?;
            }
            Some(ConfigEditorAction::Confirm) => {
                if let Screen::FieldEdit { field_idx, .. } = &self.screen
                    && self.fields[*field_idx].kind == PropKind::SecretMap
                {
                    let map_path = self.fields[*field_idx].path.clone();
                    return self.confirm_secret_map_step(&map_path).await;
                }
                if let Screen::FieldEdit { field_idx, .. } = &self.screen {
                    let field = &self.fields[*field_idx];
                    if let Some(status) =
                        scalar_validation_status(field.kind, &self.edit_buf, &field.path)
                    {
                        self.status_msg = Some(status);
                        return Ok(());
                    }
                    let prop = field.path.clone();
                    let value = serde_json::Value::String(self.edit_buf.clone());
                    match self.rpc.config_set(&prop, value).await {
                        Ok(revision) => {
                            self.application_feedback
                                .record_saved(prop.clone(), revision);
                            // The application summary below names the saved field.
                            self.status_msg = None;
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
                self.backspace_scalar_edit();
            }
            Some(
                action @ (ConfigEditorAction::CursorLeft
                | ConfigEditorAction::CursorRight
                | ConfigEditorAction::CursorWordLeft
                | ConfigEditorAction::CursorWordRight
                | ConfigEditorAction::CursorStart
                | ConfigEditorAction::CursorEnd),
            ) => {
                self.move_scalar_edit_cursor(action);
            }
            None => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    let mut encoded = [0; 4];
                    self.insert_scalar_edit_text(c.encode_utf8(&mut encoded));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_select_edit(&mut self, key: KeyEvent) -> Result<()> {
        let visible = self.filtered_indices(&self.select_items);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Cancel => return self.cancel_filter().await,
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
            && let Screen::FieldEdit { field_idx, .. } = &self.screen
        {
            let prop = self.fields[*field_idx].path.clone();
            let value = serde_json::Value::String(chosen.clone());
            match self.rpc.config_set(&prop, value).await {
                Ok(revision) => {
                    self.application_feedback
                        .record_saved(prop.clone(), revision);
                    // The application summary below names the saved field.
                    self.status_msg = None;
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
            // This silent reload is the only refresh after a successful save.
            // Callers must not run load_fields() first: it resets active_tab
            // and the composite-tab state and issues a second config/list.
            // The composite-tab state is invalidated right after it for the
            // same reason: the removed load_fields() call used to clear it.
            self.reload_fields_silent(&prefix).await;
            self.invalidate_composite_state(&prefix);
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

    fn group_rank(section: &ConfigSectionEntry) -> usize {
        LEGACY_GROUPS
            .iter()
            .position(|(_, key)| section_group_key(section) == Some(*key))
            .unwrap_or(LEGACY_GROUPS.len() - 1)
    }

    fn draw_sections_pane(&mut self, frame: &mut Frame, area: Rect, active: bool) {
        use ratatui::layout::{Constraint, Direction, Layout};
        // Reserve one line at the top for the filter bar when filtering.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        if let Some(buf) = self.filter_for(ZeroclawPane::Sections) {
            render_filter_bar(
                frame,
                rows[0],
                buf,
                &crate::i18n::t("zc-config-filter-sections"),
            );
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
        let visible = self.filtered_section_indices(&labels);

        let grouped = self.filter_for(ZeroclawPane::Sections).is_none()
            && self
                .sections
                .iter()
                .any(|s| !s.group_key.is_empty() || !s.group.is_empty());
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

        let cursor = if self.filter_for(ZeroclawPane::Sections).is_some() {
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
            (theme::selection_highlight(true, false), "\u{203a} ")
        } else {
            (theme::selection_highlight(false, false), "  ")
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
        if self.section_filter_has_no_matches() {
            render_no_matches(frame, list_area);
        }
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

        self.last_breadcrumb_areas =
            render_breadcrumb(frame, r.breadcrumb, std::slice::from_ref(&section.label));

        if let Some(buf) = self.detail_filter() {
            render_filter_bar(frame, r.help, buf, &crate::i18n::t("zc-config-filter-list"));
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(&section.help, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
        }

        let type_names = self.type_list_labels(section_idx);
        let visible = self.filtered_indices(&type_names);

        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let name = &type_names[i];
                let count = self
                    .template_idx_for_type_row(section_idx, i)
                    .and_then(|template_idx| self.type_alias_counts.get(template_idx).copied())
                    .unwrap_or(0);
                let mut spans = vec![Span::styled(name.to_string(), theme::body_style())];
                if count > 0 {
                    spans.push(Span::styled(format!("  ({count})"), theme::accent_style()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let cursor = if self.detail_filter().is_some() {
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

        if self.detail_filter().is_some() && visible.is_empty() {
            render_no_matches(frame, r.main);
        }
        self.draw_status(frame, r);
    }

    fn draw_alias_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        section_idx: usize,
        breadcrumb: &[String],
    ) {
        let section = &self.sections[section_idx];
        let guidance =
            if self.aliases.is_empty() && self.detail_filter().is_none() && self.alias_tab == 0 {
                setup_section_key(&section.key).map(|key| {
                    format!(
                        "{}\n{}\n{}",
                        crate::i18n::t("zc-config-setup-empty"),
                        crate::i18n::t(key),
                        crate::i18n::t_args(
                            "zc-config-setup-add",
                            &[("keys", &tab_key(crate::keymap::ConfigTabAction::Enter))],
                        ),
                    )
                })
            } else {
                None
            };
        let mut r = guidance.as_ref().map_or_else(
            || {
                if self.detail_filter().is_some()
                    && self.filtered_indices(&self.aliases).is_empty()
                    && self.alias_list_has_tabs()
                {
                    regions_with_help(area, 3)
                } else {
                    regions(area)
                }
            },
            |text| guidance_regions(area, text, self.alias_list_has_tabs()),
        );

        let mut bc: Vec<String> = Vec::new();
        bc.extend(breadcrumb.iter().cloned());
        self.last_breadcrumb_areas = render_breadcrumb(frame, r.breadcrumb, &bc);

        // Aliases/Costs tab bar on cost-bearing provider types. Reuses the
        // same two-row help split the FieldList tab bar uses.
        let has_tabs = self.alias_list_has_tabs();
        let tab_area = if has_tabs {
            use ratatui::layout::{Constraint, Direction, Layout};
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
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

        if let Some(buf) = self.detail_filter() {
            render_filter_bar(frame, r.help, buf, &crate::i18n::t("zc-config-filter-list"));
        } else {
            frame.render_widget(
                Paragraph::new(Text::styled(
                    guidance.as_deref().unwrap_or(&section.help),
                    theme::dim_style(),
                ))
                .wrap(Wrap { trim: false }),
                r.help,
            );
        }

        let visible = self.filtered_indices(&self.aliases);
        if self.detail_filter().is_some() && visible.is_empty() && r.help.height > 1 {
            frame.render_widget(
                Paragraph::new(crate::i18n::t("zc-config-setup-no-filter-matches"))
                    .style(theme::dim_style()),
                Rect::new(r.help.x, r.help.y + 1, r.help.width, r.help.height - 1),
            );
        }

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
        if self.detail_filter().is_none() {
            items.push(ListItem::new(Line::from(Span::styled(
                "[+ Add]",
                theme::accent_style(),
            ))));
        }

        let cursor = if self.detail_filter().is_some() {
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

        if self.detail_filter().is_some() && visible.is_empty() {
            render_no_matches(frame, r.main);
        }
        self.draw_status(frame, r);
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
        self.draw_status(frame, r);
    }

    fn draw_alias_create(&mut self, frame: &mut Frame, area: Rect, breadcrumb: &[String]) {
        let setup_section = match &self.screen {
            // Rate sheets created from the Costs tab are not aliases.
            Screen::AliasCreate {
                section_idx,
                map_path,
                ..
            } if !map_path.starts_with("cost.") => self
                .sections
                .get(*section_idx)
                .and_then(|section| setup_section_key(&section.key)),
            _ => None,
        };
        // One paragraph: the narrow create pane has no row to spare for a break.
        let hint = if let Some(key) = setup_section {
            format!(
                "{} {}",
                crate::i18n::t(key),
                crate::i18n::t_args(
                    "zc-config-setup-create",
                    &[
                        (
                            "confirm",
                            &editor_key(crate::keymap::ConfigEditorAction::Confirm)
                        ),
                        (
                            "cancel",
                            &editor_key(crate::keymap::ConfigEditorAction::Cancel)
                        ),
                    ],
                ),
            )
        } else {
            crate::i18n::t("zc-config-alias-create-hint")
        };
        let r = guidance_regions(area, &hint, false);

        let mut bc: Vec<String> = Vec::new();
        bc.extend(breadcrumb.iter().cloned());
        bc.push(crate::i18n::t("zc-config-breadcrumb-new"));
        self.last_breadcrumb_areas = render_breadcrumb(frame, r.breadcrumb, &bc);

        frame.render_widget(
            Paragraph::new(Span::styled(hint, theme::dim_style())).wrap(Wrap { trim: false }),
            r.help,
        );

        let input_display = format!("{}{}", self.edit_buf, "█");
        let input = Paragraph::new(Line::from(Span::styled(
            input_display,
            theme::input_style(),
        )))
        .block(theme::panel_block(" Alias name "));
        frame.render_widget(input, r.main);

        self.draw_status(frame, r);
    }

    fn draw_alias_rename(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        map_path: &str,
        breadcrumb: &[String],
        from: &str,
        confirming: bool,
    ) {
        let r = regions(area);
        let mut bc = breadcrumb.to_vec();
        bc.push(from.to_string());
        bc.push(crate::i18n::t("zc-config-breadcrumb-rename"));
        self.last_breadcrumb_areas = render_breadcrumb(frame, r.breadcrumb, &bc);
        frame.render_widget(
            Paragraph::new(Span::styled(
                crate::i18n::t(if confirming {
                    "zc-config-alias-rename-confirm-hint"
                } else {
                    "zc-config-alias-rename-input-hint"
                }),
                theme::dim_style(),
            ))
            .wrap(Wrap { trim: false }),
            r.help,
        );
        let content = if confirming {
            crate::i18n::t_args(
                "zc-config-alias-rename-confirm-target",
                &[("path", map_path), ("from", from), ("to", &self.edit_buf)],
            )
        } else {
            format!("{}█", self.edit_buf)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(content, theme::input_style()))
                .wrap(Wrap { trim: false })
                .block(theme::panel_block(&format!(
                    " {} ",
                    crate::i18n::t(if confirming {
                        "zc-config-alias-rename-confirm-title"
                    } else {
                        "zc-config-alias-rename-input-title"
                    })
                ))),
            r.main,
        );
        self.last_main_area = r.main;
        self.last_tab_area = None;
        self.draw_status(frame, r);
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
        self.last_breadcrumb_areas = render_breadcrumb(frame, r.breadcrumb, &bc);

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

        if let Some(buf) = self.detail_filter() {
            render_filter_bar(frame, r.help, buf, &crate::i18n::t("zc-config-filter-list"));
        } else if let Some(field) = self.fields.get(self.field_cursor) {
            frame.render_widget(
                Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
        }

        let cursor = if self.detail_filter().is_some() {
            self.filter_cursor
        } else {
            visible
                .iter()
                .position(|&i| i == self.field_cursor)
                .unwrap_or(0)
        };
        let selected_field = visible.get(cursor).copied();

        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let f = &self.fields[i];
                let short_name =
                    &tab_names[tab_indices.iter().position(|&ti| ti == i).unwrap_or(0)];
                let val_display = if f.kind == PropKind::SecretMap {
                    // Entries are listed as their own `<path>.<KEY>` rows;
                    // the container row is the add affordance.
                    crate::i18n::t("zc-config-secret-map-add")
                } else if f.is_secret {
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

                let setup_marker = f
                    .setup
                    .as_ref()
                    .map(|setup| {
                        let key = if setup.missing {
                            "zc-config-setup-required-missing"
                        } else {
                            "zc-config-setup-required"
                        };
                        format!(" [{}]", crate::i18n::t(key))
                    })
                    .unwrap_or_default();
                let env_marker = if f.is_env_overridden { " [env]" } else { "" };
                let press_hint = if Some(i) == selected_field {
                    let enter_key = tab_key(crate::keymap::ConfigTabAction::Enter);
                    format!(
                        "  \u{2500}\u{2192} {}",
                        crate::i18n::t_args("zc-config-field-edit-hint", &[("keys", &enter_key)])
                    )
                } else {
                    String::new()
                };
                let line =
                    format!("{short_name}{setup_marker} = {val_display}{env_marker}{press_hint}");

                let style = if f.populated {
                    theme::body_style()
                } else {
                    theme::dim_style()
                };
                ListItem::new(Line::from(Span::styled(line, style)))
            })
            .collect();

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

        if self.detail_filter().is_some() && visible.is_empty() {
            render_no_matches(frame, r.main);
        }
        self.draw_status(frame, r);
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

            self.draw_status(frame, r);
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

            self.draw_status(frame, r);
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

            self.draw_status(frame, r);
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

            self.draw_status(frame, r);
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
        self.last_breadcrumb_areas = render_breadcrumb(frame, r.breadcrumb, &bc);

        if self.is_select_edit() {
            // Enum, Bool, or model select — with optional `/` filter.
            if let Some(buf) = self.detail_filter() {
                render_filter_bar(
                    frame,
                    r.help,
                    buf,
                    &crate::i18n::t("zc-config-filter-options"),
                );
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

            let cursor = if self.detail_filter().is_some() {
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
                    .highlight_style(theme::selection_highlight(true, false))
                    .highlight_symbol("\u{203a} "),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();
            self.last_tab_area = None;

            if self.detail_filter().is_some() && visible.is_empty() {
                render_no_matches(frame, r.main);
            }
            self.draw_status(frame, r);
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
                self.draw_status(frame, r);
                return;
            }

            if field.kind == PropKind::SecretMap {
                // Two-step add: key in clear text, then its value masked.
                let (hint, masked, title) = match &self.secret_map_entry_key {
                    None => (
                        crate::i18n::t("zc-config-secret-map-key-prompt"),
                        false,
                        format!(" {short_name} \u{203a} + "),
                    ),
                    Some(key) => (
                        crate::i18n::t_args(
                            "zc-config-secret-map-value-prompt",
                            &[("key", key.as_str())],
                        ),
                        true,
                        format!(" {short_name} \u{203a} {key} "),
                    ),
                };
                let input_display = scalar_edit_display(&self.edit_buf, self.edit_cursor, masked);
                let input = Paragraph::new(vec![
                    Line::from(Span::styled(hint, theme::dim_style())),
                    Line::from(Span::styled(input_display, theme::input_style())),
                ])
                .block(theme::panel_block(&title));
                frame.render_widget(input, r.main);
                self.draw_status(frame, r);
                return;
            }

            let input_display =
                scalar_edit_display(&self.edit_buf, self.edit_cursor, field.is_secret);

            let input = Paragraph::new(vec![
                Line::from(Span::styled(&kind_hint, theme::dim_style())),
                Line::from(Span::styled(input_display, theme::input_style())),
            ])
            .block(theme::panel_block(&format!(" {short_name} ")));

            frame.render_widget(input, r.main);

            self.draw_status(frame, r);
        }
    }

    fn draw_status(&self, frame: &mut Frame, r: Regions) {
        // The action hint is unified at the pane bottom; only transient status
        // messages render inline with the active detail pane.
        if let Some(msg) = &self.status_msg {
            frame.render_widget(
                Paragraph::new(Span::styled(msg.as_str(), theme::warn_style())),
                r.status,
            );
        }
    }

    /// Handle a bracketed-paste payload. Routes pasted text into whichever
    /// text-input surface is currently active (filter, edit buffer, alias
    /// create/rename, personality/skills editor). Filters out the bracket-paste
    /// terminator bytes and normalises CRLF.
    pub(crate) async fn handle_paste(&mut self, text: &str) {
        if self.application_feedback.details_open() {
            return;
        }
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
            self.filter_cursor = 0;
            if self.filter_for(ZeroclawPane::Sections).is_some() {
                let labels: Vec<&str> = self.sections.iter().map(|s| s.label.as_str()).collect();
                if let Some(&orig) = self.filtered_section_indices(&labels).first() {
                    self.section_cursor = orig;
                    if let Err(err) = self.preview_section(orig).await {
                        self.status_msg = Some(crate::i18n::t_args(
                            "zc-config-status-load-failed",
                            &[("err", &err.to_string())],
                        ));
                    }
                }
            }
            return;
        }

        match &self.screen {
            Screen::AliasCreate { .. }
            | Screen::AliasRename {
                confirming: false, ..
            } => {
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
                    let scalar: String = cleaned.chars().filter(|c| *c != '\n').collect();
                    self.insert_scalar_edit_text(&scalar);
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

    /// Whether the pane owns text-input keys (filter, edit buf, alias create/rename, editors).
    pub(crate) fn wants_text_input(&self) -> bool {
        if self.section == ConfigSection::Zerocode {
            return self.zerocode.wants_text_input();
        }
        if self.filter.is_some() {
            return true;
        }
        match &self.screen {
            Screen::AliasCreate { .. } | Screen::AliasRename { .. } => true,
            Screen::FieldEdit { .. } if !self.is_select_edit() => true,
            Screen::FieldList { .. } => {
                self.personality_active_file.is_some() || self.skills_active.is_some()
            }
            _ => false,
        }
    }

    pub(crate) fn claims_pane_navigation(&self, key: &KeyEvent) -> bool {
        self.section == ConfigSection::Zeroclaw
            && self.zeroclaw_pane == ZeroclawPane::Detail
            && self.is_scalar_field_edit()
            && matches!(
                crate::keymap::ConfigEditorAction::from_chord(key),
                Some(
                    crate::keymap::ConfigEditorAction::CursorWordLeft
                        | crate::keymap::ConfigEditorAction::CursorWordRight
                )
            )
    }

    pub(crate) fn claims_session_shortcut(&self, key: &KeyEvent) -> bool {
        if self.section == ConfigSection::Zerocode {
            return self.zerocode.claims_session_shortcut(key);
        }
        self.wants_text_input()
            || crate::keymap::ConfigTabAction::from_chord(key).is_some()
            || crate::keymap::ConfigEditorAction::from_chord(key).is_some()
    }
}

fn scalar_edit_display(text: &str, cursor: usize, secret: bool) -> String {
    let cursor = crate::text_navigation::normalize_grapheme_cursor(text, cursor);
    if secret {
        let before = text[..cursor].graphemes(true).count();
        let after = text[cursor..].graphemes(true).count();
        let mut display = String::with_capacity((before + after + 1) * '•'.len_utf8());
        for _ in 0..before {
            display.push('•');
        }
        display.push('█');
        for _ in 0..after {
            display.push('•');
        }
        display
    } else {
        format!("{}█{}", &text[..cursor], &text[cursor..])
    }
}

impl crate::widgets::HelpContext for App {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::ConfigTabAction as A;
        use crate::widgets::HelpEntry as E;
        if self.application_feedback.details_open() {
            return crate::widgets::HelpNode::entries(vec![
                E::new(nav_keys_split(), crate::i18n::t("zc-config-help-navigate")),
                E::new(tab_keys(A::Back), crate::i18n::t("zc-config-help-back")),
            ]);
        }
        // Section switch is available in either sub-tab.
        let section_nav = E::new(
            [tab_keys(A::SectionNext), tab_keys(A::SectionPrev)].concat(),
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
        let help = || {
            E::new(
                crate::keymap::action_key_labels(crate::keymap::GlobalAction::Help),
                crate::i18n::t("zc-config-help-this-help"),
            )
        };
        let filter = || {
            E::new(
                tab_keys(A::BeginSearch),
                crate::i18n::t("zc-config-help-filter"),
            )
        };
        let clear_filter = || k(A::Back, "zc-config-help-clear-filter");
        let back = || k(A::Back, "zc-config-help-back");
        let mouse_open = || E::key("Mouse", crate::i18n::t("zc-config-help-mouse-open"));

        if self.zeroclaw_pane == ZeroclawPane::Sections {
            let mut entries = vec![nav(), k(A::Enter, "zc-config-help-open-section")];
            if self.filter_for(ZeroclawPane::Sections).is_some() {
                entries.push(clear_filter());
            } else {
                entries.push(filter());
            }
            entries.push(help());
            entries.push(mouse_open());
            return HelpNode::entries(entries);
        }

        match &self.screen {
            Screen::SectionList => {
                if self.detail_filter().is_some() {
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
                if self.detail_filter().is_some() {
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
                if self.detail_filter().is_some() {
                    HelpNode::entries(vec![
                        nav(),
                        k(A::Enter, "zc-config-help-open-alias"),
                        clear_filter(),
                        help(),
                    ])
                } else if self.alias_list_has_tabs() {
                    let mut entries = vec![
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
                    ];
                    if self.provider_alias_rename_available() {
                        entries.insert(3, k(A::RenameAlias, "zc-config-help-rename-alias"));
                    }
                    HelpNode::entries(entries)
                } else {
                    let open = [tab_keys(A::Enter), tab_keys(A::TabRight)].concat();
                    let mut entries = vec![
                        nav(),
                        E::new(open, crate::i18n::t("zc-config-help-open-alias")),
                        k(A::ToggleSecret, "zc-config-help-delete-alias"),
                        filter(),
                        back(),
                        help(),
                        E::spacer(),
                        mouse_open(),
                    ];
                    if self.provider_alias_rename_available() {
                        entries.insert(2, k(A::RenameAlias, "zc-config-help-rename-alias"));
                    }
                    HelpNode::entries(entries)
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
            Screen::AliasRename { confirming, .. } => HelpNode::entries(vec![
                E::new(
                    vec![editor_key(crate::keymap::ConfigEditorAction::Confirm)],
                    crate::i18n::t(if *confirming {
                        "zc-config-help-confirm-rename"
                    } else {
                        "zc-config-help-review-rename"
                    }),
                ),
                E::new(
                    vec![editor_key(crate::keymap::ConfigEditorAction::Cancel)],
                    crate::i18n::t("zc-config-help-cancel"),
                ),
                help(),
            ]),
            Screen::FieldList { .. } => {
                if self.detail_filter().is_some() {
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
                    if self.detail_filter().is_some() {
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
        if self.provider_alias_rename_available() {
            entries.push(E::new(
                tab_keys(A::RenameAlias),
                crate::i18n::t("zc-config-help-rename-alias"),
            ));
        }
        entries.push(E::new(
            tab_keys(A::DeleteRow),
            crate::i18n::t("zc-config-help-reset-default"),
        ));
        entries.push(E::new(
            tab_keys(A::ApplicationStatus),
            crate::i18n::t("zc-config-application-view"),
        ));
        entries.push(E::new(
            tab_keys(A::BeginSearch),
            crate::i18n::t("zc-config-help-filter"),
        ));
        entries.push(E::new(
            tab_keys(A::Back),
            crate::i18n::t("zc-config-help-back"),
        ));
        entries.push(E::new(
            crate::keymap::action_key_labels(crate::keymap::GlobalAction::Help),
            crate::i18n::t("zc-config-help-this-help"),
        ));
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

fn setup_section_key(section: &str) -> Option<&'static str> {
    match section {
        "providers.models" => Some("zc-config-setup-model-providers"),
        "providers.tts" => Some("zc-config-setup-tts-providers"),
        "model_routes" | "embedding_routes" => Some("zc-config-setup-routes"),
        _ => None,
    }
}

fn guidance_regions(area: Rect, text: &str, has_tabs: bool) -> Regions {
    let height = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(area.width.max(1))
        .saturating_add(usize::from(has_tabs));
    regions_with_help(
        area,
        height
            .min(usize::from(area.height.saturating_sub(6)))
            .max(2) as u16,
    )
}

fn regions(area: Rect) -> Regions {
    regions_with_help(area, 2)
}

fn regions_with_help(area: Rect, help_height: u16) -> Regions {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),           // breadcrumb
            Constraint::Length(help_height), // help
            Constraint::Min(4),              // main
            Constraint::Length(1),           // status
        ])
        .split(area);

    Regions {
        breadcrumb: chunks[0],
        help: chunks[1],
        main: chunks[2],
        status: chunks[3],
    }
}

fn render_filter_bar(frame: &mut Frame, area: Rect, buf: &str, scope: &str) {
    let display = format!("{scope}: /{buf}█");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(display, theme::input_style()))),
        area,
    );
}

fn render_no_matches(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(crate::i18n::t("zc-config-filter-no-matches"))
            .style(theme::dim_style())
            .block(theme::panel_block(" ")),
        area,
    );
}

fn render_breadcrumb(frame: &mut Frame, area: Rect, segments: &[String]) -> Vec<Rect> {
    use unicode_width::UnicodeWidthStr;
    let mut hit_areas = Vec::with_capacity(segments.len());
    let mut x = area.x;
    let mut spans: Vec<Span<'_>> = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ›  ", theme::dim_style()));
            x = x.saturating_add(5);
        }
        let style = if i == segments.len() - 1 {
            theme::accent_style().add_modifier(Modifier::BOLD)
        } else {
            theme::heading_style()
        };
        let width = UnicodeWidthStr::width(seg.as_str()).min(u16::MAX as usize) as u16;
        hit_areas.push(Rect::new(x, area.y, width, area.height.min(1)).intersection(area));
        x = x.saturating_add(width);
        spans.push(Span::styled(seg.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    hit_areas
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
        Print(mouse_shift_capture_sequence(false)),
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
    let _ = execute!(
        term.backend_mut(),
        EnterAlternateScreen,
        Print(mouse_shift_capture_sequence(true))
    );
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
    fn mouse_shift_capture_uses_xtshiftescape_sequences() {
        assert_eq!(mouse_shift_capture_sequence(true), "\x1b[>1s");
        assert_eq!(mouse_shift_capture_sequence(false), "\x1b[>0s");
    }

    #[test]
    fn model_field_catalog_reference_preserves_typed_alias() {
        assert_eq!(
            model_field_catalog_reference("providers.models.hailo_ollama.edge.model"),
            Some("hailo_ollama.edge".to_string())
        );
        assert_eq!(
            model_field_catalog_reference("providers.models.openai.default.model"),
            Some("openai.default".to_string())
        );
        assert_eq!(
            model_field_catalog_reference("providers.models.ollama.default"),
            None
        );
    }

    #[test]
    fn config_scalar_validation_rejects_invalid_integer() {
        assert_eq!(
            scalar_validation_status_key(PropKind::Integer, "20a"),
            Some("zc-config-status-invalid-integer")
        );
        assert_eq!(scalar_validation_status_key(PropKind::Integer, "20"), None);
    }

    #[test]
    fn config_scalar_validation_rejects_invalid_float() {
        assert_eq!(
            scalar_validation_status_key(PropKind::Float, "0.7x"),
            Some("zc-config-status-invalid-float")
        );
        assert_eq!(scalar_validation_status_key(PropKind::Float, "0.7"), None);
    }

    #[tokio::test]
    async fn scalar_field_edit_supports_cursor_insertion_and_word_navigation() {
        let mut manager = test_manager();
        let mut editable = field("example.name");
        editable.value = Some(serde_json::Value::String("alpha beta".into()));
        editable.populated = true;
        manager.fields = vec![editable];
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "example".into(),
            breadcrumb: vec!["example".into()],
            field_idx: 0,
        };
        manager.prepare_edit_at(0);

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(manager.edit_cursor, "alpha ".len());

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(manager.edit_buf, "alpha Xbeta");
        assert_eq!(manager.edit_cursor, "alpha X".len());

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(manager.edit_buf, "alpha beta");
        assert_eq!(manager.edit_cursor, "alpha ".len());
    }

    #[tokio::test]
    async fn pane_navigation_claims_only_scalar_field_edits() {
        let mut manager = test_manager();
        manager.fields = vec![field("example.name")];
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "example".into(),
            breadcrumb: vec!["example".into()],
            field_idx: 0,
        };
        manager.zeroclaw_pane = ZeroclawPane::Detail;
        let word_left = KeyEvent::new(KeyCode::Left, KeyModifiers::ALT);

        assert!(manager.claims_pane_navigation(&word_left));

        manager.section = ConfigSection::Zerocode;
        assert!(!manager.claims_pane_navigation(&word_left));

        manager.section = ConfigSection::Zeroclaw;
        manager.zeroclaw_pane = ZeroclawPane::Sections;
        assert!(!manager.claims_pane_navigation(&word_left));

        manager.zeroclaw_pane = ZeroclawPane::Detail;
        manager.select_items = vec!["first".into(), "second".into()];
        assert!(!manager.claims_pane_navigation(&word_left));

        manager.select_items.clear();
        manager.fields[0].kind = PropKind::StringArray;
        assert!(!manager.claims_pane_navigation(&word_left));

        manager.screen = Screen::FieldList {
            section_idx: 0,
            prefix: "example".into(),
            breadcrumb: vec!["example".into()],
        };
        assert!(!manager.claims_pane_navigation(&word_left));
    }

    #[test]
    fn scalar_field_cursor_display_masks_by_grapheme_without_losing_position() {
        let text = "e\u{301}x";
        let cursor = "e\u{301}".len();
        assert_eq!(scalar_edit_display(text, cursor, false), "e\u{301}█x");
        assert_eq!(scalar_edit_display(text, cursor, true), "•█•");
    }

    #[tokio::test]
    async fn scalar_field_insertion_normalizes_zwj_cursor_and_backspace() {
        let mut manager = test_manager();
        manager.edit_buf = "👩👩".into();
        manager.edit_cursor = "👩".len();

        manager.insert_scalar_edit_text("\u{200d}");
        assert_eq!(manager.edit_buf, "👩\u{200d}👩");
        assert_eq!(manager.edit_cursor, manager.edit_buf.len());
        assert_eq!(
            scalar_edit_display(&manager.edit_buf, manager.edit_cursor, true),
            "•█"
        );

        manager.backspace_scalar_edit();
        assert_eq!(manager.edit_buf, "");
        assert_eq!(manager.edit_cursor, 0);
    }

    #[tokio::test]
    async fn scalar_field_backspace_normalizes_a_joined_grapheme() {
        let mut manager = test_manager();
        manager.edit_buf = "🇺x🇸".into();
        manager.edit_cursor = "🇺x".len();

        manager.backspace_scalar_edit();

        assert_eq!(manager.edit_buf, "🇺🇸");
        assert_eq!(manager.edit_cursor, manager.edit_buf.len());
    }

    #[test]
    fn keyboard_enhancement_flags_disambiguate_modified_enter() {
        assert!(
            keyboard_enhancement_flags()
                .contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            "Shift+Enter reaches crossterm as plain Enter on common terminals unless keyboard enhancement asks for modified-key disambiguation"
        );
    }

    /// Fake daemon reply policy for the handler-level timeout tests.
    const DAEMON_SILENT: u8 = 0;
    const DAEMON_ANSWER: u8 = 1;
    const DAEMON_FAIL: u8 = 2;

    /// A config manager wired to an in-process fake daemon. The daemon reads
    /// every request frame the client sends, records its method, and then
    /// stays silent (so the client's own 5s `call` deadline fires), answers
    /// with an empty field list, or answers with a JSON-RPC error, per
    /// `mode`.
    fn manager_with_fake_daemon() -> (
        App,
        Arc<std::sync::atomic::AtomicU8>,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use crate::jsonrpc::{JsonRpcError, RpcOutbound};
        use std::sync::atomic::{AtomicU8, Ordering};
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
        let outbound = Arc::new(RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(outbound.clone()));
        let mode = Arc::new(AtomicU8::new(DAEMON_SILENT));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (daemon_mode, daemon_seen) = (mode.clone(), seen.clone());
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let request: serde_json::Value =
                    serde_json::from_str(&frame).expect("client sends JSON-RPC frames");
                let id = request["id"].as_str().unwrap_or_default().to_string();
                daemon_seen
                    .lock()
                    .unwrap()
                    .push(request["method"].as_str().unwrap_or_default().to_string());
                match daemon_mode.load(Ordering::SeqCst) {
                    DAEMON_ANSWER => outbound.dispatch_response(
                        &id,
                        Some(serde_json::json!({ "entries": [] })),
                        None,
                    ),
                    DAEMON_FAIL => outbound.dispatch_response(
                        &id,
                        None,
                        Some(JsonRpcError {
                            code: -32000,
                            message: "boom".to_string(),
                            data: None,
                        }),
                    ),
                    _ => {}
                }
            }
        });
        let mut mgr = App::new(rpc, std::path::Path::new("/tmp"));
        mgr.sections = vec![
            entry_with_cost("alpha", ""),
            entry_with_cost("beta", ""),
            entry_with_cost("gamma", ""),
        ];
        (mgr, mode, seen)
    }

    /// The real handlers take the app's `Term`. A fixed viewport never
    /// queries the tty, and the section-list paths these tests drive never
    /// draw through it, so nothing is written to stdout.
    fn handler_term() -> Term {
        Terminal::with_options(
            WideCellCleanupBackend::new(std::io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 160, 40)),
            },
        )
        .expect("fixed-viewport terminal")
    }

    /// Render through `draw_into`, the same entry point `app::run` uses for
    /// the Config mode, and return the screen as text.
    fn render_config(mgr: &mut App) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        term.draw(|frame| {
            let area = frame.area();
            mgr.draw_into(frame, area);
        })
        .unwrap();
        let buffer = term.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mouse_at(kind: MouseEventKind, area: Rect) -> MouseEvent {
        MouseEvent {
            kind,
            column: area.x + area.width / 2,
            row: area.y + area.height / 2,
            modifiers: KeyModifiers::NONE,
        }
    }

    // Paused clock: the client's real 5s `call` deadline elapses instantly.
    #[tokio::test(start_paused = true)]
    async fn config_pane_survives_rpc_timeout_on_real_key_and_mouse_input() {
        use std::sync::atomic::Ordering;
        let (mut mgr, mode, seen) = manager_with_fake_daemon();
        let mut term = handler_term();
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        // Like `init`, eagerly load the first section while the daemon
        // answers, so the timeouts below land on a loaded detail screen whose
        // existing status row shows them.
        mode.store(DAEMON_ANSWER, Ordering::SeqCst);
        mgr.load_section_content(0)
            .await
            .expect("first section loads");
        mode.store(DAEMON_SILENT, Ordering::SeqCst);
        render_config(&mut mgr);
        let section_pane = mgr.last_section_pane_area;
        let area = Rect::new(0, 0, 160, 40);

        // Key: Down previews `beta`; its config/list gets no reply.
        let quit = mgr.handle_key(down, &mut term).await;
        assert!(
            matches!(quit, Ok(false)),
            "a timed-out RPC under a key must neither quit nor propagate: {quit:?}"
        );
        let screen = render_config(&mut mgr);
        assert!(
            screen.contains("config/list") && screen.contains("timed out after 5s"),
            "timeout must be visible in the status line after a key:\n{screen}"
        );

        // Mouse: scrolling over the section pane previews `gamma`, also
        // unanswered.
        let scrolled = mgr
            .handle_mouse(
                mouse_at(MouseEventKind::ScrollDown, section_pane),
                area,
                &mut term,
            )
            .await;
        assert!(
            scrolled.is_ok(),
            "a timed-out RPC under a mouse event must not propagate: {scrolled:?}"
        );
        assert_eq!(mgr.section_cursor, 2);
        let screen = render_config(&mut mgr);
        assert!(
            screen.contains("timed out after 5s"),
            "timeout must be visible in the status line after a mouse event:\n{screen}"
        );
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["config/list", "config/list", "config/list"],
            "the initial load and both inputs must have reached the daemon"
        );

        // The daemon recovers: further key and mouse input works normally.
        mode.store(DAEMON_ANSWER, Ordering::SeqCst);
        let quit = mgr.handle_key(up, &mut term).await;
        assert!(matches!(quit, Ok(false)), "{quit:?}");
        assert_eq!(
            mgr.loaded_section,
            Some(1),
            "Up must load `beta` once answered"
        );
        assert!(mgr.status_msg.is_none());
        let screen = render_config(&mut mgr);
        assert!(
            !screen.contains("timed out"),
            "status clears on the next successful input:\n{screen}"
        );
        let scrolled = mgr
            .handle_mouse(
                mouse_at(MouseEventKind::ScrollUp, section_pane),
                area,
                &mut term,
            )
            .await;
        assert!(scrolled.is_ok(), "{scrolled:?}");
        assert_eq!(
            mgr.loaded_section,
            Some(0),
            "scroll must load `alpha` once answered"
        );
        assert_eq!(seen.lock().unwrap().len(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn config_pane_handlers_still_propagate_non_timeout_rpc_errors() {
        use std::sync::atomic::Ordering;
        let (mut mgr, mode, seen) = manager_with_fake_daemon();
        mode.store(DAEMON_FAIL, Ordering::SeqCst);
        let mut term = handler_term();
        render_config(&mut mgr);
        let section_pane = mgr.last_section_pane_area;

        let quit = mgr
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut term)
            .await;
        let error = quit.expect_err("a daemon error is not a timeout and must propagate");
        assert!(format!("{error:#}").contains("boom"), "{error:#}");
        assert!(
            mgr.status_msg.is_none(),
            "only timeouts become a status line"
        );

        let scrolled = mgr
            .handle_mouse(
                mouse_at(MouseEventKind::ScrollDown, section_pane),
                Rect::new(0, 0, 160, 40),
                &mut term,
            )
            .await;
        assert!(scrolled.is_err(), "mouse path must propagate it too");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    fn test_manager() -> App {
        use crate::jsonrpc::RpcOutbound;
        use tokio::sync::mpsc;
        let (tx, _rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcClient::with_rpc(Arc::new(RpcOutbound::new(tx))));
        App::new(rpc, std::path::Path::new("/tmp"))
    }

    fn filter_test_term() -> Term {
        Terminal::with_options(
            WideCellCleanupBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 120, 40)),
            },
        )
        .unwrap()
    }

    fn render_config_120(manager: &mut App) -> String {
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        term.draw(|frame| manager.draw_into(frame, frame.area()))
            .unwrap();
        term.backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn filter_fixture(manager: &mut App) {
        manager.sections = vec![
            entry_with_cost("Models", ""),
            entry_with_cost("Agents", ""),
            entry_with_cost("MCP", ""),
        ];
        manager.section_cursor = 1;
        manager.loaded_section = Some(1);
        manager.fields = vec![field("a.first"), field("a.second")];
        manager.field_cursor = 1;
        manager.screen = Screen::FieldList {
            section_idx: 1,
            prefix: "Agents".into(),
            breadcrumb: vec!["Agents".into()],
        };
    }

    #[tokio::test]
    async fn config_filter_cancel_restores_section_preview_cursor_and_focus_after_paste() {
        let (mut manager, calls) = responding_manager();
        filter_fixture(&mut manager);
        manager.fields[0].tab = ConfigTab::Connection;
        manager.fields[1].tab = ConfigTab::Advanced;
        manager.tab_names = vec![ConfigTab::Connection, ConfigTab::Advanced];
        manager.active_tab = 1;
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.filter_cursor, 1);
        manager.handle_paste("MCP\r\n").await;
        assert_eq!(manager.section_cursor, 2);
        assert_eq!(manager.loaded_section, Some(2));
        assert_eq!(manager.filter_cursor, 0);
        let rendered = render_config_120(&mut manager);
        assert!(rendered.contains("Filter sections: /MCP"));
        assert!(!rendered.contains("Filter list:"));
        assert!(rendered.contains("first ="));
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert!(manager.filter.is_none());
        assert_eq!(manager.section_cursor, 1);
        assert_eq!(manager.loaded_section, Some(1));
        assert_eq!(manager.field_cursor, 1);
        assert_eq!(manager.active_tab, 1);
        assert!(render_config_120(&mut manager).contains("second ="));
        assert_eq!(manager.zeroclaw_pane, ZeroclawPane::Sections);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [
                crate::client::method::CONFIG_LIST,
                crate::client::method::CONFIG_LIST
            ]
        );
    }

    #[tokio::test]
    async fn config_filter_model_picker_is_local_and_cancel_keeps_original_selection() {
        let mut manager = test_manager();
        filter_fixture(&mut manager);
        manager.zeroclaw_pane = ZeroclawPane::Detail;
        manager.screen = Screen::FieldEdit {
            section_idx: 1,
            prefix: "Agents".into(),
            breadcrumb: vec!["Agents".into()],
            field_idx: 0,
        };
        manager.select_items = vec!["openai.default".into(), "ollama.local".into()];
        manager.select_cursor = 1;
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager.handle_paste("openai").await;
        let rendered = render_config_120(&mut manager);
        assert!(rendered.contains("Models"));
        assert!(rendered.contains("Agents"));
        assert!(rendered.contains("MCP"));
        assert!(rendered.contains("Filter options: /openai"));
        assert!(!rendered.contains("ollama.local"));
        assert!(!rendered.contains("Filter sections:"));
        manager.handle_paste(".missing").await;
        assert!(render_config_120(&mut manager).contains("No matches"));
        manager
            .handle_key(key(KeyCode::Enter), &mut term)
            .await
            .unwrap();
        assert!(matches!(manager.screen, Screen::FieldEdit { .. }));
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.select_cursor, 1);
        assert_eq!(manager.zeroclaw_pane, ZeroclawPane::Detail);
        assert!(matches!(manager.screen, Screen::FieldEdit { .. }));
        assert!(render_config_120(&mut manager).contains("ollama.local"));
    }

    #[tokio::test]
    async fn config_filter_empty_sections_disables_stale_preview_and_enter() {
        let (mut manager, calls) = responding_manager();
        filter_fixture(&mut manager);
        let mut term = filter_test_term();
        render_config_120(&mut manager);
        let stale_main = manager.last_main_area;
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        for ch in "timeout".chars() {
            manager
                .handle_key(key(KeyCode::Char(ch)), &mut term)
                .await
                .unwrap();
        }
        let rendered = render_config_120(&mut manager);
        assert_eq!(rendered.matches("No matches").count(), 2);
        assert!(!rendered.contains("second ="));
        assert!(manager.last_section_rows.is_empty());
        assert_eq!(manager.last_main_area, Rect::default());
        assert!(manager.last_tab_area.is_none());
        manager
            .handle_key(key(KeyCode::Enter), &mut term)
            .await
            .unwrap();
        for kind in [
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            MouseEventKind::ScrollDown,
        ] {
            manager
                .handle_mouse(
                    MouseEvent {
                        kind,
                        column: stale_main.x + 2,
                        row: stale_main.y + 1,
                        modifiers: KeyModifiers::NONE,
                    },
                    Rect::new(0, 0, 120, 40),
                    &mut term,
                )
                .await
                .unwrap();
        }
        assert_eq!(manager.section_cursor, 1);
        assert_eq!(manager.field_cursor, 1);
        assert!(calls.lock().unwrap().is_empty());
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.section_cursor, 1);
        assert!(render_config_120(&mut manager).contains("second ="));
    }

    #[tokio::test]
    async fn config_filter_mouse_accepts_filtered_alias_and_discards_query() {
        let (mut manager, _) = responding_manager();
        filter_fixture(&mut manager);
        manager.zeroclaw_pane = ZeroclawPane::Detail;
        manager.screen = Screen::AliasList {
            section_idx: 1,
            map_path: "agents".into(),
            breadcrumb: vec!["agents".into()],
        };
        manager.aliases = vec!["first".into(), "second".into()];
        manager.alias_cursor = 0;
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager.handle_paste("second").await;
        render_config_120(&mut manager);
        let area = manager.last_main_area;
        let click = MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 2,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        manager
            .handle_mouse(click, Rect::new(0, 0, 120, 40), &mut term)
            .await
            .unwrap();
        manager
            .handle_mouse(click, Rect::new(0, 0, 120, 40), &mut term)
            .await
            .unwrap();
        assert!(
            matches!(&manager.screen, Screen::FieldList { prefix, .. } if prefix == "agents.second")
        );
        assert!(manager.filter.is_none());
        assert_eq!(manager.zeroclaw_pane, ZeroclawPane::Detail);
    }

    #[tokio::test]
    async fn config_filter_field_cancel_precedes_top_level_back_and_tab_change_discards_query() {
        let mut manager = test_manager();
        filter_fixture(&mut manager);
        manager.zeroclaw_pane = ZeroclawPane::Detail;
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager.handle_paste("missing").await;
        assert!(render_config_120(&mut manager).contains("No matches"));
        manager
            .handle_key(key(KeyCode::Enter), &mut term)
            .await
            .unwrap();
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.zeroclaw_pane, ZeroclawPane::Detail);
        assert_eq!(manager.field_cursor, 1);
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager
            .handle_key(key(KeyCode::Tab), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.section, ConfigSection::Zerocode);
        assert!(manager.filter.is_none());
    }

    #[tokio::test]
    async fn config_filter_cancel_reopens_the_nested_page_it_started_from() {
        let (mut manager, _) = responding_manager();
        filter_fixture(&mut manager);
        manager.screen = Screen::FieldList {
            section_idx: 1,
            prefix: "agents.alpha".into(),
            breadcrumb: vec!["Agents".into(), "alpha".into()],
        };
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager.handle_paste("MCP").await;
        assert_eq!(manager.loaded_section, Some(2));
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert!(manager.filter.is_none());
        assert_eq!(manager.section_cursor, 1);
        assert_eq!(manager.loaded_section, Some(1));
        assert_eq!(manager.field_cursor, 1);
        match &manager.screen {
            Screen::FieldList {
                prefix, breadcrumb, ..
            } => {
                assert_eq!(prefix, "agents.alpha");
                assert_eq!(breadcrumb.len(), 2);
            }
            _ => panic!("cancel should reopen the agent page"),
        }
    }

    #[tokio::test]
    async fn config_filter_cancel_keeps_the_costs_add_row_selected() {
        let (mut manager, _) = responding_manager();
        filter_fixture(&mut manager);
        manager.sections[1] = entry_with_cost("Agents", "llm");
        manager.screen = Screen::AliasList {
            section_idx: 1,
            map_path: "Agents.anthropic".into(),
            breadcrumb: vec!["Agents".into(), "anthropic".into()],
        };
        manager.alias_tab = 1;
        manager.cost_resources = vec!["first".into(), "second".into()];
        manager.cost_cursor = 2;
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager.handle_paste("MCP").await;
        assert_eq!(manager.loaded_section, Some(2));
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.alias_tab, 1);
        assert_eq!(manager.cost_resources.len(), 2);
        assert_eq!(manager.cost_cursor, 2);
    }

    #[tokio::test]
    async fn config_filter_failed_paste_and_cancel_keep_tui_open_and_detail_inert() {
        for fail_on_cancel in [false, true] {
            let (mut manager, _) = responding_manager();
            filter_fixture(&mut manager);
            let mut term = filter_test_term();
            manager
                .handle_key(key(KeyCode::Char('/')), &mut term)
                .await
                .unwrap();
            if fail_on_cancel {
                manager.handle_paste("MCP").await;
            }
            manager.rpc = test_manager().rpc;
            if fail_on_cancel {
                manager
                    .handle_key(key(KeyCode::Esc), &mut term)
                    .await
                    .unwrap();
                assert_eq!(manager.section_cursor, 1);
                assert_eq!(manager.loaded_section, Some(2));
                assert!(manager.filter.is_none());
            } else {
                manager.handle_paste("MCP").await;
                assert_eq!(manager.section_cursor, 2);
                assert_eq!(manager.loaded_section, Some(1));
            }
            assert!(manager.status_msg.is_some());
            let rendered = render_config_120(&mut manager);
            assert!(!rendered.contains("first ="));
            assert!(!rendered.contains("second ="));
            assert_eq!(manager.last_main_area, Rect::default());
            assert!(manager.last_tab_area.is_none());
            manager
                .handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                        column: 35,
                        row: 10,
                        modifiers: KeyModifiers::NONE,
                    },
                    Rect::new(0, 0, 120, 40),
                    &mut term,
                )
                .await
                .unwrap();
            assert_eq!(manager.zeroclaw_pane, ZeroclawPane::Sections);
            assert!(matches!(manager.screen, Screen::FieldList { .. }));
        }
    }

    #[tokio::test]
    async fn config_filter_from_add_row_selects_a_real_alias_on_enter() {
        let (mut manager, _) = responding_manager();
        filter_fixture(&mut manager);
        manager.zeroclaw_pane = ZeroclawPane::Detail;
        manager.screen = Screen::AliasList {
            section_idx: 1,
            map_path: "agents".into(),
            breadcrumb: vec!["agents".into()],
        };
        manager.aliases = vec!["first".into(), "second".into()];
        manager.alias_cursor = 2;
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.filter_cursor, 1);
        manager
            .handle_key(key(KeyCode::Enter), &mut term)
            .await
            .unwrap();
        assert!(
            matches!(&manager.screen, Screen::FieldList { prefix, .. } if prefix == "agents.second")
        );
        assert!(manager.filter.is_none());
    }

    #[tokio::test]
    async fn config_filter_sidebar_mouse_scroll_and_click_use_matching_rows() {
        let (mut manager, _) = responding_manager();
        filter_fixture(&mut manager);
        let mut term = filter_test_term();
        manager
            .handle_key(key(KeyCode::Char('/')), &mut term)
            .await
            .unwrap();
        manager
            .handle_key(key(KeyCode::Char('M')), &mut term)
            .await
            .unwrap();
        render_config_120(&mut manager);
        let area = manager.last_section_list_area;
        manager
            .handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: area.x + 2,
                    row: area.y + 1,
                    modifiers: KeyModifiers::NONE,
                },
                Rect::new(0, 0, 120, 40),
                &mut term,
            )
            .await
            .unwrap();
        assert_eq!(manager.filter_cursor, 1);
        assert_eq!(manager.section_cursor, 2);
        manager
            .handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    column: area.x + 2,
                    row: area.y + 1,
                    modifiers: KeyModifiers::NONE,
                },
                Rect::new(0, 0, 120, 40),
                &mut term,
            )
            .await
            .unwrap();
        assert_eq!(manager.filter_cursor, 0);
        assert_eq!(manager.section_cursor, 0);
        manager
            .handle_key(key(KeyCode::Esc), &mut term)
            .await
            .unwrap();
        assert_eq!(manager.section_cursor, 1);
    }

    fn set_detail_filter(manager: &mut App, query: String) {
        manager.filter_origin = Some(FilterOrigin {
            pane: ZeroclawPane::Detail,
            section_cursor: manager.section_cursor,
            active_tab: manager.active_tab,
            detail: None,
        });
        manager.filter = Some(query);
    }

    fn draw_setup_test_frame(frame: &mut Frame, manager: &mut App) {
        manager.loaded_section = Some(manager.section_cursor);
        let mut chat = crate::chat::Chat::new(manager.rpc.clone(), crate::chat::PaneKind::Chat);
        let mut acp = crate::acp::Acp::new(manager.rpc.clone());
        crate::app::draw_app_frame(
            frame,
            crate::app::Mode::Config,
            &mut chat,
            &mut acp,
            |frame, chunks, _, _| manager.draw_into(frame, chunks[1]),
        );
    }

    #[test]
    fn older_daemon_setup_metadata_stays_unknown() {
        let value = serde_json::to_value(field("model_routes.fast.model")).unwrap();
        let mut value = value.as_object().unwrap().clone();
        value.remove("setup");
        let decoded: ConfigFieldEntry = serde_json::from_value(value.into()).unwrap();
        assert!(decoded.setup.is_none());
    }

    #[tokio::test]
    async fn alias_creation_uses_typed_missing_metadata_and_selects_its_tab() {
        use crate::jsonrpc::RpcOutbound;
        use crate::wire::ConfigFieldSetup;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        let outbound = Arc::new(RpcOutbound::new(tx));
        let rpc = Arc::new(RpcClient::with_rpc(outbound.clone()));
        let mut manager = App::new(rpc, Path::new("/tmp"));
        manager.sections = vec![typed_section("providers.tts")];
        manager.screen = Screen::AliasCreate {
            section_idx: 0,
            map_path: "providers.tts.openai".to_string(),
            breadcrumb: vec!["TTS".to_string(), "openai".to_string()],
        };
        manager.edit_buf = "sample".to_string();
        let mut optional = field("providers.tts.openai.sample.voice");
        optional.tab = ConfigTab::Model;
        let mut required = field("providers.tts.openai.sample.api_key");
        required.tab = ConfigTab::Connection;
        required.is_secret = true;
        required.populated = true; // Masked display population must not control setup.
        required.setup = Some(ConfigFieldSetup { missing: true });
        let responder = tokio::spawn(async move {
            let create: serde_json::Value =
                serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            assert_eq!(
                create["method"],
                crate::client::method::CONFIG_MAP_KEY_CREATE
            );
            assert_eq!(create["params"]["key"], "sample");
            outbound.dispatch_response(
                create["id"].as_str().unwrap(),
                Some(serde_json::json!({})),
                None,
            );
            let list: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            assert_eq!(list["method"], crate::client::method::CONFIG_LIST);
            outbound.dispatch_response(
                list["id"].as_str().unwrap(),
                Some(serde_json::json!({"entries": [optional, required]})),
                None,
            );
        });
        manager
            .handle_alias_create(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        responder.await.unwrap();
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(manager.field_cursor, 1);
        assert_eq!(manager.tab_names[manager.active_tab], ConfigTab::Connection);
    }

    #[tokio::test]
    async fn cancelling_alias_name_only_reads_the_alias_list() {
        use crate::jsonrpc::RpcOutbound;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        let outbound = Arc::new(RpcOutbound::new(tx));
        let mut manager = App::new(
            Arc::new(RpcClient::with_rpc(outbound.clone())),
            Path::new("/tmp"),
        );
        manager.sections = vec![typed_section("model_routes")];
        manager.screen = Screen::AliasCreate {
            section_idx: 0,
            map_path: "model_routes".to_string(),
            breadcrumb: vec!["Routes".to_string()],
        };
        manager.edit_buf = "discarded".to_string();
        let responder = tokio::spawn(async move {
            let request: serde_json::Value =
                serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            assert_eq!(request["method"], crate::client::method::CONFIG_MAP_KEYS);
            outbound.dispatch_response(
                request["id"].as_str().unwrap(),
                Some(serde_json::json!({"keys": []})),
                None,
            );
            rx
        });
        manager
            .handle_alias_create(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        let mut rx = responder.await.unwrap();
        assert!(rx.try_recv().is_err());
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
    }

    fn render_setup_text(manager: &mut App) -> String {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| draw_setup_test_frame(frame, manager))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[tokio::test]
    async fn section_search_keeps_setup_guidance_in_the_detail_preview() {
        let mut manager = test_manager();
        manager.sections = vec![typed_section("providers.tts")];
        manager.screen = Screen::AliasList {
            section_idx: 0,
            map_path: "providers.tts.edge".to_string(),
            breadcrumb: vec![],
        };
        manager.filter_origin = Some(FilterOrigin {
            pane: ZeroclawPane::Sections,
            section_cursor: 0,
            active_tab: manager.active_tab,
            detail: None,
        });
        manager.filter = Some("tts".to_string());
        let text = render_setup_text(&mut manager);
        assert!(text.contains("No aliases are configured"), "{text}");
        assert!(!text.contains("No matches. Clear filter."));
    }

    #[tokio::test]
    async fn cost_rate_creation_keeps_the_generic_name_hint() {
        let mut manager = test_manager();
        manager.sections = vec![typed_section("providers.tts")];
        manager.screen = Screen::AliasCreate {
            section_idx: 0,
            map_path: "cost.rates.providers.tts.openai".to_string(),
            breadcrumb: vec![],
        };
        let text = render_setup_text(&mut manager);
        assert!(!text.contains("Text-to-speech is optional"), "{text}");
        assert!(!text.contains("[missing]"), "{text}");
    }

    #[tokio::test]
    async fn setup_explanation_is_visible_without_adding_selectable_rows() {
        use ratatui::backend::TestBackend;
        for (width, height) in [(120, 40), (60, 24)] {
            let mut manager = test_manager();
            manager.sections = vec![typed_section("providers.tts")];
            manager.screen = Screen::AliasList {
                section_idx: 0,
                map_path: "providers.tts.edge".to_string(),
                breadcrumb: vec!["TTS".to_string(), "edge".to_string()],
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw_setup_test_frame(frame, &mut manager))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("optional"));
            assert!(text.contains("[+ Add]"));
            assert!(!text.contains("list.Text"), "guidance lines ran together");
            assert_eq!(manager.alias_cursor, 0);
            assert!(manager.last_main_area.height >= 4);
            manager.aliases = vec!["present".to_string()];
            set_detail_filter(&mut manager, "nomatch".to_string());
            terminal
                .draw(|frame| draw_setup_test_frame(frame, &mut manager))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("No matches."));
            assert!(!text.contains("No aliases are configured"));
            assert!(!text.contains("[+ Add]"));
        }
    }

    #[tokio::test]
    async fn missing_field_and_creation_guidance_survive_narrow_detail_pane() {
        use crate::wire::ConfigFieldSetup;
        use ratatui::backend::TestBackend;
        for (width, height) in [(120, 40), (60, 24)] {
            for path in [
                "providers.tts.openai.sample.api_key",
                "model_routes.fast.model_provider",
            ] {
                let mut manager = test_manager();
                manager.sections = vec![typed_section("providers.tts")];
                manager.zeroclaw_pane = ZeroclawPane::Detail;
                let prefix = path.rsplit_once('.').unwrap().0.to_string();
                manager.screen = Screen::FieldList {
                    section_idx: 0,
                    prefix,
                    breadcrumb: vec![],
                };
                let mut required = field(path);
                required.setup = Some(ConfigFieldSetup { missing: true });
                manager.fields = vec![required];
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| draw_setup_test_frame(frame, &mut manager))
                    .unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                let name = path.rsplit_once('.').unwrap().1;
                assert!(
                    text.contains(&format!("{name} [missing]")),
                    "{width} columns: {name}"
                );
                manager.screen = Screen::AliasCreate {
                    section_idx: 0,
                    map_path: "providers.tts.openai".into(),
                    breadcrumb: vec![],
                };
                terminal
                    .draw(|frame| draw_setup_test_frame(frame, &mut manager))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let mut detail = String::new();
                for y in 0..height {
                    for x in 30..width {
                        detail.push_str(buffer[(x, y)].symbol());
                    }
                    detail.push(' ');
                }
                let detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(detail.contains("cancels without saving"), "{detail}");
                assert!(detail.contains("keeps the saved entry"), "{detail}");
            }
        }
    }

    fn entry_with_cost(key: &str, cost_category: &str) -> ConfigSectionEntry {
        ConfigSectionEntry {
            key: key.to_string(),
            label: key.to_string(),
            help: String::new(),
            completed: false,
            group: String::new(),
            group_key: String::new(),
            shape: None,
            cost_category: cost_category.to_string(),
        }
    }

    #[test]
    fn config_metadata_keys_use_locale_independent_identifiers() {
        assert_eq!(
            config_i18n_key("group", "multi_agent", None),
            "zc-config-group-multi-agent"
        );
        assert_eq!(
            config_i18n_key("section", "providers.models", Some("label")),
            "zc-config-section-providers-models-label"
        );
        assert_eq!(
            config_i18n_key("section", "query_classification", Some("help")),
            "zc-config-section-query-classification-help"
        );
    }

    #[test]
    fn group_order_uses_new_keys_and_legacy_labels() {
        let mut stable = entry_with_cost("providers.models", "");
        stable.group = "Localized display text".to_string();
        stable.group_key = "foundation".to_string();
        assert_eq!(App::group_rank(&stable), 0);

        let mut legacy = entry_with_cost("cron", "");
        legacy.group = "Agent".to_string();
        assert_eq!(App::group_rank(&legacy), 1);
    }

    #[test]
    fn config_metadata_localization_preserves_wire_fallbacks_and_legacy_groups() {
        let translate = |key: &str| match key {
            "zc-config-group-foundation" => Some("基盤".to_string()),
            "zc-config-section-providers-models-label" => Some("モデルプロバイダー".to_string()),
            _ => None,
        };

        let mut current = entry_with_cost("providers.models", "");
        current.label = "Model providers".to_string();
        current.help = "Server-owned help".to_string();
        current.group = "Foundation".to_string();
        current.group_key = "foundation".to_string();
        localize_section_metadata_with(&mut current, &translate);
        assert_eq!(current.group, "基盤");
        assert_eq!(current.label, "モデルプロバイダー");
        assert_eq!(current.help, "Server-owned help");

        let mut legacy = entry_with_cost("cron", "");
        legacy.group = "Agent".to_string();
        localize_section_metadata_with(&mut legacy, &|_| None);
        assert_eq!(legacy.group_key, "agent");
        assert_eq!(legacy.group, "Agent");

        let mut unknown = entry_with_cost("extension", "");
        unknown.group = "Extension group".to_string();
        localize_section_metadata_with(&mut unknown, &|_| None);
        assert!(unknown.group_key.is_empty());
        assert_eq!(unknown.group, "Extension group");
        assert_eq!(App::group_rank(&unknown), 8);

        let mut ungrouped = entry_with_cost("legacy-flat", "");
        localize_section_metadata_with(&mut ungrouped, &|_| None);
        assert!(ungrouped.group.is_empty());
        assert!(ungrouped.group_key.is_empty());
    }

    fn typed_section(key: &str) -> ConfigSectionEntry {
        ConfigSectionEntry {
            key: key.to_string(),
            label: key.to_string(),
            help: String::new(),
            completed: false,
            group: String::new(),
            group_key: String::new(),
            shape: Some(SectionShape::TypedFamilyMap),
            cost_category: String::new(),
        }
    }

    fn alias_rename_manager(
        section_key: &str,
        family: &str,
    ) -> (
        App,
        Arc<crate::jsonrpc::RpcOutbound>,
        tokio::sync::mpsc::Receiver<String>,
        Term,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let outbound = Arc::new(crate::jsonrpc::RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(outbound.clone()));
        let mut manager = App::new(client, Path::new("/tmp"));
        manager.sections = vec![typed_section(section_key)];
        manager.screen = Screen::AliasList {
            section_idx: 0,
            map_path: format!("{section_key}.{family}"),
            breadcrumb: vec![section_key.into(), family.into()],
        };
        manager.aliases = vec!["old".into(), "other".into()];
        manager.zeroclaw_pane = ZeroclawPane::Detail;
        manager.loaded_section = Some(0);
        let term = Terminal::with_options(
            WideCellCleanupBackend::new(std::io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 120, 40)),
            },
        )
        .unwrap();
        (manager, outbound, rx, term)
    }

    async fn config_key(manager: &mut App, term: &mut Term, code: KeyCode) {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            manager.handle_key(KeyEvent::new(code, KeyModifiers::NONE), term),
        )
        .await
        .expect("config key handler must complete")
        .unwrap();
    }

    async fn enter_alias_rename(manager: &mut App, term: &mut Term, to: &str) {
        config_key(manager, term, KeyCode::Char('e')).await;
        assert!(manager.wants_text_input());
        for _ in 0..manager.edit_buf.chars().count() {
            config_key(manager, term, KeyCode::Backspace).await;
        }
        manager.handle_paste(to).await;
    }

    async fn answer_config_request(
        outbound: &crate::jsonrpc::RpcOutbound,
        rx: &mut tokio::sync::mpsc::Receiver<String>,
        method: &str,
        params: serde_json::Value,
        response: std::result::Result<serde_json::Value, &str>,
    ) {
        let raw = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("expected config RPC request")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(req["method"], method);
        assert_eq!(req["params"], params);
        let id = req["id"].as_str().unwrap();
        match response {
            Ok(result) => outbound.dispatch_response(id, Some(result), None),
            Err(message) => outbound.dispatch_response(
                id,
                None,
                Some(crate::jsonrpc::JsonRpcError {
                    code: -32602,
                    message: message.into(),
                    data: None,
                }),
            ),
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_requires_confirmation_and_refreshes_each_family() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        for ((section, family), from_settings) in [
            ("providers.models", "anthropic"),
            ("providers.tts", "openai"),
            ("providers.transcription", "groq"),
        ]
        .into_iter()
        .flat_map(|provider| [(provider, false), (provider, true)])
        {
            let (mut manager, outbound, mut rx, mut term) = alias_rename_manager(section, family);
            if from_settings {
                manager.screen = Screen::FieldList {
                    section_idx: 0,
                    prefix: format!("{section}.{family}.old"),
                    breadcrumb: vec![section.into(), family.into(), "old".into()],
                };
                // The open alias, not a remembered list selection, owns this target.
                manager.alias_cursor = 1;
            }
            enter_alias_rename(&mut manager, &mut term, "new\r\n").await;
            // The alias-list cursor is no longer authoritative once editing starts.
            manager.alias_cursor = 1;
            assert!(rx.try_recv().is_err());
            config_key(&mut manager, &mut term, KeyCode::Enter).await;
            assert!(matches!(
                manager.screen,
                Screen::AliasRename {
                    confirming: true,
                    ..
                }
            ));
            manager
                .handle_key(
                    KeyEvent::new_with_kind(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                        KeyEventKind::Repeat,
                    ),
                    &mut term,
                )
                .await
                .unwrap();
            assert!(rx.try_recv().is_err());
            manager.handle_paste("ignored").await;
            config_key(&mut manager, &mut term, KeyCode::Char('x')).await;
            assert_eq!(manager.edit_buf, "new");

            let path = format!("{section}.{family}");
            let (_, ()) =
                tokio::join!(config_key(&mut manager, &mut term, KeyCode::Enter), async {
                    answer_config_request(
                        &outbound,
                        &mut rx,
                        "config/map-key-rename",
                        serde_json::json!({"path": path, "from": "old", "to": "new"}),
                        Ok(serde_json::json!({"renamed": true, "warnings": ["synthetic warning"]})),
                    )
                    .await;
                    answer_config_request(
                        &outbound,
                        &mut rx,
                        "config/map-keys",
                        serde_json::json!({"path": path}),
                        Ok(serde_json::json!({"keys": ["other", "new"]})),
                    )
                    .await;
                    for alias in ["other", "new"] {
                        answer_config_request(
                            &outbound,
                            &mut rx,
                            "config/list",
                            serde_json::json!({"prefix": format!("{path}.{alias}.enabled")}),
                            Ok(serde_json::json!({"entries": []})),
                        )
                        .await;
                    }
                });
            assert!(matches!(manager.screen, Screen::AliasList { .. }));
            assert_eq!(manager.aliases[manager.alias_cursor], "new");
            assert!(manager.edit_buf.is_empty());
            assert!(
                manager
                    .status_msg
                    .as_deref()
                    .unwrap()
                    .contains("synthetic warning")
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_blank_unchanged_and_cancel_send_no_rpc() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        let (mut manager, _outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        for to in ["   ", "old"] {
            enter_alias_rename(&mut manager, &mut term, to).await;
            config_key(&mut manager, &mut term, KeyCode::Enter).await;
            assert!(matches!(
                manager.screen,
                Screen::AliasRename {
                    confirming: false,
                    ..
                }
            ));
            assert!(manager.status_msg.is_some());
            config_key(&mut manager, &mut term, KeyCode::Esc).await;
            assert!(matches!(manager.screen, Screen::AliasList { .. }));
            assert!(rx.try_recv().is_err());
        }
        for confirm in [false, true] {
            enter_alias_rename(&mut manager, &mut term, "new").await;
            if confirm {
                config_key(&mut manager, &mut term, KeyCode::Enter).await;
            }
            config_key(&mut manager, &mut term, KeyCode::Esc).await;
            assert!(matches!(manager.screen, Screen::AliasList { .. }));
            assert!(manager.edit_buf.is_empty());
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_failure_keeps_editable_name_and_requires_reconfirmation() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        let (mut manager, outbound, mut rx, mut term) =
            alias_rename_manager("providers.tts", "openai");
        enter_alias_rename(&mut manager, &mut term, "taken").await;
        config_key(&mut manager, &mut term, KeyCode::Enter).await;
        tokio::join!(
            config_key(&mut manager, &mut term, KeyCode::Enter),
            answer_config_request(
                &outbound,
                &mut rx,
                "config/map-key-rename",
                serde_json::json!({"path": "providers.tts.openai", "from": "old", "to": "taken"}),
                Err("alias already exists")
            )
        );
        assert!(matches!(
            manager.screen,
            Screen::AliasRename {
                confirming: false,
                ..
            }
        ));
        assert_eq!(manager.edit_buf, "taken");
        assert!(
            manager
                .status_msg
                .as_deref()
                .unwrap()
                .contains("alias already exists")
        );
        config_key(&mut manager, &mut term, KeyCode::Char('2')).await;
        config_key(&mut manager, &mut term, KeyCode::Enter).await;
        assert!(rx.try_recv().is_err());
        tokio::join!(
            config_key(&mut manager, &mut term, KeyCode::Enter),
            answer_config_request(
                &outbound,
                &mut rx,
                "config/map-key-rename",
                serde_json::json!({"path": "providers.tts.openai", "from": "old", "to": "taken2"}),
                Ok(serde_json::json!({"renamed": false, "warnings": ["not applied warning"]}))
            )
        );
        assert!(matches!(
            manager.screen,
            Screen::AliasRename {
                confirming: false,
                ..
            }
        ));
        assert_eq!(manager.edit_buf, "taken2");
        assert!(
            manager
                .status_msg
                .as_deref()
                .unwrap()
                .contains("not applied warning")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_lost_reply_reconciles_before_another_mutation() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        for refresh_succeeds in [true, false] {
            let (mut manager, outbound, mut rx, mut term) =
                alias_rename_manager("providers.models", "openai");
            enter_alias_rename(&mut manager, &mut term, "new").await;
            config_key(&mut manager, &mut term, KeyCode::Enter).await;
            let mut saved_aliases = vec!["old".to_string()];
            let (result, ()) = tokio::join!(
                manager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut term),
                async {
                    let raw = rx.recv().await.unwrap();
                    let req: serde_json::Value = serde_json::from_str(&raw).unwrap();
                    assert_eq!(req["method"], "config/map-key-rename");
                    assert_eq!(
                        req["params"],
                        serde_json::json!({"path": "providers.models.openai", "from": "old", "to": "new"})
                    );
                    // Commit the fixture rename, but suppress its success response.
                    saved_aliases[0] = "new".into();
                    tokio::time::advance(std::time::Duration::from_secs(121)).await;
                    answer_config_request(
                        &outbound,
                        &mut rx,
                        "config/map-keys",
                        serde_json::json!({"path": "providers.models.openai"}),
                        if refresh_succeeds {
                            Ok(serde_json::json!({"keys": saved_aliases}))
                        } else {
                            Err("reload unavailable")
                        },
                    )
                    .await;
                    assert_eq!(outbound.pending_count(), 0);
                    if refresh_succeeds {
                        answer_config_request(
                            &outbound,
                            &mut rx,
                            "config/list",
                            serde_json::json!({"prefix": "providers.models.openai.new.enabled"}),
                            Ok(serde_json::json!({"entries": []})),
                        )
                        .await;
                    }
                }
            );
            result.unwrap();
            assert_eq!(saved_aliases, ["new"]);
            assert!(matches!(manager.screen, Screen::AliasList { .. }));
            assert!(manager.edit_buf.is_empty());
            assert!(
                manager
                    .status_msg
                    .as_deref()
                    .unwrap()
                    .contains("outcome is unknown")
            );
            render_config_for_mouse(&mut manager);
            if refresh_succeeds {
                assert_eq!(manager.aliases, saved_aliases);
                assert_eq!(manager.loaded_section, Some(0));
                assert_ne!(manager.last_main_area, Rect::default());
                // A fresh Rename explicitly targets the reconciled destination.
                enter_alias_rename(&mut manager, &mut term, "next").await;
                config_key(&mut manager, &mut term, KeyCode::Enter).await;
                assert!(
                    matches!(&manager.screen, Screen::AliasRename { from, confirming: true, .. } if from == "new")
                );
            } else {
                assert!(manager.aliases.is_empty());
                assert!(manager.alias_enabled.is_empty());
                assert!(manager.loaded_section.is_none());
                assert_eq!(manager.last_main_area, Rect::default());
                config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
                config_key(&mut manager, &mut term, KeyCode::Enter).await;
                config_key(&mut manager, &mut term, KeyCode::Enter).await;
            }
            assert!(
                rx.try_recv().is_err(),
                "confirmation must not replay the stale rename"
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_refresh_failure_does_not_retry_committed_mutation() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        let (mut manager, outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        enter_alias_rename(&mut manager, &mut term, "new").await;
        config_key(&mut manager, &mut term, KeyCode::Enter).await;
        tokio::join!(config_key(&mut manager, &mut term, KeyCode::Enter), async {
            answer_config_request(
                &outbound,
                &mut rx,
                "config/map-key-rename",
                serde_json::json!({"path": "providers.models.openai", "from": "old", "to": "new"}),
                Ok(serde_json::json!({"renamed": true, "warnings": ["saved warning"]})),
            )
            .await;
            answer_config_request(
                &outbound,
                &mut rx,
                "config/map-keys",
                serde_json::json!({"path": "providers.models.openai"}),
                Err("refresh unavailable"),
            )
            .await;
        });
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        assert!(manager.aliases.is_empty());
        assert!(manager.edit_buf.is_empty());
        assert_eq!(manager.loaded_section, None);
        let status = manager.status_msg.as_deref().unwrap();
        assert!(status.contains("Renamed"));
        assert!(status.contains("refresh unavailable"));
        assert!(status.contains("saved warning"));
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_is_rebindable_and_excludes_other_rows() {
        use crate::keymap::{Chord, ConfigTabAction};
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        for section in ["channels", "agents", "skill-bundles", "knowledge_bundles"] {
            let (mut manager, _outbound, mut rx, mut term) =
                alias_rename_manager(section, "example");
            if section != "channels" {
                manager.sections[0].shape = Some(SectionShape::OneTierAliasMap);
                manager.screen = Screen::AliasList {
                    section_idx: 0,
                    map_path: section.into(),
                    breadcrumb: vec![section.into()],
                };
            }
            config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
            assert!(matches!(manager.screen, Screen::AliasList { .. }));
            assert!(rx.try_recv().is_err());
        }
        let (mut manager, _outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        manager.alias_cursor = manager.aliases.len();
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        manager.alias_cursor = 0;
        manager.sections[0].cost_category = "models".into();
        manager.alias_tab = 1;
        manager.cost_resources = vec!["example-model".into()];
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        manager.alias_tab = 0;
        config_key(&mut manager, &mut term, KeyCode::Char('/')).await;
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert_eq!(manager.filter.as_deref(), Some("e"));
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        config_key(&mut manager, &mut term, KeyCode::Esc).await;
        crate::keymap::overrides::set_row(
            "config_tab",
            "rename_alias",
            vec![Chord::key(KeyCode::F(6))],
        );
        assert_eq!(
            ConfigTabAction::from_chord(&KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
            Some(ConfigTabAction::RenameAlias)
        );
        assert!(manager.bottom_hint().contains("F6"));
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        config_key(&mut manager, &mut term, KeyCode::F(6)).await;
        assert!(matches!(manager.screen, Screen::AliasRename { .. }));
        assert!(rx.try_recv().is_err());
        crate::keymap::overrides::reset();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_settings_cancel_preserves_context_and_rebinding() {
        use crate::keymap::Chord;
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        let (mut manager, _outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        let prefix = "providers.models.openai.old";
        let breadcrumb = vec!["providers.models".into(), "openai".into(), "old".into()];
        manager.screen = Screen::FieldList {
            section_idx: 0,
            prefix: prefix.into(),
            breadcrumb: breadcrumb.clone(),
        };
        manager.active_tab = 2;
        manager.field_cursor = 3;
        crate::keymap::overrides::set_row(
            "config_tab",
            "rename_alias",
            vec![Chord::key(KeyCode::F(6))],
        );
        assert!(manager.bottom_hint().contains("F6=rename"));
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        config_key(&mut manager, &mut term, KeyCode::F(6)).await;
        assert!(matches!(
            manager.screen,
            Screen::AliasRename {
                return_to_fields: true,
                ..
            }
        ));
        manager.edit_buf = "new".into();
        config_key(&mut manager, &mut term, KeyCode::Enter).await;
        config_key(&mut manager, &mut term, KeyCode::Esc).await;
        assert!(
            matches!(&manager.screen, Screen::FieldList { prefix: actual, breadcrumb: bc, .. } if actual == prefix && bc == &breadcrumb)
        );
        assert_eq!(manager.active_tab, 2);
        assert_eq!(manager.field_cursor, 3);
        assert!(manager.edit_buf.is_empty());
        assert!(rx.try_recv().is_err());
        crate::keymap::overrides::reset();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_settings_excludes_non_alias_paths_and_search_input() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        let (mut manager, _outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        for prefix in [
            "providers.models",
            "providers.models.openai",
            "providers.models.openai.old.model",
            "cost.rates.providers.models.openai.example",
            "channels.telegram.old",
            "agents.old",
        ] {
            manager.screen = Screen::FieldList {
                section_idx: 0,
                prefix: prefix.into(),
                breadcrumb: vec![],
            };
            assert!(!manager.bottom_hint().contains("=rename"));
            config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
            assert!(matches!(manager.screen, Screen::FieldList { .. }));
        }
        manager.screen = Screen::FieldList {
            section_idx: 0,
            prefix: "providers.models.openai.old".into(),
            breadcrumb: vec!["providers.models".into(), "openai".into(), "old".into()],
        };
        config_key(&mut manager, &mut term, KeyCode::Char('/')).await;
        config_key(&mut manager, &mut term, KeyCode::Char('e')).await;
        assert_eq!(manager.filter.as_deref(), Some("e"));
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_is_cleared_when_switching_sections() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        let (mut manager, _outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        enter_alias_rename(&mut manager, &mut term, "new").await;
        config_key(&mut manager, &mut term, KeyCode::Enter).await;
        config_key(&mut manager, &mut term, KeyCode::Tab).await;
        assert!(manager.section == ConfigSection::Zerocode);
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        assert!(manager.edit_buf.is_empty());
        config_key(&mut manager, &mut term, KeyCode::BackTab).await;
        assert!(manager.section == ConfigSection::Zeroclaw);
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        enter_alias_rename(&mut manager, &mut term, "new").await;
        // Passive preview keeps the loaded drill level while cancelling rename.
        manager.preview_section(0).await.unwrap();
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        assert!(manager.edit_buf.is_empty());
        enter_alias_rename(&mut manager, &mut term, "new").await;
        manager.section_tab_area = Some(Rect::new(0, 0, 40, 1));
        manager
            .handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    column: 12,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
                Rect::default(),
                &mut term,
            )
            .await
            .unwrap();
        assert!(manager.section == ConfigSection::Zerocode);
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        assert!(manager.edit_buf.is_empty());
        config_key(&mut manager, &mut term, KeyCode::BackTab).await;
        enter_alias_rename(&mut manager, &mut term, "new").await;
        manager.sections.push(typed_section("providers.tts"));
        manager.preview_section(1).await.unwrap();
        assert!(matches!(
            manager.screen,
            Screen::TypeList { section_idx: 1 }
        ));
        assert!(manager.edit_buf.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn provider_alias_rename_section_click_returns_to_root_without_mutation() {
        for drill_level in 0..3 {
            let (mut manager, outbound, mut rx, mut term) =
                alias_rename_manager("providers.models", "openai");
            let breadcrumb = vec!["providers.models".into(), "openai".into(), "old".into()];
            manager.screen = match drill_level {
                0 => manager.screen,
                1 => Screen::FieldList {
                    section_idx: 0,
                    prefix: "providers.models.openai.old".into(),
                    breadcrumb,
                },
                _ => Screen::AliasRename {
                    section_idx: 0,
                    map_path: "providers.models.openai".into(),
                    breadcrumb,
                    from: "old".into(),
                    confirming: true,
                    return_to_fields: false,
                },
            };
            manager.templates = vec![ConfigTemplateEntry {
                path: "providers.models.openai".into(),
            }];
            manager.activate_filter(manager.visible_count());
            manager.filter = Some("old".into());
            manager.edit_buf = "unsaved".into();
            manager.edit_cursor = manager.edit_buf.len();
            manager.active_tab = 1;
            manager.last_tab_area = Some(Rect::new(32, 1, 40, 1));
            manager.last_section_pane_area = Rect::new(0, 0, 30, 10);
            manager.last_section_list_area = Rect::new(1, 1, 28, 8);
            manager.last_section_rows = vec![None, Some(0)];
            let mut click = MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            };
            // Group headers and empty space must not cancel the current work.
            for row in [2, 5] {
                click.row = row;
                manager
                    .handle_mouse(click, Rect::default(), &mut term)
                    .await
                    .unwrap();
                assert_eq!(manager.edit_buf, "unsaved");
                assert!(manager.zeroclaw_pane == ZeroclawPane::Detail);
                assert!(rx.try_recv().is_err());
            }
            click.row = 3;
            let reply = answer_config_request(
                &outbound,
                &mut rx,
                "config/map-keys",
                serde_json::json!({"path": "providers.models.openai"}),
                Ok(serde_json::json!({"keys": ["old", "other"]})),
            );
            let (result, ()) = tokio::join!(
                manager.handle_mouse(click, Rect::default(), &mut term),
                reply
            );
            result.unwrap();
            assert!(matches!(
                manager.screen,
                Screen::TypeList { section_idx: 0 }
            ));
            assert!(manager.zeroclaw_pane == ZeroclawPane::Sections);
            assert_eq!(manager.type_list_labels(0), vec!["openai"]);
            assert_eq!(manager.type_alias_counts, vec![2]);
            assert!(manager.filter.is_none());
            assert!(manager.edit_buf.is_empty());
            assert_eq!(manager.edit_cursor, 0);
            assert_eq!(manager.active_tab, 0);
            assert!(manager.last_tab_area.is_none());
            // Reclicking the starting view keeps its selection and avoids a reload.
            manager.types.push(ConfigTemplateEntry {
                path: "providers.models.anthropic".into(),
            });
            manager.type_cursor = 1;
            manager.tab_names = vec![ConfigTab::Settings, ConfigTab::Personality];
            manager.active_tab = 1;
            manager
                .handle_mouse(click, Rect::default(), &mut term)
                .await
                .unwrap();
            assert!(manager.at_section_top_level());
            assert_eq!(manager.type_cursor, 1);
            assert!(rx.try_recv().is_err());
        }
    }

    fn render_config_for_mouse(manager: &mut App) {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        Terminal::new(backend)
            .unwrap()
            .draw(|frame| manager.draw_into(frame, frame.area()))
            .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_detail_row_click_routes_keyboard_to_the_list() {
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        for screen in 0..3 {
            let (mut manager, _outbound, mut rx, mut term) =
                alias_rename_manager("providers.models", "openai");
            match screen {
                0 => {
                    manager.screen = Screen::TypeList { section_idx: 0 };
                    manager.types = vec![
                        ConfigTemplateEntry {
                            path: "providers.models.openai".into(),
                        },
                        ConfigTemplateEntry {
                            path: "providers.models.anthropic".into(),
                        },
                    ];
                }
                1 => {}
                _ => {
                    manager.screen = Screen::FieldList {
                        section_idx: 0,
                        prefix: "providers.models.openai.old".into(),
                        breadcrumb: vec!["providers.models".into(), "openai".into(), "old".into()],
                    };
                    manager.fields = vec![
                        field("providers.models.openai.old.model"),
                        field("providers.models.openai.old.api_url"),
                    ];
                }
            }
            manager.zeroclaw_pane = ZeroclawPane::Sections;
            render_config_for_mouse(&mut manager);
            let area = manager.last_main_area;
            let click = |row| MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: area.x + 3,
                row,
                modifiers: KeyModifiers::NONE,
            };
            for row in [area.y, area.y + 8] {
                manager
                    .handle_mouse(click(row), Rect::default(), &mut term)
                    .await
                    .unwrap();
                assert!(manager.zeroclaw_pane == ZeroclawPane::Sections);
            }
            for column in [area.x, area.right() - 1] {
                for _ in 0..2 {
                    manager
                        .handle_mouse(
                            MouseEvent {
                                column,
                                ..click(area.y + 1)
                            },
                            Rect::default(),
                            &mut term,
                        )
                        .await
                        .unwrap();
                }
                assert!(manager.zeroclaw_pane == ZeroclawPane::Sections);
                assert_eq!(manager.visible_cursor(), 0);
                assert!(rx.try_recv().is_err());
            }
            manager
                .handle_mouse(click(area.y + 1), Rect::default(), &mut term)
                .await
                .unwrap();
            assert!(manager.zeroclaw_pane == ZeroclawPane::Detail);
            config_key(&mut manager, &mut term, KeyCode::Down).await;
            assert_eq!(manager.visible_cursor(), 1);
            assert_eq!(manager.section_cursor, 0);
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_alias_rename_add_mouse_and_keyboard_open_without_mutation() {
        use crate::keymap::Chord;
        let _guard = crate::keymap::overrides::TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::keymap::overrides::reset();
        crate::keymap::overrides::set_row("config_tab", "enter", vec![Chord::key(KeyCode::F(6))]);
        for costs in [false, true] {
            for count in [0, 2] {
                for keyboard in [false, true] {
                    let (mut manager, outbound, mut rx, mut term) =
                        alias_rename_manager("providers.tts", "openai");
                    manager.aliases.truncate(count);
                    if costs {
                        manager.sections[0].cost_category = "tts".into();
                        manager.alias_tab = 1;
                        manager.cost_resources = manager.aliases.clone();
                    }
                    manager.zeroclaw_pane = ZeroclawPane::Sections;
                    render_config_for_mouse(&mut manager);
                    let area = manager.last_main_area;
                    let click = MouseEvent {
                        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                        column: area.x + 3,
                        row: area.y + 1 + count as u16,
                        modifiers: KeyModifiers::NONE,
                    };
                    for column in [area.x, area.right() - 1] {
                        for _ in 0..2 {
                            manager
                                .handle_mouse(
                                    MouseEvent { column, ..click },
                                    Rect::default(),
                                    &mut term,
                                )
                                .await
                                .unwrap();
                        }
                        assert!(manager.zeroclaw_pane == ZeroclawPane::Sections);
                        assert!(matches!(manager.screen, Screen::AliasList { .. }));
                        assert!(rx.try_recv().is_err());
                    }
                    manager
                        .handle_mouse(click, Rect::default(), &mut term)
                        .await
                        .unwrap();
                    assert!(matches!(manager.screen, Screen::AliasList { .. }));
                    assert!(manager.zeroclaw_pane == ZeroclawPane::Detail);
                    assert!(manager.bottom_hint().contains("F6"));
                    assert!(
                        manager
                            .bottom_hint()
                            .contains(&crate::i18n::t("zc-config-footer-action-add"))
                    );
                    assert!(rx.try_recv().is_err());
                    if keyboard {
                        config_key(&mut manager, &mut term, KeyCode::F(6)).await;
                    } else {
                        manager
                            .handle_mouse(click, Rect::default(), &mut term)
                            .await
                            .unwrap();
                    }
                    let expected_path = if costs {
                        "cost.rates.providers.tts.openai"
                    } else {
                        "providers.tts.openai"
                    };
                    let Screen::AliasCreate {
                        map_path,
                        breadcrumb,
                        ..
                    } = &manager.screen
                    else {
                        panic!("Add must open the name form");
                    };
                    assert_eq!(map_path, expected_path);
                    assert_eq!(breadcrumb.len(), if costs { 3 } else { 2 });
                    if costs {
                        assert_eq!(breadcrumb.last().unwrap(), ConfigTab::Costs.label());
                    }
                    assert!(manager.edit_buf.is_empty());
                    manager.handle_paste("unsaved").await;
                    assert!(rx.try_recv().is_err());
                    let replies = async {
                        answer_config_request(
                            &outbound,
                            &mut rx,
                            "config/map-keys",
                            serde_json::json!({"path": "providers.tts.openai"}),
                            Ok(serde_json::json!({"keys": []})),
                        )
                        .await;
                        if costs {
                            answer_config_request(
                                &outbound,
                                &mut rx,
                                "config/map-keys",
                                serde_json::json!({"path": expected_path}),
                                Ok(serde_json::json!({"keys": []})),
                            )
                            .await;
                        }
                    };
                    let ((), ()) =
                        tokio::join!(config_key(&mut manager, &mut term, KeyCode::Esc), replies);
                    assert!(matches!(manager.screen, Screen::AliasList { .. }));
                    assert_eq!(manager.alias_tab, usize::from(costs));
                    assert!(rx.try_recv().is_err());
                }
            }
        }
        crate::keymap::overrides::reset();
    }

    #[tokio::test]
    async fn provider_alias_rename_filtered_mouse_click_opens_the_visible_alias() {
        let (mut manager, outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        manager.activate_filter(manager.visible_count());
        manager.filter = Some("no-match".into());
        manager.zeroclaw_pane = ZeroclawPane::Sections;
        render_config_for_mouse(&mut manager);
        let area = manager.last_main_area;
        let click = MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 3,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        for _ in 0..2 {
            manager
                .handle_mouse(click, Rect::default(), &mut term)
                .await
                .unwrap();
        }
        assert!(manager.zeroclaw_pane == ZeroclawPane::Sections);
        assert!(matches!(manager.screen, Screen::AliasList { .. }));
        assert!(rx.try_recv().is_err());
        manager.filter = Some("other".into());
        render_config_for_mouse(&mut manager);
        manager
            .handle_mouse(click, Rect::default(), &mut term)
            .await
            .unwrap();
        let reply = answer_config_request(
            &outbound,
            &mut rx,
            "config/list",
            serde_json::json!({"prefix": "providers.models.openai.other"}),
            Ok(serde_json::json!({"entries": []})),
        );
        let (result, ()) = tokio::join!(
            manager.handle_mouse(click, Rect::default(), &mut term),
            reply
        );
        result.unwrap();
        assert!(
            matches!(&manager.screen, Screen::FieldList { prefix, .. } if prefix == "providers.models.openai.other")
        );
        assert!(manager.filter.is_none());
        assert!(manager.zeroclaw_pane == ZeroclawPane::Detail);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn provider_alias_rename_breadcrumbs_navigate_without_saving() {
        for target in 0..3 {
            let (mut manager, outbound, mut rx, mut term) =
                alias_rename_manager("providers.models", "openai");
            let prefix = "providers.models.openai.old";
            manager.fields = vec![field(&format!("{prefix}.model"))];
            manager.screen = Screen::FieldEdit {
                section_idx: 0,
                prefix: prefix.into(),
                breadcrumb: vec!["providers.models".into(), "openai".into(), "old".into()],
                field_idx: 0,
            };
            manager.edit_buf = "unsaved".into();
            manager.edit_cursor = manager.edit_buf.len();
            render_config_for_mouse(&mut manager);
            let areas = manager.last_breadcrumb_areas.clone();
            let click = |column| MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row: areas[0].y,
                modifiers: KeyModifiers::NONE,
            };
            // A separator and the current field only focus; neither cancels input.
            for column in [areas[0].right(), areas[3].x] {
                manager
                    .handle_mouse(click(column), Rect::default(), &mut term)
                    .await
                    .unwrap();
                assert_eq!(manager.edit_buf, "unsaved");
                assert!(rx.try_recv().is_err());
            }
            manager.activate_filter(manager.visible_count());
            manager.filter = Some("old".into());
            let fields = manager.fields.clone();
            let replies = async {
                if target > 0 {
                    answer_config_request(
                        &outbound,
                        &mut rx,
                        "config/list",
                        serde_json::json!({"prefix": prefix}),
                        Ok(serde_json::json!({"entries": fields})),
                    )
                    .await;
                }
                if target == 1 {
                    answer_config_request(
                        &outbound,
                        &mut rx,
                        "config/map-keys",
                        serde_json::json!({"path": "providers.models.openai"}),
                        Ok(serde_json::json!({"keys": []})),
                    )
                    .await;
                }
            };
            let (result, ()) = tokio::join!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    manager.handle_mouse(click(areas[target].x), Rect::default(), &mut term),
                ),
                replies
            );
            result.unwrap().unwrap();
            match target {
                0 => assert!(matches!(
                    manager.screen,
                    Screen::TypeList { section_idx: 0 }
                )),
                1 => assert!(matches!(&manager.screen, Screen::AliasList { map_path, .. }
                    if map_path == "providers.models.openai")),
                _ => assert!(
                    matches!(&manager.screen, Screen::FieldList { prefix: path, .. }
                    if path == prefix)
                ),
            }
            assert!(manager.zeroclaw_pane == ZeroclawPane::Detail);
            assert!(manager.filter.is_none());
            assert!(manager.edit_buf.is_empty());
            assert!(manager.last_breadcrumb_areas.is_empty());
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn provider_alias_rename_field_tab_clicks_focus_the_detail_pane() {
        let (mut manager, outbound, mut rx, mut term) =
            alias_rename_manager("providers.models", "openai");
        let prefix = "providers.models.openai.old";
        manager.tab_names = vec![ConfigTab::Connection, ConfigTab::Model, ConfigTab::Advanced];
        manager.fields = manager
            .tab_names
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let mut entry = field(&format!("{prefix}.field_{i}"));
                entry.tab = *tab;
                entry
            })
            .collect();
        manager.screen = Screen::FieldList {
            section_idx: 0,
            prefix: prefix.into(),
            breadcrumb: vec!["providers.models".into(), "openai".into(), "old".into()],
        };
        for target in [0, 1, 2, 0] {
            manager.zeroclaw_pane = ZeroclawPane::Sections;
            render_config_for_mouse(&mut manager);
            let rect = manager.last_tab_area.unwrap();
            let column = rect.x
                + manager
                    .tab_names
                    .iter()
                    .enumerate()
                    .take(target)
                    .map(|(i, tab)| {
                        tab.label().len() as u16 + 3 + u16::from(i == manager.active_tab) * 2
                    })
                    .sum::<u16>();
            let click = MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            };
            let changing = target != manager.active_tab;
            let fields = manager.fields.clone();
            let reply = async {
                if changing {
                    answer_config_request(
                        &outbound,
                        &mut rx,
                        "config/list",
                        serde_json::json!({"prefix": prefix}),
                        Ok(serde_json::json!({"entries": fields})),
                    )
                    .await;
                }
            };
            let (result, ()) = tokio::join!(
                manager.handle_mouse(click, Rect::default(), &mut term),
                reply
            );
            result.unwrap();
            assert_eq!(manager.active_tab, target);
            assert_eq!(manager.field_cursor, target);
            assert!(manager.zeroclaw_pane == ZeroclawPane::Detail);
            assert!(rx.try_recv().is_err());
        }
    }

    fn field(path: &str) -> ConfigFieldEntry {
        ConfigFieldEntry {
            path: path.to_string(),
            category: String::new(),
            kind: PropKind::String,
            type_hint: String::new(),
            value: None,
            populated: false,
            is_secret: false,
            is_env_overridden: false,
            enum_variants: Vec::new(),
            description: String::new(),
            section: None,
            tab: ConfigTab::None,
            alias_source: None,
            setup: None,
        }
    }

    fn secret_map_field(path: &str) -> ConfigFieldEntry {
        let mut f = field(path);
        f.kind = PropKind::SecretMap;
        f.is_secret = true;
        f
    }

    /// Manager whose RPC is answered by a scripted fake daemon: every
    /// outbound request is recorded and acknowledged (`config/list` gets an
    /// empty entry list, everything else `{}`).
    fn test_manager_with_recording_rpc() -> (App, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
        use crate::jsonrpc::RpcOutbound;
        use tokio::sync::mpsc;
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let outbound = Arc::new(RpcOutbound::new(tx));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let responder = Arc::clone(&outbound);
        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let Ok(req) = serde_json::from_str::<serde_json::Value>(&frame) else {
                    continue;
                };
                let id = req["id"].as_str().unwrap_or_default().to_string();
                let result = if req["method"] == "config/list" {
                    serde_json::json!({ "entries": [] })
                } else {
                    serde_json::json!({})
                };
                recorder.lock().unwrap().push(req);
                responder.dispatch_response(&id, Some(result), None);
            }
        });
        let rpc = Arc::new(RpcClient::with_rpc(outbound));
        (App::new(rpc, std::path::Path::new("/tmp")), seen)
    }

    async fn type_text(manager: &mut App, text: &str) {
        for c in text.chars() {
            manager
                .handle_field_edit(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
    }

    async fn press_enter(manager: &mut App) {
        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    #[test]
    fn secret_map_entry_kind_deserializes_from_daemon_wire_tag() {
        let entry: ConfigFieldEntry = serde_json::from_value(serde_json::json!({
            "path": "mcp.servers.acme.env",
            "category": "Mcp",
            "kind": "secret_map",
            "type_hint": "HashMap<String,String>",
            "populated": false,
            "is_secret": true,
            "description": "Optional environment variables for stdio transport.",
        }))
        .unwrap();
        assert_eq!(entry.kind, PropKind::SecretMap);
        assert_eq!(entry.kind.wire_name(), "secret_map");
    }

    #[test]
    fn secret_map_key_validation_rejects_unusable_names() {
        assert_eq!(
            secret_map_key_validation_key(""),
            Some("zc-config-secret-map-key-empty")
        );
        for bad in ["HAS SPACE", "A=B", "TAB\tKEY", "NL\nKEY"] {
            assert_eq!(
                secret_map_key_validation_key(bad),
                Some("zc-config-secret-map-key-invalid"),
                "{bad:?}"
            );
        }
        for good in ["GITHUB_TOKEN", "Authorization", "X-Api-Key", "x.dotted"] {
            assert_eq!(secret_map_key_validation_key(good), None, "{good:?}");
        }
    }

    #[tokio::test]
    async fn secret_map_row_adds_entry_in_two_steps() {
        let (mut manager, seen) = test_manager_with_recording_rpc();
        manager.fields = vec![secret_map_field("mcp.servers.acme.env")];
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "mcp.servers.acme".into(),
            breadcrumb: vec!["mcp.servers".into(), "acme".into()],
            field_idx: 0,
        };
        manager.prepare_edit_at(0);

        // Step 1: an invalid key is refused without any RPC.
        type_text(&mut manager, "BAD KEY").await;
        press_enter(&mut manager).await;
        assert!(manager.secret_map_entry_key.is_none());
        assert!(manager.status_msg.is_some());
        manager.edit_buf.clear();
        manager.edit_cursor = 0;

        type_text(&mut manager, "API_TOKEN").await;
        press_enter(&mut manager).await;
        assert_eq!(manager.secret_map_entry_key.as_deref(), Some("API_TOKEN"));
        assert!(manager.edit_buf.is_empty(), "value step starts blank");

        // Step 2: an empty value is refused (it would remove the entry).
        press_enter(&mut manager).await;
        assert_eq!(manager.secret_map_entry_key.as_deref(), Some("API_TOKEN"));
        assert!(seen.lock().unwrap().is_empty(), "no RPC before a value");

        type_text(&mut manager, "s3cr3t").await;
        press_enter(&mut manager).await;

        let requests = seen.lock().unwrap().clone();
        let set = requests
            .iter()
            .find(|r| r["method"] == "config/set")
            .expect("config/set must be sent");
        assert_eq!(set["params"]["prop"], "mcp.servers.acme.env.API_TOKEN");
        assert_eq!(set["params"]["value"], "s3cr3t");
        assert!(manager.secret_map_entry_key.is_none());
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
    }

    #[tokio::test]
    async fn channels_type_list_reserves_a_global_settings_row() {
        let mut mgr = test_manager();
        mgr.sections = vec![typed_section("channels")];
        mgr.screen = Screen::TypeList { section_idx: 0 };
        mgr.types = vec![
            ConfigTemplateEntry {
                path: "channels.telegram".to_string(),
            },
            ConfigTemplateEntry {
                path: "channels.whatsapp".to_string(),
            },
        ];

        assert_eq!(mgr.visible_count(), 3);
        assert_eq!(
            mgr.type_list_labels(0),
            vec!["[channels]", "telegram", "whatsapp"]
        );
        assert_eq!(mgr.template_idx_for_type_row(0, 0), None);
        assert_eq!(mgr.template_idx_for_type_row(0, 1), Some(0));
        assert_eq!(mgr.template_idx_for_type_row(0, 2), Some(1));
    }

    #[tokio::test]
    async fn non_channel_type_list_does_not_reserve_global_settings_row() {
        let mut mgr = test_manager();
        mgr.sections = vec![typed_section("providers.models")];
        mgr.screen = Screen::TypeList { section_idx: 0 };
        mgr.types = vec![ConfigTemplateEntry {
            path: "providers.models.anthropic".to_string(),
        }];

        assert_eq!(mgr.type_list_labels(0), vec!["anthropic"]);
        assert_eq!(mgr.template_idx_for_type_row(0, 0), Some(0));
    }

    #[test]
    fn channels_root_settings_keep_only_direct_children() {
        let mut fields = vec![
            field("channels.show_tool_calls"),
            field("channels.ack_reactions"),
            field("channels.matrix.default.enabled"),
            field("channels.whatsapp.personal.session_path"),
            field("agents.default.model"),
        ];

        retain_direct_children(&mut fields, "channels");

        let paths: Vec<&str> = fields.iter().map(|field| field.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["channels.show_tool_calls", "channels.ack_reactions"]
        );
    }

    #[tokio::test]
    async fn filtered_type_list_selected_row_tracks_filter_cursor() {
        let mut mgr = test_manager();
        mgr.sections = vec![typed_section("channels")];
        mgr.screen = Screen::TypeList { section_idx: 0 };
        mgr.types = vec![
            ConfigTemplateEntry {
                path: "channels.telegram".to_string(),
            },
            ConfigTemplateEntry {
                path: "channels.whatsapp".to_string(),
            },
        ];
        mgr.type_cursor = 2;
        mgr.filter = Some("chan".to_string());
        mgr.filter_origin = Some(FilterOrigin {
            pane: ZeroclawPane::Detail,
            section_cursor: 0,
            active_tab: 0,
            detail: None,
        });
        mgr.filter_cursor = 0;

        assert_eq!(mgr.selected_type_row(), Some(0));
    }

    #[tokio::test]
    async fn left_on_leftmost_alias_tab_walks_back_to_type_list() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut mgr = test_manager();
        mgr.sections = vec![entry_with_cost("providers.models", "models")];
        mgr.screen = Screen::AliasList {
            section_idx: 0,
            map_path: "providers.models.anthropic".to_string(),
            breadcrumb: vec!["providers.models".to_string(), "anthropic".to_string()],
        };
        mgr.alias_tab = 0;

        assert!(
            mgr.alias_list_has_tabs(),
            "Aliases/Costs tabs must be present for this scenario"
        );

        mgr.handle_alias_list(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(
            matches!(mgr.screen, Screen::TypeList { section_idx: 0 }),
            "Left on the leftmost alias tab walks out to the type list like Back"
        );
    }

    fn field_with_value(kind: PropKind, value: &str, populated: bool) -> ConfigFieldEntry {
        ConfigFieldEntry {
            path: "test.field".into(),
            category: String::new(),
            kind,
            type_hint: String::new(),
            value: Some(serde_json::Value::String(value.into())),
            populated,
            is_secret: false,
            is_env_overridden: false,
            enum_variants: Vec::new(),
            description: String::new(),
            section: None,
            tab: ConfigTab::None,
            alias_source: None,
            setup: None,
        }
    }

    #[tokio::test]
    async fn prepare_edit_respects_populated_field() {
        let mut mgr = test_manager();

        mgr.fields = vec![field_with_value(PropKind::String, "<unset>", false)];
        mgr.prepare_edit_at(0);
        assert_eq!(
            mgr.edit_buf, "",
            "scalar unset field must start with an empty edit buffer"
        );

        mgr.fields = vec![field_with_value(
            PropKind::StringArray,
            r#"["ignored"]"#,
            false,
        )];
        mgr.prepare_edit_at(0);
        assert_eq!(
            mgr.edit_buf, "",
            "StringArray unset field must start with an empty edit buffer"
        );

        mgr.fields = vec![field_with_value(PropKind::String, "<unset>", true)];
        mgr.prepare_edit_at(0);
        assert_eq!(
            mgr.edit_buf, "<unset>",
            "populated scalar values must be preserved verbatim"
        );
    }

    /// Manager wired to a responder task that records every request method
    /// and answers config/set and config/list. List responses echo the
    /// two-field tabbed fixture, applying the saved value to a.second.
    fn responding_manager() -> (App, Arc<std::sync::Mutex<Vec<String>>>) {
        use crate::jsonrpc::RpcOutbound;
        use tokio::sync::mpsc;
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(16);
        let outbound = Arc::new(RpcOutbound::new(writer_tx));
        let rpc = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
        let manager = App::new(rpc, std::path::Path::new("/tmp"));
        let calls: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_for_task = Arc::clone(&calls);
        let outbound_for_task = Arc::clone(&outbound);
        tokio::spawn(async move {
            let mut saved: Option<(String, serde_json::Value)> = None;
            while let Some(raw) = writer_rx.recv().await {
                let Ok(req) = serde_json::from_str::<serde_json::Value>(&raw) else {
                    continue;
                };
                let method = req["method"].as_str().unwrap_or_default().to_string();
                calls_for_task
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(method.clone());
                let id = req["id"].as_str().unwrap_or_default().to_string();
                let result = if method == crate::client::method::CONFIG_SET {
                    saved = Some((
                        req["params"]["prop"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        req["params"]["value"].clone(),
                    ));
                    serde_json::json!({"revision": {"epoch": "test-epoch", "sequence": 1}})
                } else if method == crate::client::method::CONFIG_MAP_KEYS {
                    serde_json::json!({ "keys": ["first", "second"] })
                } else if method == crate::client::method::CONFIG_LIST {
                    let mut first = field("a.first");
                    first.tab = ConfigTab::Connection;
                    let mut second = field("a.second");
                    second.tab = ConfigTab::Advanced;
                    if let Some((prop, value)) = &saved
                        && prop == "a.second"
                    {
                        second.value = Some(value.clone());
                        second.populated = true;
                    }
                    serde_json::json!({ "entries": [
                        serde_json::to_value(&first).unwrap(),
                        serde_json::to_value(&second).unwrap(),
                    ] })
                } else if method == crate::client::method::CONFIG_MAP_KEY_CREATE {
                    serde_json::json!({"created": false})
                } else if method == crate::client::method::CONFIG_STATUS {
                    serde_json::json!({ "application": {
                        "published_revision": {"epoch": "test-epoch", "sequence": 1},
                        "record_limit": 512, "truncated": false,
                        "records": [
                            {"path": ["a", "second"], "target": {"kind": "daemon"},
                             "revision": {"epoch": "test-epoch", "sequence": 1},
                             "outcome": "queued_for_reload", "reason": "daemon_reload_required"},
                            {"path": ["a", "second"], "target": {"kind": "session", "id": "test-session", "generation": 7},
                             "revision": {"epoch": "test-epoch", "sequence": 1},
                             "outcome": "applied_live"}
                        ]
                    }})
                } else {
                    serde_json::json!({})
                };
                outbound_for_task.dispatch_response(&id, Some(result), None);
            }
        });
        (manager, calls)
    }

    #[tokio::test]
    async fn config_application_noop_alias_creation_preserves_the_last_save() {
        let (mut manager, _) = responding_manager();
        manager
            .application_feedback
            .record_saved("previous edit".into(), None);
        manager.screen = Screen::AliasCreate {
            section_idx: 0,
            map_path: "model_routes".into(),
            breadcrumb: vec!["Model routes".into()],
        };
        manager.edit_buf = "existing".into();
        manager
            .handle_alias_create(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            manager
                .application_feedback
                .summary()
                .contains("previous edit")
        );
        assert!(
            !manager
                .application_feedback
                .summary()
                .contains("model_routes.existing")
        );
    }

    #[tokio::test]
    async fn config_application_feedback_follows_the_real_config_save_hook() {
        let (mut manager, calls) = responding_manager();
        manager.sections = vec![typed_section("a")];
        manager.fields = vec![field("a.first"), field("a.second")];
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "a".into(),
            breadcrumb: vec!["a".into()],
            field_idx: 1,
        };
        manager.prepare_edit_at(1);
        manager.edit_buf = "new".into();
        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(manager.fields[1].value, Some(serde_json::json!("new")));
        assert!(
            manager
                .application_feedback
                .summary()
                .contains("can't confirm")
        );
        // One line, not two: the save names the field only in the summary.
        assert_eq!(manager.status_msg, None);
        manager.tick_application_status(true);
        for _ in 0..16 {
            tokio::task::yield_now().await;
            manager.tick_application_status(true);
            if manager
                .application_feedback
                .summary()
                .contains("already been applied")
            {
                break;
            }
        }
        let summary = manager.application_feedback.summary();
        assert!(summary.contains("a.second"));
        assert!(summary.contains("Open sessions already use it"));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| manager.draw_into(frame, frame.area()))
            .unwrap();
        let output = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(output.contains("a.second"));
        assert!(output.contains("Open sessions already use it"));
        let buffer = terminal.backend().buffer();
        let row = (0..buffer.area.height)
            .find(|&y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .starts_with("Saved ")
            })
            .expect("summary row");
        assert_eq!(buffer[(0, row)].fg, crate::theme::warn_style().fg.unwrap());
        manager.application_feedback.toggle_details();
        terminal
            .draw(|frame| manager.draw_into(frame, frame.area()))
            .unwrap();
        let output = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(output.contains("test-epoch"));
        assert!(output.contains("test-session"));
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["config/set", "config/list", "config/status"]
        );
        manager.tick_application_status(false);
        assert!(
            manager
                .application_feedback
                .summary()
                .contains("can't confirm")
        );
    }

    #[tokio::test]
    async fn scalar_save_refreshes_once_and_keeps_tab_and_cursor() {
        let (mut manager, calls) = responding_manager();
        let mut first = field("a.first");
        first.tab = ConfigTab::Connection;
        let mut second = field("a.second");
        second.tab = ConfigTab::Advanced;
        manager.fields = vec![first, second];
        manager.tab_names = vec![ConfigTab::Connection, ConfigTab::Advanced];
        manager.active_tab = 1;
        manager.field_cursor = 1;
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "a".into(),
            breadcrumb: vec!["a".into()],
            field_idx: 1,
        };
        manager.prepare_edit_at(1);
        manager.edit_buf = "new".into();

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["config/set", "config/list"],
            "a scalar save must issue exactly one set and one list request"
        );
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(manager.active_tab, 1, "the active tab must survive a save");
        assert_eq!(manager.field_cursor, 1, "the cursor must survive a save");
        assert!(
            manager.tab_field_indices().contains(&manager.field_cursor),
            "the restored cursor must be visible on the active tab"
        );
        assert_eq!(manager.fields[1].value, Some(serde_json::json!("new")));
    }

    #[tokio::test]
    async fn multiline_save_refreshes_once_and_keeps_tab_and_cursor() {
        let (mut manager, calls) = responding_manager();
        let mut first = field("a.first");
        first.tab = ConfigTab::Connection;
        let mut second = field("a.second");
        second.kind = PropKind::StringArray;
        second.tab = ConfigTab::Advanced;
        manager.fields = vec![first, second];
        manager.tab_names = vec![ConfigTab::Connection, ConfigTab::Advanced];
        manager.active_tab = 1;
        manager.field_cursor = 1;
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "a".into(),
            breadcrumb: vec!["a".into()],
            field_idx: 1,
        };
        manager.prepare_edit_at(1);
        manager.edit_buf = "x\ny".into();

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["config/set", "config/list"],
            "a multiline save must issue exactly one set and one list request"
        );
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(manager.active_tab, 1, "the active tab must survive a save");
        assert_eq!(manager.field_cursor, 1, "the cursor must survive a save");
        assert!(
            manager.tab_field_indices().contains(&manager.field_cursor),
            "the restored cursor must be visible on the active tab"
        );
        assert_eq!(manager.fields[1].value, Some(serde_json::json!(["x", "y"])));
    }

    #[tokio::test]
    async fn choice_save_refreshes_once_and_keeps_tab_and_cursor() {
        let (mut manager, calls) = responding_manager();
        let mut first = field("a.first");
        first.tab = ConfigTab::Connection;
        let mut second = field("a.second");
        second.kind = PropKind::Enum;
        second.tab = ConfigTab::Advanced;
        manager.fields = vec![first, second];
        manager.tab_names = vec![ConfigTab::Connection, ConfigTab::Advanced];
        manager.active_tab = 1;
        manager.field_cursor = 1;
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "a".into(),
            breadcrumb: vec!["a".into()],
            field_idx: 1,
        };
        manager.select_items = vec!["one".into(), "two".into()];

        manager.commit_select(1).await.unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["config/set", "config/list"],
            "a choice save must issue exactly one set and one list request"
        );
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(manager.active_tab, 1, "the active tab must survive a save");
        assert_eq!(manager.field_cursor, 1, "the cursor must survive a save");
        assert!(
            manager.tab_field_indices().contains(&manager.field_cursor),
            "the restored cursor must be visible on the active tab"
        );
        assert_eq!(manager.fields[1].value, Some(serde_json::json!("two")));
    }

    #[tokio::test]
    async fn skill_bundle_save_invalidates_the_skills_list_without_an_eager_fetch() {
        let (mut manager, calls) = responding_manager();
        let mut description = field("skill-bundles.demo.description");
        description.tab = ConfigTab::Settings;
        let mut include = field("skill-bundles.demo.include");
        include.tab = ConfigTab::Settings;
        manager.fields = vec![description, include];
        manager.tab_names = vec![ConfigTab::Settings, ConfigTab::Skills];
        manager.active_tab = 0;
        manager.skills_bundle = "demo".into();
        manager.skills_list = vec![crate::client::SkillListEntry {
            name: "from-dir-a".into(),
        }];
        manager.skills_active = Some("from-dir-a".into());
        manager.skills_body = "stale".into();
        manager.skills_body_loaded = "stale".into();
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "skill-bundles.demo".into(),
            breadcrumb: vec!["skill-bundles.demo".into()],
            field_idx: 0,
        };
        manager.prepare_edit_at(0);
        manager.edit_buf = "new".into();

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["config/set", "config/list"],
            "a skill-bundle save must refresh once and never eagerly fetch skills"
        );
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(manager.active_tab, 0, "the active tab must survive a save");
        assert!(
            manager.skills_list.is_empty(),
            "a save in the section must drop the cached skills list"
        );
        assert!(
            manager.skills_active.is_none(),
            "a save in the section must drop the cached skills selection"
        );
        assert!(
            manager.skills_body.is_empty(),
            "a save in the section must drop the cached skill body"
        );
        assert_eq!(
            manager.skills_bundle, "demo",
            "the bundle identity is derived from the prefix and must survive"
        );
    }

    #[tokio::test]
    async fn agent_save_invalidates_the_personality_files_without_an_eager_fetch() {
        let (mut manager, calls) = responding_manager();
        let mut model = field("agents.demo.model");
        model.tab = ConfigTab::Settings;
        let mut workspace = field("agents.demo.workspace.path");
        workspace.tab = ConfigTab::Settings;
        manager.fields = vec![model, workspace];
        manager.tab_names = vec![ConfigTab::Settings, ConfigTab::Personality];
        manager.active_tab = 0;
        manager.personality_agent = "demo".into();
        manager.personality_files = vec![crate::client::PersonalityFileEntry {
            filename: "SOUL.md".into(),
            exists: true,
            size: 3,
        }];
        manager.personality_active_file = Some("SOUL.md".into());
        manager.personality_content = "stale".into();
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "agents.demo".into(),
            breadcrumb: vec!["agents.demo".into()],
            field_idx: 0,
        };
        manager.prepare_edit_at(0);
        manager.edit_buf = "new".into();

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["config/set", "config/list"],
            "an agent save must refresh once and never eagerly fetch personality files"
        );
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(manager.active_tab, 0, "the active tab must survive a save");
        assert!(
            manager.personality_files.is_empty(),
            "a save in the section must drop the cached personality file list"
        );
        assert!(
            manager.personality_active_file.is_none(),
            "a save in the section must drop the cached personality selection"
        );
        assert!(
            manager.personality_content.is_empty(),
            "a save in the section must drop the cached personality content"
        );
        assert_eq!(
            manager.personality_agent, "demo",
            "the agent identity is derived from the prefix and must survive"
        );
    }

    #[tokio::test]
    async fn ordinary_section_save_leaves_composite_state_alone() {
        let (mut manager, calls) = responding_manager();
        let mut first = field("a.first");
        first.tab = ConfigTab::Connection;
        let mut second = field("a.second");
        second.tab = ConfigTab::Advanced;
        manager.fields = vec![first, second];
        manager.tab_names = vec![ConfigTab::Connection, ConfigTab::Advanced];
        manager.active_tab = 1;
        manager.field_cursor = 1;
        manager.skills_list = vec![crate::client::SkillListEntry {
            name: "unrelated".into(),
        }];
        manager.personality_files = vec![crate::client::PersonalityFileEntry {
            filename: "SOUL.md".into(),
            exists: true,
            size: 3,
        }];
        manager.screen = Screen::FieldEdit {
            section_idx: 0,
            prefix: "a".into(),
            breadcrumb: vec!["a".into()],
            field_idx: 1,
        };
        manager.prepare_edit_at(1);
        manager.edit_buf = "new".into();

        manager
            .handle_field_edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["config/set", "config/list"],
            "the save must go through the ordinary single-refresh path"
        );
        assert!(matches!(manager.screen, Screen::FieldList { .. }));
        assert_eq!(
            manager.skills_list.len(),
            1,
            "a save outside composite sections must not drop the skills list"
        );
        assert_eq!(
            manager.personality_files.len(),
            1,
            "a save outside composite sections must not drop the personality files"
        );
    }
}
