use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Verify the SHA256 checksum of a downloaded artifact against the SHA256SUMS file.
pub async fn verify_checksum(
    checksum_url: &str,
    artifact_path: &Path,
    artifact_name: &str,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let sums_text = client
        .get(checksum_url)
        .send()
        .await?
        .error_for_status()
        .context("Failed to download SHA256SUMS")?
        .text()
        .await?;

    // Parse SHA256SUMS (format: "<hash>  <filename>")
    let expected_hash = sums_text
        .lines()
        .find_map(|line| {
            let parts: Vec<&str> = line.splitn(2, "  ").collect();
            if parts.len() == 2 && parts[1].trim() == artifact_name {
                Some(parts[0].to_lowercase())
            } else {
                None
            }
        })
        .with_context(|| {
            format!("Artifact '{artifact_name}' not found in SHA256SUMS")
        })?;

    // Compute actual hash
    let file_bytes = std::fs::read(artifact_path)
        .context("Failed to read downloaded artifact for checksum")?;
    let actual_hash = format!("{:x}", Sha256::digest(&file_bytes));

    if actual_hash != expected_hash {
        bail!(
            "SHA256 checksum mismatch!\n  Expected: {expected_hash}\n  Actual:   {actual_hash}\n\
             The downloaded file may be corrupted or tampered with."
        );
    }

    Ok(())
}

/// Verify cosign signature if cosign is available on PATH.
/// This is best-effort: warns if cosign is not installed, aborts if verification fails.
pub async fn verify_cosign(asset_url: &str, artifact_path: &Path) {
    let cosign_available = which_cosign();

    if !cosign_available {
        eprintln!("⚠️  cosign not found — skipping signature verification (checksum OK)");
        return;
    }

    eprint!("Verifying cosign signature... ");

    let sig_url = format!("{asset_url}.sig");
    let pem_url = format!("{asset_url}.pem");

    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => {
            eprintln!("⚠️  Failed to create HTTP client for cosign verification");
            return;
        }
    };

    // Download sig and pem files
    let artifact_dir = artifact_path.parent().unwrap_or(Path::new("."));
    let sig_path = artifact_dir.join(format!(
        "{}.sig",
        artifact_path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let pem_path = artifact_dir.join(format!(
        "{}.pem",
        artifact_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    if let Err(e) = download_file(&client, &sig_url, &sig_path).await {
        eprintln!("⚠️  Failed to download signature file: {e}");
        return;
    }
    if let Err(e) = download_file(&client, &pem_url, &pem_path).await {
        eprintln!("⚠️  Failed to download certificate file: {e}");
        return;
    }

    let result = std::process::Command::new("cosign")
        .args([
            "verify-blob",
            "--signature",
            &sig_path.to_string_lossy(),
            "--certificate",
            &pem_path.to_string_lossy(),
            "--certificate-identity-regexp",
            ".*",
            "--certificate-oidc-issuer",
            "https://token.actions.githubusercontent.com",
            &artifact_path.to_string_lossy(),
        ])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("✅");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("❌");
            eprintln!("Cosign verification failed: {stderr}");
            eprintln!("Aborting update for safety. If you trust this release, use --force.");
        }
        Err(e) => {
            eprintln!("⚠️  Failed to run cosign: {e}");
        }
    }

    // Clean up sig/pem files
    let _ = std::fs::remove_file(&sig_path);
    let _ = std::fs::remove_file(&pem_path);
}

/// Check if cosign is available on PATH.
fn which_cosign() -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg("cosign")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("which")
            .arg("cosign")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

async fn download_file(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    let bytes = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    #[test]
    fn checksum_parsing_finds_correct_hash() {
        let sums = "abc123def456  zeroclaw-x86_64-unknown-linux-gnu.tar.gz\n\
                     789abcdef012  zeroclaw-aarch64-apple-darwin.tar.gz\n";
        let artifact = "zeroclaw-x86_64-unknown-linux-gnu.tar.gz";
        let found = sums.lines().find_map(|line| {
            let parts: Vec<&str> = line.splitn(2, "  ").collect();
            if parts.len() == 2 && parts[1].trim() == artifact {
                Some(parts[0].to_lowercase())
            } else {
                None
            }
        });
        assert_eq!(found, Some("abc123def456".to_string()));
    }

    #[test]
    fn sha256_hash_computes_correctly() {
        let data = b"hello world";
        let hash = format!("{:x}", Sha256::digest(data));
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
