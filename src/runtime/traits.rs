use std::path::{Path, PathBuf};

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
    /// When `false`, the agent disables shell-based tools. Serverless and
    /// edge runtimes typically return `false`.
    fn has_shell_access(&self) -> bool;

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
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyRuntime;

    impl RuntimeAdapter for DummyRuntime {
        fn name(&self) -> &str {
            "dummy-runtime"
        }

        fn has_shell_access(&self) -> bool {
            true
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

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let mut cmd = tokio::process::Command::new("echo");
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
    fn runtime_reports_capabilities() {
        let runtime = DummyRuntime;

        assert_eq!(runtime.name(), "dummy-runtime");
        assert!(runtime.has_shell_access());
        assert!(runtime.has_filesystem_access());
        assert!(runtime.supports_long_running());
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
}
