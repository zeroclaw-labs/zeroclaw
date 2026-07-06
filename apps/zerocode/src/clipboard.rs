//! Platform clipboard image reading and text reading.
//!
//! Shells out to system clipboard tools to read image data from the
//! clipboard and read text from the clipboard. Gracefully degrades —
//! returns `None` if no tool is available or no image/text is present.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Number of read attempts when the clipboard advertises an image target but
/// the byte read comes back empty. A freshly captured screenshot populates the
/// clipboard asynchronously: the selection owner may advertise the image target
/// a beat before the pixel data is actually servable, so a single read races
/// the export and intermittently returns nothing. Re-reading across a short
/// window absorbs that race so a genuinely-present image always resolves.
const IMAGE_READ_ATTEMPTS: u32 = 8;

/// Delay between image-read attempts.
const IMAGE_READ_RETRY_DELAY: Duration = Duration::from_millis(120);

/// Try to read image data from the system clipboard.
///
/// Returns `Some((bytes, mime_type))` on success, `None` when the clipboard
/// genuinely holds no image or no clipboard tool is available.
///
/// The read is target-aware and retrying: it first asks the clipboard which
/// image target it can serve, then reads that exact target with a bounded
/// retry. Hardcoding `image/png` and reading once (the previous behaviour)
/// silently lost screenshots whenever the source offered the image under a
/// different target or had not finished exporting `image/png` yet, which is
/// why paste "sometimes worked and sometimes didn't."
pub(crate) fn read_clipboard_image() -> Option<(Vec<u8>, String)> {
    let tool = which_clipboard_tool()?;

    // Resolve the concrete image target the clipboard can actually serve.
    // Falls back to `image/png` for tools that don't enumerate targets
    // (pngpaste, PowerShell) — those always emit PNG bytes directly.
    let target = image_target_for_tool(&tool);

    for attempt in 0..IMAGE_READ_ATTEMPTS {
        match run_clipboard_tool(&tool, &target) {
            Some(bytes) if !bytes.is_empty() => {
                return Some((bytes, mime_for_target(&target)));
            }
            // Empty read while a target is advertised: the export is still
            // racing the offer. Back off briefly and retry.
            _ => {
                if attempt + 1 < IMAGE_READ_ATTEMPTS {
                    std::thread::sleep(IMAGE_READ_RETRY_DELAY);
                }
            }
        }
    }
    None
}

/// Returns `true` when the system clipboard currently advertises a servable
/// image target. Used to distinguish "no image was ever on the clipboard"
/// (fall through to text paste) from "an image is present but the byte read
/// keeps racing the export" (surface a real error instead of silently
/// dropping the screenshot).
pub(crate) fn clipboard_has_image() -> bool {
    match which_clipboard_tool() {
        Some(ClipboardTool::Xclip) => list_targets_xclip()
            .map(|t| targets_contain_image(&t))
            .unwrap_or(false),
        Some(ClipboardTool::WlPaste) => list_targets_wl_paste()
            .map(|t| targets_contain_image(&t))
            .unwrap_or(false),
        // Direct-PNG tools can't enumerate; treat presence as unknown-false so
        // callers keep the existing text-fallback behaviour there.
        Some(ClipboardTool::PngPaste) | Some(ClipboardTool::PowerShellImage) => false,
        None => false,
    }
}

fn targets_contain_image(targets: &[String]) -> bool {
    targets.iter().any(|t| {
        ACCEPTED_IMAGE_TARGETS
            .iter()
            .any(|wanted| t.eq_ignore_ascii_case(wanted))
    })
}

/// Try to read UTF-8 text from the system clipboard.
///
/// This is the fallback path for terminals that do not deliver bracketed
/// paste (`Event::Paste`) — notably the legacy Windows console — where a
/// Ctrl+V press is the only paste signal the TUI receives. Returns `None`
/// when no text tool is available or the clipboard holds no text.
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

/// Image MIME targets we accept from the clipboard, in preference order.
/// PNG first (lossless, universally supported by vision models), then common
/// screenshot-tool alternatives. Selection is intersected with what the
/// clipboard actually advertises.
const ACCEPTED_IMAGE_TARGETS: &[&str] = &[
    "image/png",
    "image/webp",
    "image/jpeg",
    "image/jpg",
    "image/gif",
    "image/bmp",
    "image/tiff",
];

/// Resolve the concrete image target the clipboard can serve for this tool.
///
/// For target-enumerating tools (xclip, wl-paste) this intersects the
/// clipboard's advertised targets with [`ACCEPTED_IMAGE_TARGETS`] and returns
/// the highest-preference match. For direct-PNG tools (pngpaste, PowerShell)
/// enumeration is not available, so it returns `image/png` unconditionally —
/// those tools re-encode to PNG regardless of the source format.
fn image_target_for_tool(tool: &ClipboardTool) -> String {
    let advertised = match tool {
        ClipboardTool::Xclip => list_targets_xclip(),
        ClipboardTool::WlPaste => list_targets_wl_paste(),
        ClipboardTool::PngPaste | ClipboardTool::PowerShellImage => None,
    };
    match advertised {
        Some(targets) => ACCEPTED_IMAGE_TARGETS
            .iter()
            .find(|wanted| targets.iter().any(|t| t.eq_ignore_ascii_case(wanted)))
            .map(|t| t.to_string())
            .unwrap_or_else(|| "image/png".to_string()),
        None => "image/png".to_string(),
    }
}

/// Normalize a clipboard target to a MIME type the vision pipeline accepts.
/// `image/jpg` is a common non-standard target name for JPEG.
fn mime_for_target(target: &str) -> String {
    if target.eq_ignore_ascii_case("image/jpg") {
        "image/jpeg".to_string()
    } else {
        target.to_ascii_lowercase()
    }
}

fn list_targets_xclip() -> Option<Vec<String>> {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_target_lines(&output.stdout))
}

fn list_targets_wl_paste() -> Option<Vec<String>> {
    let output = Command::new("wl-paste")
        .arg("--list-types")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_target_lines(&output.stdout))
}

fn parse_target_lines(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
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

fn run_clipboard_tool(tool: &ClipboardTool, target: &str) -> Option<Vec<u8>> {
    let mut cmd = match tool {
        ClipboardTool::Xclip => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-t", target, "-o"]);
            c
        }
        ClipboardTool::WlPaste => {
            let mut c = Command::new("wl-paste");
            c.args(["--type", target]);
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

    #[test]
    fn parse_target_lines_trims_and_filters() {
        let raw = b"image/png\n  image/webp \n\nTIMESTAMP\n";
        let parsed = parse_target_lines(raw);
        assert_eq!(parsed, vec!["image/png", "image/webp", "TIMESTAMP"]);
    }

    #[test]
    fn targets_contain_image_detects_supported() {
        assert!(targets_contain_image(&[
            "TIMESTAMP".into(),
            "text/plain".into(),
            "image/png".into(),
        ]));
        assert!(targets_contain_image(&["IMAGE/PNG".into()]));
        assert!(!targets_contain_image(&[
            "TIMESTAMP".into(),
            "text/plain".into(),
            "text/html".into(),
        ]));
        assert!(!targets_contain_image(&[]));
    }

    #[test]
    fn image_target_selection_prefers_png() {
        // The intersection logic lives in image_target_for_tool via the
        // advertised list; exercise the preference ordering directly.
        let advertised = ["image/jpeg".to_string(), "image/png".to_string()];
        let chosen = ACCEPTED_IMAGE_TARGETS
            .iter()
            .find(|w| advertised.iter().any(|t| t.eq_ignore_ascii_case(w)))
            .map(|t| t.to_string());
        assert_eq!(chosen.as_deref(), Some("image/png"));
    }

    #[test]
    fn image_target_selection_falls_back_to_next_preference() {
        let advertised = ["image/gif".to_string(), "image/jpeg".to_string()];
        let chosen = ACCEPTED_IMAGE_TARGETS
            .iter()
            .find(|w| advertised.iter().any(|t| t.eq_ignore_ascii_case(w)))
            .map(|t| t.to_string());
        assert_eq!(chosen.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn mime_for_target_normalizes_jpg() {
        assert_eq!(mime_for_target("image/jpg"), "image/jpeg");
        assert_eq!(mime_for_target("IMAGE/PNG"), "image/png");
        assert_eq!(mime_for_target("image/webp"), "image/webp");
    }
}
