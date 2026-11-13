use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const REGISTRY_CACHE_DIR_NAME: &str = "plugin-registry";
pub const REGISTRY_CACHE_FILE_NAME: &str = "registry.json";

/// Metadata entry from an installable plugin registry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRegistryEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Installable plugin registry index.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRegistryIndex {
    #[serde(default)]
    pub plugins: Vec<PluginRegistryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_url: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PluginSpec {
    pub name: String,
    pub version: Option<String>,
}

pub fn search_entries<'a>(
    index: &'a PluginRegistryIndex,
    query: &str,
) -> Vec<&'a PluginRegistryEntry> {
    let query = query.to_lowercase();
    index
        .plugins
        .iter()
        .filter(|entry| {
            entry.name.to_lowercase().contains(&query)
                || entry
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        })
        .collect()
}

pub fn registry_cache_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join(REGISTRY_CACHE_DIR_NAME)
        .join(REGISTRY_CACHE_FILE_NAME)
}

pub fn install_source(entry: &PluginRegistryEntry) -> String {
    format!("{}@{}", entry.name, entry.version)
}

pub fn install_command(entry: &PluginRegistryEntry, registry_url: Option<&str>) -> String {
    let source = install_source(entry);
    match registry_url {
        Some(registry_url) if !registry_url.trim().is_empty() => {
            format!("zeroclaw plugin install {source} --registry {registry_url}")
        }
        _ => format!("zeroclaw plugin install {source}"),
    }
}

pub fn parse_plugin_spec(source: &str) -> Result<PluginSpec> {
    let source = source.trim();
    if source.is_empty() {
        bail!("plugin name must not be empty");
    }
    // A package name cannot contain `@` and a registry version can, so the
    // name ends at the first one: `name@version` parses back to the entry it
    // was printed from, whatever the version holds.
    let Some((name, version)) = source.split_once('@') else {
        return Ok(PluginSpec {
            name: source.to_string(),
            version: None,
        });
    };
    if name.is_empty() || version.is_empty() {
        bail!("plugin registry source must be name or name@version");
    }
    Ok(PluginSpec {
        name: name.to_string(),
        version: Some(version.to_string()),
    })
}

pub fn resolve_entry<'a>(
    index: &'a PluginRegistryIndex,
    spec: &PluginSpec,
) -> Result<&'a PluginRegistryEntry> {
    let mut matches = index
        .plugins
        .iter()
        .filter(|entry| entry.name == spec.name)
        .filter(|entry| {
            spec.version
                .as_ref()
                .is_none_or(|version| &entry.version == version)
        });
    matches
        .next_back()
        .ok_or_else(|| anyhow::Error::msg(format!("plugin '{}' not found in registry", spec.name)))
}

/// Resolve the install-default entry for every distinct registry package.
///
/// Version selection delegates to [`resolve_entry`], keeping unpinned install
/// and catalog views on the same rule: the last matching entry in the index.
#[must_use]
pub fn resolved_entries(index: &PluginRegistryIndex) -> Vec<&PluginRegistryEntry> {
    let mut seen = std::collections::HashSet::new();
    index
        .plugins
        .iter()
        .filter_map(|entry| {
            if !seen.insert(entry.name.as_str()) {
                return None;
            }
            resolve_entry(
                index,
                &PluginSpec {
                    name: entry.name.clone(),
                    version: None,
                },
            )
            .ok()
        })
        .collect()
}

pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw-plugins/main/registry.json";
pub const REGISTRY_URL_ENV: &str = "ZEROCLAW_PLUGIN_REGISTRY_URL";

/// A single URL remains the legacy representation; lists are JSON, never CSV.
pub fn registry_sources(value: &str) -> Result<Vec<String>> {
    let values = if value.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<String>>(value).context("parsing plugin registry URL list")?
    } else {
        vec![value.to_string()]
    };
    let mut sources = Vec::new();
    for value in values {
        if value.trim().is_empty()
            || !(value
                .get(..8)
                .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
                || value
                    .get(..7)
                    .is_some_and(|s| s.eq_ignore_ascii_case("http://")))
            || value.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            bail!("plugin registry must be an HTTP(S) URL: {value}");
        }
        if !sources.contains(&value) {
            sources.push(value);
        }
    }
    if sources.is_empty() || sources.len() > 8 {
        bail!("plugin registry list must contain one to eight unique URLs");
    }
    Ok(sources)
}

/// Canonical origin-aware discovery/cache, additive to the legacy index API.
/// Empty successful indexes remain represented for exact source-set matching.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryCollection {
    pub registries: Vec<PluginRegistryIndex>,
}
#[derive(Clone, Copy, Debug)]
pub struct RegistryEntryView<'a> {
    pub origin: &'a str,
    pub entry: &'a PluginRegistryEntry,
}
impl std::ops::Deref for RegistryEntryView<'_> {
    type Target = PluginRegistryEntry;
    fn deref(&self) -> &Self::Target {
        self.entry
    }
}
impl RegistryCollection {
    pub fn entries(&self) -> Vec<RegistryEntryView<'_>> {
        self.registries
            .iter()
            .flat_map(|index| {
                index.plugins.iter().map(move |entry| RegistryEntryView {
                    origin: index
                        .registry_url
                        .as_deref()
                        .unwrap_or(DEFAULT_REGISTRY_URL),
                    entry,
                })
            })
            .collect()
    }
    pub fn sources(&self) -> Vec<String> {
        self.registries
            .iter()
            .map(|index| {
                index
                    .registry_url
                    .clone()
                    .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string())
            })
            .collect()
    }
}
impl From<PluginRegistryIndex> for RegistryCollection {
    fn from(index: PluginRegistryIndex) -> Self {
        Self {
            registries: vec![index],
        }
    }
}
pub fn search_registry_entries<'a>(
    catalog: &'a RegistryCollection,
    query: &str,
) -> Vec<RegistryEntryView<'a>> {
    catalog
        .registries
        .iter()
        .flat_map(|index| {
            search_entries(index, query)
                .into_iter()
                .map(move |entry| RegistryEntryView {
                    origin: index
                        .registry_url
                        .as_deref()
                        .unwrap_or(DEFAULT_REGISTRY_URL),
                    entry,
                })
        })
        .collect()
}
pub fn resolve_registry_entry<'a>(
    catalog: &'a RegistryCollection,
    spec: &PluginSpec,
) -> Result<RegistryEntryView<'a>> {
    let origins: Vec<_> = catalog
        .registries
        .iter()
        .filter(|index| index.plugins.iter().any(|entry| entry.name == spec.name))
        .collect();
    if origins.len() > 1 {
        bail!(
            "plugin '{}' is listed by multiple registries; select one with --registry <url>",
            spec.name
        );
    }
    let index = *origins.first().ok_or_else(|| {
        anyhow::Error::msg(format!("plugin '{}' not found in registry", spec.name))
    })?;
    Ok(RegistryEntryView {
        origin: index
            .registry_url
            .as_deref()
            .unwrap_or(DEFAULT_REGISTRY_URL),
        entry: resolve_entry(index, spec)?,
    })
}
pub fn resolved_registry_entries(catalog: &RegistryCollection) -> Vec<RegistryEntryView<'_>> {
    catalog
        .registries
        .iter()
        .flat_map(|index| {
            resolved_entries(index)
                .into_iter()
                .map(move |entry| RegistryEntryView {
                    origin: index
                        .registry_url
                        .as_deref()
                        .unwrap_or(DEFAULT_REGISTRY_URL),
                    entry,
                })
        })
        .collect()
}
pub fn cache_matches_sources(catalog: &RegistryCollection, sources: &[String]) -> bool {
    let cached = catalog.sources();
    cached.len() == sources.len() && cached.iter().all(|source| sources.contains(source))
}
pub fn cache_matches_environment(catalog: &RegistryCollection) -> bool {
    match std::env::var(REGISTRY_URL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        Some(value) => {
            registry_sources(&value).is_ok_and(|sources| cache_matches_sources(catalog, &sources))
        }
        None => true,
    }
}
fn validate_collection(catalog: &RegistryCollection) -> Result<()> {
    let sources = catalog.sources();
    let parsed = registry_sources(&serde_json::to_string(&sources)?)?;
    if parsed.len() != sources.len() {
        bail!("duplicate cached registry origin");
    }
    for index in &catalog.registries {
        if index
            .registry_url
            .as_deref()
            .is_some_and(|origin| origin.trim_start().starts_with('['))
        {
            bail!("each cached index must have exactly one origin");
        }
    }
    Ok(())
}
pub fn read_cached_registry_collection(data_dir: &Path) -> Result<Option<RegistryCollection>> {
    let path = registry_cache_path(data_dir);
    let json = match std::fs::read_to_string(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading cached plugin registry {}", path.display()));
        }
    };
    let value: serde_json::Value = serde_json::from_str(&json)
        .with_context(|| format!("parsing cached plugin registry {}", path.display()))?;
    let catalog = if value.get("registries").is_some() {
        serde_json::from_value::<RegistryCollection>(value)?
    } else {
        RegistryCollection::from(serde_json::from_value::<PluginRegistryIndex>(value)?)
    };
    validate_collection(&catalog)?;
    Ok(Some(catalog))
}
/// Legacy callers must never mistake a union for one publisher.
pub fn read_cached_registry_index(data_dir: &Path) -> Result<Option<PluginRegistryIndex>> {
    match read_cached_registry_collection(data_dir)? {
        Some(mut catalog) if catalog.registries.len() == 1 => Ok(catalog.registries.pop()),
        Some(_) => bail!("multi-origin cache requires the origin-aware registry reader"),
        None => Ok(None),
    }
}
pub fn write_cached_registry_index(
    data_dir: &Path,
    registry_url: &str,
    index: &PluginRegistryIndex,
) -> Result<()> {
    if registry_sources(registry_url)?.len() != 1 || registry_url.trim_start().starts_with('[') {
        bail!("legacy cache writer expects one registry URL");
    }
    let mut index = index.clone();
    index.registry_url = Some(registry_url.to_string());
    write_cached_registry_collection(data_dir, &RegistryCollection::from(index))
}
pub fn write_cached_registry_collection(
    data_dir: &Path,
    catalog: &RegistryCollection,
) -> Result<()> {
    validate_collection(catalog)?;
    let path = registry_cache_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = if catalog.registries.len() == 1 {
        serde_json::to_string_pretty(&catalog.registries[0])?
    } else {
        serde_json::to_string_pretty(catalog)?
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let staged = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    write_registry_cache(&path, &staged, |file| {
        use std::io::Write;
        file.write_all(json.as_bytes())
    })
}

fn write_registry_cache(
    path: &Path,
    staged: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<()> {
    // Armed only after exclusive creation. Drop the file before this guard so
    // error cleanup also works on platforms that cannot unlink an open file.
    struct OwnedStage<'a>(&'a Path);
    impl Drop for OwnedStage<'_> {
        fn drop(&mut self) {
            // Cleanup is best effort; never replace the original I/O error.
            let _ = std::fs::remove_file(self.0);
        }
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // Preserve restrictive existing privacy, never widening past owner-only.
        let mode = std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o600)
            .unwrap_or(0o600);
        options.mode(mode);
    }
    let cleanup;
    let mut file = options
        .open(staged)
        .context("staging private plugin registry cache")?;
    cleanup = OwnedStage(staged);
    write(&mut file)?;
    drop(file);
    std::fs::rename(staged, path).context("publishing plugin registry cache")?;
    // The rename consumed our stage; do not remove a later writer at that path.
    std::mem::forget(cleanup);
    Ok(())
}
/// No configured credential bytes enter successful labels; the full digest
/// also distinguishes two same-host URLs differing only in credential data.
pub fn registry_origin_label(origin: &str) -> String {
    format!(
        "registry:{}",
        crate::signature::sha256_hex(origin.as_bytes())
    )
}
/// Raw arguments are quoted only once, for the operator's host shell.
/// Returns `None` when a Windows copy-ready command cannot preserve every
/// argument in both cmd.exe and PowerShell. This does not reject registry data
/// or prevent structured installation. Explicit PowerShell is shell-specific.
pub fn registry_install_command_for(
    dialect: ShellDialect,
    entry: &PluginRegistryEntry,
    origin: Option<&str>,
    config_dir: Option<&Path>,
    no_verify: bool,
) -> Option<String> {
    let spec = install_source(entry);
    let dir = config_dir.map(|path| path.to_string_lossy().into_owned());
    let mut values = vec![spec.as_str()];
    if let Some(origin) = origin {
        values.push(origin);
    }
    if let Some(dir) = dir.as_deref() {
        values.push(dir);
    }
    // A marked PowerShell recipe is not inert in cmd.exe: separators can run.
    if dialect == ShellDialect::Windows
        && !values.iter().all(|value| {
            windows_form_is_literal(value)
                // PowerShell also treats these smart double quotes as string
                // delimiters, even when the opening delimiter is ASCII.
                && !value.chars().any(|c| matches!(c, '\u{201c}' | '\u{201d}' | '\u{201e}'))
        })
    {
        return None;
    }
    let arg = |raw: &str| {
        if raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._/@:+,=".contains(c))
            && !raw.is_empty()
        {
            raw.to_string()
        } else {
            dialect.quote_literal(raw)
        }
    };
    let mut command = "zeroclaw".to_string();
    if let Some(dir) = dir.as_deref() {
        command.push_str(&format!(" --config-dir {}", dialect.quote_literal(dir)));
    }
    command.push_str(&format!(" plugin install {}", arg(&spec)));
    if let Some(origin) = origin {
        command.push_str(&format!(" --registry {}", arg(origin)));
    }
    if no_verify {
        command.push_str(" --no-verify");
    }
    Some(command)
}
/// The quoting dialect of the shell an operator pastes a printed command into.
///
/// The commands this module renders are copied out of `zeroclaw plugin install`
/// and `zeroclaw plugin list` output and pasted into the operator's interactive
/// shell, so a value is quoted for *that* shell, not for the shell the runtime
/// uses to execute tools. ZeroClaw cannot see which shell its output lands in.
/// On Linux and macOS every supported shell shares the POSIX single-quote
/// form. On Windows the operator may be in either of two shells, `cmd.exe`
/// (the default the native runtime documents) or PowerShell (the default
/// Windows Terminal opens), so the Windows form has to be literal in both, and
/// a value that is literal in neither is refused rather than trusted to
/// whichever shell happens to be open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellDialect {
    /// `sh`, `bash`, `zsh`, `fish`: single quotes protect every character but
    /// the single quote itself, which is closed, escaped and reopened
    /// (`'it'\''s'`).
    Posix,
    /// PowerShell (`powershell`, `pwsh`) on its own: single quotes protect
    /// every character but the single quote itself, which is doubled
    /// (`'it''s'`). Only suitable when the receiving shell is known to be
    /// PowerShell; it is not a copy-ready Windows console form.
    PowerShell,
    /// A Windows console, whichever of `cmd.exe` and PowerShell is running it:
    /// one double-quoted argument, which both shells hand to the native binary
    /// untouched as long as the value holds nothing either of them expands
    /// inside double quotes. [`windows_form_is_literal`] is that test.
    Windows,
}

/// Whether `raw` survives a Windows console as one double-quoted argument, in
/// both `cmd.exe` and PowerShell.
///
/// `cmd.exe` has no fully literal quoting form: inside double quotes it still
/// expands `%name%`, expands `!name!` under delayed expansion, cannot escape an
/// embedded `"`, and treats a single quote as an ordinary character.
/// PowerShell's double quotes interpolate `$` and the backtick. Both shells
/// then hand the argument to the native binary through the C runtime's
/// command-line rules, where a trailing backslash swallows the closing quote.
/// A value is literal in both shells only when it contains none of `"`, `%`,
/// `!`, `$` and `` ` ``, no control character, and does not end in `\`.
///
/// Every declared host of the ordinary shape (`api.example.com`,
/// `*.cdn.example.com`) and every ordinary profile path, spaces included,
/// passes. A manifest that declares `$(id).example.com` does not: the egress
/// grammar accepts it, and no Windows form could promise it reaches
/// `config set` unexpanded, so `egress_set_command` refuses it instead.
#[must_use]
pub fn windows_form_is_literal(raw: &str) -> bool {
    !raw.ends_with('\\')
        && !raw
            .chars()
            .any(|c| matches!(c, '"' | '%' | '!' | '$' | '`') || c.is_control())
}

/// Legacy advisory prefix for a shell-specific PowerShell command.
///
/// WARNING: this is not an execution guard. cmd.exe does not recognize `#` as
/// a comment, and separators later in the line can execute. Copy-ready registry
/// commands return `None` instead of using this fallback.
pub const POWERSHELL_ONLY_MARKER: &str =
    "# PowerShell only, cmd.exe cannot pass this value literally: ";

impl ShellDialect {
    /// The dialect of the operator's shell on the host this binary runs on:
    /// the Windows console form on Windows, POSIX everywhere else.
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    /// Quote `raw` as one literal argument in this dialect: after the shell has
    /// parsed the result, the argument is `raw`, byte for byte.
    ///
    /// For [`Self::Windows`] that promise holds only while
    /// [`windows_form_is_literal`] does; a value outside it is rendered in the
    /// [`Self::PowerShell`] form. That fallback is not safe in cmd.exe;
    /// copy-ready callers must refuse unsupported Windows values first.
    #[must_use]
    pub fn quote_literal(self, raw: &str) -> String {
        match self {
            Self::Posix => format!("'{}'", raw.replace('\'', "'\\''")),
            Self::PowerShell => format!("'{}'", raw.replace('\'', "''")),
            Self::Windows if windows_form_is_literal(raw) => format!("\"{raw}\""),
            Self::Windows => Self::PowerShell.quote_literal(raw),
        }
    }

    /// The dialect a whole command line carrying `values` is rendered in, and
    /// the marker it starts with.
    ///
    /// Every dialect but [`Self::Windows`] renders itself, unmarked. A Windows
    /// line renders itself only when every value passes
    /// [`windows_form_is_literal`]; otherwise the line is the PowerShell form
    /// behind [`POWERSHELL_ONLY_MARKER`]. This is legacy advisory labeling,
    /// not a safety boundary: cmd.exe may execute separators in that line.
    #[must_use]
    pub fn command_form(self, values: &[&str]) -> (Self, &'static str) {
        match self {
            Self::Windows if !values.iter().all(|value| windows_form_is_literal(value)) => {
                (Self::PowerShell, POWERSHELL_ONLY_MARKER)
            }
            other => (other, ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_collection_preserves_empty_sources_exact_sets_and_legacy_reader_guard() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = sample_index();
        a.registry_url = Some("hTtPs://a.invalid/index".into());
        let b = PluginRegistryIndex {
            plugins: vec![],
            registry_url: Some("HTTPS://b.invalid/index".into()),
        };
        let mut catalog = RegistryCollection {
            registries: vec![a, b],
        };
        write_cached_registry_collection(dir.path(), &catalog).unwrap();
        let cached = read_cached_registry_collection(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(cached, catalog);
        assert!(read_cached_registry_index(dir.path()).is_err());
        assert!(cache_matches_sources(&cached, &cached.sources()));
        assert!(!cache_matches_sources(&cached, &cached.sources()[..1]));
        let path = registry_cache_path(dir.path());
        let before = std::fs::read(&path).unwrap();
        assert!(
            write_cached_registry_collection(
                dir.path(),
                &RegistryCollection { registries: vec![] }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
        catalog.registries[1].plugins = vec![catalog.registries[0].plugins[0].clone()];
        assert_eq!(search_registry_entries(&catalog, "team-calendar").len(), 3);
        assert!(
            resolve_registry_entry(&catalog, &parse_plugin_spec("team-calendar@0.1.0").unwrap())
                .is_err()
        );
        let rows = crate::catalog::origin_package_catalog(&[], Some(&catalog));
        assert_eq!(
            rows.iter()
                .filter(|row| row.name() == "team-calendar")
                .count(),
            2
        );
        for row in rows {
            assert!(row.origin.is_some());
        }
    }

    #[test]
    fn mixed_case_legacy_cache_and_public_command_helpers_remain_compatible() {
        let dir = tempfile::tempdir().unwrap();
        let index = sample_index();
        let origin = "HTTPS://Example.invalid/index?channel=stable";
        write_cached_registry_index(dir.path(), origin, &index).unwrap();
        let legacy = read_cached_registry_index(dir.path()).unwrap().unwrap();
        assert_eq!(legacy.registry_url.as_deref(), Some(origin));
        assert_eq!(legacy.plugins, index.plugins);
        assert!(install_command(&legacy.plugins[0], Some(origin)).ends_with(origin));
        assert!(registry_sources("hTtP://example.invalid/index").is_ok());
    }

    #[test]
    fn cache_staging_collision_preserves_foreign_file_and_canonical_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let staged = dir.path().join("collision.tmp");
        std::fs::write(&path, b"last good cache").unwrap();
        std::fs::write(&staged, b"another writer stage").unwrap();

        let error = write_registry_cache(&path, &staged, |_| {
            panic!("exclusive creation must fail before writing")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "staging private plugin registry cache");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"last good cache");
        assert_eq!(std::fs::read(&staged).unwrap(), b"another writer stage");
    }

    #[test]
    fn cache_staging_partial_write_failure_preserves_canonical_cache() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let staged = dir.path().join("owned.tmp");
        std::fs::write(&path, b"last good cache").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }

        let error = write_registry_cache(&path, &staged, |file| {
            file.write_all(b"partial new cache")?;
            assert_eq!(std::fs::read(&staged).unwrap(), b"partial new cache");
            Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "injected cache write failure",
            ))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "injected cache write failure");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::WriteZero
        );
        assert!(!staged.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"last good cache");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o400
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn cache_staging_rename_failure_removes_only_owned_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = registry_cache_path(dir.path());
        // A directory at the destination makes the real publication rename fail.
        std::fs::create_dir_all(&path).unwrap();
        let sentinel = path.join("last-good");
        std::fs::write(&sentinel, b"keep canonical contents").unwrap();
        let foreign = path.with_extension("foreign.tmp");
        std::fs::write(&foreign, b"another writer stage").unwrap();

        let error = write_cached_registry_index(
            dir.path(),
            "https://example.invalid/index",
            &sample_index(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "publishing plugin registry cache");
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        assert_eq!(std::fs::read(sentinel).unwrap(), b"keep canonical contents");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"another writer stage");
        let mut remaining: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        remaining.sort();
        let mut expected = vec![path, foreign];
        expected.sort();
        assert_eq!(
            remaining, expected,
            "owned staging file leaked after rename failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_refresh_never_widens_an_existing_private_cache() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write_cached_registry_index(dir.path(), "https://example.invalid/index", &sample_index())
            .unwrap();
        let path = registry_cache_path(dir.path());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for mode in [0o600, 0o400] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            write_cached_registry_index(
                dir.path(),
                "https://user:secret@example.invalid/index?token=secret",
                &sample_index(),
            )
            .unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert_eq!(
                read_cached_registry_index(dir.path())
                    .unwrap()
                    .unwrap()
                    .plugins,
                sample_index().plugins
            );
            let remaining: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(remaining, vec![path.clone()]);
        }
    }

    #[test]
    fn raw_registry_arguments_render_for_each_supported_shell() {
        let entry = &sample_index().plugins[0];
        let origin = "https://registry.example/index?channel=stable&team=one";
        let posix =
            registry_install_command_for(ShellDialect::Posix, entry, Some(origin), None, false)
                .unwrap();
        assert!(posix.ends_with(&format!("--registry '{origin}'")));
        let windows = registry_install_command_for(
            ShellDialect::Windows,
            entry,
            Some(origin),
            Some(Path::new("C:/profiles/team one")),
            true,
        )
        .unwrap();
        assert_eq!(
            windows,
            format!(
                r#"zeroclaw --config-dir "C:/profiles/team one" plugin install {} --registry "{origin}" --no-verify"#,
                install_source(entry),
            )
        );
        assert!(
            !registry_install_command_for(ShellDialect::Windows, entry, Some(origin), None, false,)
                .unwrap()
                .contains("--no-verify")
        );
        assert_eq!(
            registry_install_command_for(
                ShellDialect::Windows,
                entry,
                Some(origin),
                Some(Path::new("C:/profiles/100%team")),
                false,
            ),
            None
        );
        let tricky = "https://registry.example/index?team=it's%20mine";
        let powershell =
            registry_install_command_for(ShellDialect::PowerShell, entry, Some(tricky), None, true)
                .unwrap();
        assert!(powershell.contains("team=it''s%20mine'"));
        assert!(powershell.ends_with(" --no-verify"));
        assert_eq!(
            registry_install_command_for(ShellDialect::Windows, entry, Some(tricky), None, true,),
            None
        );
    }

    #[test]
    fn registry_copy_ready_boundary_refuses_unsafe_windows_values_only() {
        let ordinary = sample_index().plugins.remove(0);
        for raw in [
            "1%PATH% & echo ZC_REGISTRY_MARKER",
            "1!PATH!",
            r#"1" & echo ZC_REGISTRY_MARKER"#,
            "1$HOME",
            "1`echo ZC_REGISTRY_MARKER`",
            "1\u{201c}",
            "1\u{201d}",
            "1\u{201e}",
            "1”; echo ZC_REGISTRY_HARMLESS_MARKER_12216; echo “",
            "1\rnext",
            "1\nnext",
            "1\\",
        ] {
            let mut entry = ordinary.clone();
            entry.version = raw.into();
            let mut named_entry = ordinary.clone();
            named_entry.name = raw.into();
            for dialect in [ShellDialect::Posix, ShellDialect::PowerShell] {
                assert!(
                    registry_install_command_for(dialect, &named_entry, None, None, true).is_some()
                );
            }
            // A name's final backslash becomes internal when @version is appended.
            if !raw.ends_with('\\') {
                assert_eq!(
                    registry_install_command_for(
                        ShellDialect::Windows,
                        &named_entry,
                        None,
                        None,
                        true
                    ),
                    None,
                    "name {raw:?}"
                );
            }
            for (entry, origin, dir) in [
                (&entry, None, None),
                (&ordinary, Some(raw), None),
                (&ordinary, None, Some(Path::new(raw))),
            ] {
                assert_eq!(
                    registry_install_command_for(ShellDialect::Windows, entry, origin, dir, true,),
                    None,
                    "{raw:?}"
                );
                for dialect in [ShellDialect::Posix, ShellDialect::PowerShell] {
                    assert!(
                        registry_install_command_for(dialect, entry, origin, dir, true).is_some()
                    );
                }
            }
        }
        let mut named_entry = ordinary.clone();
        named_entry.name = "calendar\\".into();
        named_entry.version = "1".into();
        assert_eq!(
            registry_install_command_for(ShellDialect::Windows, &named_entry,
                Some("https://registry.invalid/index?team=one&channel=stable"),
                Some(Path::new("C:/profiles/team one")), true),
            Some(r#"zeroclaw --config-dir "C:/profiles/team one" plugin install "calendar\@1" --registry "https://registry.invalid/index?team=one&channel=stable" --no-verify"#.into())
        );
        // Separators, caret and parentheses remain data inside the shared
        // double-quoted subset. They do not by themselves require suppression.
        for raw in [
            "1 & echo ZC_REGISTRY_MARKER",
            "1|echo ZC_REGISTRY_MARKER",
            "1;echo ZC_REGISTRY_MARKER",
            "1^(team)",
            "1's team",
            "1café日历",
        ] {
            let mut entry = ordinary.clone();
            entry.version = raw.into();
            let command =
                registry_install_command_for(ShellDialect::Windows, &entry, None, None, false)
                    .unwrap();
            assert!(command.contains(&format!(r#""{}""#, install_source(&entry))));
        }
    }

    #[test]
    fn new_origin_labels_are_nonempty_private_and_distinguishable() {
        let a = "https://alice:password-a@same.invalid/index?token=secret-a#private-a";
        let b = "https://alice:password-b@same.invalid/index?token=secret-b#private-b";
        let a_label = registry_origin_label(a);
        let b_label = registry_origin_label(b);
        assert_ne!(a_label, b_label);
        for label in [a_label, b_label] {
            assert!(!label.is_empty());
            for secret in [
                "alice",
                "password",
                "token",
                "secret",
                "private",
                "same.invalid",
            ] {
                assert!(!label.contains(secret));
            }
        }
    }

    fn sample_index() -> PluginRegistryIndex {
        PluginRegistryIndex {
            plugins: vec![
                PluginRegistryEntry {
                    name: "team-calendar".to_string(),
                    version: "0.1.0".to_string(),
                    description: Some("Schedule meetings".to_string()),
                    author: None,
                    capabilities: vec!["tool".to_string()],
                    url: "https://example.invalid/team-calendar-0.1.0.zip".to_string(),
                    sha256: None,
                },
                PluginRegistryEntry {
                    name: "web-research".to_string(),
                    version: "0.1.0".to_string(),
                    description: Some("Research web pages".to_string()),
                    author: None,
                    capabilities: vec!["tool".to_string()],
                    url: "https://example.invalid/web-research-0.1.0.zip".to_string(),
                    sha256: None,
                },
                PluginRegistryEntry {
                    name: "team-calendar".to_string(),
                    version: "0.2.0".to_string(),
                    description: Some("Team calendar scheduling".to_string()),
                    author: None,
                    capabilities: vec!["tool".to_string()],
                    url: "https://example.invalid/team-calendar-0.2.0.zip".to_string(),
                    sha256: None,
                },
            ],
            registry_url: None,
        }
    }

    #[test]
    fn search_matches_name_or_description() {
        let index = sample_index();

        let matches = search_entries(&index, "calendar");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].name, "team-calendar");
        assert_eq!(matches[1].version, "0.2.0");
    }

    #[test]
    fn resolved_entries_use_unpinned_install_selection() {
        let index = sample_index();

        let resolved = resolved_entries(&index);

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].name, "team-calendar");
        assert_eq!(resolved[0].version, "0.2.0");
        assert_eq!(resolved[1].name, "web-research");
    }

    #[test]
    fn resolves_pinned_version_or_latest_listed_match() {
        let index = sample_index();

        let latest = resolve_entry(
            &index,
            &PluginSpec {
                name: "team-calendar".to_string(),
                version: None,
            },
        )
        .unwrap();
        let pinned = resolve_entry(
            &index,
            &PluginSpec {
                name: "team-calendar".to_string(),
                version: Some("0.1.0".to_string()),
            },
        )
        .unwrap();

        assert_eq!(latest.version, "0.2.0");
        assert_eq!(pinned.version, "0.1.0");
        assert_eq!(
            parse_plugin_spec("team-calendar@0.2.0").unwrap(),
            PluginSpec {
                name: "team-calendar".to_string(),
                version: Some("0.2.0".to_string()),
            }
        );
    }

    #[test]
    fn a_version_holding_an_at_sign_parses_back_whole() {
        let entry = PluginRegistryEntry {
            name: "team-calendar".to_string(),
            version: "0.3.0@rc.1".to_string(),
            description: None,
            author: None,
            capabilities: vec!["tool".to_string()],
            url: "https://example.invalid/team-calendar-0.3.0.zip".to_string(),
            sha256: None,
        };

        assert_eq!(
            parse_plugin_spec(&install_source(&entry)).unwrap(),
            PluginSpec {
                name: "team-calendar".to_string(),
                version: Some("0.3.0@rc.1".to_string()),
            }
        );
        for malformed in ["@0.3.0", "team-calendar@"] {
            assert!(parse_plugin_spec(malformed).is_err(), "{malformed}");
        }
    }

    #[test]
    fn writes_and_reads_cached_registry_with_origin() {
        let dir = tempfile::tempdir().unwrap();
        let index = sample_index();

        write_cached_registry_index(dir.path(), "https://example.invalid/registry.json", &index)
            .unwrap();
        let cached = read_cached_registry_index(dir.path())
            .unwrap()
            .expect("cache should exist");

        assert_eq!(cached.plugins, index.plugins);
        assert_eq!(
            cached.registry_url.as_deref(),
            Some("https://example.invalid/registry.json")
        );
        assert!(registry_cache_path(dir.path()).is_file());
    }

    #[test]
    fn cache_absence_is_distinct_from_an_unreadable_payload() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_cached_registry_index(dir.path()).unwrap().is_none());

        let path = registry_cache_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0xff, 0xfe]).unwrap();

        let error = read_cached_registry_index(dir.path())
            .expect_err("invalid UTF-8 should be reported as a cache read failure");
        assert!(error.to_string().contains("reading cached plugin registry"));
    }
}
