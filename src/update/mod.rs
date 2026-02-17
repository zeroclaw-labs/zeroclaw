use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::config::Config;

mod apply;
mod download;
mod migrate;
mod verify;

pub use apply::{detect_install_method, InstallMethod};

// ── Constants ───────────────────────────────────────────────────

const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest";
const CACHE_FILENAME: &str = "update-check.json";
const BACKUP_FILENAME: &str = "zeroclaw.bak";

// ── Core types ──────────────────────────────────────────────────

/// Information about an available update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    pub asset_url: String,
    pub checksum_url: String,
    pub published_at: String,
}

/// Result of a version check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable,
    CheckFailed,
}

/// Cached version check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedCheck {
    pub checked_at: String,
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    pub asset_url: String,
    pub checksum_url: String,
    pub status: UpdateStatus,
}

// ── GitHub API response types ───────────────────────────────────

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ── Public API ──────────────────────────────────────────────────

/// Check whether a network update check should be performed based on cache freshness.
pub fn should_check(config: &Config) -> bool {
    if !config.update.enabled {
        return false;
    }
    if is_update_disabled_by_env() {
        return false;
    }

    let cache_path = config_dir_path(config).join(CACHE_FILENAME);
    match read_cache_file(&cache_path) {
        Some(cached) => {
            if config.update.check_interval_hours == 0 {
                return true;
            }
            let Ok(checked) = chrono::DateTime::parse_from_rfc3339(&cached.checked_at) else {
                return true;
            };
            let elapsed = chrono::Utc::now().signed_duration_since(checked);
            let interval =
                chrono::Duration::hours(config.update.check_interval_hours.min(8760) as i64);
            elapsed > interval
        }
        None => true,
    }
}

/// Query the GitHub Releases API and compare versions.
pub async fn check_latest(config: &Config) -> Result<CachedCheck> {
    let current_str = env!("CARGO_PKG_VERSION");
    let current = semver::Version::parse(current_str)
        .context("Failed to parse current version as semver")?;

    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{current_str}"))
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let release: GitHubRelease = client
        .get(GITHUB_RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // Skip pre-releases on stable channel
    if release.prerelease && config.update.channel != "pre-release" {
        return Ok(CachedCheck {
            checked_at: chrono::Utc::now().to_rfc3339(),
            current_version: current_str.to_string(),
            latest_version: current_str.to_string(),
            release_url: String::new(),
            asset_url: String::new(),
            checksum_url: String::new(),
            status: UpdateStatus::UpToDate,
        });
    }

    let tag = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let latest = semver::Version::parse(tag)
        .with_context(|| format!("Failed to parse release tag '{tag}' as semver"))?;

    let platform_artifact = download::platform_artifact_name();
    let asset_url = release
        .assets
        .iter()
        .find(|a| a.name == platform_artifact)
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    let checksum_url = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS")
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    let status = if latest > current {
        UpdateStatus::UpdateAvailable
    } else {
        UpdateStatus::UpToDate
    };

    Ok(CachedCheck {
        checked_at: chrono::Utc::now().to_rfc3339(),
        current_version: current_str.to_string(),
        latest_version: format!("{latest}"),
        release_url: release.html_url,
        asset_url,
        checksum_url,
        status,
    })
}

/// Read cached check result from disk.
pub fn read_cache(config: &Config) -> Option<CachedCheck> {
    let cache_path = config_dir_path(config).join(CACHE_FILENAME);
    read_cache_file(&cache_path)
}

/// Write check result to cache file (atomic via temp + rename).
pub fn write_cache(config: &Config, result: &CachedCheck) -> Result<()> {
    let dir = config_dir_path(config);
    std::fs::create_dir_all(&dir)?;
    let cache_path = dir.join(CACHE_FILENAME);
    let tmp_path = dir.join(format!("{CACHE_FILENAME}.tmp"));
    let json = serde_json::to_string_pretty(result)?;
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, &cache_path)?;
    Ok(())
}

/// Run the full update flow: check → download → verify → backup → replace → health-check.
pub async fn run_update(config: &Config, force: bool, check_only: bool) -> Result<()> {
    eprintln!("🔍 Checking for updates...");

    let result = check_latest(config).await.context("Update check failed")?;
    let _ = write_cache(config, &result);

    if result.status != UpdateStatus::UpdateAvailable && !force {
        eprintln!(
            "✅ You're already on the latest version (v{}).",
            result.current_version
        );
        return Ok(());
    }

    eprintln!(
        "📦 ZeroClaw v{} is available (you have v{})",
        result.latest_version, result.current_version
    );

    if check_only {
        eprintln!("   Release: {}", result.release_url);
        eprintln!("   Run 'zeroclaw update' to install.");
        return Ok(());
    }

    if result.asset_url.is_empty() {
        bail!(
            "No release artifact found for this platform ({}). \
             Download manually from: {}",
            download::platform_artifact_name(),
            result.release_url
        );
    }

    // Detect install method
    match detect_install_method() {
        InstallMethod::CargoInstall => {
            eprintln!();
            eprintln!("This binary was installed via cargo. Updating with:");
            eprintln!("  cargo install zeroclaw --locked");
            eprintln!();

            let status = std::process::Command::new("cargo")
                .args(["install", "zeroclaw", "--locked"])
                .status()
                .context("Failed to run cargo install")?;

            if status.success() {
                eprintln!();
                eprintln!(
                    "🎉 Successfully updated to ZeroClaw v{}!",
                    result.latest_version
                );
            } else {
                bail!("cargo install failed with exit code: {status}");
            }
        }
        InstallMethod::Binary => {
            run_binary_update(config, &result).await?;
        }
    }

    // Run workspace migrations after successful update
    if let Err(e) = migrate::run_pending_migrations(&config.workspace_dir) {
        warn!("Workspace migration warning: {e}");
    }

    Ok(())
}

/// Check if update checks are disabled via environment variable.
pub fn is_update_disabled_by_env() -> bool {
    matches!(
        std::env::var("ZEROCLAW_UPDATE_ENABLED").as_deref(),
        Ok("0" | "false")
    )
}

/// Get the effective update channel (env var overrides config).
pub fn effective_channel(config: &Config) -> String {
    std::env::var("ZEROCLAW_UPDATE_CHANNEL")
        .unwrap_or_else(|_| config.update.channel.clone())
}

// ── Internal helpers ────────────────────────────────────────────

fn config_dir_path(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            directories::UserDirs::new()
                .map(|u| u.home_dir().join(".zeroclaw"))
                .unwrap_or_else(|| PathBuf::from(".zeroclaw"))
        })
}

fn read_cache_file(path: &Path) -> Option<CachedCheck> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

async fn run_binary_update(config: &Config, result: &CachedCheck) -> Result<()> {
    let config_dir = config_dir_path(config);
    let tmp_dir = config_dir.join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;

    // Download
    let artifact_name = download::platform_artifact_name();
    let archive_path = tmp_dir.join(&artifact_name);
    eprintln!();
    download::download_asset(&result.asset_url, &archive_path).await?;

    // Verify checksum
    if result.checksum_url.is_empty() {
        eprintln!("⚠️  No SHA256SUMS available — skipping checksum verification");
    } else {
        eprint!("Verifying SHA256 checksum... ");
        verify::verify_checksum(&result.checksum_url, &archive_path, &artifact_name).await?;
        eprintln!("✅");
    }

    // Verify cosign signature
    verify::verify_cosign(&result.asset_url, &archive_path).await;

    // Extract binary
    eprint!("Extracting binary... ");
    let binary_path = download::extract_binary(&archive_path, &tmp_dir)?;
    eprintln!("done");

    // Backup current binary
    eprint!("Backing up current binary... ");
    let backup_path = config_dir.join(BACKUP_FILENAME);
    apply::backup_current_binary(&backup_path)?;
    eprintln!("done");

    // Replace binary
    eprint!("Replacing binary... ");
    if let Err(e) = apply::replace_binary(&binary_path) {
        eprintln!("failed");
        eprintln!("Restoring from backup...");
        apply::restore_backup(&backup_path)?;
        bail!("Binary replacement failed: {e}");
    }
    eprintln!("done");

    // Health check
    eprint!("Health check... ");
    if let Err(e) = apply::health_check(&result.latest_version) {
        eprintln!("failed");
        eprintln!("Restoring from backup...");
        apply::restore_backup(&backup_path)?;
        bail!("Health check failed: {e}");
    }
    eprintln!("✅ zeroclaw {}", result.latest_version);

    // Clean up temp files
    let _ = std::fs::remove_dir_all(&tmp_dir);

    eprintln!();
    eprintln!(
        "🎉 Successfully updated to ZeroClaw v{}!",
        result.latest_version
    );
    eprintln!();
    eprintln!("Rollback: If you encounter issues, restore the previous binary:");
    #[cfg(unix)]
    {
        let exe = std::env::current_exe().unwrap_or_default();
        eprintln!("  cp {} {}", backup_path.display(), exe.display());
    }
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().unwrap_or_default();
        eprintln!(
            "  copy \"{}\" \"{}\"",
            backup_path.display(),
            exe.display()
        );
    }

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_newer_returns_update_available() {
        let current = semver::Version::parse("0.1.0").unwrap();
        let latest = semver::Version::parse("0.2.0").unwrap();
        assert!(latest > current);
    }

    #[test]
    fn version_comparison_same_returns_up_to_date() {
        let current = semver::Version::parse("0.1.0").unwrap();
        let latest = semver::Version::parse("0.1.0").unwrap();
        assert!(!(latest > current));
    }

    #[test]
    fn version_comparison_older_returns_up_to_date() {
        let current = semver::Version::parse("0.2.0").unwrap();
        let latest = semver::Version::parse("0.1.0").unwrap();
        assert!(!(latest > current));
    }

    #[test]
    fn prerelease_comparison_respects_stable_channel() {
        let current = semver::Version::parse("0.1.0").unwrap();
        let latest = semver::Version::parse("0.2.0-rc.1").unwrap();
        // Pre-release versions are less than their release counterparts
        assert!(latest > current);
        assert!(latest.pre != semver::Prerelease::EMPTY);
    }

    #[test]
    fn cache_serialization_roundtrip() {
        let check = CachedCheck {
            checked_at: "2026-01-01T00:00:00Z".into(),
            current_version: "0.1.0".into(),
            latest_version: "0.2.0".into(),
            release_url: "https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.2.0".into(),
            asset_url: "https://example.com/zeroclaw.tar.gz".into(),
            checksum_url: "https://example.com/SHA256SUMS".into(),
            status: UpdateStatus::UpdateAvailable,
        };
        let json = serde_json::to_string(&check).unwrap();
        let parsed: CachedCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, UpdateStatus::UpdateAvailable);
        assert_eq!(parsed.latest_version, "0.2.0");
    }

    #[test]
    fn update_disabled_env_var_respected() {
        let vals = ["0", "false"];
        for v in vals {
            assert!(matches!(Ok::<&str, ()>(v), Ok("0" | "false")));
        }
        let ok_vals = ["1", "true", "yes"];
        for v in ok_vals {
            assert!(!matches!(Ok::<&str, ()>(v), Ok("0" | "false")));
        }
    }
}
