//! `zeroclaw update` — self-update pipeline with rollback.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::Path;

#[cfg(feature = "agent-runtime")]
use zeroclaw_runtime::i18n::get_required_cli_string_with_args;

fn update_already_current_message(version: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string_with_args("cli-update-already-current", &[("version", version)])
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        format!("Already up to date (v{version}).")
    }
}

fn update_success_message(version: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string_with_args("cli-update-success", &[("version", version)])
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        format!("Successfully updated to v{version}!")
    }
}

const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest";
const GITHUB_RELEASES_TAG_URL: &str =
    "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/tags";

#[derive(Debug)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub download_url: Option<String>,
    pub sha256sums_url: Option<String>,
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
    let sha256sums_url = find_sha256sums_url(&release);
    let is_newer = version_is_newer(&current, &tag);

    Ok(UpdateInfo {
        current_version: current,
        latest_version: tag,
        download_url,
        sha256sums_url,
        is_newer,
    })
}

/// Run the full 6-phase update pipeline.
///
/// If `target_version` is `Some`, fetch that specific version instead of latest.
pub async fn run(target_version: Option<&str>) -> Result<()> {
    // Phase 1: Preflight
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 1/6: Preflight checks..."
    );
    let update_info = check(target_version).await?;

    if !update_info.is_newer {
        println!(
            "{}",
            update_already_current_message(&update_info.current_version)
        );
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
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 2/6: Downloading..."
    );
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let download_path = temp_dir.path().join("zeroclaw_new");
    download_binary(
        &download_url,
        update_info.sha256sums_url.as_deref(),
        &download_path,
    )
    .await?;

    // Phase 3: Backup
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 3/6: Creating backup..."
    );
    let backup_path = current_exe.with_extension("bak");
    tokio::fs::copy(&current_exe, &backup_path)
        .await
        .context("failed to backup current binary")?;

    // Phase 4: Validate
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 4/6: Validating download..."
    );
    validate_binary(&download_path).await?;

    // Phase 5: Swap
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 5/6: Swapping binary..."
    );
    if let Err(e) = swap_binary(&download_path, &current_exe).await {
        // Rollback
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "Swap failed, rolling back"
        );
        if let Err(rollback_err) = rollback_binary(&backup_path, &current_exe).await {
            eprintln!("CRITICAL: Rollback also failed: {rollback_err}"); // i18n-exempt: emergency operator recovery diagnostic, must be unambiguous
            eprintln!(
                "Manual recovery: cp {} {}",
                backup_path.display(),
                current_exe.display()
            );
        }
        bail!("Update failed during swap: {e}");
    }

    // Phase 6: Smoke test
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 6/6: Smoke test..."
    );
    match smoke_test(&current_exe).await {
        Ok(()) => {
            // Cleanup backup on success
            let _ = tokio::fs::remove_file(&backup_path).await;
            println!("{}", update_success_message(&update_info.latest_version));
            Ok(())
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Smoke test failed, rolling back"
            );
            rollback_binary(&backup_path, &current_exe)
                .await
                .context("rollback after smoke test failure")?;
            bail!("Update rolled back — smoke test failed: {e}");
        }
    }
}

fn find_asset_url(release: &serde_json::Value) -> Option<String> {
    let target = current_target_triple()?;

    release["assets"].as_array()?.iter().find_map(|asset| {
        let name = asset["name"].as_str()?;
        if !is_installable_release_asset(name, target) {
            return None;
        }
        let url = asset["browser_download_url"].as_str()?.trim();
        (!url.is_empty()).then(|| url.to_string())
    })
}

fn find_sha256sums_url(release: &serde_json::Value) -> Option<String> {
    let assets = release["assets"].as_array()?;
    assets
        .iter()
        .find_map(|asset| sha256sums_url_for_asset(asset, is_exact_sha256sums_asset))
        .or_else(|| {
            assets
                .iter()
                .find_map(|asset| sha256sums_url_for_asset(asset, is_sha256sums_asset))
        })
}

fn sha256sums_url_for_asset(
    asset: &serde_json::Value,
    predicate: impl Fn(&str) -> bool,
) -> Option<String> {
    let name = asset["name"].as_str()?;
    if !predicate(name) {
        return None;
    }
    let url = asset["browser_download_url"].as_str()?.trim();
    (!url.is_empty()).then(|| url.to_string())
}

fn is_exact_sha256sums_asset(name: &str) -> bool {
    name.eq_ignore_ascii_case("sha256sums")
}

fn is_sha256sums_asset(name: &str) -> bool {
    is_exact_sha256sums_asset(name)
        || name.eq_ignore_ascii_case("sha256sums.txt")
        || name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("sha256sums"))
}

fn is_installable_release_asset(name: &str, target: &str) -> bool {
    // .tar.gz and .tgz are universal across all platforms
    if name == format!("zeroclaw-{target}.tar.gz") || name == format!("zeroclaw-{target}.tgz") {
        return true;
    }
    // On Windows the release artifacts are published as .zip
    if target.contains("windows") && name == format!("zeroclaw-{target}.zip") {
        return true;
    }
    false
}

/// Return the exact Rust target triple for the current platform.
///
/// Using full triples (e.g. `aarch64-unknown-linux-gnu` instead of the
/// shorter `aarch64-unknown-linux`) prevents substring matches from
/// selecting the wrong asset (e.g. an Android binary on a GNU/Linux host).
fn current_target_triple() -> Option<&'static str> {
    target_triple_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "gnu"),
    )
}

fn target_triple_for(os: &str, arch: &str, windows_gnu: bool) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        ("windows", "x86_64") if windows_gnu => Some("x86_64-pc-windows-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn version_is_newer(current: &str, candidate: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').filter_map(|p| p.parse().ok()).collect() };
    let cur = parse(current);
    let cand = parse(candidate);
    cand > cur
}

async fn download_binary(url: &str, sha256sums_url: Option<&str>, dest: &Path) -> Result<()> {
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

    if let Some(sums_url) = sha256sums_url {
        verify_download_checksum(&bytes, url, sums_url, &client).await?;
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "No SHA256SUMS asset found; skipping update download checksum verification"
        );
    }

    // Release assets are .tar.gz archives containing a single `zeroclaw` binary.
    // Extract the binary from the archive instead of writing the raw tarball.
    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        extract_tar_gz(&bytes, dest).context("failed to extract binary from tar.gz archive")?;
    } else {
        tokio::fs::write(dest, &bytes)
            .await
            .context("failed to write downloaded binary")?;
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(dest, perms).await?;
    }

    Ok(())
}

async fn verify_download_checksum(
    bytes: &[u8],
    asset_url: &str,
    sha256sums_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let asset_name = asset_name_from_url(asset_url)
        .context("cannot derive release asset filename from download URL")?;

    let sums_resp = client
        .get(sha256sums_url)
        .send()
        .await
        .context("failed to fetch SHA256SUMS")?;
    if !sums_resp.status().is_success() {
        bail!("SHA256SUMS fetch returned {}", sums_resp.status());
    }

    let sums_text = sums_resp
        .text()
        .await
        .context("failed to read SHA256SUMS body")?;
    verify_checksum_bytes(bytes, &asset_name, &sums_text)?;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({"asset": asset_name})),
        "Update download checksum verified"
    );
    Ok(())
}

fn verify_checksum_bytes(bytes: &[u8], asset_name: &str, sums_text: &str) -> Result<()> {
    let expected_hex = expected_sha256_for_asset(sums_text, asset_name)?;
    let actual_hex = hex::encode(Sha256::digest(bytes));

    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        bail!(
            "checksum mismatch for '{asset_name}': expected {expected_hex}, got {actual_hex}. \
             The downloaded update may be corrupted or tampered with."
        );
    }

    Ok(())
}

fn asset_name_from_url(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .path_segments()?
        .next_back()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn expected_sha256_for_asset<'a>(sums_text: &'a str, asset_name: &str) -> Result<&'a str> {
    for line in sums_text.lines() {
        let mut parts = line.split_whitespace();
        let Some(digest) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        let name = name.trim_start_matches('*');
        if name == asset_name {
            if parts.next().is_some() {
                bail!("invalid SHA256SUMS entry for '{asset_name}'");
            }
            if !is_sha256_hex(digest) {
                bail!("invalid SHA256SUMS entry for '{asset_name}'");
            }
            return Ok(digest);
        }
    }

    bail!("asset '{asset_name}' not found in SHA256SUMS")
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extract the `zeroclaw` binary from a `.tar.gz` archive.
fn extract_tar_gz(archive_bytes: &[u8], dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);

    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("failed to read tar entry")?;
        let path = entry.path().context("failed to read entry path")?;

        // The archive contains a single binary named "zeroclaw" (or "zeroclaw.exe" on Windows).
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

    // Check binary architecture before attempting execution so we can give
    // a clear diagnostic instead of the opaque "Exec format error (os error 8)".
    check_binary_arch(path).await?;

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

/// Read the binary header and verify its architecture matches the host.
///
/// On Linux/FreeBSD this reads the ELF header; on macOS the Mach-O header.
/// If the binary is for a different architecture, returns a descriptive error
/// instead of the opaque "Exec format error (os error 8)".
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

    if let (Some(bin), Some(host)) = (binary_arch, host_arch) {
        if bin != host {
            bail!(
                "architecture mismatch: downloaded binary is {bin} but this host is {host} — \
                 the release asset may be mispackaged"
            );
        }
    }

    Ok(())
}

/// Detect the CPU architecture from an ELF or Mach-O binary header.
fn detect_arch_from_header(header: &[u8]) -> Option<&'static str> {
    // ELF magic: 0x7f 'E' 'L' 'F'
    if header.len() >= 20 && header[0..4] == [0x7f, b'E', b'L', b'F'] {
        // e_machine is at offset 18 (2 bytes, little-endian for LE binaries)
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

    // Mach-O magic (64-bit little-endian): 0xFEEDFACF
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

/// Return the host CPU architecture as a human-readable string.
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
    // On Linux, a running binary cannot be overwritten in place (ETXTBSY).
    // Remove the old file first, then copy the new one into the now-free path.
    // This works because the kernel keeps the inode alive until the process exits.
    tokio::fs::remove_file(target)
        .await
        .context("failed to remove old binary")?;
    tokio::fs::copy(new, target)
        .await
        .context("failed to write new binary")?;
    Ok(())
}

async fn rollback_binary(backup: &Path, target: &Path) -> Result<()> {
    // Remove-then-copy to avoid ETXTBSY if the target is somehow still mapped.
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
        let triple = current_target_triple().expect("supported test platform");
        // The triple must contain at least two hyphens (arch-vendor-os or arch-vendor-os-env)
        assert!(
            triple.matches('-').count() >= 2,
            "triple should have at least two hyphens: {triple}"
        );
    }

    #[test]
    fn target_triple_for_rejects_unsupported_architectures() {
        assert_eq!(target_triple_for("linux", "arm", false), None);
        assert_eq!(target_triple_for("macos", "powerpc", false), None);
        assert_eq!(target_triple_for("windows", "x86", false), None);
    }

    #[test]
    fn target_triple_for_distinguishes_windows_envs() {
        assert_eq!(
            target_triple_for("windows", "x86_64", false),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(
            target_triple_for("windows", "x86_64", true),
            Some("x86_64-pc-windows-gnu")
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
        // Must NOT match the android binary
        assert!(
            !url.contains("android"),
            "should not select android binary, got: {url}"
        );
    }

    #[test]
    fn find_asset_url_ignores_non_installable_assets() {
        let target = current_target_triple().expect("supported test platform");
        let release = make_release(&[
            &format!("zeroclaw-{target}.tar.gz.sha256"),
            &format!("zeroclaw-{target}.zip.sha256"),
            &format!("zeroclaw-{target}.zip"),
            &format!("zeroclaw-{target}.tar.gz"),
        ]);

        let url = find_asset_url(&release).expect("should select archive asset");
        let is_tar = url.ends_with(".tar.gz");
        let is_zip = url.ends_with(".zip");
        assert!(
            is_tar || is_zip,
            "should select release archive, got: {url}"
        );
    }

    #[test]
    fn find_asset_url_skips_matching_asset_with_unusable_url() {
        let target = current_target_triple().expect("supported test platform");
        let release = serde_json::json!({
            "assets": [
                {
                    "name": format!("zeroclaw-{target}.tar.gz"),
                    "browser_download_url": ""
                },
                {
                    "name": format!("zeroclaw-{target}.tgz"),
                    "browser_download_url": null
                },
                {
                    "name": format!("zeroclaw-{target}.tar.gz"),
                    "browser_download_url": format!("https://example.com/zeroclaw-{target}.tar.gz")
                }
            ]
        });

        let url = find_asset_url(&release).expect("should skip unusable URLs");
        assert_eq!(url, format!("https://example.com/zeroclaw-{target}.tar.gz"));
    }

    #[test]
    fn find_asset_url_ignores_non_zeroclaw_assets() {
        let target = current_target_triple().expect("supported test platform");
        let release = make_release(&[
            &format!("helper-{target}.tar.gz"),
            &format!("zeroclaw-{target}.tar.gz"),
        ]);

        let url = find_asset_url(&release).expect("should select zeroclaw asset");
        assert!(
            url.contains(&format!("zeroclaw-{target}.tar.gz")),
            "should select zeroclaw archive, got: {url}"
        );
    }

    #[test]
    fn installable_release_asset_rejects_unknown_target() {
        assert!(!is_installable_release_asset(
            "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
            "unknown"
        ));
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
    fn find_sha256sums_url_accepts_common_names() {
        for name in ["SHA256SUMS", "sha256sums.txt", "checksums.sha256sums"] {
            let release = make_release(&[name]);
            assert_eq!(
                find_sha256sums_url(&release),
                Some(format!("https://example.com/{name}"))
            );
        }
    }

    #[test]
    fn find_sha256sums_url_is_case_insensitive() {
        let release = make_release(&["Sha256Sums"]);
        assert_eq!(
            find_sha256sums_url(&release),
            Some("https://example.com/Sha256Sums".to_string())
        );
    }

    #[test]
    fn find_sha256sums_url_skips_missing_or_unusable_url() {
        let release = serde_json::json!({
            "assets": [
                {
                    "name": "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/asset"
                },
                {
                    "name": "SHA256SUMS",
                    "browser_download_url": ""
                },
                {
                    "name": "sha256sums.txt",
                    "browser_download_url": null
                },
                {
                    "name": "checksums.sha256sums",
                    "browser_download_url": "https://example.com/checksums.sha256sums"
                }
            ]
        });

        assert_eq!(
            find_sha256sums_url(&release),
            Some("https://example.com/checksums.sha256sums".to_string())
        );
    }

    #[test]
    fn find_sha256sums_url_prefers_canonical_asset() {
        let release = serde_json::json!({
            "assets": [
                {
                    "name": "checksums.sha256sums",
                    "browser_download_url": "https://example.com/checksums.sha256sums"
                },
                {
                    "name": "SHA256SUMS",
                    "browser_download_url": "https://example.com/SHA256SUMS"
                }
            ]
        });

        assert_eq!(
            find_sha256sums_url(&release),
            Some("https://example.com/SHA256SUMS".to_string())
        );
    }

    #[test]
    fn expected_sha256_for_asset_matches_text_and_binary_mode_entries() {
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let sums = format!(
            "{digest}  zeroclaw-aarch64-apple-darwin.tar.gz\n\
             {digest} *zeroclaw-x86_64-unknown-linux-gnu.tar.gz\n"
        );

        assert_eq!(
            expected_sha256_for_asset(&sums, "zeroclaw-aarch64-apple-darwin.tar.gz").unwrap(),
            digest
        );
        assert_eq!(
            expected_sha256_for_asset(&sums, "zeroclaw-x86_64-unknown-linux-gnu.tar.gz").unwrap(),
            digest
        );
    }

    #[test]
    fn expected_sha256_for_asset_rejects_missing_or_malformed_entry() {
        let err = expected_sha256_for_asset(
            "not-a-hex-digest  zeroclaw-x86_64-unknown-linux-gnu.tar.gz\n",
            "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid SHA256SUMS entry"));

        let err = expected_sha256_for_asset(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  other.tar.gz\n",
            "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not found"));

        let err = expected_sha256_for_asset(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  zeroclaw-x86_64-unknown-linux-gnu.tar.gz extra\n",
            "zeroclaw-x86_64-unknown-linux-gnu.tar.gz",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid SHA256SUMS entry"));
    }

    #[test]
    fn verify_checksum_bytes_accepts_matching_digest_and_rejects_mismatch() {
        let asset_name = "zeroclaw-x86_64-unknown-linux-gnu.tar.gz";
        let digest = hex::encode(Sha256::digest(b"downloaded bytes"));
        let sums = format!("{digest}  {asset_name}\n");

        verify_checksum_bytes(b"downloaded bytes", asset_name, &sums).unwrap();

        let err = verify_checksum_bytes(b"tampered bytes", asset_name, &sums)
            .unwrap_err()
            .to_string();
        assert!(err.contains("checksum mismatch"));
    }

    #[test]
    fn asset_name_from_url_uses_last_path_component() {
        assert_eq!(
            asset_name_from_url(
                "https://github.com/zeroclaw-labs/zeroclaw/releases/download/v0.8.0/zeroclaw-aarch64-apple-darwin.tar.gz"
            ),
            Some("zeroclaw-aarch64-apple-darwin.tar.gz".to_string())
        );
        assert_eq!(
            asset_name_from_url(
                "https://github.com/zeroclaw-labs/zeroclaw/releases/download/v0.8.0/zeroclaw-aarch64-apple-darwin.tar.gz?download=1#asset"
            ),
            Some("zeroclaw-aarch64-apple-darwin.tar.gz".to_string())
        );
        assert_eq!(asset_name_from_url("https://example.com/releases/"), None);
    }

    #[tokio::test]
    async fn download_binary_verifies_checksum_before_writing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let asset = b"downloaded bytes";
        let digest = hex::encode(Sha256::digest(asset));
        let sums = format!("{digest}  zeroclaw-test.bin\n");

        Mock::given(method("GET"))
            .and(path("/zeroclaw-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(asset))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/SHA256SUMS"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sums))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_new");
        download_binary(
            &format!("{}/zeroclaw-test.bin", server.uri()),
            Some(&format!("{}/SHA256SUMS", server.uri())),
            &dest,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(dest).unwrap(), asset);
    }

    #[tokio::test]
    async fn download_binary_rejects_checksum_mismatch_without_writing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let asset = b"downloaded bytes";
        let digest = hex::encode(Sha256::digest(b"different bytes"));
        let sums = format!("{digest}  zeroclaw-test.bin\n");

        Mock::given(method("GET"))
            .and(path("/zeroclaw-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(asset))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/SHA256SUMS"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sums))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_new");
        let err = download_binary(
            &format!("{}/zeroclaw-test.bin", server.uri()),
            Some(&format!("{}/SHA256SUMS", server.uri())),
            &dest,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("checksum mismatch"));
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn download_binary_preserves_missing_checksum_fallback() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let asset = b"downloaded bytes";

        Mock::given(method("GET"))
            .and(path("/zeroclaw-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(asset))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_new");
        download_binary(&format!("{}/zeroclaw-test.bin", server.uri()), None, &dest)
            .await
            .unwrap();

        assert_eq!(std::fs::read(dest).unwrap(), asset);
    }

    #[test]
    fn detect_arch_elf_x86_64() {
        // Minimal ELF header with e_machine = 0x3E (x86_64)
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
        // Mach-O 64-bit LE magic + cputype 0x01000007 (x86_64)
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
        let header = vec![0u8; 20]; // all zeros — not ELF or Mach-O
        assert_eq!(detect_arch_from_header(&header), None);
    }

    #[test]
    fn detect_arch_too_short() {
        let header = vec![0x7f, b'E', b'L', b'F']; // only 4 bytes
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

        // Build a tar.gz in memory containing a fake "zeroclaw" binary.
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

        // Build a tar.gz with a file that is NOT named "zeroclaw".
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
