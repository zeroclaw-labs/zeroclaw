use std::path::{Path, PathBuf};

/// Runtime adapter — abstracts platform differences so the same agent
/// code runs on native, Docker, Cloudflare Workers, Raspberry Pi, etc.
pub trait RuntimeAdapter: Send + Sync {
    /// Human-readable runtime name
    fn name(&self) -> &str;

    /// Whether this runtime supports shell access
    fn has_shell_access(&self) -> bool;

    /// Whether this runtime supports filesystem access
    fn has_filesystem_access(&self) -> bool;

    /// Base storage path for this runtime
    fn storage_path(&self) -> PathBuf;

    /// Whether long-running processes (gateway, heartbeat) are supported
    fn supports_long_running(&self) -> bool;

    /// Maximum memory budget in bytes (0 = unlimited)
    fn memory_budget(&self) -> u64 {
        0
    }

    /// Build a shell command process for this runtime.
    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command>;
}
