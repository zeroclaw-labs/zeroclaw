//! Landlock sandbox (Linux kernel 5.13+ LSM)
//!
//! Landlock provides unprivileged sandboxing through the Linux kernel.
//! This module uses the pure-Rust `landlock` crate for filesystem access control.

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
use landlock::{AccessFS, Ruleset, RulesetCreated};

use crate::security::traits::Sandbox;
use std::path::Path;

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
        let test_ruleset = Ruleset::new().set_access_fs(AccessFS::read_file | AccessFS::write_file);

        match test_ruleset.create() {
            Ok(_) => Ok(Self { workspace_dir }),
            Err(e) => {
                tracing::debug!("Landlock not available: {}", e);
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

    /// Apply Landlock restrictions to the current process
    fn apply_restrictions(&self) -> std::io::Result<()> {
        let mut ruleset = Ruleset::new().set_access_fs(
            AccessFS::read_file
                | AccessFS::write_file
                | AccessFS::read_dir
                | AccessFS::remove_dir
                | AccessFS::remove_file
                | AccessFS::make_char
                | AccessFS::make_sock
                | AccessFS::make_fifo
                | AccessFS::make_block
                | AccessFS::make_reg
                | AccessFS::make_sym,
        );

        // Allow workspace directory (read/write)
        if let Some(ref workspace) = self.workspace_dir {
            if workspace.exists() {
                ruleset = ruleset.add_path(
                    workspace,
                    AccessFS::read_file | AccessFS::write_file | AccessFS::read_dir,
                )?;
            }
        }

        // Allow /tmp for general operations
        ruleset = ruleset.add_path(
            Path::new("/tmp"),
            AccessFS::read_file | AccessFS::write_file,
        )?;

        // Allow /usr and /bin for executing commands
        ruleset = ruleset.add_path(Path::new("/usr"), AccessFS::read_file | AccessFS::read_dir)?;
        ruleset = ruleset.add_path(Path::new("/bin"), AccessFS::read_file | AccessFS::read_dir)?;

        // Apply the ruleset
        match ruleset.create() {
            Ok(_) => {
                tracing::debug!("Landlock restrictions applied successfully");
                Ok(())
            }
            Err(e) => {
                tracing::warn!("Failed to apply Landlock restrictions: {}", e);
                Err(std::io::Error::new(std::io::ErrorKind::Other, e))
            }
        }
    }
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
impl Sandbox for LandlockSandbox {
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()> {
        // Apply Landlock restrictions before executing the command
        // Note: This affects the current process, not the child process
        // Child processes inherit the Landlock restrictions
        self.apply_restrictions()
    }

    fn is_available(&self) -> bool {
        // Try to create a minimal ruleset to verify availability
        Ruleset::new()
            .set_access_fs(AccessFS::read_file)
            .create()
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
        // Result depends on platform and feature flag
        match result {
            Ok(sandbox) => assert!(sandbox.is_available()),
            Err(_) => assert!(!cfg!(all(
                feature = "sandbox-landlock",
                target_os = "linux"
            ))),
        }
    }
}
