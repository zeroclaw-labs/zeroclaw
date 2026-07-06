//! Pure-filesystem git branch lookup. No `git` shell-out.

use std::fs;
use std::path::{Path, PathBuf};

/// Resolved HEAD state for a working tree: the branch name (when on a branch)
/// and the short commit hash. Either field may be `None` — e.g. a freshly
/// `git init`'d repo with no commits has a branch but no hash, and a detached
/// HEAD has a hash but no branch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadInfo {
    pub branch: Option<String>,
    pub hash: Option<String>,
}

/// Branch name, short SHA for detached HEAD, or `None` outside a git repo.
pub fn branch_for(start: &Path) -> Option<String> {
    let info = head_info(start)?;
    info.branch.or(info.hash)
}

/// Branch name and short commit hash for the repo containing `start`, or
/// `None` outside a git repo. On a branch, both fields are populated when the
/// ref resolves to a commit. Detached HEAD yields `branch: None` with the hash.
pub fn head_info(start: &Path) -> Option<HeadInfo> {
    let head_path = find_head(start)?;
    let git_dir = head_path.parent()?.to_path_buf();
    let head = fs::read_to_string(&head_path).ok()?;
    let head = head.trim();

    if let Some(refname) = head.strip_prefix("ref: ") {
        let name = refname
            .strip_prefix("refs/heads/")
            .or_else(|| refname.strip_prefix("refs/tags/"))
            .or_else(|| refname.strip_prefix("refs/remotes/"))
            .unwrap_or(refname);
        let hash = resolve_ref(&git_dir, refname);
        Some(HeadInfo {
            branch: Some(name.to_string()),
            hash,
        })
    } else if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(HeadInfo {
            branch: None,
            hash: Some(head[..7].to_string()),
        })
    } else {
        None
    }
}

/// Resolve a full refname (e.g. `refs/heads/main`) to its short commit hash,
/// checking the loose ref file first, then `packed-refs`. Worktrees keep their
/// own `HEAD` but share refs through the common git dir (`commondir`), so loose
/// and packed lookups resolve against that shared dir. Returns `None` when the
/// ref has no commit yet (unborn branch) or cannot be read.
fn resolve_ref(git_dir: &Path, refname: &str) -> Option<String> {
    let common = common_dir(git_dir);
    let loose = common.join(refname);
    if let Ok(sha) = fs::read_to_string(&loose) {
        return short_hash(sha.trim());
    }
    let packed = fs::read_to_string(common.join("packed-refs")).ok()?;
    for line in packed.lines() {
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        if let Some((sha, name)) = line.split_once(' ')
            && name.trim() == refname
        {
            return short_hash(sha.trim());
        }
    }
    None
}

/// The shared git directory for `git_dir`. For a linked worktree, `git_dir` is
/// `.git/worktrees/<name>` and the `commondir` file points (often relatively)
/// at the main `.git`. For a normal repo there is no `commondir` file and the
/// dir is itself the common dir.
fn common_dir(git_dir: &Path) -> PathBuf {
    let Ok(pointer) = fs::read_to_string(git_dir.join("commondir")) else {
        return git_dir.to_path_buf();
    };
    let pointer = pointer.trim();
    let candidate = git_dir.join(pointer);
    candidate.canonicalize().unwrap_or(candidate)
}

fn short_hash(sha: &str) -> Option<String> {
    if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha[..7].to_string())
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
    fn nested_branch_name_is_preserved() {
        let td = TempDir::new().unwrap();
        write(
            &td.path().join(".git/HEAD"),
            "ref: refs/heads/feat/some-thing\n",
        );
        assert_eq!(branch_for(td.path()).as_deref(), Some("feat/some-thing"));
    }

    #[test]
    fn integration_prefix_is_preserved() {
        let td = TempDir::new().unwrap();
        write(
            &td.path().join(".git/HEAD"),
            "ref: refs/heads/integration/zeroclaw-tui\n",
        );
        assert_eq!(
            branch_for(td.path()).as_deref(),
            Some("integration/zeroclaw-tui"),
        );
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
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_meta.display())).unwrap();
        assert_eq!(branch_for(&wt).as_deref(), Some("feature"));
    }

    #[test]
    fn worktree_resolves_hash_via_commondir() {
        let td = TempDir::new().unwrap();
        // Shared refs live in the main .git, reachable via the commondir pointer.
        write(
            &td.path().join(".git/refs/heads/feature"),
            "4a8f5970483036c9c3083e8da75bfb4fcfc32911\n",
        );
        let wt_meta = td.path().join(".git/worktrees/feature");
        write(&wt_meta.join("HEAD"), "ref: refs/heads/feature\n");
        write(&wt_meta.join("commondir"), "../..\n");
        let wt = td.path().join("wt-checkout");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_meta.display())).unwrap();
        let info = head_info(&wt).unwrap();
        assert_eq!(info.branch.as_deref(), Some("feature"));
        assert_eq!(info.hash.as_deref(), Some("4a8f597"));
    }

    #[test]
    fn no_git_returns_none() {
        let td = TempDir::new().unwrap();
        assert_eq!(branch_for(td.path()), None);
    }

    #[test]
    fn head_info_branch_with_loose_ref_hash() {
        let td = TempDir::new().unwrap();
        write(&td.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &td.path().join(".git/refs/heads/main"),
            "4a8f5970483036c9c3083e8da75bfb4fcfc32911\n",
        );
        let info = head_info(td.path()).unwrap();
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.hash.as_deref(), Some("4a8f597"));
    }

    #[test]
    fn head_info_branch_from_packed_refs() {
        let td = TempDir::new().unwrap();
        write(&td.path().join(".git/HEAD"), "ref: refs/heads/master\n");
        write(
            &td.path().join(".git/packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted\n\
             4a8f5970483036c9c3083e8da75bfb4fcfc32911 refs/heads/master\n",
        );
        let info = head_info(td.path()).unwrap();
        assert_eq!(info.branch.as_deref(), Some("master"));
        assert_eq!(info.hash.as_deref(), Some("4a8f597"));
    }

    #[test]
    fn head_info_detached_has_hash_no_branch() {
        let td = TempDir::new().unwrap();
        write(
            &td.path().join(".git/HEAD"),
            "4a8f5970483036c9c3083e8da75bfb4fcfc32911\n",
        );
        let info = head_info(td.path()).unwrap();
        assert_eq!(info.branch, None);
        assert_eq!(info.hash.as_deref(), Some("4a8f597"));
    }

    #[test]
    fn head_info_unborn_branch_has_no_hash() {
        let td = TempDir::new().unwrap();
        write(&td.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        let info = head_info(td.path()).unwrap();
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.hash, None);
    }
}
