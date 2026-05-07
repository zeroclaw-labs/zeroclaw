//! `OnboardUi` implementation backed by ratatui.
//!
//! Six-region layout — banner, breadcrumb, status log, help text, input,
//! nav hints — keeps everything decision-relevant clustered around the
//! input at the bottom while the rolling log stays as background context.
//! All flow logic lives above the trait; this file is just drawing + input.

use std::io::{self, Stdout};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
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
use zeroclaw_config::traits::{Answer, OnboardUi, SelectItem};

use crate::theme;
use crate::widgets::{BANNER_HEIGHT, Banner, InputPrompt};

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Nav-hint text rendered at the bottom of the screen, specialized per
/// prompt type. The user sees these keys on every prompt without having
/// to remember what the current screen accepts.
const HINTS_CONFIRM: &str = "y=yes  n=no  ← →=toggle  Enter=confirm  Esc=back";
const HINTS_STRING: &str = "Enter=accept  Esc=back";
const HINTS_SECRET: &str = "Enter=save  Esc=back  (input hidden)";
const HINTS_SELECT: &str = "↑↓=navigate  type=filter  Enter=select  Esc=back";

pub struct RatatuiUi {
    terminal: Term,
    /// Current section (e.g. "Providers"). Rendered in the breadcrumb bar
    /// until replaced by `heading(1, _)`.
    section: Option<String>,
    /// Current subsection (e.g. "Anthropic"). Rendered after the section
    /// in the breadcrumb; cleared by `heading(1, _)`.
    subsection: Option<String>,
    /// Help text for the current prompt (field docstring). Ephemeral —
    /// replaced by each `note(_)` call. Renders directly above the input.
    help: Option<String>,
    /// Rolling status / warning log. Renders between breadcrumb and help
    /// as background context. Cleared on section entry.
    log: Vec<LogLine>,
}

enum LogLevel {
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
            section: None,
            subsection: None,
            help: None,
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
}

impl Drop for RatatuiUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

/// Layout regions returned by `layout()`. Any region may have height 0
/// when its contents are empty / space is tight.
struct Regions {
    banner: Rect,
    breadcrumb: Rect,
    log: Rect,
    help: Rect,
    input: Rect,
    hints: Rect,
}

/// Compute the six-region layout. `help_text_rows` and `input_rows` are
/// driven by wrapped content length so long docstrings / multi-line
/// prompts don't get truncated; log gets the remaining flex space.
fn layout(area: Rect, help_text: Option<&str>, input_rows: u16) -> Regions {
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let help_rows: u16 = help_text
        .map(|s| {
            s.split('\n')
                .map(|line| line.len().div_ceil(inner_width).max(1))
                .sum::<usize>()
                .min(6) as u16
        })
        .unwrap_or(0);

    // Fixed overhead: breadcrumb (1) + input + hints (1) + help (0..=6)
    let fixed_below_banner = 1 + input_rows + 1 + help_rows;
    let banner_rows = if area.height >= BANNER_HEIGHT + fixed_below_banner + 3 {
        BANNER_HEIGHT
    } else {
        0
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_rows),
            Constraint::Length(1),          // breadcrumb
            Constraint::Min(1),             // log (flex)
            Constraint::Length(help_rows),  // help (0..=6)
            Constraint::Length(input_rows), // input
            Constraint::Length(1),          // hints
        ])
        .split(area);
    Regions {
        banner: chunks[0],
        breadcrumb: chunks[1],
        log: chunks[2],
        help: chunks[3],
        input: chunks[4],
        hints: chunks[5],
    }
}

fn render_banner(frame: &mut Frame, area: Rect) {
    if area.height > 0 {
        frame.render_widget(Banner, area);
    }
}

fn render_breadcrumb(
    frame: &mut Frame,
    area: Rect,
    section: Option<&str>,
    subsection: Option<&str>,
) {
    let mut spans: Vec<Span<'_>> = vec![Span::styled("ZeroClaw Onboard", theme::heading_style())];
    if let Some(s) = section {
        spans.push(Span::styled("  ›  ", theme::dim_style()));
        spans.push(Span::styled(s.to_string(), theme::accent_style()));
    }
    if let Some(sub) = subsection {
        spans.push(Span::styled("  ›  ", theme::dim_style()));
        spans.push(Span::styled(
            sub.to_string(),
            theme::accent_style().add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_log(frame: &mut Frame, area: Rect, log: &[LogLine]) {
    let lines: Vec<Line<'_>> = log
        .iter()
        .rev()
        .take(area.height as usize)
        .rev()
        .map(|entry| {
            let style = match entry.level {
                LogLevel::Status => theme::body_style(),
                LogLevel::Warn => theme::warn_style(),
            };
            Line::from(Span::styled(entry.text.clone(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_help(frame: &mut Frame, area: Rect, help: Option<&str>) {
    if area.height == 0 {
        return;
    }
    let text = help.unwrap_or("");
    // Split explicit `\n` separators into distinct Lines. A single
    // `Span` containing `\n` does not produce a visual break under
    // `Paragraph::wrap`, which is how the description + "Current: …"
    // suffix were fusing together mid-sentence.
    let lines: Vec<Line<'_>> = text
        .split('\n')
        .map(|line| Line::from(Span::styled(line.to_string(), theme::dim_style())))
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_hints(frame: &mut Frame, area: Rect, hints: &str) {
    frame.render_widget(
        Paragraph::new(Span::styled(hints, theme::dim_style())),
        area,
    );
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
            let section = self.section.clone();
            let subsection = self.subsection.clone();
            let help = self.help.clone();
            let log_snapshot: Vec<(LogLevel, String)> = self
                .log
                .iter()
                .map(|l| {
                    (
                        match l.level {
                            LogLevel::Status => LogLevel::Status,
                            LogLevel::Warn => LogLevel::Warn,
                        },
                        l.text.clone(),
                    )
                })
                .collect();
            let label = format!("◆ {prompt}  [{}]", if choice { "Yes" } else { "No" });
            self.terminal.draw(|frame| {
                let r = layout(frame.area(), help.as_deref(), 3);
                render_banner(frame, r.banner);
                render_breadcrumb(
                    frame,
                    r.breadcrumb,
                    section.as_deref(),
                    subsection.as_deref(),
                );
                let log_lines: Vec<LogLine> = log_snapshot
                    .iter()
                    .map(|(lvl, txt)| LogLine {
                        level: match lvl {
                            LogLevel::Status => LogLevel::Status,
                            LogLevel::Warn => LogLevel::Warn,
                        },
                        text: txt.clone(),
                    })
                    .collect();
                render_log(frame, r.log, &log_lines);
                render_help(frame, r.help, help.as_deref());
                frame.render_widget(
                    Paragraph::new(label.clone())
                        .style(theme::body_style())
                        .wrap(Wrap { trim: false })
                        .block(Block::default().borders(Borders::ALL)),
                    r.input,
                );
                render_hints(frame, r.hints, HINTS_CONFIRM);
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
            let section = self.section.clone();
            let subsection = self.subsection.clone();
            let help = self.help.clone();
            let log_lines: Vec<LogLine> = self
                .log
                .iter()
                .map(|l| LogLine {
                    level: match l.level {
                        LogLevel::Status => LogLevel::Status,
                        LogLevel::Warn => LogLevel::Warn,
                    },
                    text: l.text.clone(),
                })
                .collect();
            let input = buffer.clone();
            self.terminal.draw(|frame| {
                let r = layout(frame.area(), help.as_deref(), 3);
                render_banner(frame, r.banner);
                render_breadcrumb(
                    frame,
                    r.breadcrumb,
                    section.as_deref(),
                    subsection.as_deref(),
                );
                render_log(frame, r.log, &log_lines);
                render_help(frame, r.help, help.as_deref());
                frame.render_widget(
                    InputPrompt {
                        label: prompt,
                        input: &input,
                        masked: false,
                    },
                    r.input,
                );
                render_hints(frame, r.hints, HINTS_STRING);
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
                .confirm(&format!("{prompt} (already stored — replace?)"), false)
                .await?
            {
                Answer::Value(false) => return Ok(Answer::Value(None)),
                Answer::Back => return Ok(Answer::Back),
                Answer::Value(true) => {}
            }
        }
        let mut buffer = String::new();
        loop {
            let section = self.section.clone();
            let subsection = self.subsection.clone();
            let help = self.help.clone();
            let log_lines: Vec<LogLine> = self
                .log
                .iter()
                .map(|l| LogLine {
                    level: match l.level {
                        LogLevel::Status => LogLevel::Status,
                        LogLevel::Warn => LogLevel::Warn,
                    },
                    text: l.text.clone(),
                })
                .collect();
            let input = buffer.clone();
            self.terminal.draw(|frame| {
                let r = layout(frame.area(), help.as_deref(), 3);
                render_banner(frame, r.banner);
                render_breadcrumb(
                    frame,
                    r.breadcrumb,
                    section.as_deref(),
                    subsection.as_deref(),
                );
                render_log(frame, r.log, &log_lines);
                render_help(frame, r.help, help.as_deref());
                frame.render_widget(
                    InputPrompt {
                        label: prompt,
                        input: &input,
                        masked: true,
                    },
                    r.input,
                );
                render_hints(frame, r.hints, HINTS_SECRET);
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

            let section = self.section.clone();
            let subsection = self.subsection.clone();
            let help = self.help.clone();
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
            let prompt_line = format!("◆ {prompt}  filter: {filter}");

            self.terminal.draw(|frame| {
                let area = frame.area();
                let inner_width = area.width.saturating_sub(2).max(1) as usize;
                let help_rows: u16 = help
                    .as_deref()
                    .map(|s| {
                        s.split('\n')
                            .map(|line| line.len().div_ceil(inner_width).max(1))
                            .sum::<usize>()
                            .min(6) as u16
                    })
                    .unwrap_or(0);
                let fixed = 1 + help_rows + 1 + 1; // breadcrumb + help + prompt + hints
                let banner_rows = if area.height >= BANNER_HEIGHT + fixed + 6 {
                    BANNER_HEIGHT
                } else {
                    0
                };
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(banner_rows),
                        Constraint::Length(1),         // breadcrumb
                        Constraint::Length(help_rows), // help
                        Constraint::Length(1),         // prompt + filter line
                        Constraint::Min(4),            // list (flex)
                        Constraint::Length(1),         // hints
                    ])
                    .split(area);
                render_banner(frame, chunks[0]);
                render_breadcrumb(frame, chunks[1], section.as_deref(), subsection.as_deref());
                render_help(frame, chunks[2], help.as_deref());
                frame.render_widget(
                    Paragraph::new(Span::styled(prompt_line, theme::heading_style())),
                    chunks[3],
                );
                frame.render_stateful_widget(
                    List::new(list_items)
                        .block(Block::default().borders(Borders::ALL))
                        .highlight_style(theme::selected_style())
                        .highlight_symbol("› "),
                    chunks[4],
                    &mut list_state,
                );
                render_hints(frame, chunks[5], HINTS_SELECT);
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
                } if cursor + 1 < matches.len() => cursor += 1,
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
        // level 1 = section (breadcrumb root). Entering a new section
        // clears subsection, help, and log — the user is in a new phase.
        // level 2 = subsection (e.g. picked model_provider / channel). Only
        // clears help, since the log may still be carrying status from
        // just-completed subsection work.
        match level {
            1 => {
                self.section = Some(text.to_string());
                self.subsection = None;
                self.help = None;
                self.log.clear();
            }
            _ => {
                self.subsection = Some(text.to_string());
                self.help = None;
            }
        }
    }

    fn note(&mut self, msg: &str) {
        // Note = help text for the upcoming prompt (usually the field
        // docstring). Persists until replaced by the next note() or
        // cleared by entering a new section / subsection.
        if msg.is_empty() {
            self.help = None;
        } else {
            self.help = Some(msg.to_string());
        }
        // A new field is being prompted — drop stale warns from the
        // previous field. Status entries (informational) are preserved.
        self.log.retain(|l| matches!(l.level, LogLevel::Status));
    }

    fn status(&mut self, msg: &str) {
        self.log.push(LogLine {
            level: LogLevel::Status,
            text: msg.to_string(),
        });
        // Force a paint so the message is visible before any subsequent
        // blocking work (e.g. a models.dev fetch). Without this, the new
        // log line only surfaces when the next prompt triggers a draw,
        // which defeats the point of a "waiting…" indicator.
        let _ = self.draw_idle();
    }

    fn warn(&mut self, msg: &str) {
        self.log.push(LogLine {
            level: LogLevel::Warn,
            text: msg.to_string(),
        });
        let _ = self.draw_idle();
    }
}

impl RatatuiUi {
    /// Render a frame with banner + breadcrumb + log + help and no input
    /// region. Used by `status` / `warn` to flush the log immediately,
    /// since those are called between prompts (no prompt loop is active
    /// to drive its own draw).
    fn draw_idle(&mut self) -> Result<()> {
        let section = self.section.clone();
        let subsection = self.subsection.clone();
        let help = self.help.clone();
        let log_lines: Vec<LogLine> = self
            .log
            .iter()
            .map(|l| LogLine {
                level: match l.level {
                    LogLevel::Status => LogLevel::Status,
                    LogLevel::Warn => LogLevel::Warn,
                },
                text: l.text.clone(),
            })
            .collect();
        self.terminal.draw(|frame| {
            let r = layout(frame.area(), help.as_deref(), 0);
            render_banner(frame, r.banner);
            render_breadcrumb(
                frame,
                r.breadcrumb,
                section.as_deref(),
                subsection.as_deref(),
            );
            render_log(frame, r.log, &log_lines);
            render_help(frame, r.help, help.as_deref());
        })?;
        Ok(())
    }
}
