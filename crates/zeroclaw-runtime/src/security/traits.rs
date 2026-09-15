//! Sandbox trait for pluggable OS-level isolation.

use async_trait::async_trait;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxShellProgram {
    Host,
    Isolated {
        program: OsString,
        /// Whether host-controlled files are visible in the isolated
        /// executable namespace.
        mutable_mount: bool,
    },
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()>;

    /// Wrap a shell runtime command while retaining the runtime's original
    /// program spelling. Namespace-changing sandboxes may need that spelling
    /// because a host-resolved absolute path has no meaning inside the target
    /// namespace.
    fn wrap_shell_command(
        &self,
        cmd: &mut Command,
        _original_program: &OsStr,
    ) -> std::io::Result<SandboxShellProgram> {
        self.wrap_command(cmd)?;
        Ok(SandboxShellProgram::Host)
    }

    /// Materialize the sandbox policy inputs that are not already visible in
    /// the wrapped command. The returned bytes are fingerprint input only;
    /// callers hash them before placing them in action facts.
    fn execution_fingerprint_material(&self, _launch_program: &Path) -> std::io::Result<Vec<u8>> {
        Ok(b"sandbox-policy-v1:stateless".to_vec())
    }

    /// Pin mutable sandbox launch references before the final command is
    /// fingerprinted and spawned. Container sandboxes resolve image tags to
    /// content-addressed IDs here; ordinary host sandboxes need no extra step.
    fn pin_shell_command(
        &self,
        _command: &mut Command,
        _resolved_launcher: &Path,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn is_available(&self) -> bool;

    /// Return the human-readable name of this sandbox backend.
    /// Used in logs and diagnostics to identify which isolation strategy is
    /// active (e.g., `"firejail"`, `"bubblewrap"`, `"none"`).
    fn name(&self) -> &str;

    /// Return a brief description of the isolation guarantees this sandbox provides.
    /// Displayed in status output and health checks so operators can verify
    /// the active security posture.
    fn description(&self) -> &str;

    /// Return a reason when this sandbox cannot preserve coding CLI semantics.
    ///
    /// Coding CLI tools need a writable validated working directory and selected
    /// environment values to reach the actual CLI process. Container-style or
    /// replacing wrappers that cannot preserve those semantics must fail closed
    /// instead of spawning a misleading partial sandbox.
    fn coding_cli_unsupported_reason(&self) -> Option<&'static str> {
        None
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

    fn execution_fingerprint_material(&self, _launch_program: &Path) -> std::io::Result<Vec<u8>> {
        Ok(b"sandbox-policy-v1:none".to_vec())
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
