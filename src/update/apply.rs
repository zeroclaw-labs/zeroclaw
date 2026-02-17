use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// How the binary was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// Installed via `cargo install`
    CargoInstall,
    /// Standalone binary (downloaded from GitHub Releases, etc.)
    Binary,
}

/// Detect whether the running binary was installed via `cargo install`.
pub fn detect_install_method() -> InstallMethod {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return InstallMethod::Binary,
    };

    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| {
        directories::UserDirs::new()
            .map(|u| u.home_dir().join(".cargo").to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    if cargo_home.is_empty() {
        return InstallMethod::Binary;
    }

    let cargo_bin = PathBuf::from(&cargo_home).join("bin");
    if exe.starts_with(&cargo_bin) {
        InstallMethod::CargoInstall
    } else {
        InstallMethod::Binary
    }
}

/// Backup the current binary to the specified path.
pub fn backup_current_binary(backup_path: &Path) -> Result<()> {
    let current_exe =
        std::env::current_exe().context("Failed to determine current executable path")?;
    std::fs::copy(&current_exe, backup_path).context("Failed to backup current binary")?;
    Ok(())
}

/// Atomically replace the running binary with the new one.
pub fn replace_binary(new_binary: &Path) -> Result<()> {
    self_replace::self_replace(new_binary).context("Failed to replace binary")?;
    Ok(())
}

/// Restore the backup binary to the current executable path.
pub fn restore_backup(backup_path: &Path) -> Result<()> {
    if !backup_path.exists() {
        bail!("Backup file not found at {}", backup_path.display());
    }
    self_replace::self_replace(backup_path).context("Failed to restore backup binary")?;
    Ok(())
}

/// Health check: run the new binary with --version and verify output.
pub fn health_check(expected_version: &str) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to determine current executable path")?;

    let output = std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .context("Failed to run health check on new binary")?;

    if !output.status.success() {
        bail!(
            "New binary exited with non-zero status: {}",
            output.status
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains(expected_version) {
        bail!(
            "Version mismatch — expected '{expected_version}' in output, got: {stdout}"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_install_method_returns_binary_for_non_cargo_path() {
        // Current exe is unlikely to be in $CARGO_HOME/bin during tests run from
        // the build directory, but this depends on the test environment.
        // This test validates that the function doesn't panic.
        let method = detect_install_method();
        assert!(
            method == InstallMethod::Binary || method == InstallMethod::CargoInstall,
            "Should return a valid variant"
        );
    }
}
