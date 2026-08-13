use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Shell language understood by a runtime's command builder.
///
/// This is part of the execution boundary: security policy must validate the
/// same language that [`RuntimeAdapter::build_shell_command`] will interpret.
///
/// The command-risk policy needs this to apply platform-specific safety rules —
/// notably the null device: a POSIX shell treats `nul` as an ordinary relative
/// filename (so `echo x >nul` would create/truncate a workspace file), while
/// Windows `cmd.exe` resolves it to the discard-only null device. A redirect to
/// `nul` is therefore only safe under [`ShellDialect::WindowsCmd`].
///
/// The dialect follows the *effective execution sink*, not merely the host OS.
/// Docker always runs through `sh -c` and stays [`ShellDialect::Posix`] even on
/// a Windows host. Native cron execution follows the configured runtime dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellDialect {
    /// POSIX `sh`/`bash` semantics — Unix native execution and Docker `sh -c`.
    /// The conservative default.
    #[default]
    Posix,
    /// Windows `cmd.exe` semantics — native execution on a Windows host.
    WindowsCmd,
    /// Windows PowerShell or PowerShell 7+.
    PowerShell,
    /// The runtime does not expose shell execution.
    None,
}

/// Runtime adapter that abstracts platform differences for the agent.
///
/// Implement this trait to port the agent to a new execution environment.
/// The adapter declares platform capabilities (shell access, filesystem,
/// long-running processes) and provides platform-specific implementations
/// for operations like spawning shell commands. The orchestration loop
/// queries these capabilities to adapt its behavior—for example, disabling
/// tool execution on runtimes without shell access.
///
/// Implementations must be `Send + Sync` because the adapter is shared
/// across async tasks on the Tokio runtime.
pub trait RuntimeAdapter: Send + Sync {
    /// Return the human-readable name of this runtime environment.
    ///
    /// Used in logs and diagnostics (e.g., `"native"`, `"docker"`,
    /// `"cloudflare-workers"`).
    fn name(&self) -> &str;

    /// Report whether this runtime supports shell command execution.
    ///
    /// Shell capability is derived from [`Self::shell_dialect`] so adapters
    /// cannot report a shell while omitting the language that policy must
    /// validate (or report a language while disabling shell tools).
    fn has_shell_access(&self) -> bool {
        self.shell_dialect() != ShellDialect::None
    }

    /// Report whether this runtime supports filesystem read/write.
    ///
    /// When `false`, the agent disables file-based tools and falls back to
    /// in-memory storage.
    fn has_filesystem_access(&self) -> bool;

    /// Return the base directory for persistent storage on this runtime.
    ///
    /// Memory backends, logs, and other artifacts are stored under this path.
    /// Implementations should return a platform-appropriate writable directory.
    fn storage_path(&self) -> PathBuf;

    /// Report whether this runtime supports long-running background processes.
    ///
    /// When `true`, the agent may start the gateway server, heartbeat loop,
    /// and other persistent tasks. Serverless runtimes with short execution
    /// limits should return `false`.
    fn supports_long_running(&self) -> bool;

    /// Return the maximum memory budget in bytes for this runtime.
    ///
    /// A value of `0` (the default) indicates no limit. Constrained
    /// environments (embedded, serverless) should return their actual
    /// memory ceiling so the agent can adapt buffer sizes and caching.
    fn memory_budget(&self) -> u64 {
        0
    }

    /// Return the shell language accepted by [`Self::build_shell_command`].
    ///
    /// This is the source of truth for both shell capability and command
    /// policy. Adapters without shell access must return [`ShellDialect::None`].
    ///
    /// An adapter must report the dialect it *actually* runs under, because the
    /// command-risk policy consults this to decide platform-specific safety
    /// (e.g. accepting a redirect to the `nul` null device). Docker executes
    /// via `sh -c` and therefore stays POSIX even on Windows; native cron jobs
    /// follow the configured runtime dialect.
    fn shell_dialect(&self) -> ShellDialect;

    /// Build a shell command process configured for this runtime.
    ///
    /// Constructs a [`tokio::process::Command`] that will execute `command`
    /// with `workspace_dir` as the working directory. Implementations may
    /// prepend sandbox wrappers, set environment variables, or redirect
    /// I/O as appropriate for the platform.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime does not support shell access or if
    /// the command cannot be constructed (e.g., missing shell binary).
    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command>;

    /// Build a shell command process with runtime-visible environment names.
    ///
    /// `env_keys` contains variable names selected by the caller for
    /// passthrough. Implementations that need explicit forwarding, such as
    /// container runtimes, should pass only these names across their runtime
    /// boundary and rely on the spawned process environment for the values.
    fn build_shell_command_with_env_keys(
        &self,
        command: &str,
        workspace_dir: &Path,
        env_keys: &[&OsStr],
    ) -> anyhow::Result<tokio::process::Command> {
        let _ = env_keys;
        self.build_shell_command(command, workspace_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyRuntime;

    impl RuntimeAdapter for DummyRuntime {
        fn name(&self) -> &str {
            "dummy-runtime"
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            PathBuf::from("/tmp/dummy-runtime")
        }

        fn supports_long_running(&self) -> bool {
            true
        }

        fn shell_dialect(&self) -> ShellDialect {
            #[cfg(windows)]
            {
                ShellDialect::WindowsCmd
            }
            #[cfg(not(windows))]
            {
                ShellDialect::Posix
            }
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            #[cfg(windows)]
            let mut cmd = {
                let mut cmd = tokio::process::Command::new("cmd");
                cmd.args(["/C", "echo", command]);
                cmd
            };

            #[cfg(not(windows))]
            let mut cmd = tokio::process::Command::new("echo");
            #[cfg(not(windows))]
            cmd.arg(command);

            cmd.current_dir(workspace_dir);
            Ok(cmd)
        }
    }

    #[test]
    fn default_memory_budget_is_zero() {
        let runtime = DummyRuntime;
        assert_eq!(runtime.memory_budget(), 0);
    }

    #[test]
    fn default_shell_dialect_is_posix() {
        // Any adapter that does not override `shell_dialect` uses the
        // conservative POSIX default. Concrete adapters report their actual
        // execution sink, including the configured native shell dialect.
        let runtime = DummyRuntime;
        assert_eq!(runtime.shell_dialect(), ShellDialect::Posix);
    }

    #[test]
    fn runtime_reports_capabilities() {
        let runtime = DummyRuntime;

        assert_eq!(runtime.name(), "dummy-runtime");
        assert!(runtime.has_shell_access());
        assert!(runtime.has_filesystem_access());
        assert!(runtime.supports_long_running());
        #[cfg(windows)]
        assert_eq!(runtime.shell_dialect(), ShellDialect::WindowsCmd);
        #[cfg(not(windows))]
        assert_eq!(runtime.shell_dialect(), ShellDialect::Posix);
        assert_eq!(runtime.storage_path(), PathBuf::from("/tmp/dummy-runtime"));
    }

    #[tokio::test]
    async fn build_shell_command_executes() {
        let runtime = DummyRuntime;
        let mut cmd = runtime
            .build_shell_command("hello-runtime", Path::new("."))
            .unwrap();

        let output = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success());
        assert!(stdout.contains("hello-runtime"));
    }

    #[tokio::test]
    async fn default_env_key_builder_delegates_to_shell_command() {
        let runtime = DummyRuntime;
        let mut cmd = runtime
            .build_shell_command_with_env_keys(
                "hello-env-key-runtime",
                Path::new("."),
                &[OsStr::new("ZC_RUNTIME_TOKEN")],
            )
            .unwrap();

        let output = cmd.output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success());
        assert!(stdout.contains("hello-env-key-runtime"));
    }
}
