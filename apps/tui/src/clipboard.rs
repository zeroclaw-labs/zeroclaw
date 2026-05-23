//! Platform clipboard image reading.
//!
//! Shells out to system clipboard tools to read image data from the
//! clipboard. Gracefully degrades — returns `None` if no tool is
//! available or no image is present.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Try to read image data from the system clipboard.
///
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
}

impl ClipboardTool {
    fn mime_type(&self) -> &'static str {
        "image/png"
    }
}

fn which_clipboard_tool() -> Option<ClipboardTool> {
    // Check Wayland first (more modern), then X11, then macOS.
    if which_exists("wl-paste") {
        Some(ClipboardTool::WlPaste)
    } else if which_exists("xclip") {
        Some(ClipboardTool::Xclip)
    } else if which_exists("pngpaste") {
        Some(ClipboardTool::PngPaste)
    } else {
        None
    }
}

fn which_exists(name: &str) -> bool {
    Command::new("which")
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
    };

    cmd.stderr(std::process::Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
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
    fn which_exists_finds_sh() {
        // `sh` should exist on any Unix system.
        assert!(which_exists("sh"));
    }

    #[test]
    fn which_exists_rejects_nonsense() {
        assert!(!which_exists("this_tool_definitely_does_not_exist_12345"));
    }

    #[test]
    fn temp_path_has_extension() {
        let p = clipboard_temp_path("png");
        assert!(p.to_str().unwrap().ends_with(".png"));
        assert!(p.to_str().unwrap().contains("clipboard_"));
    }
}
