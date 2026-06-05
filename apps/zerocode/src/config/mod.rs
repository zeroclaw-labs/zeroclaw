//! Local zerocode client configuration: theme and keybindings.
//!
//! Always read from the local `<config_dir>/zerocode-config.toml`, independent
//! of the connection target. Layering: defaults -> file -> `ZEROCODE_*` env.
#![allow(dead_code)]

pub mod keybindings;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::keymap::{Chord, overrides::OverrideTable};
use crate::theme::{self, Theme};

const FILE_NAME: &str = "zerocode-config.toml";
const ENV_PREFIX: &str = "ZEROCODE_";
const ENV_SEP: &str = "__";

/// One or more chords bound to an action. Accepts a bare string (one
/// chord) or an array on the wire; always serialized back as an array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ChordSpec {
    One(Chord),
    Many(Vec<Chord>),
}

impl ChordSpec {
    fn into_vec(self) -> Vec<Chord> {
        match self {
            Self::One(c) => vec![c],
            Self::Many(cs) => cs,
        }
    }
}

/// The `[theme]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ThemeSection {
    #[serde(default = "default_theme")]
    pub name: String,
    /// Per-agent theme overrides keyed by agent alias. When the Code or Chat
    /// pane is focused on an agent listed here, that agent's theme replaces
    /// the base `name` while the pane is active. Sparse: agents not listed use
    /// the base theme.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub agent_override: HashMap<String, AgentThemeOverride>,
}

/// One `[theme.agent_override.<alias>]` entry. Mirrors the `{ name }` shape of
/// the base `[theme]` section so the resolver path is identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentThemeOverride {
    pub name: String,
}

impl Default for ThemeSection {
    fn default() -> Self {
        Self {
            name: default_theme(),
            agent_override: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ConnectionSection {
    #[serde(default, skip_serializing_if = "WssSection::is_empty")]
    pub wss: WssSection,
}

impl ConnectionSection {
    fn is_empty(&self) -> bool {
        self.wss.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WssSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "WssTlsSection::is_empty")]
    pub tls: WssTlsSection,
}

impl WssSection {
    fn is_empty(&self) -> bool {
        self.uri.is_none() && self.tls.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WssTlsSection {
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_verify: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_verify_routes: Vec<String>,
}

impl WssTlsSection {
    pub fn route_acked(&self, uri: &str) -> bool {
        self.skip_verify_routes.iter().any(|r| r == uri)
    }

    fn is_empty(&self) -> bool {
        !self.skip_verify && self.skip_verify_routes.is_empty()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZerocodeConfig {
    #[serde(default = "default_locale")]
    pub locale: Option<String>,
    #[serde(default)]
    pub theme: ThemeSection,
    #[serde(default, skip_serializing_if = "ConnectionSection::is_empty")]
    pub connection: ConnectionSection,
    /// Sparse keybinding overrides keyed `"<tag>.<variant>"`. Absent
    /// entries fall back to compile-time defaults.
    #[serde(default)]
    keybindings: HashMap<String, ChordSpec>,
}

impl Default for ZerocodeConfig {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            theme: ThemeSection::default(),
            connection: ConnectionSection::default(),
            keybindings: HashMap::new(),
        }
    }
}

fn default_locale() -> Option<String> {
    Some("en".to_string())
}

fn default_theme() -> String {
    theme::DEFAULT_THEME_NAME.to_string()
}

impl ZerocodeConfig {
    pub fn resolve_theme(&self) -> Result<Theme> {
        let name = &self.theme.name;
        if name.trim().is_empty() {
            return theme::theme_by_name(theme::DEFAULT_THEME_NAME)
                .context("default theme missing from registry");
        }
        theme::theme_by_name(name).with_context(|| {
            let known = theme::theme_names().collect::<Vec<_>>().join(", ");
            format!("unknown theme '{name}' in {FILE_NAME}; known themes: {known}")
        })
    }

    /// Resolve the per-agent theme override for `alias`, if one is configured.
    /// Returns `Ok(None)` when the agent has no override (the pane uses the base
    /// theme). An override naming an unknown theme is a hard error, matching the
    /// base theme's strictness, so a typo fails loud rather than silently
    /// ignoring the override.
    pub fn resolve_agent_theme(&self, alias: &str) -> Result<Option<Theme>> {
        let Some(over) = self.theme.agent_override.get(alias) else {
            return Ok(None);
        };
        let name = over.name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        theme::theme_by_name(name).map(Some).with_context(|| {
            let known = theme::theme_names().collect::<Vec<_>>().join(", ");
            format!(
                "unknown theme '{name}' in [theme.agent_override.{alias}] of {FILE_NAME}; \
                     known themes: {known}"
            )
        })
    }

    /// Resolve the stored keybindings into a validated override table.
    /// An empty section yields an empty table (compile-time defaults).
    pub fn resolve_keybindings(&self) -> Result<OverrideTable> {
        let rows: HashMap<String, Vec<Chord>> = self
            .keybindings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().into_vec()))
            .collect();
        keybindings::build_override_table(rows)
    }

    /// Aliases that have a `[theme.agent_override.<alias>]` entry. The single
    /// iteration point over the override map so callers never reach into the
    /// section's internals.
    pub fn agent_override_aliases(&self) -> impl Iterator<Item = &str> {
        self.theme.agent_override.keys().map(String::as_str)
    }

    /// The configured override theme name for `alias`, if any. Returns the raw
    /// stored name without validating it against the registry; for a resolved
    /// palette use `resolve_agent_theme`.
    pub fn agent_override_name(&self, alias: &str) -> Option<&str> {
        self.theme
            .agent_override
            .get(alias)
            .map(|o| o.name.as_str())
    }

    pub fn resolve_locale(&self) -> Option<String> {
        self.locale
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
}

pub(crate) fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE_NAME)
}

/// Ensure the config dir and file exist, then load + apply env overrides.
///
/// Theme and keybindings are loaded independently: a bad `[keybindings]`
/// table must not blank the user's theme (or vice versa). The whole
/// document is first parsed as a raw `toml::Table`; each typed section
/// is then deserialised on its own and falls back to its default on
/// failure with a stderr warning.
pub(crate) fn ensure_and_load(config_dir: &Path) -> Result<ZerocodeConfig> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("creating config dir {}", config_dir.display()))?;

    let path = config_path(config_dir);
    if !path.exists() {
        let default = ZerocodeConfig::default();
        let body = toml::to_string_pretty(&default).context("serializing default config")?;
        std::fs::write(&path, body)
            .with_context(|| format!("writing default {}", path.display()))?;
    }

    let doc = load_document(&path)?;
    let mut config = ZerocodeConfig::default();
    if let Some(v) = doc.get("locale").and_then(|v| v.as_str()) {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.locale = Some(trimmed.to_string());
        }
    }
    if let Some(v) = doc.get("theme") {
        match v.clone().try_into::<ThemeSection>() {
            Ok(section) => config.theme = section,
            Err(e) => eprintln!(
                "zerocode: ignoring [theme] in {} ({e}); using default",
                path.display()
            ),
        }
    }
    if let Some(v) = doc.get("connection") {
        match v.clone().try_into::<ConnectionSection>() {
            Ok(section) => config.connection = section,
            Err(e) => eprintln!(
                "zerocode: ignoring [connection] in {} ({e}); using default",
                path.display()
            ),
        }
    }
    if let Some(v) = doc.get("keybindings") {
        match v.clone().try_into::<HashMap<String, ChordSpec>>() {
            Ok(rows) => config.keybindings = rows,
            Err(e) => eprintln!(
                "zerocode: ignoring [keybindings] in {} ({e}); using defaults",
                path.display()
            ),
        }
    }

    apply_env_overrides(&mut config)?;
    Ok(config)
}

/// Load the on-disk file as a raw `toml::Table`. A missing or empty file
/// yields an empty table; any other section the running struct does not
/// model is carried through untouched so a partial write never clobbers it.
fn load_document(path: &Path) -> Result<toml::Table> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(toml::Table::new());
    }
    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// Serialize a mutated document table back to disk.
fn write_document(path: &Path, doc: &toml::Table) -> Result<()> {
    let body = toml::to_string_pretty(doc).context("serializing config")?;
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))
}

/// Mutable borrow of `key`'s sub-table, inserting an empty one when absent.
fn section_mut<'a>(doc: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table> {
    doc.entry(key)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::Error::msg(format!("'{key}' is not a table")))
}

/// Persist the selected theme name, editing only the `[theme]` section.
pub(crate) fn persist_theme(config_dir: &Path, theme_name: &str) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    section_mut(&mut doc, "theme")?.insert(
        "name".to_string(),
        toml::Value::String(theme_name.to_string()),
    );
    write_document(&path, &doc)
}

/// Persist a per-agent theme override, writing only
/// `[theme.agent_override.<alias>].name`. Other agents' overrides and every
/// other section are preserved.
pub(crate) fn persist_agent_theme(config_dir: &Path, alias: &str, theme_name: &str) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    section_mut_path(&mut doc, &["theme", "agent_override", alias])?.insert(
        "name".to_string(),
        toml::Value::String(theme_name.to_string()),
    );
    write_document(&path, &doc)
}

/// Remove a per-agent theme override, dropping the whole
/// `[theme.agent_override.<alias>]` entry (and the `agent_override` table if it
/// becomes empty). A no-op when the agent has no override. Other sections are
/// preserved.
pub(crate) fn persist_agent_theme_clear(config_dir: &Path, alias: &str) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    let Some(theme_tbl) = doc.get_mut("theme").and_then(toml::Value::as_table_mut) else {
        return write_document(&path, &doc);
    };
    let Some(over_tbl) = theme_tbl
        .get_mut("agent_override")
        .and_then(toml::Value::as_table_mut)
    else {
        return write_document(&path, &doc);
    };
    over_tbl.remove(alias);
    if over_tbl.is_empty() {
        theme_tbl.remove("agent_override");
    }
    write_document(&path, &doc)
}

fn section_mut_path<'a>(doc: &'a mut toml::Table, keys: &[&str]) -> Result<&'a mut toml::Table> {
    let mut cur = doc;
    for key in keys {
        cur = section_mut(cur, key)?;
    }
    Ok(cur)
}

pub(crate) fn persist_wss_route_ack(config_dir: &Path, uri: &str) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    let tls = section_mut_path(&mut doc, &["connection", "wss", "tls"])?;
    let routes = tls
        .entry("skip_verify_routes")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow::Error::msg("skip_verify_routes is not an array"))?;
    let already = routes.iter().any(|v| v.as_str().is_some_and(|s| s == uri));
    if !already {
        routes.push(toml::Value::String(uri.to_string()));
    }
    write_document(&path, &doc)
}

pub(crate) fn persist_connection_field(
    config_dir: &Path,
    leaf_path: &str,
    value: toml::Value,
) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    let mut segments: Vec<&str> = leaf_path.split('.').collect();
    let leaf = segments
        .pop()
        .ok_or_else(|| anyhow::Error::msg("empty connection field path"))?;
    let mut prefix = vec!["connection", "wss"];
    prefix.extend(segments);
    section_mut_path(&mut doc, &prefix)?.insert(leaf.to_string(), value);
    write_document(&path, &doc)
}

pub(crate) fn persist_locale(config_dir: &Path, locale: &str) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    doc.insert(
        "locale".to_string(),
        toml::Value::String(locale.to_string()),
    );
    write_document(&path, &doc)
}

/// Overwrite the `[keybindings]` section from a resolved override table
/// (preset pick). Sparse: only overridden actions are written; everything
/// else falls back to compile-time defaults on next load. Only the
/// `[keybindings]` section is touched; other sections are preserved.
pub(crate) fn persist_keybindings(config_dir: &Path, table: &OverrideTable) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    let rows = flatten_table(table);
    let serialized = toml::Value::try_from(&rows)
        .context("serializing keybindings")?
        .as_table()
        .cloned()
        .unwrap_or_default();
    doc.insert("keybindings".to_string(), toml::Value::Table(serialized));
    write_document(&path, &doc)
}

/// Insert or replace a single `"<tag>.<variant>"` row (capture-modal
/// save), leaving the rest of `[keybindings]` and all other sections intact.
pub(crate) fn persist_keybind_row(
    config_dir: &Path,
    action_key: &str,
    chords: Vec<Chord>,
) -> Result<()> {
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    let value = toml::Value::try_from(ChordSpec::Many(chords)).context("serializing chords")?;
    section_mut(&mut doc, "keybindings")?.insert(action_key.to_string(), value);
    write_document(&path, &doc)
}

/// Collapse a nested `tag -> variant -> chords` table into the flat
/// `"<tag>.<variant>" -> ChordSpec` map the toml section stores.
fn flatten_table(table: &OverrideTable) -> HashMap<String, ChordSpec> {
    let mut out = HashMap::new();
    for (tag, variants) in table {
        for (variant, chords) in variants {
            out.insert(format!("{tag}.{variant}"), ChordSpec::Many(chords.clone()));
        }
    }
    out
}

/// Apply every `ZEROCODE_<dotted__path>=value` env var. Hard-errors on any var
/// that does not resolve to a known config path.
fn apply_env_overrides(config: &mut ZerocodeConfig) -> Result<()> {
    let mut entries: Vec<(String, String, String)> = std::env::vars()
        .filter_map(|(k, v)| {
            let tail = k.strip_prefix(ENV_PREFIX)?;
            (!tail.is_empty()
                && tail
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .then(|| (k.clone(), v, tail.replace(ENV_SEP, ".")))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (env_name, value, path) in entries {
        set_prop(config, &path, &value).with_context(|| format!("{env_name} -> {path}"))?;
    }
    Ok(())
}

/// Set a leaf at a dotted `path` via a serde roundtrip through `toml::Value`.
/// No field names are hardcoded: the struct's serialized shape is the registry.
fn set_prop<T: Serialize + serde::de::DeserializeOwned>(
    target: &mut T,
    path: &str,
    value: &str,
) -> Result<()> {
    let mut root = toml::Value::try_from(&*target).context("serializing config for set_prop")?;
    let segments: Vec<&str> = path.split('.').collect();
    let (leaf, parents) = segments
        .split_last()
        .ok_or_else(|| anyhow::Error::msg("empty config path"))?;

    let mut cursor = &mut root;
    for seg in parents {
        cursor = cursor
            .as_table_mut()
            .and_then(|t| t.get_mut(*seg))
            .ok_or_else(|| {
                anyhow::Error::msg(format!("path '{path}' did not resolve to a config field"))
            })?;
    }
    let table = cursor.as_table_mut().ok_or_else(|| {
        anyhow::Error::msg(format!("path '{path}' did not resolve to a config field"))
    })?;
    if !table.contains_key(*leaf) {
        anyhow::bail!("path '{path}' did not resolve to a config field");
    }
    table.insert((*leaf).to_string(), toml::Value::String(value.to_string()));

    *target = root
        .try_into()
        .context("deserializing config after set_prop")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_is_registered() {
        let c = ZerocodeConfig::default();
        assert_eq!(c.theme.name, theme::DEFAULT_THEME_NAME);
        assert!(c.resolve_theme().is_ok());
    }

    #[test]
    fn default_config_emits_locale() {
        let body = toml::to_string_pretty(&ZerocodeConfig::default()).unwrap();
        assert!(
            body.contains("locale = \"en\""),
            "default config must surface the locale prop on disk; got:\n{body}"
        );
    }

    #[test]
    fn resolve_locale_trims_and_blanks_fall_back() {
        let c = ZerocodeConfig {
            locale: Some("  fr  ".to_string()),
            ..Default::default()
        };
        assert_eq!(c.resolve_locale().as_deref(), Some("fr"));
        let blank = ZerocodeConfig {
            locale: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(blank.resolve_locale(), None);
    }

    #[test]
    fn set_prop_locale_roundtrip() {
        let mut c = ZerocodeConfig::default();
        set_prop(&mut c, "locale", "ja").unwrap();
        assert_eq!(c.locale.as_deref(), Some("ja"));
    }

    #[test]
    fn persist_locale_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "locale = \"en\"\n\n[theme]\nname = \"nord\"\n\n[future]\nkeep = true\n",
        );
        persist_locale(dir.path(), "fr").unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["locale"].as_str(), Some("fr"));
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));
    }

    #[test]
    fn set_prop_roundtrip() {
        let mut c = ZerocodeConfig::default();
        set_prop(&mut c, "theme.name", "nord").unwrap();
        assert_eq!(c.theme.name, "nord");
    }

    #[test]
    fn set_prop_unknown_path_errors() {
        let mut c = ZerocodeConfig::default();
        let err = set_prop(&mut c, "no_such_field", "x").unwrap_err();
        assert!(err.to_string().contains("did not resolve"));
    }

    #[test]
    fn resolve_unknown_theme_errors() {
        let c = ZerocodeConfig {
            theme: ThemeSection {
                name: "bogus".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = c.resolve_theme().unwrap_err();
        assert!(err.to_string().contains("unknown theme 'bogus'"));
    }

    #[test]
    fn agent_override_resolves_known_theme() {
        let body =
            "[theme]\nname = \"nord_dark\"\n\n[theme.agent_override.coder]\nname = \"dracula\"\n";
        let c: ZerocodeConfig = toml::from_str(body).unwrap();
        let t = c
            .resolve_agent_theme("coder")
            .unwrap()
            .expect("override present");
        assert_eq!(t.title, theme::theme_by_name("dracula").unwrap().title);
    }

    #[test]
    fn agent_override_absent_alias_is_none() {
        let c: ZerocodeConfig = toml::from_str("[theme]\nname = \"nord_dark\"\n").unwrap();
        assert!(c.resolve_agent_theme("nobody").unwrap().is_none());
    }

    #[test]
    fn agent_override_unknown_theme_errors() {
        let body = "[theme.agent_override.coder]\nname = \"no_such_theme\"\n";
        let c: ZerocodeConfig = toml::from_str(body).unwrap();
        let err = c.resolve_agent_theme("coder").unwrap_err();
        assert!(err.to_string().contains("unknown theme 'no_such_theme'"));
        assert!(err.to_string().contains("[theme.agent_override.coder]"));
    }

    #[test]
    fn agent_override_blank_name_is_none() {
        let body = "[theme.agent_override.coder]\nname = \"  \"\n";
        let c: ZerocodeConfig = toml::from_str(body).unwrap();
        assert!(c.resolve_agent_theme("coder").unwrap().is_none());
    }

    #[test]
    fn agent_override_aliases_lists_configured() {
        let body = "[theme.agent_override.a]\nname = \"dracula\"\n[theme.agent_override.b]\nname = \"nord_dark\"\n";
        let c: ZerocodeConfig = toml::from_str(body).unwrap();
        let mut aliases: Vec<&str> = c.agent_override_aliases().collect();
        aliases.sort_unstable();
        assert_eq!(aliases, vec!["a", "b"]);
    }

    #[test]
    fn default_config_emits_no_agent_override() {
        let body = toml::to_string_pretty(&ZerocodeConfig::default()).unwrap();
        assert!(
            !body.contains("agent_override"),
            "default config must not scaffold agent_override; got:\n{body}"
        );
    }

    #[test]
    fn persist_agent_theme_writes_nested_and_preserves_others() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"nord_dark\"\n\n[future]\nkeep = true\n",
        );
        persist_agent_theme(dir.path(), "coder", "dracula").unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord_dark"));
        assert_eq!(
            doc["theme"]["agent_override"]["coder"]["name"].as_str(),
            Some("dracula")
        );
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));
    }

    #[test]
    fn persist_agent_theme_round_trips_through_resolver() {
        let dir = tempfile::tempdir().unwrap();
        persist_agent_theme(dir.path(), "coder", "dracula").unwrap();
        let cfg = ensure_and_load(dir.path()).unwrap();
        let t = cfg.resolve_agent_theme("coder").unwrap().unwrap();
        assert_eq!(t.title, theme::theme_by_name("dracula").unwrap().title);
    }

    #[test]
    fn persist_agent_theme_clear_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        persist_agent_theme(dir.path(), "a", "dracula").unwrap();
        persist_agent_theme(dir.path(), "b", "nord_dark").unwrap();
        persist_agent_theme_clear(dir.path(), "a").unwrap();
        let cfg = ensure_and_load(dir.path()).unwrap();
        assert!(cfg.resolve_agent_theme("a").unwrap().is_none());
        assert!(cfg.resolve_agent_theme("b").unwrap().is_some());
    }

    #[test]
    fn persist_agent_theme_clear_drops_empty_table() {
        let dir = tempfile::tempdir().unwrap();
        persist_agent_theme(dir.path(), "only", "dracula").unwrap();
        persist_agent_theme_clear(dir.path(), "only").unwrap();
        let on_disk = read(dir.path());
        assert!(
            !on_disk.contains("agent_override"),
            "clearing the last override must drop the table; got:\n{on_disk}"
        );
    }

    #[test]
    fn persist_agent_theme_clear_is_noop_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), "[theme]\nname = \"nord_dark\"\n");
        persist_agent_theme_clear(dir.path(), "ghost").unwrap();
        let cfg = ensure_and_load(dir.path()).unwrap();
        assert_eq!(cfg.theme.name, "nord_dark");
    }

    #[test]
    fn resolve_empty_theme_recovers_to_default() {
        for blank in ["", "   "] {
            let c = ZerocodeConfig {
                theme: ThemeSection {
                    name: blank.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let resolved = c.resolve_theme().expect("empty theme recovers to default");
            assert_eq!(resolved.title, theme::default_theme().title);
        }
    }

    fn seed(dir: &Path, body: &str) {
        std::fs::write(config_path(dir), body).unwrap();
    }

    fn read(dir: &Path) -> String {
        std::fs::read_to_string(config_path(dir)).unwrap()
    }

    #[test]
    fn persist_theme_preserves_unmodeled_sections() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"nord\"\n\n[future]\nfield = 42\nnested = [\"a\", \"b\"]\n",
        );
        persist_theme(dir.path(), "gruvbox").unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("gruvbox"));
        assert_eq!(doc["future"]["field"].as_integer(), Some(42));
        assert_eq!(
            doc["future"]["nested"].as_array().unwrap().len(),
            2,
            "unmodeled section must survive a theme write"
        );
    }

    #[test]
    fn persist_keybind_row_preserves_theme_and_unmodeled() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"nord\"\n\n[future]\nkeep = true\n",
        );
        persist_keybind_row(dir.path(), "dashboard.up", vec![Chord::char('z')]).unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));
        assert!(
            doc["keybindings"]
                .as_table()
                .unwrap()
                .contains_key("dashboard.up")
        );
    }

    #[test]
    fn persist_keybindings_replaces_only_its_section() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"nord\"\n\n[keybindings]\nold = \"x\"\n\n[future]\nkeep = 1\n",
        );
        let mut table: OverrideTable = OverrideTable::new();
        table
            .entry("dashboard".to_string())
            .or_default()
            .insert("up".to_string(), vec![Chord::char('z')]);
        persist_keybindings(dir.path(), &table).unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_integer(), Some(1));
        let kb = doc["keybindings"].as_table().unwrap();
        assert!(kb.contains_key("dashboard.up"));
        assert!(!kb.contains_key("old"), "preset pick replaces the section");
    }

    #[test]
    fn bad_keybindings_do_not_blank_theme() {
        let dir = tempfile::tempdir().unwrap();
        // `"+"` was historically unparseable; even if a future bug
        // re-introduces that, the theme must still load.
        seed(
            dir.path(),
            "[theme]\nname = \"dracula\"\n\n[keybindings]\n\"logs.increase_level\" = [\"completely::bogus::token\"]\n",
        );
        let cfg = ensure_and_load(dir.path()).unwrap();
        assert_eq!(cfg.theme.name, "dracula");
        assert!(
            cfg.keybindings.is_empty(),
            "bad keybindings drop to default"
        );
    }

    #[test]
    fn bad_theme_does_not_blank_keybindings() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = 42\n\n[keybindings]\n\"dashboard.up\" = [\"k\"]\n",
        );
        let cfg = ensure_and_load(dir.path()).unwrap();
        assert_eq!(cfg.theme.name, theme::DEFAULT_THEME_NAME);
        assert!(cfg.keybindings.contains_key("dashboard.up"));
    }

    #[test]
    fn persist_theme_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        persist_theme(dir.path(), "gruvbox").unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("gruvbox"));
    }

    #[test]
    fn connection_section_round_trips() {
        let mut c = ZerocodeConfig::default();
        c.connection.wss.uri = Some("wss://host:9781".to_string());
        c.connection.wss.tls.skip_verify = true;
        c.connection.wss.tls.skip_verify_routes = vec!["wss://host:9781".to_string()];
        let body = toml::to_string_pretty(&c).unwrap();
        let back: ZerocodeConfig = toml::from_str(&body).unwrap();
        assert_eq!(back.connection.wss.uri.as_deref(), Some("wss://host:9781"));
        assert!(back.connection.wss.tls.skip_verify);
        assert_eq!(
            back.connection.wss.tls.skip_verify_routes,
            vec!["wss://host:9781"]
        );
    }

    #[test]
    fn empty_connection_defaults_are_clean() {
        let c = ZerocodeConfig::default();
        assert!(c.connection.wss.uri.is_none());
        assert!(!c.connection.wss.tls.skip_verify);
        assert!(c.connection.wss.tls.skip_verify_routes.is_empty());
        let parsed: ZerocodeConfig = toml::from_str("locale = \"en\"\n").unwrap();
        assert!(parsed.connection.wss.uri.is_none());
        assert!(parsed.connection.wss.tls.skip_verify_routes.is_empty());
    }

    #[test]
    fn default_config_writes_no_connection_scaffolding() {
        let body = toml::to_string_pretty(&ZerocodeConfig::default()).unwrap();
        assert!(
            !body.contains("connection"),
            "default config must not emit any [connection] scaffolding; got:\n{body}"
        );
        assert!(!body.contains("skip_verify"), "got:\n{body}");
        assert!(!body.contains("wss"), "got:\n{body}");
    }

    #[test]
    fn first_run_file_has_no_connection_section() {
        let dir = tempfile::tempdir().unwrap();
        ensure_and_load(dir.path()).unwrap();
        let on_disk = read(dir.path());
        assert!(
            !on_disk.contains("connection"),
            "first-run file must not scaffold [connection]; got:\n{on_disk}"
        );
    }

    #[test]
    fn setting_one_field_materializes_only_that_path() {
        let dir = tempfile::tempdir().unwrap();
        persist_connection_field(dir.path(), "tls.skip_verify", toml::Value::Boolean(true))
            .unwrap();
        let on_disk = read(dir.path());
        assert!(on_disk.contains("[connection.wss.tls]"));
        assert!(on_disk.contains("skip_verify = true"));
        assert!(
            !on_disk.contains("skip_verify_routes"),
            "untouched fields must not appear; got:\n{on_disk}"
        );
    }

    #[test]
    fn route_acked_membership() {
        let tls = WssTlsSection {
            skip_verify_routes: vec!["wss://a:1".to_string(), "wss://b:2".to_string()],
            ..Default::default()
        };
        assert!(tls.route_acked("wss://a:1"));
        assert!(tls.route_acked("wss://b:2"));
        assert!(!tls.route_acked("wss://c:3"));
    }

    #[test]
    fn persist_wss_route_ack_dedups() {
        let dir = tempfile::tempdir().unwrap();
        persist_wss_route_ack(dir.path(), "wss://a:1").unwrap();
        persist_wss_route_ack(dir.path(), "wss://a:1").unwrap();
        persist_wss_route_ack(dir.path(), "wss://b:2").unwrap();
        let cfg = ensure_and_load(dir.path()).unwrap();
        assert_eq!(
            cfg.connection.wss.tls.skip_verify_routes,
            vec!["wss://a:1".to_string(), "wss://b:2".to_string()]
        );
    }

    #[test]
    fn persist_wss_route_ack_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"nord\"\n\n[future]\nkeep = true\n",
        );
        persist_wss_route_ack(dir.path(), "wss://a:1").unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));
        assert_eq!(
            doc["connection"]["wss"]["tls"]["skip_verify_routes"][0].as_str(),
            Some("wss://a:1")
        );
    }

    #[test]
    fn persist_connection_field_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), "[theme]\nname = \"nord\"\n");
        persist_connection_field(
            dir.path(),
            "uri",
            toml::Value::String("wss://host:9781".to_string()),
        )
        .unwrap();
        persist_connection_field(dir.path(), "tls.skip_verify", toml::Value::Boolean(true))
            .unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(
            doc["connection"]["wss"]["uri"].as_str(),
            Some("wss://host:9781")
        );
        assert_eq!(
            doc["connection"]["wss"]["tls"]["skip_verify"].as_bool(),
            Some(true)
        );
    }
}
