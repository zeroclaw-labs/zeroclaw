//! Git changed-line discovery and recursive Rust file collection.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Added or modified line ranges, keyed by repository-relative path.
#[derive(Debug, Default)]
pub struct ChangedSet {
    merge_base: String,
    files: BTreeSet<PathBuf>,
    ranges: BTreeMap<PathBuf, Vec<RangeInclusive<usize>>>,
    hunks: BTreeMap<PathBuf, Vec<DiffHunk>>,
    baseline_absent: BTreeSet<PathBuf>,
    baseline_paths: BTreeMap<PathBuf, PathBuf>,
}

#[derive(Clone, Copy, Debug)]
struct DiffHunk {
    old_count: usize,
    new_start: usize,
    new_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiffPath {
    old: Option<PathBuf>,
    new: Option<PathBuf>,
}

impl ChangedSet {
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.iter()
    }

    pub fn contains(&self, path: &Path, line: usize) -> bool {
        self.ranges
            .get(path)
            .is_some_and(|ranges| ranges.iter().any(|range| range.contains(&line)))
    }

    fn insert_range(&mut self, path: PathBuf, start: usize, count: usize) {
        if count == 0 {
            return;
        }
        self.files.insert(path.clone());
        self.ranges
            .entry(path)
            .or_default()
            .push(start..=start.saturating_add(count - 1));
    }

    fn insert_hunk(&mut self, path: PathBuf, hunk: DiffHunk) {
        self.files.insert(path.clone());
        self.hunks.entry(path).or_default().push(hunk);
    }

    fn register_diff_path(&mut self, diff_path: &DiffPath) {
        let Some(path) = &diff_path.new else {
            return;
        };
        if path.extension() != Some(OsStr::new("rs")) {
            return;
        }
        self.files.insert(path.clone());
        match &diff_path.old {
            Some(old_path) if old_path.extension() == Some(OsStr::new("rs")) => {
                if old_path != path {
                    self.baseline_paths.insert(path.clone(), old_path.clone());
                }
            }
            _ => {
                self.baseline_absent.insert(path.clone());
            }
        }
    }

    /// Map a current line back to its merge-base line when Git left it unchanged.
    pub fn old_line_for_new(&self, path: &Path, line: usize) -> Option<usize> {
        if self.baseline_absent.contains(path) {
            return None;
        }
        let mut mapped = i64::try_from(line).ok()?;
        for hunk in self.hunks.get(path).into_iter().flatten() {
            let new_end = hunk
                .new_start
                .saturating_add(hunk.new_count.saturating_sub(1));
            if hunk.new_count > 0 && (hunk.new_start..=new_end).contains(&line) {
                return None;
            }
            let follows_hunk = if hunk.new_count == 0 {
                line > hunk.new_start
            } else {
                line > new_end
            };
            if !follows_hunk {
                break;
            }
            mapped += i64::try_from(hunk.old_count).ok()? - i64::try_from(hunk.new_count).ok()?;
        }
        usize::try_from(mapped).ok().filter(|line| *line > 0)
    }

    /// Read this path as it existed at the merge-base, or `None` for a new file.
    pub fn baseline_source(&self, repo: &Path, path: &Path) -> Result<Option<String>, String> {
        if self.baseline_absent.contains(path) {
            return Ok(None);
        }
        let object = format!("{}:{}", self.merge_base, self.baseline_path(path).display());
        git_output(repo, ["show", object.as_str()]).map(Some)
    }

    /// Return the merge-base path, which may differ after a Git rename.
    pub fn baseline_path<'a>(&'a self, path: &'a Path) -> &'a Path {
        self.baseline_paths
            .get(path)
            .map(PathBuf::as_path)
            .unwrap_or(path)
    }
}

/// Find changed Rust paths and line mappings since the merge-base of `base` and HEAD.
///
/// The diff includes staged and unstaged changes. Untracked Rust files are treated
/// as entirely new so a local gate cannot silently skip them.
pub fn changed_rust_lines(
    repo: &Path,
    base: &str,
    roots: &[PathBuf],
) -> Result<ChangedSet, String> {
    let merge_base = git_output(repo, ["merge-base", "--", base, "HEAD"])?;
    let merge_base = merge_base.trim();
    if merge_base.is_empty() {
        return Err(format!("git merge-base returned no commit for {base}"));
    }

    let mut names = Command::new("git");
    names.current_dir(repo).args([
        "-c",
        "core.quotePath=false",
        "diff",
        "--name-status",
        "-z",
        "--find-renames",
        "--relative",
        merge_base,
        "--",
    ]);
    names.args(roots);
    let output = names
        .output()
        .map_err(|error| format!("failed to list changed paths: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff --name-status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let diff_paths = parse_name_status_z(&output.stdout)?;

    let mut diff = Command::new("git");
    diff.current_dir(repo).args([
        "-c",
        "core.quotePath=false",
        "diff",
        "--unified=0",
        "--no-ext-diff",
        "--find-renames",
        "--relative",
        merge_base,
        "--",
    ]);
    diff.args(roots);
    let output = diff
        .output()
        .map_err(|error| format!("failed to run git diff: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut changed = parse_unified_zero_diff_with_paths(
        &String::from_utf8_lossy(&output.stdout),
        Some(&diff_paths),
    )?;
    changed.merge_base = merge_base.to_owned();

    let mut untracked = Command::new("git");
    untracked
        .current_dir(repo)
        .args(["ls-files", "-z", "--others", "--exclude-standard", "--"])
        .args(roots);
    let output = untracked
        .output()
        .map_err(|error| format!("failed to list untracked files: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    for raw_path in output.stdout.split(|byte| *byte == 0) {
        if raw_path.is_empty() {
            continue;
        }
        let path = PathBuf::from(
            std::str::from_utf8(raw_path)
                .map_err(|_| "git reported a non-UTF-8 untracked path".to_string())?,
        );
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }
        let count = fs::read_to_string(repo.join(&path))
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?
            .lines()
            .count()
            .max(1);
        changed.baseline_absent.insert(path.clone());
        changed.insert_range(path, 1, count);
    }

    Ok(changed)
}

/// Recursively collect Rust files below the requested repository-relative roots.
pub fn collect_rust_files(repo: &Path, roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for root in roots {
        collect_path(repo, root, &mut files)?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn collect_path(repo: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let absolute = repo.join(relative);
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("failed to inspect {}: {error}", relative.display())),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        if relative.extension() == Some(OsStr::new("rs")) {
            files.push(relative.to_path_buf());
        }
        return Ok(());
    }
    if should_skip_dir(relative) {
        return Ok(());
    }
    let entries = fs::read_dir(&absolute)
        .map_err(|error| format!("failed to read {}: {error}", relative.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read an entry below {}: {error}",
                relative.display()
            )
        })?;
        collect_path(repo, &relative.join(entry.file_name()), files)?;
    }
    Ok(())
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some(".git" | "target" | "node_modules" | "vendor" | "book" | "dist")
        )
    })
}

fn git_output<'a>(repo: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
fn parse_unified_zero_diff(diff: &str) -> Result<ChangedSet, String> {
    parse_unified_zero_diff_with_paths(diff, None)
}

fn parse_unified_zero_diff_with_paths(
    diff: &str,
    diff_paths: Option<&[DiffPath]>,
) -> Result<ChangedSet, String> {
    let mut changed = ChangedSet::default();
    let mut current_path = None;
    let mut baseline_absent = false;
    let mut rename_from = None;
    let mut diff_path_index = 0;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            current_path = if let Some(diff_paths) = diff_paths {
                let diff_path = diff_paths.get(diff_path_index).ok_or_else(|| {
                    "git patch contained more files than its raw path list".to_string()
                })?;
                diff_path_index += 1;
                changed.register_diff_path(diff_path);
                diff_path
                    .new
                    .as_ref()
                    .filter(|path| path.extension() == Some(OsStr::new("rs")))
                    .cloned()
            } else {
                None
            };
            baseline_absent = false;
            rename_from = None;
            continue;
        }
        if diff_paths.is_some() && (line.starts_with("+++ ") || line.starts_with("rename ")) {
            continue;
        }
        if let Some(path) = line.strip_prefix("rename from ") {
            rename_from = Some(PathBuf::from(path));
            continue;
        }
        if let Some(path) = line.strip_prefix("rename to ") {
            let path = PathBuf::from(path);
            if path.extension() == Some(OsStr::new("rs")) {
                changed.files.insert(path.clone());
                match rename_from.take() {
                    Some(old_path) if old_path.extension() == Some(OsStr::new("rs")) => {
                        changed.baseline_paths.insert(path.clone(), old_path);
                    }
                    _ => {
                        changed.baseline_absent.insert(path.clone());
                    }
                }
            }
            current_path = Some(path);
            continue;
        }
        if line == "--- /dev/null" {
            baseline_absent = true;
            continue;
        }
        if line == "+++ /dev/null" {
            current_path = None;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ b/") {
            let path = PathBuf::from(path);
            if baseline_absent {
                changed.baseline_absent.insert(path.clone());
            }
            current_path = Some(path);
            continue;
        }
        if !line.starts_with("@@ ") {
            continue;
        }
        let Some(path) = current_path.clone() else {
            continue;
        };
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }
        let minus = line
            .split_whitespace()
            .find(|part| part.starts_with('-'))
            .ok_or_else(|| format!("malformed diff hunk: {line}"))?;
        let plus = line
            .split_whitespace()
            .find(|part| part.starts_with('+'))
            .ok_or_else(|| format!("malformed diff hunk: {line}"))?;
        let (_, old_count) = parse_hunk_range(minus, '-', line)?;
        let (new_start, new_count) = parse_hunk_range(plus, '+', line)?;
        changed.insert_hunk(
            path.clone(),
            DiffHunk {
                old_count,
                new_start,
                new_count,
            },
        );
        changed.insert_range(path, new_start, new_count);
    }
    if let Some(diff_paths) = diff_paths
        && diff_path_index != diff_paths.len()
    {
        return Err("git raw path list contained files missing from its patch".to_string());
    }
    Ok(changed)
}

fn parse_name_status_z(output: &[u8]) -> Result<Vec<DiffPath>, String> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut paths = Vec::new();
    while let Some(status) = fields.next() {
        let status = status
            .first()
            .copied()
            .ok_or_else(|| "git emitted an empty change status".to_string())?;
        let next_path = |fields: &mut dyn Iterator<Item = &[u8]>| {
            fields
                .next()
                .ok_or_else(|| "git change status omitted a path".to_string())
                .and_then(|path| {
                    std::str::from_utf8(path)
                        .map(PathBuf::from)
                        .map_err(|_| "git reported a non-UTF-8 changed path".to_string())
                })
        };
        let diff_path = match status {
            b'R' => DiffPath {
                old: Some(next_path(&mut fields)?),
                new: Some(next_path(&mut fields)?),
            },
            b'C' => {
                let _source = next_path(&mut fields)?;
                DiffPath {
                    old: None,
                    new: Some(next_path(&mut fields)?),
                }
            }
            b'A' => DiffPath {
                old: None,
                new: Some(next_path(&mut fields)?),
            },
            b'D' => DiffPath {
                old: Some(next_path(&mut fields)?),
                new: None,
            },
            _ => {
                let path = next_path(&mut fields)?;
                DiffPath {
                    old: Some(path.clone()),
                    new: Some(path),
                }
            }
        };
        paths.push(diff_path);
    }
    Ok(paths)
}

fn parse_hunk_range(value: &str, prefix: char, hunk: &str) -> Result<(usize, usize), String> {
    let range = value
        .strip_prefix(prefix)
        .ok_or_else(|| format!("malformed diff hunk: {hunk}"))?;
    match range.split_once(',') {
        Some((start, count)) => Ok((parse_number(start, hunk)?, parse_number(count, hunk)?)),
        None => Ok((parse_number(range, hunk)?, 1)),
    }
}

fn parse_number(value: &str, hunk: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("malformed diff hunk: {hunk}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn parses_only_added_and_modified_rust_lines() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -2,0 +3,2 @@\n\
+a\n\
+b\n\
@@ -10 +12 @@\n\
-old\n\
+new\n\
diff --git a/README.md b/README.md\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1 @@\n";
        let changed = parse_unified_zero_diff(diff).expect("fixture diff should parse");
        assert!(changed.contains(Path::new("src/lib.rs"), 3));
        assert!(changed.contains(Path::new("src/lib.rs"), 4));
        assert!(changed.contains(Path::new("src/lib.rs"), 12));
        assert!(!changed.contains(Path::new("src/lib.rs"), 11));
        assert!(!changed.contains(Path::new("README.md"), 1));
        assert_eq!(
            changed.old_line_for_new(Path::new("src/lib.rs"), 5),
            Some(3)
        );
        assert_eq!(
            changed.old_line_for_new(Path::new("src/lib.rs"), 13),
            Some(11)
        );
        assert_eq!(changed.old_line_for_new(Path::new("src/lib.rs"), 3), None);
    }

    #[test]
    fn deletion_only_hunks_keep_the_file_and_map_shifted_lines() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -2 +1,0 @@\n\
-// SAFETY: prior justification\n";
        let changed = parse_unified_zero_diff(diff).expect("fixture diff should parse");
        assert_eq!(
            changed.files().map(PathBuf::as_path).collect::<Vec<_>>(),
            [Path::new("src/lib.rs")]
        );
        assert!(!changed.contains(Path::new("src/lib.rs"), 1));
        assert_eq!(
            changed.old_line_for_new(Path::new("src/lib.rs"), 1),
            Some(1)
        );
        assert_eq!(
            changed.old_line_for_new(Path::new("src/lib.rs"), 2),
            Some(3)
        );
    }

    #[test]
    fn rejects_malformed_hunks() {
        let error = parse_unified_zero_diff("+++ b/src/lib.rs\n@@ -1 +wat @@")
            .expect_err("malformed hunk must fail closed");
        assert!(error.contains("malformed diff hunk"));
    }

    #[test]
    fn nul_name_status_preserves_control_characters_and_renames() {
        let paths = parse_name_status_z(
            b"M\0src/tab\tname.rs\0R100\0tests/old\nname.rs\0src/new\nname.rs\0",
        )
        .expect("raw names should parse");
        assert_eq!(
            paths,
            [
                DiffPath {
                    old: Some(PathBuf::from("src/tab\tname.rs")),
                    new: Some(PathBuf::from("src/tab\tname.rs")),
                },
                DiffPath {
                    old: Some(PathBuf::from("tests/old\nname.rs")),
                    new: Some(PathBuf::from("src/new\nname.rs")),
                },
            ]
        );
    }

    #[test]
    fn non_utf8_changed_paths_fail_closed() {
        let error = parse_name_status_z(b"M\0src/invalid-\xff.rs\0")
            .expect_err("non-UTF-8 paths must not be skipped");
        assert!(error.contains("non-UTF-8"));
    }

    #[test]
    fn discovers_worktree_and_untracked_lines_from_git() {
        let repo = tempfile::tempdir().expect("temporary repo should be created");
        fs::create_dir(repo.path().join("src")).expect("src should be created");
        fs::write(repo.path().join("src/lib.rs"), "fn original() {}\n")
            .expect("tracked fixture should be written");
        run_git(repo.path(), &["init", "-q"]);
        run_git(repo.path(), &["add", "src/lib.rs"]);
        run_git(
            repo.path(),
            &[
                "-c",
                "user.name=anti-slop-test",
                "-c",
                "user.email=anti-slop@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--no-gpg-sign",
                "-qm",
                "fixture",
            ],
        );

        fs::write(
            repo.path().join("src/lib.rs"),
            "fn original() {}\nfn changed() {}\n",
        )
        .expect("tracked change should be written");
        fs::write(repo.path().join("src/new.rs"), "fn new_file() {}\n")
            .expect("untracked fixture should be written");

        let changed = changed_rust_lines(repo.path(), "HEAD", &[PathBuf::from("src")])
            .expect("git changes should be discovered");
        assert!(changed.contains(Path::new("src/lib.rs"), 2));
        assert!(changed.contains(Path::new("src/new.rs"), 1));
        assert!(!changed.contains(Path::new("src/lib.rs"), 1));
    }

    #[test]
    fn repository_root_discovers_future_rust_directories() {
        let repo = tempfile::tempdir().expect("temporary repo should be created");
        fs::create_dir_all(repo.path().join("examples/future"))
            .expect("future source directory should be created");
        fs::create_dir(repo.path().join("target")).expect("target should be created");
        fs::write(
            repo.path().join("examples/future/demo.rs"),
            "fn demo() {}\n",
        )
        .expect("Rust fixture should be written");
        fs::write(
            repo.path().join("target/generated.rs"),
            "fn generated() {}\n",
        )
        .expect("generated fixture should be written");

        let files = collect_rust_files(repo.path(), &[PathBuf::from(".")])
            .expect("repository source should be collected");
        assert!(
            files
                .iter()
                .any(|path| path.ends_with("examples/future/demo.rs"))
        );
        assert!(
            !files
                .iter()
                .any(|path| path.ends_with("target/generated.rs"))
        );
    }

    #[test]
    fn deleting_a_justification_exposes_a_new_baseline_delta() {
        let repo = tempfile::tempdir().expect("temporary repo should be created");
        fs::create_dir(repo.path().join("src")).expect("src should be created");
        let path = Path::new("src/lib.rs");
        fs::write(
            repo.path().join(path),
            "fn boundary() {\n    // SAFETY: this fixture owns the invariant.\n    unsafe { std::hint::unreachable_unchecked() }\n}\n",
        )
        .expect("tracked fixture should be written");
        run_git(repo.path(), &["init", "-q"]);
        run_git(repo.path(), &["add", "src/lib.rs"]);
        run_git(
            repo.path(),
            &[
                "-c",
                "user.name=anti-slop-test",
                "-c",
                "user.email=anti-slop@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--no-gpg-sign",
                "-qm",
                "fixture",
            ],
        );

        let current = "fn boundary() {\n    unsafe { std::hint::unreachable_unchecked() }\n}\n";
        fs::write(repo.path().join(path), current).expect("comment should be deleted");

        let changed = changed_rust_lines(repo.path(), "HEAD", &[PathBuf::from("src")])
            .expect("git changes should be discovered");
        assert_eq!(
            changed.files().map(PathBuf::as_path).collect::<Vec<_>>(),
            [path]
        );
        let baseline = changed
            .baseline_source(repo.path(), path)
            .expect("baseline should be readable")
            .expect("tracked file should have a baseline");
        assert!(
            crate::check_source(path, &baseline)
                .expect("baseline should parse")
                .is_empty()
        );
        let diagnostic = crate::check_source(path, current)
            .expect("current source should parse")
            .into_iter()
            .find(|diagnostic| diagnostic.rule == "require-safety-comment-for-unsafe")
            .expect("deleting the comment should expose the unsafe diagnostic");
        assert_eq!(changed.old_line_for_new(path, diagnostic.line), Some(3));
    }
}
