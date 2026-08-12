use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;

/// Which side of the canonical policy a Git-affected path set is checked
/// against. `GitOperationsTool` is registered without the `PathGuardedTool`
/// wrapper, so these checks are the only enforcement of `deny_read` /
/// `deny_write` over paths Git reads or mutates.
#[derive(Clone, Copy)]
enum GitAccess {
    Read,
    Write,
}

impl GitAccess {
    fn noun(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// Git operations tool for structured repository management.
/// Provides safe, parsed git operations with JSON output.
pub struct GitOperationsTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: std::path::PathBuf,
}

impl GitOperationsTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: std::path::PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }

    /// Sanitize git arguments to prevent injection attacks
    fn sanitize_git_args(&self, args: &str) -> anyhow::Result<Vec<String>> {
        let mut result = Vec::new();
        for arg in args.split_whitespace() {
            // Block dangerous git options that could lead to command injection
            let arg_lower = arg.to_lowercase();
            if arg_lower.starts_with("--exec=")
                || arg_lower.starts_with("--upload-pack=")
                || arg_lower.starts_with("--receive-pack=")
                || arg_lower.starts_with("--pager=")
                || arg_lower.starts_with("--editor=")
                || arg_lower == "--no-verify"
                || arg_lower.contains("$(")
                || arg_lower.contains('`')
                || arg.contains('|')
                || arg.contains(';')
                || arg.contains('>')
            {
                anyhow::bail!("Blocked potentially dangerous git argument: {arg}");
            }
            // Block `-c` config injection (exact match or `-c=...` prefix).
            // This must not false-positive on `--cached` or `-cached`.
            if arg_lower == "-c" || arg_lower.starts_with("-c=") {
                anyhow::bail!("Blocked potentially dangerous git argument: {arg}");
            }
            result.push(arg.to_string());
        }
        Ok(result)
    }

    /// Check if an operation requires write access.
    ///
    /// `reset` and `revert` are listed for autonomy gating but are not
    /// dispatched by `execute`, so they cannot reach a working tree today.
    /// Every dispatched operation that reads file contents or mutates
    /// working-tree paths carries a preflight over its enumerated affected set:
    /// `add` (`preflight_add`), `diff` (inline), `checkout`
    /// (`ensure_checkout_does_not_overwrite_denied_paths`), `stash`
    /// (`preflight_stash`), and `worktree` add/remove
    /// (`preflight_worktree_add` / `preflight_worktree_remove`). `commit`
    /// records already-staged content and `log`/`branch`/`status` report
    /// metadata only, so neither reaches file contents. Any operation added to
    /// the dispatch table must gain the same treatment before it is wired up —
    /// gating on autonomy alone does not enforce the canonical policy.
    fn requires_write_access(&self, operation: &str) -> bool {
        matches!(
            operation,
            "commit" | "add" | "checkout" | "stash" | "reset" | "revert" | "worktree"
        )
    }

    #[cfg(test)]
    fn is_read_only(&self, operation: &str) -> bool {
        matches!(
            operation,
            "status" | "diff" | "log" | "show" | "branch" | "rev-parse"
        )
    }

    /// Resolve a user-provided path to an absolute path within the workspace.
    /// Returns the workspace_dir if no path is provided.
    /// Rejects paths that escape the workspace via traversal.
    fn resolve_working_dir(&self, path: Option<&str>) -> anyhow::Result<std::path::PathBuf> {
        let base = match path {
            Some(p) if !p.is_empty() => {
                let candidate = if std::path::Path::new(p).is_absolute() {
                    std::path::PathBuf::from(p)
                } else {
                    self.workspace_dir.join(p)
                };
                let resolved = candidate.canonicalize().map_err(|e| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "path": p,
                                "error": format!("{}", e),
                            })),
                        "git_operations: cannot resolve path"
                    );
                    anyhow::Error::msg(format!("Cannot resolve path '{}': {}", p, e))
                })?;
                let workspace_canonical = self
                    .workspace_dir
                    .canonicalize()
                    .unwrap_or_else(|_| self.workspace_dir.clone());
                if !resolved.starts_with(&workspace_canonical) {
                    anyhow::bail!("Path '{}' resolves outside the workspace directory", p);
                }
                resolved
            }
            _ => self.workspace_dir.clone(),
        };
        Ok(base)
    }

    fn candidate_path(&self, raw_path: &str) -> anyhow::Result<PathBuf> {
        if raw_path.contains('\0') {
            anyhow::bail!("Path not allowed: contains null byte");
        }
        if Path::new(raw_path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!("Path not allowed: parent-directory traversal is not allowed");
        }

        let raw = Path::new(raw_path);
        Ok(if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.workspace_dir.join(raw)
        })
    }

    fn ensure_worktree_add_target_allowed(&self, raw_path: &str) -> anyhow::Result<PathBuf> {
        let candidate = self.candidate_path(raw_path)?;
        let parent = candidate.parent().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"raw_path": raw_path})),
                "git_operations: worktree path has no parent"
            );
            anyhow::Error::msg("Worktree path must have a parent directory")
        })?;
        let file_name = candidate.file_name().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"raw_path": raw_path})),
                "git_operations: worktree path has no file name"
            );
            anyhow::Error::msg("Worktree path must include a final path component")
        })?;
        let resolved_parent = parent.canonicalize().map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "parent": parent.display().to_string(),
                        "error": format!("{}", e),
                    })),
                "git_operations: cannot resolve worktree parent"
            );
            anyhow::Error::msg(format!(
                "Cannot resolve worktree parent '{}': {e}",
                parent.display()
            ))
        })?;
        let resolved_target = resolved_parent.join(file_name);

        if !self.security.is_resolved_path_allowed(&resolved_target) {
            anyhow::bail!(
                "Worktree path '{}' resolves outside the workspace or allowed roots",
                raw_path
            );
        }

        Ok(resolved_target)
    }

    fn ensure_worktree_remove_target_allowed(&self, raw_path: &str) -> anyhow::Result<PathBuf> {
        let candidate = self.candidate_path(raw_path)?;
        let resolved = candidate.canonicalize().map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "raw_path": raw_path,
                        "error": format!("{}", e),
                    })),
                "git_operations: cannot resolve worktree path"
            );
            anyhow::Error::msg(format!("Cannot resolve worktree path '{}': {e}", raw_path))
        })?;

        if !self.security.is_resolved_path_allowed(&resolved) {
            anyhow::bail!(
                "Worktree path '{}' resolves outside the workspace or allowed roots",
                raw_path
            );
        }

        Ok(resolved)
    }

    async fn run_git_command(
        &self,
        args: &[&str],
        working_dir: &std::path::Path,
    ) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(working_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git command failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Enumerate the files a `git checkout <branch_name>` would change
    /// relative to `HEAD` and reject the checkout before it runs if any of
    /// them resolve to a `deny_write`-guarded path (e.g. the mandatory
    /// `.env`/`.git/config` guardrails). `file_write`/`file_edit` check a
    /// single write target before mutating; `checkout` has no single
    /// target, so this enumerates the actual mutation set instead of
    /// leaving it unchecked. Fails closed: if the diff enumeration itself
    /// fails (unknown ref, detached HEAD edge cases), the checkout is
    /// rejected rather than allowed to proceed unchecked.
    async fn ensure_checkout_does_not_overwrite_denied_paths(
        &self,
        branch_name: &str,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let diff_output = self
            .run_git_command(
                &["diff", "--name-only", "HEAD", branch_name, "--"],
                working_dir,
            )
            .await
            .map_err(|e| {
                anyhow::Error::msg(format!(
                    "Checkout blocked: cannot determine which files switching to \
                     '{branch_name}' would overwrite: {e}"
                ))
            })?;

        self.ensure_git_paths_allowed(
            &diff_output,
            working_dir,
            GitAccess::Write,
            &format!("Checkout of '{branch_name}'"),
        )
    }

    /// Apply the canonical policy to a Git-reported path set before Git runs.
    ///
    /// `path_list` is newline-separated repository-relative paths exactly as
    /// `--name-only` output produces them. Every entry is resolved against
    /// `working_dir` and checked in `mode`; the first denial aborts the whole
    /// operation rather than letting Git act on the remainder, because a
    /// partially applied Git command cannot be rolled back from here.
    fn ensure_git_paths_allowed(
        &self,
        path_list: &str,
        working_dir: &std::path::Path,
        mode: GitAccess,
        operation: &str,
    ) -> anyhow::Result<()> {
        for relative in path_list.lines().filter(|line| !line.is_empty()) {
            let candidate = working_dir.join(relative);
            let resolved = zeroclaw_config::policy::canonicalize_best_effort(&candidate);
            let allowed = match mode {
                GitAccess::Read => self.security.is_resolved_path_readable(&resolved),
                GitAccess::Write => self.security.is_resolved_path_allowed(&resolved),
            };
            if !allowed {
                anyhow::bail!(
                    "{operation} blocked: '{relative}' is denied by the current {} policy",
                    mode.noun()
                );
            }
        }

        Ok(())
    }

    /// Enumerate the paths `git add` would stage and reject the operation when
    /// any is denied for reads.
    ///
    /// Staging copies a file's bytes into the object store, so `add` is a read
    /// of every path it touches even though it never mutates the working tree.
    /// `--dry-run` is what Git itself would act on, so the pathspec is expanded
    /// by Git rather than matched textually here. Fails closed on any line that
    /// does not have the documented `add '<path>'` shape — an unparsed line
    /// means the affected set cannot be proven safe.
    async fn preflight_add(
        &self,
        pathspec: &[String],
        working_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let mut args: Vec<&str> = vec!["add", "--dry-run", "--"];
        args.extend(pathspec.iter().map(String::as_str));

        let dry_run = self
            .run_git_command(&args, working_dir)
            .await
            .map_err(|e| {
                anyhow::Error::msg(format!(
                    "Add blocked: cannot determine which files it would stage: {e}"
                ))
            })?;

        let mut affected = String::new();
        for line in dry_run.lines().filter(|line| !line.is_empty()) {
            let path = line
                .strip_prefix("add '")
                .or_else(|| line.strip_prefix("remove '"))
                .and_then(|rest| rest.strip_suffix('\''))
                .ok_or_else(|| {
                    anyhow::Error::msg(format!(
                        "Add blocked: cannot interpret the affected path in git output: {line}"
                    ))
                })?;
            affected.push_str(path);
            affected.push('\n');
        }

        self.ensure_git_paths_allowed(&affected, working_dir, GitAccess::Read, "Add")
    }

    /// Enumerate the files `git worktree add` would materialize and reject the
    /// operation when any is denied for writes. The target root is already
    /// checked by [`Self::ensure_worktree_add_target_allowed`]; this covers the
    /// tree Git writes underneath it, which a root-only check cannot see.
    async fn preflight_worktree_add(
        &self,
        target: &std::path::Path,
        reference: &str,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let tree = self
            .run_git_command(&["ls-tree", "-r", "--name-only", reference], working_dir)
            .await
            .map_err(|e| {
                anyhow::Error::msg(format!(
                    "Worktree add blocked: cannot determine which files '{reference}' would \
                     materialize: {e}"
                ))
            })?;

        self.ensure_git_paths_allowed(&tree, target, GitAccess::Write, "Worktree add")
    }

    /// Enumerate everything `git worktree remove` would delete and reject the
    /// operation when any of it is denied for writes. Deletion is a write, and
    /// the existing check only covers the worktree root — a denied path nested
    /// inside would be removed unchecked. Walks the real directory rather than
    /// asking Git, because removal takes the whole tree, not just tracked
    /// files. Symlinks are recorded but never followed, so the walk cannot
    /// escape the worktree. Fails closed if the tree cannot be enumerated.
    async fn preflight_worktree_remove(&self, target: &std::path::Path) -> anyhow::Result<()> {
        let mut affected = String::new();
        let mut pending = vec![target.to_path_buf()];

        while let Some(dir) = pending.pop() {
            let mut entries = tokio::fs::read_dir(&dir).await.map_err(|e| {
                anyhow::Error::msg(format!(
                    "Worktree remove blocked: cannot enumerate '{}': {e}",
                    dir.display()
                ))
            })?;
            while let Some(entry) = entries.next_entry().await.map_err(|e| {
                anyhow::Error::msg(format!(
                    "Worktree remove blocked: cannot enumerate '{}': {e}",
                    dir.display()
                ))
            })? {
                let path = entry.path();
                let meta = tokio::fs::symlink_metadata(&path).await.map_err(|e| {
                    anyhow::Error::msg(format!(
                        "Worktree remove blocked: cannot inspect '{}': {e}",
                        path.display()
                    ))
                })?;
                affected.push_str(&path.to_string_lossy());
                affected.push('\n');
                if meta.is_dir() {
                    pending.push(path);
                }
            }
        }

        self.ensure_git_paths_allowed(&affected, target, GitAccess::Write, "Worktree remove")
    }

    /// Enumerate a stash action's mutation set and reject it when any affected
    /// path is denied for writes.
    async fn preflight_stash(
        &self,
        action: &str,
        include_untracked: bool,
        pathspec: &[String],
        working_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let affected = self
            .stash_mutation_set(action, include_untracked, pathspec, working_dir)
            .await?;
        self.ensure_git_paths_allowed(
            &affected,
            working_dir,
            GitAccess::Write,
            &format!("Stash {action}"),
        )
    }

    /// Enumerate the working-tree paths a `git stash` action would create,
    /// replace, or delete. `push`/`save` revert tracked modifications (and,
    /// with `include_untracked`, remove untracked files); `pop` writes the
    /// stashed contents back. Both mutation sets come from Git itself so the
    /// check covers exactly what Git is about to touch.
    async fn stash_mutation_set(
        &self,
        action: &str,
        include_untracked: bool,
        pathspec: &[String],
        working_dir: &std::path::Path,
    ) -> anyhow::Result<String> {
        let mut affected = String::new();

        if action == "pop" {
            // `stash show` defaults to the most recent entry — the same one
            // `stash pop` restores.
            let restored = self
                .run_git_command(&["stash", "show", "--name-only"], working_dir)
                .await
                .map_err(|e| {
                    anyhow::Error::msg(format!(
                        "Stash pop blocked: cannot determine which files it would restore: {e}"
                    ))
                })?;
            affected.push_str(&restored);
            return Ok(affected);
        }

        let mut tracked_args: Vec<&str> = vec!["diff", "--name-only", "HEAD", "--"];
        for p in pathspec {
            tracked_args.push(p);
        }
        let tracked = self
            .run_git_command(&tracked_args, working_dir)
            .await
            .map_err(|e| {
                anyhow::Error::msg(format!(
                    "Stash blocked: cannot determine which tracked files it would revert: {e}"
                ))
            })?;
        affected.push_str(&tracked);

        if include_untracked {
            let mut untracked_args: Vec<&str> =
                vec!["ls-files", "--others", "--exclude-standard", "--"];
            for p in pathspec {
                untracked_args.push(p);
            }
            let untracked = self
                .run_git_command(&untracked_args, working_dir)
                .await
                .map_err(|e| {
                    anyhow::Error::msg(format!(
                        "Stash blocked: cannot determine which untracked files it would \
                         remove: {e}"
                    ))
                })?;
            affected.push_str(&untracked);
        }

        Ok(affected)
    }

    async fn git_status(
        &self,
        _args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let output = self
            .run_git_command(&["status", "--porcelain=2", "--branch"], working_dir)
            .await?;

        // Parse git status output into structured format
        let mut result = serde_json::Map::new();
        let mut branch = String::new();
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        for line in output.lines() {
            if line.starts_with("# branch.head ") {
                branch = line.trim_start_matches("# branch.head ").to_string();
            } else if let Some(rest) = line.strip_prefix("1 ") {
                // Ordinary changed entry
                let mut parts = rest.splitn(3, ' ');
                if let (Some(staging), Some(path)) = (parts.next(), parts.next())
                    && !staging.is_empty()
                {
                    let status_char = staging.chars().next().unwrap_or(' ');
                    if status_char != '.' && status_char != ' ' {
                        staged.push(json!({"path": path, "status": status_char}));
                    }
                    let status_char = staging.chars().nth(1).unwrap_or(' ');
                    if status_char != '.' && status_char != ' ' {
                        unstaged.push(json!({"path": path, "status": status_char}));
                    }
                }
            } else if let Some(rest) = line.strip_prefix("? ") {
                untracked.push(rest.to_string());
            }
        }

        result.insert("branch".to_string(), json!(branch));
        result.insert("staged".to_string(), json!(staged));
        result.insert("unstaged".to_string(), json!(unstaged));
        result.insert("untracked".to_string(), json!(untracked));
        result.insert(
            "clean".to_string(),
            json!(staged.is_empty() && unstaged.is_empty() && untracked.is_empty()),
        );

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)
                .unwrap_or_default()
                .into(),
            error: None,
        })
    }

    async fn git_diff(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let files = args.get("files").and_then(|v| v.as_str()).unwrap_or(".");
        let cached = args
            .get("cached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Validate files argument against injection patterns
        self.sanitize_git_args(files)?;

        // A diff prints file contents, so the requested pathspec has to clear
        // the canonical read policy before Git runs. The pathspec is expanded
        // by Git itself (`--name-only` over the same arguments) rather than
        // matched textually, so a glob or directory that selects a denied file
        // is caught. Fails closed: if the affected read set cannot be
        // enumerated, the diff is refused rather than run unchecked.
        let mut enumerate_args = vec!["diff", "--name-only"];
        if cached {
            enumerate_args.push("--cached");
        }
        enumerate_args.push("--");
        enumerate_args.push(files);

        let affected = match self.run_git_command(&enumerate_args, working_dir).await {
            Ok(list) => list,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Diff blocked: cannot determine which files it would read: {e}"
                    )),
                });
            }
        };

        if let Err(e) =
            self.ensure_git_paths_allowed(&affected, working_dir, GitAccess::Read, "Diff")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("{e}")),
            });
        }

        let mut git_args = vec!["diff", "--unified=3"];
        if cached {
            git_args.push("--cached");
        }
        git_args.push("--");
        git_args.push(files);

        let output = self.run_git_command(&git_args, working_dir).await?;

        // Parse diff into structured hunks
        let mut result = serde_json::Map::new();
        let mut hunks = Vec::new();
        let mut current_file = String::new();
        let mut current_hunk = serde_json::Map::new();
        let mut lines = Vec::new();

        for line in output.lines() {
            if line.starts_with("diff --git ") {
                if !lines.is_empty() {
                    current_hunk.insert("lines".to_string(), json!(lines));
                    if !current_hunk.is_empty() {
                        hunks.push(serde_json::Value::Object(current_hunk.clone()));
                    }
                    lines = Vec::new();
                    current_hunk = serde_json::Map::new();
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    current_file = parts[3].trim_start_matches("b/").to_string();
                    current_hunk.insert("file".to_string(), json!(current_file));
                }
            } else if line.starts_with("@@ ") {
                if !lines.is_empty() {
                    current_hunk.insert("lines".to_string(), json!(lines));
                    if !current_hunk.is_empty() {
                        hunks.push(serde_json::Value::Object(current_hunk.clone()));
                    }
                    lines = Vec::new();
                    current_hunk = serde_json::Map::new();
                    current_hunk.insert("file".to_string(), json!(current_file));
                }
                current_hunk.insert("header".to_string(), json!(line));
            } else if !line.is_empty() {
                lines.push(json!({
                    "text": line,
                    "type": if line.starts_with('+') { "add" }
                           else if line.starts_with('-') { "delete" }
                           else { "context" }
                }));
            }
        }

        if !lines.is_empty() {
            current_hunk.insert("lines".to_string(), json!(lines));
            if !current_hunk.is_empty() {
                hunks.push(serde_json::Value::Object(current_hunk));
            }
        }

        result.insert("hunks".to_string(), json!(hunks));
        result.insert("file_count".to_string(), json!(hunks.len()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)
                .unwrap_or_default()
                .into(),
            error: None,
        })
    }

    async fn git_log(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let limit_raw = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
        let limit = usize::try_from(limit_raw).unwrap_or(usize::MAX).min(1000);
        let limit_str = limit.to_string();

        let output = self
            .run_git_command(
                &[
                    "log",
                    &format!("-{limit_str}"),
                    "--pretty=format:%H|%an|%ae|%ad|%s",
                    "--date=iso",
                ],
                working_dir,
            )
            .await?;

        let mut commits = Vec::new();

        for line in output.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 5 {
                commits.push(json!({
                    "hash": parts[0],
                    "author": parts[1],
                    "email": parts[2],
                    "date": parts[3],
                    "message": parts[4]
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({ "commits": commits }))
                .unwrap_or_default()
                .into(),
            error: None,
        })
    }

    async fn git_branch(
        &self,
        _args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let output = self
            .run_git_command(
                &["branch", "--format=%(refname:short)|%(HEAD)"],
                working_dir,
            )
            .await?;

        let mut branches = Vec::new();
        let mut current = String::new();

        for line in output.lines() {
            if let Some((name, head)) = line.split_once('|') {
                let is_current = head == "*";
                if is_current {
                    current = name.to_string();
                }
                branches.push(json!({
                    "name": name,
                    "current": is_current
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "current": current,
                "branches": branches
            }))
            .unwrap_or_default()
            .into(),
            error: None,
        })
    }

    fn truncate_commit_message(message: &str) -> String {
        if message.chars().count() > 2000 {
            format!("{}...", message.chars().take(1997).collect::<String>())
        } else {
            message.to_string()
        }
    }

    async fn git_commit(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "message"})),
                    "git_operations: missing message parameter"
                );
                anyhow::Error::msg("Missing 'message' parameter")
            })?;

        let trimmed_lines: Vec<&str> = message.lines().map(|l| l.trim_end()).collect();
        // Drop leading blank lines.
        let trimmed_lines = trimmed_lines
            .iter()
            .copied()
            .skip_while(|l| l.is_empty())
            .collect::<Vec<_>>();
        // Collapse runs of more than 2 consecutive blank lines to 2.
        let mut sanitized_lines: Vec<&str> = Vec::with_capacity(trimmed_lines.len());
        let mut consecutive_blanks = 0usize;
        for line in &trimmed_lines {
            if line.is_empty() {
                consecutive_blanks += 1;
                if consecutive_blanks <= 2 {
                    sanitized_lines.push(line);
                }
            } else {
                consecutive_blanks = 0;
                sanitized_lines.push(line);
            }
        }
        // Drop trailing blank lines.
        while sanitized_lines.last().is_some_and(|l: &&str| l.is_empty()) {
            sanitized_lines.pop();
        }
        let sanitized = sanitized_lines.join("\n");

        if sanitized.is_empty() {
            anyhow::bail!("Commit message cannot be empty");
        }

        // Limit message length
        let message = Self::truncate_commit_message(&sanitized);

        let output = self
            .run_git_command(&["commit", "-m", &message], working_dir)
            .await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Committed: {message}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Commit failed: {e}")),
            }),
        }
    }

    async fn git_add(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let paths = args.get("paths").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "paths"})),
                "git_operations: missing paths parameter"
            );
            anyhow::Error::msg("Missing 'paths' parameter")
        })?;

        // Validate paths against injection patterns. Returns each
        // whitespace-separated pathspec as its own argument so the join is
        // not handed to git as a single literal path.
        let sanitized = self.sanitize_git_args(paths)?;
        if sanitized.is_empty() {
            anyhow::bail!("No paths to stage");
        }

        if let Err(e) = self.preflight_add(&sanitized, working_dir).await {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("{e}")),
            });
        }

        let mut git_args: Vec<&str> = vec!["add", "--"];
        git_args.extend(sanitized.iter().map(String::as_str));

        let output = self.run_git_command(&git_args, working_dir).await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Staged: {}", sanitized.join(" ")).into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Add failed: {e}")),
            }),
        }
    }

    async fn git_checkout(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let branch = args.get("branch").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "branch"})),
                "git_operations: missing branch parameter"
            );
            anyhow::Error::msg("Missing 'branch' parameter")
        })?;

        // Sanitize branch name
        let sanitized = self.sanitize_git_args(branch)?;

        if sanitized.is_empty() || sanitized.len() > 1 {
            anyhow::bail!("Invalid branch specification");
        }

        let branch_name = &sanitized[0];

        // Block dangerous branch names
        if branch_name.contains('@') || branch_name.contains('^') || branch_name.contains('~') {
            anyhow::bail!("Branch name contains invalid characters");
        }

        if let Err(e) = self
            .ensure_checkout_does_not_overwrite_denied_paths(branch_name, working_dir)
            .await
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("{e}")),
            });
        }

        let output = self
            .run_git_command(&["checkout", branch_name], working_dir)
            .await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Switched to branch: {branch_name}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Checkout failed: {e}")),
            }),
        }
    }

    async fn git_stash(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("push");

        let output = match action {
            "push" | "save" => {
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto-stash")
                    .to_string();
                let keep_index = args
                    .get("keep_index")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let include_untracked = args
                    .get("include_untracked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let paths_raw = args
                    .get("paths")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let pathspec: Vec<String> = paths_raw
                    .split_whitespace()
                    .map(ToString::to_string)
                    .collect();

                // `stash push` reverts tracked modifications in the working
                // tree (and removes untracked files with `-u`), so its
                // mutation set is checked against `deny_write` first — the
                // same contract `checkout` enforces.
                if let Err(e) = self
                    .preflight_stash(action, include_untracked, &pathspec, working_dir)
                    .await
                {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("{e}")),
                    });
                }

                let mut cmd: Vec<String> =
                    vec!["stash".into(), "push".into(), "-m".into(), message];
                if keep_index {
                    cmd.push("-k".into());
                }
                if include_untracked {
                    cmd.push("-u".into());
                }
                if !paths_raw.is_empty() {
                    cmd.push("--".into());
                    for p in paths_raw.split_whitespace() {
                        cmd.push(p.to_string());
                    }
                }
                let cmd_refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
                self.run_git_command(&cmd_refs, working_dir).await
            }
            "pop" => {
                // `pop` writes the stashed contents back over the working
                // tree, so the restored path set is checked the same way.
                if let Err(e) = self.preflight_stash(action, false, &[], working_dir).await {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("{e}")),
                    });
                }
                self.run_git_command(&["stash", "pop"], working_dir).await
            }
            "list" => self.run_git_command(&["stash", "list"], working_dir).await,
            "drop" => {
                let index_raw = args.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let index = i32::try_from(index_raw).map_err(|_| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"index": index_raw})),
                        "git_operations: stash index too large"
                    );
                    anyhow::Error::msg(format!("stash index too large: {index_raw}"))
                })?;
                self.run_git_command(
                    &["stash", "drop", &format!("stash@{{{index}}}")],
                    working_dir,
                )
                .await
            }
            _ => anyhow::bail!("Unknown stash action: {action}. Use: push, pop, list, drop"),
        };

        match output {
            Ok(out) => Ok(ToolResult {
                success: true,
                output: out.into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Stash {action} failed: {e}")),
            }),
        }
    }

    fn parse_worktree_list(&self, output: &str) -> serde_json::Value {
        let mut worktrees = Vec::new();
        let mut current_path = String::new();
        let mut current_branch = String::new();
        let mut current_head = String::new();
        let mut is_detached = false;

        let workspace = self.workspace_dir.to_string_lossy();

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                if !current_path.is_empty() {
                    worktrees.push(json!({
                        "path": &current_path,
                        "branch": if is_detached { "HEAD" } else { &current_branch },
                        "head": &current_head,
                        "detached": is_detached,
                        "active": current_path == workspace.as_ref()
                    }));
                    current_path.clear();
                    current_branch.clear();
                    current_head.clear();
                    is_detached = false;
                }
            } else if let Some(p) = line.strip_prefix("worktree ") {
                current_path = p.to_string();
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                current_head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch ") {
                current_branch = b.trim_start_matches("refs/heads/").to_string();
            } else if line == "detached" {
                is_detached = true;
            }
        }
        // Flush final entry if output has no trailing blank line
        if !current_path.is_empty() {
            worktrees.push(json!({
                "path": &current_path,
                "branch": if is_detached { "HEAD" } else { current_branch.as_str() },
                "head": &current_head,
                "detached": is_detached,
                "active": current_path == workspace.as_ref()
            }));
        }

        json!({ "worktrees": worktrees })
    }

    async fn git_worktree(
        &self,
        args: serde_json::Value,
        working_dir: &std::path::Path,
    ) -> anyhow::Result<ToolResult> {
        let subcommand = match args.get("subcommand").and_then(|v| v.as_str()) {
            Some(cmd) => cmd,
            None => anyhow::bail!("Missing 'subcommand' parameter. Use: list, add, remove, prune"),
        };

        match subcommand {
            "list" => {
                let output = self
                    .run_git_command(&["worktree", "list", "--porcelain"], working_dir)
                    .await?;
                let parsed = self.parse_worktree_list(&output);
                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&parsed)
                        .unwrap_or_default()
                        .into(),
                    error: None,
                })
            }
            "add" => {
                let worktree_path = match args.get("worktree_path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => anyhow::bail!("Missing 'worktree_path' parameter for worktree add"),
                };
                self.sanitize_git_args(worktree_path)?;
                let worktree_path = self.ensure_worktree_add_target_allowed(worktree_path)?;
                let worktree_path = worktree_path.to_str().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "git_operations: worktree path not valid UTF-8"
                    );
                    anyhow::Error::msg("Worktree path must be valid UTF-8 for git execution")
                })?;

                let branch = args
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                // git worktree add <path> [<branch>]
                let mut git_args = vec!["worktree", "add", worktree_path];
                if !branch.is_empty() {
                    self.sanitize_git_args(branch)?;
                    git_args.push(branch);
                }

                // Without a branch Git creates one from HEAD, so HEAD is the
                // tree that gets materialized in that case.
                let reference = if branch.is_empty() { "HEAD" } else { branch };
                if let Err(e) = self
                    .preflight_worktree_add(
                        std::path::Path::new(worktree_path),
                        reference,
                        working_dir,
                    )
                    .await
                {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("{e}")),
                    });
                }

                self.run_git_command(&git_args, working_dir).await?;
                Ok(ToolResult {
                    success: true,
                    output: format!("Worktree added at: {worktree_path}").into(),
                    error: None,
                })
            }
            "remove" => {
                let worktree_path = match args.get("worktree_path").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => anyhow::bail!("Missing 'worktree_path' parameter for worktree remove"),
                };
                self.sanitize_git_args(worktree_path)?;
                let worktree_path = self.ensure_worktree_remove_target_allowed(worktree_path)?;
                let worktree_path = worktree_path.to_str().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "git_operations: worktree path not valid UTF-8"
                    );
                    anyhow::Error::msg("Worktree path must be valid UTF-8 for git execution")
                })?;

                if let Err(e) = self
                    .preflight_worktree_remove(std::path::Path::new(worktree_path))
                    .await
                {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("{e}")),
                    });
                }

                self.run_git_command(&["worktree", "remove", worktree_path], working_dir)
                    .await?;
                Ok(ToolResult {
                    success: true,
                    output: format!("Worktree removed: {worktree_path}").into(),
                    error: None,
                })
            }
            "prune" => {
                self.run_git_command(&["worktree", "prune"], working_dir)
                    .await?;
                Ok(ToolResult {
                    success: true,
                    output: "Worktree prune completed".to_string().into(),
                    error: None,
                })
            }
            _ => anyhow::bail!(
                "Unknown worktree subcommand: {subcommand}. Use: list, add, remove, prune"
            ),
        }
    }
}

#[async_trait]
impl Tool for GitOperationsTool {
    fn name(&self) -> &str {
        "git_operations"
    }

    fn description(&self) -> &str {
        "Perform structured Git operations (status, diff, log, branch, commit, add, checkout, stash, worktree). Provides parsed JSON output and integrates with security policy for autonomy controls."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "branch", "commit", "add", "checkout", "stash", "worktree"],
                    "description": "Git operation to perform"
                },
                "subcommand": {
                    "type": "string",
                    "enum": ["list", "add", "remove", "prune"],
                    "description": "Worktree subcommand"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (for 'commit' operation); stash message (for 'stash push', defaults to 'auto-stash')"
                },
                "paths": {
                    "type": "string",
                    "description": "Space-separated file paths. For 'add', files to stage. For 'stash push', pathspecs to scope the stash to — without this, the entire working tree is stashed."
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name (for 'checkout' operation or 'worktree add' subcommand)"
                },
                "worktree_path": {
                    "type": "string",
                    "description": "Filesystem path for the worktree (for 'worktree add' and 'worktree remove' subcommands). Relative paths resolve under the workspace; absolute paths must stay inside the workspace or configured allowed roots."
                },
                "files": {
                    "type": "string",
                    "description": "File or path to diff (for 'diff' operation, default: '.')"
                },
                "cached": {
                    "type": "boolean",
                    "description": "Show staged changes (for 'diff' operation)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of log entries (for 'log' operation, default: 10)"
                },
                "action": {
                    "type": "string",
                    "enum": ["push", "pop", "list", "drop"],
                    "description": "Stash action (for 'stash' operation)"
                },
                "index": {
                    "type": "integer",
                    "description": "Stash index (for 'stash' with 'drop' action)"
                },
                "keep_index": {
                    "type": "boolean",
                    "description": "For 'stash push': preserve staged changes in the working tree after stashing — only unstaged changes go into the stash."
                },
                "include_untracked": {
                    "type": "boolean",
                    "description": "For 'stash push': also stash untracked files (-u). Without this, `git stash push` only touches tracked files."
                },
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory path within the workspace to run git operations in. Defaults to workspace root."
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let operation = match args.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'operation' parameter".into()),
                });
            }
        };

        let path = args.get("path").and_then(|v| v.as_str());
        let working_dir = match self.resolve_working_dir(path) {
            Ok(d) => d,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Invalid path: {e}")),
                });
            }
        };

        // Check if we're in a git repository
        if !working_dir.join(".git").exists() {
            // Try to find .git in parent directories
            let mut current_dir = working_dir.as_path();
            let mut found_git = false;
            while current_dir.parent().is_some() {
                if current_dir.join(".git").exists() {
                    found_git = true;
                    break;
                }
                current_dir = current_dir.parent().unwrap();
            }

            if !found_git {
                let path_display = working_dir.display().to_string();
                let error_msg = crate::i18n::get_required_tool_string_with_args(
                    "tool-git-operations-error-not-in-repo",
                    &[("path", &path_display)],
                );
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error_msg),
                });
            }
        }

        // Check autonomy level for write operations
        if self.requires_write_access(operation) {
            if !self.security.can_act() {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(
                        "Action blocked: git write operations require higher autonomy level".into(),
                    ),
                });
            }

            match self.security.autonomy {
                AutonomyLevel::ReadOnly => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some("Action blocked: read-only mode".into()),
                    });
                }
                AutonomyLevel::Supervised | AutonomyLevel::Full => {}
            }
        }

        // Record action for rate limiting
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        // Execute the requested operation
        match operation {
            "status" => self.git_status(args, &working_dir).await,
            "diff" => self.git_diff(args, &working_dir).await,
            "log" => self.git_log(args, &working_dir).await,
            "branch" => self.git_branch(args, &working_dir).await,
            "commit" => self.git_commit(args, &working_dir).await,
            "add" => self.git_add(args, &working_dir).await,
            "checkout" => self.git_checkout(args, &working_dir).await,
            "stash" => self.git_stash(args, &working_dir).await,
            "worktree" => self.git_worktree(args, &working_dir).await,
            _ => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Unknown operation: {operation}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool(dir: &std::path::Path) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            ..SecurityPolicy::default()
        });
        GitOperationsTool::new(security, dir.to_path_buf())
    }

    /// Initialise a git repo for tests with commit/tag signing disabled and a
    /// fixed identity. Tests run real `git commit`; without this they inherit
    /// the developer's global `commit.gpgsign`, blocking the suite on a
    /// hardware-key tap.
    fn git_init_no_sign(dir: &std::path::Path, extra_init: &[&str]) {
        let mut init = vec!["init"];
        init.extend_from_slice(extra_init);
        for args in [
            init.as_slice(),
            &["config", "user.email", "test@test.com"],
            &["config", "user.name", "Test"],
            &["config", "commit.gpgsign", "false"],
            &["config", "tag.gpgsign", "false"],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
        }
    }

    fn test_tool_with_allowed_root(
        dir: &std::path::Path,
        allowed_root: std::path::PathBuf,
    ) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: dir.to_path_buf(),
            allowed_roots: vec![allowed_root],
            ..SecurityPolicy::default()
        });
        GitOperationsTool::new(security, dir.to_path_buf())
    }

    #[test]
    fn sanitize_git_blocks_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Should block dangerous arguments
        assert!(tool.sanitize_git_args("--exec=rm -rf /").is_err());
        assert!(tool.sanitize_git_args("$(echo pwned)").is_err());
        assert!(tool.sanitize_git_args("`malicious`").is_err());
        assert!(tool.sanitize_git_args("arg | cat").is_err());
        assert!(tool.sanitize_git_args("arg; rm file").is_err());
    }

    #[test]
    fn sanitize_git_blocks_pager_editor_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.sanitize_git_args("--pager=less").is_err());
        assert!(tool.sanitize_git_args("--editor=vim").is_err());
    }

    #[test]
    fn sanitize_git_blocks_config_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Exact `-c` flag (config injection)
        assert!(tool.sanitize_git_args("-c core.sshCommand=evil").is_err());
        assert!(tool.sanitize_git_args("-c=core.pager=less").is_err());
    }

    #[test]
    fn sanitize_git_blocks_no_verify() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.sanitize_git_args("--no-verify").is_err());
    }

    #[test]
    fn sanitize_git_blocks_redirect_in_args() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.sanitize_git_args("file.txt > /tmp/out").is_err());
    }

    #[test]
    fn sanitize_git_cached_not_blocked() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // --cached must NOT be blocked by the `-c` check
        assert!(tool.sanitize_git_args("--cached").is_ok());
        // Other safe flags starting with -c prefix
        assert!(tool.sanitize_git_args("-cached").is_ok());
    }

    #[test]
    fn worktree_add_target_must_stay_inside_workspace_or_allowed_root() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let tool = test_tool(workspace.path());

        assert!(
            tool.ensure_worktree_add_target_allowed("new-worktree")
                .is_ok()
        );
        assert!(
            tool.ensure_worktree_add_target_allowed(
                outside.path().join("new-worktree").to_str().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn worktree_add_target_allows_configured_allowed_root() {
        let workspace = TempDir::new().unwrap();
        let allowed = TempDir::new().unwrap();
        let tool = test_tool_with_allowed_root(workspace.path(), allowed.path().to_path_buf());

        assert!(
            tool.ensure_worktree_add_target_allowed(
                allowed.path().join("new-worktree").to_str().unwrap()
            )
            .is_ok()
        );
    }

    #[test]
    fn worktree_remove_target_must_stay_inside_workspace() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(workspace.path().join("old-worktree")).unwrap();
        std::fs::create_dir(outside.path().join("old-worktree")).unwrap();
        let tool = test_tool(workspace.path());

        assert!(
            tool.ensure_worktree_remove_target_allowed("old-worktree")
                .is_ok()
        );
        assert!(
            tool.ensure_worktree_remove_target_allowed(
                outside.path().join("old-worktree").to_str().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn sanitize_git_allows_safe() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Should allow safe arguments
        assert!(tool.sanitize_git_args("main").is_ok());
        assert!(tool.sanitize_git_args("feature/test-branch").is_ok());
        assert!(tool.sanitize_git_args("--cached").is_ok());
        assert!(tool.sanitize_git_args("src/main.rs").is_ok());
        assert!(tool.sanitize_git_args(".").is_ok());
    }

    #[test]
    fn requires_write_detection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.requires_write_access("commit"));
        assert!(tool.requires_write_access("add"));
        assert!(tool.requires_write_access("checkout"));
        assert!(tool.requires_write_access("stash"));
        assert!(tool.requires_write_access("worktree"));

        assert!(!tool.requires_write_access("status"));
        assert!(!tool.requires_write_access("diff"));
        assert!(!tool.requires_write_access("log"));
        assert!(!tool.requires_write_access("branch"));
    }

    #[test]
    fn is_read_only_detection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        assert!(tool.is_read_only("status"));
        assert!(tool.is_read_only("diff"));
        assert!(tool.is_read_only("log"));
        assert!(tool.is_read_only("branch"));

        // worktree has write subcommands (add/remove), so it is not read-only
        assert!(!tool.is_read_only("worktree"));
        assert!(!tool.is_read_only("commit"));
        assert!(!tool.is_read_only("add"));
    }

    #[test]
    fn branch_is_not_write_gated() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        // Branch listing is read-only; it must not require write access
        assert!(!tool.requires_write_access("branch"));
        assert!(tool.is_read_only("branch"));
    }

    #[tokio::test]
    async fn git_credential_op_fails_fast_without_terminal_prompt() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);
        let tool = test_tool(tmp.path());

        let fetch = tool.run_git_command(
            &["fetch", "https://127.0.0.1:1/private/repo.git"],
            tmp.path(),
        );
        let res = tokio::time::timeout(std::time::Duration::from_secs(10), fetch).await;

        assert!(
            res.is_ok(),
            "git fetch hung — it likely prompted for credentials on the terminal"
        );
        assert!(
            res.unwrap().is_err(),
            "fetch to an unreachable private remote should fail, not succeed"
        );
    }

    #[tokio::test]
    async fn blocks_readonly_mode_for_write_ops() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "commit", "message": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        // can_act() returns false for ReadOnly, so we get the "higher autonomy level" message
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("higher autonomy")
        );
    }

    #[tokio::test]
    async fn allows_branch_listing_in_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool.execute(json!({"operation": "branch"})).await.unwrap();
        // Branch listing must not be blocked by read-only autonomy
        let error_msg = result.error.as_deref().unwrap_or("");
        assert!(
            !error_msg.contains("read-only") && !error_msg.contains("higher autonomy"),
            "branch listing should not be blocked in read-only mode, got: {error_msg}"
        );
    }

    #[tokio::test]
    async fn allows_readonly_ops_in_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        // This will fail because there's no git repo, but it shouldn't be blocked by autonomy
        let result = tool.execute(json!({"operation": "status"})).await.unwrap();
        // The error should be about git (not about autonomy/read-only mode)
        assert!(!result.success, "Expected failure due to missing git repo");
        let error_msg = result.error.as_deref().unwrap_or("");
        assert!(
            !error_msg.contains("read-only") && !error_msg.contains("autonomy"),
            "Error should be about git, not about autonomy restrictions: {error_msg}"
        );
    }

    #[tokio::test]
    async fn rejects_missing_operation() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Missing 'operation'")
        );
    }

    #[tokio::test]
    async fn rejects_unknown_operation() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let tool = test_tool(tmp.path());

        let result = tool.execute(json!({"operation": "push"})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Unknown operation")
        );
    }

    #[tokio::test]
    async fn commit_message_preserves_blank_line_between_subject_and_body() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);
        // Create an initial commit so HEAD exists.
        std::fs::write(tmp.path().join("README.md"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path());

        let msg = "fix(foo): subject line\n\nThis is the body paragraph.\n\nSecond paragraph.";
        let result = tool
            .execute(json!({"operation": "commit", "message": msg}))
            .await
            .unwrap();
        assert!(result.success, "commit failed: {:?}", result.error);

        // Read back the raw commit message via git log.
        let log_out = std::process::Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log_msg = String::from_utf8_lossy(&log_out.stdout);

        // Subject line must be on its own line.
        assert!(
            log_msg.starts_with("fix(foo): subject line\n"),
            "subject line missing or not first: {log_msg:?}"
        );
        // A blank line must follow the subject.
        assert!(
            log_msg.contains("fix(foo): subject line\n\n"),
            "blank line between subject and body missing: {log_msg:?}"
        );
        // Body text must be present.
        assert!(
            log_msg.contains("This is the body paragraph."),
            "body paragraph missing: {log_msg:?}"
        );
    }

    #[test]
    fn truncates_multibyte_commit_message_without_panicking() {
        let long = "🦀".repeat(2500);
        let truncated = GitOperationsTool::truncate_commit_message(&long);

        assert_eq!(truncated.chars().count(), 2000);
    }

    #[test]
    fn resolve_working_dir_none_returns_workspace() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(None).unwrap();
        assert_eq!(result, tmp.path().to_path_buf());
    }

    #[test]
    fn resolve_working_dir_empty_returns_workspace() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(Some("")).unwrap();
        assert_eq!(result, tmp.path().to_path_buf());
    }

    #[test]
    fn resolve_working_dir_valid_subdir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("subproject")).unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(Some("subproject")).unwrap();
        let expected = tmp.path().join("subproject").canonicalize().unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn resolve_working_dir_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path());

        let result = tool.resolve_working_dir(Some(".."));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("resolves outside the workspace"),
            "Expected traversal rejection, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn git_operations_work_in_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("nested");
        std::fs::create_dir(&sub).unwrap();
        git_init_no_sign(&sub, &[]);

        let tool = test_tool(tmp.path());

        let result = tool
            .execute(json!({"operation": "status", "path": "nested"}))
            .await
            .unwrap();
        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
        assert!(result.output.contains("branch"));
    }

    #[tokio::test]
    async fn git_worktree_list_works() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);

        let tool = test_tool(tmp.path());

        let result = tool
            .execute(json!({"operation": "worktree", "subcommand": "list"}))
            .await
            .unwrap();
        assert!(result.success, "Expected success, got: {:?}", result.error);

        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let worktrees = parsed["worktrees"]
            .as_array()
            .expect("worktrees must be an array");
        assert!(
            !worktrees.is_empty(),
            "Expected at least the main worktree in the list"
        );
        assert!(
            worktrees[0]["path"].as_str().is_some_and(|p| !p.is_empty()),
            "Main worktree must have a non-empty path"
        );
    }

    async fn bootstrap_repo(dir: &std::path::Path, tracked_files: &[&str]) {
        git_init_no_sign(dir, &["-b", "master"]);
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        for f in tracked_files {
            std::fs::write(dir.join(f), "initial").unwrap();
        }
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn stash_push_default_stashes_staged_and_unstaged() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["staged.txt", "unstaged.txt"]).await;

        std::fs::write(tmp.path().join("staged.txt"), "s-modified").unwrap();
        std::fs::write(tmp.path().join("unstaged.txt"), "u-modified").unwrap();
        std::process::Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({"operation": "stash", "action": "push"}))
            .await
            .unwrap();
        assert!(result.success, "stash push failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_out.trim().is_empty(),
            "expected clean working tree after default stash, got: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_keep_index_preserves_staged() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["staged.txt", "unstaged.txt"]).await;

        std::fs::write(tmp.path().join("staged.txt"), "s-modified").unwrap();
        std::fs::write(tmp.path().join("unstaged.txt"), "u-modified").unwrap();
        std::process::Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "keep_index": true,
            }))
            .await
            .unwrap();
        assert!(result.success, "stash push -k failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout).to_string();
        // `staged.txt` modification still present and staged (`M ` prefix);
        // `unstaged.txt` modification was stashed away — file matches HEAD.
        assert!(
            status_out.contains("M  staged.txt"),
            "staged modification should remain staged, status: {status_out:?}"
        );
        assert!(
            !status_out.contains("unstaged.txt"),
            "unstaged modification should have been stashed, status: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_paths_scopes_to_pathspec() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["a.txt", "b.txt"]).await;

        std::fs::write(tmp.path().join("a.txt"), "a-modified").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "b-modified").unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "paths": "a.txt",
            }))
            .await
            .unwrap();
        assert!(
            result.success,
            "stash push -- a.txt failed: {:?}",
            result.error
        );

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout).to_string();
        assert!(
            !status_out.contains("a.txt"),
            "a.txt should have been stashed, status: {status_out:?}"
        );
        assert!(
            status_out.contains("b.txt"),
            "b.txt should remain modified, status: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_custom_message() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["a.txt"]).await;
        std::fs::write(tmp.path().join("a.txt"), "a-modified").unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "message": "scoped-fix-wip",
            }))
            .await
            .unwrap();
        assert!(result.success, "stash push -m failed: {:?}", result.error);

        let list = std::process::Command::new("git")
            .args(["stash", "list"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let list_out = String::from_utf8_lossy(&list.stdout).to_string();
        assert!(
            list_out.contains("scoped-fix-wip"),
            "custom stash message missing from list, got: {list_out:?}"
        );
    }

    #[tokio::test]
    async fn stash_push_with_include_untracked_captures_new_files() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[]).await;
        std::fs::write(tmp.path().join("new.txt"), "untracked").unwrap();

        let tool = test_tool(tmp.path());
        let result = tool
            .execute(json!({
                "operation": "stash",
                "action": "push",
                "include_untracked": true,
            }))
            .await
            .unwrap();
        assert!(result.success, "stash push -u failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_out = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_out.trim().is_empty(),
            "expected clean tree after -u stash, got: {status_out:?}"
        );
    }

    #[tokio::test]
    async fn add_stages_multiple_space_separated_paths() {
        let tmp = TempDir::new().unwrap();
        git_init_no_sign(tmp.path(), &[]);
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "add", "paths": "a.txt b.txt"}))
            .await
            .unwrap();
        assert!(result.success, "add failed: {:?}", result.error);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&status.stdout);
        assert!(out.contains("A  a.txt"), "a.txt not staged: {out:?}");
        assert!(out.contains("A  b.txt"), "b.txt not staged: {out:?}");
    }

    #[tokio::test]
    async fn non_repository_error_includes_path_context_and_recovery_hint() {
        let tmp = TempDir::new().unwrap();
        // Do NOT git-init the temp dir — we want a non-repository path.
        let tool = test_tool(tmp.path());

        let result = tool.execute(json!({"operation": "status"})).await.unwrap();

        assert!(
            !result.success,
            "git_operations should fail when not in a repository"
        );

        let error = result.error.as_deref().unwrap_or("");
        let path_display = tmp.path().display().to_string();

        // The error message must include the resolved working directory
        // path so the user can see where the tool was looking.
        assert!(
            error.contains(&path_display),
            "error should contain the working directory path '{path_display}', got: {error}"
        );

        // The error message must include recovery guidance keywords
        // that tell the user how to resolve the issue.
        assert!(
            error.contains("worktree") || error.contains("work tree") || error.contains("path"),
            "error should contain a recovery keyword (worktree/work tree/path), got: {error}"
        );
        assert!(
            error.contains("initialize") || error.contains("init"),
            "error should mention initializing a repository, got: {error}"
        );
    }

    #[tokio::test]
    async fn checkout_rejects_branch_that_would_overwrite_mandatory_deny_write_target() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env"]).await;

        // Branch that changes the tracked .env content relative to master.
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        std::fs::write(tmp.path().join(".env"), "MALICIOUS=1").unwrap();
        std::process::Command::new("git")
            .args(["commit", "-am", "change env"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        // Back on master so the tool's checkout actually switches branches.
        std::process::Command::new("git")
            .args(["checkout", "master"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            deny_write: vec![tmp.path().join(".env")],
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "checkout", "branch": "feature"}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "checkout must be blocked when the target branch would overwrite a deny_write path"
        );
        assert!(
            result.error.as_deref().unwrap_or("").contains(".env"),
            "error should name the denied path, got: {:?}",
            result.error
        );

        // Must not have partially executed: still on master with the
        // original .env content untouched.
        let content = std::fs::read_to_string(tmp.path().join(".env")).unwrap();
        assert_eq!(
            content, "initial",
            ".env must be unchanged after a blocked checkout"
        );
        let branch = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "master");
    }

    #[tokio::test]
    async fn checkout_succeeds_when_target_branch_touches_no_denied_paths() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env", "docs.txt"]).await;

        std::process::Command::new("git")
            .args(["checkout", "-b", "docs"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        std::fs::write(tmp.path().join("docs.txt"), "updated docs").unwrap();
        std::process::Command::new("git")
            .args(["commit", "-am", "update docs"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        std::process::Command::new("git")
            .args(["checkout", "master"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            deny_write: vec![tmp.path().join(".env")],
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "checkout", "branch": "docs"}))
            .await
            .unwrap();

        assert!(
            result.success,
            "checkout touching only non-denied paths should succeed: {:?}",
            result.error
        );
        let content = std::fs::read_to_string(tmp.path().join("docs.txt")).unwrap();
        assert_eq!(content, "updated docs");
    }

    // ── Git tool policy boundary: deny_read on reads, deny_write on mutations ──

    /// Build a tool whose policy denies reads of `denied` (a repo-relative
    /// path). The workspace is canonicalized because the tool resolves Git's
    /// reported paths through `canonicalize_best_effort`; comparing those
    /// against a symlinked `/var` temp root would make the denial never match
    /// and the regression pass vacuously.
    fn deny_read_git_tool(root: &std::path::Path, denied: &str) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: root.to_path_buf(),
            forbidden_paths: vec![root.join(denied).display().to_string()],
            ..SecurityPolicy::default()
        });
        GitOperationsTool::new(security, root.to_path_buf())
    }

    fn deny_write_git_tool(root: &std::path::Path, denied: &str) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: root.to_path_buf(),
            deny_write: vec![root.join(denied)],
            ..SecurityPolicy::default()
        });
        GitOperationsTool::new(security, root.to_path_buf())
    }

    #[tokio::test]
    async fn diff_rejects_pathspec_that_would_read_a_denied_file() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET=leaked").unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "diff", "files": ".env"}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "diff of a deny_read target must be refused"
        );
        let rendered = format!("{result:?}");
        assert!(
            !rendered.contains("SECRET=leaked"),
            "a refused diff must not surface the denied file's content: {rendered}"
        );
    }

    #[tokio::test]
    async fn cached_diff_rejects_a_denied_file() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET=leaked").unwrap();
        std::process::Command::new("git")
            .args(["add", ".env"])
            .current_dir(&root)
            .output()
            .unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "diff", "files": ".env", "cached": true}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "the cached diff path must apply the same read policy"
        );
        assert!(
            !format!("{result:?}").contains("SECRET=leaked"),
            "a refused cached diff must not surface denied content"
        );
    }

    #[tokio::test]
    async fn diff_fails_closed_when_a_multi_file_pathspec_includes_a_denied_file() {
        // The default "." pathspec expands to several files. One denied entry
        // must abort the whole diff rather than emitting the rest — Git prints
        // all matched files in one pass, so partial filtering is not available.
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env", "notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET=leaked").unwrap();
        std::fs::write(root.join("notes.txt"), "public change").unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "diff", "files": "."}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a pathspec selecting a denied file must fail closed"
        );
        let rendered = format!("{result:?}");
        assert!(
            !rendered.contains("SECRET=leaked") && !rendered.contains("public change"),
            "failing closed must emit neither the denied nor the permitted diff: {rendered}"
        );
    }

    #[tokio::test]
    async fn diff_still_reports_a_permitted_file_under_the_same_policy() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env", "notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "public change").unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "diff", "files": "notes.txt"}))
            .await
            .unwrap();

        assert!(
            result.success,
            "a permitted pathspec must still diff normally: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn stash_push_rejected_when_it_would_revert_a_denied_file() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET=modified").unwrap();

        let tool = deny_write_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "stash", "action": "push"}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "stash push must be blocked when it would revert a deny_write path"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "SECRET=modified",
            "a blocked stash must leave the protected file untouched"
        );
        let stashes = std::process::Command::new("git")
            .args(["stash", "list"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&stashes.stdout).trim().is_empty(),
            "a blocked stash must not have created a stash entry"
        );
    }

    #[tokio::test]
    async fn stash_pop_rejected_when_it_would_restore_over_a_denied_file() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env"]).await;
        let root = tmp.path().canonicalize().unwrap();

        // Create the stash entry directly through git so the tool's own
        // push-side preflight is not what this test exercises.
        std::fs::write(root.join(".env"), "SECRET=stashed").unwrap();
        std::process::Command::new("git")
            .args(["stash", "push", "-m", "fixture"])
            .current_dir(&root)
            .output()
            .unwrap();

        let tool = deny_write_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "stash", "action": "pop"}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "stash pop must be blocked when it would write a deny_write path"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "initial",
            "a blocked pop must not restore the stashed contents"
        );
    }

    #[tokio::test]
    async fn add_rejected_when_it_would_stage_a_deny_read_file() {
        // Staging hashes the file's bytes into the object store, so `add` is a
        // read of every path it touches even though the working tree is
        // untouched.
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET=leaked").unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "add", "paths": ".env"}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "staging a deny_read target must be refused"
        );
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
            "a refused add must not have staged anything"
        );
    }

    #[tokio::test]
    async fn add_fails_closed_when_a_broad_pathspec_covers_a_denied_file() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "SECRET=leaked").unwrap();
        std::fs::write(root.join("notes.txt"), "public change").unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "add", "paths": "."}))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a pathspec expanding onto a denied file must fail closed"
        );
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
            "failing closed must stage neither the denied nor the permitted file"
        );
    }

    #[tokio::test]
    async fn add_still_stages_a_permitted_file_under_the_same_policy() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "public change").unwrap();

        let tool = deny_read_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "add", "paths": "notes.txt"}))
            .await
            .unwrap();

        assert!(
            result.success,
            "permitted add must work: {:?}",
            result.error
        );
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "notes.txt");
    }

    #[tokio::test]
    async fn worktree_remove_rejected_when_the_tree_holds_a_denied_file() {
        // `worktree remove` deletes the whole tree. The root-only check cannot
        // see a denied path nested inside it, and deletion is a write.
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        let wt = root.join("wt");

        std::process::Command::new("git")
            .args(["worktree", "add", wt.to_str().unwrap(), "-b", "side"])
            .current_dir(&root)
            .output()
            .unwrap();
        std::fs::write(wt.join("protected.txt"), "keep me").unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: root.clone(),
            deny_write: vec![wt.join("protected.txt")],
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, root.clone());

        let result = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "remove",
                "worktree_path": wt.to_str().unwrap(),
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "removing a worktree containing a deny_write path must be refused"
        );
        assert!(
            wt.join("protected.txt").exists(),
            "a blocked worktree remove must not delete the protected file"
        );
    }

    #[tokio::test]
    async fn worktree_remove_succeeds_when_the_tree_holds_no_denied_path() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        let wt = root.join("wt");

        std::process::Command::new("git")
            .args(["worktree", "add", wt.to_str().unwrap(), "-b", "side"])
            .current_dir(&root)
            .output()
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: root.clone(),
            deny_write: vec![root.join("untouched.txt")],
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, root.clone());

        let result = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "remove",
                "worktree_path": wt.to_str().unwrap(),
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "removing a worktree with no denied paths must work: {:?}",
            result.error
        );
        assert!(!wt.exists(), "the worktree should be gone");
    }

    #[tokio::test]
    async fn worktree_add_rejected_when_the_checked_out_tree_holds_a_denied_path() {
        // The target root passes the existing check; the denial is on a file
        // the branch would materialize underneath it.
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &["notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        let wt = root.join("wt");

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: root.clone(),
            deny_write: vec![wt.join("notes.txt")],
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, root.clone());

        let result = tool
            .execute(json!({
                "operation": "worktree",
                "subcommand": "add",
                "worktree_path": wt.to_str().unwrap(),
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "materializing a denied path through worktree add must be refused"
        );
        assert!(!wt.exists(), "a blocked worktree add must create nothing");
    }

    #[tokio::test]
    async fn stash_push_succeeds_when_no_denied_path_is_affected() {
        let tmp = TempDir::new().unwrap();
        bootstrap_repo(tmp.path(), &[".env", "notes.txt"]).await;
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("notes.txt"), "modified").unwrap();

        let tool = deny_write_git_tool(&root, ".env");
        let result = tool
            .execute(json!({"operation": "stash", "action": "push", "paths": "notes.txt"}))
            .await
            .unwrap();

        assert!(
            result.success,
            "stashing only permitted paths must still work: {:?}",
            result.error
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "initial",
            "the permitted file should have been reverted by the stash"
        );
    }
}
