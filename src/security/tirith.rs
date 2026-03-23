//! Tirith pre-exec security scanning.
//!
//! Scans commands for content-level threats (homograph URLs, pipe-to-shell,
//! terminal injection, etc.) by invoking the tirith binary as a subprocess.
//!
//! Exit code is the verdict source of truth:
//!   0 = allow, 1 = block, 2 = warn
//!
//! Auto-install: if tirith is not found, it is downloaded from GitHub releases
//! to ~/.zeroclaw/bin/tirith with SHA-256 checksum verification.
//!
//! Already integrated in Hermes Agent (NousResearch/hermes-agent#1256) and
//! EurekaClaw (EurekaClaw/EurekaClaw#1). This is the ZeroClaw adaptation.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

pub use crate::config::schema::TirithScanConfig;

/// Shell kind tells the guard which tokenizer tirith should use.
#[derive(Clone, Copy, Debug)]
pub enum ShellKind {
    /// Native runtime: posix on Unix, cmd on Windows (matches native.rs).
    Native,
    /// Always POSIX (cron scheduler uses `sh -c` on all platforms).
    Posix,
}

/// Scan a command with tirith before execution.
///
/// Call **after** policy checks (allowlist, forbidden paths, risk classification)
/// which are cheap and deterministic. Tirith adds content-level scanning.
///
/// Returns `Ok(())` if allowed, `Err(message)` if blocked.
pub async fn guard(
    command: &str,
    shell_kind: ShellKind,
    config: &TirithScanConfig,
) -> Result<(), String> {
    if !config.enabled {
        return Ok(());
    }

    let shell_flag = match shell_kind {
        ShellKind::Posix => "posix",
        ShellKind::Native => {
            if cfg!(windows) {
                "cmd"
            } else {
                "posix"
            }
        }
    };

    let bin = resolve_bin(&config.bin);

    let result = tokio::time::timeout(
        Duration::from_secs(config.timeout_secs),
        Command::new(&bin)
            .args([
                "check",
                "--json",
                "--non-interactive",
                "--shell",
                shell_flag,
                "--",
                command,
            ])
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => match output.status.code() {
            Some(0) => Ok(()),
            Some(1) => {
                let summary = summarize_findings(&output.stdout);
                Err(format!("blocked by tirith security scan: {summary}"))
            }
            Some(2) => {
                let summary = summarize_findings(&output.stdout);
                tracing::warn!("tirith security warning: {summary}");
                Ok(())
            }
            _ if config.fail_open => {
                tracing::debug!("tirith: unexpected exit code (fail-open)");
                Ok(())
            }
            _ => Err("tirith: unexpected exit code (fail-closed)".into()),
        },
        Ok(Err(e)) if config.fail_open => {
            tracing::debug!("tirith unavailable (fail-open): {e}");
            Ok(())
        }
        Ok(Err(e)) => Err(format!("tirith failed (fail-closed): {e}")),
        Err(_) if config.fail_open => {
            tracing::debug!("tirith timed out (fail-open)");
            Ok(())
        }
        Err(_) => Err("tirith timed out (fail-closed)".into()),
    }
}

fn summarize_findings(stdout: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Output {
        #[serde(default)]
        findings: Vec<Finding>,
    }
    #[derive(Deserialize)]
    struct Finding {
        #[serde(default)]
        severity: String,
        #[serde(default)]
        title: String,
    }

    let Ok(data) = serde_json::from_slice::<Output>(stdout) else {
        return "security issue detected (details unavailable)".to_string();
    };

    if data.findings.is_empty() {
        return "security issue detected".to_string();
    }

    data.findings
        .iter()
        .filter(|f| !f.title.is_empty())
        .map(|f| format!("[{}] {}", f.severity, f.title))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Resolve the tirith binary path. Checks:
/// 1. PATH (via `which`)
/// 2. ~/.zeroclaw/bin/tirith (previously auto-installed)
/// 3. Auto-install from GitHub releases
fn resolve_bin(configured: &str) -> String {
    static RESOLVED: OnceLock<String> = OnceLock::new();

    RESOLVED
        .get_or_init(|| {
            // Explicit path
            if configured != "tirith" {
                return configured.to_string();
            }

            // Check PATH
            if which::which("tirith").is_ok() {
                return "tirith".to_string();
            }

            // Check ~/.zeroclaw/bin/tirith
            let local = zeroclaw_bin_dir().join(bin_name());
            if local.is_file() {
                return local.to_string_lossy().to_string();
            }

            // Auto-install
            match auto_install() {
                Ok(path) => {
                    tracing::info!("tirith installed to {path} (SHA-256 verified)");
                    path
                }
                Err(e) => {
                    tracing::debug!("tirith auto-install failed: {e}");
                    configured.to_string()
                }
            }
        })
        .clone()
}

fn bin_name() -> &'static str {
    if cfg!(windows) {
        "tirith.exe"
    } else {
        "tirith"
    }
}

fn zeroclaw_bin_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".zeroclaw").join("bin")
}

/// Download and install tirith from GitHub releases with SHA-256 verification.
fn auto_install() -> Result<String, String> {
    let (target, ext) = detect_target()?;
    let archive_name = format!("tirith-{target}{ext}");
    let base_url = format!(
        "https://github.com/sheeki03/tirith/releases/latest/download"
    );

    let tmpdir = tempfile::tempdir().map_err(|e| format!("tmpdir: {e}"))?;

    let archive_path = tmpdir.path().join(&archive_name);
    let checksums_path = tmpdir.path().join("checksums.txt");

    tracing::info!("tirith not found — downloading for {target}...");

    download_file(
        &format!("{base_url}/{archive_name}"),
        &archive_path,
    )?;
    download_file(
        &format!("{base_url}/checksums.txt"),
        &checksums_path,
    )?;

    // Verify SHA-256
    verify_checksum(&archive_path, &checksums_path, &archive_name)?;

    // Extract binary
    let extracted = if ext == ".zip" {
        extract_zip(&archive_path, tmpdir.path(), bin_name())?
    } else {
        extract_tar_gz(&archive_path, tmpdir.path(), bin_name())?
    };

    let dest_dir = zeroclaw_bin_dir();
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir: {e}"))?;
    let dest = dest_dir.join(bin_name());
    std::fs::copy(&extracted, &dest).map_err(|e| format!("copy: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }

    Ok(dest.to_string_lossy().to_string())
}

fn detect_target() -> Result<(String, &'static str), String> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err("unsupported arch".into());
    };

    let (plat, ext) = if cfg!(target_os = "macos") {
        ("apple-darwin", ".tar.gz")
    } else if cfg!(target_os = "linux") {
        ("unknown-linux-gnu", ".tar.gz")
    } else if cfg!(target_os = "windows") {
        ("pc-windows-msvc", ".zip")
    } else {
        return Err("unsupported os".into());
    };

    Ok((format!("{arch}-{plat}"), ext))
}

fn download_file(
    url: &str,
    dest: &std::path::Path,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("download {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("download {url}: {e}"))?;

    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    std::io::copy(&mut resp, &mut file)
        .map_err(|e| format!("write {}: {e}", dest.display()))?;
    Ok(())
}

fn verify_checksum(
    archive: &std::path::Path,
    checksums: &std::path::Path,
    name: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let checksums_text =
        std::fs::read_to_string(checksums).map_err(|e| format!("read checksums: {e}"))?;

    let expected = checksums_text
        .lines()
        .find_map(|line| {
            let (hash, fname) = line.split_once("  ")?;
            if fname == name {
                Some(hash.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| format!("no checksum for {name}"))?;

    let data = std::fs::read(archive).map_err(|e| format!("read archive: {e}"))?;
    let actual = hex::encode(Sha256::digest(&data));

    if actual != expected {
        return Err(format!("checksum mismatch: expected {expected}, got {actual}"));
    }
    Ok(())
}

fn extract_tar_gz(
    archive: &std::path::Path,
    dest_dir: &std::path::Path,
    bin_name: &str,
) -> Result<PathBuf, String> {
    // Use system tar (available on all Darwin/Linux where .tar.gz is used)
    let status = std::process::Command::new("tar")
        .args(["xzf", &archive.to_string_lossy(), "-C", &dest_dir.to_string_lossy()])
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err("tar extraction failed".into());
    }

    // Find the binary in extracted files
    fn find_bin(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_bin(&path, name) {
                    return Some(found);
                }
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Some(path);
            }
        }
        None
    }

    find_bin(dest_dir, bin_name).ok_or_else(|| "binary not found in archive".into())
}

fn extract_zip(
    archive: &std::path::Path,
    dest_dir: &std::path::Path,
    bin_name: &str,
) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("open: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
        let name = std::path::Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if name == bin_name {
            let dest = dest_dir.join(bin_name);
            let mut out = std::fs::File::create(&dest).map_err(|e| format!("create: {e}"))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| format!("copy: {e}"))?;
            return Ok(dest);
        }
    }
    Err("binary not found in archive".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_empty() {
        let s = summarize_findings(b"{}");
        assert_eq!(s, "security issue detected");
    }

    #[test]
    fn test_summarize_with_findings() {
        let json = br#"{"findings":[{"severity":"HIGH","title":"Pipe to shell"}]}"#;
        let s = summarize_findings(json);
        assert!(s.contains("Pipe to shell"));
        assert!(s.contains("HIGH"));
    }

    #[test]
    fn test_summarize_invalid_json() {
        let s = summarize_findings(b"not json");
        assert!(s.contains("details unavailable"));
    }

    #[tokio::test]
    async fn test_disabled_allows() {
        let cfg = TirithScanConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(guard("anything", ShellKind::Posix, &cfg).await.is_ok());
    }

    #[tokio::test]
    async fn test_missing_binary_fail_open() {
        let cfg = TirithScanConfig {
            enabled: true,
            bin: "/nonexistent/tirith".to_string(),
            timeout_secs: 1,
            fail_open: true,
        };
        assert!(guard("echo hello", ShellKind::Posix, &cfg).await.is_ok());
    }

    #[tokio::test]
    async fn test_missing_binary_fail_closed() {
        let cfg = TirithScanConfig {
            enabled: true,
            bin: "/nonexistent/tirith".to_string(),
            timeout_secs: 1,
            fail_open: false,
        };
        assert!(guard("echo hello", ShellKind::Posix, &cfg).await.is_err());
    }
}
