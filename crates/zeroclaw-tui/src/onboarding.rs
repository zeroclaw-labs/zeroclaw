//! `OnboardUi` implementation backed by ratatui.
//!
//! Provides the same prompt surface as the dialoguer-based `TermUi` in
//! `zeroclaw-runtime` — all flow logic lives above the trait, this is just
//! the drawing + input backend. Each trait method renders a small screen,
//! runs a synchronous crossterm event loop, and returns the value. No
//! duplication of orchestrator / section logic.

use std::io::{self, Stdout};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use zeroclaw_config::traits::{Answer, OnboardUi, SelectItem};

use crate::theme;
use crate::widgets::{BANNER_HEIGHT, Banner, InfoPanel, InputPrompt};

type Term = Terminal<CrosstermBackend<Stdout>>;

pub struct RatatuiUi {
    terminal: Term,
    log: Vec<LogLine>,
    section: Option<String>,
    subsection: Option<String>,
}

enum LogLevel {
    Note,
    Status,
    Warn,
}

struct LogLine {
    level: LogLevel,
    text: String,
}

impl RatatuiUi {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            log: Vec::new(),
            section: None,
            subsection: None,
        })
    }

    /// Suspend the alt screen + raw mode so a child process (e.g. `$EDITOR`)
    /// can take over the terminal. Caller re-enters on return.
    fn suspend(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        enable_raw_mode()?;
        execute!(self.terminal.backend_mut(), EnterAlternateScreen)?;
        self.terminal.clear()?;
        Ok(())
    }

    /// Compose the panel title from persistent section state. Owned so the
    /// caller can stash it in a local and then pass a `&str` reference to
    /// `log_panel` without conflicting borrows on `self`.
    fn panel_title(&self) -> String {
        match &self.section {
            Some(s) => format!("ZeroClaw Onboard › {s}"),
            None => "ZeroClaw Onboard".to_string(),
        }
    }
}

/// Build the rolling log panel. Free function (not `&self`) so the caller can
/// hold a shared borrow of `self.log` while `self.terminal` is borrowed
/// mutably by `draw()`.
///
/// The title is the section breadcrumb — `ZeroClaw Onboard › Providers`
/// or `ZeroClaw Onboard › Hardware › Transport` — so every prompt screen
/// tells the user which phase + sub-phase they're in. Notes / status /
/// warn lines render beneath as the body.
fn log_panel<'a>(title: &'a str, subsection: Option<&'a str>, log: &'a [LogLine]) -> InfoPanel<'a> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    if let Some(sub) = subsection {
        lines.push(Line::from(Span::styled(
            format!("› {sub}"),
            theme::accent_style(),
        )));
    }
    lines.extend(log.iter().rev().take(8).rev().map(|entry| {
        let style = match entry.level {
            LogLevel::Note => theme::dim_style(),
            LogLevel::Status => theme::body_style(),
            LogLevel::Warn => theme::warn_style(),
        };
        Line::from(Span::styled(entry.text.clone(), style))
    }));
    InfoPanel { title, lines }
}

impl Drop for RatatuiUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

/// Vertical layout: branded header + log panel (flex) + prompt bar sized to
/// the prompt text so long doc-comment labels don't get truncated. The
/// banner collapses when the terminal is too short to leave room for it.
fn split(area: Rect, prompt_text: &str) -> (Rect, Rect, Rect) {
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let wrapped_rows = prompt_text
        .split('\n')
        .map(|line| line.len().div_ceil(inner_width).max(1))
        .sum::<usize>();
    let bottom_rows = (wrapped_rows + 2).clamp(3, (area.height / 2) as usize) as u16;
    let banner_rows = if area.height >= BANNER_HEIGHT + bottom_rows + 3 {
        BANNER_HEIGHT
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_rows),
            Constraint::Min(3),
            Constraint::Length(bottom_rows),
        ])
        .split(area);
    (chunks[0], chunks[1], chunks[2])
}

fn wait_key() -> Result<KeyEvent> {
    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                anyhow::bail!("aborted by user (Ctrl+C)");
            }
            return Ok(key);
        }
    }
}

#[async_trait]
impl OnboardUi for RatatuiUi {
    async fn confirm(&mut self, prompt: &str, default: bool) -> Result<Answer<bool>> {
        let mut choice = default;
        loop {
            let title = self.panel_title();
            let log = log_panel(&title, self.subsection.as_deref(), &self.log);
            let label = format!(
                "{prompt}  [{}] (y/n, Enter confirms, Esc=back)",
                if choice { "Yes" } else { "No" }
            );
            self.terminal.draw(|frame| {
                let (header, top, bottom) = split(frame.area(), &label);
                if header.height > 0 {
                    frame.render_widget(Banner, header);
                }
                frame.render_widget(log, top);
                frame.render_widget(
                    Paragraph::new(label.clone())
                        .style(theme::body_style())
                        .wrap(Wrap { trim: false })
                        .block(Block::default().borders(Borders::ALL)),
                    bottom,
                );
            })?;
            match wait_key()?.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(Answer::Value(true)),
                KeyCode::Char('n') | KeyCode::Char('N') => return Ok(Answer::Value(false)),
                KeyCode::Enter => return Ok(Answer::Value(choice)),
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => choice = !choice,
                KeyCode::Esc => return Ok(Answer::Back),
                _ => {}
            }
        }
    }

    async fn string(&mut self, prompt: &str, current: Option<&str>) -> Result<Answer<String>> {
        let mut buffer = current.unwrap_or_default().to_string();
        loop {
            let title = self.panel_title();
            let log = log_panel(&title, self.subsection.as_deref(), &self.log);
            let label = prompt.to_string();
            let input = buffer.clone();
            let measure = format!("{label}  {input}");
            self.terminal.draw(|frame| {
                let (header, top, bottom) = split(frame.area(), &measure);
                if header.height > 0 {
                    frame.render_widget(Banner, header);
                }
                frame.render_widget(log, top);
                frame.render_widget(
                    InputPrompt {
                        label: &label,
                        input: &input,
                        masked: false,
                    },
                    bottom,
                );
            })?;
            match wait_key()? {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => return Ok(Answer::Value(buffer)),
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => return Ok(Answer::Back),
                KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                } => {
                    buffer.pop();
                }
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => buffer.push(c),
                _ => {}
            }
        }
    }

    async fn secret(&mut self, prompt: &str, has_current: bool) -> Result<Answer<Option<String>>> {
        if has_current {
            match self
                .confirm(&format!("{prompt} (stored, replace?)"), false)
                .await?
            {
                Answer::Value(false) => return Ok(Answer::Value(None)),
                Answer::Back => return Ok(Answer::Back),
                Answer::Value(true) => {}
            }
        }
        let mut buffer = String::new();
        loop {
            let title = self.panel_title();
            let log = log_panel(&title, self.subsection.as_deref(), &self.log);
            let label = prompt.to_string();
            let input = buffer.clone();
            let measure = label.clone();
            self.terminal.draw(|frame| {
                let (header, top, bottom) = split(frame.area(), &measure);
                if header.height > 0 {
                    frame.render_widget(Banner, header);
                }
                frame.render_widget(log, top);
                frame.render_widget(
                    InputPrompt {
                        label: &label,
                        input: &input,
                        masked: true,
                    },
                    bottom,
                );
            })?;
            match wait_key()? {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => return Ok(Answer::Value(Some(buffer))),
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => return Ok(Answer::Back),
                KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                } => {
                    buffer.pop();
                }
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => buffer.push(c),
                _ => {}
            }
        }
    }

    async fn select(
        &mut self,
        prompt: &str,
        items: &[SelectItem],
        current: Option<usize>,
    ) -> Result<Answer<usize>> {
        if items.is_empty() {
            return Err(anyhow!("no items to choose from"));
        }
        let mut filter = String::new();
        let mut cursor = current.unwrap_or(0).min(items.len() - 1);

        loop {
            let matches: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    filter.is_empty()
                        || item
                            .label
                            .to_ascii_lowercase()
                            .contains(&filter.to_ascii_lowercase())
                })
                .map(|(i, _)| i)
                .collect();
            if cursor >= matches.len() && !matches.is_empty() {
                cursor = matches.len() - 1;
            }

            let title = self.panel_title();
            let log = log_panel(&title, self.subsection.as_deref(), &self.log);
            let prompt_text = prompt.to_string();
            let filter_text = filter.clone();
            let list_items: Vec<ListItem<'_>> = matches
                .iter()
                .map(|&real_idx| {
                    let item = &items[real_idx];
                    let mut spans = vec![Span::styled(item.label.clone(), theme::body_style())];
                    if let Some(badge) = &item.badge {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            badge.clone(),
                            theme::dim_style().add_modifier(Modifier::ITALIC),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let mut list_state = ListState::default();
            if !matches.is_empty() {
                list_state.select(Some(cursor));
            }

            self.terminal.draw(|frame| {
                let area = frame.area();
                let banner_rows = if area.height >= BANNER_HEIGHT + 13 {
                    BANNER_HEIGHT
                } else {
                    0
                };
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(banner_rows),
                        Constraint::Length(8),
                        Constraint::Length(1),
                        Constraint::Min(3),
                        Constraint::Length(1),
                    ])
                    .split(area);
                if banner_rows > 0 {
                    frame.render_widget(Banner, chunks[0]);
                }
                frame.render_widget(log, chunks[1]);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(prompt_text, theme::heading_style()),
                        Span::raw(" "),
                        Span::styled(format!("filter: {filter_text}"), theme::dim_style()),
                    ])),
                    chunks[2],
                );
                // ratatui's List + ListState handles scrolling + highlight
                // automatically — the visible window follows the cursor so
                // long lists (e.g., 28 channels + Done) stay reachable.
                frame.render_stateful_widget(
                    List::new(list_items)
                        .block(Block::default().borders(Borders::ALL))
                        .highlight_style(theme::selected_style())
                        .highlight_symbol("› "),
                    chunks[3],
                    &mut list_state,
                );
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "type to filter  Enter=select  Esc=back",
                        theme::dim_style(),
                    )),
                    chunks[4],
                );
            })?;

            match wait_key()? {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    if let Some(&real) = matches.get(cursor) {
                        return Ok(Answer::Value(real));
                    }
                }
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => return Ok(Answer::Back),
                KeyEvent {
                    code: KeyCode::Up, ..
                } => cursor = cursor.saturating_sub(1),
                KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => {
                    if cursor + 1 < matches.len() {
                        cursor += 1;
                    }
                }
                KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                } => {
                    filter.pop();
                    cursor = 0;
                }
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => {
                    filter.push(c);
                    cursor = 0;
                }
                _ => {}
            }
        }
    }

    async fn editor(&mut self, hint: &str, initial: &str) -> Result<Answer<String>> {
        // Suspend ratatui and hand the terminal to $EDITOR. Inlined rather
        // than pulling in dialoguer just for the launcher — it's ~15 lines
        // of std::process + std::fs, no extra dep footprint.
        self.suspend()?;
        let path =
            std::env::temp_dir().join(format!("zeroclaw-onboard-{}.txt", std::process::id()));
        std::fs::write(&path, initial)?;
        if !hint.is_empty() {
            println!("  {hint}");
        }
        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| {
                if cfg!(windows) {
                    "notepad".into()
                } else {
                    "vi".into()
                }
            });
        let status = std::process::Command::new(&editor).arg(&path).status()?;
        let edited = if status.success() {
            std::fs::read_to_string(&path).unwrap_or_else(|_| initial.to_string())
        } else {
            initial.to_string()
        };
        let _ = std::fs::remove_file(&path);
        self.resume()?;
        Ok(Answer::Value(edited))
    }

    fn heading(&mut self, level: u8, text: &str) {
        // level 1 = section (persists in title bar). Entering a new section
        // clears any stale subsection and log lines. level 2 = subsection
        // (renders as the first line inside the panel until replaced).
        match level {
            1 => {
                self.section = Some(text.to_string());
                self.subsection = None;
                self.log.clear();
            }
            _ => {
                self.subsection = Some(text.to_string());
            }
        }
    }

    fn note(&mut self, msg: &str) {
        // Note = "current context for the next prompt". Replace (don't
        // append) so stale context from a previous section doesn't leak
        // into a later screen. Section / subsection headings live outside
        // the log and are untouched here.
        self.log.clear();
        if !msg.is_empty() {
            self.log.push(LogLine {
                level: LogLevel::Note,
                text: msg.to_string(),
            });
        }
    }

    fn status(&mut self, msg: &str) {
        self.log.push(LogLine {
            level: LogLevel::Status,
            text: msg.to_string(),
        });
    }

    fn warn(&mut self, msg: &str) {
        self.log.push(LogLine {
            level: LogLevel::Warn,
            text: msg.to_string(),
        });
    }
}
