use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
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
use zeroclaw_config::traits::{ConfigFieldEntry, PropKind};

use crate::client::{ConfigSectionEntry, ConfigTemplateEntry, RpcClient};
use crate::theme;

type Term = Terminal<CrosstermBackend<Stdout>>;

pub async fn run(rpc: &RpcClient) -> Result<()> {
    let mut term = init_terminal()?;
    let result = App::new(rpc).run(&mut term).await;
    restore_terminal(&mut term)?;
    result
}

fn init_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
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

struct App<'a> {
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
}

impl<'a> App<'a> {
    fn new(rpc: &'a RpcClient) -> Self {
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
        }
    }

    async fn run(&mut self, term: &mut Term) -> Result<()> {
        self.sections = self.rpc.config_sections().await?;
        self.templates = self.rpc.config_templates().await?;

        loop {
            self.draw(term)?;

            let key = match wait_key()? {
                Some(k) => k,
                None => continue,
            };

            self.status_msg = None;

            match &self.screen {
                Screen::SectionList => {
                    if self.handle_section_list(key).await? {
                        return Ok(());
                    }
                }
                Screen::TypeList { .. } => self.handle_type_list(key).await?,
                Screen::AliasList { .. } => self.handle_alias_list(key).await?,
                Screen::AliasCreate { .. } => self.handle_alias_create(key).await?,
                Screen::FieldList { .. } => self.handle_field_list(key).await?,
                Screen::FieldEdit { .. } => self.handle_field_edit(key).await?,
            }
        }
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

    async fn handle_field_list(&mut self, key: KeyEvent) -> Result<()> {
        let field_names: Vec<String> = self
            .fields
            .iter()
            .map(|f| f.path.rsplit('.').next().unwrap_or(&f.path).to_string())
            .collect();
        let visible = self.filtered_indices(&field_names);

        match self.handle_filter_key(key, visible.len()) {
            FilterAction::Consumed => return Ok(()),
            FilterAction::Accept => {
                if let Some(&orig) = visible.get(self.filter_cursor) {
                    self.deactivate_filter();
                    self.field_cursor = orig;
                    self.enter_field_edit(orig);
                }
                return Ok(());
            }
            FilterAction::Passthrough => {}
        }

        match key.code {
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
                self.field_cursor = self.field_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.field_cursor + 1 < self.fields.len() {
                    self.field_cursor += 1;
                }
            }
            KeyCode::Enter => {
                if self.field_cursor < self.fields.len() {
                    self.enter_field_edit(self.field_cursor);
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

    fn enter_field_edit(&mut self, idx: usize) {
        self.prepare_edit_at(idx);
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
        match key.code {
            KeyCode::Esc => self.pop_to_field_list().await?,
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_cursor = self.select_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.select_cursor + 1 < self.select_items.len() {
                    self.select_cursor += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(chosen) = self.select_items.get(self.select_cursor) {
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
            }
            _ => {}
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
            match &self.screen {
                Screen::SectionList => self.draw_section_list(frame, area),
                Screen::TypeList { section_idx } => {
                    self.draw_type_list(frame, area, *section_idx);
                }
                Screen::AliasList {
                    section_idx,
                    breadcrumb,
                    ..
                } => {
                    self.draw_alias_list(frame, area, *section_idx, breadcrumb);
                }
                Screen::AliasCreate { breadcrumb, .. } => {
                    self.draw_alias_create(frame, area, breadcrumb);
                }
                Screen::FieldList {
                    section_idx,
                    breadcrumb,
                    ..
                } => {
                    self.draw_field_list(frame, area, *section_idx, breadcrumb);
                }
                Screen::FieldEdit {
                    breadcrumb,
                    field_idx,
                    ..
                } => {
                    self.draw_field_edit(frame, area, breadcrumb, *field_idx);
                }
            }
        })?;
        Ok(())
    }

    fn draw_section_list(&self, frame: &mut Frame, area: Rect) {
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

        let hints = if self.filter.is_some() {
            "↑↓=navigate  Enter=open  Esc=clear filter"
        } else {
            "↑↓/jk=navigate  Enter=open  /=filter  q=quit"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_type_list(&self, frame: &mut Frame, area: Rect, section_idx: usize) {
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

        let hints = if self.filter.is_some() {
            "↑↓=navigate  Enter=open  Esc=clear filter"
        } else {
            "↑↓/jk=navigate  Enter=open  /=filter  Esc=back"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_alias_list(
        &self,
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

        let hints = if self.filter.is_some() {
            "↑↓=navigate  Enter=open  Esc=clear filter"
        } else {
            "↑↓/jk=navigate  Enter=open  x=delete  /=filter  Esc=back"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_alias_create(&self, frame: &mut Frame, area: Rect, breadcrumb: &[String]) {
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
        &self,
        frame: &mut Frame,
        area: Rect,
        _section_idx: usize,
        breadcrumb: &[String],
    ) {
        let r = regions(area);

        let bc: Vec<&str> = std::iter::once("Config")
            .chain(breadcrumb.iter().map(String::as_str))
            .collect();
        render_breadcrumb(frame, r.breadcrumb, &bc);

        let field_names: Vec<String> = self
            .fields
            .iter()
            .map(|f| f.path.rsplit('.').next().unwrap_or(&f.path).to_string())
            .collect();
        let visible = self.filtered_indices(&field_names);

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
                let short_name = &field_names[i];
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

        let hints = if self.filter.is_some() {
            "↑↓=navigate  Enter=edit  Esc=clear filter"
        } else {
            "↑↓/jk=navigate  Enter=edit  d=reset  /=filter  Esc=back"
        };
        self.draw_footer(frame, r, hints);
    }

    fn draw_field_edit(
        &self,
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

        frame.render_widget(
            Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                .wrap(Wrap { trim: false }),
            r.help,
        );

        if self.is_select_edit() {
            // Enum or Bool select
            let items: Vec<ListItem> = self
                .select_items
                .iter()
                .map(|v| ListItem::new(Line::from(Span::styled(v.clone(), theme::body_style()))))
                .collect();

            let mut state = ListState::default();
            state.select(Some(self.select_cursor));

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

            self.draw_footer(frame, r, "↑↓/jk=navigate  Enter=save  Esc=cancel");
        } else {
            // Text input (masked for secrets)
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

// ── Input ────────────────────────────────────────────────────────

fn wait_key() -> Result<Option<KeyEvent>> {
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
