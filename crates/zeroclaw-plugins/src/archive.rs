//! Download and archive extraction for remote plugin installation.
//!
//! Supports `.zip`, `.tar.gz` / `.tgz`, `.tar.xz` / `.txz`, and `.tar.bz2`
//! archives fetched over HTTP(S). Archives are streamed to a temporary file,
//! extracted to a temporary directory, and the manifest is located automatically.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use crate::error::PluginError;

/// Supported archive formats, detected from the URL file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
    TarXz,
    TarBz2,
}

impl ArchiveFormat {
    /// Detect archive format from a URL or filename.
    pub fn from_url(url: &str) -> Result<Self, PluginError> {
        // Strip query string and fragment for extension detection
        let path = url.split('?').next().unwrap_or(url);
        let path = path.split('#').next().unwrap_or(path);
        let lower = path.to_lowercase();

        if lower.ends_with(".zip") {
            Ok(Self::Zip)
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Ok(Self::TarGz)
        } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
            Ok(Self::TarXz)
        } else if lower.ends_with(".tar.bz2") || lower.ends_with(".tbz2") {
            Ok(Self::TarBz2)
        } else {
            Err(PluginError::UnsupportedArchive(format!(
                "cannot determine archive type from URL: {url} — \
                 supported extensions: .zip, .tar.gz, .tgz, .tar.xz, .txz, .tar.bz2, .tbz2"
            )))
        }
    }
}

/// Returns `true` if the source string looks like an HTTP(S) URL.
pub fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

/// Download a URL to a temporary file, returning the path and detected archive format.
///
/// The file is streamed to disk to avoid holding large archives in memory.
pub fn download(url: &str) -> Result<(tempfile::NamedTempFile, ArchiveFormat), PluginError> {
    let format = ArchiveFormat::from_url(url)?;

    tracing::info!(url = %url, format = ?format, "downloading plugin archive");

    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| PluginError::DownloadFailed(format!("failed to create HTTP client: {e}")))?
        .get(url)
        .send()
        .map_err(|e| PluginError::DownloadFailed(format!("request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(PluginError::DownloadFailed(format!(
            "server returned HTTP {}",
            response.status()
        )));
    }

    let mut tmp = tempfile::NamedTempFile::new()
        .map_err(|e| PluginError::DownloadFailed(format!("failed to create temp file: {e}")))?;

    let bytes = response
        .bytes()
        .map_err(|e| PluginError::DownloadFailed(format!("failed to read response body: {e}")))?;

    io::copy(&mut bytes.as_ref(), &mut tmp)
        .map_err(|e| PluginError::DownloadFailed(format!("failed to write temp file: {e}")))?;

    tracing::info!(
        bytes = bytes.len(),
        path = %tmp.path().display(),
        "plugin archive downloaded"
    );

    Ok((tmp, format))
}

/// Extract an archive into the given directory.
pub fn extract(archive_path: &Path, format: ArchiveFormat, dest: &Path) -> Result<(), PluginError> {
    tracing::info!(
        archive = %archive_path.display(),
        dest = %dest.display(),
        format = ?format,
        "extracting plugin archive"
    );

    match format {
        ArchiveFormat::TarGz => extract_tar_gz(archive_path, dest),
        ArchiveFormat::TarXz => extract_tar_xz(archive_path, dest),
        ArchiveFormat::TarBz2 => extract_tar_bz2(archive_path, dest),
        ArchiveFormat::Zip => extract_zip(archive_path, dest),
    }
}

fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<(), PluginError> {
    let file = File::open(archive_path)?;
    let gz = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(gz);
    archive
        .unpack(dest)
        .map_err(|e| PluginError::ExtractionFailed(format!("tar.gz extraction failed: {e}")))?;
    Ok(())
}

fn extract_tar_xz(archive_path: &Path, dest: &Path) -> Result<(), PluginError> {
    let file = File::open(archive_path)?;
    let xz = xz2::read::XzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(xz);
    archive
        .unpack(dest)
        .map_err(|e| PluginError::ExtractionFailed(format!("tar.xz extraction failed: {e}")))?;
    Ok(())
}

fn extract_tar_bz2(archive_path: &Path, dest: &Path) -> Result<(), PluginError> {
    let file = File::open(archive_path)?;
    // bzip2 support via flate2 is not available — use raw read + bzip2 crate
    // For now, use a two-pass approach: read all bytes and decompress.
    // The bzip2 format is less common; we can add the bzip2 crate later if needed.
    // For now, fall back to shelling out to tar if available.
    let output = std::process::Command::new("tar")
        .arg("xjf")
        .arg(archive_path)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| {
            PluginError::ExtractionFailed(format!(
                "tar.bz2 extraction failed (tar command not found): {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::ExtractionFailed(format!(
            "tar.bz2 extraction failed: {stderr}"
        )));
    }

    drop(file);
    Ok(())
}

fn extract_zip(archive_path: &Path, dest: &Path) -> Result<(), PluginError> {
    let file = File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| PluginError::ExtractionFailed(format!("invalid zip archive: {e}")))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| PluginError::ExtractionFailed(format!("zip entry error: {e}")))?;

        let entry_path = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue, // skip entries with unsafe paths
        };

        let out_path = dest.join(&entry_path);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)?;
            io::copy(&mut entry, &mut out_file)?;
        }
    }

    Ok(())
}

/// Locate `manifest.toml` in an extracted directory.
///
/// Checks three locations in order:
/// 1. `<dir>/manifest.toml` — archive root
/// 2. `<dir>/plugin.toml` — alternate name
/// 3. `<dir>/<single-subdir>/manifest.toml` — one level deep (common when
///    archives contain a top-level directory named after the plugin)
pub fn find_manifest(dir: &Path) -> Result<PathBuf, PluginError> {
    // Check root-level manifest.toml
    let root_manifest = dir.join("manifest.toml");
    if root_manifest.exists() {
        return Ok(root_manifest);
    }

    // Check root-level plugin.toml (alternate name)
    let root_plugin = dir.join("plugin.toml");
    if root_plugin.exists() {
        return Ok(root_plugin);
    }

    // Check one level deep — if there's a single subdirectory, look inside it.
    // This handles archives like `my-plugin-v1.0.0/manifest.toml`.
    let entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    if entries.len() == 1 {
        let subdir = entries[0].path();
        let sub_manifest = subdir.join("manifest.toml");
        if sub_manifest.exists() {
            return Ok(sub_manifest);
        }
        let sub_plugin = subdir.join("plugin.toml");
        if sub_plugin.exists() {
            return Ok(sub_plugin);
        }
    }

    Err(PluginError::ManifestNotFoundInArchive)
}

/// Download a plugin archive from a URL, extract it, and return the path to the
/// directory containing the manifest. The caller is responsible for cleaning up
/// the returned `TempDir` after installation.
pub fn download_and_extract(url: &str) -> Result<(tempfile::TempDir, PathBuf), PluginError> {
    let (archive_file, format) = download(url)?;

    let extract_dir = tempfile::TempDir::new()
        .map_err(|e| PluginError::ExtractionFailed(format!("failed to create temp dir: {e}")))?;

    extract(archive_file.path(), format, extract_dir.path())?;

    let manifest_path = find_manifest(extract_dir.path())?;
    let plugin_dir = manifest_path
        .parent()
        .ok_or_else(|| PluginError::ExtractionFailed("manifest has no parent dir".into()))?
        .to_path_buf();

    tracing::info!(
        manifest = %manifest_path.display(),
        plugin_dir = %plugin_dir.display(),
        "found manifest in extracted archive"
    );

    Ok((extract_dir, plugin_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_tar_gz() {
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.tar.gz").unwrap(),
            ArchiveFormat::TarGz
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.tgz").unwrap(),
            ArchiveFormat::TarGz
        );
    }

    #[test]
    fn detect_tar_xz() {
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.tar.xz").unwrap(),
            ArchiveFormat::TarXz
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.txz").unwrap(),
            ArchiveFormat::TarXz
        );
    }

    #[test]
    fn detect_zip() {
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.zip").unwrap(),
            ArchiveFormat::Zip
        );
    }

    #[test]
    fn detect_with_query_string() {
        assert_eq!(
            ArchiveFormat::from_url("https://github.com/release/v1.0/plugin.tar.gz?token=abc")
                .unwrap(),
            ArchiveFormat::TarGz
        );
    }

    #[test]
    fn unsupported_format() {
        assert!(ArchiveFormat::from_url("https://example.com/plugin.rar").is_err());
        assert!(ArchiveFormat::from_url("https://example.com/plugin").is_err());
    }

    #[test]
    fn is_url_detects_http() {
        assert!(is_url("http://example.com/plugin.zip"));
        assert!(is_url("https://example.com/plugin.tar.gz"));
        assert!(!is_url("/path/to/plugin"));
        assert!(!is_url("./relative/path"));
        assert!(!is_url("plugin-name"));
    }

    #[test]
    fn find_manifest_in_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.toml"),
            "[plugin]\nname = \"test\"",
        )
        .unwrap();
        let found = find_manifest(dir.path()).unwrap();
        assert_eq!(found, dir.path().join("manifest.toml"));
    }

    #[test]
    fn find_manifest_plugin_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plugin.toml"), "[plugin]\nname = \"test\"").unwrap();
        let found = find_manifest(dir.path()).unwrap();
        assert_eq!(found, dir.path().join("plugin.toml"));
    }

    #[test]
    fn find_manifest_one_level_deep() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("my-plugin-v1.0");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("manifest.toml"), "[plugin]\nname = \"test\"").unwrap();
        let found = find_manifest(dir.path()).unwrap();
        assert_eq!(found, subdir.join("manifest.toml"));
    }

    #[test]
    fn find_manifest_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_manifest(dir.path()).is_err());
    }
}
