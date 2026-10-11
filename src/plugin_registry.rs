use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
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
/// How long a registry request may spend connecting. An unreachable or
/// black-holed host fails here instead of hanging the command.
const REGISTRY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a registry response may deliver nothing: from the request until
/// its headers arrive, then between reads of its body. A stalled server fails
/// here, while a slow download that keeps delivering data does not.
const REGISTRY_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-request bound on fetching the registry index, body included.
const REGISTRY_INDEX_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-request ceiling on downloading one plugin archive, body included.
/// Stalls end at [`REGISTRY_READ_TIMEOUT`], so this only bounds a download
/// that keeps trickling in: it gives a link of about 30 KB/s the time to fetch
/// an archive at the size cap.
const REGISTRY_ARCHIVE_TIMEOUT: Duration = Duration::from_mins(30);

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
    RegistryClient::new(RegistryTimeouts::default())?
        .fetch_index(registry_url)
        .await
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
    let entry = resolve_entry(&index, &spec)?;
    download_registry_entry(entry).await
}

/// Download one resolved registry entry and unpack it for admission.
///
/// Covers everything after entry resolution: the archive fetch, the digest
/// check when the entry carries one, confined extraction, manifest discovery,
/// and the check that the archive holds the package the entry names. The
/// result is only a candidate; the caller still admits it through the plugin
/// host before anything is installed.
pub(crate) async fn download_registry_entry(
    entry: &PluginRegistryEntry,
) -> Result<DownloadedPlugin> {
    RegistryClient::new(RegistryTimeouts::default())?
        .download_entry(entry)
        .await
}

/// Request bounds for registry traffic.
///
/// `Default` is the production policy. Tests inject shorter bounds so a
/// stalled server fails in milliseconds rather than after the real bound.
#[derive(Clone, Copy, Debug)]
struct RegistryTimeouts {
    connect: Duration,
    /// The longest silence on any request; see [`REGISTRY_READ_TIMEOUT`].
    read: Duration,
    index: Duration,
    archive: Duration,
}

impl Default for RegistryTimeouts {
    fn default() -> Self {
        Self {
            connect: REGISTRY_CONNECT_TIMEOUT,
            read: REGISTRY_READ_TIMEOUT,
            index: REGISTRY_INDEX_TIMEOUT,
            archive: REGISTRY_ARCHIVE_TIMEOUT,
        }
    }
}

/// Every registry request goes through this client, so none of them can run
/// unbounded.
struct RegistryClient {
    http: reqwest::Client,
    timeouts: RegistryTimeouts,
}

impl RegistryClient {
    fn new(timeouts: RegistryTimeouts) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .read_timeout(timeouts.read)
            .build()
            .context("building the plugin registry HTTP client")?;
        Ok(Self { http, timeouts })
    }

    async fn fetch_index(&self, registry_url: &str) -> Result<PluginRegistryIndex> {
        let response = self
            .http
            .get(registry_url)
            .timeout(self.timeouts.index)
            .send()
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

    async fn download_entry(&self, entry: &PluginRegistryEntry) -> Result<DownloadedPlugin> {
        let bytes = self.download_archive_bytes(&entry.url).await?;
        verify_sha256_if_present(&bytes, entry.sha256.as_deref())?;

        let temp_dir =
            tempfile::tempdir().context("creating temporary plugin extraction directory")?;
        let extract_dir = temp_dir.path().join("plugin");
        extract_zip_safe(std::io::Cursor::new(bytes), &extract_dir)?;
        let plugin_dir = find_manifest_dir(&extract_dir)?;
        let manifest = load_plugin_manifest(&plugin_dir)?;
        verify_manifest_matches_registry(entry, &manifest)?;

        Ok(DownloadedPlugin {
            _temp_dir: temp_dir,
            plugin_dir,
            manifest,
        })
    }

    async fn download_archive_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let mut response = self
            .http
            .get(url)
            .timeout(self.timeouts.archive)
            .send()
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
}

#[cfg(test)]
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zip::write::SimpleFileOptions;

    /// Injected bound for the stalled-request tests.
    const TEST_TIMEOUT: Duration = Duration::from_millis(200);
    /// Far past `TEST_TIMEOUT`, so only the client's own bound can end the
    /// request before the server answers.
    const STALLED_RESPONSE_DELAY: Duration = Duration::from_secs(10);
    const SAMPLE_ARCHIVE_PATH: &str = "/sample-0.1.0.zip";
    const SAMPLE_MANIFEST: &[u8] = br#"name = "sample"
version = "0.1.0"
capabilities = ["tool"]
"#;

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
            wasm_sha256: None,
            capabilities: vec![zeroclaw::plugins::PluginCapability::Tool],
            provides: None,
            permissions: Vec::new(),
            config_schema: None,
            signature: None,
            publisher_key: None,
            egress: zeroclaw::plugins::PluginEgressDeclaration::default(),
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

    #[tokio::test]
    async fn registry_index_parses_a_successful_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/registry.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "plugins": [{
                    "name": "sample",
                    "version": "0.1.0",
                    "url": "https://example.invalid/sample.zip",
                }],
            })))
            .mount(&server)
            .await;

        let index = fetch_registry_index(&format!("{}/registry.json", server.uri()))
            .await
            .unwrap();

        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].name, "sample");
        assert_eq!(index.plugins[0].version, "0.1.0");
    }

    #[tokio::test]
    async fn registry_index_not_found_on_a_custom_registry_reports_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/registry.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let registry_url = format!("{}/registry.json", server.uri());

        let err = fetch_registry_index(&registry_url)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("HTTP 404"), "unexpected error: {err}");
        assert!(err.contains(&registry_url), "unexpected error: {err}");
        assert!(
            !err.contains("not populated"),
            "only the default registry gets the unpopulated hint: {err}"
        );
    }

    #[tokio::test]
    async fn registry_index_server_error_reports_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/registry.json"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let registry_url = format!("{}/registry.json", server.uri());

        let err = fetch_registry_index(&registry_url)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("HTTP 500"), "unexpected error: {err}");
        assert!(err.contains(&registry_url), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn registry_entry_download_unpacks_the_named_package() {
        let server = MockServer::start().await;
        let archive = sample_archive();
        let digest = hex::encode(Sha256::digest(&archive));
        serve_sample_archive(&server, ResponseTemplate::new(200).set_body_bytes(archive)).await;

        let downloaded =
            download_registry_entry(&sample_entry(&server, Some(format!("sha256:{digest}"))))
                .await
                .unwrap();

        assert_eq!(downloaded.manifest().name, "sample");
        assert_eq!(downloaded.manifest().version, "0.1.0");
        assert!(downloaded.plugin_dir().join("manifest.toml").is_file());
    }

    #[tokio::test]
    async fn registry_entry_download_refuses_a_digest_mismatch() {
        let server = MockServer::start().await;
        serve_sample_archive(
            &server,
            ResponseTemplate::new(200).set_body_bytes(sample_archive()),
        )
        .await;

        let Err(err) = download_registry_entry(&sample_entry(&server, Some("0".repeat(64)))).await
        else {
            panic!("an archive that does not match the registry digest must be refused");
        };

        assert!(
            err.to_string().contains("sha256 mismatch"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn registry_plugin_download_resolves_caches_and_unpacks_the_entry() {
        let server = MockServer::start().await;
        let archive = sample_archive();
        let entry = sample_entry(&server, Some(hex::encode(Sha256::digest(&archive))));
        Mock::given(method("GET"))
            .and(path("/registry.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "plugins": [entry] })),
            )
            .mount(&server)
            .await;
        serve_sample_archive(&server, ResponseTemplate::new(200).set_body_bytes(archive)).await;
        let data_dir = tempfile::tempdir().unwrap();
        let registry_url = format!("{}/registry.json", server.uri());

        let downloaded =
            download_registry_plugin(&registry_url, "sample@0.1.0", Some(data_dir.path()))
                .await
                .unwrap();

        assert_eq!(downloaded.manifest().name, "sample");
        let cached = zeroclaw::plugins::registry::read_cached_registry_index(data_dir.path())
            .unwrap()
            .expect("the fetched index is cached");
        assert_eq!(cached.registry_url.as_deref(), Some(registry_url.as_str()));
        assert_eq!(cached.plugins, vec![entry]);
    }

    #[tokio::test]
    async fn a_stalled_registry_index_fails_at_the_index_bound() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/registry.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "plugins": [] }))
                    .set_delay(STALLED_RESPONSE_DELAY),
            )
            .mount(&server)
            .await;
        let client = RegistryClient::new(RegistryTimeouts {
            index: TEST_TIMEOUT,
            ..RegistryTimeouts::default()
        })
        .unwrap();

        let err = client
            .fetch_index(&format!("{}/registry.json", server.uri()))
            .await
            .unwrap_err();

        assert!(is_timeout(&err), "expected a timeout, got {err:#}");
    }

    #[tokio::test]
    async fn a_stalled_registry_index_fails_at_the_read_bound() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/registry.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "plugins": [] }))
                    .set_delay(STALLED_RESPONSE_DELAY),
            )
            .mount(&server)
            .await;
        // The index bound stays at its production value, so only the read
        // bound can end the request before the server answers.
        let client = RegistryClient::new(RegistryTimeouts {
            read: TEST_TIMEOUT,
            ..RegistryTimeouts::default()
        })
        .unwrap();

        let err = client
            .fetch_index(&format!("{}/registry.json", server.uri()))
            .await
            .unwrap_err();

        assert!(is_timeout(&err), "expected a timeout, got {err:#}");
    }

    #[tokio::test]
    async fn a_stalled_archive_download_fails_at_the_read_bound() {
        let server = MockServer::start().await;
        serve_sample_archive(
            &server,
            ResponseTemplate::new(200)
                .set_body_bytes(sample_archive())
                .set_delay(STALLED_RESPONSE_DELAY),
        )
        .await;
        // The whole-request ceiling stays at its production value, so only
        // the read bound can end the request before the server answers.
        let client = RegistryClient::new(RegistryTimeouts {
            read: TEST_TIMEOUT,
            ..RegistryTimeouts::default()
        })
        .unwrap();

        let Err(err) = client.download_entry(&sample_entry(&server, None)).await else {
            panic!("a stalled archive download must fail");
        };

        assert!(is_timeout(&err), "expected a timeout, got {err:#}");
    }

    #[tokio::test]
    async fn the_archive_ceiling_still_bounds_the_whole_download() {
        let server = MockServer::start().await;
        serve_sample_archive(
            &server,
            ResponseTemplate::new(200)
                .set_body_bytes(sample_archive())
                .set_delay(STALLED_RESPONSE_DELAY),
        )
        .await;
        let client = RegistryClient::new(RegistryTimeouts {
            archive: TEST_TIMEOUT,
            ..RegistryTimeouts::default()
        })
        .unwrap();

        let Err(err) = client.download_entry(&sample_entry(&server, None)).await else {
            panic!("a download past the ceiling must fail");
        };

        assert!(is_timeout(&err), "expected a timeout, got {err:#}");
    }

    /// The read bound ends a stall, not a slow download: an archive that
    /// keeps arriving in pieces, each well inside the bound, completes even
    /// though the whole transfer takes longer than the bound.
    #[tokio::test]
    async fn a_slow_archive_download_that_keeps_delivering_completes() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        const READ_BOUND: Duration = Duration::from_secs(1);
        const GAP: Duration = Duration::from_millis(200);
        const PIECES: usize = 8;

        let archive = sample_archive();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let entry = PluginRegistryEntry {
            name: "sample".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            capabilities: vec!["tool".to_string()],
            url: format!("http://{address}{SAMPLE_ARCHIVE_PATH}"),
            sha256: Some(hex::encode(Sha256::digest(&archive))),
        };
        let client = RegistryClient::new(RegistryTimeouts {
            read: READ_BOUND,
            ..RegistryTimeouts::default()
        })
        .unwrap();

        // A server that sends the headers at once and then the body in
        // pieces, pausing before each one.
        let trickle = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.set_nodelay(true).unwrap();
            let mut head = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0, "the client closed before sending its request");
                head.extend_from_slice(&buffer[..read]);
            }
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                archive.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            for piece in archive.chunks(archive.len().div_ceil(PIECES)) {
                tokio::time::sleep(GAP).await;
                socket.write_all(piece).await.unwrap();
                socket.flush().await.unwrap();
            }
        };
        let started = std::time::Instant::now();

        let (downloaded, ()) = tokio::join!(client.download_entry(&entry), trickle);

        let downloaded = downloaded.expect("a download that keeps delivering completes");
        assert_eq!(downloaded.manifest().name, "sample");
        assert!(
            started.elapsed() > READ_BOUND,
            "the transfer outlasted the read bound, so only the pauses were bounded"
        );
    }

    fn sample_archive() -> Vec<u8> {
        zip_with_entry("sample/manifest.toml", SAMPLE_MANIFEST)
    }

    fn sample_entry(server: &MockServer, sha256: Option<String>) -> PluginRegistryEntry {
        PluginRegistryEntry {
            name: "sample".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            capabilities: vec!["tool".to_string()],
            url: format!("{}{SAMPLE_ARCHIVE_PATH}", server.uri()),
            sha256,
        }
    }

    async fn serve_sample_archive(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(SAMPLE_ARCHIVE_PATH))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn is_timeout(err: &anyhow::Error) -> bool {
        err.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        })
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
