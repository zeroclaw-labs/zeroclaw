//! Git changed-line discovery and recursive Rust file collection.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Added or modified line ranges, keyed by repository-relative path.
#[derive(Debug, Default)]
pub struct ChangedSet {
    ranges: BTreeMap<PathBuf, Vec<RangeInclusive<usize>>>,
}

impl ChangedSet {
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.ranges.keys()
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
        self.ranges
            .entry(path)
            .or_default()
            .push(start..=start.saturating_add(count - 1));
    }
}

/// Find added and modified Rust lines since the merge-base of `base` and HEAD.
///
/// The diff includes staged and unstaged changes. Untracked Rust files are treated
/// as entirely new so a local gate cannot silently skip them.
pub fn changed_rust_lines(
    repo: &Path,
    base: &str,
    roots: &[PathBuf],
) -> Result<ChangedSet, String> {
    let merge_base = git_output(repo, ["merge-base", base, "HEAD"])?;
    let merge_base = merge_base.trim();
    if merge_base.is_empty() {
        return Err(format!("git merge-base returned no commit for {base}"));
    }

    let mut diff = Command::new("git");
    diff.current_dir(repo).args([
        "-c",
        "core.quotePath=false",
        "diff",
        "--unified=0",
        "--no-ext-diff",
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
    let mut changed = parse_unified_zero_diff(&String::from_utf8_lossy(&output.stdout))?;

    let mut untracked = Command::new("git");
    untracked
        .current_dir(repo)
        .args(["ls-files", "--others", "--exclude-standard", "--"])
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
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let path = PathBuf::from(line);
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }
        let count = fs::read_to_string(repo.join(&path))
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?
            .lines()
            .count()
            .max(1);
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

fn parse_unified_zero_diff(diff: &str) -> Result<ChangedSet, String> {
    let mut changed = ChangedSet::default();
    let mut current_path = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_path = (path != "/dev/null").then(|| PathBuf::from(path));
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
        let plus = line
            .split_whitespace()
            .find(|part| part.starts_with('+'))
            .ok_or_else(|| format!("malformed diff hunk: {line}"))?;
        let range = plus.trim_start_matches('+');
        let (start, count) = match range.split_once(',') {
            Some((start, count)) => (parse_number(start, line)?, parse_number(count, line)?),
            None => (parse_number(range, line)?, 1),
        };
        changed.insert_range(path, start, count);
    }
    Ok(changed)
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
    }

    #[test]
    fn rejects_malformed_hunks() {
        let error = parse_unified_zero_diff("+++ b/src/lib.rs\n@@ -1 +wat @@")
            .expect_err("malformed hunk must fail closed");
        assert!(error.contains("malformed diff hunk"));
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
}
