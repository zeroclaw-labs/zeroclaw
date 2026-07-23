pub use zeroclaw_runtime::cli_input::*;

use anyhow::Result;
use crossterm::{
    cursor::MoveToColumn,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, Clear, ClearType},
};
use std::io::Write;

const SECRET_MASK_MAX: usize = 24;

#[derive(Debug, Default)]
pub struct SecretInput {
    prompt: String,
}

impl SecretInput {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    pub fn interact(self) -> Result<String> {
        let raw_mode_was_enabled = terminal::is_raw_mode_enabled()?;
        if !raw_mode_was_enabled {
            terminal::enable_raw_mode()?;
        }
        let _raw_mode = RawModeGuard {
            restore_cooked_mode: !raw_mode_was_enabled,
        };

        let stderr = std::io::stderr();
        let mut writer = stderr.lock();
        self.interact_with_events(
            &mut writer,
            || Ok(event::read()?),
            || terminal::size().map_or(80, |(columns, _)| columns),
        )
    }

    fn interact_with_events<W, R, C>(
        &self,
        writer: &mut W,
        mut read_event: R,
        mut terminal_columns: C,
    ) -> Result<String>
    where
        W: Write,
        R: FnMut() -> Result<Event>,
        C: FnMut() -> u16,
    {
        write_secret_prompt(writer, &self.prompt)?;

        let outcome = (|| {
            let mut buffer = String::new();
            loop {
                match read_event()? {
                    Event::Key(KeyEvent {
                        code,
                        modifiers,
                        kind,
                        ..
                    }) if kind == KeyEventKind::Press || kind == KeyEventKind::Repeat => match code
                    {
                        KeyCode::Enter => return Ok(buffer),
                        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                            return Err(interrupted_input());
                        }
                        KeyCode::Char(c)
                            if !modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::META) =>
                        {
                            buffer.push(c);
                            render_secret_feedback(writer, &buffer, terminal_columns())?;
                        }
                        KeyCode::Backspace => {
                            buffer.pop();
                            if buffer.is_empty() {
                                clear_secret_feedback(writer)?;
                            } else {
                                render_secret_feedback(writer, &buffer, terminal_columns())?;
                            }
                        }
                        KeyCode::Esc => return Err(interrupted_input()),
                        _ => {}
                    },
                    Event::Paste(text) => {
                        buffer.extend(text.chars().filter(|c| !c.is_control()));
                        if !buffer.is_empty() {
                            render_secret_feedback(writer, &buffer, terminal_columns())?;
                        }
                    }
                    Event::Resize(_, _) if !buffer.is_empty() => {
                        render_secret_feedback(writer, &buffer, terminal_columns())?;
                    }
                    _ => {}
                }
            }
        })();

        let cleanup = clear_secret_feedback(writer);
        match outcome {
            Err(error) => Err(error),
            Ok(value) => {
                cleanup?;
                Ok(value)
            }
        }
    }
}

struct RawModeGuard {
    restore_cooked_mode: bool,
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.restore_cooked_mode {
            let _ = terminal::disable_raw_mode();
        }
    }
}

fn interrupted_input() -> anyhow::Error {
    std::io::Error::from(std::io::ErrorKind::Interrupted).into()
}

fn write_secret_prompt(writer: &mut impl Write, prompt: &str) -> Result<()> {
    execute!(writer, Print(prompt), Print("\r\n"), MoveToColumn(0))?;
    Ok(())
}

fn render_secret_feedback(
    writer: &mut impl Write,
    buffer: &str,
    terminal_columns: u16,
) -> Result<()> {
    let mask_width = usize::from(terminal_columns).saturating_sub(1);
    execute!(
        writer,
        MoveToColumn(0),
        Clear(ClearType::CurrentLine),
        Print(masked_secret_feedback(buffer, mask_width))
    )?;
    Ok(())
}

fn clear_secret_feedback(writer: &mut impl Write) -> Result<()> {
    execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    Ok(())
}

fn masked_secret_feedback(value: &str, available: usize) -> String {
    let count = value.chars().count();
    if count == 0 || available == 0 {
        return String::new();
    }

    let max_shown = count.min(SECRET_MASK_MAX).min(available);
    if max_shown == count {
        return "•".repeat(count);
    }

    for shown in (0..=max_shown).rev() {
        let suffix = format!(" (+{})", count - shown);
        if shown + suffix.chars().count() <= available {
            return format!("{}{suffix}", "•".repeat(shown));
        }
    }

    "•".repeat(count.min(available))
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn prompt_and_repaints_stay_on_one_feedback_line() {
        let events = vec![
            key(KeyCode::Char('a')),
            key(KeyCode::Char('b')),
            key(KeyCode::Backspace),
            key(KeyCode::Char('c')),
            key(KeyCode::Enter),
        ];
        let mut events = events.into_iter();
        let mut output = Vec::new();

        let value = SecretInput::new()
            .with_prompt("Enter secret")
            .interact_with_events(&mut output, || Ok(events.next().expect("event")), || 80)
            .expect("input should succeed");

        assert_eq!(value, "ac");
        assert!(output.starts_with(b"Enter secret\r\n\x1b[1G"));
        assert!(String::from_utf8_lossy(&output).contains("\x1b[1G\x1b[2K•"));
        assert_eq!(
            output.iter().position(|&byte| byte == b'\n'),
            output.iter().rposition(|&byte| byte == b'\n')
        );
        assert_eq!(
            output
                .windows(b"\x1b[2K".len())
                .filter(|w| *w == b"\x1b[2K")
                .count(),
            5
        );
    }

    #[test]
    fn paste_filters_controls_and_mask_fits_terminal_width() {
        let events = vec![Event::Paste("secret\r\nvalue".into()), key(KeyCode::Enter)];
        let mut events = events.into_iter();
        let mut output = Vec::new();

        let value = SecretInput::new()
            .with_prompt("Enter secret")
            .interact_with_events(&mut output, || Ok(events.next().expect("event")), || 12)
            .expect("paste should succeed");

        assert_eq!(value, "secretvalue");
        assert!(masked_secret_feedback(&value, 11).chars().count() <= 11);
    }

    #[test]
    fn escape_cancels_and_clears_feedback_line() {
        let events = vec![key(KeyCode::Char('x')), key(KeyCode::Esc)];
        let mut events = events.into_iter();
        let mut output = Vec::new();

        let error = SecretInput::new()
            .with_prompt("Enter secret")
            .interact_with_events(&mut output, || Ok(events.next().expect("event")), || 80)
            .expect_err("escape should cancel");

        assert!(
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| { io.kind() == std::io::ErrorKind::Interrupted })
        );
        assert!(output.ends_with(b"\x1b[1G\x1b[2K"));
    }
}
