//! Sandbox trait for pluggable OS-level isolation.

use async_trait::async_trait;
use std::process::Command;

#[async_trait]
pub trait Sandbox: Send + Sync {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()>;

    fn is_available(&self) -> bool;

    /// Return the human-readable name of this sandbox backend.
    /// Used in logs and diagnostics to identify which isolation strategy is
    /// active (e.g., `"firejail"`, `"bubblewrap"`, `"none"`).
    fn name(&self) -> &str;

    /// Return a brief description of the isolation guarantees this sandbox provides.
    /// Displayed in status output and health checks so operators can verify
    /// the active security posture.
    fn description(&self) -> &str;
}

/// Carry over the settings a replacing wrapper would otherwise silently drop.
///
/// Backends that isolate by *replacing* the command (`sandbox-exec ...`,
/// `firejail ...`, `bwrap ...`, `docker run ...`) build a fresh [`Command`] and
/// assign it over the caller's. A fresh `Command` has no working directory and
/// no explicit environment, so without this the child inherits the daemon's
/// cwd — not the workspace the policy resolved the command's relative paths
/// against. The more isolation is configured, the further the child drifts from
/// the directory it was validated for.
///
/// Only what `Command` exposes a getter for can be carried: the working
/// directory and the explicit environment. Stdio has no getter, so callers
/// configure it after wrapping.
///
/// For container backends the *in-container* directory comes from the
/// backend's own flag (`docker run --workdir`); what is carried here is the cwd
/// of the client process that launches the container.
pub(crate) fn adopt_command_context(source: &Command, wrapper: &mut Command) {
    if let Some(dir) = source.get_current_dir() {
        wrapper.current_dir(dir);
    }
    for (key, value) in source.get_envs() {
        match value {
            Some(value) => {
                wrapper.env(key, value);
            }
            None => {
                wrapper.env_remove(key);
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NoopSandbox;

impl Sandbox for NoopSandbox {
    fn wrap_command(&self, _cmd: &mut Command) -> std::io::Result<()> {
        // Pass through unchanged
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "none"
    }

    fn description(&self) -> &str {
        "No sandboxing (application-layer security only)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_sandbox_name() {
        assert_eq!(NoopSandbox.name(), "none");
    }

    #[test]
    fn noop_sandbox_is_always_available() {
        assert!(NoopSandbox.is_available());
    }

    #[test]
    fn adopt_command_context_carries_the_working_directory() {
        let mut source = Command::new("sh");
        source.current_dir("/workspace");
        let mut wrapper = Command::new("sandbox-exec");

        adopt_command_context(&source, &mut wrapper);

        assert_eq!(
            wrapper.get_current_dir(),
            Some(std::path::Path::new("/workspace")),
            "a replacing wrapper must run in the directory the caller chose"
        );
    }

    #[test]
    fn adopt_command_context_carries_set_and_removed_env_vars() {
        let mut source = Command::new("sh");
        source.env("KEEP", "yes");
        source.env_remove("DROP");
        let mut wrapper = Command::new("sandbox-exec");

        adopt_command_context(&source, &mut wrapper);

        let envs: Vec<(String, Option<String>)> = wrapper
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert!(envs.contains(&("KEEP".to_string(), Some("yes".to_string()))));
        assert!(envs.contains(&("DROP".to_string(), None)));
    }

    #[test]
    fn adopt_command_context_leaves_an_unset_working_directory_alone() {
        let source = Command::new("sh");
        let mut wrapper = Command::new("sandbox-exec");
        wrapper.current_dir("/preset");

        adopt_command_context(&source, &mut wrapper);

        assert_eq!(
            wrapper.get_current_dir(),
            Some(std::path::Path::new("/preset")),
            "an unset source cwd must not clear one the backend set itself"
        );
    }

    #[test]
    fn noop_sandbox_wrap_command_is_noop() {
        let mut cmd = Command::new("echo");
        cmd.arg("test");
        let original_program = cmd.get_program().to_string_lossy().to_string();
        let original_args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let sandbox = NoopSandbox;
        assert!(sandbox.wrap_command(&mut cmd).is_ok());

        // Command should be unchanged
        assert_eq!(cmd.get_program().to_string_lossy(), original_program);
        assert_eq!(
            cmd.get_args()
                .map(|s| s.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            original_args
        );
    }
}
