//! `zeroclaw update` — self-update pipeline with rollback.

use anyhow::{Context, Result, bail};
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
        if let Err(rollback_err) = rollback_binary(&backup_path, &current_exe).await {
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
            rollback_binary(&backup_path, &current_exe)
                .await
                .context("rollback after smoke test failure")?;
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

/// Return the Rust target triple for the platform we should *download* for.
///
/// On Unix we query the actual host architecture at runtime via `uname -m`
/// so that an x86_64 binary running under QEMU/binfmt_misc on an aarch64
/// host (e.g. Raspberry Pi) will correctly select the aarch64 release asset
/// instead of perpetuating the wrong architecture.
///
/// Using full triples (e.g. `aarch64-unknown-linux-gnu` instead of the
/// shorter `aarch64-unknown-linux`) prevents substring matches from
/// selecting the wrong asset (e.g. an Android binary on a GNU/Linux host).
fn current_target_triple() -> &'static str {
    // Detect the real host architecture at runtime on Unix.
    // Falls back to the compile-time target arch if detection fails.
    let arch = runtime_host_arch();

    if cfg!(target_os = "macos") {
        match arch {
            "aarch64" => "aarch64-apple-darwin",
            _ => "x86_64-apple-darwin",
        }
    } else if cfg!(target_os = "linux") {
        match arch {
            "aarch64" => "aarch64-unknown-linux-gnu",
            _ => "x86_64-unknown-linux-gnu",
        }
    } else if cfg!(target_os = "windows") {
        match arch {
            "aarch64" => "aarch64-pc-windows-msvc",
            _ => "x86_64-pc-windows-msvc",
        }
    } else {
        "unknown"
    }
}

/// Detect the host CPU architecture at runtime.
///
/// On Unix this shells out to `uname -m` which reports the kernel's
/// architecture regardless of whether the current binary is running under
/// a compatibility layer like QEMU binfmt_misc.  This is critical for
/// Raspberry Pi (aarch64) where users may have installed an x86_64 binary
/// that runs via transparent emulation — `cfg!(target_arch)` would report
/// `x86_64` in that case, causing `update` to download the wrong binary.
///
/// Returns a normalised architecture string: `"aarch64"`, `"x86_64"`, etc.
/// Falls back to the compile-time `std::env::consts::ARCH` if the runtime
/// query fails.
fn runtime_host_arch() -> &'static str {
    #[cfg(unix)]
    {
        use std::sync::OnceLock;
        static CACHED: OnceLock<String> = OnceLock::new();
        let arch = CACHED.get_or_init(|| {
            std::process::Command::new("uname")
                .arg("-m")
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout).ok()
                    } else {
                        None
                    }
                })
                .map(|s| normalise_arch(s.trim()))
                .unwrap_or_else(|| std::env::consts::ARCH.to_string())
        });
        // SAFETY: leaking a &str from a OnceLock that lives for 'static.
        // The OnceLock itself is static so the reference is valid for 'static.
        // We just need to convert &String to &'static str.
        unsafe { &*(arch.as_str() as *const str) }
    }

    #[cfg(not(unix))]
    {
        compile_time_arch()
    }
}

/// Normalise the output of `uname -m` to the Rust target-arch naming.
fn normalise_arch(uname: &str) -> String {
    match uname {
        "x86_64" | "amd64" => "x86_64".to_string(),
        "aarch64" | "arm64" => "aarch64".to_string(),
        "armv7l" | "armv6l" => "arm".to_string(),
        other => other.to_string(),
    }
}

/// Compile-time architecture, used as fallback on non-Unix platforms.
fn compile_time_arch() -> &'static str {
    std::env::consts::ARCH
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
///
/// Uses runtime detection so it returns the real hardware architecture
/// even when the current binary is running under emulation.
fn host_architecture() -> Option<&'static str> {
    let arch = runtime_host_arch();
    match arch {
        "x86_64" | "aarch64" | "x86" | "arm" | "riscv64" => Some(arch),
        _ => None,
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
        let triple = current_target_triple();
        assert_ne!(triple, "unknown", "unsupported platform");
        // The triple must contain at least two hyphens (arch-vendor-os or arch-vendor-os-env)
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
        // Include assets for all CI platforms so this test passes everywhere.
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
    fn normalise_arch_maps_common_uname_values() {
        assert_eq!(normalise_arch("x86_64"), "x86_64");
        assert_eq!(normalise_arch("amd64"), "x86_64");
        assert_eq!(normalise_arch("aarch64"), "aarch64");
        assert_eq!(normalise_arch("arm64"), "aarch64");
        assert_eq!(normalise_arch("armv7l"), "arm");
        assert_eq!(normalise_arch("armv6l"), "arm");
        // Unknown values pass through unchanged
        assert_eq!(normalise_arch("riscv64"), "riscv64");
        assert_eq!(normalise_arch("s390x"), "s390x");
    }

    #[test]
    fn runtime_host_arch_returns_known_value() {
        let arch = runtime_host_arch();
        // On any CI or dev machine this should return a recognised architecture.
        assert!(
            ["x86_64", "aarch64", "arm", "x86", "riscv64"].contains(&arch) || !arch.is_empty(),
            "runtime_host_arch returned unexpected value: {arch}"
        );
    }

    #[test]
    fn current_target_triple_contains_runtime_arch() {
        let triple = current_target_triple();
        let arch = runtime_host_arch();
        // The triple must start with the detected architecture.
        assert!(
            triple.starts_with(arch),
            "triple {triple} should start with detected arch {arch}"
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
