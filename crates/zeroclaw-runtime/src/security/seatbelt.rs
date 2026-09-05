//! macOS sandbox-exec (Seatbelt) sandbox backend.

use crate::security::traits::Sandbox;
use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub(super) const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone)]
pub struct SeatbeltSandbox {
    /// Directory where per-session policy files are stored.
    policy_dir: PathBuf,
    /// Path to the generated policy file for this session.
    policy_path: PathBuf,
}

impl SeatbeltSandbox {
    /// Create a new Seatbelt sandbox, generating a per-session policy file.
    /// Returns an error if `sandbox-exec` is not available or the policy file
    /// cannot be written.
    pub fn new() -> std::io::Result<Self> {
        Self::with_workspace(None)
    }

    /// Create a new Seatbelt sandbox for the provided workspace root.
    /// If no workspace is provided, falls back to the process current
    /// directory for compatibility with direct construction.
    pub fn with_workspace(workspace: Option<&Path>) -> std::io::Result<Self> {
        Self::with_roots(workspace, &[], &[], &[])
    }

    /// Create a new Seatbelt sandbox for the provided workspace root plus the
    /// extra filesystem roots resolved from `SecurityPolicy` (carried in
    /// `SandboxExtraRoots`).
    ///
    /// Tier semantics mirror the Landlock backend:
    /// - `read_write` roots receive `file-read*` and `file-write*` grants;
    /// - `read_only` roots receive `file-read*` grants only;
    /// - `write_only` roots receive `file-write*` grants only.
    ///
    /// Seatbelt is defense in depth: this grants nothing beyond the tiers the
    /// application layer already resolved, and every path is interpolated
    /// through the Seatbelt string-literal escaping seam so a configured root
    /// cannot break out of its SBPL string literal.
    /// Existing symlinks are resolved before granting or restricting access;
    /// missing descendants are retained, but resolution errors are returned.
    pub fn with_roots(
        workspace: Option<&Path>,
        read_write: &[PathBuf],
        read_only: &[PathBuf],
        write_only: &[PathBuf],
    ) -> std::io::Result<Self> {
        if !Self::is_installed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "sandbox-exec not found (requires macOS)",
            ));
        }

        let workspace = match workspace {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir()?,
        };
        let mut symlinks = BTreeSet::new();
        let workspace = resolve_policy_root(&workspace, &mut symlinks)?;
        let mut resolve_roots = |roots: &[PathBuf]| {
            roots
                .iter()
                .map(|root| resolve_policy_root(root, &mut symlinks))
                .collect::<std::io::Result<Vec<_>>>()
        };
        let read_write = resolve_roots(read_write)?;
        let read_only = resolve_roots(read_only)?;
        let write_only = resolve_roots(write_only)?;
        let mut policy = generate_policy(&workspace, &read_write, &read_only, &write_only);
        // Traversal must survive tier denials, without granting the contents of
        // a regular file that replaces a configured symlink after construction.
        policy.push_str(&ancestor_metadata_rules(
            symlinks.iter().map(PathBuf::as_path),
        ));
        for symlink in &symlinks {
            let literal = seatbelt_string_literal(&symlink.to_string_lossy());
            policy.push_str(&format!(
                "(allow file-read-metadata (require-all (literal \"{literal}\") (vnode-type SYMLINK)))\n"
            ));
        }

        let policy_dir = std::env::temp_dir().join("zeroclaw-seatbelt");
        std::fs::create_dir_all(&policy_dir)?;
        let session_id = uuid::Uuid::new_v4();
        let policy_path = policy_dir.join(format!("{session_id}.sb"));
        std::fs::write(&policy_path, &policy)?;

        Ok(Self {
            policy_dir,
            policy_path,
        })
    }

    /// Probe if sandbox-exec is available (for auto-detection).
    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    /// Check if `sandbox-exec` is available on this system.
    fn is_installed() -> bool {
        Path::new(SANDBOX_EXEC_PATH).is_file()
    }

    /// Return the path to the generated policy file.
    pub fn policy_path(&self) -> &Path {
        &self.policy_path
    }

    /// Return the policy directory path.
    pub fn policy_dir(&self) -> &Path {
        &self.policy_dir
    }
}

impl Drop for SeatbeltSandbox {
    fn drop(&mut self) {
        // Clean up the per-session policy file
        let _ = std::fs::remove_file(&self.policy_path);
    }
}

impl Sandbox for SeatbeltSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let current_dir = cmd.get_current_dir().map(Path::to_path_buf);

        // Use the same fixed system binary checked by availability detection.
        // Resolving a bare name through PATH would happen before Seatbelt is
        // active and could select an attacker-controlled workspace executable.
        let mut sandbox_cmd = Command::new(SANDBOX_EXEC_PATH);
        sandbox_cmd.arg("-f");
        sandbox_cmd.arg(&self.policy_path);
        sandbox_cmd.arg(&program);
        sandbox_cmd.args(&args);
        if let Some(current_dir) = current_dir {
            sandbox_cmd.current_dir(current_dir);
        }

        *cmd = sandbox_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        Self::is_installed() && self.policy_path.exists()
    }

    fn name(&self) -> &str {
        "sandbox-exec"
    }

    fn description(&self) -> &str {
        "macOS Seatbelt sandbox (built-in sandbox-exec)"
    }
}

/// Resolve existing symlinks while retaining absent suffixes for future writes.
fn resolve_policy_root(path: &Path, symlinks: &mut BTreeSet<PathBuf>) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut pending: VecDeque<_> = absolute
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        match Path::new(&part).components().next() {
            Some(Component::RootDir) => resolved.push(Path::new("/")),
            Some(Component::CurDir) => {}
            Some(Component::ParentDir) => {
                resolved.pop();
            }
            Some(Component::Normal(_)) => {
                let next = resolved.join(&part);
                match next.symlink_metadata() {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        hops += 1;
                        if hops > 64 {
                            return Err(std::io::Error::other(
                                "Seatbelt root symlink limit exceeded",
                            ));
                        }
                        let target = std::fs::read_link(&next)?;
                        symlinks.insert(next);
                        for component in target.components().rev() {
                            pending.push_front(component.as_os_str().to_owned());
                        }
                    }
                    Ok(_) => resolved = next.canonicalize()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolved = next,
                    Err(error) => return Err(error),
                }
            }
            _ => return Err(std::io::Error::other("Invalid Seatbelt root component")),
        }
    }
    Ok(resolved)
}

fn seatbelt_string_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str(r"\\"),
            '"' => escaped.push_str(r#"\""#),
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            c if c.is_control() => escaped.push('?'),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Build one `(<decision> <filter> (subpath "<root>"))` rule per root, routing
/// every path through the Seatbelt string-literal escaping seam.
fn subpath_rules(decision: &str, filter: &str, roots: &[PathBuf]) -> String {
    let mut rules = String::new();
    for root in roots {
        let literal = seatbelt_string_literal(&root.to_string_lossy());
        rules.push_str(&format!("({decision} {filter} (subpath \"{literal}\"))\n"));
    }
    rules
}

fn ancestor_metadata_rules<'a>(roots: impl IntoIterator<Item = &'a Path>) -> String {
    let mut ancestors = std::collections::BTreeSet::new();
    for root in roots {
        for ancestor in root.ancestors().skip(1) {
            if ancestor != Path::new("/") {
                ancestors.insert(ancestor.to_path_buf());
            }
        }
    }

    let mut rules = String::new();
    for ancestor in ancestors {
        let literal = seatbelt_string_literal(&ancestor.to_string_lossy());
        rules.push_str(&format!(
            "(allow file-read-metadata (literal \"{literal}\"))\n"
        ));
    }
    rules
}

fn generate_policy(
    workspace: &Path,
    read_write: &[PathBuf],
    read_only: &[PathBuf],
    write_only: &[PathBuf],
) -> String {
    let workspace_str = seatbelt_string_literal(&workspace.to_string_lossy());
    let root_ancestor_rules = ancestor_metadata_rules(
        std::iter::once(workspace)
            .chain(read_write.iter().map(PathBuf::as_path))
            .chain(read_only.iter().map(PathBuf::as_path))
            .chain(write_only.iter().map(PathBuf::as_path)),
    );
    let mut extra_root_restrictions = String::new();
    extra_root_restrictions.push_str(&subpath_rules("deny", "file-write*", read_only));
    extra_root_restrictions.push_str(&subpath_rules("deny", "file-read*", write_only));

    // Seatbelt uses the last matching rule. Put policy-derived grants after the
    // restrictions so workspace and overlapping configured tiers retain the
    // union of rights authorized by SecurityPolicy.
    let mut policy_root_grants = String::new();
    policy_root_grants.push_str(&format!(
        "(allow file-read* (subpath \"{workspace_str}\"))\n"
    ));
    policy_root_grants.push_str(&format!(
        "(allow file-write* (subpath \"{workspace_str}\"))\n"
    ));
    policy_root_grants.push_str(&subpath_rules("allow", "file-read*", read_write));
    policy_root_grants.push_str(&subpath_rules("allow", "file-write*", read_write));
    policy_root_grants.push_str(&subpath_rules("allow", "file-read*", read_only));
    policy_root_grants.push_str(&subpath_rules("allow", "file-write*", write_only));
    format!(
        r#"(version 1)

;; Deny everything by default
(deny default)

;; ── Process execution ──────────────────────────────────────
;; Allow basic process operations needed for command execution
(allow process-exec)
(allow process-fork)
(allow signal (target self))

;; ── Filesystem reads ───────────────────────────────────────
;; Allow reading system libraries, frameworks, and executables
(allow file-read*
    (subpath "/usr")
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/Library")
    (subpath "/System")
    (subpath "/private/var")
    (subpath "/dev")
    (subpath "/etc")
    (subpath "/Applications")
    (subpath "/opt")
    (subpath "/nix")
    (literal "/")
    (subpath "/var"))

;; Allow reading the workspace
(allow file-read* (subpath "{workspace}"))

;; Allow reading temp directories (needed for policy file itself)
(allow file-read* (subpath "/tmp"))
(allow file-read* (subpath "/private/tmp"))
(allow file-read*
    (regex #"^/private/var/folders/"))

;; Allow reading user home for tool configs
(allow file-read*
    (regex #"^/Users/[^/]+/\\."))

;; ── Filesystem writes ──────────────────────────────────────
;; Only allow writes to workspace and temp directories
(allow file-write*
    (subpath "{workspace}"))
(allow file-write*
    (subpath "/tmp")
    (subpath "/private/tmp"))
(allow file-write*
    (regex #"^/private/var/folders/"))
(allow file-write* (subpath "/dev/null"))
(allow file-write* (subpath "/dev/tty"))

;; ── Configured extra roots ─────────────────────────────────
;; Tiered grants mirroring SecurityPolicy's resolved allowed-roots tiers:
;; read-write roots receive file-read* and file-write*, read-only roots
;; file-read* only, and write-only roots file-write* only. Seatbelt is
;; defense in depth: no root outside the resolved tiers is granted here.
;; Literal metadata grants on root ancestors permit path traversal and Git
;; canonicalization without exposing ancestor directory contents.
{root_ancestor_rules}
;; Restrictions come after the broad compatibility grants above. Policy-owned
;; workspace and tier grants follow them so overlapping policy roots compose.
{extra_root_restrictions}{policy_root_grants}

;; ── Network ────────────────────────────────────────────────
;; Deny all network by default (inherited from deny default)
;; Allow DNS resolution only
(allow network-outbound
    (remote unix-socket (path-literal "/var/run/mDNSResponder")))
(allow system-socket)

;; Allow localhost connections only (for local dev servers).
;; Note: macOS sandbox-exec only accepts "localhost:*" or "*:port" in
;; (remote ip ...) filters — raw IP addresses cause the entire policy
;; to fail to parse.
(allow network-outbound
    (remote ip "localhost:*"))

;; ── Mach / IPC ─────────────────────────────────────────────
;; Allow basic mach services needed for process execution
(allow mach-lookup
    (global-name "com.apple.system.logger")
    (global-name "com.apple.system.notification_center")
    (global-name "com.apple.SecurityServer")
    (global-name "com.apple.CoreServices.coreservicesd"))

;; ── Sysctl / misc ──────────────────────────────────────────
(allow sysctl-read)
(allow mach-task-name)
"#,
        workspace = workspace_str,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII fixture directory under the user's home directory.
    ///
    /// Deliberately NOT under the policy's broad `/tmp`, `/private/tmp`,
    /// `/private/var/folders`, system, or hidden-dotfile (`^/Users/<name>/\.`)
    /// grants: a fixture the baseline policy can already touch would make the
    /// negative sibling assertions vacuous.
    struct HomeFixture {
        path: PathBuf,
    }

    impl HomeFixture {
        fn new(label: &str) -> Self {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .expect("$HOME must be set for seatbelt integration fixtures");
            let path = home.join(format!(
                "zeroclaw-seatbelt-it-{}-{label}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&path).expect("failed to create fixture directory under $HOME");
            Self { path }
        }

        fn child(&self, name: &str) -> PathBuf {
            let path = self.path.join(name);
            std::fs::create_dir_all(&path).expect("failed to create fixture child directory");
            path
        }
    }

    impl Drop for HomeFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Run `sh -c <script>` with `$0` bound to `path` inside `sandbox`'s policy.
    fn sandboxed_sh(
        sandbox: &SeatbeltSandbox,
        cwd: &Path,
        script: &str,
        path: &Path,
    ) -> std::process::Output {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd.arg(path);
        cmd.current_dir(cwd);
        sandbox.wrap_command(&mut cmd).expect("wrap_command failed");
        cmd.output().expect("failed to spawn sandboxed command")
    }

    #[test]
    fn seatbelt_sandbox_name() {
        let sandbox = SeatbeltSandbox {
            policy_dir: PathBuf::from("/tmp/test-seatbelt"),
            policy_path: PathBuf::from("/tmp/test-seatbelt/test.sb"),
        };
        assert_eq!(sandbox.name(), "sandbox-exec");
    }

    #[test]
    fn seatbelt_description_mentions_macos() {
        let sandbox = SeatbeltSandbox {
            policy_dir: PathBuf::from("/tmp/test-seatbelt"),
            policy_path: PathBuf::from("/tmp/test-seatbelt/test.sb"),
        };
        assert!(sandbox.description().contains("macOS"));
        assert!(sandbox.description().contains("Seatbelt"));
    }

    #[test]
    fn generate_policy_contains_workspace_path() {
        let workspace = PathBuf::from("/Users/test/project");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.contains("/Users/test/project"));
    }

    #[test]
    fn generate_policy_escapes_workspace_path_string_literal() {
        let workspace = PathBuf::from("/tmp/zc\"quote\\slash\nnewline");
        let policy = generate_policy(&workspace, &[], &[], &[]);

        assert!(policy.contains(r#"(subpath "/tmp/zc\"quote\\slash\nnewline")"#));
        assert!(!policy.contains("zc\"quote\\slash\nnewline"));
    }

    #[test]
    fn generate_policy_uses_provided_workspace_for_access_rules() {
        let workspace = PathBuf::from("/tmp/zeroclaw-seatbelt-test-workspace");
        let policy = generate_policy(&workspace, &[], &[], &[]);

        assert!(
            policy.contains(
                r#"(allow file-read* (subpath "/tmp/zeroclaw-seatbelt-test-workspace"))"#
            )
        );
        assert!(policy.contains(r#"(subpath "/tmp/zeroclaw-seatbelt-test-workspace")"#));
    }

    #[test]
    fn generate_policy_denies_by_default() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.contains("(deny default)"));
    }

    #[test]
    fn generate_policy_allows_workspace_writes() {
        let workspace = PathBuf::from("/home/user/code");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.contains("(allow file-write*"));
        assert!(policy.contains("/home/user/code"));
    }

    #[test]
    fn generate_policy_restricts_network() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.contains("localhost"));
        assert!(!policy.contains("127.0.0.1"));
        assert!(!policy.contains("(allow network*)"));
    }

    #[test]
    fn generate_policy_allows_system_reads() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.contains("(subpath \"/usr\")"));
        assert!(policy.contains("(subpath \"/bin\")"));
        assert!(policy.contains("(subpath \"/System\")"));
    }

    #[test]
    fn generate_policy_allows_process_execution() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.contains("(allow process-exec)"));
        assert!(policy.contains("(allow process-fork)"));
    }

    #[test]
    fn seatbelt_wrap_command_prepends_sandbox_exec() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");
        std::fs::write(&policy_path, "(version 1)\n(deny default)").unwrap();

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path: policy_path.clone(),
        };

        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        sandbox.wrap_command(&mut cmd).unwrap();

        assert_eq!(cmd.get_program(), SANDBOX_EXEC_PATH);
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&policy_path.to_string_lossy().to_string()));
        assert!(args.contains(&"echo".to_string()));
        assert!(args.contains(&"hello".to_string()));
    }

    #[test]
    fn seatbelt_wrap_command_preserves_original_args() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");
        std::fs::write(&policy_path, "(version 1)").unwrap();

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path,
        };

        let mut cmd = Command::new("ls");
        cmd.arg("-la");
        cmd.arg("/workspace");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"ls".to_string()),
            "original program must be passed as argument"
        );
        assert!(
            args.contains(&"-la".to_string()),
            "original args must be preserved"
        );
        assert!(
            args.contains(&"/workspace".to_string()),
            "original args must be preserved"
        );
    }

    #[test]
    fn seatbelt_wrap_command_preserves_current_dir() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");
        std::fs::write(&policy_path, "(version 1)").unwrap();

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path,
        };
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        let mut cmd = Command::new("pwd");
        cmd.current_dir(&workspace);
        sandbox.wrap_command(&mut cmd).unwrap();

        assert_eq!(cmd.get_current_dir(), Some(workspace.as_path()));
    }

    #[test]
    fn seatbelt_wrap_command_ignores_workspace_launcher_on_relative_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");
        std::fs::write(&policy_path, "(version 1)").unwrap();
        let fake_launcher = dir.path().join("sandbox-exec");
        std::fs::write(&fake_launcher, "#!/bin/sh\nexit 99\n").unwrap();
        std::fs::set_permissions(&fake_launcher, std::fs::Permissions::from_mode(0o755)).unwrap();

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path,
        };
        let mut cmd = Command::new("/bin/echo");
        cmd.arg("safe");
        cmd.current_dir(dir.path());
        cmd.env("PATH", ".:/usr/bin:/bin");
        sandbox.wrap_command(&mut cmd).unwrap();

        assert!(fake_launcher.is_file());
        assert_eq!(cmd.get_program(), SANDBOX_EXEC_PATH);
        assert_eq!(cmd.get_current_dir(), Some(dir.path()));
    }

    #[test]
    fn seatbelt_wrapped_command_executes_in_current_dir() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }

        let workspace = tempfile::tempdir().unwrap();
        let sandbox = SeatbeltSandbox::with_workspace(Some(workspace.path())).unwrap();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "pwd"]);
        cmd.current_dir(workspace.path());
        sandbox.wrap_command(&mut cmd).unwrap();

        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "sandboxed pwd failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert_eq!(
            actual.canonicalize().unwrap(),
            workspace.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn seatbelt_policy_file_cleanup_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("session.sb");
        std::fs::write(&policy_path, "(version 1)").unwrap();
        assert!(policy_path.exists());

        {
            let _sandbox = SeatbeltSandbox {
                policy_dir: dir.path().to_path_buf(),
                policy_path: policy_path.clone(),
            };
        }

        assert!(
            !policy_path.exists(),
            "policy file should be cleaned up on drop"
        );
    }

    #[test]
    fn seatbelt_new_fails_if_not_installed() {
        let result = SeatbeltSandbox::new();
        match result {
            Ok(sandbox) => {
                assert_eq!(sandbox.name(), "sandbox-exec");
                assert!(sandbox.policy_path().exists());
            }
            Err(e) => {
                assert!(
                    e.kind() == std::io::ErrorKind::NotFound
                        || e.kind() == std::io::ErrorKind::PermissionDenied
                );
            }
        }
    }

    #[test]
    fn seatbelt_is_available_checks_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path: policy_path.clone(),
        };

        if Path::new(SANDBOX_EXEC_PATH).is_file() {
            assert!(
                !sandbox.is_available(),
                "should be false without policy file"
            );
        }

        std::fs::write(&policy_path, "(version 1)").unwrap();
        if Path::new(SANDBOX_EXEC_PATH).is_file() {
            assert!(sandbox.is_available(), "should be true with policy file");
        }
    }

    #[test]
    fn generate_policy_is_valid_sb_format() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[], &[], &[]);
        assert!(policy.starts_with("(version 1)"));
        let open = policy.chars().filter(|c| *c == '(').count();
        let close = policy.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close, "parentheses must be balanced in .sb policy");
    }

    #[test]
    fn generate_policy_grants_tier_specific_extra_root_rules() {
        let workspace = PathBuf::from("/ws");
        let read_write = vec![PathBuf::from("/srv/rw-root")];
        let read_only = vec![PathBuf::from("/srv/ro-root")];
        let write_only = vec![PathBuf::from("/srv/wo-root")];
        let policy = generate_policy(&workspace, &read_write, &read_only, &write_only);

        // Read-write roots receive both grants.
        assert!(policy.contains(r#"(allow file-read* (subpath "/srv/rw-root"))"#));
        assert!(policy.contains(r#"(allow file-write* (subpath "/srv/rw-root"))"#));

        // Read-only roots receive the read grant only.
        assert!(policy.contains(r#"(allow file-read* (subpath "/srv/ro-root"))"#));
        assert!(!policy.contains(r#"(allow file-write* (subpath "/srv/ro-root"))"#));
        assert!(policy.contains(r#"(deny file-write* (subpath "/srv/ro-root"))"#));

        // Write-only roots receive the write grant only.
        assert!(policy.contains(r#"(allow file-write* (subpath "/srv/wo-root"))"#));
        assert!(!policy.contains(r#"(allow file-read* (subpath "/srv/wo-root"))"#));
        assert!(policy.contains(r#"(deny file-read* (subpath "/srv/wo-root"))"#));
    }

    #[test]
    fn generate_policy_escapes_extra_root_paths() {
        let tricky = PathBuf::from("/srv/zc\"quote\\slash\nnewline");
        let policy = generate_policy(&PathBuf::from("/ws"), &[tricky], &[], &[]);

        assert!(
            policy.contains(r#"(allow file-read* (subpath "/srv/zc\"quote\\slash\nnewline"))"#)
        );
        assert!(
            policy.contains(r#"(allow file-write* (subpath "/srv/zc\"quote\\slash\nnewline"))"#)
        );
        // The raw, unescaped path must never be interpolated into the SBPL.
        assert!(!policy.contains("zc\"quote\\slash\nnewline"));
    }

    #[test]
    fn resolve_policy_root_follows_alias_chain_and_missing_suffix() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("target")).unwrap();
        std::os::unix::fs::symlink("target", root.join("inner")).unwrap();
        std::os::unix::fs::symlink("inner", root.join("outer")).unwrap();
        let mut symlinks = BTreeSet::new();
        assert_eq!(
            resolve_policy_root(&root.join("outer/new/child"), &mut symlinks).unwrap(),
            root.join("target/new/child")
        );
        assert_eq!(
            symlinks,
            BTreeSet::from([root.join("inner"), root.join("outer")])
        );
        std::os::unix::fs::symlink("target/not-created", root.join("dangling")).unwrap();
        assert_eq!(
            resolve_policy_root(&root.join("dangling/file"), &mut symlinks).unwrap(),
            root.join("target/not-created/file")
        );
    }

    #[test]
    fn resolve_policy_root_rejects_symlink_cycles() {
        let fixture = tempfile::tempdir().unwrap();
        let cycle = fixture.path().join("cycle");
        std::os::unix::fs::symlink("cycle", &cycle).unwrap();
        assert!(resolve_policy_root(&cycle, &mut BTreeSet::new()).is_err());
        if SeatbeltSandbox::is_installed() {
            assert!(
                SeatbeltSandbox::with_roots(
                    Some(fixture.path()),
                    std::slice::from_ref(&cycle),
                    &[],
                    &[],
                )
                .is_err()
            );
        }
    }

    #[test]
    fn seatbelt_alias_roots_enforce_tiers_at_process_boundary() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }
        let fixture = HomeFixture::new("aliases");
        let workspace = fixture.child("workspace");
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        // Home proves positive grants are not masked by the baseline. Temp
        // proves restrictions still override the broad compatibility grants.
        for parent in [fixture.path.as_path(), temp.path()] {
            let root = parent.canonicalize().unwrap();
            let mut canonical = Vec::new();
            let mut aliases = Vec::new();
            for tier in ["rw", "ro", "wo"] {
                let target = root.join(tier);
                std::fs::create_dir(&target).unwrap();
                std::fs::write(target.join("input.txt"), "content").unwrap();
                let intermediate = root.join(format!("{tier}-intermediate"));
                std::os::unix::fs::symlink(tier, &intermediate).unwrap();
                let alias = root.join(format!("{tier}-alias"));
                std::os::unix::fs::symlink(&intermediate, &alias).unwrap();
                canonical.push(target);
                aliases.push(alias);
            }
            let sandbox = SeatbeltSandbox::with_roots(
                Some(&workspace),
                &aliases[0..1],
                &aliases[1..2],
                &aliases[2..3],
            )
            .unwrap();
            for paths in [&canonical, &aliases] {
                for (index, path) in paths.iter().enumerate() {
                    let read =
                        sandboxed_sh(&sandbox, &workspace, r#"cat "$0""#, &path.join("input.txt"));
                    assert_eq!(
                        read.status.success(),
                        index != 2,
                        "read tier {index} at {path:?}: {}",
                        String::from_utf8_lossy(&read.stderr)
                    );
                    let write = sandboxed_sh(
                        &sandbox,
                        &workspace,
                        r#"printf x > "$0""#,
                        &path.join("output.txt"),
                    );
                    assert_eq!(
                        write.status.success(),
                        index != 1,
                        "write tier {index} at {path:?}: {}",
                        String::from_utf8_lossy(&write.stderr)
                    );
                }
            }
            assert_eq!(
                std::fs::read_to_string(canonical[1].join("input.txt")).unwrap(),
                "content"
            );
            assert!(!canonical[1].join("output.txt").exists());
        }
    }

    #[test]
    fn seatbelt_alias_traversal_does_not_grant_replacement_contents() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }
        let fixture = HomeFixture::new("alias-replacement");
        let workspace = fixture.child("workspace");
        let target = fixture.child("target");
        let outside = fixture.child("outside");
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        let alias = fixture.path.join("alias");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let sandbox =
            SeatbeltSandbox::with_roots(Some(&workspace), std::slice::from_ref(&alias), &[], &[])
                .unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::fs::write(&alias, "replacement-secret").unwrap();
        let out = sandboxed_sh(&sandbox, &workspace, r#"cat "$0""#, &alias);
        assert!(
            !out.status.success(),
            "alias traversal must not authorize replacement file contents"
        );
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&outside, &alias).unwrap();
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"cat "$0""#,
            &alias.join("secret.txt"),
        );
        assert!(
            !out.status.success(),
            "retargeted alias must not authorize an unlisted root"
        );
    }

    #[test]
    fn seatbelt_extra_root_tiers_grant_and_deny_in_real_sandbox() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }

        let fixture = HomeFixture::new("tiers");
        let workspace = fixture.child("workspace");
        let read_write = fixture.child("rw-root");
        let read_only = fixture.child("ro-root");
        let write_only = fixture.child("wo-root");
        let sibling = fixture.child("unlisted-sibling");

        std::fs::write(read_only.join("readable.txt"), "ro-content").unwrap();
        std::fs::write(sibling.join("secret.txt"), "sibling-secret").unwrap();

        let sandbox = SeatbeltSandbox::with_roots(
            Some(workspace.as_path()),
            std::slice::from_ref(&read_write),
            std::slice::from_ref(&read_only),
            std::slice::from_ref(&write_only),
        )
        .unwrap();
        assert!(sandbox.is_available());

        // The primary workspace keeps its read-write grants.
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"printf ws > "$0" && cat "$0""#,
            &workspace.join("ws-file.txt"),
        );
        assert!(
            out.status.success(),
            "workspace read/write must keep working: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Read-write root: write then read back inside the sandbox.
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"printf hello > "$0" && cat "$0""#,
            &read_write.join("written.txt"),
        );
        assert!(
            out.status.success(),
            "read-write root must allow write and read: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), "hello");

        // Read-only root: read succeeds.
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"cat "$0""#,
            &read_only.join("readable.txt"),
        );
        assert!(
            out.status.success(),
            "read-only root must allow read: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            "ro-content"
        );

        // Read-only root: writes are denied.
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"printf x >> "$0""#,
            &read_only.join("readable.txt"),
        );
        assert!(
            !out.status.success(),
            "write to a read-only root must be denied"
        );

        // Write-only root: write succeeds, reading it back is denied.
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"printf wo > "$0""#,
            &write_only.join("written.txt"),
        );
        assert!(
            out.status.success(),
            "write-only root must allow write: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"cat "$0""#,
            &write_only.join("written.txt"),
        );
        assert!(
            !out.status.success(),
            "read from a write-only root must be denied"
        );

        // An unlisted visible sibling of the configured roots stays denied.
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"cat "$0""#,
            &sibling.join("secret.txt"),
        );
        assert!(
            !out.status.success(),
            "unlisted sibling of the configured roots must stay denied"
        );
    }

    #[test]
    fn seatbelt_restricted_tiers_override_broad_temp_grants() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }

        let workspace_fixture = HomeFixture::new("temp-overlap-workspace");
        let workspace = workspace_fixture.child("workspace");
        let temp_fixture = tempfile::tempdir_in("/tmp").unwrap();
        let temp_root = temp_fixture.path().canonicalize().unwrap();
        let read_only = temp_root.join("read-only");
        let write_only = temp_root.join("write-only");
        std::fs::create_dir_all(&read_only).unwrap();
        std::fs::create_dir_all(&write_only).unwrap();
        std::fs::write(read_only.join("readable.txt"), "ro-content").unwrap();

        let sandbox = SeatbeltSandbox::with_roots(
            Some(workspace.as_path()),
            &[],
            std::slice::from_ref(&read_only),
            std::slice::from_ref(&write_only),
        )
        .unwrap();

        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"cat "$0""#,
            &read_only.join("readable.txt"),
        );
        assert!(out.status.success(), "read-only temp root must be readable");

        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"printf x >> "$0""#,
            &read_only.join("readable.txt"),
        );
        assert!(!out.status.success(), "broad temp write grant must not win");

        let written = write_only.join("written.txt");
        let out = sandboxed_sh(&sandbox, &workspace, r#"printf wo > "$0""#, &written);
        assert!(
            out.status.success(),
            "write-only temp root must be writable"
        );

        let out = sandboxed_sh(&sandbox, &workspace, r#"cat "$0""#, &written);
        assert!(!out.status.success(), "broad temp read grant must not win");
    }

    #[test]
    fn seatbelt_git_worktree_status_with_main_checkout_read_write() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }

        let fixture = HomeFixture::new("git-worktree");
        let main = fixture.child("main");
        // `git worktree add` creates the worktree directory itself.
        let worktree = fixture.path.join("linked-worktree");

        // Fixture setup runs unsandboxed; only `git status` below is confined.
        let main_s = main.to_string_lossy().into_owned();
        let worktree_s = worktree.to_string_lossy().into_owned();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .output()
                .expect("git on PATH");
            assert!(
                out.status.success(),
                "git fixture setup failed ({args:?}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", main_s.as_str()]);
        run(&[
            "-C",
            main_s.as_str(),
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ]);
        run(&[
            "-C",
            main_s.as_str(),
            "worktree",
            "add",
            worktree_s.as_str(),
            "-b",
            "side",
        ]);
        // Make the worktree dirty so a successful `git status --short` must
        // have inspected THIS worktree, not some ambient checkout.
        std::fs::write(worktree.join("notes.txt"), "dirty\n").unwrap();

        let sandbox = SeatbeltSandbox::with_roots(
            Some(worktree.as_path()),
            std::slice::from_ref(&main),
            &[],
            &[],
        )
        .unwrap();
        assert!(sandbox.is_available());

        // Pass the worktree via `git -C` rather than `Command::current_dir`:
        // `wrap_command` reconstructs the command, so this regression must not
        // depend on the configured current directory surviving the wrap.
        let mut cmd = Command::new("/usr/bin/env");
        cmd.args([
            "GIT_CONFIG_GLOBAL=/dev/null",
            "GIT_CONFIG_NOSYSTEM=1",
            "/usr/bin/git",
            "-C",
            worktree_s.as_str(),
            "-c",
            "status.showUntrackedFiles=normal",
            "status",
            "--short",
        ]);
        sandbox.wrap_command(&mut cmd).unwrap();
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "git status --short in a linked worktree must succeed when the main checkout \
             git metadata root is configured read-write: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("?? notes.txt"),
            "status must report the untracked file, proving it inspected the linked \
             worktree rather than the ambient directory: {stdout}"
        );
    }

    #[test]
    fn seatbelt_with_workspace_grants_no_extra_roots() {
        if !SeatbeltSandbox::is_installed() {
            return;
        }

        let fixture = HomeFixture::new("empty-tiers");
        let workspace = fixture.child("workspace");
        let sibling = fixture.child("outside");
        std::fs::write(sibling.join("secret.txt"), "secret").unwrap();

        let sandbox = SeatbeltSandbox::with_workspace(Some(workspace.as_path())).unwrap();
        let out = sandboxed_sh(
            &sandbox,
            &workspace,
            r#"cat "$0""#,
            &sibling.join("secret.txt"),
        );
        assert!(
            !out.status.success(),
            "with_workspace must grant nothing outside the workspace"
        );
    }
}
