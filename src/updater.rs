use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest";
const RELEASES_TAG_URL_PREFIX: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/tags/";
const HTTP_USER_AGENT: &str = "zeroclaw-updater";

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release: ReleaseInfo,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateApplyOptions {
    pub target_version: Option<String>,
    pub install_path: Option<PathBuf>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateApplyResult {
    pub from_version: String,
    pub to_version: String,
    pub target: String,
    pub asset_name: String,
    pub install_path: PathBuf,
    pub dry_run: bool,
    pub release_url: Option<String>,
}

pub async fn fetch_latest_release_info() -> Result<ReleaseInfo> {
    fetch_release_info(None).await
}

pub async fn fetch_release_info_for_version(version: &str) -> Result<ReleaseInfo> {
    fetch_release_info(Some(version)).await
}

pub async fn check_for_updates(
    current_version: &str,
    target_version: Option<&str>,
) -> Result<UpdateCheckResult> {
    let release = if let Some(version) = target_version {
        fetch_release_info_for_version(version).await?
    } else {
        fetch_latest_release_info().await?
    };

    let current_norm = normalize_version(current_version);
    let latest_norm = normalize_version(&release.tag_name);
    let update_available = compare_versions(&latest_norm, &current_norm) == Ordering::Greater;

    Ok(UpdateCheckResult {
        current_version: current_norm,
        latest_version: latest_norm,
        update_available,
        release,
    })
}

pub async fn apply_update(options: UpdateApplyOptions) -> Result<UpdateApplyResult> {
    let from_version = normalize_version(env!("CARGO_PKG_VERSION"));
    let check = check_for_updates(&from_version, options.target_version.as_deref()).await?;

    if !check.update_available {
        let requested = options
            .target_version
            .as_deref()
            .map(normalize_version)
            .unwrap_or_else(|| check.latest_version.clone());
        bail!(
            "No update to apply: current version {} is not older than target version {}",
            check.current_version,
            requested
        );
    }

    let target = detect_release_target()?;
    let (asset_name, download_url) = release_asset_url(&check.release, &target)?;
    let install_path = resolve_install_path(options.install_path)?;

    if options.dry_run {
        return Ok(UpdateApplyResult {
            from_version: check.current_version,
            to_version: check.latest_version,
            target,
            asset_name,
            install_path,
            dry_run: true,
            release_url: check.release.html_url.clone(),
        });
    }

    let work_dir = std::env::temp_dir().join(format!(
        "zeroclaw-update-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&work_dir).context("failed to create updater temp dir")?;

    let archive_path = work_dir.join(&asset_name);
    let extract_dir = work_dir.join("extract");
    fs::create_dir_all(&extract_dir).context("failed to create updater extraction dir")?;

    let apply_result = async {
        download_release_asset(&download_url, &archive_path).await?;
        extract_release_archive(&archive_path, &extract_dir)?;

        let expected_bin = expected_binary_name();
        let new_binary = find_extracted_binary(&extract_dir, expected_bin)?;
        install_binary(&new_binary, &install_path)?;

        Ok::<UpdateApplyResult, anyhow::Error>(UpdateApplyResult {
            from_version: check.current_version,
            to_version: check.latest_version,
            target,
            asset_name,
            install_path,
            dry_run: false,
            release_url: check.release.html_url.clone(),
        })
    }
    .await;

    let _ = fs::remove_dir_all(&work_dir);
    apply_result
}

fn expected_binary_name() -> &'static str {
    if cfg!(windows) {
        "zeroclaw.exe"
    } else {
        "zeroclaw"
    }
}

fn resolve_install_path(install_path: Option<PathBuf>) -> Result<PathBuf> {
    let path = if let Some(path) = install_path {
        path
    } else {
        std::env::current_exe().context("failed to resolve current executable path")?
    };

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create install dir {}", parent.display()))?;
        }
    }
    Ok(path)
}

async fn fetch_release_info(target_version: Option<&str>) -> Result<ReleaseInfo> {
    let url = if let Some(version) = target_version {
        format!(
            "{RELEASES_TAG_URL_PREFIX}{}",
            normalize_tag_for_api(version)
        )
    } else {
        RELEASES_LATEST_URL.to_string()
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, HTTP_USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("failed to query GitHub release API")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub release API returned {}: {}", status, body);
    }

    let release: ReleaseInfo = response
        .json()
        .await
        .context("failed to parse GitHub release payload")?;

    if release.assets.is_empty() {
        bail!("release {} has no downloadable assets", release.tag_name);
    }

    Ok(release)
}

async fn download_release_asset(download_url: &str, destination: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(download_url)
        .header(reqwest::header::USER_AGENT, HTTP_USER_AGENT)
        .send()
        .await
        .with_context(|| format!("failed to download release asset: {download_url}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("asset download failed with {}: {}", status, body);
    }

    let bytes = response
        .bytes()
        .await
        .context("failed reading release asset payload")?;
    fs::write(destination, &bytes)
        .with_context(|| format!("failed to write archive {}", destination.display()))?;
    Ok(())
}

pub fn detect_release_target() -> Result<String> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => bail!("Unsupported update target: {os}/{arch}"),
    };
    Ok(target.to_string())
}

fn release_asset_filename_for_target(target: &str) -> String {
    if target.contains("windows") {
        format!("zeroclaw-{target}.zip")
    } else {
        format!("zeroclaw-{target}.tar.gz")
    }
}

pub fn release_asset_url(release: &ReleaseInfo, target: &str) -> Result<(String, String)> {
    let filename = release_asset_filename_for_target(target);
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == filename)
        .ok_or_else(|| {
            let available = release
                .assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!(
                "No release asset for target {} (expected {}). Available assets: {}",
                target,
                filename,
                available
            )
        })?;

    Ok((asset.name.clone(), asset.browser_download_url.clone()))
}

pub fn extract_release_archive(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let archive_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid archive filename"))?;

    if archive_name.ends_with(".tar.gz") {
        let status = std::process::Command::new("tar")
            .arg("xzf")
            .arg(archive_path)
            .arg("-C")
            .arg(dest_dir)
            .status()
            .context("failed to spawn tar for extraction")?;
        if !status.success() {
            bail!("tar extraction failed with status {status}");
        }
        return Ok(());
    }

    if archive_name.ends_with(".zip") {
        if cfg!(windows) {
            let command = format!(
                "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                archive_path.display(),
                dest_dir.display()
            );
            let status = std::process::Command::new("powershell")
                .arg("-NoProfile")
                .arg("-Command")
                .arg(command)
                .status()
                .context("failed to spawn powershell for zip extraction")?;
            if !status.success() {
                bail!("zip extraction failed with status {status}");
            }
        } else {
            let status = std::process::Command::new("unzip")
                .arg("-o")
                .arg(archive_path)
                .arg("-d")
                .arg(dest_dir)
                .status()
                .context("failed to spawn unzip for extraction")?;
            if !status.success() {
                bail!("zip extraction failed with status {status}");
            }
        }
        return Ok(());
    }

    bail!("unsupported archive format: {archive_name}")
}

fn find_extracted_binary(root_dir: &Path, binary_name: &str) -> Result<PathBuf> {
    let mut stack = vec![root_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read extraction dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == binary_name)
            {
                return Ok(path);
            }
        }
    }

    bail!(
        "updated binary {} not found in extracted archive {}",
        binary_name,
        root_dir.display()
    )
}

pub fn install_binary(new_binary: &Path, install_path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = new_binary;
        let _ = install_path;
        bail!(
            "Automatic in-place update is not supported on Windows while the binary is running. \
             Use --dry-run to inspect and replace manually from the release package."
        );
    }

    #[cfg(not(windows))]
    {
        let tmp_install = install_path.with_extension(format!("new.{}", std::process::id()));
        fs::copy(new_binary, &tmp_install).with_context(|| {
            format!(
                "failed to copy new binary from {} to {}",
                new_binary.display(),
                tmp_install.display()
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&tmp_install, perms).with_context(|| {
                format!(
                    "failed to set executable permissions on {}",
                    tmp_install.display()
                )
            })?;
        }

        fs::rename(&tmp_install, install_path).with_context(|| {
            format!(
                "failed to replace binary at {} (temporary: {})",
                install_path.display(),
                tmp_install.display()
            )
        })?;

        Ok(())
    }
}

pub fn normalize_version(version: &str) -> String {
    version
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
}

fn normalize_tag_for_api(version: &str) -> String {
    let normalized = normalize_version(version);
    format!("v{normalized}")
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_segments = parse_version_segments(left);
    let right_segments = parse_version_segments(right);

    let max_len = left_segments.len().max(right_segments.len());
    for idx in 0..max_len {
        let l = left_segments.get(idx).copied().unwrap_or(0);
        let r = right_segments.get(idx).copied().unwrap_or(0);
        match l.cmp(&r) {
            Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    Ordering::Equal
}

fn parse_version_segments(version: &str) -> Vec<u64> {
    normalize_version(version)
        .split('-')
        .next()
        .unwrap_or_default()
        .split('.')
        .map(|segment| segment.parse::<u64>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_release() -> ReleaseInfo {
        ReleaseInfo {
            tag_name: "v1.2.3".to_string(),
            name: Some("v1.2.3".to_string()),
            html_url: Some("https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v1.2.3".into()),
            published_at: Some("2026-02-21T00:00:00Z".to_string()),
            assets: vec![
                ReleaseAsset {
                    name: "zeroclaw-aarch64-apple-darwin.tar.gz".to_string(),
                    browser_download_url: "https://example.com/mac-arm.tar.gz".to_string(),
                },
                ReleaseAsset {
                    name: "zeroclaw-x86_64-pc-windows-msvc.zip".to_string(),
                    browser_download_url: "https://example.com/win.zip".to_string(),
                },
            ],
        }
    }

    #[test]
    fn normalize_version_strips_v_prefix() {
        assert_eq!(normalize_version("v1.2.3"), "1.2.3");
        assert_eq!(normalize_version("V2.0.0"), "2.0.0");
        assert_eq!(normalize_version("1.2.3"), "1.2.3");
    }

    #[test]
    fn compare_versions_semver_like_ordering() {
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.4", "1.2.3"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(compare_versions("2.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Ordering::Greater);
    }

    #[test]
    fn release_asset_url_selects_expected_target_asset() {
        let release = sample_release();
        let (name, url) = release_asset_url(&release, "aarch64-apple-darwin").unwrap();
        assert_eq!(name, "zeroclaw-aarch64-apple-darwin.tar.gz");
        assert_eq!(url, "https://example.com/mac-arm.tar.gz");
    }

    #[test]
    fn release_asset_url_errors_when_missing_target() {
        let release = sample_release();
        let err = release_asset_url(&release, "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(err
            .to_string()
            .contains("No release asset for target x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn release_asset_filename_for_target_uses_zip_for_windows() {
        assert_eq!(
            release_asset_filename_for_target("x86_64-pc-windows-msvc"),
            "zeroclaw-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            release_asset_filename_for_target("aarch64-apple-darwin"),
            "zeroclaw-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn normalize_tag_for_api_adds_v_prefix() {
        assert_eq!(normalize_tag_for_api("1.2.3"), "v1.2.3");
        assert_eq!(normalize_tag_for_api("v1.2.3"), "v1.2.3");
    }
}
