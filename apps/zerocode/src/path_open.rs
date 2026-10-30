//! Recognize, open and reveal local file paths shown in the transcript.
//!
//! Detection only accepts paths that exist on this machine. That makes the
//! match precise enough to extend across spaces (`/Volumes/Work SSD/...`)
//! without turning ordinary prose into links. Opening hands the path to the
//! platform launcher, which would also *run* executables, app bundles and
//! scripts, so those are revealed in the file manager instead.
//!
//! Paths are not restricted to the workspace. This launch gate is not a
//! filesystem sandbox. Links are enabled only for local sessions and are
//! disabled on Windows. Remote session paths must never be resolved against
//! the client's files.
//!
//! # Security
//!
//! Detection runs background `metadata` probes without a click. Known
//! host-keyed automount roots (`/net`, `/Network`, `/automount`) are excluded
//! before any probe. Other mounted filesystems, custom automount maps and
//! symlinks into them can still cause filesystem or network I/O during a
//! probe. No opener is spawned until an explicit user action.
//!
//! Opening and revealing canonicalize the target first. The launch check and
//! opener use that same canonical path, so a harmless symlink name cannot
//! conceal a launching extension. Finder aliases are not filesystem symlinks;
//! canonicalization does not resolve them, and alias handling remains a
//! follow-up. File replacement between validation and launch remains possible.
//!
//! # Rollback
//!
//! This high-risk interaction is confined to ZeroCode and changes no wire
//! protocol or configuration. Reverting the feature's squash commit restores
//! URL-only links without a data migration.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// A path candidate may span at most this many whitespace-separated words.
const MAX_PATH_WORDS: usize = 8;
const MAX_PATH_BYTES: usize = 1024;
const EXISTS_CACHE_TTL: Duration = Duration::from_secs(5);
const EXISTS_CACHE_CAP: usize = 4096;
/// Existence checks running at once. A hung network mount parks one thread
/// per distinct path, so this bounds the damage.
const MAX_PENDING_CHECKS: usize = 32;

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
    "mobileconfig",
    "dmg",
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
    "pyw",
    "pyz",
    "jnlp",
    "rb",
    "pl",
    // Linux
    "desktop",
    "appimage",
    "run",
    "deb",
    "rpm",
    "flatpakref",
    "snap",
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
    if cfg!(target_os = "windows") {
        return Vec::new();
    }
    recognized_path_ranges_with(text, taken, home_dir().as_deref(), &|path| {
        EXISTS_PROBE.lookup(path)
    })
}

/// Bumped whenever a background existence check changes whether a path is a
/// link. Panes compare it each tick and rebuild the transcript when it moves.
pub(crate) fn path_links_generation() -> u64 {
    EXISTS_PROBE.generation.load(Ordering::Acquire)
}

/// Wrap `path` in a Markdown code span that survives backticks inside it.
pub(crate) fn markdown_code_span(path: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for ch in path.chars() {
        run = if ch == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let pad = if path.starts_with('`') || path.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{path}{pad}{fence}")
}

fn recognized_path_ranges_with(
    text: &str,
    taken: &[(usize, usize, String)],
    home: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<(usize, usize, String)> {
    let probe_exists = |path: &str| !is_automount_path(path) && exists(path);
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
            let expand = |shown: &str| match (tilde, home) {
                (true, Some(home)) => format!("{}{}", home.trim_end_matches('/'), &shown[1..]),
                _ => shown.to_string(),
            };
            let end = trim_trailing_punctuation(text, start, word_end);
            // A real name may end in punctuation (`report!`), so the exact
            // word wins over its sentence-trimmed form.
            let exact = &text[start..word_end];
            if end < word_end && !exact.contains('\u{2026}') && probe_exists(&expand(exact)) {
                best = Some((word_end, expand(exact)));
                continue;
            }
            let candidate = &text[start..end];
            if candidate.len() < 2 || candidate.contains('\u{2026}') {
                continue;
            }
            if probe_exists(&expand(candidate)) {
                best = Some((end, expand(candidate)));
            } else if let Some(stripped) = strip_line_suffix(candidate)
                && probe_exists(&expand(stripped))
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

/// Inspect components without filesystem access. Check every root-level
/// component reached by `..` too; the kernel traverses it before moving back.
fn is_automount_path(path: &str) -> bool {
    let mut depth = 0usize;
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => {
                if depth == 0
                    && name.to_str().is_some_and(|name| {
                        ["net", "network", "automount"]
                            .iter()
                            .any(|root| name.eq_ignore_ascii_case(root))
                    })
                {
                    return true;
                }
                depth += 1;
            }
            Component::ParentDir => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
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

static EXISTS_PROBE: LazyLock<ExistsProbe> =
    LazyLock::new(|| ExistsProbe::new(Box::new(|path| std::fs::metadata(path).is_ok())));

/// Existence checks for the draw path. Detection runs while rendering, and a
/// `metadata` call on a slow or disconnected mount can block for seconds, so
/// lookups never touch the filesystem: they answer from the cache (stale
/// entries included) and hand misses to a background thread. A finished check
/// that changes the answer bumps `generation` so the UI redraws.
struct ExistsProbe {
    state: Mutex<ExistsState>,
    generation: AtomicU64,
    check: Box<dyn Fn(&str) -> bool + Send + Sync>,
}

#[derive(Default)]
struct ExistsState {
    entries: HashMap<String, (bool, Instant)>,
    pending: HashSet<String>,
    /// A lookup was skipped because every slot was busy. The next finished
    /// check bumps `generation` so the skipped path is asked for again.
    starved: bool,
}

impl ExistsProbe {
    fn new(check: Box<dyn Fn(&str) -> bool + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(ExistsState::default()),
            generation: AtomicU64::new(0),
            check,
        }
    }

    fn lookup(&'static self, path: &str) -> bool {
        if is_automount_path(path) {
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let cached = state.entries.get(path).copied();
        if let Some((exists, at)) = cached
            && at.elapsed() < EXISTS_CACHE_TTL
        {
            return exists;
        }
        if !state.pending.contains(path) && state.pending.len() >= MAX_PENDING_CHECKS {
            state.starved = true;
        } else if !state.pending.contains(path) {
            state.pending.insert(path.to_string());
            let owned = path.to_string();
            let spawned = std::thread::Builder::new()
                .name("zc-path-exists".to_string())
                .spawn(move || {
                    let exists = (self.check)(&owned);
                    self.finish(owned, exists);
                });
            if spawned.is_err() {
                state.pending.remove(path);
            }
        }
        cached.is_some_and(|(exists, _)| exists)
    }

    fn finish(&self, path: String, exists: bool) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.pending.remove(&path);
        let shown = state.entries.get(&path).is_some_and(|(shown, _)| *shown);
        if state.entries.len() >= EXISTS_CACHE_CAP {
            state.entries.clear();
        }
        state.entries.insert(path, (exists, Instant::now()));
        let starved = std::mem::take(&mut state.starved);
        if shown != exists || starved {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[cfg(test)]
    fn wait_idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .state
            .lock()
            .is_ok_and(|state| !state.pending.is_empty())
        {
            assert!(
                Instant::now() < deadline,
                "path existence checks never finished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Block until background existence checks settle, so tests can render twice
/// and see real files linked.
#[cfg(test)]
pub(crate) fn wait_for_path_checks() {
    EXISTS_PROBE.wait_idle();
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

fn validate(path: &str) -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "windows") {
        anyhow::bail!("opening transcript paths is unsupported on Windows");
    }
    if !is_path_target(path) {
        anyhow::bail!("only absolute paths can be opened");
    }
    if path.chars().any(char::is_control) {
        anyhow::bail!("path contains control characters");
    }
    let canonical = std::fs::canonicalize(path)?;
    if canonical.to_str().is_none() || canonical.to_string_lossy().chars().any(char::is_control) {
        anyhow::bail!("canonical path is not valid display text");
    }
    Ok(canonical)
}

pub(crate) fn open_spec(path: &str) -> anyhow::Result<(LaunchSpec, OpenOutcome)> {
    let checked = validate(path)?;
    if may_launch(&checked) {
        return Ok((reveal_checked_spec(&checked)?, OpenOutcome::Revealed));
    }
    let spec = LaunchSpec {
        program: platform_program()?,
        args: vec![checked.to_string_lossy().into_owned()],
    };
    Ok((spec, OpenOutcome::Opened))
}

pub(crate) fn reveal_spec(path: &str) -> anyhow::Result<LaunchSpec> {
    let checked = validate(path)?;
    reveal_checked_spec(&checked)
}

fn reveal_checked_spec(checked: &Path) -> anyhow::Result<LaunchSpec> {
    let program = platform_program()?;
    #[cfg(target_os = "macos")]
    let args = vec!["-R".to_string(), checked.to_string_lossy().into_owned()];
    #[cfg(target_os = "windows")]
    let args = vec![format!("/select,{}", checked.display())];
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
    fn exact_name_ending_in_punctuation_beats_the_trimmed_one() {
        let found = ranges("open /tmp/report! now", &["/tmp/report!", "/tmp/report"]);
        assert_eq!(found, vec![(5, 17, "/tmp/report!".to_string())]);
        let found = ranges("open /tmp/report! now", &["/tmp/report"]);
        assert_eq!(found, vec![(5, 16, "/tmp/report".to_string())]);
    }

    #[cfg(not(windows))]
    #[test]
    fn automount_candidates_never_reach_the_existence_check() {
        for path in [
            "/net/host/file",
            "/Network/host/file",
            "/automount/host/file",
            "/net",
            "//net/host/file",
            "/./Network/host/file",
            "/tmp/../net/host/file",
            "/net/../tmp/file",
            "/NETWORK/host/file:42",
        ] {
            let found = recognized_path_ranges_with(path, &[], None, &|candidate| {
                panic!("automount candidate was probed: {candidate}")
            });
            assert!(found.is_empty(), "automount path linked: {path}");
        }
        assert!(
            recognized_path_ranges_with("~/file", &[], Some("/net/host"), &|_| {
                panic!("home-relative automount candidate was probed")
            })
            .is_empty()
        );
        assert_eq!(ranges("/networking/file", &["/networking/file"]).len(), 1);
    }

    #[test]
    fn automount_lookup_never_schedules_a_background_probe() {
        let probe: &'static ExistsProbe = Box::leak(Box::new(ExistsProbe::new(Box::new(|path| {
            panic!("automount lookup scheduled a check: {path}")
        }))));
        assert!(!probe.lookup("/net/host/file"));
        let state = probe.state.lock().unwrap();
        assert!(state.pending.is_empty());
        assert!(state.entries.is_empty());
    }

    #[test]
    fn existence_lookup_never_waits_on_the_filesystem() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let probe: &'static ExistsProbe =
            Box::leak(Box::new(ExistsProbe::new(Box::new(move |_| {
                let _ = release_rx.lock().unwrap().recv();
                true
            }))));

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || done_tx.send(probe.lookup("/slow/mount/file")));
        let first = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("lookup blocked on a hung filesystem check");
        assert!(!first, "unknown paths are not links until checked");
        assert_eq!(probe.generation.load(Ordering::Acquire), 0);

        release_tx.send(()).unwrap();
        probe.wait_idle();
        assert_eq!(probe.generation.load(Ordering::Acquire), 1);
        assert!(probe.lookup("/slow/mount/file"));
    }

    #[test]
    fn a_lookup_skipped_while_busy_triggers_a_retry() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let probe: &'static ExistsProbe =
            Box::leak(Box::new(ExistsProbe::new(Box::new(move |path| {
                if path.starts_with("/slow/") {
                    let _ = release_rx.lock().unwrap().recv();
                    return false;
                }
                true
            }))));
        for idx in 0..MAX_PENDING_CHECKS {
            assert!(!probe.lookup(&format!("/slow/{idx}")));
        }
        assert!(!probe.lookup("/fast/file"), "every slot is busy");

        for _ in 0..MAX_PENDING_CHECKS {
            release_tx.send(()).unwrap();
        }
        probe.wait_idle();
        assert!(
            probe.generation.load(Ordering::Acquire) > 0,
            "a freed slot must ask the pane to retry the skipped path"
        );
        assert!(!probe.lookup("/fast/file"), "retry schedules the check");
        probe.wait_idle();
        assert!(probe.lookup("/fast/file"));
    }

    #[test]
    fn code_span_outgrows_backticks_in_the_path() {
        assert_eq!(markdown_code_span("/tmp/a.txt"), "`/tmp/a.txt`");
        assert_eq!(markdown_code_span("/tmp/a`b.txt"), "``/tmp/a`b.txt``");
        assert_eq!(markdown_code_span("/tmp/a``"), "``` /tmp/a`` ```");
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

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (_, outcome) = open_spec(doc.to_str().unwrap()).unwrap();
            assert_eq!(outcome, OpenOutcome::Opened);
            let (spec, outcome) = open_spec(script.to_str().unwrap()).unwrap();
            assert_eq!(outcome, OpenOutcome::Revealed);
            assert_eq!(spec, reveal_spec(script.to_str().unwrap()).unwrap());
        }
    }

    #[test]
    fn interpreter_and_installer_extensions_are_launching_targets() {
        let dir = tempfile::tempdir().unwrap();
        for extension in [
            "pyw",
            "pyz",
            "jnlp",
            "mobileconfig",
            "dmg",
            "deb",
            "rpm",
            "flatpakref",
            "snap",
        ] {
            for extension in [extension.to_string(), extension.to_uppercase()] {
                let target = dir.path().join(format!("file.{extension}"));
                std::fs::write(&target, "fixture").unwrap();
                assert!(may_launch(&target));
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                assert_eq!(
                    open_spec(target.to_str().unwrap()).unwrap().1,
                    OpenOutcome::Revealed
                );
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn symlink_launch_checks_and_opener_use_the_canonical_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("notes.md");
        for extension in ["command", "terminal", "fileloc", "webloc", "inetloc"] {
            let target = dir.path().join(format!("launch.{extension}"));
            std::fs::write(&target, "fixture").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
            symlink(&target, &link).unwrap();

            let (spec, outcome) = open_spec(link.to_str().unwrap()).unwrap();
            assert_eq!(outcome, OpenOutcome::Revealed, "extension: {extension}");
            let canonical = std::fs::canonicalize(&target).unwrap();
            assert_eq!(spec, reveal_checked_spec(&canonical).unwrap());
            assert_eq!(spec, reveal_spec(link.to_str().unwrap()).unwrap());
            std::fs::remove_file(&link).unwrap();
        }

        let doc = dir.path().join("document.md");
        std::fs::write(&doc, "fixture").unwrap();
        symlink(&doc, &link).unwrap();
        let (spec, outcome) = open_spec(link.to_str().unwrap()).unwrap();
        assert_eq!(outcome, OpenOutcome::Opened);
        assert_eq!(
            spec.args,
            vec![
                std::fs::canonicalize(&doc)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            ]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_never_links_or_opens_transcript_paths() {
        assert!(recognized_path_ranges("/net/host/file /Windows", &[]).is_empty());
        assert!(open_spec("/Windows").is_err());
        assert!(reveal_spec("/Windows").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reveal_uses_finder_select() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        let spec = reveal_spec(&path).unwrap();
        assert_eq!(spec.program, "/usr/bin/open");
        assert_eq!(
            spec.args,
            vec![
                "-R".to_string(),
                std::fs::canonicalize(path)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            ]
        );
    }

    #[test]
    fn relative_or_missing_paths_are_rejected() {
        assert!(open_spec("relative/file").is_err());
        assert!(open_spec("/definitely/not/here/zc-path-open").is_err());
        assert!(reveal_spec("/bad\u{0007}path").is_err());
    }
}
