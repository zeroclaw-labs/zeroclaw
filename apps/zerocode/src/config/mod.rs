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
}

impl Default for ThemeSection {
    fn default() -> Self {
        Self {
            name: default_theme(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZerocodeConfig {
    #[serde(default = "default_locale")]
    pub locale: Option<String>,
    #[serde(default)]
    pub theme: ThemeSection,
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

/// Persist the top-level `locale` key, leaving every other section intact.
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
            },
            ..Default::default()
        };
        let err = c.resolve_theme().unwrap_err();
        assert!(err.to_string().contains("unknown theme 'bogus'"));
    }

    #[test]
    fn resolve_empty_theme_recovers_to_default() {
        for blank in ["", "   "] {
            let c = ZerocodeConfig {
                theme: ThemeSection {
                    name: blank.to_string(),
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
}
