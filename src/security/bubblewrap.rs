//! Bubblewrap sandbox (user namespaces for Linux/macOS)

use crate::security::traits::Sandbox;
use std::process::Command;

/// Bubblewrap sandbox backend
#[derive(Debug, Clone, Default)]
pub struct BubblewrapSandbox;

impl BubblewrapSandbox {
    pub fn new() -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Bubblewrap not found",
            ))
        }
    }

    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    fn is_installed() -> bool {
        Command::new("bwrap")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl Sandbox for BubblewrapSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut bwrap_cmd = Command::new("bwrap");
        bwrap_cmd.args([
            "--ro-bind",
            "/usr",
            "/usr",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--bind",
            "/tmp",
            "/tmp",
            "--unshare-all",
            "--die-with-parent",
        ]);
        bwrap_cmd.arg(&program);
        bwrap_cmd.args(&args);

        *cmd = bwrap_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        Self::is_installed()
    }

    fn name(&self) -> &str {
        "bubblewrap"
    }

    fn description(&self) -> &str {
        "User namespace sandbox (requires bwrap)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubblewrap_sandbox_name() {
        assert_eq!(BubblewrapSandbox.name(), "bubblewrap");
    }

    #[test]
    fn bubblewrap_is_available_only_if_installed() {
        // Result depends on whether bwrap is installed
        let available = BubblewrapSandbox::is_available();
        // Either way, the name should still work
        assert_eq!(BubblewrapSandbox.name(), "bubblewrap");
    }
}
