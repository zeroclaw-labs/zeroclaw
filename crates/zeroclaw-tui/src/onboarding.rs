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
    widgets::{Block, Borders, Paragraph, Wrap},
};
use zeroclaw_config::traits::{OnboardUi, SelectItem};

use crate::theme;
use crate::widgets::{InfoPanel, InputPrompt};

type Term = Terminal<CrosstermBackend<Stdout>>;

pub struct RatatuiUi {
    terminal: Term,
    log: Vec<LogLine>,
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

    fn log_panel(&self) -> InfoPanel<'_> {
        let lines: Vec<Line<'_>> = self
            .log
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|entry| {
                let style = match entry.level {
                    LogLevel::Note => theme::dim_style(),
                    LogLevel::Status => theme::body_style(),
                    LogLevel::Warn => theme::warn_style(),
                };
                Line::from(Span::styled(entry.text.clone(), style))
            })
            .collect();
        InfoPanel {
            title: "ZeroClaw Onboard",
            lines,
        }
    }
}

impl Drop for RatatuiUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

fn split(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);
    (chunks[0], chunks[1])
}

fn wait_key() -> Result<KeyEvent> {
    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    anyhow::bail!("aborted by user (Ctrl+C)");
                }
                return Ok(key);
            }
        }
    }
}

#[async_trait]
impl OnboardUi for RatatuiUi {
    async fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        let mut choice = default;
        loop {
            let log = self.log_panel();
            let prompt = prompt.to_string();
            self.terminal.draw(|frame| {
                let (top, bottom) = split(frame.area());
                frame.render_widget(log, top);
                let label = format!(
                    "{prompt}  [{}] (y/n, Enter confirms)",
                    if choice { "Yes" } else { "No" }
                );
                frame.render_widget(
                    Paragraph::new(label)
                        .style(theme::body_style())
                        .block(Block::default().borders(Borders::ALL)),
                    bottom,
                );
            })?;
            match wait_key()?.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(true),
                KeyCode::Char('n') | KeyCode::Char('N') => return Ok(false),
                KeyCode::Enter => return Ok(choice),
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => choice = !choice,
                KeyCode::Esc => anyhow::bail!("aborted by user"),
                _ => {}
            }
        }
    }

    async fn string(&mut self, prompt: &str, current: Option<&str>) -> Result<String> {
        let mut buffer = current.unwrap_or_default().to_string();
        loop {
            let log = self.log_panel();
            let label = prompt.to_string();
            let input = buffer.clone();
            self.terminal.draw(|frame| {
                let (top, bottom) = split(frame.area());
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
                } => return Ok(buffer),
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => anyhow::bail!("aborted by user"),
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

    async fn secret(&mut self, prompt: &str, has_current: bool) -> Result<Option<String>> {
        if has_current {
            let replace = self
                .confirm(&format!("{prompt} (stored, replace?)"), false)
                .await?;
            if !replace {
                return Ok(None);
            }
        }
        let mut buffer = String::new();
        loop {
            let log = self.log_panel();
            let label = prompt.to_string();
            let input = buffer.clone();
            self.terminal.draw(|frame| {
                let (top, bottom) = split(frame.area());
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
                } => return Ok(Some(buffer)),
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => anyhow::bail!("aborted by user"),
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
    ) -> Result<usize> {
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

            let log = self.log_panel();
            let prompt_text = prompt.to_string();
            let filter_text = filter.clone();
            let visible: Vec<(String, bool, Option<String>)> = matches
                .iter()
                .enumerate()
                .map(|(display_idx, &real_idx)| {
                    (
                        items[real_idx].label.clone(),
                        display_idx == cursor,
                        items[real_idx].badge.clone(),
                    )
                })
                .collect();

            self.terminal.draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(8),
                        Constraint::Length(1),
                        Constraint::Min(3),
                        Constraint::Length(1),
                    ])
                    .split(frame.area());
                frame.render_widget(log, chunks[0]);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(prompt_text, theme::heading_style()),
                        Span::raw(" "),
                        Span::styled(format!("filter: {filter_text}"), theme::dim_style()),
                    ])),
                    chunks[1],
                );
                let lines: Vec<Line<'_>> = visible
                    .iter()
                    .map(|(label, selected, badge)| {
                        let style = if *selected {
                            theme::selected_style()
                        } else {
                            theme::body_style()
                        };
                        let mut spans = vec![Span::styled(
                            if *selected { "› " } else { "  " },
                            style,
                        )];
                        spans.push(Span::styled(label.clone(), style));
                        if let Some(b) = badge {
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(
                                b.clone(),
                                theme::dim_style().add_modifier(Modifier::ITALIC),
                            ));
                        }
                        Line::from(spans)
                    })
                    .collect();
                frame.render_widget(
                    Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .block(Block::default().borders(Borders::ALL)),
                    chunks[2],
                );
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "type to filter  Enter=select  Esc=cancel",
                        theme::dim_style(),
                    )),
                    chunks[3],
                );
            })?;

            match wait_key()? {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    if let Some(&real) = matches.get(cursor) {
                        return Ok(real);
                    }
                }
                KeyEvent {
                    code: KeyCode::Esc, ..
                } => anyhow::bail!("aborted by user"),
                KeyEvent {
                    code: KeyCode::Up, ..
                } => {
                    if cursor > 0 {
                        cursor -= 1;
                    }
                }
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

    async fn editor(&mut self, hint: &str, initial: &str) -> Result<String> {
        // Suspend ratatui and hand the terminal to $EDITOR. Inlined rather
        // than pulling in dialoguer just for the launcher — it's ~15 lines
        // of std::process + std::fs, no extra dep footprint.
        self.suspend()?;
        let path = std::env::temp_dir().join(format!(
            "zeroclaw-onboard-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, initial)?;
        if !hint.is_empty() {
            println!("  {hint}");
        }
        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| if cfg!(windows) { "notepad".into() } else { "vi".into() });
        let status = std::process::Command::new(&editor).arg(&path).status()?;
        let edited = if status.success() {
            std::fs::read_to_string(&path).unwrap_or_else(|_| initial.to_string())
        } else {
            initial.to_string()
        };
        let _ = std::fs::remove_file(&path);
        self.resume()?;
        Ok(edited)
    }

    fn note(&mut self, msg: &str) {
        self.log.push(LogLine {
            level: LogLevel::Note,
            text: msg.to_string(),
        });
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
