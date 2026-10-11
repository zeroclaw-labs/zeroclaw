//! Platform clipboard reading and text delivery.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// What the writer can establish about a clipboard copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyOutcome {
    Copied,
    /// OSC 52 was emitted, but terminals do not acknowledge clipboard delivery.
    TerminalRequested,
}

impl CopyOutcome {
    pub(crate) fn notice(self) -> String {
        crate::i18n::t(match self {
            Self::Copied => "zc-chat-copied-clipboard",
            Self::TerminalRequested => "zc-clipboard-requested",
        })
    }

    pub(crate) fn label(self) -> String {
        crate::i18n::t(match self {
            Self::Copied => "zc-chat-copy-message-copied",
            Self::TerminalRequested => "zc-clipboard-requested-label",
        })
    }
}

pub(crate) fn copy_failure_notice(error: &io::Error) -> String {
    crate::i18n::t_args("zc-clipboard-copy-failed", &[("error", &error.to_string())])
}

/// Deliver text to a local Linux clipboard writer, or request terminal delivery.
#[cfg(not(test))]
pub(crate) fn copy_text(text: &str) -> io::Result<CopyOutcome> {
    #[cfg(target_os = "linux")]
    if let Some(outcome) = copy_with_native_tools(
        text,
        &native_writer_tools(|name| std::env::var_os(name)),
        TextWriter::command,
    )? {
        return Ok(outcome);
    }

    crate::mouse::copy_osc52(text)?;
    Ok(CopyOutcome::TerminalRequested)
}

#[cfg(test)]
std::thread_local! {
    static COPY_RESULT: std::cell::RefCell<Option<io::Result<CopyOutcome>>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Tests never touch the host clipboard or terminal output.
#[cfg(test)]
pub(crate) fn copy_text(_text: &str) -> io::Result<CopyOutcome> {
    COPY_RESULT.with(|slot| match slot.borrow().as_ref() {
        Some(Ok(outcome)) => Ok(*outcome),
        Some(Err(error)) => Err(match error.raw_os_error() {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(error.kind(), error.to_string()),
        }),
        None => Ok(CopyOutcome::Copied),
    })
}

/// Override copy delivery for this thread and restore it even if the test panics.
#[cfg(test)]
pub(crate) fn with_copy_result<R>(result: io::Result<CopyOutcome>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<io::Result<CopyOutcome>>);

    impl Drop for Restore {
        fn drop(&mut self) {
            COPY_RESULT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }

    let _restore = Restore(COPY_RESULT.with(|slot| slot.replace(Some(result))));
    f()
}

#[cfg(any(target_os = "linux", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextWriter {
    WlCopy,
    Xclip,
}

#[cfg(any(target_os = "linux", all(test, unix)))]
impl TextWriter {
    fn command(self) -> Command {
        match self {
            Self::WlCopy => {
                let mut command = Command::new("wl-copy");
                command.args(["--type", "text/plain;charset=utf-8"]);
                command
            }
            Self::Xclip => {
                let mut command = Command::new("xclip");
                command.args(["-selection", "clipboard"]);
                command
            }
        }
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn native_writer_tools(mut env: impl FnMut(&str) -> Option<std::ffi::OsString>) -> Vec<TextWriter> {
    // Forwarded displays still belong to an SSH session. Copy must target the
    // user's terminal rather than a clipboard on the execution host.
    if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|name| env(name).is_some())
    {
        return Vec::new();
    }

    let mut tools = Vec::with_capacity(2);
    if env("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        tools.push(TextWriter::WlCopy);
    }
    if env("DISPLAY").is_some_and(|value| !value.is_empty()) {
        tools.push(TextWriter::Xclip);
    }
    tools
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn copy_with_native_tools(
    text: &str,
    tools: &[TextWriter],
    mut command: impl FnMut(TextWriter) -> Command,
) -> io::Result<Option<CopyOutcome>> {
    for &tool in tools {
        match run_text_writer(&mut command(tool), text.as_bytes(), Duration::from_secs(1)) {
            Ok(()) => return Ok(Some(CopyOutcome::Copied)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// One deadline covers both pipe backpressure and process completion.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn run_text_writer(command: &mut Command, payload: &[u8], timeout: Duration) -> io::Result<()> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::process::Stdio;
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("clipboard writer stdin unavailable"))?;
        let fd = stdin.as_raw_fd();
        // SAFETY: stdin owns this live pipe descriptor throughout both calls.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: F_SETFL only changes descriptor flags on the owned pipe.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }

        let mut remaining = payload;
        while !remaining.is_empty() {
            check_writer_deadline(deadline)?;
            match stdin.write(remaining) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => remaining = &remaining[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    wait_writer_poll(Some(fd), deadline)?;
                }
                Err(error) => return Err(error),
            }
        }
        drop(stdin);

        loop {
            check_writer_deadline(deadline)?;
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "clipboard writer exited with {status}"
                    )))
                };
            }
            wait_writer_poll(None, deadline)?;
        }
    })();

    if result.is_err() {
        // Always reap after a failed write or deadline; dropping Child does not
        // stop a clipboard helper that is still running.
        let _ = child.kill();
        loop {
            match child.wait() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                _ => break,
            }
        }
    }
    result
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn check_writer_deadline(deadline: std::time::Instant) -> io::Result<()> {
    if std::time::Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "clipboard writer timed out",
        ))
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn wait_writer_poll(
    fd: Option<std::os::fd::RawFd>,
    deadline: std::time::Instant,
) -> io::Result<()> {
    check_writer_deadline(deadline)?;
    let wait_ms = deadline
        .saturating_duration_since(std::time::Instant::now())
        .as_millis()
        .clamp(1, 10) as libc::c_int;
    let mut descriptor = libc::pollfd {
        fd: fd.unwrap_or(-1),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: poll receives one initialized descriptor; -1 is ignored while
    // waiting for process completion, and timeout is bounded by the deadline.
    if unsafe { libc::poll(&mut descriptor, 1, wait_ms) } == -1 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

/// Try to read image data from the system clipboard.
/// Returns `Some((bytes, mime_type))` on success, `None` if no image
/// is present or no clipboard tool is available.
pub(crate) fn read_clipboard_image() -> Option<(Vec<u8>, String)> {
    let tool = which_clipboard_tool()?;
    let output = run_clipboard_tool(&tool)?;
    if output.is_empty() {
        return None;
    }
    Some((output, tool.mime_type().to_string()))
}

pub(crate) fn read_clipboard_text() -> Option<String> {
    let tool = which_text_tool()?;
    let output = run_text_tool(&tool)?;
    let text = String::from_utf8_lossy(&output).into_owned();
    if text.is_empty() {
        return None;
    }
    Some(text)
}

/// Check if text looks like a filesystem path that could be auto-attached.
pub(crate) fn looks_like_file_path(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return false;
    }
    // Must start with / or ~
    if !trimmed.starts_with('/') && !trimmed.starts_with('~') {
        return false;
    }
    // No control characters (except normal whitespace already trimmed)
    !trimmed.chars().any(|c| c.is_control())
}

// ── Platform tool detection ──────────────────────────────────────

#[derive(Debug, Clone)]
enum ClipboardTool {
    /// xclip (X11)
    Xclip,
    /// wl-paste (Wayland)
    WlPaste,
    /// pngpaste (macOS, homebrew)
    PngPaste,
    /// PowerShell Get-Clipboard -Format Image (Windows)
    PowerShellImage,
}

impl ClipboardTool {
    fn mime_type(&self) -> &'static str {
        "image/png"
    }
}

/// Clipboard text reader, selected per platform.
#[derive(Debug, Clone)]
enum TextTool {
    /// xclip (X11)
    Xclip,
    /// wl-paste (Wayland)
    WlPaste,
    /// pbpaste (macOS)
    PbPaste,
    /// PowerShell Get-Clipboard (Windows)
    PowerShell,
}

fn which_clipboard_tool() -> Option<ClipboardTool> {
    // Windows first: the legacy console doesn't deliver bracketed paste, so
    // the clipboard tool is the only image path. Then Wayland, X11, macOS.
    if cfg!(windows) {
        Some(ClipboardTool::PowerShellImage)
    } else if which_exists("wl-paste") {
        Some(ClipboardTool::WlPaste)
    } else if which_exists("xclip") {
        Some(ClipboardTool::Xclip)
    } else if which_exists("pngpaste") {
        Some(ClipboardTool::PngPaste)
    } else {
        None
    }
}

fn which_text_tool() -> Option<TextTool> {
    if cfg!(windows) {
        Some(TextTool::PowerShell)
    } else if which_exists("wl-paste") {
        Some(TextTool::WlPaste)
    } else if which_exists("xclip") {
        Some(TextTool::Xclip)
    } else if which_exists("pbpaste") {
        Some(TextTool::PbPaste)
    } else {
        None
    }
}

fn which_exists(name: &str) -> bool {
    // `which` is absent on Windows; `where` is the equivalent. Both take the
    // tool name as a positional arg and exit non-zero when it's not found.
    let locator = if cfg!(windows) { "where" } else { "which" };
    Command::new(locator)
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Tool execution ───────────────────────────────────────────────

fn run_clipboard_tool(tool: &ClipboardTool) -> Option<Vec<u8>> {
    let mut cmd = match tool {
        ClipboardTool::Xclip => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-t", "image/png", "-o"]);
            c
        }
        ClipboardTool::WlPaste => {
            let mut c = Command::new("wl-paste");
            c.args(["--type", "image/png"]);
            c
        }
        ClipboardTool::PngPaste => {
            let mut c = Command::new("pngpaste");
            c.arg("-");
            c
        }
        ClipboardTool::PowerShellImage => {
            // Read the clipboard image and emit raw PNG bytes to stdout.
            // System.Windows.Forms.Clipboard requires STA; -Sta provides it.
            let mut c = Command::new("powershell");
            c.args([
                "-NoProfile",
                "-Sta",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $img = [System.Windows.Forms.Clipboard]::GetImage(); \
                 if ($img) { \
                   $ms = New-Object System.IO.MemoryStream; \
                   $img.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); \
                   $out = [System.Console]::OpenStandardOutput(); \
                   $bytes = $ms.ToArray(); \
                   $out.Write($bytes, 0, $bytes.Length); \
                   $out.Flush() \
                 }",
            ]);
            c
        }
    };

    cmd.stderr(std::process::Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(output.stdout)
}

fn run_text_tool(tool: &TextTool) -> Option<Vec<u8>> {
    let mut cmd = match tool {
        TextTool::Xclip => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-o"]);
            c
        }
        TextTool::WlPaste => {
            let mut c = Command::new("wl-paste");
            c.arg("--no-newline");
            c
        }
        TextTool::PbPaste => Command::new("pbpaste"),
        TextTool::PowerShell => {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Get-Clipboard -Raw"]);
            c
        }
    };

    cmd.stderr(std::process::Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

/// Generate a temp file path for a clipboard image.
pub(crate) fn clipboard_temp_path(ext: &str) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    std::env::temp_dir().join(format!("clipboard_{ts}.{ext}"))
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_override_restores_nested_result_after_panic() {
        assert_eq!(copy_text("sample").unwrap(), CopyOutcome::Copied);
        with_copy_result(Ok(CopyOutcome::TerminalRequested), || {
            let panic = std::panic::catch_unwind(|| {
                with_copy_result(
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixture error")),
                    || {
                        for _ in 0..2 {
                            let error = copy_text("sample").unwrap_err();
                            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                            assert_eq!(error.to_string(), "fixture error");
                        }
                        panic!("restore the outer result");
                    },
                );
            });
            assert!(panic.is_err());
            assert_eq!(copy_text("sample").unwrap(), CopyOutcome::TerminalRequested);
        });
        assert_eq!(copy_text("sample").unwrap(), CopyOutcome::Copied);
    }

    #[cfg(unix)]
    #[test]
    fn native_writer_selection_requires_a_local_display() {
        let select = |variables: &[(&str, &str)]| {
            native_writer_tools(|name| {
                variables
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| std::ffi::OsString::from(*value))
            })
        };
        assert!(select(&[]).is_empty());
        assert!(select(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", "")]).is_empty());
        assert_eq!(
            select(&[("WAYLAND_DISPLAY", "wayland-fixture")]),
            [TextWriter::WlCopy]
        );
        assert_eq!(select(&[("DISPLAY", ":fixture")]), [TextWriter::Xclip]);
        assert_eq!(
            select(&[
                ("WAYLAND_DISPLAY", "wayland-fixture"),
                ("DISPLAY", ":fixture")
            ]),
            [TextWriter::WlCopy, TextWriter::Xclip]
        );
        for ssh_variable in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
            assert!(
                select(&[
                    ("WAYLAND_DISPLAY", "wayland-fixture"),
                    ("DISPLAY", ":fixture"),
                    (ssh_variable, ""),
                ])
                .is_empty(),
                "the presence of {ssh_variable} suppresses native writers"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_writer_commands_use_clipboard_selection() {
        let wayland = TextWriter::WlCopy.command();
        assert_eq!(wayland.get_program(), "wl-copy");
        assert_eq!(
            wayland.get_args().collect::<Vec<_>>(),
            ["--type", "text/plain;charset=utf-8"]
        );
        let x11 = TextWriter::Xclip.command();
        assert_eq!(x11.get_program(), "xclip");
        assert_eq!(
            x11.get_args().collect::<Vec<_>>(),
            ["-selection", "clipboard"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_writer_receives_exact_payload_without_a_shell() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clipboard-fixture");
        let payload = "literal $(clipboard-fixture) `fixture`\nUnicode: 🦀\0";
        let mut command = Command::new("tee");
        command.arg(&path);
        run_text_writer(&mut command, payload.as_bytes(), Duration::from_secs(1)).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), payload.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn native_writer_nonzero_exit_is_not_silently_retried() {
        let mut attempted = Vec::new();
        let error = copy_with_native_tools("", &[TextWriter::WlCopy, TextWriter::Xclip], |tool| {
            attempted.push(tool);
            Command::new("false")
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("exited with"));
        assert_eq!(attempted, [TextWriter::WlCopy]);
    }

    #[cfg(unix)]
    #[test]
    fn missing_native_writer_can_try_the_next_compatible_tool() {
        let mut attempted = Vec::new();
        let outcome =
            copy_with_native_tools("", &[TextWriter::WlCopy, TextWriter::Xclip], |tool| {
                attempted.push(tool);
                Command::new(match tool {
                    TextWriter::WlCopy => "/nonexistent/zerocode-clipboard-fixture",
                    TextWriter::Xclip => "true",
                })
            })
            .unwrap();
        assert_eq!(outcome, Some(CopyOutcome::Copied));
        assert_eq!(attempted, [TextWriter::WlCopy, TextWriter::Xclip]);
        assert_eq!(
            copy_with_native_tools("", &[], TextWriter::command).unwrap(),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_writer_deadline_covers_blocked_stdin_and_process_exit() {
        // A never-reading child fills the pipe. An empty payload isolates the
        // later process-wait path under the same deadline.
        let oversized = vec![b'x'; 2 * 1024 * 1024];
        for payload in [&oversized[..], &[][..]] {
            let mut command = Command::new("sleep");
            command.arg("30");
            let start = std::time::Instant::now();
            let error =
                run_text_writer(&mut command, payload, Duration::from_millis(80)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn looks_like_path_absolute() {
        assert!(looks_like_file_path("/home/user/photo.png"));
        assert!(looks_like_file_path("~/Documents/file.txt"));
        assert!(looks_like_file_path("/tmp/test"));
    }

    #[test]
    fn looks_like_path_rejects() {
        assert!(!looks_like_file_path(""));
        assert!(!looks_like_file_path("hello world"));
        assert!(!looks_like_file_path("relative/path.txt"));
        assert!(!looks_like_file_path("/path/one\n/path/two"));
    }

    #[test]
    fn which_exists_finds_known_tool() {
        // A tool present on the host: `cmd` on Windows, `sh` on Unix.
        let known = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(which_exists(known));
    }

    #[test]
    fn which_exists_rejects_nonsense() {
        assert!(!which_exists("this_tool_definitely_does_not_exist_12345"));
    }

    #[test]
    fn text_tool_resolves_on_windows() {
        // Windows always resolves to the PowerShell reader without probing
        // PATH, so clipboard text paste has a route even on a bare console.
        if cfg!(windows) {
            assert!(matches!(which_text_tool(), Some(TextTool::PowerShell)));
        }
    }

    #[test]
    fn temp_path_has_extension() {
        let p = clipboard_temp_path("png");
        assert!(p.to_str().unwrap().ends_with(".png"));
        assert!(p.to_str().unwrap().contains("clipboard_"));
    }
}
