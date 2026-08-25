use anyhow::{Result, bail};
use crossterm::{
    cursor::MoveToColumn,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, Clear, ClearType},
};
use std::io::{BufRead, Write};

const SECRET_MASK_MAX: usize = 24;

#[must_use]
pub fn ensure_terminal_utf8_erase() -> TerminalUtf8EraseGuard {
    imp::ensure_terminal_utf8_erase()
}

pub struct TerminalUtf8EraseGuard {
    #[cfg(any(target_os = "android", target_os = "linux"))]
    fd: libc::c_int,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    original: Option<libc::termios>,
}

#[cfg(any(target_os = "android", target_os = "linux"))]
impl Drop for TerminalUtf8EraseGuard {
    fn drop(&mut self) {
        let Some(original) = self.original.as_ref() else {
            return;
        };
        // Best-effort restoration: this guard only tweaks terminal line
        // discipline for interactive CLI input, and drop must not panic.
        // SAFETY: `original` was initialized by a successful `tcgetattr` on
        // this same descriptor, and the guard retains both until restoration.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSANOW, original);
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
impl Drop for TerminalUtf8EraseGuard {
    fn drop(&mut self) {}
}

#[cfg(any(target_os = "android", target_os = "linux"))]
mod imp {
    use super::TerminalUtf8EraseGuard;

    pub(super) fn ensure_terminal_utf8_erase() -> TerminalUtf8EraseGuard {
        ensure_terminal_utf8_erase_for_fd(libc::STDIN_FILENO)
    }

    pub(super) fn ensure_terminal_utf8_erase_for_fd(fd: libc::c_int) -> TerminalUtf8EraseGuard {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `termios` points to writable, correctly aligned storage for
        // one `libc::termios`; initialization is trusted only when rc is zero.
        let rc = unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) };
        if rc != 0 {
            return TerminalUtf8EraseGuard { fd, original: None };
        }

        // SAFETY: the successful `tcgetattr` above initialized every byte of
        // the `termios` output object.
        let original = unsafe { termios.assume_init() };
        if original.c_iflag & libc::IUTF8 != 0 {
            return TerminalUtf8EraseGuard { fd, original: None };
        }

        let mut updated = original;
        updated.c_iflag |= libc::IUTF8;
        // SAFETY: `updated` is a fully initialized `termios` value derived
        // from this descriptor's state and remains live for the call.
        let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &updated) };
        TerminalUtf8EraseGuard {
            fd,
            original: (rc == 0).then_some(original),
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
mod imp {
    use super::TerminalUtf8EraseGuard;

    pub(super) fn ensure_terminal_utf8_erase() -> TerminalUtf8EraseGuard {
        TerminalUtf8EraseGuard {}
    }
}

#[derive(Debug, Clone, Default)]
pub struct Input {
    prompt: String,
    default: Option<String>,
    allow_empty: bool,
}

impl Input {
    #[must_use]
    pub fn new() -> Self {
        Self {
            prompt: String::new(),
            default: None,
            allow_empty: false,
        }
    }

    #[must_use]
    pub fn with_prompt<S: Into<String>>(mut self, prompt: S) -> Self {
        self.prompt = prompt.into();
        self
    }

    #[must_use]
    pub fn allow_empty(mut self, val: bool) -> Self {
        self.allow_empty = val;
        self
    }

    #[must_use]
    pub fn default<S: Into<String>>(mut self, value: S) -> Self {
        self.default = Some(value.into());
        self
    }

    pub fn interact_text(self) -> Result<String> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        self.interact_text_with_io(stdin.lock(), stdout.lock())
    }

    fn interact_text_with_io<R: BufRead, W: Write>(
        self,
        mut reader: R,
        mut writer: W,
    ) -> Result<String> {
        loop {
            write!(writer, "{}", self.render_prompt())?;
            writer.flush()?;

            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line)?;
            if bytes_read == 0 {
                bail!("No input received from stdin");
            }

            let trimmed = trim_trailing_line_ending(&line);
            if trimmed.is_empty() {
                if let Some(default) = &self.default {
                    return Ok(default.clone());
                }
                if self.allow_empty {
                    return Ok(String::new());
                }
                writeln!(writer, "Input cannot be empty.")?;
                continue;
            }

            return Ok(trimmed.to_string());
        }
    }

    fn render_prompt(&self) -> String {
        match &self.default {
            Some(default) => format!("{} [{}]: ", self.prompt, default),
            None => format!("{}: ", self.prompt),
        }
    }
}

fn trim_trailing_line_ending(input: &str) -> &str {
    input.trim_end_matches(['\n', '\r'])
}

/// Interactive secret prompt that echoes a masked placeholder instead of the
/// typed characters. Reads from the terminal in raw mode and renders feedback
/// on stderr so a piped stdout stays clean.
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

#[cfg(test)]
mod tests {
    use super::{Input, trim_trailing_line_ending};
    use anyhow::Result;
    use std::io::Cursor;

    #[test]
    fn trim_trailing_line_ending_strips_newlines() {
        assert_eq!(trim_trailing_line_ending("value\n"), "value");
        assert_eq!(trim_trailing_line_ending("value\r\n"), "value");
        assert_eq!(trim_trailing_line_ending("value\r"), "value");
        assert_eq!(trim_trailing_line_ending("value"), "value");
    }

    #[test]
    fn interact_text_returns_typed_value_without_newline() -> Result<()> {
        let input = Input::new().with_prompt("Prompt");
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"typed-value\n"), &mut output)?;

        assert_eq!(value, "typed-value");
        assert_eq!(String::from_utf8(output)?, "Prompt: ");
        Ok(())
    }

    #[test]
    fn interact_text_returns_default_for_blank_input() -> Result<()> {
        let input = Input::new().with_prompt("Prompt").default("fallback");
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"\n"), &mut output)?;

        assert_eq!(value, "fallback");
        assert_eq!(String::from_utf8(output)?, "Prompt [fallback]: ");
        Ok(())
    }

    #[test]
    fn interact_text_allows_empty_when_requested() -> Result<()> {
        let input = Input::new().with_prompt("Prompt").allow_empty(true);
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"\n"), &mut output)?;

        assert_eq!(value, "");
        assert_eq!(String::from_utf8(output)?, "Prompt: ");
        Ok(())
    }

    #[test]
    fn interact_text_reprompts_when_empty_is_not_allowed() -> Result<()> {
        let input = Input::new().with_prompt("Prompt");
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"\nsecond-try\n"), &mut output)?;

        assert_eq!(value, "second-try");
        assert_eq!(
            String::from_utf8(output)?,
            "Prompt: Input cannot be empty.\nPrompt: "
        );
        Ok(())
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    #[test]
    fn terminal_utf8_erase_guard_sets_and_restores_iutf8() {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        // SAFETY: both output pointers refer to live `c_int` storage; the
        // optional name and termios/winsize inputs are intentionally null.
        let openpty_rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(openpty_rc, 0, "openpty failed");

        // SAFETY: `openpty` succeeded, so both descriptors are live. Each
        // `MaybeUninit` output is read only after its `tcgetattr` succeeds,
        // and both descriptors are closed exactly once at the end.
        unsafe {
            let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
            assert_eq!(libc::tcgetattr(slave_fd, original.as_mut_ptr()), 0);
            let mut original = original.assume_init();
            original.c_iflag &= !libc::IUTF8;
            assert_eq!(libc::tcsetattr(slave_fd, libc::TCSANOW, &original), 0);

            {
                let _guard = super::imp::ensure_terminal_utf8_erase_for_fd(slave_fd);
                let mut updated = std::mem::MaybeUninit::<libc::termios>::uninit();
                assert_eq!(libc::tcgetattr(slave_fd, updated.as_mut_ptr()), 0);
                assert_ne!(updated.assume_init().c_iflag & libc::IUTF8, 0);
            }

            let mut restored = std::mem::MaybeUninit::<libc::termios>::uninit();
            assert_eq!(libc::tcgetattr(slave_fd, restored.as_mut_ptr()), 0);
            assert_eq!(restored.assume_init().c_iflag & libc::IUTF8, 0);

            libc::close(slave_fd);
            libc::close(master_fd);
        }
    }
}
