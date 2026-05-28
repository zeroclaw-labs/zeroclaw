use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;

use super::types::{FileDiff, FileStatus, Patch};

const SIZE_LIMIT: u64 = 2 * 1024 * 1024; // 2 MB — skip large untracked files
const PRUNE_PERIOD: &str = "7.days";

/// Internal result of a `git` invocation.
struct GitResult {
    code: i32,
    text: String,
    stderr: String,
}

/// A shadow git repository that stores worktree state as git tree objects
/// (no commits, no branches). Stored outside the user's repo at
/// `<data_dir>/snapshot/<project_hash>/<worktree_hash>/`.
pub struct ShadowSnapshot {
    /// Path to the shadow bare gitdir.
    gitdir: PathBuf,
    /// User's working tree root (also used as cwd for git commands).
    worktree: PathBuf,
    /// Serialises all mutating git operations to prevent index corruption.
    lock: Arc<Mutex<()>>,
}

impl ShadowSnapshot {
    /// Create a `ShadowSnapshot` for `working_dir`, or return `None` when
    /// git is not on PATH or the directory is not inside a git repository.
    ///
    /// `data_dir` is the root under which `snapshot/<project>/<worktree>/`
    /// shadow gitdirs are kept (typically `config.data_dir`).
    pub fn for_session(working_dir: &Path, data_dir: &Path) -> Option<Self> {
        if which::which("git").is_err() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "snapshot: git not on PATH, disabled"
            );
            return None;
        }
        let repo_root = find_repo_root(working_dir)?;
        let project_hash = path_hash(&repo_root);
        let worktree_hash = path_hash(working_dir);
        let gitdir = data_dir
            .join("snapshot")
            .join(project_hash)
            .join(worktree_hash);
        Some(Self {
            gitdir,
            worktree: working_dir.to_path_buf(),
            lock: Arc::new(Mutex::new(())),
        })
    }

    // -------------------------------------------------------------------------
    // Public API
    // -------------------------------------------------------------------------

    /// Stage changed/untracked files and record a tree object.
    /// Returns the tree hash, or `None` on error / git unavailable.
    pub async fn track(&self) -> Option<String> {
        let _g = self.lock.lock().await;
        self.do_track().await
    }

    /// List files changed since `hash`. Returns absolute forward-slash paths.
    pub async fn patch(&self, hash: &str) -> Patch {
        let _g = self.lock.lock().await;
        self.do_patch(hash).await
    }

    /// Restore the entire worktree to the tree captured by `hash`.
    pub async fn restore(&self, hash: &str) {
        let _g = self.lock.lock().await;
        self.do_restore(hash).await;
    }

    /// Revert individual files to their state at each patch's hash.
    pub async fn revert(&self, patches: &[Patch]) {
        let _g = self.lock.lock().await;
        self.do_revert(patches).await;
    }

    /// Unified diff of the current worktree vs `hash`.
    pub async fn diff(&self, hash: &str) -> String {
        let _g = self.lock.lock().await;
        self.do_diff(hash).await
    }

    /// Per-file diff statistics + inline unified diffs between two tree hashes.
    pub async fn diff_full(&self, from: &str, to: &str) -> Vec<FileDiff> {
        let _g = self.lock.lock().await;
        self.do_diff_full(from, to).await
    }

    /// Prune unreachable git objects older than 7 days.
    pub async fn cleanup(&self) {
        let _g = self.lock.lock().await;
        if !self.gitdir.exists() {
            return;
        }
        let prune_arg = format!("--prune={PRUNE_PERIOD}");
        let r = self.shadow(vec!["gc", &prune_arg]).await;
        if r.code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"stderr": r.stderr.trim()})),
                "snapshot gc failed"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Implementations
    // -------------------------------------------------------------------------

    async fn do_track(&self) -> Option<String> {
        let existed = self.gitdir.exists();
        if let Err(e) = tokio::fs::create_dir_all(&self.gitdir).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "snapshot: cannot create gitdir"
            );
            return None;
        }
        if !existed && !self.init_shadow().await {
            return None;
        }
        self.stage().await;
        let r = self.shadow(vec!["write-tree"]).await;
        if r.code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"stderr": r.stderr.trim()})),
                "snapshot write-tree failed"
            );
            return None;
        }
        let hash = r.text.trim().to_string();
        Some(hash)
    }

    async fn do_patch(&self, hash: &str) -> Patch {
        self.stage().await;
        let r = self
            .shadow(vec![
                "-c",
                "core.quotepath=false",
                "diff",
                "--cached",
                "--no-ext-diff",
                "--name-only",
                hash,
                "--",
                ".",
            ])
            .await;
        if r.code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"hash": hash, "stderr": r.stderr.trim()})),
                "snapshot patch failed"
            );
            return Patch {
                hash: hash.to_string(),
                files: vec![],
            };
        }
        let names: Vec<&str> = r
            .text
            .trim()
            .split('\n')
            .filter(|s| !s.is_empty())
            .collect();
        let ignored = self.check_ignore_user(&names).await;
        let files = names
            .into_iter()
            .filter(|f| !ignored.contains(*f))
            .map(|f| to_abs_fwd(&self.worktree, f))
            .collect();
        Patch {
            hash: hash.to_string(),
            files,
        }
    }

    async fn do_restore(&self, hash: &str) {
        let r = self
            .shadow(vec![
                "-c",
                "core.longpaths=true",
                "-c",
                "core.symlinks=true",
                "read-tree",
                hash,
            ])
            .await;
        if r.code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"hash": hash, "stderr": r.stderr.trim()})),
                "snapshot read-tree failed"
            );
            return;
        }
        let r2 = self
            .shadow(vec![
                "-c",
                "core.longpaths=true",
                "-c",
                "core.symlinks=true",
                "checkout-index",
                "-a",
                "-f",
            ])
            .await;
        if r2.code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"stderr": r2.stderr.trim()})),
                "snapshot checkout-index failed"
            );
        }
    }

    async fn do_revert(&self, patches: &[Patch]) {
        struct Op {
            hash: String,
            abs: PathBuf,
            rel: String,
        }

        let mut ops: Vec<Op> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for patch in patches {
            for abs in &patch.files {
                let key = fwd_str(abs);
                if !seen.insert(key) {
                    continue;
                }
                let rel = abs
                    .strip_prefix(&self.worktree)
                    .map(fwd_str)
                    .unwrap_or_else(|_| fwd_str(abs));
                ops.push(Op {
                    hash: patch.hash.clone(),
                    abs: abs.clone(),
                    rel,
                });
            }
        }

        let clash = |a: &str, b: &str| {
            a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
        };

        let mut i = 0;
        while i < ops.len() {
            let first_hash = ops[i].hash.clone();
            let mut end = i + 1;
            while end < ops.len() && end - i < 100 {
                let next = &ops[end];
                if next.hash != first_hash {
                    break;
                }
                if ops[i..end].iter().any(|o| clash(&o.rel, &next.rel)) {
                    break;
                }
                end += 1;
            }
            let run = &ops[i..end];

            if run.len() == 1 {
                self.revert_one(&ops[i].hash, &ops[i].abs, &ops[i].rel)
                    .await;
                i = end;
                continue;
            }

            // Discover which files exist in this tree snapshot.
            let rels: Vec<&str> = run.iter().map(|o| o.rel.as_str()).collect();
            let mut ls_args = vec!["ls-tree", "--name-only", &first_hash, "--"];
            ls_args.extend_from_slice(&rels);
            let tree_r = self.shadow(ls_args).await;

            if tree_r.code != 0 {
                for op in run {
                    self.revert_one(&op.hash, &op.abs, &op.rel).await;
                }
                i = end;
                continue;
            }

            let have: HashSet<&str> = tree_r
                .text
                .trim()
                .split('\n')
                .filter(|s| !s.is_empty())
                .collect();

            // Batch-checkout files that exist in the tree.
            let present: Vec<&Op> = run
                .iter()
                .filter(|o| have.contains(o.rel.as_str()))
                .collect();
            if !present.is_empty() {
                let mut args: Vec<String> = vec![
                    "-c".into(),
                    "core.longpaths=true".into(),
                    "-c".into(),
                    "core.symlinks=true".into(),
                ];
                args.extend(self.shadow_prefix());
                args.extend(["checkout".into(), first_hash.clone(), "--".into()]);
                args.extend(present.iter().map(|o| o.abs.to_string_lossy().into_owned()));
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let r = self.raw_git(&refs, None).await;
                if r.code != 0 {
                    for op in run {
                        self.revert_one(&op.hash, &op.abs, &op.rel).await;
                    }
                    i = end;
                    continue;
                }
            }

            // Delete files absent from the tree.
            for op in run {
                if !have.contains(op.rel.as_str()) {
                    let _ = tokio::fs::remove_file(&op.abs).await;
                }
            }
            i = end;
        }
    }

    async fn revert_one(&self, hash: &str, abs: &Path, rel: &str) {
        let abs_s = abs.to_string_lossy().into_owned();
        let mut args: Vec<String> = vec![
            "-c".into(),
            "core.longpaths=true".into(),
            "-c".into(),
            "core.symlinks=true".into(),
        ];
        args.extend(self.shadow_prefix());
        args.extend(["checkout".into(), hash.into(), "--".into(), abs_s]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = self.raw_git(&refs, None).await;
        if r.code == 0 {
            return;
        }
        // If the file didn't exist in that snapshot, delete it.
        let ls_r = self.shadow(vec!["ls-tree", hash, "--", rel]).await;
        if ls_r.code == 0 && ls_r.text.trim().is_empty() {
            let _ = tokio::fs::remove_file(abs).await;
        }
    }

    async fn do_diff(&self, hash: &str) -> String {
        self.stage().await;
        let r = self
            .shadow(vec![
                "-c",
                "core.quotepath=false",
                "diff",
                "--cached",
                "--no-ext-diff",
                hash,
                "--",
                ".",
            ])
            .await;
        if r.code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"hash": hash, "stderr": r.stderr.trim()})),
                "snapshot diff failed"
            );
            return String::new();
        }
        r.text.trim().to_string()
    }

    async fn do_diff_full(&self, from: &str, to: &str) -> Vec<FileDiff> {
        // Status per file.
        let status_r = self
            .shadow(vec![
                "-c",
                "core.quotepath=false",
                "diff",
                "--no-ext-diff",
                "--name-status",
                "--no-renames",
                from,
                to,
                "--",
                ".",
            ])
            .await;
        let mut status_map: HashMap<String, FileStatus> = HashMap::new();
        for line in status_r.text.trim().split('\n').filter(|l| !l.is_empty()) {
            let mut parts = line.splitn(2, '\t');
            let code = parts.next().unwrap_or("");
            let file = parts.next().unwrap_or("").to_string();
            let s = match code.chars().next() {
                Some('A') => FileStatus::Added,
                Some('D') => FileStatus::Deleted,
                _ => FileStatus::Modified,
            };
            status_map.insert(file, s);
        }

        // Numstat for add/del line counts.
        let num_r = self
            .shadow(vec![
                "-c",
                "core.quotepath=false",
                "diff",
                "--no-ext-diff",
                "--no-renames",
                "--numstat",
                from,
                to,
                "--",
                ".",
            ])
            .await;

        struct Row {
            file: String,
            adds: u32,
            dels: u32,
            binary: bool,
        }
        let mut rows: Vec<Row> = Vec::new();
        for line in num_r.text.trim().split('\n').filter(|l| !l.is_empty()) {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() < 3 {
                continue;
            }
            let binary = parts[0] == "-" && parts[1] == "-";
            rows.push(Row {
                file: parts[2].to_string(),
                adds: if binary {
                    0
                } else {
                    parts[0].parse().unwrap_or(0)
                },
                dels: if binary {
                    0
                } else {
                    parts[1].parse().unwrap_or(0)
                },
                binary,
            });
        }

        // Filter gitignored (collect to owned Strings so we don't borrow `rows`).
        let ignored: HashSet<String> = {
            let names: Vec<&str> = rows.iter().map(|r| r.file.as_str()).collect();
            self.check_ignore_user(&names)
                .await
                .into_iter()
                .map(|s| s.to_string())
                .collect()
        };
        let rows: Vec<Row> = rows
            .into_iter()
            .filter(|r| !ignored.contains(&r.file))
            .collect();

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let status = status_map.remove(&row.file);
            let patch_text = if row.binary {
                String::new()
            } else {
                let (before, after) = self.show_pair(&row.file, from, to, status.as_ref()).await;
                make_unified_diff(&row.file, &before, &after)
            };
            result.push(FileDiff {
                file: row.file,
                additions: row.adds,
                deletions: row.dels,
                status,
                patch: patch_text,
            });
        }
        result
    }

    // -------------------------------------------------------------------------
    // Shadow repo management
    // -------------------------------------------------------------------------

    async fn init_shadow(&self) -> bool {
        let gitdir_s = self.gitdir.to_string_lossy().into_owned();
        let worktree_s = self.worktree.to_string_lossy().into_owned();
        let mut cmd = TokioCommand::new("git");
        cmd.arg("init")
            .env("GIT_DIR", &gitdir_s)
            .env("GIT_WORK_TREE", &worktree_s)
            .current_dir(&self.worktree)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = cmd.status().await;
        for (k, v) in [
            ("core.autocrlf", "false"),
            ("core.longpaths", "true"),
            ("core.symlinks", "true"),
            ("core.fsmonitor", "false"),
        ] {
            self.raw_git(&["--git-dir", &gitdir_s, "config", k, v], None)
                .await;
        }
        true
    }

    async fn stage(&self) {
        self.sync_excludes(&[]).await;

        let diff_r = self
            .shadow(vec![
                "-c",
                "core.quotepath=false",
                "diff-files",
                "--name-only",
                "-z",
                "--",
                ".",
            ])
            .await;
        let tracked = if diff_r.code == 0 {
            split_nul(&diff_r.text)
        } else {
            vec![]
        };

        let ls_r = self
            .shadow(vec![
                "-c",
                "core.quotepath=false",
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                ".",
            ])
            .await;
        let untracked = if ls_r.code == 0 {
            split_nul(&ls_r.text)
        } else {
            vec![]
        };

        let all: Vec<String> = {
            let mut s: HashSet<String> = HashSet::new();
            for f in tracked.iter().chain(untracked.iter()) {
                s.insert(f.clone());
            }
            let mut v: Vec<_> = s.into_iter().collect();
            v.sort();
            v
        };
        if all.is_empty() {
            return;
        }
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let ignored = self.check_ignore_user(&refs).await;

        // Drop now-ignored files from shadow index.
        let to_drop: Vec<String> = all
            .iter()
            .filter(|f| ignored.contains(f.as_str()))
            .cloned()
            .collect();
        if !to_drop.is_empty() {
            let stdin = nul_list(&to_drop);
            self.shadow_stdin(
                vec![
                    "rm",
                    "--cached",
                    "-f",
                    "--ignore-unmatch",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                stdin,
            )
            .await;
        }

        // Skip large untracked files (exclude them from shadow).
        let mut large: Vec<String> = Vec::new();
        let mut stageable: Vec<String> = Vec::new();
        for f in &all {
            if ignored.contains(f.as_str()) {
                continue;
            }
            if untracked.contains(f)
                && let Ok(meta) = tokio::fs::metadata(self.worktree.join(f)).await
                && meta.len() > SIZE_LIMIT
            {
                large.push(f.clone());
                continue;
            }
            stageable.push(f.clone());
        }
        if !large.is_empty() {
            self.sync_excludes(&large).await;
        }
        if !stageable.is_empty() {
            let stdin = nul_list(&stageable);
            let r = self
                .shadow_stdin(
                    vec![
                        "add",
                        "--all",
                        "--sparse",
                        "--pathspec-from-file=-",
                        "--pathspec-file-nul",
                    ],
                    stdin,
                )
                .await;
            if r.code != 0 {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"stderr": r.stderr.trim()})),
                    "snapshot stage failed"
                );
            }
        }
    }

    async fn sync_excludes(&self, extra: &[String]) {
        let user_content = self.read_user_exclude().await;
        let info_dir = self.gitdir.join("info");
        let _ = tokio::fs::create_dir_all(&info_dir).await;
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = user_content {
            let t = c.trim_end().to_string();
            if !t.is_empty() {
                parts.push(t);
            }
        }
        for p in extra {
            parts.push(format!("/{}", p.replace('\\', "/")));
        }
        let content = if parts.is_empty() {
            String::new()
        } else {
            format!("{}\n", parts.join("\n"))
        };
        let _ = tokio::fs::write(info_dir.join("exclude"), content).await;
    }

    async fn read_user_exclude(&self) -> Option<String> {
        let worktree_s = self.worktree.to_string_lossy().into_owned();
        let r = self
            .raw_git(
                &[
                    "-C",
                    &worktree_s,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/exclude",
                ],
                None,
            )
            .await;
        let path = r.text.trim().to_string();
        if path.is_empty() {
            return None;
        }
        tokio::fs::read_to_string(&path).await.ok()
    }

    async fn check_ignore_user<'a>(&self, files: &[&'a str]) -> HashSet<&'a str> {
        if files.is_empty() {
            return HashSet::new();
        }
        let stdin: Vec<u8> = files
            .iter()
            .flat_map(|f| {
                let mut b = f.as_bytes().to_vec();
                b.push(0);
                b
            })
            .collect();
        let dot_git = self.worktree.join(".git").to_string_lossy().into_owned();
        let worktree_s = self.worktree.to_string_lossy().into_owned();
        let r = self
            .raw_git(
                &[
                    "-c",
                    "core.quotepath=false",
                    "--git-dir",
                    &dot_git,
                    "--work-tree",
                    &worktree_s,
                    "check-ignore",
                    "--no-index",
                    "--stdin",
                    "-z",
                ],
                Some(stdin),
            )
            .await;
        if r.code != 0 && r.code != 1 {
            return HashSet::new();
        }
        let raw: Vec<&str> = r.text.split('\0').filter(|s| !s.is_empty()).collect();
        files.iter().copied().filter(|f| raw.contains(f)).collect()
    }

    async fn show_blob(&self, hash: &str, file: &str) -> String {
        let spec = format!("{hash}:{file}");
        let r = self.shadow(vec!["show", &spec]).await;
        if r.code == 0 { r.text } else { String::new() }
    }

    async fn show_pair(
        &self,
        file: &str,
        from: &str,
        to: &str,
        status: Option<&FileStatus>,
    ) -> (String, String) {
        match status {
            Some(FileStatus::Added) => (String::new(), self.show_blob(to, file).await),
            Some(FileStatus::Deleted) => (self.show_blob(from, file).await, String::new()),
            _ => tokio::join!(self.show_blob(from, file), self.show_blob(to, file)),
        }
    }

    // -------------------------------------------------------------------------
    // Raw git invocation
    // -------------------------------------------------------------------------

    async fn raw_git(&self, args: &[&str], stdin_data: Option<Vec<u8>>) -> GitResult {
        let mut cmd = TokioCommand::new("git");
        cmd.args(args)
            .current_dir(&self.worktree)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if stdin_data.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return GitResult {
                    code: 1,
                    text: String::new(),
                    stderr: e.to_string(),
                };
            }
        };
        if let Some(data) = stdin_data
            && let Some(mut si) = child.stdin.take()
        {
            let _ = si.write_all(&data).await;
        }
        match child.wait_with_output().await {
            Ok(out) => GitResult {
                code: out.status.code().unwrap_or(1),
                text: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Err(e) => GitResult {
                code: 1,
                text: String::new(),
                stderr: e.to_string(),
            },
        }
    }

    fn shadow_prefix(&self) -> Vec<String> {
        vec![
            "--git-dir".into(),
            self.gitdir.to_string_lossy().into_owned(),
            "--work-tree".into(),
            self.worktree.to_string_lossy().into_owned(),
        ]
    }

    async fn shadow(&self, extra: Vec<&str>) -> GitResult {
        let mut args: Vec<String> = self.shadow_prefix();
        args.extend(extra.into_iter().map(String::from));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.raw_git(&refs, None).await
    }

    async fn shadow_stdin(&self, extra: Vec<&str>, stdin: Vec<u8>) -> GitResult {
        let mut args: Vec<String> = self.shadow_prefix();
        args.extend(extra.into_iter().map(String::from));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.raw_git(&refs, Some(stdin)).await
    }
}

// -------------------------------------------------------------------------
// Free helpers
// -------------------------------------------------------------------------

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn path_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let bytes = canonical.to_string_lossy().into_owned().into_bytes();
    hex::encode(&Sha256::digest(&bytes)[..8])
}

fn fwd_str(p: impl AsRef<Path>) -> String {
    p.as_ref().to_string_lossy().replace('\\', "/")
}

fn to_abs_fwd(base: &Path, rel: &str) -> PathBuf {
    PathBuf::from(base.join(rel).to_string_lossy().replace('\\', "/"))
}

fn split_nul(s: &str) -> Vec<String> {
    s.split('\0')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

fn nul_list(files: &[String]) -> Vec<u8> {
    files
        .iter()
        .flat_map(|f| {
            let mut b = f.as_bytes().to_vec();
            b.push(0);
            b
        })
        .collect()
}

fn make_unified_diff(file: &str, before: &str, after: &str) -> String {
    use similar::TextDiff;
    TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(1_000_000)
        .header(&format!("a/{file}"), &format!("b/{file}"))
        .to_string()
}
