//! Pure-filesystem git branch lookup. No `git` shell-out.

use std::fs;
use std::path::{Path, PathBuf};

/// Branch name, short SHA for detached HEAD, or `None` outside a git repo.
pub fn branch_for(start: &Path) -> Option<String> {
    let head_path = find_head(start)?;
    let head = fs::read_to_string(&head_path).ok()?;
    let head = head.trim();

    if let Some(refname) = head.strip_prefix("ref: ") {
        Some(refname.rsplit('/').next()?.to_string())
    } else if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(head[..7].to_string())
    } else {
        None
    }
}

fn find_head(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git.join("HEAD"));
        }
        if meta.is_file() {
            let contents = fs::read_to_string(&dot_git).ok()?;
            let gitdir = contents.lines().find_map(|l| l.strip_prefix("gitdir: "))?;
            return Some(PathBuf::from(gitdir.trim()).join("HEAD"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn symbolic_ref_returns_branch() {
        let td = TempDir::new().unwrap();
        write(&td.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        assert_eq!(branch_for(td.path()).as_deref(), Some("main"));
    }

    #[test]
    fn nested_branch_name_keeps_only_leaf() {
        let td = TempDir::new().unwrap();
        write(
            &td.path().join(".git/HEAD"),
            "ref: refs/heads/feat/some-thing\n",
        );
        assert_eq!(branch_for(td.path()).as_deref(), Some("some-thing"));
    }

    #[test]
    fn detached_head_returns_short_sha() {
        let td = TempDir::new().unwrap();
        write(
            &td.path().join(".git/HEAD"),
            "4a8f5970483036c9c3083e8da75bfb4fcfc32911\n",
        );
        assert_eq!(branch_for(td.path()).as_deref(), Some("4a8f597"));
    }

    #[test]
    fn subdirectory_walks_up() {
        let td = TempDir::new().unwrap();
        write(&td.path().join(".git/HEAD"), "ref: refs/heads/master\n");
        let sub = td.path().join("crates/inner");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(branch_for(&sub).as_deref(), Some("master"));
    }

    #[test]
    fn worktree_follows_gitdir_pointer() {
        let td = TempDir::new().unwrap();
        let wt_meta = td.path().join(".git/worktrees/feature");
        write(&wt_meta.join("HEAD"), "ref: refs/heads/feature\n");
        let wt = td.path().join("wt-checkout");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_meta.display()),
        )
        .unwrap();
        assert_eq!(branch_for(&wt).as_deref(), Some("feature"));
    }

    #[test]
    fn no_git_returns_none() {
        let td = TempDir::new().unwrap();
        assert_eq!(branch_for(td.path()), None);
    }
}
