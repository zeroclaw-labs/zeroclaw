//! `zeroclaw update` — self-update pipeline with rollback.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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

#[cfg(any(test, not(feature = "agent-runtime")))]
const PREBUILT_CHANNEL_NOTE_FALLBACK: &str = "Pre-built updates use the lean standard distribution. Build from source with `./install.sh --source --preset full`, `--features channels-full`, or a specific `channel-*` feature for Slack and other channels outside that distribution.";

fn prebuilt_channel_note_message() -> String {
    #[cfg(feature = "agent-runtime")]
    {
        get_required_cli_string("cli-update-prebuilt-channel-note")
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        PREBUILT_CHANNEL_NOTE_FALLBACK.to_string()
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
    /// GitHub release page URL (`html_url`), for "view release" links.
    pub release_url: Option<String>,
    /// Release notes body (Markdown), for rendering in the dashboard.
    pub release_notes: Option<String>,
    /// Release publish timestamp (ISO-8601), as returned by GitHub.
    pub published_at: Option<String>,
}

/// Check for available updates without downloading.
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

    let release_url = release["html_url"].as_str().map(str::to_string);
    let release_notes = release["body"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let published_at = release["published_at"].as_str().map(str::to_string);

    Ok(UpdateInfo {
        current_version: current,
        latest_version: tag,
        download_url,
        sha256sums_url,
        is_newer,
        release_url,
        release_notes,
        published_at,
    })
}

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
    let temp_dir = tempfile::Builder::new()
        .prefix(".zeroclaw-update-")
        .tempdir()
        .context("failed to create temp dir")?;
    let staging = temp_dir.path().join("staging");
    let main_binary = download_release(
        &download_url,
        update_info.sha256sums_url.as_deref(),
        &staging,
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
    validate_binary(&main_binary).await?;

    // Phase 5: Swap
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "Phase 5/6: Swapping binary..."
    );
    if let Err(e) = swap_binary(&main_binary, &current_exe).await {
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
            // Install everything else the archive shipped (the `zerocode`
            // companion, the `web/dist` dashboard bundle, …). Best-effort:
            // the validated main binary is already in place and must not be
            // rolled back if these fail.
            install_companion_artifacts(&staging, &current_exe).await;
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

/// Download a release asset and unpack it into `staging`.
///
/// `.tar.gz`/`.tgz` (universal) and `.zip` (Windows) archives are unpacked
/// **wholesale** into `staging` using each archive crate's own `unpack`/`extract`
/// — both reject `..` and absolute-root entries internally, so we do not need
/// a hand-written traversal guard. A non-archive URL is treated as a bare
/// binary and written through as `staging/zeroclaw`, preserving the legacy
/// single-file behavior for older release channels.
///
/// Returns the path to the freshly unpacked main `zeroclaw` (or `zeroclaw.exe`)
/// binary, which is the only artifact the caller needs by name — everything
/// else in the archive is installed later by walking `staging` generically.
async fn download_release(
    url: &str,
    sha256sums_url: Option<&str>,
    staging: &Path,
) -> Result<PathBuf> {
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_mins(5))
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

    tokio::fs::create_dir_all(staging)
        .await
        .context("failed to create staging directory")?;

    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        unpack_tar_gz(&bytes, staging).context("failed to extract tar.gz archive")?;
    } else if url.ends_with(".zip") {
        unpack_zip(&bytes, staging).context("failed to extract zip archive")?;
    } else {
        // Bare (non-archive) asset: write it through as the main binary.
        let binary = staging.join(main_binary_name());
        std::fs::write(&binary, &bytes).context("failed to write downloaded binary")?;
    }

    let binary = locate_main_binary(staging)?;

    // Make the extracted main binary executable on Unix. Other extracted
    // files keep whatever permissions the archive carried — `tar` preserves
    // them, and `zip::ZipArchive::extract` sets the Unix mode for us.
    #[cfg(unix)]
    {
        make_executable(&binary).await?;
    }

    Ok(binary)
}

/// Set the Unix executable bit (0o755) on a freshly extracted binary.
#[cfg(unix)]
async fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o755);
    tokio::fs::set_permissions(path, perms)
        .await
        .with_context(|| format!("failed to set executable bit on {}", path.display()))?;
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

fn main_binary_name() -> &'static str {
    if cfg!(windows) {
        "zeroclaw.exe"
    } else {
        "zeroclaw"
    }
}

/// Names of top-level *file* artifacts (not directories) the release archive is
/// allowed to install next to the running binary, beyond the main `zeroclaw`
/// executable itself. Anything else in the archive's top level is warned about
/// and skipped — symmetric with how unknown top-level *directories* are
/// handled, and defense-in-depth for the browser-triggered self-upgrade path:
/// a compromised or forged release cannot introduce an arbitrarily-named file
/// beside the running binary just by naming it in its own archive.
///
/// Grow this list — and its `.exe` twin on Windows — deliberately when a new
/// companion ships. The CI release-artifact list is the source of truth this
/// mirrors (currently: `zerocode` next to `zeroclaw`, plus the `web/dist`
/// directory that's handled by the whole-directory swap above).
#[cfg(windows)]
const KNOWN_COMPANION_FILES: &[&str] = &["zerocode.exe"];
#[cfg(not(windows))]
const KNOWN_COMPANION_FILES: &[&str] = &["zerocode"];

fn is_known_companion(name: &str) -> bool {
    KNOWN_COMPANION_FILES.contains(&name)
}

/// Wholesale-unpack a `.tar.gz` release archive into `staging`.
///
/// Delegates to `tar::Archive::unpack`, which:
/// * refuses absolute paths and any `..` segment outside the destination root,
///   so a malicious archive cannot escape `staging`;
/// * preserves the entry's recorded mode on Unix (the main binary keeps `0o755`).
///
/// Symlinks are also unpacked verbatim — every later `walk_files` over the
/// staged tree skips them, so a symlink-out-then-write-through trick cannot
/// reach a file outside `staging`.
fn unpack_tar_gz(archive_bytes: &[u8], staging: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);
    archive.set_preserve_permissions(true);
    archive
        .unpack(staging)
        .context("failed to unpack tar.gz archive")?;
    Ok(())
}

/// Wholesale-unpack a `.zip` release archive (Windows) into `staging`.
///
/// Delegates to `zip::ZipArchive::extract`, which uses `enclosed_name` per
/// entry — that rejects absolute paths and any `..` segment that would escape
/// the destination root.
fn unpack_zip(archive_bytes: &[u8], staging: &Path) -> Result<()> {
    let cursor = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("failed to open zip archive")?;
    archive
        .extract(staging)
        .context("failed to extract zip archive")?;
    Ok(())
}

/// Find the freshly unpacked main binary in `staging`.
///
/// Walks the staged tree looking for a `zeroclaw` (or `zeroclaw.exe`) file. The
/// release archive is flat — the binary sits at the staging root — but we walk
/// in case a future archive layout introduces a wrapper directory (e.g.
/// `zeroclaw-v0.9/zeroclaw.exe`, which Windows zip tooling sometimes produces).
fn locate_main_binary(staging: &Path) -> Result<PathBuf> {
    let target_name = main_binary_name();
    for entry in walk_files(staging) {
        if entry
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == target_name)
            .unwrap_or(false)
        {
            return Ok(entry);
        }
    }
    bail!("archive does not contain a '{target_name}' binary")
}

/// Collect all **regular file** paths under `root`, skipping directories and
/// symlinks. Used by `locate_main_binary` so it sees the same view of the
/// staged tree as `install_companion_artifacts`.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // file_type() does *not* follow symlinks, so a symlink shows up as
            // is_symlink() — we drop it rather than dereferencing it.
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                out.push(path);
            }
            // Symlinks and any other special files are silently skipped.
        }
    }
    out
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
            && &coff[0..4] == b"PE\0\0"
        {
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

/// Best-effort sweep of `.<base>.update-*` and `.<base>.update-old-*` residue
/// left in `parent` by an earlier interrupted or partially-locked update of
/// `base`. Used by `swap_file` and `install_web_dist`, which both name their
/// staging / sidelined entries with that pattern; matches both files (locked
/// sibling executables) and directories (a sidelined `web/dist` whose contents
/// were still open when the previous run tried to delete them).
///
/// Anything still locked just stays and gets swept by a later run.
async fn sweep_update_residue(parent: &Path, base: &str) {
    let prefix = format!(".{base}.update-");
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        match entry.file_type().await {
            Ok(ft) if ft.is_dir() => {
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
            Ok(_) => {
                let _ = tokio::fs::remove_file(&path).await;
            }
            Err(_) => {
                // Can't tell what it is; try file first, then dir.
                if tokio::fs::remove_file(&path).await.is_err() {
                    let _ = tokio::fs::remove_dir_all(&path).await;
                }
            }
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

/// Install every artifact in `staging` other than the main binary onto the
/// running install: the `web/dist` dashboard bundle and any other top-level
/// files (e.g. the `zerocode` companion).
///
/// Best-effort by design — the `zeroclaw` binary has already been swapped and
/// smoke-tested. A failure here (e.g. an unwritable data directory) is logged
/// and swallowed rather than failing or rolling back an otherwise-good update.
async fn install_companion_artifacts(staging: &Path, current_exe: &Path) {
    // 1. Dashboard bundle, if present: swap the whole `web/dist` directory so a
    //    stale file removed in a release is *gone*, not orphaned in place.
    let staged_web_dist = staging.join("web").join("dist");
    if staged_web_dist.is_dir() {
        match install_web_dist(&staged_web_dist, current_exe).await {
            Ok(target) => ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({"dir": target.display().to_string()})),
                "Updated web dashboard assets"
            ),
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                "Web dashboard assets not updated; the main update still succeeded"
            ),
        }
    }

    // 2. Every *other* top-level file in the archive that is on the explicit
    //    companion allowlist (see `KNOWN_COMPANION_FILES`), swapped into place
    //    next to the running binary. Unknown top-level files are warned and
    //    skipped — symmetric with how unknown top-level *directories* are
    //    handled below, and narrows the browser-triggered self-upgrade blast
    //    radius so a compromised or forged release cannot install an
    //    arbitrarily-named file beside the running binary.
    let Some(install_dir) = current_exe.parent() else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Cannot determine install directory; sibling files not refreshed"
        );
        return;
    };
    let main_name = main_binary_name();
    let staged_top = match std::fs::read_dir(staging) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in staged_top.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        // `web/` is handled above as a whole-directory swap. Any *other* top-
        // level directory in the archive is a layout the updater does not yet
        // know how to install (a new `themes/`, `plugins/`, …): warn loudly so
        // a future archive change does not silently fail to take effect, but
        // skip the entry rather than guessing where it should go.
        if ft.is_dir() {
            if name_str != "web" {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"name": name_str})),
                    "Release archive contains a top-level directory the updater \
                     doesn't know how to install — skipping. Teach \
                     install_companion_artifacts about it or remove it from \
                     the release packaging."
                );
            }
            continue;
        }
        // Symlinks are dropped — same posture as `walk_files`.
        if !ft.is_file() {
            continue;
        }
        if name_str == main_name {
            // Already swapped + smoke-tested via the transactional path.
            continue;
        }
        // Everything else must appear on `KNOWN_COMPANION_FILES` explicitly.
        // Bare-name check: mirrors the file-type gate above and matches how
        // `install_dir.join(&name)` composes the target path — no traversal
        // is possible because `read_dir(staging)` only yields staged names.
        if !is_known_companion(name_str) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"name": name_str})),
                "Release archive contains a top-level file that is not on the \
                 companion allowlist — skipping. Add it to KNOWN_COMPANION_FILES \
                 (and its `.exe` twin on Windows) if it should be installed \
                 next to the running binary."
            );
            continue;
        }
        let staged_path = entry.path();
        let target = install_dir.join(&name);
        match swap_file(&staged_path, &target).await {
            Ok(()) => ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "name": name_str,
                        "path": target.display().to_string()
                    })),
                "Updated sibling file from release archive"
            ),
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "name": name_str,
                        "error": format!("{e}")
                    })),
                "Sibling file not updated; the main update still succeeded"
            ),
        }
    }
}

/// Stage `new` into a process-unique temp sibling and parse the target
/// path into `(dir, base)` used by both platform variants of `swap_file`.
async fn stage_swap_file(new: &Path, target: &Path) -> Result<(PathBuf, String, PathBuf)> {
    let dir = target
        .parent()
        .context("cannot determine target directory")?
        .to_path_buf();
    let base = target
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid target file name")?
        .to_string();
    let tmp = dir.join(format!(".{base}.update-{}", std::process::id()));
    tokio::fs::copy(new, &tmp)
        .await
        .with_context(|| format!("failed to stage {base}"))?;
    Ok((dir, base, tmp))
}

/// Atomically replace `target` with `new` on Unix.
///
/// Copies `new` into a process-unique temp sibling, mirrors the source's
/// mode bits, then renames the temp over `target` (atomic — even if `target`
/// is a running executable, the kernel keeps the old inode alive for the
/// process while the new file takes the path).
///
/// On success, sweeps `.<base>.update-*` residue left by previous runs.
#[cfg(not(windows))]
async fn swap_file(new: &Path, target: &Path) -> Result<()> {
    let (dir, base, tmp) = stage_swap_file(new, target).await?;
    // Mirror the source's mode bits so an executable companion stays
    // executable. tar/zip both restore mode on extraction.
    if let Ok(src_meta) = tokio::fs::metadata(new).await {
        let _ = tokio::fs::set_permissions(&tmp, src_meta.permissions()).await;
    }
    if let Err(e) = tokio::fs::rename(&tmp, target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("failed to install {base} to {}", target.display()));
    }
    sweep_update_residue(&dir, &base).await;
    Ok(())
}

/// Atomically(-ish) replace `target` with `new` on Windows.
///
/// If the direct rename fails (the destination is locked because the file
/// is currently executing — e.g. the user has the `zerocode` TUI open), we
/// rename the existing `target` aside under a process-unique `.update-old`
/// name and then rename the staged file into its place — the same idiom
/// `swap_binary` uses for the main executable.
///
/// On success, sweeps `.<base>.update-*` residue left by previous runs
/// whose sidelined file was still locked at cleanup time.
#[cfg(windows)]
async fn swap_file(new: &Path, target: &Path) -> Result<()> {
    let (dir, base, tmp) = stage_swap_file(new, target).await?;
    if tokio::fs::rename(&tmp, target).await.is_ok() {
        sweep_update_residue(&dir, &base).await;
        return Ok(());
    }
    // rename failed — target is likely locked by a running process.
    // Move the old aside, then rename the staged file in.
    let sidelined = dir.join(format!(".{base}.update-old-{}", std::process::id()));
    match tokio::fs::rename(target, &sidelined).await {
        Ok(()) => {
            if let Err(e) = tokio::fs::rename(&tmp, target).await {
                let _ = tokio::fs::rename(&sidelined, target).await;
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e)
                    .with_context(|| format!("failed to install {base} to {}", target.display()));
            }
            // The old sidelined file is still mapped by the running process
            // and usually cannot be removed until it exits; the post-success
            // sweep picks up leftovers from previous runs.
            let _ = tokio::fs::remove_file(&sidelined).await;
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e).with_context(|| format!("failed to move old {base} aside"));
        }
    }
    sweep_update_residue(&dir, &base).await;
    Ok(())
}

/// Install the new `web/dist` dashboard bundle where the gateway will serve it
/// from. Uses a whole-directory swap (rename old aside, rename new into place,
/// restore on failure) so files removed in a release vanish from the served
/// tree instead of lingering as orphans. Returns the directory the assets were
/// installed into.
///
/// "In use" survival on both platforms:
/// * **Unix** — directories with open files inside are happily renamed; the
///   gateway keeps serving the open inodes, the next request picks up the new
///   tree.
/// * **Windows** — same idea (a directory rename succeeds even if files inside
///   have open handles), but `remove_dir_all` of the sidelined tree fails as
///   long as any file inside is held open. The leftover `.<name>.update-old-*`
///   directory is cleaned up by `sweep_update_residue` on the next update.
async fn install_web_dist(staged_dist: &Path, current_exe: &Path) -> Result<PathBuf> {
    let target = resolve_web_dist_target(current_exe);
    let parent = target
        .parent()
        .context("web dashboard target has no parent")?;
    tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let target_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dist");
    let sidelined = parent.join(format!(".{target_name}.update-old-{}", std::process::id()));
    let staging_dir = parent.join(format!(".{target_name}.update-{}", std::process::id()));

    // Move the freshly unpacked tree onto the target filesystem under a temp
    // name first. If `staged_dist` lives on a different filesystem than
    // `target` (it does — staging is in /tmp), `rename` would fail with
    // EXDEV; fall back to a recursive copy in that case.
    if let Err(rename_err) = tokio::fs::rename(staged_dist, &staging_dir).await {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"rename_err": rename_err.to_string()})),
            "rename into target filesystem failed, falling back to copy"
        );
        if let Err(e) = copy_dir_recursive(staged_dist, &staging_dir).await {
            // Copy may have produced a partial tree on the target filesystem
            // before failing (disk full, perms mid-copy, …); clean it up so
            // we don't leave an `.<name>.update-<pid>` orphan behind.
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            return Err(e).context("failed to stage new dashboard bundle on target filesystem");
        }
    }

    // If a previous version exists, move it aside first so the swap is
    // genuinely a *replace*, not an overlay. Missing target is fine — we just
    // skip straight to the rename.
    let had_existing = tokio::fs::metadata(&target).await.is_ok();
    if had_existing && let Err(e) = tokio::fs::rename(&target, &sidelined).await {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(e).with_context(|| format!("failed to move old {} aside", target.display()));
    }

    if let Err(e) = tokio::fs::rename(&staging_dir, &target).await {
        // Try to put the old tree back so the dashboard does not vanish.
        if had_existing {
            let _ = tokio::fs::rename(&sidelined, &target).await;
        }
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(e).context("failed to install new dashboard bundle");
    }

    // Best-effort: drop the old tree once the new one is in. Files inside it
    // may still be open (Windows gateway holding `index.html`, or any
    // platform's serve handler keeping an asset mapped) — those keep the
    // sidelined dir alive, and the post-success sweep collects it next run.
    if had_existing {
        let _ = tokio::fs::remove_dir_all(&sidelined).await;
    }
    // Pick up `.<target_name>.update-*` residue from earlier interrupted runs.
    sweep_update_residue(parent, target_name).await;
    Ok(target)
}

/// Decide where to install the refreshed `web/dist` bundle, mirroring the
/// gateway's dashboard auto-detection ([`crates/zeroclaw-gateway/src/lib.rs`])
/// and `install.sh`:
///   1. existing `web/dist` next to the running binary (dev / packaged), or
///   2. Docker layout `/zeroclaw-data/web/dist`, or
///   3. system package layout `/usr/share/zeroclawlabs/web/dist`, or
///   4. platform data dir `…/zeroclaw/web/dist` (prebuilt installer), or
///   5. **fallback**: `web/dist` next to the running binary, even if it does
///      not yet exist — write *somewhere* so a fresh install gets a dashboard.
///
/// The custom-config path (`gateway.web_dist_dir`) is intentionally ignored;
/// reading the config here would couple this command to gateway internals and
/// the operator can always re-run with `--force` after fixing the layout.
fn resolve_web_dist_target(current_exe: &Path) -> PathBuf {
    let binary_adjacent = current_exe.parent().map(|p| p.join("web").join("dist"));

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(ref p) = binary_adjacent {
        candidates.push(p.clone());
    }
    candidates.push(PathBuf::from("/zeroclaw-data/web/dist"));
    candidates.push(PathBuf::from("/usr/share/zeroclawlabs/web/dist"));
    if let Some(base) = directories::BaseDirs::new() {
        candidates.push(
            base.data_local_dir()
                .join("zeroclaw")
                .join("web")
                .join("dist"),
        );
    }
    for c in &candidates {
        if c.join("index.html").is_file() {
            return c.clone();
        }
    }
    // Fallback: write next to the binary even if nothing existed yet.
    binary_adjacent.unwrap_or_else(|| PathBuf::from("web/dist"))
}

/// Recursively copy `src` into `dst`. Used as the cross-filesystem fallback
/// for the dashboard directory swap (when staging and the install root live
/// on different filesystems and `rename` returns `EXDEV`).
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from_dir, to_dir)) = stack.pop() {
        tokio::fs::create_dir_all(&to_dir)
            .await
            .with_context(|| format!("failed to create {}", to_dir.display()))?;
        let mut entries = tokio::fs::read_dir(&from_dir)
            .await
            .with_context(|| format!("failed to read {}", from_dir.display()))?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let from = entry.path();
            let to = to_dir.join(entry.file_name());
            if file_type.is_dir() {
                stack.push((from, to));
            } else if file_type.is_file() {
                tokio::fs::copy(&from, &to)
                    .await
                    .with_context(|| format!("failed to copy {}", from.display()))?;
            }
            // Symlinks are dropped — same posture as `walk_files`.
        }
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
    fn prebuilt_channel_note_describes_the_lean_distribution() {
        assert!(PREBUILT_CHANNEL_NOTE_FALLBACK.contains("lean standard distribution"));
        assert!(PREBUILT_CHANNEL_NOTE_FALLBACK.contains("Slack"));
        assert!(!PREBUILT_CHANNEL_NOTE_FALLBACK.contains("lean default channel bundle"));
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
    async fn download_release_verifies_checksum_before_writing() {
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
        let staging = tmp.path().join("staging");
        let binary = download_release(
            &format!("{}/zeroclaw-test.bin", server.uri()),
            Some(&format!("{}/SHA256SUMS", server.uri())),
            &staging,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&binary).unwrap(), asset);
        // Bare (non-archive) download path — no siblings should appear.
        assert!(!staging.join("zerocode").exists());
        assert!(!staging.join("web").exists());
    }

    #[tokio::test]
    async fn download_release_rejects_checksum_mismatch_without_writing() {
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
        let staging = tmp.path().join("staging");
        let err = download_release(
            &format!("{}/zeroclaw-test.bin", server.uri()),
            Some(&format!("{}/SHA256SUMS", server.uri())),
            &staging,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("checksum mismatch"));
        assert!(!staging.join("zeroclaw").exists());
    }

    #[tokio::test]
    async fn download_release_preserves_missing_checksum_fallback() {
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
        let staging = tmp.path().join("staging");
        let binary = download_release(
            &format!("{}/zeroclaw-test.bin", server.uri()),
            None,
            &staging,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&binary).unwrap(), asset);
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

    /// Build a gzip-compressed tar archive from `(path, bytes)` entries.
    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            for (name, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, *bytes).unwrap();
            }
            builder.finish().unwrap();
        }

        let mut gz_buf = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::fast());
            encoder.write_all(&tar_buf).unwrap();
            encoder.finish().unwrap();
        }
        gz_buf
    }

    #[test]
    fn unpack_tar_gz_writes_main_binary() {
        let fake_binary = b"#!/bin/sh\necho zeroclaw";
        let gz_buf = make_tar_gz(&[("zeroclaw", fake_binary)]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        unpack_tar_gz(&gz_buf, &staging).unwrap();

        let binary = locate_main_binary(&staging).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), fake_binary);
        // Bare-archive case: nothing else should appear.
        assert!(!staging.join("web").exists());
    }

    #[test]
    fn unpack_tar_gz_extracts_full_tree() {
        let zeroclaw = b"#!/bin/sh\necho zeroclaw";
        let zerocode = b"#!/bin/sh\necho zerocode";
        let index = b"<!doctype html><title>dash</title>";
        let asset = b"console.log('app')";
        let gz_buf = make_tar_gz(&[
            ("zeroclaw", zeroclaw),
            ("zerocode", zerocode),
            ("web/dist/index.html", index),
            ("web/dist/assets/app.js", asset),
        ]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        unpack_tar_gz(&gz_buf, &staging).unwrap();

        let binary = locate_main_binary(&staging).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), zeroclaw);
        assert_eq!(std::fs::read(staging.join("zerocode")).unwrap(), zerocode);
        assert_eq!(
            std::fs::read(staging.join("web").join("dist").join("index.html")).unwrap(),
            index
        );
        assert_eq!(
            std::fs::read(staging.join("web").join("dist").join("assets/app.js")).unwrap(),
            asset,
            "nested web asset preserved"
        );
    }

    #[test]
    fn locate_main_binary_errors_on_missing_binary() {
        let gz_buf = make_tar_gz(&[("README.md", b"hello")]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        unpack_tar_gz(&gz_buf, &staging).unwrap();

        let err = locate_main_binary(&staging).unwrap_err().to_string();
        assert!(
            err.contains("does not contain"),
            "should report missing binary: {err}"
        );
    }

    /// Regression for the historical `extract_tar_gz` traversal guard: the
    /// tar pipeline must refuse `../` entries end-to-end. We assert this at
    /// the *encoder* end — `tar::Builder::append_data` rejects a non-Normal
    /// path component before it ever reaches the wire. That guarantee plus
    /// `tar::Archive::unpack`'s symmetric refusal means no malicious archive
    /// can be both produced and unpacked through this stack. (The zip path
    /// is exercised via a crafted archive in `unpack_zip_refuses_path_traversal`
    /// because zip's encoder does not reject the same input.)
    #[test]
    fn tar_pipeline_refuses_path_traversal_at_encoder() {
        use std::io::Write;
        let mut tar_buf = Vec::new();
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        let err = builder
            .append_data(&mut header, "../escape.txt", &b"evil"[..])
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        assert!(
            err.to_string().contains(".."),
            "tar encoder must refuse `..` entries: {err}"
        );
        // (Silence the unused-Write warning; the builder is consumed above.)
        let _ = &mut std::io::sink() as &mut dyn Write;
    }

    /// Build a ZIP archive from `(path, bytes)` entries.
    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;

        let mut zip_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut zip_buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, bytes) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        zip_buf
    }

    /// Regression: verify the zip unpacker writes the zeroclaw.exe
    /// binary bytes from a minimal Windows ZIP release asset.
    #[test]
    fn unpack_zip_writes_zeroclaw_exe() {
        let fake_exe = b"fake zeroclaw windows binary content";
        let zip_buf = make_zip(&[("zeroclaw.exe", fake_exe)]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        unpack_zip(&zip_buf, &staging).unwrap();

        // locate_main_binary searches by platform-specific name; assert by
        // staging path to keep the test cross-platform.
        assert_eq!(
            std::fs::read(staging.join("zeroclaw.exe")).unwrap(),
            fake_exe
        );
    }

    #[test]
    fn unpack_zip_finds_zeroclaw_exe_in_subdirectory() {
        // Windows archive tools sometimes produce paths like
        // `zeroclaw-v0.9/zeroclaw.exe`. `locate_main_binary` walks the tree, so
        // a nested binary is still found.
        let fake_exe = b"zeroclaw-exe-in-subdir";
        let zip_buf = make_zip(&[("zeroclaw-v0.9/zeroclaw.exe", fake_exe)]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        unpack_zip(&zip_buf, &staging).unwrap();

        // We don't go through locate_main_binary here because its platform
        // gate hides zeroclaw.exe on non-Windows hosts — assert the staged
        // path directly so the test runs everywhere CI does.
        assert_eq!(
            std::fs::read(staging.join("zeroclaw-v0.9/zeroclaw.exe")).unwrap(),
            fake_exe
        );
    }

    #[test]
    fn unpack_zip_extracts_full_tree() {
        let zeroclaw = b"fake zeroclaw.exe";
        let zerocode = b"fake zerocode.exe";
        let index = b"<!doctype html>";
        let zip_buf = make_zip(&[
            ("zeroclaw.exe", zeroclaw),
            ("zerocode.exe", zerocode),
            ("web/dist/index.html", index),
        ]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        unpack_zip(&zip_buf, &staging).unwrap();

        assert_eq!(
            std::fs::read(staging.join("zeroclaw.exe")).unwrap(),
            zeroclaw
        );
        assert_eq!(
            std::fs::read(staging.join("zerocode.exe")).unwrap(),
            zerocode
        );
        assert_eq!(
            std::fs::read(staging.join("web").join("dist").join("index.html")).unwrap(),
            index
        );
    }

    /// `zip::ZipArchive::extract` uses `enclosed_name` internally and refuses
    /// to write entries that would escape the destination root.
    #[test]
    fn unpack_zip_refuses_path_traversal() {
        let zip_buf = make_zip(&[("zeroclaw.exe", b"fake"), ("../escape.txt", b"evil")]);

        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let _ = unpack_zip(&zip_buf, &staging);
        assert!(!tmp.path().join("escape.txt").exists());
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
    fn resolve_web_dist_target_prefers_binary_adjacent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(bin_dir.join("web").join("dist")).unwrap();
        std::fs::write(bin_dir.join("web/dist/index.html"), b"<html>").unwrap();
        let exe = bin_dir.join("zeroclaw");

        let target = resolve_web_dist_target(&exe);
        assert_eq!(target, bin_dir.join("web").join("dist"));
    }

    #[test]
    fn resolve_web_dist_target_falls_back_to_binary_adjacent_when_no_candidate_exists() {
        // No candidate directory contains index.html, so the resolver returns
        // the binary-adjacent path as the "write somewhere" fallback.
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("bin").join("zeroclaw");
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();

        let target = resolve_web_dist_target(&exe);
        // Fallback either lands on the binary-adjacent dir or on the platform
        // data dir, depending on whether `/usr/share/zeroclawlabs/web/dist`
        // happens to exist on the runner. Both are acceptable; what matters is
        // it does not error.
        assert!(target.ends_with("web/dist"));
    }

    #[tokio::test]
    async fn install_web_dist_swaps_directory_dropping_removed_files() {
        // Verify the directory-swap semantics: a file present in the OLD
        // dashboard but absent from the NEW one must NOT survive the update.
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("zeroclaw");
        std::fs::write(&exe, b"zeroclaw").unwrap();

        // Existing (old) dashboard: contains a stale file the new release drops.
        let old_dist = bin_dir.join("web").join("dist");
        std::fs::create_dir_all(&old_dist).unwrap();
        std::fs::write(old_dist.join("index.html"), b"OLD INDEX").unwrap();
        std::fs::write(old_dist.join("removed-in-new.txt"), b"stale").unwrap();

        // Staged (new) dashboard: only the new index.
        let staged_dist = tmp.path().join("staging").join("web").join("dist");
        std::fs::create_dir_all(&staged_dist).unwrap();
        std::fs::write(staged_dist.join("index.html"), b"NEW INDEX").unwrap();
        std::fs::write(staged_dist.join("brand-new.txt"), b"fresh").unwrap();

        let installed = install_web_dist(&staged_dist, &exe).await.unwrap();
        // Installs adjacent because old_dist already contains index.html.
        assert_eq!(installed, old_dist);
        assert_eq!(
            std::fs::read(installed.join("index.html")).unwrap(),
            b"NEW INDEX"
        );
        assert_eq!(
            std::fs::read(installed.join("brand-new.txt")).unwrap(),
            b"fresh"
        );
        assert!(
            !installed.join("removed-in-new.txt").exists(),
            "file removed in the new release must not linger in the installed tree"
        );
    }

    #[tokio::test]
    async fn swap_file_replaces_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("zerocode");
        std::fs::write(&target, b"old zerocode").unwrap();
        let new = tmp.path().join("staging").join("zerocode");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, b"new zerocode").unwrap();

        swap_file(&new, &target).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new zerocode");
    }

    #[tokio::test]
    async fn install_companion_artifacts_swaps_top_level_siblings() {
        // The companion-artifact loop swaps every top-level file that is on
        // the explicit `KNOWN_COMPANION_FILES` allowlist (currently
        // `zerocode` / `zerocode.exe`) — and only those. The main binary is
        // handled earlier by the transactional `swap_binary` path; unknown
        // top-level files are covered by the sibling test below.
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("zeroclaw");
        std::fs::write(&exe, b"zeroclaw").unwrap();
        std::fs::write(bin_dir.join("zerocode"), b"old zerocode").unwrap();

        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        // Main binary lives in the staged tree but must NOT be re-swapped here
        // (it was already handled by the transactional `swap_binary` path).
        std::fs::write(staging.join("zeroclaw"), b"new zeroclaw").unwrap();
        std::fs::write(staging.join("zerocode"), b"new zerocode").unwrap();

        install_companion_artifacts(&staging, &exe).await;

        // zerocode swapped in.
        let expected_zerocode = bin_dir.join("zerocode");
        assert_eq!(std::fs::read(&expected_zerocode).unwrap(), b"new zerocode");
        // Main binary must be unchanged by the companion pass.
        assert_eq!(std::fs::read(&exe).unwrap(), b"zeroclaw");
    }

    /// An unknown top-level *file* in the archive (anything not on
    /// `KNOWN_COMPANION_FILES`) must be skipped — the updater warns rather
    /// than blindly installing it — so a compromised or forged release cannot
    /// use the browser-triggered self-upgrade path to introduce an
    /// arbitrarily-named file next to the running binary. This is symmetric
    /// with `install_companion_artifacts_skips_unknown_top_level_directories`.
    #[tokio::test]
    async fn install_companion_artifacts_skips_unknown_top_level_files() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("zeroclaw");
        std::fs::write(&exe, b"zeroclaw").unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        // Known companion — must be installed.
        std::fs::write(staging.join("zerocode"), b"new zerocode").unwrap();
        // Unknown top-level file — must NOT be installed. This is the
        // defense-in-depth surface: a forged release cannot smuggle a
        // `zerodash`, `.bashrc`, `evil.so`, etc. next to `zeroclaw` just by
        // naming it in its own archive.
        std::fs::write(staging.join("zerodash"), b"unknown artifact").unwrap();

        install_companion_artifacts(&staging, &exe).await;

        // Known companion installed.
        assert_eq!(
            std::fs::read(bin_dir.join("zerocode")).unwrap(),
            b"new zerocode"
        );
        // Unknown sibling NOT installed.
        assert!(
            !bin_dir.join("zerodash").exists(),
            "unknown top-level file must not be installed; got it under {}",
            bin_dir.display()
        );
    }

    /// An unknown top-level directory in the archive (anything other than
    /// `web/`) must be skipped — the updater warns rather than guessing where
    /// to install it — while sibling files and the `web/dist` swap continue
    /// to work normally.
    #[tokio::test]
    async fn install_companion_artifacts_skips_unknown_top_level_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("zeroclaw");
        std::fs::write(&exe, b"zeroclaw").unwrap();
        let installed_dist = bin_dir.join("web").join("dist");
        std::fs::create_dir_all(&installed_dist).unwrap();
        std::fs::write(installed_dist.join("index.html"), b"OLD INDEX").unwrap();

        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("zerocode"), b"new zerocode").unwrap();
        // An unknown directory (a future layout, a mispackaged release, …).
        std::fs::create_dir_all(staging.join("themes").join("dark")).unwrap();
        std::fs::write(staging.join("themes/dark/index.css"), b"body{}").unwrap();
        // And the known `web/dist` tree, to confirm the rest of the pipeline
        // still runs after the unknown-directory branch.
        std::fs::create_dir_all(staging.join("web").join("dist")).unwrap();
        std::fs::write(staging.join("web/dist/index.html"), b"NEW INDEX").unwrap();

        install_companion_artifacts(&staging, &exe).await;

        // Known artifacts installed.
        assert_eq!(
            std::fs::read(bin_dir.join("zerocode")).unwrap(),
            b"new zerocode"
        );
        assert_eq!(
            std::fs::read(bin_dir.join("web/dist/index.html")).unwrap(),
            b"NEW INDEX"
        );
        // Unknown directory NOT silently materialized next to the binary.
        assert!(
            !bin_dir.join("themes").exists(),
            "unknown top-level directory must not be installed; got it under {}",
            bin_dir.display()
        );
    }

    #[tokio::test]
    async fn sweep_update_residue_cleans_files_and_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        // Simulate residue left by previous PIDs.
        std::fs::write(parent.join(".zerocode.update-99998"), b"stale tmp").unwrap();
        std::fs::write(parent.join(".zerocode.update-old-99997"), b"stale old").unwrap();
        let stale_dir = parent.join(".dist.update-old-99996");
        std::fs::create_dir_all(stale_dir.join("assets")).unwrap();
        std::fs::write(stale_dir.join("index.html"), b"old").unwrap();
        std::fs::write(stale_dir.join("assets/app.js"), b"old").unwrap();
        // A file that does NOT match the prefix — must survive.
        std::fs::write(parent.join("zerocode"), b"live").unwrap();

        sweep_update_residue(parent, "zerocode").await;
        sweep_update_residue(parent, "dist").await;

        assert!(!parent.join(".zerocode.update-99998").exists());
        assert!(!parent.join(".zerocode.update-old-99997").exists());
        assert!(!stale_dir.exists());
        assert_eq!(std::fs::read(parent.join("zerocode")).unwrap(), b"live");
    }

    #[tokio::test]
    async fn swap_file_cleans_up_residue_from_earlier_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let target = dir.join("zerocode");
        std::fs::write(&target, b"old").unwrap();
        // Pretend a previous update left a sidelined file (e.g. because the
        // old process was still running and the file was locked).
        std::fs::write(dir.join(".zerocode.update-old-99999"), b"stale").unwrap();

        let new = dir.join("new-zerocode");
        std::fs::write(&new, b"new").unwrap();
        swap_file(&new, &target).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        // The stale sidelined file from the previous run must be swept.
        assert!(
            !dir.join(".zerocode.update-old-99999").exists(),
            "swap_file must sweep residue from previous runs"
        );
    }

    #[tokio::test]
    async fn install_web_dist_cleans_up_residue_from_earlier_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("zeroclaw");
        std::fs::write(&exe, b"zeroclaw").unwrap();

        // Existing dashboard.
        let old_dist = bin_dir.join("web").join("dist");
        std::fs::create_dir_all(&old_dist).unwrap();
        std::fs::write(old_dist.join("index.html"), b"OLD").unwrap();

        // Residue from a previous run (sidelined dir that couldn't be removed).
        let stale = bin_dir.join("web").join(".dist.update-old-99999");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("index.html"), b"ancient").unwrap();

        // Staged new dashboard.
        let staged_dist = tmp.path().join("staging").join("web").join("dist");
        std::fs::create_dir_all(&staged_dist).unwrap();
        std::fs::write(staged_dist.join("index.html"), b"NEW").unwrap();

        let installed = install_web_dist(&staged_dist, &exe).await.unwrap();

        assert_eq!(std::fs::read(installed.join("index.html")).unwrap(), b"NEW");
        // Previous-run sidelined directory must be swept.
        assert!(
            !stale.exists(),
            "install_web_dist must sweep residue from previous runs"
        );
    }
}
