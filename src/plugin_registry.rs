use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use zeroclaw::plugins::PluginManifest;
pub(crate) use zeroclaw::plugins::registry::search_entries;
use zeroclaw::plugins::registry::{
    PluginRegistryEntry, PluginRegistryIndex, parse_plugin_spec, resolve_entry,
    write_cached_registry_index,
};

pub(crate) const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw-plugins/main/registry.json";
pub(crate) const MAX_PLUGIN_ZIP_BYTES: usize = 50 * 1024 * 1024;
pub(crate) const MAX_PLUGIN_EXTRACTED_BYTES: u64 = 50 * 1024 * 1024;
const REGISTRY_URL_ENV: &str = "ZEROCLAW_PLUGIN_REGISTRY_URL";

pub(crate) struct DownloadedPlugin {
    _temp_dir: TempDir,
    plugin_dir: PathBuf,
    manifest: PluginManifest,
}

impl DownloadedPlugin {
    pub(crate) fn plugin_dir(&self) -> &Path {
        &self.plugin_dir
    }

    pub(crate) fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
}

pub(crate) fn registry_url(override_url: Option<&str>) -> String {
    override_url
        .filter(|url| !url.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var(REGISTRY_URL_ENV).ok())
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string())
}

pub(crate) fn is_local_plugin_source(source: &str) -> bool {
    let path = Path::new(source);
    path.exists()
        || source.starts_with('.')
        || source.starts_with('~')
        || source.contains('/')
        || source.contains('\\')
}

pub(crate) fn looks_like_url(source: &str) -> bool {
    source.contains("://")
}

pub(crate) async fn fetch_registry_index(registry_url: &str) -> Result<PluginRegistryIndex> {
    let response = reqwest::get(registry_url)
        .await
        .with_context(|| format!("fetching plugin registry {registry_url}"))?;
    let status = response.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::NOT_FOUND && registry_url == DEFAULT_REGISTRY_URL {
            bail!(
                "the public plugin registry is not populated yet; use --registry <url> to point at a custom registry"
            );
        }
        bail!("plugin registry returned HTTP {status} for {registry_url}");
    }
    response
        .json::<PluginRegistryIndex>()
        .await
        .context("parsing plugin registry JSON")
}

pub(crate) async fn download_registry_plugin(
    registry_url: &str,
    source: &str,
    cache_data_dir: Option<&Path>,
) -> Result<DownloadedPlugin> {
    let index = fetch_registry_index(registry_url).await?;
    if let Some(data_dir) = cache_data_dir {
        write_cached_registry_index(data_dir, registry_url, &index)?;
    }
    let spec = parse_plugin_spec(source)?;
    let entry = resolve_entry(&index, &spec)?.clone();
    let bytes = download_archive_bytes(&entry.url).await?;
    verify_sha256_if_present(&bytes, entry.sha256.as_deref())?;

    let temp_dir = tempfile::tempdir().context("creating temporary plugin extraction directory")?;
    let extract_dir = temp_dir.path().join("plugin");
    extract_zip_safe(std::io::Cursor::new(bytes), &extract_dir)?;
    let plugin_dir = find_manifest_dir(&extract_dir)?;
    let manifest = load_plugin_manifest(&plugin_dir)?;
    verify_manifest_matches_registry(&entry, &manifest)?;

    Ok(DownloadedPlugin {
        _temp_dir: temp_dir,
        plugin_dir,
        manifest,
    })
}

async fn download_archive_bytes(url: &str) -> Result<Vec<u8>> {
    let mut response = reqwest::get(url)
        .await
        .with_context(|| format!("downloading plugin archive {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("plugin archive returned HTTP {status} for {url}");
    }
    if let Some(len) = response.content_length()
        && len > MAX_PLUGIN_ZIP_BYTES as u64
    {
        bail!("plugin archive exceeds maximum size of {MAX_PLUGIN_ZIP_BYTES} bytes");
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading plugin archive response body")?
    {
        append_chunk_capped(&mut bytes, &chunk, MAX_PLUGIN_ZIP_BYTES)?;
    }
    Ok(bytes)
}

pub(crate) fn collect_capped_chunks<I>(chunks: I, max_bytes: usize) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = Result<Vec<u8>>>,
{
    let mut bytes = Vec::new();
    for chunk in chunks {
        let chunk = chunk?;
        append_chunk_capped(&mut bytes, &chunk, max_bytes)?;
    }
    Ok(bytes)
}

fn append_chunk_capped(bytes: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<()> {
    if bytes.len().saturating_add(chunk.len()) > max_bytes {
        bail!("plugin archive exceeds maximum size of {max_bytes} bytes");
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

fn verify_sha256_if_present(bytes: &[u8], expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let expected = expected.strip_prefix("sha256:").unwrap_or(expected);
    let actual = hex::encode(Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("plugin archive sha256 mismatch");
    }
    Ok(())
}

pub(crate) fn extract_zip_safe<R>(reader: R, dest: &Path) -> Result<PathBuf>
where
    R: Read + Seek,
{
    extract_zip_safe_with_limit(reader, dest, MAX_PLUGIN_EXTRACTED_BYTES)
}

fn extract_zip_safe_with_limit<R>(
    reader: R,
    dest: &Path,
    max_extracted_bytes: u64,
) -> Result<PathBuf>
where
    R: Read + Seek,
{
    let mut archive = zip::ZipArchive::new(reader)?;
    std::fs::create_dir_all(dest)?;
    let mut extracted_bytes = 0_u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let enclosed = enclosed_zip_path(file.name(), &file)?;
        let out_path = dest.join(enclosed);
        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        let Some(parent) = out_path.parent() else {
            bail!("plugin archive entry has no parent: {}", file.name());
        };
        if extracted_bytes.saturating_add(file.size()) > max_extracted_bytes {
            bail!("plugin archive exceeds extracted size limit of {max_extracted_bytes} bytes");
        }
        std::fs::create_dir_all(parent)?;
        let mut out = File::create(&out_path)?;
        copy_zip_entry_capped(
            &mut file,
            &mut out,
            &mut extracted_bytes,
            max_extracted_bytes,
        )?;
    }
    Ok(dest.to_path_buf())
}

fn enclosed_zip_path<R>(raw_name: &str, file: &zip::read::ZipFile<'_, R>) -> Result<PathBuf>
where
    R: Read,
{
    if is_unsafe_zip_entry_name(raw_name) {
        bail!("plugin archive contains unsafe path: {raw_name}");
    }
    file.enclosed_name().ok_or_else(|| {
        anyhow::Error::msg(format!("plugin archive contains unsafe path: {raw_name}"))
    })
}

fn is_unsafe_zip_entry_name(raw_name: &str) -> bool {
    raw_name.starts_with('/')
        || raw_name.starts_with('\\')
        || has_windows_drive_prefix(raw_name)
        || raw_name
            .split(['/', '\\'])
            .any(|component| component == "..")
}

fn has_windows_drive_prefix(raw_name: &str) -> bool {
    let bytes = raw_name.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

fn find_manifest_dir(root: &Path) -> Result<PathBuf> {
    let mut matches = Vec::new();
    if root.join("manifest.toml").is_file() {
        matches.push(root.to_path_buf());
    }
    collect_manifest_dirs(root, &mut matches)?;
    match matches.as_slice() {
        [dir] => Ok(dir.clone()),
        [] => bail!("plugin archive does not contain manifest.toml"),
        _ => bail!("plugin archive contains multiple manifest.toml files"),
    }
}

fn collect_manifest_dirs(dir: &Path, matches: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.join("manifest.toml").is_file() {
                matches.push(path.clone());
            }
            collect_manifest_dirs(&path, matches)?;
        }
    }
    Ok(())
}

fn copy_zip_entry_capped<R, W>(
    reader: &mut R,
    writer: &mut W,
    extracted_bytes: &mut u64,
    max_extracted_bytes: u64,
) -> Result<()>
where
    R: Read,
    W: Write,
{
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        if extracted_bytes.saturating_add(read as u64) > max_extracted_bytes {
            bail!("plugin archive exceeds extracted size limit of {max_extracted_bytes} bytes");
        }
        writer.write_all(&buffer[..read])?;
        *extracted_bytes += read as u64;
    }
}

fn load_plugin_manifest(plugin_dir: &Path) -> Result<PluginManifest> {
    let manifest_path = plugin_dir.join("manifest.toml");
    let manifest_toml = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    toml::from_str(&manifest_toml).with_context(|| format!("parsing {}", manifest_path.display()))
}

fn verify_manifest_matches_registry(
    entry: &PluginRegistryEntry,
    manifest: &PluginManifest,
) -> Result<()> {
    if manifest.name != entry.name {
        bail!(
            "plugin archive manifest name '{}' does not match registry name '{}'",
            manifest.name,
            entry.name
        );
    }
    if manifest.version != entry.version {
        bail!(
            "plugin archive manifest version '{}' does not match registry version '{}'",
            manifest.version,
            entry.version
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::{Cursor, Write};
    use std::rc::Rc;
    use zip::write::SimpleFileOptions;

    struct CountingChunks {
        chunks: Vec<Vec<u8>>,
        next: usize,
        pulls: Rc<Cell<usize>>,
    }

    impl Iterator for CountingChunks {
        type Item = Result<Vec<u8>>;

        fn next(&mut self) -> Option<Self::Item> {
            let chunk = self.chunks.get(self.next)?.clone();
            self.next += 1;
            self.pulls.set(self.next);
            Some(Ok(chunk))
        }
    }

    #[test]
    fn capped_chunk_collection_stops_before_buffering_unknown_length_archive() {
        let pulls = Rc::new(Cell::new(0));
        let chunks = CountingChunks {
            chunks: vec![vec![1; 4], vec![2; 4], vec![3; 4]],
            next: 0,
            pulls: Rc::clone(&pulls),
        };

        let err = collect_capped_chunks(chunks, 6).expect_err("oversized archive must fail");

        assert!(
            err.to_string().contains("maximum size"),
            "unexpected error: {err}"
        );
        assert_eq!(
            pulls.get(),
            2,
            "reader should stop as soon as the accumulated body exceeds the cap"
        );
    }

    #[test]
    fn safe_zip_extraction_rejects_paths_that_can_escape_destination() {
        for entry_name in ["../manifest.toml", "/manifest.toml", "C:/tmp/manifest.toml"] {
            let zip = zip_with_entry(entry_name, b"not a plugin");
            let dest = tempfile::tempdir().unwrap();

            assert!(
                extract_zip_safe(Cursor::new(zip), dest.path()).is_err(),
                "{entry_name} should be rejected before writing"
            );
        }
    }

    #[test]
    fn safe_zip_extraction_accepts_nested_relative_paths() {
        let zip = zip_with_entry("sample/manifest.toml", b"name = \"sample\"");
        let dest = tempfile::tempdir().unwrap();

        extract_zip_safe(Cursor::new(zip), dest.path()).unwrap();

        assert!(dest.path().join("sample/manifest.toml").is_file());
    }

    #[test]
    fn safe_zip_extraction_rejects_root_plus_nested_manifests() {
        let zip = zip_with_entries(&[
            (
                "manifest.toml",
                br#"name = "root"
version = "0.1.0"
capabilities = ["tool"]
"#,
            ),
            (
                "nested/manifest.toml",
                br#"name = "nested"
version = "0.1.0"
capabilities = ["tool"]
"#,
            ),
        ]);
        let dest = tempfile::tempdir().unwrap();

        extract_zip_safe(Cursor::new(zip), dest.path()).unwrap();

        assert!(find_manifest_dir(dest.path()).is_err());
    }

    #[test]
    fn safe_zip_extraction_rejects_excessive_uncompressed_size() {
        let zip = zip_with_entry("sample/manifest.toml", b"12345678");
        let dest = tempfile::tempdir().unwrap();

        assert!(extract_zip_safe_with_limit(Cursor::new(zip), dest.path(), 6).is_err());
    }

    #[test]
    fn verifies_optional_sha256_digest() {
        let bytes = b"plugin archive";
        let digest = hex::encode(Sha256::digest(bytes));

        verify_sha256_if_present(bytes, Some(&digest)).unwrap();
        verify_sha256_if_present(bytes, Some(&format!("sha256:{digest}"))).unwrap();
        assert!(verify_sha256_if_present(bytes, Some("00")).is_err());
    }

    #[test]
    fn rejects_registry_entry_manifest_identity_mismatch() {
        let entry = PluginRegistryEntry {
            name: "team-calendar".to_string(),
            version: "0.2.0".to_string(),
            description: None,
            author: None,
            capabilities: Vec::new(),
            url: "https://example.invalid/team-calendar.zip".to_string(),
            sha256: None,
        };
        let manifest = PluginManifest {
            name: "other-plugin".to_string(),
            version: "0.2.0".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            capabilities: vec![zeroclaw::plugins::PluginCapability::Tool],
            permissions: Vec::new(),
            signature: None,
            publisher_key: None,
        };

        assert!(verify_manifest_matches_registry(&entry, &manifest).is_err());
    }

    #[test]
    fn finds_manifest_at_root_or_single_nested_plugin_dir() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("manifest.toml"), "").unwrap();
        assert_eq!(find_manifest_dir(root.path()).unwrap(), root.path());

        let nested_root = tempfile::tempdir().unwrap();
        let nested = nested_root.path().join("plugin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("manifest.toml"), "").unwrap();
        assert_eq!(find_manifest_dir(nested_root.path()).unwrap(), nested);
    }

    fn zip_with_entry(name: &str, body: &[u8]) -> Vec<u8> {
        zip_with_entries(&[(name, body)])
    }

    fn zip_with_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut bytes);
            for (name, body) in entries {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(body).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes.into_inner()
    }
}
