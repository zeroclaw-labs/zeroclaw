//! Git-worktree isolation helpers for delegated/spawned sub-agents.
//!
//! When a sub-agent is delegated with `isolation: "worktree"`, the runtime
//! creates a detached git worktree under `std::env::temp_dir()` rooted at
//! the calling agent's git repository (if one exists). The sub-agent runs
//! its tool loop against that isolated checkout so its file edits do not
//! conflict with the parent checkout or with sibling agents running in
//! parallel.
//!
//! Implementation pattern adapted from claurst's `AgentTool` worktree
//! isolation. Failure modes are non-fatal: if no git root is found or
//! `git worktree add` fails, the sub-agent falls back to the parent's
//! working directory and a warning is emitted via the structured log.

use std::path::{Path, PathBuf};

/// Walk upward from `start` looking for a directory containing a `.git`
/// entry (directory or file — git worktrees use a `.git` file pointing
/// back at the main repo). Returns the path of the first match.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Create a detached worktree at `<tmp>/zeroclaw-agent-<agent_id>` rooted
/// at HEAD of `git_root`. Returns the worktree path on success, `None`
/// on any failure (git missing, dirty index, permission denied, etc.).
pub async fn create_worktree(git_root: &Path, agent_id: &str) -> Option<PathBuf> {
    let worktree_dir = std::env::temp_dir().join(format!("zeroclaw-agent-{agent_id}"));
    // git worktree add refuses to clobber an existing directory; remove a
    // stale one from a previous run with the same agent id before retrying.
    if worktree_dir.exists() {
        remove_worktree(git_root, &worktree_dir).await;
    }
    let output = tokio::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            worktree_dir.to_str()?,
            "HEAD",
        ])
        .current_dir(git_root)
        .output()
        .await
        .ok()?;
    if output.status.success() {
        Some(worktree_dir)
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "git_root": git_root.display().to_string(),
                    "agent_id": agent_id,
                    "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
                })),
            "git worktree add failed; sub-agent will use parent's working directory"
        );
        None
    }
}

/// Best-effort cleanup of a worktree previously created via
/// [`create_worktree`]. Uses `git worktree remove --force` so an in-progress
/// uncommitted edit by the sub-agent does not block teardown — anything the
/// sub-agent wanted to keep needs to have been committed or copied out.
pub async fn remove_worktree(git_root: &Path, worktree_dir: &Path) {
    let _ = tokio::process::Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_dir.to_str().unwrap_or_default(),
        ])
        .current_dir(git_root)
        .output()
        .await;
}

/// RAII guard that removes a worktree when dropped.
///
/// Use [`WorktreeGuard::take_path`] to retrieve the worktree path; if the
/// guard is dropped without `take_path` being called the worktree is left
/// in place (callers spawn into background tasks that own the guard for
/// their full lifetime — see `DelegateTool::execute_background`).
pub struct WorktreeGuard {
    git_root: PathBuf,
    worktree: Option<PathBuf>,
}

impl WorktreeGuard {
    pub fn new(git_root: PathBuf, worktree: PathBuf) -> Self {
        Self {
            git_root,
            worktree: Some(worktree),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }

    /// Cleanly remove the worktree without waiting for Drop. Required when
    /// the cleanup must observe completion (e.g. before a synchronous tool
    /// result is returned). Synchronous-style: blocks on the git invocation.
    pub async fn cleanup(mut self) {
        if let Some(wt) = self.worktree.take() {
            remove_worktree(&self.git_root, &wt).await;
        }
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        // Fire-and-forget cleanup for the common case where the owning
        // task panics or returns early. Synchronous `git worktree remove`
        // is used because we're in a destructor — no async runtime here.
        if let Some(wt) = self.worktree.take() {
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&wt)
                .current_dir(&self.git_root)
                .output();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_git_root_walks_up_directory_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();

        let found = find_git_root(&nested).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn find_git_root_returns_none_when_no_git_dir_exists() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_git_root(tmp.path()).is_none());
    }

    #[tokio::test]
    async fn create_and_remove_worktree_round_trip() {
        // Skip if git is not available on the test runner.
        if tokio::process::Command::new("git")
            .arg("--version")
            .output()
            .await
            .is_err()
        {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let init = tokio::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        if !init.status.success() {
            // Some CI envs ship a git version lacking --initial-branch; skip.
            return;
        }
        // git worktree add requires at least one commit on HEAD.
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(repo)
            .output()
            .await;
        std::fs::write(repo.join("README"), b"seed").unwrap();
        let _ = tokio::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(repo)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(repo)
            .output()
            .await;

        let agent_id = format!("test-{}", std::process::id());
        let wt = create_worktree(repo, &agent_id).await;
        assert!(wt.is_some(), "create_worktree must succeed in seeded repo");
        let wt_path = wt.unwrap();
        assert!(wt_path.exists(), "worktree directory must exist");
        assert!(
            wt_path.join("README").exists(),
            "worktree must contain seeded files"
        );

        remove_worktree(repo, &wt_path).await;
        assert!(
            !wt_path.exists(),
            "remove_worktree must delete the worktree directory"
        );
    }
}
