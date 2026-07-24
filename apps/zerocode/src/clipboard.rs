//! Platform clipboard image reading and text reading.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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
