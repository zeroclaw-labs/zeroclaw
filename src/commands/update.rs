//! `zeroclaw update` — self-update pipeline with rollback.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::Path;

#[cfg(feature = "agent-runtime")]
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

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

fn prebuilt_channel_note_message() -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string("cli-update-prebuilt-channel-note")
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        "Pre-built updates use the lean default channel bundle. Build from source with `./install.sh --source --preset full`, `--features channels-full`, or a specific `channel-*` feature for Slack, Discord, and other non-default channels.".to_string()
    }
}

fn update_available_message(current: &str, latest: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string_with_args(
            "cli-update-available",
            &[("current", current), ("latest", latest)],
        )
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        format!("Update available: v{current} -> v{latest}")
    }
}

fn update_forcing_reinstall_message(current: &str, latest: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string_with_args(
            "cli-update-forcing-reinstall",
            &[("current", current), ("latest", latest)],
        )
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        format!("Forcing reinstall: v{current} -> v{latest}")
    }
}

fn install_dir_not_writable_message(dir: &str, error: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string_with_args(
            "cli-update-not-writable",
            &[("dir", dir), ("error", error)],
        )
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        format!(
            "install directory {dir} is not writable ({error}); re-run `zeroclaw update` with \
             elevated privileges (sudo on macOS/Linux, an Administrator console on Windows)"
        )
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
/// When `force` is set, install the target even if it is not newer than the
/// current version (reinstall, or downgrade/pin to a specific `--version`).
pub async fn run(target_version: Option<&str>, force: bool) -> Result<()> {
    // Phase 1: Preflight
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 1/6: Preflight checks..."
    );
    let update_info = check(target_version).await?;

    if !should_install(update_info.is_newer, force) {
        println!(
            "{}",
            update_already_current_message(&update_info.current_version)
        );
        return Ok(());
    }

    if update_info.is_newer {
        println!(
            "{}",
            update_available_message(&update_info.current_version, &update_info.latest_version)
        );
    } else {
        // --force on a version that is not newer: reinstall or downgrade/pin.
        println!(
            "{}",
            update_forcing_reinstall_message(
                &update_info.current_version,
                &update_info.latest_version
            )
        );
    }

    let download_url = update_info
        .download_url
        .context("no suitable binary found for this platform")?;

    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;

    // Fail fast before downloading if the install directory is not writable
    // (e.g. a system-wide install that needs sudo / an elevated console).
    ensure_install_dir_writable(&current_exe).await?;

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
            println!("{}", prebuilt_channel_note_message());
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

/// Decide whether to proceed with the install. A newer version always installs;
/// a non-newer one (same or older) installs only with `--force`, which enables
/// reinstalling the current version or downgrading/pinning to a specific
/// `--version`.
fn should_install(is_newer: bool, force: bool) -> bool {
    is_newer || force
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

    // Release assets are .tar.gz archives (universal) or .zip archives
    // (Windows) containing the `zeroclaw` (or `zeroclaw.exe`) binary.
    // Extract the binary from the archive instead of writing raw bytes.
    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        extract_tar_gz(&bytes, dest).context("failed to extract binary from tar.gz archive")?;
    } else if url.ends_with(".zip") {
        extract_zip(&bytes, dest).context("failed to extract binary from zip archive")?;
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

/// Extract the `zeroclaw.exe` binary from a `.zip` archive (Windows).
fn extract_zip(archive_bytes: &[u8], dest: &Path) -> Result<()> {
    use std::io::Read;

    let cursor = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("failed to open zip archive")?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("failed to read zip entry")?;
        let file_name = entry.name().rsplit(&['/', '\\']).next().unwrap_or("");
        if file_name == "zeroclaw.exe" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("failed to read binary from zip archive")?;
            std::fs::write(dest, &buf).context("failed to write extracted binary")?;
            return Ok(());
        }
    }

    bail!("zip archive does not contain a 'zeroclaw.exe' binary")
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
/// On Linux/FreeBSD this reads the ELF header, on macOS the Mach-O header, and
/// on Windows the PE/COFF header. If the binary is for a different architecture,
/// returns a descriptive error instead of the opaque "Exec format error
/// (os error 8)" (Unix) or its Windows equivalent.
async fn check_binary_arch(path: &Path) -> Result<()> {
    use tokio::io::AsyncReadExt;

    // Read only the header — enough to cover a PE file's DOS stub and reach the
    // COFF machine field pointed to by `e_lfanew` (well under 4 KiB in practice)
    // — instead of pulling the whole multi-megabyte binary into memory.
    let mut header = Vec::new();
    tokio::fs::File::open(path)
        .await
        .context("failed to open binary to read header")?
        .take(4096)
        .read_to_end(&mut header)
        .await
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

/// Detect the CPU architecture from an ELF, Mach-O, or PE binary header.
///
/// Returns `None` when the container format or its machine type is not
/// recognized, so callers treat "can't tell" as "skip the check" rather than a
/// mismatch (returning a placeholder string here would make a known host arch
/// compare unequal and falsely report an architecture mismatch).
fn detect_arch_from_header(header: &[u8]) -> Option<&'static str> {
    // ELF magic: 0x7f 'E' 'L' 'F'
    if header.len() >= 20 && header[0..4] == [0x7f, b'E', b'L', b'F'] {
        // e_machine is at offset 18 (2 bytes, little-endian for LE binaries)
        let e_machine = u16::from_le_bytes([header[18], header[19]]);
        return match e_machine {
            0x3E => Some("x86_64"),
            0xB7 => Some("aarch64"),
            0x03 => Some("x86"),
            0x28 => Some("arm"),
            0xF3 => Some("riscv"),
            _ => None,
        };
    }

    // Mach-O magic (64-bit little-endian): 0xFEEDFACF
    if header.len() >= 8 && header[0..4] == [0xCF, 0xFA, 0xED, 0xFE] {
        let cputype = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        return match cputype {
            0x0100_0007 => Some("x86_64"),
            0x0100_000C => Some("aarch64"),
            _ => None,
        };
    }

    // PE (Windows): "MZ" DOS header; the PE header offset is stored at 0x3C and
    // the COFF machine field follows the "PE\0\0" signature.
    if header.len() >= 0x40 && header[0] == b'M' && header[1] == b'Z' {
        let pe_off =
            u32::from_le_bytes([header[0x3C], header[0x3D], header[0x3E], header[0x3F]]) as usize;
        if let Some(coff) = pe_off
            .checked_add(6)
            .and_then(|end| header.get(pe_off..end))
        {
            if &coff[0..4] == b"PE\0\0" {
                let machine = u16::from_le_bytes([coff[4], coff[5]]);
                return match machine {
                    0x8664 => Some("x86_64"),
                    0xAA64 => Some("aarch64"),
                    0x014C => Some("x86"),
                    0x01C0 => Some("arm"),
                    _ => None,
                };
            }
        }
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

/// Verify the directory containing the executable is writable, so the update
/// fails fast with an actionable message instead of downloading a binary and
/// only then erroring during backup or swap. System-wide installs (e.g.
/// `/usr/local/bin`, `/usr/bin`, or `Program Files`) typically require elevated
/// privileges.
async fn ensure_install_dir_writable(exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .context("cannot determine install directory for the current executable")?;
    let probe = dir.join(format!(".zeroclaw-update-probe-{}", std::process::id()));
    match tokio::fs::File::create(&probe).await {
        Ok(_) => {
            let _ = tokio::fs::remove_file(&probe).await;
            Ok(())
        }
        Err(e) => bail!(install_dir_not_writable_message(
            &dir.display().to_string(),
            &e.to_string()
        )),
    }
}

/// Replace the running executable at `target` with the freshly downloaded
/// binary at `new`.
///
/// The mechanism differs by platform because each OS treats the file backing a
/// running process differently:
///
/// * Unix — the kernel keeps the inode alive while the process runs, so the old
///   path can be unlinked and the new binary copied into the now-free path
///   (this avoids `ETXTBSY`).
/// * Windows — the OS locks the image of a running process and refuses to delete
///   it, but it *does* allow renaming. So the running exe is moved aside to a
///   `.old` sidecar and the new binary is copied into the original path. The
///   sidecar usually cannot be removed until the old process exits, so its
///   deletion is best-effort and any leftover is swept on the next update run.
#[cfg(not(windows))]
async fn swap_binary(new: &Path, target: &Path) -> Result<()> {
    tokio::fs::remove_file(target)
        .await
        .context("failed to remove old binary")?;
    tokio::fs::copy(new, target)
        .await
        .context("failed to write new binary")?;
    Ok(())
}

#[cfg(windows)]
async fn swap_binary(new: &Path, target: &Path) -> Result<()> {
    // Move the running exe aside under a process-unique name. A fixed name could
    // collide with a sidecar left by an earlier update whose old process is
    // still running (and therefore still locking the file); the rename would
    // then have to delete that locked file and fail. A unique name sidesteps it.
    let sidelined = sidecar_path(target, "old");
    // Renaming a running executable is permitted on Windows even though deleting
    // it is not.
    tokio::fs::rename(target, &sidelined)
        .await
        .context("failed to move old binary aside")?;
    if let Err(e) = tokio::fs::copy(new, target).await {
        // Put the original back so the install is not left without a binary.
        let _ = tokio::fs::rename(&sidelined, target).await;
        return Err(e).context("failed to write new binary");
    }
    // Best-effort: the old image is still mapped by this process and usually
    // cannot be removed until it exits. Also sweep sidecars from earlier runs.
    let _ = tokio::fs::remove_file(&sidelined).await;
    sweep_stale_sidecars(target).await;
    Ok(())
}

#[cfg(not(windows))]
async fn rollback_binary(backup: &Path, target: &Path) -> Result<()> {
    // Remove-then-copy to avoid ETXTBSY if the target is somehow still mapped.
    let _ = tokio::fs::remove_file(target).await;
    tokio::fs::copy(backup, target)
        .await
        .context("failed to restore backup binary")?;
    Ok(())
}

#[cfg(windows)]
async fn rollback_binary(backup: &Path, target: &Path) -> Result<()> {
    // `target` may be the currently running image (which cannot be deleted but
    // can be renamed) or a stale, not-running new binary. Move whatever is there
    // aside under a process-unique name, then restore the backup into the
    // original path.
    let sidelined = sidecar_path(target, "rollback-old");
    let _ = tokio::fs::rename(target, &sidelined).await;
    tokio::fs::copy(backup, target)
        .await
        .context("failed to restore backup binary")?;
    let _ = tokio::fs::remove_file(&sidelined).await;
    Ok(())
}

/// Build a process-unique sidecar path next to `target`, e.g.
/// `zeroclaw.exe` -> `zeroclaw.exe.<pid>.old`.
#[cfg(windows)]
fn sidecar_path(target: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{suffix}", std::process::id()));
    target.with_file_name(name)
}

/// Best-effort removal of sidecars left by earlier updates whose old process had
/// not yet exited. Files still locked by a live process are silently skipped and
/// swept by a later run.
#[cfg(windows)]
async fn sweep_stale_sidecars(target: &Path) {
    let (Some(dir), Some(base)) = (target.parent(), target.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let prefix = format!("{base}.");
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(&prefix) && (name.ends_with(".old") || name.ends_with(".rollback-old"))
        {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
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
    fn should_install_requires_newer_or_force() {
        assert!(should_install(true, false)); // newer → install
        assert!(should_install(true, true)); // newer + force → install
        assert!(!should_install(false, false)); // not newer → skip
        assert!(should_install(false, true)); // not newer + force → reinstall/downgrade
    }

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
            "zeroclaw-x86_64-pc-windows-msvc.tar.gz",
            "zeroclaw-aarch64-pc-windows-msvc.tar.gz",
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

    fn make_pe_header(machine: u16) -> Vec<u8> {
        // "MZ" DOS header, e_lfanew at 0x3C pointing to a PE header at 0x40,
        // "PE\0\0" signature, then the COFF machine field.
        let mut header = vec![0u8; 0x48];
        header[0] = b'M';
        header[1] = b'Z';
        header[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        header[0x40..0x44].copy_from_slice(b"PE\0\0");
        header[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        header
    }

    #[test]
    fn detect_arch_pe_x86_64() {
        assert_eq!(
            detect_arch_from_header(&make_pe_header(0x8664)),
            Some("x86_64")
        );
    }

    #[test]
    fn detect_arch_pe_aarch64() {
        assert_eq!(
            detect_arch_from_header(&make_pe_header(0xAA64)),
            Some("aarch64")
        );
    }

    #[test]
    fn detect_arch_pe_unknown_machine_returns_none() {
        // A valid PE container with an unrecognized machine must yield None so
        // the caller skips the check instead of reporting a false mismatch.
        assert_eq!(detect_arch_from_header(&make_pe_header(0xFFFF)), None);
    }

    #[test]
    fn detect_arch_elf_unknown_machine_returns_none() {
        let mut header = vec![0u8; 20];
        header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        header[18] = 0xEE; // not a recognized e_machine
        header[19] = 0x00;
        assert_eq!(detect_arch_from_header(&header), None);
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

    /// Regression: #7509 — verify extract_zip writes the zeroclaw.exe
    /// binary bytes from a minimal Windows ZIP release asset.
    #[test]
    fn extract_zip_writes_zeroclaw_exe() {
        use std::io::Write;

        let fake_exe = b"fake zeroclaw windows binary content";
        let mut zip_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut zip_buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("zeroclaw.exe", options).unwrap();
            writer.write_all(fake_exe).unwrap();
            writer.finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_new");
        extract_zip(&zip_buf, &dest).unwrap();

        let content = std::fs::read(&dest).unwrap();
        assert_eq!(content, fake_exe);
    }

    #[test]
    fn extract_zip_finds_zeroclaw_exe_in_subdirectory() {
        use std::io::Write;

        // Windows archive tools sometimes produce paths like
        // `zeroclaw-v0.9/zeroclaw.exe`.  extract_zip matches by
        // basename, so subdirectory entries must be found.
        let fake_exe = b"zeroclaw-exe-in-subdir";
        let mut zip_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut zip_buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .start_file("zeroclaw-v0.9/zeroclaw.exe", options)
                .unwrap();
            writer.write_all(fake_exe).unwrap();
            writer.finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_new");
        extract_zip(&zip_buf, &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), fake_exe);
    }

    #[tokio::test]
    async fn ensure_install_dir_writable_accepts_writable_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("zeroclaw");
        ensure_install_dir_writable(&exe).await.unwrap();
    }

    #[tokio::test]
    async fn ensure_install_dir_writable_rejects_missing_dir() {
        let exe = Path::new("/no-such-zeroclaw-install-dir-9f1c/zeroclaw");
        let err = ensure_install_dir_writable(exe)
            .await
            .unwrap_err()
            .to_string();
        // Assert on the install-directory path, which the message interpolates in
        // every locale, rather than the (now localized) "not writable" wording.
        assert!(
            err.contains("no-such-zeroclaw-install-dir-9f1c"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn swap_binary_replaces_target_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("zeroclaw");
        let new = tmp.path().join("zeroclaw_new");
        std::fs::write(&target, b"old binary").unwrap();
        std::fs::write(&new, b"new binary").unwrap();

        swap_binary(&new, &target).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new binary");
    }

    #[tokio::test]
    async fn rollback_binary_restores_backup_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("zeroclaw");
        let backup = tmp.path().join("zeroclaw.bak");
        std::fs::write(&target, b"broken binary").unwrap();
        std::fs::write(&backup, b"good binary").unwrap();

        rollback_binary(&backup, &target).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"good binary");
    }

    #[test]
    fn extract_zip_errors_on_missing_exe() {
        use std::io::Write;

        let mut zip_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut zip_buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("README.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("zeroclaw_new");
        let result = extract_zip(&zip_buf, &dest);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("does not contain"),
            "should report missing zeroclaw.exe"
        );
    }
}
