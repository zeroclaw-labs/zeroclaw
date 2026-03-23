//! `zeroclaw update` — self-update pipeline with rollback.

use anyhow::{bail, Context, Result};
use std::path::Path;
use tracing::{info, warn};

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest";
const GITHUB_RELEASES_TAG_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/tags";

#[derive(Debug)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub download_url: Option<String>,
    pub is_newer: bool,
}

/// Check for available updates without downloading.
///
/// If `target_version` is `Some`, fetch that specific release tag instead of latest.
pub async fn check(target_version: Option<&str>) -> Result<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION").to_string();

    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{current}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let url = match target_version {
        Some(v) => {
            let tag = if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{v}")
            };
            format!("{GITHUB_RELEASES_TAG_URL}/{tag}")
        }
        None => GITHUB_RELEASES_LATEST_URL.to_string(),
    };

    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to reach GitHub releases API")?;

    if !resp.status().is_success() {
        bail!("GitHub API returned {}", resp.status());
    }

    let release: serde_json::Value = resp.json().await?;
    let tag = release["tag_name"]
        .as_str()
        .unwrap_or("unknown")
        .trim_start_matches('v')
        .to_string();

    let download_url = find_asset_url(&release);
    let is_newer = version_is_newer(&current, &tag);

    Ok(UpdateInfo {
        current_version: current,
        latest_version: tag,
        download_url,
        is_newer,
    })
}

/// Run the full 6-phase update pipeline.
///
/// If `target_version` is `Some`, fetch that specific version instead of latest.
pub async fn run(target_version: Option<&str>) -> Result<()> {
    // Phase 1: Preflight
    info!("Phase 1/6: Preflight checks...");
    let update_info = check(target_version).await?;

    if !update_info.is_newer {
        println!("Already up to date (v{}).", update_info.current_version);
        return Ok(());
    }

    println!(
        "Update available: v{} -> v{}",
        update_info.current_version, update_info.latest_version
    );

    let download_url = update_info
        .download_url
        .context("no suitable binary found for this platform")?;

    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;

    // Phase 2: Download
    info!("Phase 2/6: Downloading...");
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let download_path = temp_dir.path().join("zeroclaw_new");
    download_binary(&download_url, &download_path).await?;

    // Phase 3: Backup
    info!("Phase 3/6: Creating backup...");
    let backup_path = current_exe.with_extension("bak");
    tokio::fs::copy(&current_exe, &backup_path)
        .await
        .context("failed to backup current binary")?;

    // Phase 4: Validate
    info!("Phase 4/6: Validating download...");
    validate_binary(&download_path).await?;

    // Phase 5: Swap
    info!("Phase 5/6: Swapping binary...");
    if let Err(e) = swap_binary(&download_path, &current_exe).await {
        // Rollback
        warn!("Swap failed, rolling back: {e}");
        if let Err(rollback_err) = tokio::fs::copy(&backup_path, &current_exe).await {
            eprintln!("CRITICAL: Rollback also failed: {rollback_err}");
            eprintln!(
                "Manual recovery: cp {} {}",
                backup_path.display(),
                current_exe.display()
            );
        }
        bail!("Update failed during swap: {e}");
    }

    // Phase 6: Smoke test
    info!("Phase 6/6: Smoke test...");
    match smoke_test(&current_exe).await {
        Ok(()) => {
            // Cleanup backup on success
            let _ = tokio::fs::remove_file(&backup_path).await;
            println!("Successfully updated to v{}!", update_info.latest_version);
            Ok(())
        }
        Err(e) => {
            warn!("Smoke test failed, rolling back: {e}");
            tokio::fs::copy(&backup_path, &current_exe)
                .await
                .context("rollback after smoke test failure")?;
            bail!("Update rolled back — smoke test failed: {e}");
        }
    }
}

fn find_asset_url(release: &serde_json::Value) -> Option<String> {
    let target = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else {
            "x86_64-unknown-linux-gnu"
        }
    } else if cfg!(target_os = "windows") {
        "x86_64-pc-windows-msvc"
    } else {
        return None;
    };

    release["assets"]
        .as_array()?
        .iter()
        .find(|asset| {
            asset["name"]
                .as_str()
                .map(|name| {
                    name.starts_with("zeroclaw-")
                        && name.contains(target)
                        && (name.ends_with(".tar.gz") || name.ends_with(".zip"))
                })
                .unwrap_or(false)
        })
        .and_then(|asset| asset["browser_download_url"].as_str().map(String::from))
}

fn version_is_newer(current: &str, candidate: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').filter_map(|p| p.parse().ok()).collect() };
    let cur = parse(current);
    let cand = parse(candidate);
    cand > cur
}

async fn download_binary(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let resp = client
        .get(url)
        .send()
        .await
        .context("download request failed")?;
    if !resp.status().is_success() {
        bail!("download returned {}", resp.status());
    }

    let bytes = resp.bytes().await.context("failed to read download body")?;
    tokio::fs::write(dest, &bytes)
        .await
        .context("failed to write downloaded binary")?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(dest, perms).await?;
    }

    Ok(())
}

async fn validate_binary(path: &Path) -> Result<()> {
    let meta = tokio::fs::metadata(path).await?;
    if meta.len() < 1_000_000 {
        bail!(
            "downloaded binary too small ({} bytes), likely corrupt",
            meta.len()
        );
    }

    // Quick check: try running --version
    let output = tokio::process::Command::new(path)
        .arg("--version")
        .output()
        .await
        .context("cannot execute downloaded binary")?;

    if !output.status.success() {
        bail!("downloaded binary --version check failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("zeroclaw") {
        bail!("downloaded binary does not appear to be zeroclaw");
    }

    Ok(())
}

async fn swap_binary(new: &Path, target: &Path) -> Result<()> {
    tokio::fs::copy(new, target)
        .await
        .context("failed to overwrite binary")?;
    Ok(())
}

async fn smoke_test(binary: &Path) -> Result<()> {
    let output = tokio::process::Command::new(binary)
        .arg("--version")
        .output()
        .await
        .context("smoke test: cannot execute updated binary")?;

    if !output.status.success() {
        bail!("smoke test: updated binary returned non-zero exit code");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_asset_url_prefers_gnu_over_android() {
        let release = serde_json::json!({
            "assets": [
                {
                    "name": "zeroclaw-aarch64-linux-android.tar.gz",
                    "browser_download_url": "https://example.com/android"
                },
                {
                    "name": "zeroclaw-aarch64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/linux-gnu"
                },
                {
                    "name": "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/x86-linux-gnu"
                },
                {
                    "name": "zeroclaw-x86_64-pc-windows-msvc.zip",
                    "browser_download_url": "https://example.com/windows"
                },
                {
                    "name": "zeroclaw-aarch64-apple-darwin.tar.gz",
                    "browser_download_url": "https://example.com/macos-arm"
                }
            ]
        });

        let url = find_asset_url(&release);

        // The result depends on the compile target, but it must never be the
        // Android asset.  On any recognised platform it should resolve to a
        // URL that is NOT the android one.
        if let Some(ref u) = url {
            assert!(
                !u.contains("android"),
                "find_asset_url must not match Android binary, got: {u}"
            );
        }

        // Platform-specific assertions (compile-time).
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        assert_eq!(url.as_deref(), Some("https://example.com/linux-gnu"));

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(url.as_deref(), Some("https://example.com/x86-linux-gnu"));

        #[cfg(target_os = "windows")]
        assert_eq!(url.as_deref(), Some("https://example.com/windows"));

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(url.as_deref(), Some("https://example.com/macos-arm"));
    }

    #[test]
    fn test_find_asset_url_ignores_non_archive_assets() {
        let release = serde_json::json!({
            "assets": [
                {
                    "name": "zeroclaw-x86_64-unknown-linux-gnu.sha256",
                    "browser_download_url": "https://example.com/checksum"
                },
                {
                    "name": "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/archive"
                }
            ]
        });

        let _url = find_asset_url(&release);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(_url.as_deref(), Some("https://example.com/archive"));
    }

    #[test]
    fn test_version_comparison() {
        assert!(version_is_newer("0.4.3", "0.5.0"));
        assert!(version_is_newer("0.4.3", "0.4.4"));
        assert!(!version_is_newer("0.5.0", "0.4.3"));
        assert!(!version_is_newer("0.4.3", "0.4.3"));
        assert!(version_is_newer("1.0.0", "2.0.0"));
    }
}
