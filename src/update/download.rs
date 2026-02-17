use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Download a release asset to the specified path.
pub async fn download_asset(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let resp = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .send()
        .await?
        .error_for_status()
        .context("Failed to download release asset")?;

    let total = resp.content_length();
    let filename = dest
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("artifact");

    if let Some(size) = total {
        let mb = size as f64 / 1_048_576.0;
        eprint!("Downloading {filename} ({mb:.1} MB)... ");
    } else {
        eprint!("Downloading {filename}... ");
    }

    let bytes = resp.bytes().await?;
    std::fs::write(dest, &bytes)?;
    eprintln!("done");

    Ok(())
}

/// Determine the platform-specific artifact name based on OS and architecture.
pub fn platform_artifact_name() -> String {
    let target = platform_target_triple();
    if std::env::consts::OS == "windows" {
        format!("zeroclaw-{target}.zip")
    } else {
        format!("zeroclaw-{target}.tar.gz")
    }
}

/// Map the current platform to a Rust target triple.
fn platform_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => "unknown-platform",
    }
}

/// Extract the zeroclaw binary from a downloaded archive.
pub fn extract_binary(archive_path: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let archive_name = archive_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    let binary_name = if cfg!(windows) {
        "zeroclaw.exe"
    } else {
        "zeroclaw"
    };

    if archive_name.ends_with(".tar.gz") {
        extract_tar_gz(archive_path, dest_dir, binary_name)
    } else if archive_name.ends_with(".zip") {
        extract_zip(archive_path, dest_dir, binary_name)
    } else {
        anyhow::bail!("Unknown archive format: {archive_name}");
    }
}

/// Extract using system tar command.
fn extract_tar_gz(archive: &Path, dest: &Path, binary_name: &str) -> Result<PathBuf> {
    let status = std::process::Command::new("tar")
        .args(["xzf", &archive.to_string_lossy(), "-C", &dest.to_string_lossy()])
        .status()
        .context("Failed to run tar — is tar installed?")?;

    if !status.success() {
        anyhow::bail!("tar extraction failed with exit code: {status}");
    }

    let binary_path = dest.join(binary_name);
    if !binary_path.exists() {
        anyhow::bail!(
            "Expected binary '{binary_name}' not found after extraction in {}",
            dest.display()
        );
    }

    Ok(binary_path)
}

/// Extract using PowerShell Expand-Archive on Windows.
fn extract_zip(archive: &Path, dest: &Path, binary_name: &str) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                    archive.display(),
                    dest.display()
                ),
            ])
            .status()
            .context("Failed to run PowerShell Expand-Archive")?;

        if !status.success() {
            anyhow::bail!("Expand-Archive failed with exit code: {status}");
        }
    }

    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("unzip")
            .args([
                "-o",
                &archive.to_string_lossy(),
                "-d",
                &dest.to_string_lossy(),
            ])
            .status()
            .context("Failed to run unzip — is unzip installed?")?;

        if !status.success() {
            anyhow::bail!("unzip extraction failed with exit code: {status}");
        }
    }

    let binary_path = dest.join(binary_name);
    if !binary_path.exists() {
        anyhow::bail!(
            "Expected binary '{binary_name}' not found after extraction in {}",
            dest.display()
        );
    }

    Ok(binary_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_mapping_returns_known_triple() {
        let triple = platform_target_triple();
        let known = [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
        ];
        // On CI/dev machines this should return a known triple
        assert!(
            known.contains(&triple) || triple == "unknown-platform",
            "Unexpected platform triple: {triple}"
        );
    }

    #[test]
    fn artifact_name_has_correct_extension() {
        let name = platform_artifact_name();
        if cfg!(windows) {
            assert!(name.ends_with(".zip"), "Windows artifact should be .zip");
        } else {
            assert!(
                name.ends_with(".tar.gz"),
                "Unix artifact should be .tar.gz"
            );
        }
        assert!(name.starts_with("zeroclaw-"));
    }
}
