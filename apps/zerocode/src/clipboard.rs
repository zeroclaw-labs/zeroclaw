//! Platform clipboard image reading and text reading.
//!
//! Shells out to system clipboard tools to read image data from the
//! clipboard and read text from the clipboard. Gracefully degrades —
//! returns `None` if no tool is available or no image/text is present.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Number of full probe+read attempts, each re-enumerating the clipboard's
/// advertised targets and reading a live image target as one atomic unit.
/// Recovers the async-export race and a competing owner moving the servable
/// target mid-read while an image is still advertised. Bounded so an
/// image-less clipboard still returns promptly.
const IMAGE_READ_ATTEMPTS: u32 = 10;

/// Delay between probe+read attempts.
const IMAGE_READ_RETRY_DELAY: Duration = Duration::from_millis(80);

/// Try to read image data from the system clipboard.
///
/// Returns `Some((bytes, mime_type))` on success, `None` when the clipboard
/// genuinely holds no image or no clipboard tool is available. A text-only
/// clipboard does not pay the retry budget before the caller falls through to
/// text paste: an enumerating owner that advertises only text targets lands in
/// `None` on the first probe, and a non-enumerating tool (pngpaste, PowerShell)
/// that reads nothing bails after its first read since the result cannot change
/// across attempts. The retry window is spent only by enumerating tools racing
/// a servable-target export.
pub(crate) fn read_clipboard_image() -> Option<(Vec<u8>, String)> {
    let tool = which_clipboard_tool()?;
    let can_enumerate = tool_can_enumerate(&tool);
    read_clipboard_image_with(
        can_enumerate,
        || image_targets_for_tool(&tool),
        |target| run_clipboard_tool(&tool, target),
        |attempt| {
            if attempt + 1 < IMAGE_READ_ATTEMPTS {
                std::thread::sleep(IMAGE_READ_RETRY_DELAY);
            }
        },
    )
}

/// Probe+read retry loop for the clipboard image path, parameterized on the
/// probe, read, and inter-attempt wait so the retry/fallback contract is
/// testable without a live clipboard.
///
/// `probe` re-enumerates the servable image targets for one attempt; `read`
/// fetches the bytes for a target (empty vec = nothing there yet). `wait`
/// runs between attempts for enumerating tools. `can_enumerate` distinguishes
/// target-enumerating tools (xclip, wl-paste), which may race a servable-target
/// export, from direct-PNG tools (pngpaste, PowerShell), which bail after one
/// read since their result cannot change across attempts.
fn read_clipboard_image_with<P, R, W>(
    can_enumerate: bool,
    probe: P,
    read: R,
    mut wait: W,
) -> Option<(Vec<u8>, String)>
where
    P: Fn() -> Vec<String>,
    R: Fn(&str) -> Option<Vec<u8>>,
    W: FnMut(u32),
{
    for attempt in 0..IMAGE_READ_ATTEMPTS {
        let targets = probe();
        if targets.is_empty() && can_enumerate {
            // Enumerable owner is advertising no image target at all. Not a
            // servable-target race — a plain text-only clipboard. Return
            // immediately so the caller falls through to text paste without
            // burning the full retry window on every ordinary text paste.
            return None;
        }
        for target in &targets {
            if let Some(bytes) = read(target)
                && !bytes.is_empty()
            {
                return Some((bytes, mime_for_target(target)));
            }
        }
        if !can_enumerate {
            // Non-enumerating tools (pngpaste, PowerShell) return promptly and
            // emit nothing for a text-only clipboard; the result will not
            // change across attempts. Bail after the first read instead of
            // paying the retry window on every ordinary text paste.
            return None;
        }
        wait(attempt);
    }
    None
}

/// Whether a clipboard tool can enumerate advertised targets. Used to
/// distinguish "no image advertised" (fast fall-through) from "cannot see the
/// offer" (must retry the read).
fn tool_can_enumerate(tool: &ClipboardTool) -> bool {
    matches!(tool, ClipboardTool::Xclip | ClipboardTool::WlPaste)
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
/// PNG first (lossless, universally supported by vision models), then the other
/// formats the vision pipeline accepts. This set is intentionally kept in sync
/// with `ALLOWED_IMAGE_MIME_TYPES` in `zeroclaw-providers::multimodal`: reading
/// a target the loader later rejects (e.g. BMP/TIFF) would convert a paste into
/// a hard `UnsupportedMime` error, so we never offer one. `image/jpg` is a
/// common non-standard alias for JPEG and is normalized on read.
const ACCEPTED_IMAGE_TARGETS: &[&str] = &[
    "image/png",
    "image/webp",
    "image/jpeg",
    "image/jpg",
    "image/gif",
];

/// Resolve the servable image targets for this tool, in preference order.
///
/// For target-enumerating tools (xclip, wl-paste) this intersects the
/// clipboard's advertised targets with [`ACCEPTED_IMAGE_TARGETS`] and returns
/// every match, highest preference first, so the reader can try each in turn.
/// For direct-PNG tools (pngpaste, PowerShell) enumeration is not available, so
/// it returns just `image/png` — those tools re-encode to PNG regardless of the
/// source format.
fn image_targets_for_tool(tool: &ClipboardTool) -> Vec<String> {
    let advertised = match tool {
        ClipboardTool::Xclip => list_targets_xclip(),
        ClipboardTool::WlPaste => list_targets_wl_paste(),
        ClipboardTool::PngPaste | ClipboardTool::PowerShellImage => None,
    };
    match advertised {
        Some(targets) => {
            let matched: Vec<String> = ACCEPTED_IMAGE_TARGETS
                .iter()
                .filter(|wanted| targets.iter().any(|t| t.eq_ignore_ascii_case(wanted)))
                .map(|t| t.to_string())
                .collect();
            if matched.is_empty() {
                // No advertised image target this attempt (owner shadowed it, or
                // the offer is not up yet). Return empty so the caller retries
                // the whole probe rather than reading a target that is not there.
                Vec::new()
            } else {
                matched
            }
        }
        None => vec!["image/png".to_string()],
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
    use std::cell::Cell;

    #[test]
    fn non_enumerating_tool_bails_after_one_empty_read() {
        // pngpaste / PowerShell: one empty read must return None without
        // paying the retry window.
        let reads = Cell::new(0u32);
        let waits = Cell::new(0u32);
        let out = read_clipboard_image_with(
            false,
            || vec!["image/png".to_string()],
            |_| {
                reads.set(reads.get() + 1);
                Some(Vec::new())
            },
            |_| waits.set(waits.get() + 1),
        );
        assert!(out.is_none());
        assert_eq!(reads.get(), 1, "must not re-read a non-enumerating tool");
        assert_eq!(waits.get(), 0, "must not pay the retry window");
    }

    #[test]
    fn enumerating_tool_with_no_image_returns_immediately() {
        // xclip / wl-paste advertising no image target: return without reading.
        let reads = Cell::new(0u32);
        let waits = Cell::new(0u32);
        let out = read_clipboard_image_with(
            true,
            Vec::new,
            |_| {
                reads.set(reads.get() + 1);
                Some(vec![1, 2, 3])
            },
            |_| waits.set(waits.get() + 1),
        );
        assert!(out.is_none());
        assert_eq!(reads.get(), 0, "no advertised image means no read");
        assert_eq!(waits.get(), 0);
    }

    #[test]
    fn enumerating_tool_retries_until_advertised_image_reads() {
        // Image is advertised but the first read races empty; a later attempt
        // succeeds. The bounded retry window recovers it.
        let attempt = Cell::new(0u32);
        let out = read_clipboard_image_with(
            true,
            || vec!["image/png".to_string()],
            |target| {
                let n = attempt.get();
                attempt.set(n + 1);
                if n < 2 {
                    Some(Vec::new())
                } else {
                    assert_eq!(target, "image/png");
                    Some(vec![0x89, b'P', b'N', b'G'])
                }
            },
            |_| {},
        );
        assert_eq!(
            out,
            Some((vec![0x89, b'P', b'N', b'G'], "image/png".to_string()))
        );
    }

    #[test]
    fn enumerating_tool_stops_at_bounded_attempts_when_unreadable() {
        // Image stays advertised but never reads; must stop at the bounded
        // attempt count, not spin forever.
        let reads = Cell::new(0u32);
        let waits = Cell::new(0u32);
        let out = read_clipboard_image_with(
            true,
            || vec!["image/png".to_string()],
            |_| {
                reads.set(reads.get() + 1);
                Some(Vec::new())
            },
            |_| waits.set(waits.get() + 1),
        );
        assert!(out.is_none());
        assert_eq!(reads.get(), IMAGE_READ_ATTEMPTS);
        assert_eq!(waits.get(), IMAGE_READ_ATTEMPTS);
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
        // image_targets_for_tool intersects advertised with ACCEPTED in
        // preference order; exercise the ordering directly.
        let advertised = ["image/jpeg".to_string(), "image/png".to_string()];
        let matched: Vec<&str> = ACCEPTED_IMAGE_TARGETS
            .iter()
            .copied()
            .filter(|w| advertised.iter().any(|t| t.eq_ignore_ascii_case(w)))
            .collect();
        assert_eq!(matched.first().copied(), Some("image/png"));
        assert_eq!(matched, vec!["image/png", "image/jpeg"]);
    }

    #[test]
    fn image_target_selection_falls_back_to_next_preference() {
        let advertised = ["image/gif".to_string(), "image/jpeg".to_string()];
        let matched: Vec<&str> = ACCEPTED_IMAGE_TARGETS
            .iter()
            .copied()
            .filter(|w| advertised.iter().any(|t| t.eq_ignore_ascii_case(w)))
            .collect();
        assert_eq!(matched.first().copied(), Some("image/jpeg"));
    }

    #[test]
    fn image_targets_direct_png_tools_return_png() {
        assert_eq!(
            image_targets_for_tool(&ClipboardTool::PngPaste),
            vec!["image/png".to_string()]
        );
        assert_eq!(
            image_targets_for_tool(&ClipboardTool::PowerShellImage),
            vec!["image/png".to_string()]
        );
    }

    #[test]
    fn tool_can_enumerate_matches_probing_tools() {
        assert!(tool_can_enumerate(&ClipboardTool::Xclip));
        assert!(tool_can_enumerate(&ClipboardTool::WlPaste));
        assert!(!tool_can_enumerate(&ClipboardTool::PngPaste));
        assert!(!tool_can_enumerate(&ClipboardTool::PowerShellImage));
    }

    #[test]
    fn mime_for_target_normalizes_jpg() {
        assert_eq!(mime_for_target("image/jpg"), "image/jpeg");
        assert_eq!(mime_for_target("IMAGE/PNG"), "image/png");
        assert_eq!(mime_for_target("image/webp"), "image/webp");
    }

    #[test]
    fn accepted_targets_stay_within_vision_pipeline_allowlist() {
        // Mirror of zeroclaw-providers::multimodal::ALLOWED_IMAGE_MIME_TYPES.
        // Offering a target the loader rejects turns a paste into an
        // UnsupportedMime error, so every accepted target must normalize into
        // this set. image/jpg is the one alias, normalized to image/jpeg.
        const PIPELINE_ALLOWED: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
        for target in ACCEPTED_IMAGE_TARGETS {
            let normalized = mime_for_target(target);
            assert!(
                PIPELINE_ALLOWED.contains(&normalized.as_str()),
                "clipboard offers {target} -> {normalized}, which the vision pipeline rejects"
            );
        }
    }
}
