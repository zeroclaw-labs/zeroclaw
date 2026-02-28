use super::traits::RuntimeAdapter;
use std::path::{Path, PathBuf};

/// Native runtime — full access, runs on Mac/Linux/Docker/Raspberry Pi
pub struct NativeRuntime;

impl NativeRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl RuntimeAdapter for NativeRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        "native"
    }

    fn has_shell_access(&self) -> bool {
        true
    }

    fn has_filesystem_access(&self) -> bool {
        true
    }

    fn storage_path(&self) -> PathBuf {
        directories::UserDirs::new().map_or_else(
            || PathBuf::from(".zeroclaw"),
            |u| u.home_dir().join(".zeroclaw"),
        )
    }

    fn supports_long_running(&self) -> bool {
        true
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        let mut process = if std::env::consts::OS == "windows" {
            // Use PowerShell on Windows with UTF-8 encoding support for Chinese characters
            let mut cmd = tokio::process::Command::new("powershell");
            
            // Use simplified PowerShell startup to avoid configuration file issues
            let safe_command = format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {}",
                command
            );
            
            cmd.arg("-NoProfile");           // Do not load profile
            cmd.arg("-NonInteractive");       // Non-interactive mode
            cmd.arg("-ExecutionPolicy").arg("Bypass");  // Bypass execution policy
            cmd.arg("-Command").arg(safe_command);
            cmd
        } else {
            // Use bash/sh on Linux/macOS
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(command);
            cmd
        };
        
        process.current_dir(workspace_dir);

        println!("\n{:?}\n", process);
        Ok(process)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_name() {
        assert_eq!(NativeRuntime::new().name(), "native");
    }

    #[test]
    fn native_has_shell_access() {
        assert!(NativeRuntime::new().has_shell_access());
    }

    #[test]
    fn native_has_filesystem_access() {
        assert!(NativeRuntime::new().has_filesystem_access());
    }

    #[test]
    fn native_supports_long_running() {
        assert!(NativeRuntime::new().supports_long_running());
    }

    #[test]
    fn native_memory_budget_unlimited() {
        assert_eq!(NativeRuntime::new().memory_budget(), 0);
    }

    #[test]
    fn native_storage_path_contains_zeroclaw() {
        let path = NativeRuntime::new().storage_path();
        assert!(path.to_string_lossy().contains("zeroclaw"));
    }

    #[test]
    fn native_builds_shell_command() {
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command("echo hello", &cwd)
            .unwrap();
        let debug = format!("{command:?}");
        
        if std::env::consts::OS == "windows" {
            // Should use PowerShell on Windows
            assert!(debug.contains("powershell"));
            assert!(debug.contains("OutputEncoding"));
            assert!(debug.contains("UTF8"));
            assert!(debug.contains("echo hello"));
        } else {
            // Should use sh on Linux/macOS
            assert!(debug.contains("sh"));
            assert!(debug.contains("-c"));
            assert!(debug.contains("echo hello"));
        }
    }

    #[test]
    fn native_builds_powershell_command_with_encoding() {
        if std::env::consts::OS != "windows" {
            return; // Test only on Windows
        }
        
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command("Get-ChildItem", &cwd)
            .unwrap();
        let debug = format!("{command:?}");
        
        // Verify PowerShell encoding settings
        assert!(debug.contains("powershell"));
        assert!(debug.contains("OutputEncoding"));
        assert!(debug.contains("UTF8"));
        assert!(debug.contains("Get-ChildItem"));
        assert!(debug.contains("-NoProfile"));
        assert!(debug.contains("-NonInteractive"));
        assert!(debug.contains("-ExecutionPolicy"));
        assert!(debug.contains("Bypass"));
    }
}
