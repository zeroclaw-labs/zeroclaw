//! Interactive config manager — the TUI MVP.
//!
//! Server-driven: sections, fields, and metadata all come from RPC
//! responses. The TUI is a renderer that maps `PropKind` to widget type.

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
use zeroclaw_config::traits::ConfigFieldEntry;

use crate::client::{OnboardSectionEntry, RpcClient};
use crate::theme;
use crate::widgets::{BANNER_HEIGHT, Banner};

type Term = Terminal<CrosstermBackend<Stdout>>;

// ── Top-level entry point ────────────────────────────────────────

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

// ── Screen enum ──────────────────────────────────────────────────

enum Screen {
    SectionList,
    FieldList {
        section_idx: usize,
    },
    FieldEdit {
        section_idx: usize,
        field_idx: usize,
    },
}

// ── App state ────────────────────────────────────────────────────

struct App<'a> {
    rpc: &'a RpcClient,
    screen: Screen,
    sections: Vec<OnboardSectionEntry>,
    section_cursor: usize,
    fields: Vec<ConfigFieldEntry>,
    field_cursor: usize,
    edit_buf: String,
    status_msg: Option<String>,
}

impl<'a> App<'a> {
    fn new(rpc: &'a RpcClient) -> Self {
        Self {
            rpc,
            screen: Screen::SectionList,
            sections: Vec::new(),
            section_cursor: 0,
            fields: Vec::new(),
            field_cursor: 0,
            edit_buf: String::new(),
            status_msg: None,
        }
    }

    async fn run(&mut self, term: &mut Term) -> Result<()> {
        self.load_sections().await?;

        loop {
            self.draw(term)?;

            let key = match wait_key()? {
                Some(k) => k,
                None => continue,
            };

            match &self.screen {
                Screen::SectionList => {
                    if self.handle_section_list_key(key).await? {
                        return Ok(());
                    }
                }
                Screen::FieldList { .. } => {
                    self.handle_field_list_key(key).await?;
                }
                Screen::FieldEdit { .. } => {
                    self.handle_field_edit_key(key).await?;
                }
            }
        }
    }

    // ── Data loading ─────────────────────────────────────────────

    async fn load_sections(&mut self) -> Result<()> {
        self.sections = self.rpc.onboard_sections().await?;
        Ok(())
    }

    async fn load_fields_for_section(&mut self, section_idx: usize) -> Result<()> {
        let section = &self.sections[section_idx];
        self.fields = self.rpc.config_list(Some(&section.key)).await?;
        self.field_cursor = 0;
        Ok(())
    }

    // ── Key handling ─────────────────────────────────────────────

    /// Returns `true` if the app should exit.
    async fn handle_section_list_key(&mut self, key: KeyEvent) -> Result<bool> {
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
                if !self.sections.is_empty() {
                    let idx = self.section_cursor;
                    self.load_fields_for_section(idx).await?;
                    self.screen = Screen::FieldList { section_idx: idx };
                }
            }
            _ => {}
        }
        Ok(false)
    }

    async fn handle_field_list_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::SectionList;
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
                if !self.fields.is_empty() {
                    let field = &self.fields[self.field_cursor];
                    if field.is_secret {
                        self.status_msg = Some("Secret fields cannot be edited in the TUI".into());
                    } else {
                        self.edit_buf = field
                            .value
                            .as_ref()
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Screen::FieldList { section_idx } = self.screen {
                            self.screen = Screen::FieldEdit {
                                section_idx,
                                field_idx: self.field_cursor,
                            };
                        }
                    }
                }
            }
            KeyCode::Char('d') => {
                if !self.fields.is_empty() {
                    let prop = self.fields[self.field_cursor].path.clone();
                    match self.rpc.config_delete(&prop).await {
                        Ok(()) => {
                            self.status_msg = Some(format!("Reset {prop}"));
                            if let Screen::FieldList { section_idx } = self.screen {
                                self.load_fields_for_section(section_idx).await?;
                            }
                        }
                        Err(e) => self.status_msg = Some(format!("Delete failed: {e}")),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_field_edit_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                if let Screen::FieldEdit { section_idx, .. } = self.screen {
                    self.screen = Screen::FieldList { section_idx };
                }
            }
            KeyCode::Enter => {
                if let Screen::FieldEdit {
                    section_idx,
                    field_idx,
                } = self.screen
                {
                    let prop = self.fields[field_idx].path.clone();
                    let value = serde_json::Value::String(self.edit_buf.clone());
                    match self.rpc.config_set(&prop, value).await {
                        Ok(()) => {
                            self.status_msg = Some(format!("Set {prop}"));
                            self.load_fields_for_section(section_idx).await?;
                            self.screen = Screen::FieldList { section_idx };
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

    // ── Drawing ──────────────────────────────────────────────────

    fn draw(&mut self, term: &mut Term) -> Result<()> {
        term.draw(|frame| {
            let area = frame.area();
            match &self.screen {
                Screen::SectionList => self.draw_section_list(frame, area),
                Screen::FieldList { section_idx } => {
                    let idx = *section_idx;
                    self.draw_field_list(frame, area, idx);
                }
                Screen::FieldEdit {
                    section_idx,
                    field_idx,
                } => {
                    let si = *section_idx;
                    let fi = *field_idx;
                    self.draw_field_edit(frame, area, si, fi);
                }
            }
        })?;
        Ok(())
    }

    fn draw_section_list(&self, frame: &mut Frame, area: Rect) {
        let chunks = main_layout(area);

        if chunks.banner.height > 0 {
            frame.render_widget(Banner, chunks.banner);
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("ZeroClaw Config Manager", theme::heading_style()),
                Span::styled(
                    format!("  (v{})", self.rpc.server_version),
                    theme::dim_style(),
                ),
            ])),
            chunks.breadcrumb,
        );

        let items: Vec<ListItem> = self
            .sections
            .iter()
            .map(|s| {
                let badge = if s.completed { " ✓" } else { "" };
                let label = format!("{}{}", s.label, badge,);
                ListItem::new(Line::from(Span::styled(label, theme::body_style())))
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(self.section_cursor));

        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Sections "))
                .highlight_style(theme::selected_style())
                .highlight_symbol("› "),
            chunks.main,
            &mut state,
        );

        let hints = "↑↓/jk=navigate  Enter=open  q=quit";
        self.draw_status_and_hints(frame, chunks.status, chunks.hints, hints);
    }

    fn draw_field_list(&self, frame: &mut Frame, area: Rect, section_idx: usize) {
        let chunks = main_layout(area);
        let section = &self.sections[section_idx];

        if chunks.banner.height > 0 {
            frame.render_widget(Banner, chunks.banner);
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Config", theme::heading_style()),
                Span::styled("  ›  ", theme::dim_style()),
                Span::styled(&section.label, theme::accent_style()),
            ])),
            chunks.breadcrumb,
        );

        let items: Vec<ListItem> = self
            .fields
            .iter()
            .map(|f| {
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
                let label = format!("{} = {}{}", f.path, val_display, env_marker);
                ListItem::new(Line::from(Span::styled(label, theme::body_style())))
            })
            .collect();

        let mut state = ListState::default();
        if !self.fields.is_empty() {
            state.select(Some(self.field_cursor));
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
            chunks.main,
            &mut state,
        );

        // Show field description in help area.
        if let Some(field) = self.fields.get(self.field_cursor) {
            frame.render_widget(
                Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                    .wrap(Wrap { trim: false }),
                chunks.help,
            );
        }

        let hints = "↑↓/jk=navigate  Enter=edit  d=reset  Esc=back";
        self.draw_status_and_hints(frame, chunks.status, chunks.hints, hints);
    }

    fn draw_field_edit(&self, frame: &mut Frame, area: Rect, section_idx: usize, field_idx: usize) {
        let chunks = main_layout(area);
        let section = &self.sections[section_idx];
        let field = &self.fields[field_idx];

        if chunks.banner.height > 0 {
            frame.render_widget(Banner, chunks.banner);
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Config", theme::heading_style()),
                Span::styled("  ›  ", theme::dim_style()),
                Span::styled(&section.label, theme::accent_style()),
                Span::styled("  ›  ", theme::dim_style()),
                Span::styled(
                    &field.path,
                    theme::accent_style().add_modifier(Modifier::BOLD),
                ),
            ])),
            chunks.breadcrumb,
        );

        frame.render_widget(
            Paragraph::new(Span::styled(&field.description, theme::dim_style()))
                .wrap(Wrap { trim: false }),
            chunks.help,
        );

        let kind_hint = format!("Type: {}", field.kind.wire_name());
        let input_display = format!("{}{}", self.edit_buf, "█");

        let input_block = Paragraph::new(vec![
            Line::from(Span::styled(&kind_hint, theme::dim_style())),
            Line::from(Span::styled(input_display, theme::input_style())),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", field.path)),
        );

        frame.render_widget(input_block, chunks.main);

        let hints = "Enter=save  Esc=cancel";
        self.draw_status_and_hints(frame, chunks.status, chunks.hints, hints);
    }

    fn draw_status_and_hints(
        &self,
        frame: &mut Frame,
        status_area: Rect,
        hints_area: Rect,
        hints: &str,
    ) {
        if let Some(msg) = &self.status_msg {
            frame.render_widget(
                Paragraph::new(Span::styled(msg.as_str(), theme::warn_style())),
                status_area,
            );
        }
        frame.render_widget(
            Paragraph::new(Span::styled(hints, theme::dim_style())),
            hints_area,
        );
    }
}

// ── Layout ───────────────────────────────────────────────────────

struct Regions {
    banner: Rect,
    breadcrumb: Rect,
    help: Rect,
    main: Rect,
    status: Rect,
    hints: Rect,
}

fn main_layout(area: Rect) -> Regions {
    let banner_rows = if area.height >= BANNER_HEIGHT + 10 {
        BANNER_HEIGHT
    } else {
        0
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_rows), // banner
            Constraint::Length(1),           // breadcrumb
            Constraint::Length(2),           // help
            Constraint::Min(4),              // main list/editor
            Constraint::Length(1),           // status
            Constraint::Length(1),           // hints
        ])
        .split(area);

    Regions {
        banner: chunks[0],
        breadcrumb: chunks[1],
        help: chunks[2],
        main: chunks[3],
        status: chunks[4],
        hints: chunks[5],
    }
}

// ── Input ────────────────────────────────────────────────────────

fn wait_key() -> Result<Option<KeyEvent>> {
    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            }
            return Ok(Some(key));
        }
        // Resize events trigger a redraw automatically via the next loop iteration.
    }
}
