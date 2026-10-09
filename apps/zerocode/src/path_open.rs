//! Recognize, open and reveal local file paths shown in the transcript.
//!
//! Detection only accepts paths that exist on this machine. That makes the
//! match precise enough to extend across spaces (`/Volumes/Work SSD/...`)
//! without turning ordinary prose into links. Opening hands the path to the
//! platform launcher, which would also *run* executables, app bundles and
//! scripts, so those are revealed in the file manager instead.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A path candidate may span at most this many whitespace-separated words.
const MAX_PATH_WORDS: usize = 8;
const MAX_PATH_BYTES: usize = 1024;
const EXISTS_CACHE_TTL: Duration = Duration::from_secs(5);
const EXISTS_CACHE_CAP: usize = 4096;

/// Extensions the platform launcher executes or installs instead of
/// displaying. Compared case-insensitively.
const LAUNCHING_EXTENSIONS: &[&str] = &[
    // macOS
    "app",
    "command",
    "tool",
    "terminal",
    "workflow",
    "action",
    "pkg",
    "mpkg",
    "scpt",
    "scptd",
    "applescript",
    "prefpane",
    "saver",
    "kext",
    "webloc",
    "inetloc",
    "fileloc",
    "jar",
    "sh",
    "bash",
    "zsh",
    "fish",
    "csh",
    "py",
    "rb",
    "pl",
    // Linux
    "desktop",
    "appimage",
    "run",
    // Windows
    "exe",
    "bat",
    "cmd",
    "com",
    "msi",
    "ps1",
    "lnk",
    "vbs",
    "scr",
    "url",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenOutcome {
    Opened,
    /// The path may run code when opened, so it was revealed instead.
    Revealed,
}

/// True for link targets produced by [`recognized_path_ranges`]. URL targets
/// always carry an `http(s)://` scheme, so the two never collide.
pub(crate) fn is_path_target(target: &str) -> bool {
    target.starts_with('/')
}

/// Find existing absolute (`/…`) and home-relative (`~/…`) paths in rendered
/// text. Returns `(start, end, target)` byte ranges where `target` is the
/// absolute path with `~` expanded and any `:line[:col]` suffix removed.
/// Ranges already claimed by `taken` (URLs) are skipped.
pub(crate) fn recognized_path_ranges(
    text: &str,
    taken: &[(usize, usize, String)],
) -> Vec<(usize, usize, String)> {
    recognized_path_ranges_with(text, taken, home_dir().as_deref(), &cached_exists)
}

fn recognized_path_ranges_with(
    text: &str,
    taken: &[(usize, usize, String)],
    home: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<(usize, usize, String)> {
    let mut ranges: Vec<(usize, usize, String)> = Vec::new();
    let mut search_from = 0;
    while let Some(found) = text[search_from..].find(['/', '~']) {
        let start = search_from + found;
        search_from = start + 1;
        let rest = &text[start..];
        let tilde = rest.starts_with("~/");
        if !tilde && !rest.starts_with('/') {
            continue;
        }
        let has_left_boundary = start == 0
            || text[..start].chars().next_back().is_some_and(|ch| {
                ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '<' | '\'' | '"' | '`')
            });
        if !has_left_boundary {
            continue;
        }
        if taken
            .iter()
            .chain(ranges.iter())
            .any(|(s, e, _)| start >= *s && start < *e)
        {
            continue;
        }
        if tilde && home.is_none() {
            continue;
        }

        let mut best: Option<(usize, String)> = None;
        for word_end in word_ends(text, start).take(MAX_PATH_WORDS) {
            if word_end - start > MAX_PATH_BYTES {
                break;
            }
            // A following URL or claimed range ends the candidate.
            if taken.iter().any(|(s, _, _)| *s > start && *s < word_end) {
                break;
            }
            let end = trim_trailing_punctuation(text, start, word_end);
            let candidate = &text[start..end];
            if candidate.len() < 2 || candidate.contains('\u{2026}') {
                continue;
            }
            let expand = |shown: &str| match (tilde, home) {
                (true, Some(home)) => format!("{}{}", home.trim_end_matches('/'), &shown[1..]),
                _ => shown.to_string(),
            };
            if exists(&expand(candidate)) {
                best = Some((end, expand(candidate)));
            } else if let Some(stripped) = strip_line_suffix(candidate)
                && exists(&expand(stripped))
            {
                best = Some((end, expand(stripped)));
            }
        }
        if let Some((end, target)) = best {
            search_from = end;
            ranges.push((start, end, target));
        }
    }
    ranges
}

/// Byte offsets where successive whitespace-separated words end, starting at
/// `start`. The first offset ends the word that contains `start`.
fn word_ends(text: &str, start: usize) -> impl Iterator<Item = usize> + '_ {
    let mut cursor = start;
    std::iter::from_fn(move || {
        if cursor >= text.len() {
            return None;
        }
        let rest = &text[cursor..];
        let word_start = cursor + (rest.len() - rest.trim_start().len());
        if word_start >= text.len() {
            return None;
        }
        let end = text[word_start..]
            .find(char::is_whitespace)
            .map_or(text.len(), |offset| word_start + offset);
        cursor = end;
        Some(end)
    })
}

fn trim_trailing_punctuation(text: &str, start: usize, mut end: usize) -> usize {
    while let Some(ch) = text[start..end].chars().next_back() {
        let trim = match ch {
            '.' | ',' | ';' | ':' | '\'' | '"' | '`' | '>' | '!' | '?' => true,
            ')' => unbalanced(&text[start..end], '(', ')'),
            ']' => unbalanced(&text[start..end], '[', ']'),
            '}' => unbalanced(&text[start..end], '{', '}'),
            _ => false,
        };
        if !trim {
            break;
        }
        end -= ch.len_utf8();
    }
    end
}

/// True when `text` closes more `close` brackets than it opens.
fn unbalanced(text: &str, open: char, close: char) -> bool {
    let count = |target: char| text.chars().filter(|ch| *ch == target).count();
    count(open) < count(close)
}

/// Strip a trailing `:line` or `:line:col` reference.
fn strip_line_suffix(candidate: &str) -> Option<&str> {
    let mut rest = candidate;
    for _ in 0..2 {
        let Some((head, tail)) = rest.rsplit_once(':') else {
            break;
        };
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        rest = head;
    }
    (rest.len() < candidate.len() && !rest.is_empty()).then_some(rest)
}

fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|home| home.starts_with('/'))
}

/// Existence checks run on render paths (including streaming frames), so
/// results are cached briefly to keep repeated frames off the filesystem.
fn cached_exists(path: &str) -> bool {
    static CACHE: Mutex<Option<HashMap<String, (bool, Instant)>>> = Mutex::new(None);
    let now = Instant::now();
    if let Ok(mut guard) = CACHE.lock() {
        let cache = guard.get_or_insert_with(HashMap::new);
        if let Some((exists, at)) = cache.get(path)
            && now.duration_since(*at) < EXISTS_CACHE_TTL
        {
            return *exists;
        }
        let exists = std::fs::metadata(path).is_ok();
        if cache.len() >= EXISTS_CACHE_CAP {
            cache.clear();
        }
        cache.insert(path.to_string(), (exists, now));
        return exists;
    }
    std::fs::metadata(path).is_ok()
}

/// True when handing `path` to the platform launcher could execute code:
/// app bundles, executable files, and script/installer extensions.
pub(crate) fn may_launch(path: &Path) -> bool {
    let has_launching_extension =
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                LAUNCHING_EXTENSIONS
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(ext))
            });
    if has_launching_extension {
        return true;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.is_dir() {
        // Any bundle (app, prefPane, plugin) carries Contents/Info.plist.
        return path.join("Contents").join("Info.plist").is_file();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchSpec {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

fn validate(path: &str) -> anyhow::Result<&Path> {
    if !is_path_target(path) {
        anyhow::bail!("only absolute paths can be opened");
    }
    if path.chars().any(char::is_control) {
        anyhow::bail!("path contains control characters");
    }
    let path = Path::new(path);
    if std::fs::metadata(path).is_err() {
        anyhow::bail!("{} does not exist", path.display());
    }
    Ok(path)
}

pub(crate) fn open_spec(path: &str) -> anyhow::Result<(LaunchSpec, OpenOutcome)> {
    let checked = validate(path)?;
    if may_launch(checked) {
        return Ok((reveal_spec(path)?, OpenOutcome::Revealed));
    }
    let spec = LaunchSpec {
        program: platform_program()?,
        args: vec![path.to_string()],
    };
    Ok((spec, OpenOutcome::Opened))
}

pub(crate) fn reveal_spec(path: &str) -> anyhow::Result<LaunchSpec> {
    let checked = validate(path)?;
    let program = platform_program()?;
    #[cfg(target_os = "macos")]
    let args = vec!["-R".to_string(), path.to_string()];
    #[cfg(target_os = "windows")]
    let args = vec![format!("/select,{path}")];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let args = {
        // xdg-open has no "select" mode; open the containing folder.
        let folder = if checked.is_dir() {
            checked
        } else {
            checked.parent().unwrap_or(checked)
        };
        vec![folder.to_string_lossy().into_owned()]
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let _ = checked;
    Ok(LaunchSpec { program, args })
}

fn platform_program() -> anyhow::Result<&'static str> {
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "explorer.exe";

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        Ok(program)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("opening files is unsupported on this platform")
    }
}

async fn spawn(spec: LaunchSpec) -> anyhow::Result<()> {
    let mut child = tokio::process::Command::new(spec.program)
        .args(spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// Open a path with the platform default app, or reveal it when opening
/// could run code.
pub(crate) async fn open(path: &str) -> anyhow::Result<OpenOutcome> {
    let (spec, outcome) = open_spec(path)?;
    spawn(spec).await?;
    Ok(outcome)
}

/// Show a path in the platform file manager.
pub(crate) async fn reveal(path: &str) -> anyhow::Result<()> {
    spawn(reveal_spec(path)?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(text: &str, existing: &[&str]) -> Vec<(usize, usize, String)> {
        recognized_path_ranges_with(text, &[], Some("/Users/me"), &|path| {
            existing.contains(&path)
        })
    }

    #[test]
    fn path_with_space_and_trailing_period_is_one_link() {
        let path = "/Volumes/Work SSD/Scratch/wt/.pr-evidence/shot.png";
        let text = format!("Image: {path}. It's committed.");
        let found = ranges(&text, &[path, "/Volumes/Work SSD"]);
        assert_eq!(found, vec![(7, 7 + path.len(), path.to_string())]);
    }

    #[test]
    fn prose_and_missing_paths_are_not_links() {
        assert!(ranges("and/or /nope here", &[]).is_empty());
        assert!(
            ranges("a/b/c", &["/b/c"]).is_empty(),
            "needs a left boundary"
        );
        assert!(ranges("/", &["/"]).is_empty(), "bare root is too short");
    }

    #[test]
    fn several_paths_on_one_line_stay_separate() {
        let found = ranges("see /tmp and /tmp/x, ok", &["/tmp", "/tmp/x"]);
        assert_eq!(
            found,
            vec![(4, 8, "/tmp".to_string()), (13, 19, "/tmp/x".to_string())]
        );
    }

    #[test]
    fn home_relative_paths_expand_but_keep_display_range() {
        let found = ranges("(`~/notes/a.md`)", &["/Users/me/notes/a.md"]);
        assert_eq!(found, vec![(2, 14, "/Users/me/notes/a.md".to_string())]);
        let no_home = recognized_path_ranges_with("~/a", &[], None, &|_| true);
        assert!(no_home.is_empty());
    }

    #[test]
    fn line_suffix_is_highlighted_but_not_part_of_the_target() {
        let found = ranges("at /src/main.rs:42:7 now", &["/src/main.rs"]);
        assert_eq!(found, vec![(3, 20, "/src/main.rs".to_string())]);
        let found = ranges("at /src/main.rs:42 now", &["/src/main.rs"]);
        assert_eq!(found, vec![(3, 18, "/src/main.rs".to_string())]);
    }

    #[test]
    fn urls_and_truncated_cells_are_never_paths() {
        let text = "https://example.com/a /b";
        let url = vec![(0, 21, "https://example.com/a".to_string())];
        let found = recognized_path_ranges_with(text, &url, None, &|path| path == "/b");
        assert_eq!(found, vec![(22, 24, "/b".to_string())]);
        assert!(ranges("/very/long/pa\u{2026}", &["/very/long/pa\u{2026}"]).is_empty());
    }

    #[test]
    fn launching_files_are_revealed_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("notes.md");
        std::fs::write(&doc, "hi").unwrap();
        let script = dir.path().join("run.command");
        std::fs::write(&script, "echo hi").unwrap();
        let bundle = dir.path().join("Thing.bundle");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents").join("Info.plist"), "").unwrap();

        assert!(!may_launch(&doc));
        assert!(!may_launch(dir.path()));
        assert!(may_launch(&script));
        assert!(may_launch(&bundle));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin = dir.path().join("tool");
            std::fs::write(&bin, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(may_launch(&bin));
        }

        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            let (_, outcome) = open_spec(doc.to_str().unwrap()).unwrap();
            assert_eq!(outcome, OpenOutcome::Opened);
            let (spec, outcome) = open_spec(script.to_str().unwrap()).unwrap();
            assert_eq!(outcome, OpenOutcome::Revealed);
            assert_eq!(spec, reveal_spec(script.to_str().unwrap()).unwrap());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reveal_uses_finder_select() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        let spec = reveal_spec(&path).unwrap();
        assert_eq!(spec.program, "/usr/bin/open");
        assert_eq!(spec.args, vec!["-R".to_string(), path]);
    }

    #[test]
    fn relative_or_missing_paths_are_rejected() {
        assert!(open_spec("relative/file").is_err());
        assert!(open_spec("/definitely/not/here/zc-path-open").is_err());
        assert!(reveal_spec("/bad\u{0007}path").is_err());
    }
}
