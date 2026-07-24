//! Landlock sandbox (Linux kernel 5.13+ LSM)
//! Landlock provides unprivileged sandboxing through the Linux kernel.
//! This module uses the pure-Rust `landlock` crate for filesystem access control.

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use landlock::{
    AccessFs, Errno, PathBeneath, PathFd, PathFdError, Ruleset, RulesetAttr, RulesetCreated,
    RulesetCreatedAttr,
};
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use std::os::unix::process::CommandExt;
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use std::path::Path;

use crate::security::traits::Sandbox;

/// Landlock sandbox backend for Linux
#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
#[derive(Debug)]
pub struct LandlockSandbox {
    workspace_dir: Option<std::path::PathBuf>,
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
impl LandlockSandbox {
    /// Create a new Landlock sandbox with the given workspace directory
    pub fn new() -> std::io::Result<Self> {
        Self::with_workspace(None)
    }

    /// Create a Landlock sandbox with a specific workspace directory
    pub fn with_workspace(workspace_dir: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        // Test if Landlock is available by trying to create a minimal ruleset
        let test_ruleset = Ruleset::default()
            .handle_access(AccessFs::ReadFile | AccessFs::WriteFile)
            .and_then(|ruleset| ruleset.create());

        match test_ruleset {
            Ok(_) => Ok(Self { workspace_dir }),
            Err(e) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Landlock not available"
                );
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Landlock not available",
                ))
            }
        }
    }

    /// Probe if Landlock is available (for auto-detection)
    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    /// Build a Landlock ruleset with all configured access rules.
    ///
    /// The ruleset is **not** enforced here. Enforcement happens in the
    /// child process via `pre_exec` (see `wrap_command`), so only the
    /// child is restricted — the daemon (parent) process is never affected.
    fn build_ruleset(&self) -> std::io::Result<RulesetCreated> {
        let mut ruleset = Ruleset::default()
            .handle_access(
                AccessFs::Execute
                    | AccessFs::WriteFile
                    | AccessFs::ReadFile
                    | AccessFs::Truncate
                    | AccessFs::ReadDir
                    | AccessFs::RemoveDir
                    | AccessFs::RemoveFile
                    | AccessFs::MakeChar
                    | AccessFs::MakeDir
                    | AccessFs::MakeReg
                    | AccessFs::MakeSock
                    | AccessFs::MakeFifo
                    | AccessFs::MakeBlock
                    | AccessFs::MakeSym,
            )
            .and_then(|ruleset| ruleset.create())
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // Allow workspace directory (read/write/execute).
        // If a workspace was supplied but doesn't exist, fail closed rather than
        // silently applying restrictions without a rule for it.
        if let Some(ref workspace) = self.workspace_dir {
            let workspace_fd =
                PathFd::new(workspace).map_err(|e| std::io::Error::other(e.to_string()))?;
            ruleset = ruleset
                .add_rule(PathBeneath::new(
                    workspace_fd,
                    AccessFs::Execute
                        | AccessFs::WriteFile
                        | AccessFs::ReadFile
                        | AccessFs::Truncate
                        | AccessFs::ReadDir
                        | AccessFs::RemoveDir
                        | AccessFs::RemoveFile
                        | AccessFs::MakeDir
                        | AccessFs::MakeReg
                        | AccessFs::MakeSock
                        | AccessFs::MakeFifo
                        | AccessFs::MakeSym,
                ))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }

        // Allow paths for general operations.
        // `required = true`  -> fail closed if the path is missing (baseline devices, system roots).
        // `required = false` -> skip on NotFound (distro-optional loader/layout paths).
        for (allow_path, perm, required) in [
            // /tmp: general temp directory for child processes (pipes, sockets, temp files).
            // Execute is intentionally omitted to prevent running untrusted binaries from /tmp.
            (
                "/tmp",
                AccessFs::Truncate | AccessFs::WriteFile | AccessFs::ReadFile,
                true,
            ),
            // Linux dynamic linker (ld-linux-yourarch.so.version) which designed to run on FHS 3.0
            // system will read the following file/directories to retrieve dynamic linker config.
            // These are optional: minimal systems may not have all of them.
            ("/etc/ld.so.cache", AccessFs::ReadFile.into(), false),
            ("/etc/ld.so.conf", AccessFs::ReadFile.into(), false),
            ("/etc/ld.so.preload", AccessFs::ReadFile.into(), false),
            (
                "/etc/ld.so.conf.d",
                AccessFs::ReadFile | AccessFs::ReadDir,
                false,
            ),
            // In FHS 3.0 systems, system binaries will live in the following directories:
            // /usr/bin, /usr/lib, /usr/lib64, /bin, /lib, /lib64.
            // Execute: needed to run binaries (execve) and for the dynamic linker's
            // access(X_OK) checks on shared libraries.
            //
            // /usr is optional: Non-FHS distros may not have it.
            (
                "/usr",
                AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
                false,
            ),
            (
                "/bin",
                AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
                true,
            ),
            // /lib and /lib64 are distro-optional: some systems have one, some both.
            (
                "/lib",
                AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
                false,
            ),
            (
                "/lib64",
                AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
                false,
            ),
            // some variant of sh requires access to /dev/null
            ("/dev/null", AccessFs::WriteFile | AccessFs::ReadFile, true),
        ] {
            match PathFd::new(Path::new(allow_path)) {
                Ok(path_fd) => {
                    ruleset = ruleset
                        .add_rule(PathBeneath::new(path_fd, perm))
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                }
                Err(PathFdError::OpenCall { source, .. }) => {
                    if source.kind() == std::io::ErrorKind::NotFound {
                        if required {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!(
                                    "Required path {allow_path} not found for Landlock sandbox"
                                ),
                            ));
                        }
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            format!(
                                "Failed to create PathFd for a nonexistent path {}.",
                                allow_path,
                            ),
                        );
                    } else {
                        Err(std::io::Error::other(source.to_string()))?;
                    }
                }
                Err(e) => {
                    Err(std::io::Error::other(e.to_string()))?;
                }
            }
        }

        // Return the ruleset WITHOUT enforcing it.
        // Enforcement is deferred to the child process via pre_exec
        // (see wrap_command), which calls restrict_self() after fork()
        // but before exec(). This prevents the daemon from locking itself.
        Ok(ruleset)
    }
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
impl Sandbox for LandlockSandbox {
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()> {
        // Build the ruleset in the parent process where allocation is safe.
        // `RulesetCreated` is `Send + Sync + 'static`, which is necessary
        // for the value to be moved into the `pre_exec` closure (the closure
        // must be `Send`), but this bound alone does not make the closure
        // fork-safe — see the invariants below.
        let mut ruleset = Some(self.build_ruleset()?);

        // Enforce Landlock **only in the child process** via pre_exec,
        // which runs after fork() but before exec(). The daemon (parent)
        // is never restricted.
        //
        // SAFETY: `pre_exec` runs in a forked child after fork() but before
        // exec(). In a multi-threaded process, only async-signal-safe
        // operations are guaranteed correct in this window. The closure
        // must not allocate heap memory, acquire locks, or call
        // async-signal-unsafe functions on the success path.
        //
        // The closure performs three operations:
        //
        // 1. `ruleset.take()` — `Option::take()`. Moves the `RulesetCreated`
        //    out of the `Option`. Pure memory manipulation: no allocation,
        //    no syscall, no lock.
        //
        // 2. `rs.restrict_self()` — consumes the `RulesetCreated`. Internally
        //    issues `prctl(PR_SET_NO_NEW_PRIVS)` and `landlock_restrict_self()`,
        //    both raw syscalls, but also performs compatibility and status
        //    bookkeeping (e.g. checking Landlock ABI version, updating internal
        //    best-effort restriction state). These bookkeeping operations read
        //    and write stack-local or already-allocated fields; they do not
        //    allocate heap memory or acquire locks on the success path.
        //    On return, `rs` is dropped, which closes the ruleset file
        //    descriptor via another raw syscall.
        //
        //    Errors are translated to `io::Error::from_raw_os_error()` via
        //    `landlock::Errno`, which extracts the raw errno from the
        //    `RulesetError`'s source chain. `from_raw_os_error` stores the
        //    error as `Repr::Os(i32)` — no heap allocation, no formatting.
        //    `Errno::from` walks `error.source()` (a reference) and calls
        //    `raw_os_error()` (reads an `i32`); dropping the consumed error
        //    frees no heap since the underlying `io::Error` is also
        //    `Repr::Os(i32)`. The parent receives a proper `Err` from
        //    `spawn()`. `std` installs `always_abort()` before invoking
        //    `pre_exec` as a safety net, but the closure does not rely on it
        //    for normal operation.
        //
        // 3. Same-child defensive guard — `ruleset.take()` returns `None` only
        //    if `pre_exec` were invoked twice within the *same* forked child.
        //    Repeated `Command::spawn()` calls fork distinct children, each
        //    receiving its own copy of the `Option` (fork copies the parent's
        //    memory), so the parent's captured `Some` is never consumed.
        //    Because `pre_exec` runs at most once per fork, this branch is
        //    unreachable; it returns `EINVAL` via `from_raw_os_error()` as a
        //    defensive guard. No allocation, no panic.
        //
        // Re-audit obligation: any version bump of the `landlock` crate
        // requires re-verifying that `RulesetCreated::restrict_self()` and
        // `Drop for RulesetCreated` remain fork-safe — no heap allocation,
        // no lock acquisition, no async-signal-unsafe calls between fork()
        // and exec().
        unsafe {
            cmd.pre_exec(move || {
                if let Some(rs) = ruleset.take() {
                    rs.restrict_self()
                        .map_err(|e| std::io::Error::from_raw_os_error(*Errno::from(e)))?;
                } else {
                    // Unreachable: `pre_exec` is called exactly once per
                    // fork, and each forked child receives its own copy of
                    // `ruleset` (always `Some` on first entry). Kept as a
                    // defensive guard against same-child double-invocation.
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                Ok(())
            });
        }

        Ok(())
    }

    fn is_available(&self) -> bool {
        // Try to create a minimal ruleset to verify availability
        Ruleset::default()
            .handle_access(AccessFs::ReadFile)
            .and_then(|ruleset| ruleset.create())
            .is_ok()
    }

    fn name(&self) -> &str {
        "landlock"
    }

    fn description(&self) -> &str {
        "Linux kernel LSM sandboxing (filesystem access control)"
    }
}

// Stub implementations for non-Linux or when feature is disabled
#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
#[derive(Debug)]
pub struct LandlockSandbox;

#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
impl LandlockSandbox {
    pub fn new() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux with the sandbox-landlock feature",
        ))
    }

    pub fn with_workspace(_workspace_dir: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }

    pub fn probe() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }
}

#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
impl Sandbox for LandlockSandbox {
    fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Landlock is only supported on Linux",
        ))
    }

    fn is_available(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "landlock"
    }

    fn description(&self) -> &str {
        "Linux kernel LSM sandboxing (not available on this platform)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn landlock_sandbox_name() {
        if let Ok(sandbox) = LandlockSandbox::new() {
            assert_eq!(sandbox.name(), "landlock");
        }
    }

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_not_available_on_non_linux() {
        assert!(!LandlockSandbox.is_available());
        assert_eq!(LandlockSandbox.name(), "landlock");
    }

    #[test]
    fn landlock_with_none_workspace() {
        // Should work even without a workspace directory
        let result = LandlockSandbox::with_workspace(None);
        // On Linux with sandbox-landlock feature, this must succeed.
        // On other platforms or without the feature, failure is acceptable.
        if cfg!(all(feature = "sandbox-landlock", target_os = "linux")) {
            let sandbox = result.expect("landlock should succeed on linux with feature enabled");
            assert!(sandbox.is_available());
        }
    }

    // `landlock_workspace_boundary` has been moved to
    // `tests/landlock_workspace_boundary.rs` as an integration test.

    // ── Parent-process protection ──
    //
    // `restrict_self()` must run in the forked child via `pre_exec`,
    // never in the parent.  These tests verify the daemon (parent)
    // process is never restricted.

    /// Regression: `wrap_command` must NOT restrict the parent process.
    ///
    /// Before the fix, `restrict_self()` was called directly inside
    /// `wrap_command`, which locked the daemon itself within the Landlock
    /// ruleset. Now enforcement is deferred to the child via `pre_exec`.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_does_not_restrict_parent_process() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return, // Landlock not available — skip
        };

        // /etc/passwd is world-readable on every Linux but NOT in the
        // Landlock allow-list (/tmp, /usr, /bin).  After wrap_command
        // the parent must still be able to read it.
        let sentinel = Path::new("/etc/passwd");

        // The sentinel must exist and be readable before the test starts.
        // If it doesn't, the test environment is broken — fail loudly
        // rather than silently passing without verifying anything.
        assert!(
            sentinel.exists(),
            "/etc/passwd must exist as a sentinel — test environment is broken"
        );
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "/etc/passwd must be readable before sandboxing — test environment is broken"
        );

        let mut cmd = std::process::Command::new("true");
        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command must succeed");

        cmd.spawn()
            .expect("child spawn must succeed")
            .wait()
            .expect("child wait must succeed");

        // THE CORE ASSERTION: after wrap_command the parent must STILL
        // be able to read /etc/passwd.  If this fails, restrict_self()
        // was called in the parent — which is the bug this commit fixes.
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "parent process must NOT be restricted by wrap_command — \
             restrict_self() must only run inside the forked child via pre_exec"
        );
    }

    /// `build_ruleset` must NOT enforce restrictions on the caller.
    /// It returns a `RulesetCreated` without calling `restrict_self()`.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn build_ruleset_does_not_restrict_parent() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return,
        };

        let sentinel = Path::new("/etc/passwd");

        // The sentinel must exist and be readable before the test starts.
        assert!(
            sentinel.exists(),
            "/etc/passwd must exist as a sentinel — test environment is broken"
        );
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "/etc/passwd must be readable before build_ruleset — test environment is broken"
        );

        // build_ruleset is safe to call — it only constructs the ruleset,
        // it does NOT enforce it.
        let _ruleset = sandbox.build_ruleset().expect("build_ruleset must succeed");

        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "build_ruleset must not restrict the parent process"
        );
    }

    /// `wrap_command` must return `Ok(())` on a valid command.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_returns_ok() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut cmd = std::process::Command::new("true");
        assert!(sandbox.wrap_command(&mut cmd).is_ok());
    }

    /// `wrap_command` must NOT replace the program binary (unlike
    /// bubblewrap/firejail which prepend their own wrapper).  Landlock
    /// uses `pre_exec` only, so the program and args stay unchanged.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_preserves_program_and_args() {
        let sandbox = match LandlockSandbox::new() {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello");
        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command must succeed");

        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "echo",
            "landlock must not replace the program — it uses pre_exec, not a wrapper binary"
        );

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            vec!["hello".to_string()],
            "landlock must not modify command arguments"
        );
    }

    /// Calling `wrap_command` on multiple distinct commands must not
    /// panic or fail.  Each call builds a fresh ruleset and a separate
    /// `pre_exec` closure, so wrapping multiple commands is safe.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_multiple_distinct_commands() {
        let sandbox = LandlockSandbox::new().expect("Failed to create landlock sandbox");

        for i in 0..3 {
            let mut cmd = std::process::Command::new("true");
            sandbox
                .wrap_command(&mut cmd)
                .unwrap_or_else(|e| panic!("wrap_command call #{i} failed: {e}"));
        }
    }

    /// When a workspace directory is set, `wrap_command` must still
    /// not lock the parent process.
    #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
    #[test]
    fn wrap_command_with_workspace_does_not_restrict_parent() {
        let tmp = tempfile::TempDir::new().expect("must create temp dir");

        let sandbox = LandlockSandbox::with_workspace(Some(tmp.path().to_path_buf()))
            .expect("Failed to create landlock sandbox");

        let sentinel = Path::new("/etc/passwd");

        // The sentinel must exist and be readable before the test starts.
        assert!(
            sentinel.exists(),
            "/etc/passwd must exist as a sentinel — test environment is broken"
        );
        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "/etc/passwd must be readable before wrap_command — test environment is broken"
        );

        let mut cmd = std::process::Command::new("true");
        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command must succeed");

        cmd.spawn()
            .expect("child spawn must succeed")
            .wait()
            .expect("child wait must succeed");

        assert!(
            std::fs::read_to_string(sentinel).is_ok(),
            "parent must not be restricted even with workspace configured"
        );
    }

    // ── §1.1 Landlock stub tests ──────────────────────────────

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_stub_wrap_command_returns_unsupported() {
        let sandbox = LandlockSandbox;
        let mut cmd = std::process::Command::new("echo");
        let result = sandbox.wrap_command(&mut cmd);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_stub_new_returns_unsupported() {
        let result = LandlockSandbox::new();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
    #[test]
    fn landlock_stub_probe_returns_unsupported() {
        let result = LandlockSandbox::probe();
        assert!(result.is_err());
    }
}
