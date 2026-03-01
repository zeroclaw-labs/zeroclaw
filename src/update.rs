//! Self-update functionality for ZeroClaw.
//!
//! Downloads and installs the latest release from GitHub.

use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

/// GitHub repository for releases
const GITHUB_REPO: &str = "zeroclaw-labs/zeroclaw";
const GITHUB_API_RELEASES: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest";

/// Release information from GitHub API
#[derive(Debug, serde::Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug, serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMethod {
    Homebrew,
    CargoOrLocal,
    Unknown,
}

/// Get the current version of the binary
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Get the target triple for the current platform
fn get_target_triple() -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    let target = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "arm") => "armv7-unknown-linux-gnueabihf",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => bail!("Unsupported platform: {}-{}", os, arch),
    };

    Ok(target.to_string())
}

/// Get the binary name for the current platform
fn get_binary_name() -> String {
    if cfg!(windows) {
        "zeroclaw.exe".to_string()
    } else {
        "zeroclaw".to_string()
    }
}

/// Get the archive name for a given target
fn get_archive_name(target: &str) -> String {
    if target.contains("windows") {
        format!("zeroclaw-{}.zip", target)
    } else {
        format!("zeroclaw-{}.tar.gz", target)
    }
}

/// Fetch the latest release information from GitHub
async fn fetch_latest_release() -> Result<Release> {
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", current_version()))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(GITHUB_API_RELEASES)
        .send()
        .await
        .context("Failed to fetch release information from GitHub")?;

    if !response.status().is_success() {
        bail!("GitHub API returned status: {}", response.status());
    }

    let release: Release = response
        .json()
        .await
        .context("Failed to parse release information")?;

    Ok(release)
}

/// Find the appropriate asset for the current platform
fn find_asset_for_platform(release: &Release) -> Result<&Asset> {
    let target = get_target_triple()?;
    let archive_name = get_archive_name(&target);

    release
        .assets
        .iter()
        .find(|a| a.name == archive_name)
        .with_context(|| {
            format!(
                "No release asset found for platform {} (looking for {})",
                target, archive_name
            )
        })
}

/// Download and extract the binary from the release archive
async fn download_binary(asset: &Asset, temp_dir: &Path) -> Result<PathBuf> {
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", current_version()))
        .build()
        .context("Failed to create HTTP client")?;

    tracing::info!("Downloading {}...", asset.name);

    let response = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .context("Failed to download release archive")?;

    if !response.status().is_success() {
        bail!("Download failed with status: {}", response.status());
    }

    let archive_path = temp_dir.join(&asset.name);
    let archive_bytes = response
        .bytes()
        .await
        .context("Failed to read download content")?;

    fs::write(&archive_path, &archive_bytes).context("Failed to write archive to temp file")?;

    tracing::info!("Extracting {}...", asset.name);

    // Extract based on archive type
    if asset.name.ends_with(".tar.gz") {
        extract_tar_gz(&archive_path, temp_dir)?;
    } else if asset.name.ends_with(".zip") {
        extract_zip(&archive_path, temp_dir)?;
    } else {
        bail!("Unsupported archive format: {}", asset.name);
    }

    let binary_name = get_binary_name();
    let binary_path = temp_dir.join(&binary_name);

    if !binary_path.exists() {
        bail!(
            "Binary not found in archive. Expected: {}",
            binary_path.display()
        );
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .context("Failed to set executable permissions")?;
    }

    Ok(binary_path)
}

/// Extract a tar.gz archive
fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(dest_dir)
        .output()
        .context("Failed to execute tar command")?;

    if !output.status.success() {
        bail!(
            "tar extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Extract a zip archive
fn extract_zip(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let output = Command::new("unzip")
        .arg("-o")
        .arg(archive_path)
        .arg("-d")
        .arg(dest_dir)
        .output()
        .context("Failed to execute unzip command")?;

    if !output.status.success() {
        bail!(
            "unzip extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Get the path to the current executable
fn get_current_exe() -> Result<PathBuf> {
    env::current_exe().context("Failed to get current executable path")
}

fn detect_install_method_for_path(resolved_path: &Path, home_dir: Option<&Path>) -> InstallMethod {
    let lower = resolved_path.to_string_lossy().to_ascii_lowercase();
    if lower.contains("/cellar/zeroclaw/") || lower.contains("/homebrew/cellar/zeroclaw/") {
        return InstallMethod::Homebrew;
    }

    if let Some(home) = home_dir {
        if resolved_path.starts_with(home.join(".cargo").join("bin"))
            || resolved_path.starts_with(home.join(".local").join("bin"))
        {
            return InstallMethod::CargoOrLocal;
        }
    }

    InstallMethod::Unknown
}

fn detect_install_method(current_exe: &Path) -> InstallMethod {
    let resolved = fs::canonicalize(current_exe).unwrap_or_else(|_| current_exe.to_path_buf());
    let home_dir = env::var_os("HOME").map(PathBuf::from);
    detect_install_method_for_path(&resolved, home_dir.as_deref())
}

/// Print human-friendly update instructions based on detected install method.
pub fn print_update_instructions() -> Result<()> {
    let current_exe = get_current_exe()?;
    let install_method = detect_install_method(&current_exe);

    println!("ZeroClaw update guide");
    println!("Detected binary: {}", current_exe.display());
    println!();
    println!("1) Check if a new release exists:");
    println!("   zeroclaw update --check");
    println!();

    match install_method {
        InstallMethod::Homebrew => {
            println!("Detected install method: Homebrew");
            println!("Recommended update commands:");
            println!("  brew update");
            println!("  brew upgrade zeroclaw");
            println!("  zeroclaw --version");
            println!();
            println!(
                "Tip: avoid `zeroclaw update` on Homebrew installs unless you intentionally want to override the managed binary."
            );
        }
        InstallMethod::CargoOrLocal => {
            println!("Detected install method: local binary (~/.cargo/bin or ~/.local/bin)");
            println!("Recommended update command:");
            println!("  zeroclaw update");
            println!("Optional force reinstall:");
            println!("  zeroclaw update --force");
            println!("Verify:");
            println!("  zeroclaw --version");
        }
        InstallMethod::Unknown => {
            println!("Detected install method: unknown");
            println!("Try the built-in updater first:");
            println!("  zeroclaw update");
            println!(
                "If your package manager owns the binary, use that manager's upgrade command."
            );
            println!("Verify:");
            println!("  zeroclaw --version");
        }
    }

    println!();
    println!("Release source: https://github.com/{GITHUB_REPO}/releases/latest");
    Ok(())
}

/// Replace the current binary with the new one
fn replace_binary(new_binary: &Path, current_exe: &Path) -> Result<()> {
    // On Windows, we can't replace a running executable directly
    // We need to rename the old one and place the new one
    #[cfg(windows)]
    {
        let old_path = current_exe.with_extension("exe.old");
        fs::rename(current_exe, &old_path).context("Failed to rename old binary")?;
        fs::copy(new_binary, current_exe).context("Failed to copy new binary")?;
        // Try to remove the old binary (may fail if still locked)
        let _ = fs::remove_file(&old_path);
    }

    // On Unix, stage the binary in the destination directory first.
    // This avoids cross-filesystem rename failures (EXDEV) from temp dirs.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let parent = current_exe
            .parent()
            .context("Current executable has no parent directory")?;
        let binary_name = current_exe
            .file_name()
            .context("Current executable path is missing a file name")?
            .to_string_lossy()
            .into_owned();
        let staged_path = parent.join(format!(".{binary_name}.new"));
        let backup_path = parent.join(format!(".{binary_name}.bak"));

        fs::copy(new_binary, &staged_path).context("Failed to stage updated binary")?;
        fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o755))
            .context("Failed to set permissions on staged binary")?;

        if let Err(err) = fs::remove_file(&backup_path) {
            if err.kind() != ErrorKind::NotFound {
                return Err(err).context("Failed to remove stale backup binary");
            }
        }

        fs::rename(current_exe, &backup_path).context("Failed to backup current binary")?;

        if let Err(err) = fs::rename(&staged_path, current_exe) {
            let _ = fs::rename(&backup_path, current_exe);
            let _ = fs::remove_file(&staged_path);
            return Err(err).context("Failed to activate updated binary");
        }

        // Best-effort cleanup of backup.
        let _ = fs::remove_file(&backup_path);
    }

    Ok(())
}

/// Check if an update is available
pub async fn check_for_update() -> Result<Option<String>> {
    let release = fetch_latest_release().await?;
    let latest_version = release.tag_name.trim_start_matches('v');

    if latest_version == current_version() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "{} (current: {})",
            release.tag_name,
            current_version()
        )))
    }
}

/// Perform the self-update
pub async fn self_update(force: bool, check_only: bool) -> Result<()> {
    println!("🦀 ZeroClaw Self-Update");
    println!();

    let current_exe = get_current_exe()?;
    let install_method = detect_install_method(&current_exe);
    println!("Current binary: {}", current_exe.display());
    println!("Current version: v{}", current_version());
    println!();

    // Fetch latest release info
    let release = fetch_latest_release().await?;
    let latest_version = release.tag_name.trim_start_matches('v');

    println!("Latest version:  {}", release.tag_name);

    if check_only {
        println!();
        if latest_version == current_version() {
            println!("✅ Already up to date.");
        } else {
            println!(
                "Update available: {} -> {}",
                current_version(),
                latest_version
            );
            println!("Run `zeroclaw update` to install the update.");
        }
        return Ok(());
    }

    if install_method == InstallMethod::Homebrew && !force {
        println!();
        println!("Detected a Homebrew-managed installation.");
        println!("Use `brew upgrade zeroclaw` for the safest update path.");
        println!(
            "Run `zeroclaw update --force` only if you intentionally want to override Homebrew."
        );
        return Ok(());
    }

    // Check if update is needed
    if latest_version == current_version() && !force {
        println!();
        println!("✅ Already up to date!");
        return Ok(());
    }

    println!();
    println!(
        "Updating from v{} to {}...",
        current_version(),
        latest_version
    );

    // Find the appropriate asset
    let asset = find_asset_for_platform(&release)?;
    println!("Downloading: {}", asset.name);

    // Create temp directory
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    // Download and extract
    let new_binary = download_binary(asset, temp_dir.path()).await?;

    println!("Installing update...");

    // Replace the binary
    replace_binary(&new_binary, &current_exe)?;

    println!();
    println!("✅ Successfully updated to {}!", release.tag_name);
    println!();
    println!("Restart ZeroClaw to use the new version.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_name_uses_zip_for_windows_and_targz_elsewhere() {
        assert_eq!(
            get_archive_name("x86_64-pc-windows-msvc"),
            "zeroclaw-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            get_archive_name("x86_64-unknown-linux-gnu"),
            "zeroclaw-x86_64-unknown-linux-gnu.tar.gz"
        );
    }

    #[test]
    fn detect_install_method_identifies_homebrew_paths() {
        let path = Path::new("/opt/homebrew/Cellar/zeroclaw/0.1.7/bin/zeroclaw");
        let method = detect_install_method_for_path(path, None);
        assert_eq!(method, InstallMethod::Homebrew);
    }

    #[test]
    fn detect_install_method_identifies_local_bin_paths() {
        let home = Path::new("/Users/example");
        let cargo_path = Path::new("/Users/example/.cargo/bin/zeroclaw");
        let local_path = Path::new("/Users/example/.local/bin/zeroclaw");

        assert_eq!(
            detect_install_method_for_path(cargo_path, Some(home)),
            InstallMethod::CargoOrLocal
        );
        assert_eq!(
            detect_install_method_for_path(local_path, Some(home)),
            InstallMethod::CargoOrLocal
        );
    }

    #[test]
    fn detect_install_method_returns_unknown_for_other_paths() {
        let path = Path::new("/usr/bin/zeroclaw");
        let method = detect_install_method_for_path(path, Some(Path::new("/Users/example")));
        assert_eq!(method, InstallMethod::Unknown);
    }
}
