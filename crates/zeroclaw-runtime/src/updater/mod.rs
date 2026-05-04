//! Self-update pipeline shared between the CLI (`zeroclaw update`) and the
//! gateway (`POST /api/system/update`).
//!
//! The pipeline is six phases — preflight, download, backup, validate, swap,
//! smoke-test — with automatic rollback on validate/swap/smoke-test failure.
//! Both entry points share the same staged execution; the gateway version
//! additionally streams structured `UpdateEvent`s so the web UI can render
//! progress.

use anyhow::{Context, Result, bail};
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{info, warn};

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest";
const GITHUB_RELEASES_TAG_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/tags";

/// Result of a release-info lookup. Rendered by both the CLI version-check
/// path and the gateway's `GET /api/system/version` endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub download_url: Option<String>,
    pub is_newer: bool,
    pub latest_published_at: Option<String>,
}

/// One phase of the update pipeline. Reported in `UpdateEvent::phase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Preflight,
    Download,
    Backup,
    Validate,
    Swap,
    SmokeTest,
    Done,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateLevel {
    Info,
    Warn,
    Error,
}

/// Single progress event emitted by `run_with_progress`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateEvent {
    pub task_id: String,
    pub phase: UpdatePhase,
    pub level: UpdateLevel,
    pub message: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl UpdateEvent {
    fn new(
        task_id: &str,
        phase: UpdatePhase,
        level: UpdateLevel,
        message: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.to_string(),
            phase,
            level,
            message: message.into(),
            timestamp: chrono::Utc::now(),
        }
    }
}

/// Check for available updates without downloading.
///
/// If `target_version` is `Some`, fetch that specific release tag instead of
/// latest.
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
    let latest_published_at = release["published_at"].as_str().map(String::from);

    Ok(UpdateInfo {
        current_version: current,
        latest_version: tag,
        download_url,
        is_newer,
        latest_published_at,
    })
}

/// Run the full 6-phase update pipeline with stdout-style logging.
///
/// Used by the CLI. For the gateway, prefer `run_with_progress` so events can
/// be streamed to the web UI.
pub async fn run(target_version: Option<&str>) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<UpdateEvent>(64);
    // Drain events to stdout so the CLI experience is unchanged.
    let drain = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event.level {
                UpdateLevel::Info => info!("{}", event.message),
                UpdateLevel::Warn => warn!("{}", event.message),
                UpdateLevel::Error => eprintln!("error: {}", event.message),
            }
        }
    });

    let task_id = "cli".to_string();
    let result = run_with_progress(target_version, &task_id, tx).await;
    let _ = drain.await;
    result.map(|_| ())
}

/// Run the full 6-phase update pipeline, streaming events through `tx`.
///
/// Returns the final `UpdateInfo` (with `is_newer = false` if no update was
/// needed) on success, or an `Err` if any phase failed. Rollback runs
/// automatically on validate/swap/smoke-test failure; the failure path emits
/// a final `RolledBack` (or `Failed` if rollback also failed) event before
/// returning the error.
pub async fn run_with_progress(
    target_version: Option<&str>,
    task_id: &str,
    tx: mpsc::Sender<UpdateEvent>,
) -> Result<UpdateInfo> {
    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::Preflight,
            UpdateLevel::Info,
            "Phase 1/6: Preflight checks…",
        ))
        .await;

    let update_info = match check(target_version).await {
        Ok(info) => info,
        Err(e) => {
            let _ = tx
                .send(UpdateEvent::new(
                    task_id,
                    UpdatePhase::Failed,
                    UpdateLevel::Error,
                    format!("Preflight failed: {e}"),
                ))
                .await;
            return Err(e);
        }
    };

    if !update_info.is_newer {
        let _ = tx
            .send(UpdateEvent::new(
                task_id,
                UpdatePhase::Done,
                UpdateLevel::Info,
                format!("Already up to date (v{})", update_info.current_version),
            ))
            .await;
        return Ok(update_info);
    }

    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::Preflight,
            UpdateLevel::Info,
            format!(
                "Update available: v{} → v{}",
                update_info.current_version, update_info.latest_version
            ),
        ))
        .await;

    let download_url = match update_info.download_url.clone() {
        Some(url) => url,
        None => {
            let msg = "no suitable binary found for this platform";
            let _ = tx
                .send(UpdateEvent::new(
                    task_id,
                    UpdatePhase::Failed,
                    UpdateLevel::Error,
                    msg,
                ))
                .await;
            bail!(msg);
        }
    };

    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;

    // Phase 2: Download
    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::Download,
            UpdateLevel::Info,
            "Phase 2/6: Downloading…",
        ))
        .await;
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let download_path = temp_dir.path().join("zeroclaw_new");
    if let Err(e) = download_binary(&download_url, &download_path).await {
        let _ = tx
            .send(UpdateEvent::new(
                task_id,
                UpdatePhase::Failed,
                UpdateLevel::Error,
                format!("Download failed: {e}"),
            ))
            .await;
        return Err(e);
    }

    // Phase 3: Backup
    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::Backup,
            UpdateLevel::Info,
            "Phase 3/6: Creating backup…",
        ))
        .await;
    let backup_path = current_exe.with_extension("bak");
    if let Err(e) = tokio::fs::copy(&current_exe, &backup_path).await {
        let _ = tx
            .send(UpdateEvent::new(
                task_id,
                UpdatePhase::Failed,
                UpdateLevel::Error,
                format!("Backup failed: {e}"),
            ))
            .await;
        return Err(e).context("failed to backup current binary");
    }

    // Phase 4: Validate
    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::Validate,
            UpdateLevel::Info,
            "Phase 4/6: Validating download…",
        ))
        .await;
    if let Err(e) = validate_binary(&download_path).await {
        let _ = tx
            .send(UpdateEvent::new(
                task_id,
                UpdatePhase::Failed,
                UpdateLevel::Error,
                format!("Validation failed: {e}"),
            ))
            .await;
        return Err(e);
    }

    // Phase 5: Swap
    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::Swap,
            UpdateLevel::Info,
            "Phase 5/6: Swapping binary…",
        ))
        .await;
    if let Err(e) = swap_binary(&download_path, &current_exe).await {
        let _ = tx
            .send(UpdateEvent::new(
                task_id,
                UpdatePhase::RolledBack,
                UpdateLevel::Warn,
                format!("Swap failed, rolling back: {e}"),
            ))
            .await;
        if let Err(rollback_err) = rollback_binary(&backup_path, &current_exe).await {
            let _ = tx
                .send(UpdateEvent::new(
                    task_id,
                    UpdatePhase::Failed,
                    UpdateLevel::Error,
                    format!(
                        "CRITICAL: rollback also failed: {rollback_err}. Manual recovery: cp {} {}",
                        backup_path.display(),
                        current_exe.display()
                    ),
                ))
                .await;
        }
        bail!("Update failed during swap: {e}");
    }

    // Phase 6: Smoke test
    let _ = tx
        .send(UpdateEvent::new(
            task_id,
            UpdatePhase::SmokeTest,
            UpdateLevel::Info,
            "Phase 6/6: Smoke test…",
        ))
        .await;
    match smoke_test(&current_exe).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&backup_path).await;
            let _ = tx
                .send(UpdateEvent::new(
                    task_id,
                    UpdatePhase::Done,
                    UpdateLevel::Info,
                    format!("Successfully updated to v{}", update_info.latest_version),
                ))
                .await;
            Ok(update_info)
        }
        Err(e) => {
            let _ = tx
                .send(UpdateEvent::new(
                    task_id,
                    UpdatePhase::RolledBack,
                    UpdateLevel::Warn,
                    format!("Smoke test failed, rolling back: {e}"),
                ))
                .await;
            rollback_binary(&backup_path, &current_exe)
                .await
                .context("rollback after smoke test failure")?;
            let _ = tx
                .send(UpdateEvent::new(
                    task_id,
                    UpdatePhase::RolledBack,
                    UpdateLevel::Warn,
                    "Rollback complete — running previous binary",
                ))
                .await;
            bail!("Update rolled back — smoke test failed: {e}");
        }
    }
}

fn find_asset_url(release: &serde_json::Value) -> Option<String> {
    let target = current_target_triple();

    release["assets"]
        .as_array()?
        .iter()
        .find(|asset| {
            asset["name"]
                .as_str()
                .map(|name| name.contains(target))
                .unwrap_or(false)
        })
        .and_then(|asset| asset["browser_download_url"].as_str().map(String::from))
}

fn current_target_triple() -> &'static str {
    if cfg!(target_os = "macos") {
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
        if cfg!(target_arch = "aarch64") {
            "aarch64-pc-windows-msvc"
        } else if cfg!(target_env = "gnu") {
            "x86_64-pc-windows-gnu"
        } else {
            "x86_64-pc-windows-msvc"
        }
    } else {
        "unknown"
    }
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

    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        extract_tar_gz(&bytes, dest).context("failed to extract binary from tar.gz archive")?;
    } else {
        tokio::fs::write(dest, &bytes)
            .await
            .context("failed to write downloaded binary")?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(dest, perms).await?;
    }

    Ok(())
}

fn extract_tar_gz(archive_bytes: &[u8], dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);

    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("failed to read tar entry")?;
        let path = entry.path().context("failed to read entry path")?;

        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if file_name == "zeroclaw" || file_name == "zeroclaw.exe" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("failed to read binary from archive")?;
            std::fs::write(dest, &buf).context("failed to write extracted binary")?;
            return Ok(());
        }
    }

    bail!("archive does not contain a 'zeroclaw' binary")
}

async fn validate_binary(path: &Path) -> Result<()> {
    let meta = tokio::fs::metadata(path).await?;
    if meta.len() < 1_000_000 {
        bail!(
            "downloaded binary too small ({} bytes), likely corrupt",
            meta.len()
        );
    }

    check_binary_arch(path).await?;

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

async fn check_binary_arch(path: &Path) -> Result<()> {
    let header = tokio::fs::read(path)
        .await
        .map(|bytes| bytes.into_iter().take(32).collect::<Vec<u8>>())
        .context("failed to read binary header")?;

    if header.len() < 20 {
        bail!("downloaded file too small to be a valid binary");
    }

    let binary_arch = detect_arch_from_header(&header);
    let host_arch = host_architecture();

    if let (Some(bin), Some(host)) = (binary_arch, host_arch)
        && bin != host
    {
        bail!(
            "architecture mismatch: downloaded binary is {bin} but this host is {host} — \
             the release asset may be mispackaged"
        );
    }

    Ok(())
}

fn detect_arch_from_header(header: &[u8]) -> Option<&'static str> {
    if header.len() >= 20 && header[0..4] == [0x7f, b'E', b'L', b'F'] {
        let e_machine = u16::from_le_bytes([header[18], header[19]]);
        return Some(match e_machine {
            0x3E => "x86_64",
            0xB7 => "aarch64",
            0x03 => "x86",
            0x28 => "arm",
            0xF3 => "riscv",
            _ => "unknown-elf",
        });
    }

    if header.len() >= 8 && header[0..4] == [0xCF, 0xFA, 0xED, 0xFE] {
        let cputype = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        return Some(match cputype {
            0x0100_0007 => "x86_64",
            0x0100_000C => "aarch64",
            _ => "unknown-macho",
        });
    }

    None
}

fn host_architecture() -> Option<&'static str> {
    if cfg!(target_arch = "x86_64") {
        Some("x86_64")
    } else if cfg!(target_arch = "aarch64") {
        Some("aarch64")
    } else if cfg!(target_arch = "x86") {
        Some("x86")
    } else if cfg!(target_arch = "arm") {
        Some("arm")
    } else {
        None
    }
}

async fn swap_binary(new: &Path, target: &Path) -> Result<()> {
    tokio::fs::remove_file(target)
        .await
        .context("failed to remove old binary")?;
    tokio::fs::copy(new, target)
        .await
        .context("failed to write new binary")?;
    Ok(())
}

async fn rollback_binary(backup: &Path, target: &Path) -> Result<()> {
    let _ = tokio::fs::remove_file(target).await;
    tokio::fs::copy(backup, target)
        .await
        .context("failed to restore backup binary")?;
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
    fn test_version_comparison() {
        assert!(version_is_newer("0.4.3", "0.5.0"));
        assert!(version_is_newer("0.4.3", "0.4.4"));
        assert!(!version_is_newer("0.5.0", "0.4.3"));
        assert!(!version_is_newer("0.4.3", "0.4.3"));
        assert!(version_is_newer("1.0.0", "2.0.0"));
    }

    #[test]
    fn current_target_triple_is_not_empty() {
        let triple = current_target_triple();
        assert_ne!(triple, "unknown", "unsupported platform");
        assert!(
            triple.matches('-').count() >= 2,
            "triple should have at least two hyphens: {triple}"
        );
    }

    fn make_release(assets: &[&str]) -> serde_json::Value {
        let assets: Vec<serde_json::Value> = assets
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "browser_download_url": format!("https://example.com/{name}")
                })
            })
            .collect();
        serde_json::json!({ "assets": assets })
    }

    #[test]
    fn find_asset_url_picks_correct_gnu_over_android() {
        let release = make_release(&[
            "zeroclaw-aarch64-linux-android.tar.gz",
            "zeroclaw-aarch64-unknown-linux-gnu.tar.gz",
            "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
            "zeroclaw-x86_64-apple-darwin.tar.gz",
            "zeroclaw-aarch64-apple-darwin.tar.gz",
            "zeroclaw-x86_64-pc-windows-msvc.zip",
            "zeroclaw-aarch64-pc-windows-msvc.zip",
        ]);

        let url = find_asset_url(&release);
        assert!(url.is_some(), "should find an asset");
        let url = url.unwrap();
        assert!(
            !url.contains("android"),
            "should not select android binary, got: {url}"
        );
    }

    #[test]
    fn find_asset_url_returns_none_for_empty_assets() {
        let release = serde_json::json!({ "assets": [] });
        assert!(find_asset_url(&release).is_none());
    }

    #[test]
    fn find_asset_url_returns_none_for_missing_assets() {
        let release = serde_json::json!({});
        assert!(find_asset_url(&release).is_none());
    }

    #[test]
    fn detect_arch_elf_x86_64() {
        let mut header = vec![0u8; 20];
        header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        header[18] = 0x3E;
        header[19] = 0x00;
        assert_eq!(detect_arch_from_header(&header), Some("x86_64"));
    }

    #[test]
    fn detect_arch_elf_aarch64() {
        let mut header = vec![0u8; 20];
        header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        header[18] = 0xB7;
        header[19] = 0x00;
        assert_eq!(detect_arch_from_header(&header), Some("aarch64"));
    }

    #[test]
    fn detect_arch_macho_x86_64() {
        let mut header = vec![0u8; 8];
        header[0..4].copy_from_slice(&[0xCF, 0xFA, 0xED, 0xFE]);
        header[4..8].copy_from_slice(&0x0100_0007u32.to_le_bytes());
        assert_eq!(detect_arch_from_header(&header), Some("x86_64"));
    }

    #[test]
    fn detect_arch_macho_aarch64() {
        let mut header = vec![0u8; 8];
        header[0..4].copy_from_slice(&[0xCF, 0xFA, 0xED, 0xFE]);
        header[4..8].copy_from_slice(&0x0100_000Cu32.to_le_bytes());
        assert_eq!(detect_arch_from_header(&header), Some("aarch64"));
    }

    #[test]
    fn detect_arch_unknown_format() {
        let header = vec![0u8; 20];
        assert_eq!(detect_arch_from_header(&header), None);
    }

    #[test]
    fn detect_arch_too_short() {
        let header = vec![0x7f, b'E', b'L', b'F'];
        assert_eq!(detect_arch_from_header(&header), None);
    }

    #[test]
    fn host_architecture_is_known() {
        assert!(
            host_architecture().is_some(),
            "host architecture should be detected on CI platforms"
        );
    }

    #[test]
    fn extract_tar_gz_finds_binary() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let fake_binary = b"#!/bin/sh\necho zeroclaw";
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_size(fake_binary.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "zeroclaw", &fake_binary[..])
                .unwrap();
            builder.finish().unwrap();
        }

        let mut gz_buf = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::fast());
            encoder.write_all(&tar_buf).unwrap();
            encoder.finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_extracted");
        extract_tar_gz(&gz_buf, &dest).unwrap();

        let content = std::fs::read(&dest).unwrap();
        assert_eq!(content, fake_binary);
    }

    #[test]
    fn extract_tar_gz_errors_on_missing_binary() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "README.md", &b"hello"[..])
                .unwrap();
            builder.finish().unwrap();
        }

        let mut gz_buf = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::fast());
            encoder.write_all(&tar_buf).unwrap();
            encoder.finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_extracted");
        let result = extract_tar_gz(&gz_buf, &dest);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("does not contain"),
            "should report missing binary"
        );
    }
}
