use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use zeroclaw_config::sections::SectionShape;
use zeroclaw_config::traits::{ConfigFieldEntry, ConfigTab, PropKind};

use crate::client::{ConfigSectionEntry, ConfigTemplateEntry, RpcClient};
use crate::theme;

pub(crate) type Term = Terminal<CrosstermBackend<Stdout>>;

pub(crate) fn init_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

pub(crate) fn restore_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
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

// ── App state ────────────────────────────────────────────────────

pub(crate) struct App<'a> {
    rpc: &'a RpcClient,
    screen: Screen,
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
    last_list_offset: usize,
    last_tab_area: Option<Rect>,
    double_click: crate::mouse::DoubleClickTracker,
}

impl<'a> App<'a> {
    pub(crate) fn new(rpc: &'a RpcClient) -> Self {
        Self {
            rpc,
            screen: Screen::SectionList,
            sections: Vec::new(),
            templates: Vec::new(),
            section_cursor: 0,
            types: Vec::new(),
            type_alias_counts: Vec::new(),
            type_cursor: 0,
            aliases: Vec::new(),
            alias_enabled: Vec::new(),
            alias_cursor: 0,
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
            last_list_offset: 0,
            last_tab_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
        }
    }

    /// Load initial data from the daemon. Call once before draw/handle_key.
    pub(crate) async fn init(&mut self) -> Result<()> {
        self.sections = self.rpc.config_sections().await?;
        self.templates = self.rpc.config_templates().await?;
        Ok(())
    }

    /// Draw the current screen into the given area.
    pub(crate) fn draw_into(&mut self, frame: &mut Frame, area: Rect) {
        // Clone values out of `screen` so draw methods can take `&mut self`.
        match &self.screen {
            Screen::SectionList => self.draw_section_list(frame, area),
            Screen::TypeList { section_idx } => {
                let si = *section_idx;
                self.draw_type_list(frame, area, si);
            }
            Screen::AliasList {
                section_idx,
                breadcrumb,
                ..
            } => {
                let si = *section_idx;
                let bc = breadcrumb.clone();
                self.draw_alias_list(frame, area, si, &bc);
            }
            Screen::AliasCreate { breadcrumb, .. } => {
                let bc = breadcrumb.clone();
                self.draw_alias_create(frame, area, &bc);
            }
            Screen::FieldList {
                section_idx,
                breadcrumb,
                ..
            } => {
                let si = *section_idx;
                let bc = breadcrumb.clone();
                self.draw_field_list(frame, area, si, &bc);
            }
            Screen::FieldEdit {
                breadcrumb,
                field_idx,
                ..
            } => {
                let bc = breadcrumb.clone();
                let fi = *field_idx;
                self.draw_field_edit(frame, area, &bc, fi);
            }
        }
    }

    /// Handle a key event. Returns `Ok(true)` when the user wants to
    /// quit this mode (Esc/q at the top-level section list).
    pub(crate) async fn handle_key(&mut self, key: KeyEvent, term: &mut Term) -> Result<bool> {
        self.status_msg = None;

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
        Ok(false)
    }

    /// Handle a mouse event forwarded from the app event loop.
    pub(crate) async fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        _area: Rect,
        term: &mut Term,
    ) -> Result<()> {
        use crate::mouse;

        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // Tab bar click (FieldList only).
                if let Some(tab_rect) = self.last_tab_area {
                    if mouse::in_rect(mouse.column, mouse.row, tab_rect) {
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
                        ) {
                            if idx != self.active_tab && idx < self.tab_names.len() {
                                self.active_tab = idx;
                                self.field_cursor =
                                    self.tab_field_indices().first().copied().unwrap_or(0);
                                self.deactivate_filter();
                                self.on_tab_switched(term).await?;
                            }
                        }
                        return Ok(());
                    }
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

            MouseEventKind::ScrollUp => {
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) {
                    let cur = self.visible_cursor();
                    let count = self.visible_count();
                    let next = mouse::list_scroll(cur, count, true, 3);
                    self.set_visible_cursor(next);
                }
            }

            MouseEventKind::ScrollDown => {
                if mouse::in_rect(mouse.column, mouse.row, self.last_main_area) {
                    let cur = self.visible_cursor();
                    let count = self.visible_count();
                    let next = mouse::list_scroll(cur, count, false, 3);
                    self.set_visible_cursor(next);
                }
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
                let vis = self.filtered_indices(&self.aliases);
                // +1 for [+ Add] when not filtering
                if self.filter.is_none() {
                    vis.len() + 1
                } else {
                    vis.len()
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

    /// Helper: visible field count for the regular (non-composite) field list.
    fn visible_field_count(&self) -> usize {
        let tab_indices = self.tab_field_indices();
        let tab_names: Vec<String> = tab_indices
            .iter()
            .map(|&i| {
                self.fields[i]
                    .path
                    .rsplit('.')
                    .next()
                    .unwrap_or(&self.fields[i].path)
                    .to_string()
            })
            .collect();
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
                if self.filter.is_some() {
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
        let tab_names: Vec<String> = tab_indices
            .iter()
            .map(|&i| {
                self.fields[i]
                    .path
                    .rsplit('.')
                    .next()
                    .unwrap_or(&self.fields[i].path)
                    .to_string()
            })
            .collect();
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
                if self.filter.is_some() {
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
        let tab_names: Vec<String> = tab_indices
            .iter()
            .map(|&i| {
                self.fields[i]
                    .path
                    .rsplit('.')
                    .next()
                    .unwrap_or(&self.fields[i].path)
                    .to_string()
            })
            .collect();
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
                if self.alias_cursor < self.aliases.len() {
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
        Ok(())
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
            FilterAction::Consumed => return Ok(false),
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.section_cursor = orig;
                    self.deactivate_filter();
                    return self.enter_section(orig).await;
                }
                return Ok(false);
            }
            FilterAction::Passthrough => {}
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Up | KeyCode::Char('k') => {
                self.section_cursor = self.section_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.section_cursor + 1 < self.sections.len() {
                    self.section_cursor += 1;
                }
            }
            KeyCode::Enter => {
                return self.enter_section(self.section_cursor).await;
            }
            _ => {}
        }
        Ok(false)
    }

    async fn enter_section(&mut self, idx: usize) -> Result<bool> {
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
            self.status_msg = None;
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

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::SectionList;
                self.status_msg = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.type_cursor = self.type_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.type_cursor + 1 < self.types.len() {
                    self.type_cursor += 1;
                }
            }
            KeyCode::Enter => {
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
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::AliasList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                {
                    if breadcrumb.len() >= 2 {
                        self.types = self.types_for_section(&self.sections[section_idx].key);
                        self.screen = Screen::TypeList { section_idx };
                    }
                }
                self.status_msg = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.alias_cursor = self.alias_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.alias_cursor + 1 < visible_total {
                    self.alias_cursor += 1;
                }
            }
            KeyCode::Enter => {
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
            KeyCode::Char('x') => {
                if self.alias_cursor < self.aliases.len() {
                    if let Screen::AliasList { map_path, .. } = &self.screen {
                        let alias = self.aliases[self.alias_cursor].clone();
                        let map_path = map_path.clone();
                        match self.rpc.config_map_key_delete(&map_path, &alias).await {
                            Ok(()) => {
                                self.status_msg = Some(format!("Deleted {alias}"));
                                self.load_aliases(&map_path).await?;
                                if self.alias_cursor > 0 && self.alias_cursor >= self.aliases.len()
                                {
                                    self.alias_cursor = self.aliases.len().saturating_sub(1);
                                }
                            }
                            Err(e) => self.status_msg = Some(format!("Delete failed: {e}")),
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn enter_alias(&mut self, orig_idx: usize) -> Result<()> {
        if let Some(alias) = self.aliases.get(orig_idx) {
            if let Screen::AliasList {
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
        }
        Ok(())
    }

    // ── Alias creation ───────────────────────────────────────────

    async fn handle_alias_create(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                if let Screen::AliasCreate {
                    section_idx,
                    map_path,
                    breadcrumb,
                    ..
                } = std::mem::replace(&mut self.screen, Screen::SectionList)
                {
                    self.load_aliases(&map_path).await?;
                    self.screen = Screen::AliasList {
                        section_idx,
                        map_path,
                        breadcrumb,
                    };
                }
            }
            KeyCode::Enter => {
                let name = self.edit_buf.trim().to_string();
                if name.is_empty() {
                    self.status_msg = Some("Alias name cannot be empty".into());
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
                            self.status_msg = Some(format!("Create failed: {e}"));
                            self.load_aliases(&map_path).await?;
                            self.screen = Screen::AliasList {
                                section_idx,
                                map_path,
                                breadcrumb,
                            };
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.edit_buf.pop();
            }
            KeyCode::Char(c) => {
                self.edit_buf.push(c);
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
                ConfigTab::Personality => return self.handle_personality_tab(key, term).await,
                ConfigTab::Skills => return self.handle_skills_tab(key, term).await,
                _ => {}
            }
        }

        // Fields visible under active tab, then filtered by `/` query.
        let tab_indices = self.tab_field_indices();
        let tab_names: Vec<String> = tab_indices
            .iter()
            .map(|&i| {
                self.fields[i]
                    .path
                    .rsplit('.')
                    .next()
                    .unwrap_or(&self.fields[i].path)
                    .to_string()
            })
            .collect();
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

        match key.code {
            KeyCode::Left | KeyCode::Char('h') if !self.tab_names.is_empty() => {
                self.active_tab = self.active_tab.saturating_sub(1);
                self.field_cursor = self.tab_field_indices().first().copied().unwrap_or(0);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            KeyCode::Right | KeyCode::Char('l') if !self.tab_names.is_empty() => {
                if self.active_tab + 1 < self.tab_names.len() {
                    self.active_tab += 1;
                }
                self.field_cursor = self.tab_field_indices().first().copied().unwrap_or(0);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                {
                    if breadcrumb.len() >= 2 {
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
                }
                self.status_msg = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(pos) = visible.iter().position(|&i| i == self.field_cursor) {
                    if pos > 0 {
                        self.field_cursor = visible[pos - 1];
                    }
                } else if let Some(&first) = visible.first() {
                    self.field_cursor = first;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(pos) = visible.iter().position(|&i| i == self.field_cursor) {
                    if pos + 1 < visible.len() {
                        self.field_cursor = visible[pos + 1];
                    }
                } else if let Some(&first) = visible.first() {
                    self.field_cursor = first;
                }
            }
            KeyCode::Enter => {
                if visible.contains(&self.field_cursor) {
                    self.enter_field_edit(self.field_cursor, term).await;
                }
            }
            KeyCode::Char('d') => {
                if let Some(field) = self.fields.get(self.field_cursor) {
                    let prop = field.path.clone();
                    let saved_cursor = self.field_cursor;
                    if let Screen::FieldList { prefix, .. } = &self.screen {
                        let prefix = prefix.clone();
                        match self.rpc.config_delete(&prop).await {
                            Ok(()) => {
                                self.status_msg = Some(format!("Reset {prop}"));
                                self.load_fields(&prefix).await?;
                                self.field_cursor =
                                    saved_cursor.min(self.fields.len().saturating_sub(1));
                            }
                            Err(e) => self.status_msg = Some(format!("Delete failed: {e}")),
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
        if !self.is_composite_tab() {
            return Ok(());
        }
        match self.tab_names[self.active_tab] {
            ConfigTab::Personality => {
                if self.personality_files.is_empty() {
                    self.status_msg = Some("Loading personality files...".into());
                    let _ = self.draw(term);
                    match self.load_personality_files().await {
                        Ok(()) => self.status_msg = None,
                        Err(e) => self.status_msg = Some(format!("Load failed: {e}")),
                    }
                }
            }
            ConfigTab::Skills => {
                if self.skills_list.is_empty() {
                    self.status_msg = Some("Loading skills...".into());
                    let _ = self.draw(term);
                    match self.load_skills_list().await {
                        Ok(()) => self.status_msg = None,
                        Err(e) => self.status_msg = Some(format!("Load failed: {e}")),
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
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.active_tab = self.active_tab.saturating_sub(1);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.active_tab + 1 < self.tab_names.len() {
                    self.active_tab += 1;
                }
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                // Back to alias list (reuse the normal Esc logic).
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                {
                    if breadcrumb.len() >= 2 {
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
                }
                self.status_msg = None;
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.personality_cursor = self.personality_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.personality_cursor + 1 < self.personality_files.len() {
                    self.personality_cursor += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(file) = self.personality_files.get(self.personality_cursor) {
                    let filename = file.filename.clone();
                    self.status_msg = Some(format!("Loading {filename}..."));
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
                                                self.status_msg = Some(format!("Saved {filename}"));
                                                let _ = self.load_personality_files().await;
                                            }
                                            Err(e) => {
                                                self.status_msg = Some(format!("Save failed: {e}"));
                                            }
                                        }
                                    } else {
                                        self.status_msg = None;
                                    }
                                    self.personality_active_file = None;
                                }
                                Err(_) => {
                                    self.status_msg = None;
                                    // $EDITOR unavailable — stays in inline
                                    // editor mode (personality_active_file is
                                    // already set by load_personality_file).
                                }
                            }
                        }
                        Err(e) => self.status_msg = Some(format!("Load failed: {e}")),
                    }
                }
            }
            KeyCode::Char('t') => {
                // Fill selected file from default template.
                if let Some(file) = self.personality_files.get(self.personality_cursor) {
                    let filename = file.filename.clone();
                    let agent = self.personality_agent.clone();
                    self.status_msg = Some("Fetching templates...".into());
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
                                        self.status_msg =
                                            Some(format!("Template loaded for {filename}"));
                                    }
                                }
                            } else {
                                self.status_msg =
                                    Some(format!("No template available for {filename}"));
                            }
                        }
                        Err(e) => self.status_msg = Some(format!("Template fetch failed: {e}")),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_personality_editor(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                // Back to file picker. Warn if dirty.
                if self.personality_content != self.personality_loaded {
                    self.status_msg = Some("Unsaved changes discarded".into());
                }
                self.personality_active_file = None;
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+S = save.
                if let Some(filename) = &self.personality_active_file {
                    let filename = filename.clone();
                    let agent = self.personality_agent.clone();
                    let content = self.personality_content.clone();
                    if content.chars().count() > self.personality_max_chars {
                        self.status_msg = Some(format!(
                            "Over {} char limit — cannot save",
                            self.personality_max_chars
                        ));
                        return Ok(());
                    }
                    self.status_msg = Some(format!("Saving {filename}..."));
                    let _ = self.draw(term);
                    match self.rpc.personality_put(&agent, &filename, &content).await {
                        Ok(_) => {
                            self.personality_loaded = self.personality_content.clone();
                            self.status_msg = Some(format!("Saved {filename}"));
                            // Refresh file list to update exists/size.
                            let _ = self.load_personality_files().await;
                            // Re-open the same file.
                            self.personality_active_file = Some(filename);
                        }
                        Err(e) => self.status_msg = Some(format!("Save failed: {e}")),
                    }
                }
            }
            KeyCode::Enter => {
                self.personality_content.push('\n');
            }
            KeyCode::Backspace => {
                self.personality_content.pop();
            }
            KeyCode::Char(c) => {
                self.personality_content.push(c);
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

        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.active_tab = self.active_tab.saturating_sub(1);
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.active_tab + 1 < self.tab_names.len() {
                    self.active_tab += 1;
                }
                self.deactivate_filter();
                self.on_tab_switched(term).await?;
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                let screen = std::mem::replace(&mut self.screen, Screen::SectionList);
                if let Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } = screen
                {
                    if breadcrumb.len() >= 2 {
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
                }
                self.status_msg = None;
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.skills_cursor = self.skills_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.skills_cursor + 1 < self.skills_list.len() {
                    self.skills_cursor += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(skill) = self.skills_list.get(self.skills_cursor) {
                    let name = skill.name.clone();
                    self.status_msg = Some(format!("Loading {name}..."));
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
                                                self.status_msg = Some(format!("Saved {name}"));
                                            }
                                            Err(e) => {
                                                self.status_msg = Some(format!("Save failed: {e}"));
                                            }
                                        }
                                    } else {
                                        self.status_msg = None;
                                    }
                                    self.skills_active = None;
                                }
                                Err(_) => {
                                    self.status_msg = None;
                                    // $EDITOR unavailable — falls into inline
                                    // editor.
                                }
                            }
                        }
                        Err(e) => self.status_msg = Some(format!("Load failed: {e}")),
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(skill) = self.skills_list.get(self.skills_cursor) {
                    let name = skill.name.clone();
                    let bundle = self.skills_bundle.clone();
                    self.status_msg = Some(format!("Deleting {name}..."));
                    let _ = self.draw(term);
                    match self.rpc.skills_delete(&bundle, &name).await {
                        Ok(_) => {
                            self.status_msg = Some(format!("Archived {name}"));
                            let _ = self.load_skills_list().await;
                        }
                        Err(e) => self.status_msg = Some(format!("Delete failed: {e}")),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_skills_editor(&mut self, key: KeyEvent, term: &mut Term) -> Result<()> {
        let _ = term;
        match key.code {
            KeyCode::Esc => {
                if self.skills_body != self.skills_body_loaded {
                    self.status_msg = Some("Unsaved changes discarded".into());
                }
                self.skills_active = None;
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(name) = &self.skills_active {
                    let name = name.clone();
                    let bundle = self.skills_bundle.clone();
                    let frontmatter = self.skills_frontmatter.clone();
                    let body = self.skills_body.clone();
                    self.status_msg = Some(format!("Saving {name}..."));
                    let _ = self.draw(term);
                    match self
                        .rpc
                        .skills_write(&bundle, &name, &frontmatter, &body)
                        .await
                    {
                        Ok(_) => {
                            self.skills_body_loaded = self.skills_body.clone();
                            self.skills_frontmatter_loaded = self.skills_frontmatter.clone();
                            self.status_msg = Some(format!("Saved {name}"));
                        }
                        Err(e) => self.status_msg = Some(format!("Save failed: {e}")),
                    }
                }
            }
            KeyCode::Enter => {
                self.skills_body.push('\n');
            }
            KeyCode::Backspace => {
                self.skills_body.pop();
            }
            KeyCode::Char(c) => {
                self.skills_body.push(c);
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
        if field_path.ends_with(".model") && field_path.starts_with("providers.models.") {
            // providers.models.<family>.<alias>.model → segment at index 2
            let segments: Vec<&str> = field_path.split('.').collect();
            if segments.len() >= 4 {
                let family = segments[2].to_string();

                // Show loading indicator before the blocking RPC call.
                self.status_msg = Some(format!("Fetching models for {family}..."));
                let _ = self.draw(term);

                match self.rpc.catalog_models(&family).await {
                    Ok(models) if !models.is_empty() => {
                        self.select_cursor =
                            models.iter().position(|m| m == &field_current).unwrap_or(0);
                        self.select_items = models;
                        self.status_msg = None;
                    }
                    Ok(_) => {
                        self.status_msg = Some("No models returned — enter manually".into());
                    }
                    Err(_) => {
                        self.status_msg = Some("Model fetch failed — enter manually".into());
                    }
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
        if self.filter.is_none() {
            if key.code == KeyCode::Char('/') {
                self.activate_filter();
                return FilterAction::Consumed;
            }
            return FilterAction::Passthrough;
        }
        match key.code {
            KeyCode::Esc => {
                self.deactivate_filter();
                FilterAction::Consumed
            }
            KeyCode::Backspace => {
                if let Some(buf) = &mut self.filter {
                    buf.pop();
                    if self.filter_cursor >= filtered_len {
                        self.filter_cursor = filtered_len.saturating_sub(1);
                    }
                }
                FilterAction::Consumed
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.filter_cursor = self.filter_cursor.saturating_sub(1);
                FilterAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.filter_cursor + 1 < filtered_len {
                    self.filter_cursor += 1;
                }
                FilterAction::Consumed
            }
            KeyCode::Char(c) => {
                if let Some(buf) = &mut self.filter {
                    buf.push(c);
                    self.filter_cursor = 0;
                }
                FilterAction::Consumed
            }
            KeyCode::Enter => FilterAction::Accept,
            _ => FilterAction::Consumed,
        }
    }

    // ── Field edit ───────────────────────────────────────────────

    async fn handle_field_edit(&mut self, key: KeyEvent) -> Result<()> {
        if self.is_select_edit() {
            return self.handle_select_edit(key).await;
        }
        match key.code {
            KeyCode::Esc => self.pop_to_field_list().await?,
            KeyCode::Enter => {
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
                            self.status_msg = Some(format!("Set {prop}"));
                            self.load_fields(&prefix).await?;
                            self.pop_to_field_list_keep_cursor().await?;
                        }
                        Err(e) => self.status_msg = Some(format!("Set failed: {e}")),
                    }
                }
            }
            KeyCode::Backspace => {
                self.edit_buf.pop();
            }
            KeyCode::Char(c) => {
                self.edit_buf.push(c);
            }
            _ => {}
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

        match key.code {
            KeyCode::Esc => {
                self.deactivate_filter();
                self.pop_to_field_list().await?;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_cursor = self.select_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.select_cursor + 1 < visible.len() {
                    self.select_cursor += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(&orig) = visible.get(self.select_cursor) {
                    return self.commit_select(orig).await;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn commit_select(&mut self, orig_idx: usize) -> Result<()> {
        if let Some(chosen) = self.select_items.get(orig_idx) {
            if let Screen::FieldEdit {
                prefix, field_idx, ..
            } = &self.screen
            {
                let prop = self.fields[*field_idx].path.clone();
                let value = serde_json::Value::String(chosen.clone());
                let prefix = prefix.clone();
                match self.rpc.config_set(&prop, value).await {
                    Ok(()) => {
                        self.status_msg = Some(format!("Set {prop}"));
                        self.load_fields(&prefix).await?;
                        self.pop_to_field_list_keep_cursor().await?;
                    }
                    Err(e) => self.status_msg = Some(format!("Set failed: {e}")),
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

    fn draw_section_list(&mut self, frame: &mut Frame, area: Rect) {
        let r = regions(area);

        render_breadcrumb(frame, r.breadcrumb, &["Config"]);

        if let Some(buf) = &self.filter {
            render_filter_bar(frame, r.help, buf);
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("ZeroClaw v{}", self.rpc.server_version),
                    theme::dim_style(),
                )),
                r.help,
            );
        }

        let labels: Vec<String> = self.sections.iter().map(|s| s.label.clone()).collect();
        let visible = self.filtered_indices(&labels);

        let items: Vec<ListItem> = visible
            .iter()
            .map(|&i| {
                let s = &self.sections[i];
                let badge = if s.completed { " ✓" } else { "" };
                ListItem::new(Line::from(Span::styled(
                    format!("{}{badge}", s.label),
                    theme::body_style(),
                )))
            })
            .collect();

        let cursor = if self.filter.is_some() {
            self.filter_cursor
        } else {
            // Map the real cursor to the visible position
            visible
                .iter()
                .position(|&i| i == self.section_cursor)
                .unwrap_or(0)
        };

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(cursor.min(items.len().saturating_sub(1))));
        }

        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Sections "))
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = None;

        let hints = if self.filter.is_some() {
            "↑↓  Enter=open  Esc=clear filter"
        } else {
            "?=help"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_type_list(&mut self, frame: &mut Frame, area: Rect, section_idx: usize) {
        let r = regions(area);
        let section = &self.sections[section_idx];

        render_breadcrumb(frame, r.breadcrumb, &["Config", &section.label]);

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

        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {} ", section.label)),
                )
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = None;

        let hints = if self.filter.is_some() {
            "↑↓  Enter=open  Esc=clear filter"
        } else {
            "?=help"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_alias_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        section_idx: usize,
        breadcrumb: &[String],
    ) {
        let r = regions(area);
        let section = &self.sections[section_idx];

        let bc: Vec<&str> = std::iter::once("Config")
            .chain(breadcrumb.iter().map(String::as_str))
            .collect();
        render_breadcrumb(frame, r.breadcrumb, &bc);

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

        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Aliases "))
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = None;

        let hints = if self.filter.is_some() {
            "↑↓  Enter=open  Esc=clear filter"
        } else {
            "?=help"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_alias_create(&mut self, frame: &mut Frame, area: Rect, breadcrumb: &[String]) {
        let r = regions(area);

        let bc: Vec<&str> = std::iter::once("Config")
            .chain(breadcrumb.iter().map(String::as_str))
            .chain(std::iter::once("New"))
            .collect();
        render_breadcrumb(frame, r.breadcrumb, &bc);

        frame.render_widget(
            Paragraph::new(Span::styled(
                "Enter a name for the new alias",
                theme::dim_style(),
            )),
            r.help,
        );

        let input_display = format!("{}{}", self.edit_buf, "█");
        let input = Paragraph::new(Line::from(Span::styled(
            input_display,
            theme::input_style(),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Alias name "));
        frame.render_widget(input, r.main);

        self.draw_footer(frame, r, "Enter=create  Esc=cancel");
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

        let bc: Vec<&str> = std::iter::once("Config")
            .chain(breadcrumb.iter().map(String::as_str))
            .collect();
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
        let tab_names: Vec<String> = tab_indices
            .iter()
            .map(|&i| {
                self.fields[i]
                    .path
                    .rsplit('.')
                    .next()
                    .unwrap_or(&self.fields[i].path)
                    .to_string()
            })
            .collect();
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

        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Fields "))
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
            r.main,
            &mut state,
        );
        self.last_main_area = r.main;
        self.last_list_offset = state.offset();
        self.last_tab_area = tab_area;

        let hints = if self.filter.is_some() {
            "↑↓  Enter=edit  Esc=clear filter"
        } else {
            "?=help"
        };
        self.draw_footer(frame, r, hints);
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
                Paragraph::new(visible_lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {filename} ")),
                ),
                r.main,
            );

            self.draw_footer(frame, r, "Ctrl+S=save  Esc=back to files");
        } else {
            // File picker mode.
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "Personality files shape your agent's voice and context.",
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
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Personality Files "),
                    )
                    .highlight_style(theme::selected_style())
                    .highlight_symbol("› "),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();

            self.draw_footer(frame, r, "?=help");
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
                Paragraph::new(visible_lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" SKILL.md — {name} ")),
                ),
                r.main,
            );

            self.draw_footer(frame, r, "Ctrl+S=save  Esc=back to skills");
        } else {
            // Skill picker mode.
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "Skills in this bundle. Enter to edit SKILL.md, x to archive.",
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
                    .block(Block::default().borders(Borders::ALL).title(" Skills "))
                    .highlight_style(theme::selected_style())
                    .highlight_symbol("› "),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();

            self.draw_footer(frame, r, "?=help");
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

        let bc: Vec<&str> = std::iter::once("Config")
            .chain(breadcrumb.iter().map(String::as_str))
            .chain(std::iter::once(short_name))
            .collect();
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
                PropKind::Enum => format!(" {short_name} (select) "),
                _ => format!(" {short_name} "),
            };

            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(theme::selected_style())
                    .highlight_symbol("› "),
                r.main,
                &mut state,
            );
            self.last_main_area = r.main;
            self.last_list_offset = state.offset();
            self.last_tab_area = None;

            let hints = if self.filter.is_some() {
                "↑↓  Enter=save  Esc=clear filter"
            } else {
                "?=help"
            };
            self.draw_footer(frame, r, hints);
        } else {
            // Text input (masked for secrets) — help text always visible.
            frame.render_widget(
                Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                r.help,
            );
            let kind_hint = if field.is_secret {
                format!("Type: {} (secret — input hidden)", field.kind.wire_name())
            } else {
                format!("Type: {}", field.kind.wire_name())
            };
            let input_display = if field.is_secret {
                format!("{}█", "•".repeat(self.edit_buf.len()))
            } else {
                format!("{}█", self.edit_buf)
            };

            let input = Paragraph::new(vec![
                Line::from(Span::styled(&kind_hint, theme::dim_style())),
                Line::from(Span::styled(input_display, theme::input_style())),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {short_name} ")),
            );

            frame.render_widget(input, r.main);

            self.draw_footer(frame, r, "Enter=save  Esc=cancel");
        }
    }

    fn draw_footer(&self, frame: &mut Frame, r: Regions, hints: &str) {
        if let Some(msg) = &self.status_msg {
            frame.render_widget(
                Paragraph::new(Span::styled(msg.as_str(), theme::warn_style())),
                r.status,
            );
        }
        frame.render_widget(
            Paragraph::new(Span::styled(hints, theme::dim_style())),
            r.hints,
        );
    }

    /// Whether the pane is in a text-input mode (filter, edit buf, alias create, editors).
    pub(crate) fn wants_text_input(&self) -> bool {
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

    /// Context-aware keybinding lines for the help modal.
    pub(crate) fn help_lines(&self) -> Vec<(&str, &str)> {
        match &self.screen {
            Screen::SectionList => {
                if self.filter.is_some() {
                    vec![
                        ("\u{2191} / \u{2193}", "Navigate"),
                        ("Enter", "Open section"),
                        ("Esc", "Clear filter"),
                        ("?", "This help"),
                    ]
                } else {
                    vec![
                        ("\u{2191}\u{2193} / j k", "Navigate"),
                        ("Enter", "Open section"),
                        ("/", "Filter"),
                        ("q", "Quit"),
                        ("?", "This help"),
                        ("", ""),
                        ("Mouse", "Click, scroll, double-click to open"),
                    ]
                }
            }
            Screen::TypeList { .. } => {
                if self.filter.is_some() {
                    vec![
                        ("\u{2191} / \u{2193}", "Navigate"),
                        ("Enter", "Open type"),
                        ("Esc", "Clear filter"),
                        ("?", "This help"),
                    ]
                } else {
                    vec![
                        ("\u{2191}\u{2193} / j k", "Navigate"),
                        ("Enter", "Open type"),
                        ("/", "Filter"),
                        ("Esc", "Back"),
                        ("?", "This help"),
                        ("", ""),
                        ("Mouse", "Click, scroll, double-click to open"),
                    ]
                }
            }
            Screen::AliasList { .. } => {
                if self.filter.is_some() {
                    vec![
                        ("\u{2191} / \u{2193}", "Navigate"),
                        ("Enter", "Open alias"),
                        ("Esc", "Clear filter"),
                        ("?", "This help"),
                    ]
                } else {
                    vec![
                        ("\u{2191}\u{2193} / j k", "Navigate"),
                        ("Enter", "Open alias"),
                        ("x", "Delete alias"),
                        ("/", "Filter"),
                        ("Esc", "Back"),
                        ("?", "This help"),
                        ("", ""),
                        ("Mouse", "Click, scroll, double-click to open"),
                    ]
                }
            }
            Screen::AliasCreate { .. } => {
                vec![
                    ("Enter", "Create alias"),
                    ("Esc", "Cancel"),
                    ("?", "This help"),
                ]
            }
            Screen::FieldList { .. } => {
                if self.filter.is_some() {
                    vec![
                        ("\u{2191} / \u{2193}", "Navigate"),
                        ("Enter", "Edit field"),
                        ("Esc", "Clear filter"),
                        ("?", "This help"),
                    ]
                } else if self.is_composite_tab() {
                    match self.tab_names.get(self.active_tab) {
                        Some(ConfigTab::Personality) => {
                            if self.personality_active_file.is_some() {
                                vec![
                                    ("Ctrl+S", "Save"),
                                    ("Esc", "Back to files"),
                                    ("?", "This help"),
                                ]
                            } else {
                                vec![
                                    ("\u{2190}\u{2192} / h l", "Switch tabs"),
                                    ("\u{2191}\u{2193} / j k", "Navigate"),
                                    ("Enter", "Edit file"),
                                    ("t", "Fill from template"),
                                    ("Esc", "Back"),
                                    ("?", "This help"),
                                    ("", ""),
                                    ("Mouse", "Click, scroll, click tabs"),
                                ]
                            }
                        }
                        Some(ConfigTab::Skills) => {
                            if self.skills_active.is_some() {
                                vec![
                                    ("Ctrl+S", "Save"),
                                    ("Esc", "Back to skills"),
                                    ("?", "This help"),
                                ]
                            } else {
                                vec![
                                    ("\u{2190}\u{2192} / h l", "Switch tabs"),
                                    ("\u{2191}\u{2193} / j k", "Navigate"),
                                    ("Enter", "Edit skill"),
                                    ("x", "Archive skill"),
                                    ("Esc", "Back"),
                                    ("?", "This help"),
                                    ("", ""),
                                    ("Mouse", "Click, scroll, click tabs"),
                                ]
                            }
                        }
                        _ => self.field_list_help(),
                    }
                } else {
                    self.field_list_help()
                }
            }
            Screen::FieldEdit { .. } => {
                if self.is_select_edit() {
                    if self.filter.is_some() {
                        vec![
                            ("\u{2191} / \u{2193}", "Navigate"),
                            ("Enter", "Save selection"),
                            ("Esc", "Clear filter"),
                            ("?", "This help"),
                        ]
                    } else {
                        vec![
                            ("\u{2191}\u{2193} / j k", "Navigate"),
                            ("Enter", "Save selection"),
                            ("/", "Filter"),
                            ("Esc", "Cancel"),
                            ("?", "This help"),
                            ("", ""),
                            ("Mouse", "Click, scroll, double-click to save"),
                        ]
                    }
                } else {
                    vec![
                        ("Enter", "Save value"),
                        ("Esc", "Cancel"),
                        ("?", "This help"),
                    ]
                }
            }
        }
    }

    fn field_list_help(&self) -> Vec<(&str, &str)> {
        let has_tabs = !self.tab_names.is_empty();
        let mut lines = Vec::new();
        if has_tabs {
            lines.push(("\u{2190}\u{2192} / h l", "Switch tabs"));
        }
        lines.push(("\u{2191}\u{2193} / j k", "Navigate"));
        lines.push(("Enter", "Edit field"));
        lines.push(("d", "Reset to default"));
        lines.push(("/", "Filter"));
        lines.push(("Esc", "Back"));
        lines.push(("?", "This help"));
        lines.push(("", ""));
        let mouse = if has_tabs {
            "Click, scroll, click tabs, double-click to edit"
        } else {
            "Click, scroll, double-click to edit"
        };
        lines.push(("Mouse", mouse));
        lines
    }
}

// ── Layout ───────────────────────────────────────────────────────

struct Regions {
    breadcrumb: Rect,
    help: Rect,
    main: Rect,
    status: Rect,
    hints: Rect,
}

fn regions(area: Rect) -> Regions {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // breadcrumb
            Constraint::Length(2), // help
            Constraint::Min(4),    // main
            Constraint::Length(1), // status
            Constraint::Length(1), // hints
        ])
        .split(area);

    Regions {
        breadcrumb: chunks[0],
        help: chunks[1],
        main: chunks[2],
        status: chunks[3],
        hints: chunks[4],
    }
}

fn render_filter_bar(frame: &mut Frame, area: Rect, buf: &str) {
    let display = format!("/{buf}█");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(display, theme::input_style()))),
        area,
    );
}

fn render_breadcrumb(frame: &mut Frame, area: Rect, segments: &[&str]) {
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
        spans.push(Span::styled(seg.to_string(), style));
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
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "vi".into()
            }
        });

    // Write content to a temp file with the right extension.
    let dir = std::env::temp_dir();
    let tmp_path = dir.join(filename_hint);
    std::fs::write(&tmp_path, content).map_err(|e| format!("tmp write: {e}"))?;

    // Suspend TUI: leave alternate screen + disable raw mode so the
    // child process gets a normal terminal.
    let _ = execute!(term.backend_mut(), LeaveAlternateScreen);
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

// ── Input ────────────────────────────────────────────────────────

pub(crate) fn wait_key() -> Result<Option<KeyEvent>> {
    loop {
        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
                }
                return Ok(Some(key));
            }
            Event::Resize(..) => return Ok(None),
            _ => continue,
        }
    }
}
