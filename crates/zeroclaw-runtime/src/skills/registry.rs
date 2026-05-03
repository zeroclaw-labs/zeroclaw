use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tar::Archive;
use zip::ZipArchive;

// ─── Shared types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SkillSearchResult {
    pub name: String,
    pub description: String,
    pub registry: String,
    pub source_url: String,
    pub version: Option<String>,
}

pub trait SkillRegistry: Send + Sync {
    fn name(&self) -> &str;
    fn matches_source(&self, source: &str) -> bool;
    fn search(&self, query: &str) -> Result<Vec<SkillSearchResult>>;
    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)>;
}

// ─── Shared HTTP ZIP installer ───────────────────────────────────────────────

const MAX_ZIP_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB
const MAX_ZIP_ENTRIES: usize = 500;
const MAX_DECOMPRESSION_RATIO: u64 = 10;

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")
}

pub fn install_http_zip_skill(
    download_url: &str,
    skill_dir_name: &str,
    skills_path: &Path,
    allow_scripts: bool,
    registry_name: &str,
) -> Result<(PathBuf, usize)> {
    let installed_dir = skills_path.join(skill_dir_name);
    if installed_dir.exists() {
        anyhow::bail!(
            "Destination skill already exists: {}",
            installed_dir.display()
        );
    }

    let client = http_client()?;
    let resp = client
        .get(download_url)
        .send()
        .with_context(|| format!("failed to fetch zip from {download_url}"))?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        anyhow::bail!("{registry_name} rate limit reached (HTTP 429). Wait a moment and retry.");
    }
    if !resp.status().is_success() {
        anyhow::bail!("{registry_name} download failed (HTTP {})", resp.status());
    }

    let bytes = resp.bytes()?.to_vec();
    let compressed_size = bytes.len() as u64;
    if compressed_size > MAX_ZIP_BYTES {
        anyhow::bail!(
            "{registry_name} zip rejected: too large ({compressed_size} bytes > {MAX_ZIP_BYTES})"
        );
    }

    std::fs::create_dir_all(&installed_dir)?;

    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).context("downloaded content is not a valid zip")?;

    if archive.len() > MAX_ZIP_ENTRIES {
        let _ = std::fs::remove_dir_all(&installed_dir);
        anyhow::bail!(
            "{registry_name} zip rejected: too many entries ({} > {MAX_ZIP_ENTRIES})",
            archive.len()
        );
    }

    let mut decompressed_total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_string();

        if raw_name.is_empty()
            || raw_name.contains("..")
            || raw_name.starts_with('/')
            || raw_name.contains('\\')
            || raw_name.contains(':')
        {
            let _ = std::fs::remove_dir_all(&installed_dir);
            anyhow::bail!("zip entry contains unsafe path: {raw_name}");
        }

        decompressed_total += entry.size();
        if compressed_size > 0 && decompressed_total > compressed_size * MAX_DECOMPRESSION_RATIO {
            let _ = std::fs::remove_dir_all(&installed_dir);
            anyhow::bail!(
                "{registry_name} zip rejected: decompression ratio exceeds {MAX_DECOMPRESSION_RATIO}x (zip bomb protection)"
            );
        }

        let out_path = installed_dir.join(&raw_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut out_file = std::fs::File::create(&out_path)
            .with_context(|| format!("failed to create extracted file: {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out_file)?;
    }

    let has_manifest = installed_dir.join("SKILL.md").exists()
        || installed_dir.join("SKILL.toml").exists()
        || installed_dir.join("manifest.toml").exists();
    if !has_manifest {
        std::fs::write(
            installed_dir.join("SKILL.toml"),
            format!(
                "[skill]\nname = \"{skill_dir_name}\"\ndescription = \"{registry_name} installed skill\"\nversion = \"0.1.0\"\n"
            ),
        )?;
    }

    match super::enforce_skill_security_audit(&installed_dir, allow_scripts) {
        Ok(report) => Ok((installed_dir, report.files_scanned)),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&installed_dir);
            Err(err)
        }
    }
}

/// Install a skill from a GitHub repo by downloading the repo tarball and
/// extracting a single subdirectory (the skill). Tries common layouts:
/// `skills/<skill>/`, `<skill>/`, then falls back to a recursive scan.
pub fn install_github_subdir_skill(
    owner: &str,
    repo: &str,
    skill: &str,
    skills_path: &Path,
    allow_scripts: bool,
    registry_name: &str,
) -> Result<(PathBuf, usize)> {
    let dir_name = normalize_skill_name(skill);
    let installed_dir = skills_path.join(&dir_name);
    if installed_dir.exists() {
        anyhow::bail!(
            "Destination skill already exists: {}",
            installed_dir.display()
        );
    }

    // GitHub's codeload tarball URL — no auth required for public repos
    let tarball_url = format!("https://codeload.github.com/{owner}/{repo}/tar.gz/HEAD");
    let client = http_client()?;
    let resp = client
        .get(&tarball_url)
        .send()
        .with_context(|| format!("failed to fetch tarball from {tarball_url}"))?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        anyhow::bail!("{registry_name} (GitHub) rate limit reached (HTTP 429)");
    }
    if !resp.status().is_success() {
        anyhow::bail!(
            "{registry_name} download failed (HTTP {}): {tarball_url}",
            resp.status()
        );
    }

    let bytes = resp.bytes()?.to_vec();
    if bytes.len() as u64 > MAX_ZIP_BYTES {
        anyhow::bail!(
            "{registry_name} tarball rejected: too large ({} bytes > {MAX_ZIP_BYTES})",
            bytes.len()
        );
    }

    // Decompress + extract in-memory; then locate the skill subdirectory and
    // copy only that subdirectory into the destination.
    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    let gz = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(gz);
    archive.set_preserve_permissions(false);
    archive
        .unpack(tmp.path())
        .with_context(|| format!("failed to extract {registry_name} tarball"))?;

    // GitHub tarballs unpack to a single top-level dir like `<repo>-<sha>/`
    let top_dir = std::fs::read_dir(tmp.path())?
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .ok_or_else(|| anyhow::anyhow!("empty tarball from {registry_name}"))?
        .path();

    let candidates = [top_dir.join("skills").join(skill), top_dir.join(skill)];

    let skill_src: PathBuf = candidates
        .iter()
        .find(|p| p.is_dir() && has_skill_manifest(p))
        .cloned()
        .or_else(|| candidates.iter().find(|p| p.is_dir()).cloned())
        .or_else(|| walk_for_skill_dir(&top_dir, skill))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "skill '{skill}' not found in {owner}/{repo} (tried skills/{skill} and {skill})"
            )
        })?;

    std::fs::create_dir_all(&installed_dir)?;
    if let Err(err) = super::copy_dir_recursive_secure(&skill_src, &installed_dir) {
        let _ = std::fs::remove_dir_all(&installed_dir);
        return Err(err);
    }

    let has_manifest = installed_dir.join("SKILL.md").exists()
        || installed_dir.join("SKILL.toml").exists()
        || installed_dir.join("manifest.toml").exists();
    if !has_manifest {
        std::fs::write(
            installed_dir.join("SKILL.toml"),
            format!(
                "[skill]\nname = \"{dir_name}\"\ndescription = \"{registry_name} skill from {owner}/{repo}\"\nversion = \"0.1.0\"\n"
            ),
        )?;
    }

    match super::enforce_skill_security_audit(&installed_dir, allow_scripts) {
        Ok(report) => Ok((installed_dir, report.files_scanned)),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&installed_dir);
            Err(err)
        }
    }
}

fn has_skill_manifest(p: &Path) -> bool {
    p.join("SKILL.md").exists() || p.join("SKILL.toml").exists() || p.join("manifest.toml").exists()
}

fn walk_for_skill_dir(root: &Path, skill: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            if p.file_name().is_some_and(|n| n == skill) && has_skill_manifest(&p) {
                return Some(p);
            }
            stack.push(p);
        }
    }
    None
}

fn search_http_json_registry(
    api_url: &str,
    query: &str,
    registry_name: &str,
) -> Result<Vec<SkillSearchResult>> {
    let client = http_client()?;
    let url = format!("{api_url}?q={}", urlencoding::encode(query));
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("{registry_name} search failed: {e}");
            return Ok(vec![]);
        }
    };

    if !resp.status().is_success() {
        tracing::warn!("{registry_name} search returned HTTP {}", resp.status());
        return Ok(vec![]);
    }

    let body: serde_json::Value = resp.json().context("invalid JSON from registry")?;

    let items = body
        .get("skills")
        .or_else(|| body.get("results"))
        .or_else(|| body.get("data"))
        .and_then(|v| v.as_array());

    let Some(items) = items else {
        return Ok(vec![]);
    };

    Ok(items
        .iter()
        .filter_map(|item| {
            let name = item
                .get("name")
                .or_else(|| item.get("slug"))
                .and_then(|v| v.as_str())?
                .to_string();
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let version = item
                .get("version")
                .and_then(|v| v.as_str())
                .map(String::from);
            let source_url = item
                .get("url")
                .or_else(|| item.get("source_url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            Some(SkillSearchResult {
                name,
                description,
                registry: registry_name.to_string(),
                source_url,
                version,
            })
        })
        .collect())
}

// ─── ClawhHub registry ──────────────────────────────────────────────────────

const CLAWHUB_DOMAIN: &str = "clawhub.ai";
const CLAWHUB_WWW_DOMAIN: &str = "www.clawhub.ai";
const CLAWHUB_DOWNLOAD_API: &str = "https://clawhub.ai/api/v1/download";
const CLAWHUB_SEARCH_API: &str = "https://clawhub.ai/api/v1/search";

pub struct ClawhubRegistry;

impl ClawhubRegistry {
    fn is_clawhub_host(host: &str) -> bool {
        host.eq_ignore_ascii_case(CLAWHUB_DOMAIN) || host.eq_ignore_ascii_case(CLAWHUB_WWW_DOMAIN)
    }

    fn parse_url(source: &str) -> Option<reqwest::Url> {
        let parsed = reqwest::Url::parse(source).ok()?;
        match parsed.scheme() {
            "https" | "http" => {}
            _ => return None,
        }
        if !parsed.host_str().is_some_and(Self::is_clawhub_host) {
            return None;
        }
        Some(parsed)
    }

    /// Bare `owner/slug` (no scheme, exactly one `/`, no leading dot/tilde,
    /// safe characters only) is treated as a ClawHub shorthand so users can
    /// run `zeroclaw skills install pskoett/self-improving-agent` without
    /// typing the full URL or `clawhub:` prefix.
    fn is_bare_owner_slug(source: &str) -> bool {
        if source.contains("://") || source.contains(':') {
            return false;
        }
        if source.starts_with('.') || source.starts_with('~') || source.starts_with('/') {
            return false;
        }
        if source.contains('\\') || source.contains("..") {
            return false;
        }
        let parts: Vec<&str> = source.split('/').collect();
        if parts.len() != 2 {
            return false;
        }
        let segment_ok = |s: &&str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
                && !s.starts_with('.')
        };
        parts.iter().all(segment_ok)
    }

    fn download_url(source: &str) -> Result<String> {
        if let Some(slug) = source.strip_prefix("clawhub:") {
            let slug = slug.trim().trim_end_matches('/');
            if slug.is_empty() {
                anyhow::bail!("invalid clawhub source '{source}': expected 'clawhub:<slug>'");
            }
            return Ok(format!("{CLAWHUB_DOWNLOAD_API}?slug={slug}"));
        }

        if Self::is_bare_owner_slug(source) {
            return Ok(format!("{CLAWHUB_DOWNLOAD_API}?slug={source}"));
        }

        if let Some(parsed) = Self::parse_url(source) {
            let path = parsed
                .path_segments()
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("/");
            if path.is_empty() {
                anyhow::bail!("could not extract slug from ClawhHub URL: {source}");
            }
            return Ok(format!("{CLAWHUB_DOWNLOAD_API}?slug={path}"));
        }

        anyhow::bail!("unrecognised ClawhHub source format: {source}")
    }

    fn skill_dir_name(source: &str) -> Result<String> {
        let raw = if let Some(slug) = source.strip_prefix("clawhub:") {
            slug.trim()
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(slug)
        } else if Self::is_bare_owner_slug(source) {
            source.rsplit('/').next().unwrap_or(source)
        } else if let Some(parsed) = Self::parse_url(source) {
            let segs: Vec<_> = parsed.path_segments().into_iter().flatten().collect();
            return Ok(normalize_skill_name(
                segs.last().copied().unwrap_or("skill"),
            ));
        } else {
            "skill"
        };
        let name = normalize_skill_name(raw);
        Ok(if name.is_empty() {
            "skill".into()
        } else {
            name
        })
    }
}

impl SkillRegistry for ClawhubRegistry {
    fn name(&self) -> &str {
        "ClawhHub"
    }

    fn matches_source(&self, source: &str) -> bool {
        source.starts_with("clawhub:")
            || Self::parse_url(source).is_some()
            || Self::is_bare_owner_slug(source)
    }

    fn search(&self, query: &str) -> Result<Vec<SkillSearchResult>> {
        search_http_json_registry(CLAWHUB_SEARCH_API, query, "ClawhHub")
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        let url = Self::download_url(source)?;
        let dir_name = Self::skill_dir_name(source)?;
        install_http_zip_skill(&url, &dir_name, skills_path, allow_scripts, "ClawhHub")
    }
}

// ─── agentskills.io registry ─────────────────────────────────────────────────
//
// agentskills.io is the **specification** site (Mintlify docs only — no skill
// download API). Skills following the agentskills format are indexed by
// skills.sh and hosted on GitHub. This registry accepts
// `agentskills:<owner>/<repo>/<skill>` as an alias for the same skillssh:
// triplet so users who think of the spec name can still install.

pub struct AgentSkillsIoRegistry;

impl AgentSkillsIoRegistry {
    fn is_agentskills_host(host: &str) -> bool {
        host.eq_ignore_ascii_case("agentskills.io")
            || host.eq_ignore_ascii_case("www.agentskills.io")
    }

    fn parse_url(source: &str) -> Option<reqwest::Url> {
        let parsed = reqwest::Url::parse(source).ok()?;
        match parsed.scheme() {
            "https" | "http" => {}
            _ => return None,
        }
        if !parsed.host_str().is_some_and(Self::is_agentskills_host) {
            return None;
        }
        Some(parsed)
    }
}

impl SkillRegistry for AgentSkillsIoRegistry {
    fn name(&self) -> &str {
        "agentskills.io"
    }

    fn matches_source(&self, source: &str) -> bool {
        source.starts_with("agentskills:") || Self::parse_url(source).is_some()
    }

    /// agentskills.io is a docs site; route searches through skills.sh.
    fn search(&self, query: &str) -> Result<Vec<SkillSearchResult>> {
        SkillsShRegistry.search(query)
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        // Extract the path portion from either the `agentskills:` prefix
        // form or a full agentskills.io URL.
        let path = if let Some(rest) = source.strip_prefix("agentskills:") {
            rest.trim().trim_matches('/').to_string()
        } else if let Some(parsed) = Self::parse_url(source) {
            parsed
                .path_segments()
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        } else {
            anyhow::bail!("unrecognised agentskills.io source format: {source}")
        };

        // agentskills.io delegates downloads to skills.sh, which requires a
        // `<owner>/<repo>/<skill>` triplet. Validate up front with an
        // agentskills-shaped error so users don't see the internal
        // skillssh: translation leak through (which previously made the
        // error message look like they'd typed `skillssh:` when they
        // hadn't).
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() != 3 {
            anyhow::bail!(
                "invalid agentskills.io source '{source}': expected \
                 'agentskills:<owner>/<repo>/<skill>' \
                 (e.g. agentskills:anthropics/skills/webapp-testing). Got {} segment(s).",
                segs.len()
            );
        }

        SkillsShRegistry.install(&format!("skillssh:{path}"), skills_path, allow_scripts)
    }
}

// ─── skills.sh registry ─────────────────────────────────────────────────────
//
// skills.sh is the de-facto agent-skills registry. Skills are indexed via its
// JSON API and the actual content is hosted on GitHub. Identity for a skill is
// the triplet `<owner>/<repo>/<skill>` — install resolves to the GitHub repo
// `<owner>/<repo>` and extracts `skills/<skill>/` (or `<skill>/` at the root).

const SKILLSSH_DOMAIN: &str = "skills.sh";
const SKILLSSH_WWW_DOMAIN: &str = "www.skills.sh";
const SKILLSSH_SEARCH_API: &str = "https://skills.sh/api/search";

pub struct SkillsShRegistry;

#[derive(Debug, Clone)]
struct SkillsShTriplet {
    owner: String,
    repo: String,
    skill: String,
}

impl SkillsShRegistry {
    fn is_skillssh_host(host: &str) -> bool {
        host.eq_ignore_ascii_case(SKILLSSH_DOMAIN) || host.eq_ignore_ascii_case(SKILLSSH_WWW_DOMAIN)
    }

    fn parse_url(source: &str) -> Option<reqwest::Url> {
        let parsed = reqwest::Url::parse(source).ok()?;
        match parsed.scheme() {
            "https" | "http" => {}
            _ => return None,
        }
        if !parsed.host_str().is_some_and(Self::is_skillssh_host) {
            return None;
        }
        Some(parsed)
    }

    /// Parse `<owner>/<repo>/<skill>` from a skillssh: prefix or skills.sh URL.
    fn parse_triplet(source: &str) -> Result<SkillsShTriplet> {
        let raw = if let Some(s) = source.strip_prefix("skillssh:") {
            s.trim().trim_matches('/').to_string()
        } else if let Some(parsed) = Self::parse_url(source) {
            parsed
                .path_segments()
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        } else {
            anyhow::bail!("unrecognised skills.sh source format: {source}")
        };

        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            anyhow::bail!(
                "invalid skills.sh source '{source}': expected '<owner>/<repo>/<skill>' (e.g. anthropics/skills/webapp-testing)"
            );
        }

        Ok(SkillsShTriplet {
            owner: parts[0].into(),
            repo: parts[1].into(),
            skill: parts[2].into(),
        })
    }

    /// Parse skills.sh JSON search response into SkillSearchResult entries.
    fn parse_search_response(body: &serde_json::Value) -> Vec<SkillSearchResult> {
        let Some(items) = body.get("skills").and_then(|v| v.as_array()) else {
            return vec![];
        };
        items
            .iter()
            .filter_map(|item| {
                let name = item.get("name").and_then(|v| v.as_str())?.to_string();
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let installs = item.get("installs").and_then(|v| v.as_u64()).unwrap_or(0);
                Some(SkillSearchResult {
                    name,
                    description: if installs > 0 {
                        format!("{installs} installs")
                    } else {
                        String::new()
                    },
                    registry: "skills.sh".into(),
                    source_url: format!("skillssh:{id}"),
                    version: None,
                })
            })
            .collect()
    }
}

impl SkillRegistry for SkillsShRegistry {
    fn name(&self) -> &str {
        "skills.sh"
    }

    fn matches_source(&self, source: &str) -> bool {
        source.starts_with("skillssh:") || Self::parse_url(source).is_some()
    }

    fn search(&self, query: &str) -> Result<Vec<SkillSearchResult>> {
        let client = http_client()?;
        let url = format!("{SKILLSSH_SEARCH_API}?q={}", urlencoding::encode(query));
        let resp = match client.get(&url).send() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("skills.sh search failed: {e}");
                return Ok(vec![]);
            }
        };
        if !resp.status().is_success() {
            tracing::warn!("skills.sh search returned HTTP {}", resp.status());
            return Ok(vec![]);
        }
        let body: serde_json::Value = resp.json().context("invalid JSON from skills.sh")?;
        Ok(Self::parse_search_response(&body))
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        let t = Self::parse_triplet(source)?;
        install_github_subdir_skill(
            &t.owner,
            &t.repo,
            &t.skill,
            skills_path,
            allow_scripts,
            "skills.sh",
        )
    }
}

// ─── GitHub direct-install registry ─────────────────────────────────────────
//
// `github:<owner>/<repo>/<skill>` — direct install from any GitHub repo
// following the agent-skills convention (skills/<skill>/SKILL.md or
// <skill>/SKILL.md at the repo root).

pub struct GitHubSkillRegistry;

impl SkillRegistry for GitHubSkillRegistry {
    fn name(&self) -> &str {
        "github"
    }

    fn matches_source(&self, source: &str) -> bool {
        source.starts_with("github:")
    }

    fn search(&self, _query: &str) -> Result<Vec<SkillSearchResult>> {
        Ok(vec![])
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        let raw = source
            .strip_prefix("github:")
            .unwrap_or("")
            .trim()
            .trim_matches('/');
        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            anyhow::bail!(
                "invalid github source '{source}': expected 'github:<owner>/<repo>/<skill>'"
            );
        }
        install_github_subdir_skill(
            parts[0],
            parts[1],
            parts[2],
            skills_path,
            allow_scripts,
            "github",
        )
    }
}

// ─── Git registry ────────────────────────────────────────────────────────────

pub struct GitRegistry;

impl SkillRegistry for GitRegistry {
    fn name(&self) -> &str {
        "git"
    }

    fn matches_source(&self, source: &str) -> bool {
        if ClawhubRegistry.matches_source(source)
            || AgentSkillsIoRegistry.matches_source(source)
            || SkillsShRegistry.matches_source(source)
            || GitHubSkillRegistry.matches_source(source)
        {
            return false;
        }
        super::is_git_source(source)
    }

    fn search(&self, _query: &str) -> Result<Vec<SkillSearchResult>> {
        Ok(vec![])
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        super::install_git_skill_source(source, skills_path, allow_scripts)
    }
}

// ─── ZeroClaw skills registry (bare-name git repo) ──────────────────────────

pub struct ZeroClawSkillsRegistry {
    pub workspace_dir: PathBuf,
    pub registry_url: Option<String>,
}

impl SkillRegistry for ZeroClawSkillsRegistry {
    fn name(&self) -> &str {
        "zeroclaw-skills"
    }

    fn matches_source(&self, source: &str) -> bool {
        super::is_registry_source(source)
    }

    fn search(&self, query: &str) -> Result<Vec<SkillSearchResult>> {
        let registry_dir =
            super::ensure_skills_registry(&self.workspace_dir, self.registry_url.as_deref())?;
        let names = super::list_registry_skill_names(&registry_dir);
        let query_lower = query.to_lowercase();
        Ok(names
            .into_iter()
            .filter(|n| n.to_lowercase().contains(&query_lower))
            .map(|n| SkillSearchResult {
                source_url: n.clone(),
                name: n,
                description: String::new(),
                registry: "zeroclaw-skills".into(),
                version: None,
            })
            .collect())
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        super::install_registry_skill_source(
            source,
            skills_path,
            allow_scripts,
            &self.workspace_dir,
            self.registry_url.as_deref(),
        )
    }
}

// ─── Registry dispatcher ────────────────────────────────────────────────────

pub struct RegistryDispatcher {
    registries: Vec<Box<dyn SkillRegistry>>,
}

impl RegistryDispatcher {
    pub fn from_config(
        skills_config: &zeroclaw_config::schema::SkillsConfig,
        workspace_dir: &Path,
    ) -> Self {
        let mut registries: Vec<Box<dyn SkillRegistry>> = vec![
            Box::new(ClawhubRegistry),
            Box::new(AgentSkillsIoRegistry),
            Box::new(SkillsShRegistry),
            Box::new(GitHubSkillRegistry),
            Box::new(GitRegistry),
            Box::new(ZeroClawSkillsRegistry {
                workspace_dir: workspace_dir.to_path_buf(),
                registry_url: skills_config.registry_url.clone(),
            }),
        ];

        for ext in &skills_config.extra_registries {
            if !ext.enabled {
                continue;
            }
            registries.push(Box::new(CustomHttpRegistry {
                reg_name: ext.name.clone(),
                base_url: ext.url.clone(),
            }));
        }

        Self { registries }
    }

    pub fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Option<Result<(PathBuf, usize)>> {
        for reg in &self.registries {
            if reg.matches_source(source) {
                // HTTP-based registries use reqwest::blocking, which spins up
                // its own tokio runtime. Dropping that nested runtime panics
                // when this function is called from inside an existing async
                // context (e.g. the CLI's #[tokio::main]). block_in_place
                // detaches the current worker thread so nested-runtime drop
                // is allowed. is_in_async_runtime gates this so tests and
                // sync callers aren't forced through the wrapper.
                return Some(call_blocking(|| {
                    reg.install(source, skills_path, allow_scripts)
                }));
            }
        }
        None
    }

    pub fn search(&self, query: &str) -> Vec<SkillSearchResult> {
        let mut results = Vec::new();
        for reg in &self.registries {
            // Same async-runtime concern as `install` — see comment there.
            let outcome = call_blocking(|| reg.search(query));
            match outcome {
                Ok(mut r) => results.append(&mut r),
                Err(e) => {
                    tracing::warn!("search failed for {}: {e}", reg.name());
                }
            }
        }
        results
    }
}

/// Run blocking I/O work, isolated from any enclosing tokio runtime.
///
/// Inside a multi-thread tokio runtime we use `block_in_place` so that the
/// dropped `reqwest::blocking` runtime doesn't trip the "cannot drop a
/// runtime in async context" panic. Outside a runtime (sync callers, unit
/// tests) we just run the closure directly.
fn call_blocking<R>(f: impl FnOnce() -> R) -> R {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

// ─── Custom HTTP registry (user-configured) ─────────────────────────────────

struct CustomHttpRegistry {
    reg_name: String,
    base_url: String,
}

impl SkillRegistry for CustomHttpRegistry {
    fn name(&self) -> &str {
        &self.reg_name
    }

    fn matches_source(&self, source: &str) -> bool {
        if let Ok(parsed) = reqwest::Url::parse(source)
            && let Some(host) = parsed.host_str()
            && let Ok(base) = reqwest::Url::parse(&self.base_url)
            && let Some(base_host) = base.host_str()
        {
            return host.eq_ignore_ascii_case(base_host);
        }
        false
    }

    fn search(&self, query: &str) -> Result<Vec<SkillSearchResult>> {
        let search_url = format!("{}/search", self.base_url.trim_end_matches('/'));
        search_http_json_registry(&search_url, query, &self.reg_name)
    }

    fn install(
        &self,
        source: &str,
        skills_path: &Path,
        allow_scripts: bool,
    ) -> Result<(PathBuf, usize)> {
        let parsed = reqwest::Url::parse(source)
            .with_context(|| format!("invalid URL for {}: {source}", self.reg_name))?;
        let slug = parsed
            .path_segments()
            .into_iter()
            .flatten()
            .rfind(|s| !s.is_empty())
            .unwrap_or("skill");
        let dir_name = normalize_skill_name(slug);
        let download_url = format!(
            "{}/download?slug={}",
            self.base_url.trim_end_matches('/'),
            slug
        );
        install_http_zip_skill(
            &download_url,
            &dir_name,
            skills_path,
            allow_scripts,
            &self.reg_name,
        )
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Normalize a registry-derived slug into a filesystem-safe skill directory
/// name.
///
/// **Preserves hyphens** so the on-disk directory matches the install slug
/// the user typed — `skills install clawhub:foo-bar` lands in
/// `skills/foo-bar/`, not `skills/foo_bar/`. Without this, subsequent
/// `skills audit foo-bar` and `skills remove foo-bar` invocations couldn't
/// find the skill the user just installed.
fn normalize_skill_name(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clawhub_matches() {
        let r = ClawhubRegistry;
        assert!(r.matches_source("clawhub:my-skill"));
        assert!(r.matches_source("https://clawhub.ai/user/skill"));
        assert!(!r.matches_source("agentskills:foo"));
        assert!(!r.matches_source("https://agentskills.io/foo"));
    }

    #[test]
    fn clawhub_accepts_owner_slug_with_slash_in_prefix_form() {
        // Real ClawHub slugs are owner/name, e.g. `pskoett/self-improving-agent`.
        // The previous parser hardcoded `slug.contains('/')` as invalid.
        let url = ClawhubRegistry::download_url("clawhub:pskoett/self-improving-agent").unwrap();
        assert_eq!(
            url,
            "https://clawhub.ai/api/v1/download?slug=pskoett/self-improving-agent"
        );
    }

    #[test]
    fn clawhub_accepts_bare_owner_slug() {
        let r = ClawhubRegistry;
        assert!(r.matches_source("pskoett/self-improving-agent"));
        let url = ClawhubRegistry::download_url("pskoett/self-improving-agent").unwrap();
        assert_eq!(
            url,
            "https://clawhub.ai/api/v1/download?slug=pskoett/self-improving-agent"
        );
        let dir = ClawhubRegistry::skill_dir_name("pskoett/self-improving-agent").unwrap();
        // Hyphens are now preserved so the on-disk dir matches the install
        // slug, allowing `skills audit/remove self-improving-agent` to
        // resolve to the same directory after install.
        assert_eq!(dir, "self-improving-agent");
    }

    #[test]
    fn clawhub_rejects_three_segment_paths() {
        // path/to/something is a local path, not a ClawHub shorthand.
        let r = ClawhubRegistry;
        assert!(!r.matches_source("path/to/something"));
        assert!(!r.matches_source("./local/path"));
        assert!(!r.matches_source("../other/skill"));
        assert!(!r.matches_source("/abs/path"));
    }

    #[test]
    fn agentskills_matches() {
        let r = AgentSkillsIoRegistry;
        assert!(r.matches_source("agentskills:my-skill"));
        assert!(r.matches_source("https://agentskills.io/skills/my-skill"));
        assert!(!r.matches_source("clawhub:foo"));
        assert!(!r.matches_source("skillssh:foo"));
    }

    #[test]
    fn skillssh_matches() {
        let r = SkillsShRegistry;
        assert!(r.matches_source("skillssh:my-skill"));
        assert!(r.matches_source("https://skills.sh/skills/my-skill"));
        assert!(!r.matches_source("clawhub:foo"));
        assert!(!r.matches_source("agentskills:foo"));
    }

    #[test]
    fn git_registry_excludes_known_registries() {
        let r = GitRegistry;
        assert!(!r.matches_source("clawhub:foo"));
        assert!(!r.matches_source("agentskills:foo"));
        assert!(!r.matches_source("skillssh:foo"));
        assert!(!r.matches_source("https://clawhub.ai/user/skill"));
        assert!(!r.matches_source("https://agentskills.io/foo"));
        assert!(!r.matches_source("https://skills.sh/foo"));
    }

    #[test]
    fn normalize_names_preserves_hyphens() {
        // Hyphens are now preserved so the on-disk dir matches the install
        // slug. Previously this asserted "my_skill" — that broke
        // `skills audit my-skill` and `skills remove my-skill` after a
        // ClawhHub install of `clawhub:my-skill`.
        assert_eq!(normalize_skill_name("My-Skill"), "my-skill");
        assert_eq!(normalize_skill_name("web_scraper"), "web_scraper");
        assert_eq!(
            normalize_skill_name("acong-hello-world"),
            "acong-hello-world"
        );
        assert_eq!(normalize_skill_name("foo/bar"), "foobar");
        // Underscores and lowercasing are still applied.
        assert_eq!(normalize_skill_name("FOO_BAR"), "foo_bar");
        // Other punctuation is still stripped.
        assert_eq!(normalize_skill_name("a.b@c"), "abc");
    }

    #[test]
    fn agentskills_install_validates_three_segment_format() {
        // The pre-fix routing silently translated `agentskills:hello-world`
        // into `skillssh:hello-world` and then leaked the skills.sh-shaped
        // error to the user. Now we validate up front and produce an
        // agentskills-shaped message.
        let tmp = tempfile::TempDir::new().unwrap();
        let r = AgentSkillsIoRegistry;

        let err = r
            .install("agentskills:hello-world", tmp.path(), false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("invalid agentskills.io source"),
            "expected agentskills-shaped error, got: {err}"
        );
        assert!(
            err.contains("<owner>/<repo>/<skill>"),
            "expected format hint in error, got: {err}"
        );
        assert!(
            !err.contains("skillssh:"),
            "internal skillssh: translation should not leak into error: {err}"
        );

        // Two-segment input is also invalid.
        let err = r
            .install("agentskills:foo/bar", tmp.path(), false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("invalid agentskills.io source") && err.contains("2 segment"),
            "expected count in error, got: {err}"
        );
    }
}
