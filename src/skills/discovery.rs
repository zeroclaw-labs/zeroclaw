//! Pinned `agentskills` well-known discovery and audited installation.
//!
//! This is deliberately a fresh, bounded fetch for each invocation. The index
//! is untrusted input and is never treated as a source of policy. Every HTTP
//! hop is checked, DNS answers are retained as the dial pin, and the selected
//! raw artifact is verified before it is unpacked or copied into the existing
//! audited installer.

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use reqwest::Url;
use reqwest::header::LOCATION;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tar::Archive;

const DISCOVERY_SCHEMA: &str = "https://schemas.agentskills.io/discovery/0.2.0/schema.json";
const WELL_KNOWN_PATH: &str = "/.well-known/agent-skills/index.json";
const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
const MAX_UNPACKED_BYTES: usize = 32 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 512;
const MAX_REDIRECTS: usize = 3;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct DiscoveryIndex {
    #[serde(rename = "$schema")]
    schema: String,
    skills: Vec<DiscoveryEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscoveryEntry {
    name: String,
    description: String,
    #[serde(rename = "type")]
    artifact_type: String,
    url: String,
    digest: String,
}

struct ValidatedTarget {
    url: Url,
    destination: zeroclaw_infra::net_guard::ResolvedDestination,
}

/// Fetch and install one selected entry from an origin's well-known index.
pub async fn install_well_known_skill(
    source: &str,
    selected_name: &str,
    skills_path: &Path,
    allow_scripts: bool,
    nat64_config: &[String],
) -> Result<(PathBuf, usize)> {
    install_well_known_skill_with_transport(
        source,
        selected_name,
        skills_path,
        allow_scripts,
        nat64_config,
        &ProductionTransport,
    )
    .await
}

#[async_trait::async_trait]
trait DiscoveryTransport {
    async fn get(
        &self,
        logical_url: &Url,
        nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
    ) -> Result<(Url, reqwest::Response)>;
}

struct ProductionTransport;

#[async_trait::async_trait]
impl DiscoveryTransport for ProductionTransport {
    async fn get(
        &self,
        url: &Url,
        nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
    ) -> Result<(Url, reqwest::Response)> {
        let target = validate_target(url, nat64_prefixes).await?;
        let builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT);
        let builder = if target.destination.host().parse::<IpAddr>().is_ok() {
            builder
        } else {
            builder.resolve_to_addrs(target.destination.host(), target.destination.addresses())
        };
        let client = builder
            .build()
            .context("failed to build bounded discovery HTTP client")?;
        let response = client
            .get(target.url.clone())
            .send()
            .await
            .context("well-known discovery request failed")?;
        Ok((target.url, response))
    }
}

async fn install_well_known_skill_with_transport<T: DiscoveryTransport>(
    source: &str,
    selected_name: &str,
    skills_path: &Path,
    allow_scripts: bool,
    nat64_config: &[String],
    transport: &T,
) -> Result<(PathBuf, usize)> {
    validate_skill_name(selected_name)?;
    let source_url = parse_https_url(source)?;
    let index_url = source_url
        .join(WELL_KNOWN_PATH)
        .context("failed to construct the well-known index URL")?;
    let nat64_prefixes =
        zeroclaw_infra::net_guard::parse_nat64_prefixes(nat64_config, "security.nat64_prefixes")?;
    let (index_bytes, final_index_url, _) =
        fetch_bounded_with_transport(index_url, MAX_INDEX_BYTES, &nat64_prefixes, transport)
            .await?;
    let index = parse_index(&index_bytes)?;
    let entry = select_entry(index, selected_name)?;
    let artifact_url = final_index_url
        .join(&entry.url)
        .context("skill artifact URL is not a valid relative or absolute URL")?;
    let (artifact, final_artifact_url, content_type) =
        fetch_bounded_with_transport(artifact_url, MAX_ARTIFACT_BYTES, &nat64_prefixes, transport)
            .await?;
    install_verified_artifact(
        &entry,
        &final_artifact_url,
        content_type.as_deref(),
        &artifact,
        skills_path,
        allow_scripts,
    )
}

fn parse_https_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("well-known source must be a valid URL")?;
    validate_url_shape(&url)?;
    Ok(url)
}

fn validate_url_shape(url: &Url) -> Result<()> {
    if url.scheme() != "https" {
        bail!("well-known discovery requires an HTTPS URL")
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("URL userinfo is not allowed")
    }
    if url.host_str().is_none() {
        bail!("URL must include a host")
    }
    Ok(())
}

fn validate_skill_name(name: &str) -> Result<()> {
    zeroclaw_runtime::skills::scaffold::validate_name(name).map_err(|error| {
        anyhow::Error::msg(format!("invalid discovered skill name '{name}': {error}"))
    })
}

fn parse_index(bytes: &[u8]) -> Result<DiscoveryIndex> {
    let index: DiscoveryIndex =
        serde_json::from_slice(bytes).context("well-known discovery index is not valid JSON")?;
    if index.schema != DISCOVERY_SCHEMA {
        bail!("unsupported well-known discovery schema: {}", index.schema)
    }
    let mut names = HashSet::new();
    for entry in &index.skills {
        validate_skill_name(&entry.name)?;
        if entry.description.trim().is_empty() || entry.url.trim().is_empty() {
            bail!(
                "well-known discovery entry '{}' has an empty required field",
                entry.name
            )
        }
        if !names.insert(entry.name.clone()) {
            bail!(
                "well-known discovery index contains duplicate skill name '{}', which is ambiguous",
                entry.name
            )
        }
        validate_digest(&entry.digest)?;
        if entry.artifact_type != "skill-md" && entry.artifact_type != "archive" {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "skipping discovered skill '{}' with unsupported artifact type '{}'",
                    entry.name, entry.artifact_type
                )
            );
        }
    }
    Ok(index)
}

fn select_entry(index: DiscoveryIndex, selected_name: &str) -> Result<DiscoveryEntry> {
    index
        .skills
        .into_iter()
        .find(|entry| entry.name == selected_name)
        .ok_or_else(|| anyhow::Error::msg(format!("selected skill '{selected_name}' was not found in the well-known index")))
        .and_then(|entry| {
            if entry.artifact_type != "skill-md" && entry.artifact_type != "archive" {
                bail!("selected skill '{selected_name}' has unsupported artifact type '{}'; choose a supported skill", entry.artifact_type)
            }
            Ok(entry)
        })
}

fn validate_digest(raw: &str) -> Result<[u8; 32]> {
    let hex_digest = raw.strip_prefix("sha256:").ok_or_else(|| {
        anyhow::Error::msg("digest must use the sha256:<64 lowercase hex> format")
    })?;
    if hex_digest.len() != 64
        || !hex_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("digest must use the sha256:<64 lowercase hex> format")
    }
    let decoded = hex::decode(hex_digest).context("invalid sha256 digest")?;
    decoded
        .try_into()
        .map_err(|_| anyhow::Error::msg("sha256 digest must be exactly 32 bytes"))
}

async fn fetch_bounded_with_transport<T: DiscoveryTransport>(
    mut url: Url,
    max_bytes: usize,
    nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
    transport: &T,
) -> Result<(Vec<u8>, Url, Option<String>)> {
    for redirect in 0..=MAX_REDIRECTS {
        validate_url_shape(&url)?;
        let (request_url, response) = transport.get(&url, nat64_prefixes).await?;
        if response.status().is_redirection() {
            if redirect == MAX_REDIRECTS {
                bail!("well-known discovery exceeded the redirect limit")
            }
            let location = response
                .headers()
                .get(LOCATION)
                .context("well-known redirect did not include a Location header")?
                .to_str()
                .context("well-known redirect Location was not valid UTF-8")?;
            url = request_url
                .join(location)
                .context("well-known redirect Location was not a valid URL")?;
            continue;
        }
        if !response.status().is_success() {
            bail!("well-known discovery returned HTTP {}", response.status())
        }
        let (body, content_type) = read_response_bounded(response, max_bytes).await?;
        return Ok((body, request_url, content_type));
    }
    bail!("well-known discovery redirect loop")
}

async fn read_response_bounded(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<(Vec<u8>, Option<String>)> {
    let declared = response.content_length().unwrap_or(0);
    if declared > max_bytes as u64 {
        bail!("well-known response exceeds the {max_bytes}-byte limit")
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::with_capacity(
        usize::try_from(declared).context("response size is not representable")?,
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed to read well-known response")?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            bail!("well-known response exceeds the {max_bytes}-byte limit")
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, content_type))
}

async fn validate_target(
    url: &Url,
    nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
) -> Result<ValidatedTarget> {
    validate_url_shape(url)?;
    let host = zeroclaw_infra::net_guard::normalize_host(
        url.host_str().context("URL must include a host")?,
    )
    .map_err(|_| anyhow::Error::msg("URL host is invalid"))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::time::timeout(HTTP_TIMEOUT, tokio::net::lookup_host((host.as_str(), port)))
            .await
            .context("discovery DNS lookup timed out")?
            .context("failed to resolve discovery host")?
            .collect::<Vec<_>>()
    };
    let destination = zeroclaw_infra::net_guard::ResolvedDestination::new(
        &host,
        port,
        addresses,
        zeroclaw_infra::net_guard::PrivateNetworkAccess::Deny,
        nat64_prefixes,
    )
    .map_err(|error| {
        anyhow::Error::msg(format!(
            "discovery destination rejected by network policy: {error}"
        ))
    })?;
    let mut pinned_url = url.clone();
    if host.parse::<IpAddr>().is_err() {
        pinned_url
            .set_host(Some(destination.host()))
            .map_err(|_| anyhow::Error::msg("URL host is invalid"))?;
    }
    Ok(ValidatedTarget {
        url: pinned_url,
        destination,
    })
}

fn install_verified_artifact(
    entry: &DiscoveryEntry,
    artifact_url: &Url,
    content_type: Option<&str>,
    artifact: &[u8],
    skills_path: &Path,
    allow_scripts: bool,
) -> Result<(PathBuf, usize)> {
    let expected = validate_digest(&entry.digest)?;
    let actual: [u8; 32] = Sha256::digest(artifact).into();
    if actual != expected {
        bail!(
            "digest mismatch for discovered skill '{}': expected {}, got sha256:{}",
            entry.name,
            entry.digest,
            hex::encode(actual)
        )
    }
    let staging_parent = skills_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let private = tempfile::Builder::new()
        .prefix(".well-known-skill-")
        .tempdir_in(staging_parent)
        .context("failed to create private well-known staging directory")?;
    let source = private.path().join(&entry.name);
    std::fs::create_dir(&source).context("failed to create discovered skill staging directory")?;
    if entry.artifact_type == "skill-md" {
        std::fs::write(source.join("SKILL.md"), artifact)
            .context("failed to stage discovered SKILL.md")?;
    } else {
        let format = archive_format(artifact_url, content_type)?;
        extract_archive(artifact, format, &source)?;
    }
    if !source.join("SKILL.md").is_file() {
        bail!(
            "discovered skill '{}' must contain a root SKILL.md",
            entry.name
        )
    }
    // This is the only publication path. It performs the existing no-follow
    // copy, security audit, destination collision check, and atomic rename.
    zeroclaw_runtime::skills::install_local_skill_source(
        source.to_str().context("staging path is not UTF-8")?,
        skills_path,
        allow_scripts,
    )
}

#[derive(Clone, Copy)]
enum ArchiveFormat {
    TarGz,
    Zip,
}

fn archive_format(url: &Url, content_type: Option<&str>) -> Result<ArchiveFormat> {
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    if matches!(media_type.as_deref(), Some("application/zip")) {
        return Ok(ArchiveFormat::Zip);
    }
    if matches!(
        media_type.as_deref(),
        Some("application/gzip" | "application/x-gzip")
    ) {
        return Ok(ArchiveFormat::TarGz);
    }
    let generic = media_type
        .as_deref()
        .is_none_or(|value| value == "application/octet-stream");
    if generic && url.path().to_ascii_lowercase().ends_with(".zip") {
        return Ok(ArchiveFormat::Zip);
    }
    if generic
        && (url.path().to_ascii_lowercase().ends_with(".tar.gz")
            || url.path().to_ascii_lowercase().ends_with(".tgz"))
    {
        return Ok(ArchiveFormat::TarGz);
    }
    bail!("archive format could not be determined from content type or URL")
}

fn validate_archive_path(raw: &str) -> Result<PathBuf> {
    if raw.is_empty()
        || raw.contains('\\')
        || raw.contains('\0')
        || raw.starts_with('/')
        || raw.starts_with(':')
    {
        bail!("archive contains an absolute or cross-platform unsafe path: {raw:?}")
    }
    let path = Path::new(raw);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_string_lossy();
                if value.contains(':') {
                    bail!("archive contains a drive or ADS path: {raw:?}")
                }
                if value != value.trim_end_matches([' ', '.']) {
                    bail!("archive contains a path with trailing spaces or dots: {raw:?}")
                }
                if is_windows_device_name(&value) {
                    bail!("archive contains a Windows device path: {raw:?}")
                }
                clean.push(value.as_ref());
            }
            Component::CurDir => bail!("archive contains a dot path: {raw:?}"),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("archive contains path traversal: {raw:?}")
            }
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("archive contains an empty path")
    }
    Ok(clean)
}

fn is_windows_device_name(component: &str) -> bool {
    let base = component
        .split_once('.')
        .map_or(component, |(base, _)| base)
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    )
}

fn archive_path_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

fn extract_archive(bytes: &[u8], format: ArchiveFormat, destination: &Path) -> Result<()> {
    match format {
        ArchiveFormat::TarGz => extract_tar_gz(bytes, destination),
        ArchiveFormat::Zip => extract_zip(bytes, destination),
    }
}

fn extract_tar_gz(bytes: &[u8], destination: &Path) -> Result<()> {
    let mut decoder = GzDecoder::new(Cursor::new(bytes));
    let mut decoded = Vec::new();
    decoder
        .by_ref()
        .take((MAX_UNPACKED_BYTES + 1) as u64)
        .read_to_end(&mut decoded)
        .context("failed to decompress tar.gz archive")?;
    if decoded.len() > MAX_UNPACKED_BYTES {
        bail!("archive expands beyond the {MAX_UNPACKED_BYTES}-byte limit")
    }
    let mut archive = Archive::new(Cursor::new(decoded));
    let mut entries = 0usize;
    let mut unpacked = 0usize;
    let mut paths = HashSet::new();
    for item in archive.entries().context("failed to read tar.gz archive")? {
        entries += 1;
        if entries > MAX_ARCHIVE_ENTRIES {
            bail!("archive contains too many entries")
        }
        let mut entry = item.context("failed to read tar archive entry")?;
        let raw = entry
            .path()
            .context("tar archive entry has an invalid path")?
            .to_string_lossy()
            .into_owned();
        let relative = validate_archive_path(&raw)?;
        if !paths.insert(archive_path_key(&relative)) {
            bail!("archive contains duplicate path: {raw}")
        }
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            bail!("archive contains a link or special file: {raw}")
        }
        let mode = entry
            .header()
            .mode()
            .context("tar archive entry has an invalid mode")?;
        validate_archive_unix_mode(mode, entry_type.is_dir())?;
        let target = destination.join(&relative);
        if entry_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = Vec::new();
        entry
            .by_ref()
            .take((MAX_UNPACKED_BYTES.saturating_sub(unpacked) + 1) as u64)
            .read_to_end(&mut content)?;
        unpacked = unpacked.saturating_add(content.len());
        if unpacked > MAX_UNPACKED_BYTES {
            bail!("archive expands beyond the {MAX_UNPACKED_BYTES}-byte limit")
        }
        std::fs::write(&target, content)?;
        set_archive_file_mode(&target, mode & 0o777)?;
    }
    Ok(())
}

fn extract_zip(bytes: &[u8], destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("invalid zip archive")?;
    let mut entries = 0usize;
    let mut unpacked = 0usize;
    let mut paths = HashSet::new();
    for index in 0..archive.len() {
        entries += 1;
        if entries > MAX_ARCHIVE_ENTRIES {
            bail!("archive contains too many entries")
        }
        let mut entry = archive
            .by_index(index)
            .context("failed to read zip archive entry")?;
        let raw = entry.name().to_string();
        let relative = validate_archive_path(&raw)?;
        if !paths.insert(archive_path_key(&relative)) {
            bail!("archive contains duplicate path: {raw}")
        }
        if entry.is_symlink() || (!entry.is_file() && !entry.is_dir()) {
            bail!("archive contains a link or special file: {raw}")
        }
        let mode = entry.unix_mode();
        if let Some(mode) = mode {
            validate_archive_unix_mode(mode, entry.is_dir())?;
        }
        if entry.is_dir() {
            std::fs::create_dir_all(destination.join(relative))?;
            continue;
        }
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = Vec::new();
        entry
            .by_ref()
            .take((MAX_UNPACKED_BYTES.saturating_sub(unpacked) + 1) as u64)
            .read_to_end(&mut content)?;
        unpacked = unpacked.saturating_add(content.len());
        if unpacked > MAX_UNPACKED_BYTES {
            bail!("archive expands beyond the {MAX_UNPACKED_BYTES}-byte limit")
        }
        std::fs::write(&target, content)?;
        if let Some(mode) = mode {
            set_archive_file_mode(&target, mode & 0o777)?;
        }
    }
    Ok(())
}

fn validate_archive_unix_mode(mode: u32, is_dir: bool) -> Result<()> {
    if mode & 0o6000 != 0 {
        bail!("archive contains setuid or setgid permissions")
    }
    let file_type = mode & 0o170_000;
    if file_type != 0 && ((is_dir && file_type != 0o040_000) || (!is_dir && file_type != 0o100_000))
    {
        bail!("archive contains a special file")
    }
    Ok(())
}

fn set_archive_file_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct FixtureTransport {
        client: reqwest::Client,
        base: Url,
    }

    #[async_trait::async_trait]
    impl DiscoveryTransport for FixtureTransport {
        async fn get(
            &self,
            logical_url: &Url,
            _nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
        ) -> Result<(Url, reqwest::Response)> {
            let mut fixture_url = self.base.clone();
            fixture_url.set_path(logical_url.path());
            fixture_url.set_query(logical_url.query());
            Ok((
                logical_url.clone(),
                self.client.get(fixture_url).send().await?,
            ))
        }
    }

    fn fixture_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    fn entry(name: &str, artifact_type: &str, url: &str, digest: &str) -> DiscoveryEntry {
        DiscoveryEntry {
            name: name.to_string(),
            description: "test".to_string(),
            artifact_type: artifact_type.to_string(),
            url: url.to_string(),
            digest: digest.to_string(),
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }

    fn index_json(artifact_url: &str, artifact: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "$schema": DISCOVERY_SCHEMA,
            "skills": [{
                "name": "demo",
                "description": "fixture skill",
                "type": "skill-md",
                "url": artifact_url,
                "digest": digest(artifact)
            }]
        }))
        .unwrap()
    }

    #[test]
    fn index_requires_pinned_schema_and_valid_digest() {
        let bad_schema = r#"{"$schema":"x","skills":[]}"#;
        assert!(parse_index(bad_schema.as_bytes()).is_err());
        let bad_digest = serde_json::json!({
            "$schema": DISCOVERY_SCHEMA,
            "skills": [{"name":"demo","description":"d","type":"skill-md","url":"demo/SKILL.md","digest":"sha256:BAD"}]
        });
        assert!(parse_index(bad_digest.to_string().as_bytes()).is_err());
    }

    #[test]
    fn artifact_url_resolution_uses_the_actual_index_url() {
        let index = Url::parse("https://example.com/a/index.json").unwrap();
        assert_eq!(
            index.join("skill/SKILL.md").unwrap().as_str(),
            "https://example.com/a/skill/SKILL.md"
        );
        assert_eq!(
            index.join("/.well-known/skill/SKILL.md").unwrap().as_str(),
            "https://example.com/.well-known/skill/SKILL.md"
        );
        assert_eq!(
            index.join("https://cdn.example/skill.md").unwrap().as_str(),
            "https://cdn.example/skill.md"
        );
    }

    #[test]
    fn installs_single_skill_md_through_audited_pipeline() {
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        let bytes = b"# demo\n";
        let e = entry("demo", "skill-md", "demo/SKILL.md", &digest(bytes));
        let url = Url::parse("https://example.com/demo/SKILL.md").unwrap();
        let (path, _) = install_verified_artifact(&e, &url, None, bytes, &skills, false).unwrap();
        assert_eq!(std::fs::read(path.join("SKILL.md")).unwrap(), bytes);
    }

    #[test]
    fn rejects_digest_mismatch_without_publishing() {
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        let e = entry("demo", "skill-md", "demo/SKILL.md", &digest(b"other"));
        let url = Url::parse("https://example.com/demo/SKILL.md").unwrap();
        assert!(install_verified_artifact(&e, &url, None, b"actual", &skills, false).is_err());
        assert!(!skills.join("demo").exists());
    }

    #[test]
    fn rejects_archive_traversal_and_links() {
        assert!(validate_archive_path("../escape").is_err());
        assert!(validate_archive_path("C:\\escape").is_err());
        assert!(validate_archive_path("dir\\escape").is_err());
        assert!(validate_archive_path("docs:secret").is_err());
        assert!(validate_archive_path("CON.txt").is_err());
        assert!(validate_archive_path("docs. ").is_err());
    }

    #[test]
    fn archive_mime_type_is_exact_and_header_wins_over_extension() {
        let zip = Url::parse("https://example.com/skill.tar.gz").unwrap();
        let tar = Url::parse("https://example.com/skill.zip").unwrap();
        assert!(matches!(
            archive_format(&zip, Some("application/zip; charset=binary")),
            Ok(ArchiveFormat::Zip)
        ));
        assert!(matches!(
            archive_format(&tar, Some("application/gzip")),
            Ok(ArchiveFormat::TarGz)
        ));
        assert!(
            archive_format(
                &Url::parse("https://example.com/skill").unwrap(),
                Some("application/x-custom")
            )
            .is_err()
        );
        assert!(matches!(
            archive_format(&zip, Some("application/octet-stream")),
            Ok(ArchiveFormat::TarGz)
        ));
        assert!(matches!(
            archive_format(&tar, Some("application/zip")),
            Ok(ArchiveFormat::Zip)
        ));
    }

    #[test]
    fn audit_failure_and_destination_collision_never_publish_partial_skill() {
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        std::fs::create_dir_all(skills.join("demo")).unwrap();
        std::fs::write(skills.join("demo/SKILL.md"), "# existing\n").unwrap();
        let bytes = b"# replacement\n";
        let e = entry("demo", "skill-md", "demo/SKILL.md", &digest(bytes));
        let url = Url::parse("https://example.com/demo/SKILL.md").unwrap();
        assert!(install_verified_artifact(&e, &url, None, bytes, &skills, false).is_err());
        assert_eq!(
            std::fs::read_to_string(skills.join("demo/SKILL.md")).unwrap(),
            "# existing\n"
        );
    }

    #[test]
    fn installs_tar_gz_and_zip_archives() {
        let root = tempfile::tempdir().unwrap();
        let tar_bytes = {
            let mut raw = Vec::new();
            {
                let encoder =
                    flate2::write::GzEncoder::new(&mut raw, flate2::Compression::default());
                let mut builder = tar::Builder::new(encoder);
                let data = b"# archive\n";
                let mut header = tar::Header::new_gnu();
                header.set_path("SKILL.md").unwrap();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, &data[..]).unwrap();
                builder.into_inner().unwrap().finish().unwrap();
            }
            raw
        };
        let zip_bytes = {
            let mut raw = Cursor::new(Vec::new());
            let mut writer = zip::ZipWriter::new(&mut raw);
            writer
                .start_file("SKILL.md", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"# zip\n").unwrap();
            writer.finish().unwrap();
            raw.into_inner()
        };
        let tar_dest = root.path().join("tar");
        let zip_dest = root.path().join("zip");
        std::fs::create_dir_all(&tar_dest).unwrap();
        std::fs::create_dir_all(&zip_dest).unwrap();
        extract_archive(&tar_bytes, ArchiveFormat::TarGz, &tar_dest).unwrap();
        extract_archive(&zip_bytes, ArchiveFormat::Zip, &zip_dest).unwrap();
        assert!(tar_dest.join("SKILL.md").is_file());
        assert!(zip_dest.join("SKILL.md").is_file());

        let tar_published = root.path().join("tar-published");
        let tar_entry = entry("tar-demo", "archive", "tar-demo.zip", &digest(&tar_bytes));
        let tar_url = Url::parse("https://example.com/tar-demo.zip").unwrap();
        let (tar_path, _) = install_verified_artifact(
            &tar_entry,
            &tar_url,
            Some("application/gzip"),
            &tar_bytes,
            &tar_published,
            false,
        )
        .unwrap();
        assert!(tar_path.join("SKILL.md").is_file());

        let published = root.path().join("published");
        let archive_entry = entry(
            "archive-demo",
            "archive",
            "archive-demo.zip",
            &digest(&zip_bytes),
        );
        let archive_url = Url::parse("https://example.com/archive-demo.zip").unwrap();
        let (published_path, _) = install_verified_artifact(
            &archive_entry,
            &archive_url,
            Some("application/zip"),
            &zip_bytes,
            &published,
            false,
        )
        .unwrap();
        assert!(published_path.join("SKILL.md").is_file());
    }

    #[test]
    fn script_audit_rejects_archive_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let skills = root.path().join("skills");
        let mut raw = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut raw);
        writer
            .start_file("SKILL.md", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"# demo\n").unwrap();
        writer
            .start_file("run.sh", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"#!/bin/sh\necho unsafe\n").unwrap();
        writer.finish().unwrap();
        let bytes = raw.into_inner();
        let e = entry("demo", "archive", "demo.zip", &digest(&bytes));
        let url = Url::parse("https://example.com/demo.zip").unwrap();
        assert!(
            install_verified_artifact(&e, &url, Some("application/zip"), &bytes, &skills, false)
                .is_err()
        );
        assert!(!skills.join("demo").exists());
    }

    #[cfg(unix)]
    #[test]
    fn preserves_safe_executable_mode_without_running_archive_scripts() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let mut raw = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut raw, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let skill = b"# demo\n";
            let mut skill_header = tar::Header::new_gnu();
            skill_header.set_path("SKILL.md").unwrap();
            skill_header.set_size(skill.len() as u64);
            skill_header.set_mode(0o644);
            skill_header.set_cksum();
            builder.append(&skill_header, &skill[..]).unwrap();
            let script = b"#!/bin/sh\nexit 0\n";
            let mut script_header = tar::Header::new_gnu();
            script_header.set_path("scripts/run.sh").unwrap();
            script_header.set_size(script.len() as u64);
            script_header.set_mode(0o755);
            script_header.set_cksum();
            builder.append(&script_header, &script[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let entry = entry("exec-demo", "archive", "exec-demo.tar.gz", &digest(&raw));
        let url = Url::parse("https://example.com/exec-demo.tar.gz").unwrap();
        let (path, _) = install_verified_artifact(
            &entry,
            &url,
            Some("application/gzip"),
            &raw,
            &root.path().join("skills"),
            true,
        )
        .unwrap();
        let mode = std::fs::metadata(path.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn rejects_archive_links_duplicates_and_entry_count_overflow() {
        let root = tempfile::tempdir().unwrap();
        let linked = {
            let mut raw = Vec::new();
            {
                let encoder =
                    flate2::write::GzEncoder::new(&mut raw, flate2::Compression::default());
                let mut builder = tar::Builder::new(encoder);
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_path("link").unwrap();
                header.set_entry_type(tar::EntryType::symlink());
                header.set_link_name("SKILL.md").unwrap();
                header.set_size(0);
                header.set_cksum();
                builder.append(&header, &[][..]).unwrap();
                builder.into_inner().unwrap().finish().unwrap();
            }
            raw
        };
        assert!(extract_archive(&linked, ArchiveFormat::TarGz, root.path()).is_err());

        let duplicate = {
            let mut raw = Vec::new();
            {
                let encoder =
                    flate2::write::GzEncoder::new(&mut raw, flate2::Compression::default());
                let mut builder = tar::Builder::new(encoder);
                for _ in 0..2 {
                    let data = b"# demo\n";
                    let mut header = tar::Header::new_gnu();
                    header.set_mode(0o644);
                    header.set_path("SKILL.md").unwrap();
                    header.set_size(data.len() as u64);
                    header.set_cksum();
                    builder.append(&header, &data[..]).unwrap();
                }
                builder.into_inner().unwrap().finish().unwrap();
            }
            raw
        };
        assert!(extract_archive(&duplicate, ArchiveFormat::TarGz, root.path()).is_err());

        let too_many = {
            let mut raw = Vec::new();
            {
                let encoder =
                    flate2::write::GzEncoder::new(&mut raw, flate2::Compression::default());
                let mut builder = tar::Builder::new(encoder);
                for index in 0..=MAX_ARCHIVE_ENTRIES {
                    let mut header = tar::Header::new_gnu();
                    header.set_mode(0o644);
                    header.set_path(format!("entry-{index}")).unwrap();
                    header.set_size(0);
                    header.set_cksum();
                    builder.append(&header, &[][..]).unwrap();
                }
                builder.into_inner().unwrap().finish().unwrap();
            }
            raw
        };
        assert!(extract_archive(&too_many, ArchiveFormat::TarGz, root.path()).is_err());
    }

    #[test]
    fn rejects_compressed_tar_with_oversized_extension_metadata_before_parsing_entries() {
        let root = tempfile::tempdir().unwrap();
        let mut raw = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut raw, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let long_path = format!("{}.md", "a".repeat(MAX_UNPACKED_BYTES));
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(0);
            builder
                .append_data(&mut header, &long_path, &[][..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        assert!(raw.len() < MAX_ARTIFACT_BYTES);
        assert!(extract_archive(&raw, ArchiveFormat::TarGz, root.path()).is_err());
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn fixture_transport_drives_selected_discovery_and_audited_publish() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let artifact = b"# demo\n";
        Mock::given(method("GET"))
            .and(path("/.well-known/agent-skills/index.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(index_json("demo/SKILL.md", artifact), "application/json"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/agent-skills/demo/SKILL.md"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(artifact.to_vec(), "text/markdown"),
            )
            .mount(&server)
            .await;
        let transport = FixtureTransport {
            client: fixture_client(),
            base: Url::parse(&server.uri()).unwrap(),
        };
        let root = tempfile::tempdir().unwrap();
        let (path, _) = install_well_known_skill_with_transport(
            "https://fixture.test",
            "demo",
            &root.path().join("skills"),
            false,
            &[],
            &transport,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(path.join("SKILL.md")).unwrap(), artifact);
    }

    #[tokio::test]
    async fn fixture_transport_rejects_redirect_to_non_https() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/agent-skills/index.json"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "http://127.0.0.1/private"),
            )
            .mount(&server)
            .await;
        let transport = FixtureTransport {
            client: fixture_client(),
            base: Url::parse(&server.uri()).unwrap(),
        };
        let error = fetch_bounded_with_transport(
            Url::parse("https://fixture.test/.well-known/agent-skills/index.json").unwrap(),
            MAX_INDEX_BYTES,
            &[],
            &transport,
        )
        .await
        .expect_err("redirect downgrade must be rejected");
        assert!(error.to_string().contains("HTTPS"), "got: {error}");
    }

    #[tokio::test]
    async fn fixture_transport_bounds_oversized_chunked_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let body = vec![b'x'; MAX_INDEX_BYTES + 1];
        let server = ::zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n")
                .await
                .unwrap();
            for chunk in body.chunks(8192) {
                if stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                    || stream.write_all(chunk).await.is_err()
                    || stream.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
        });
        let transport = FixtureTransport {
            client: fixture_client(),
            base: Url::parse(&format!("http://{address}")).unwrap(),
        };
        let error = fetch_bounded_with_transport(
            Url::parse("https://fixture.test/.well-known/agent-skills/index.json").unwrap(),
            MAX_INDEX_BYTES,
            &[],
            &transport,
        )
        .await
        .expect_err("chunked response must be bounded");
        assert!(error.to_string().contains("limit"), "got: {error}");
        server.await.unwrap();
    }
}
