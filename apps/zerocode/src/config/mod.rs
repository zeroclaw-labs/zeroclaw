//! Local zerocode client configuration: theme and keybindings.
//! Always read from the local `<config_dir>/zerocode-config.toml`, independent
//! of the connection target. Layering: defaults -> file -> `ZEROCODE_*` env.
#![allow(dead_code)]

pub mod keybindings;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::keymap::{ChatTabAction, Chord, GlobalAction, overrides::OverrideTable};
use crate::theme::{self, Theme};

const FILE_NAME: &str = "zerocode-config.toml";
const ENV_PREFIX: &str = "ZEROCODE_";
const ENV_SEP: &str = "__";

/// One or more chords bound to an action. Accepts a bare string (one
/// chord) or an array on the wire; always serialized back as an array.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum ChordSpec {
    One(Chord),
    Many(Vec<Chord>),
}

impl ChordSpec {
    fn as_slice(&self) -> &[Chord] {
        match self {
            Self::One(c) => std::slice::from_ref(c),
            Self::Many(cs) => cs,
        }
    }

    fn into_vec(self) -> Vec<Chord> {
        match self {
            Self::One(c) => vec![c],
            Self::Many(cs) => cs,
        }
    }
}

fn migrate_legacy_help_binding(rows: &mut HashMap<String, ChordSpec>) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};

    let key = GlobalAction::Help.action_key();
    let legacy = [
        Chord::char('?'),
        Chord::key(KeyCode::F(1)),
        Chord::with(KeyCode::F(1), KeyModifiers::CONTROL),
    ];
    let Some(spec) = rows.get(&key) else {
        return false;
    };
    if spec.as_slice() != legacy {
        return false;
    }

    rows.insert(key, ChordSpec::Many(GlobalAction::Help.default_chords()));
    true
}

fn migrate_legacy_copy_binding(rows: &mut HashMap<String, ChordSpec>) -> bool {
    let key = ChatTabAction::CopySelection.action_key();
    let Some(ChordSpec::Many(chords)) = rows.get(&key) else {
        return false;
    };
    if chords.as_slice() != [Chord::char('y')] {
        return false;
    }

    rows.insert(
        key,
        ChordSpec::Many(ChatTabAction::CopySelection.default_chords()),
    );
    true
}

fn migrate_legacy_chat_jump_binding(
    rows: &mut HashMap<String, ChordSpec>,
    action: ChatTabAction,
    legacy: Chord,
) -> bool {
    let key = action.action_key();
    let Some(ChordSpec::Many(chords)) = rows.get(&key) else {
        return false;
    };
    if chords.as_slice() != std::slice::from_ref(&legacy) {
        return false;
    }

    rows.insert(key, ChordSpec::Many(action.default_chords()));
    true
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
    /// Reach the daemon through a nominated relay at this `host:port` instead of
    /// connecting to `uri` directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// Node-id of the target daemon to request from the relay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_node: Option<String>,
    /// How many times to try the direct address before falling back to the relay
    /// (default 2). Only relevant when both a direct address and a relay exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_attempts: Option<u32>,
    /// Per-attempt timeout, in seconds, for a direct connect before falling back
    /// to the relay (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_timeout_secs: Option<u64>,
    /// While connected through the relay, how often (seconds) to re-probe the
    /// direct address and migrate back when it returns (default 30; 0 disables).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reprobe_secs: Option<u64>,
}

impl WssSection {
    fn is_empty(&self) -> bool {
        self.uri.is_none()
            && self.tls.is_empty()
            && self.relay_url.is_none()
            && self.relay_node.is_none()
            && self.direct_attempts.is_none()
            && self.direct_timeout_secs.is_none()
            && self.reprobe_secs.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WssTlsSection {
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_verify: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_verify_routes: Vec<String>,
    /// PEM CA certificate used to verify the daemon (mutual TLS). When set, the
    /// server certificate is verified against this CA instead of the system roots.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ca_cert_path: String,
    /// PEM client certificate presented to the daemon (mutual TLS).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_cert_path: String,
    /// PEM client private key for `client_cert_path`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_key_path: String,
}

impl WssTlsSection {
    pub fn route_acked(&self, uri: &str) -> bool {
        self.skip_verify_routes.iter().any(|r| r == uri)
    }

    fn is_empty(&self) -> bool {
        !self.skip_verify
            && self.skip_verify_routes.is_empty()
            && self.ca_cert_path.is_empty()
            && self.client_cert_path.is_empty()
            && self.client_key_path.is_empty()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ── Todo tracker ──────────────────────────────────────────────────────────────

/// Where the todo tracker renders inside the Code pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TodoTrackerLocation {
    Bottom,
    Left,
    #[default]
    Right,
}

/// The `[todotracker]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TodoTrackerSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub enabled_at_start: bool,
    #[serde(default)]
    pub location: TodoTrackerLocation,
    #[serde(default = "default_todotracker_width")]
    pub width: u16,
    #[serde(default = "default_todotracker_max_height")]
    pub max_height: u16,
}

impl Default for TodoTrackerSection {
    fn default() -> Self {
        Self {
            enabled: true,
            enabled_at_start: false,
            location: TodoTrackerLocation::Right,
            width: default_todotracker_width(),
            max_height: default_todotracker_max_height(),
        }
    }
}

impl TodoTrackerSection {
    /// Reject values that must never reach the runtime.
    ///
    /// Run at **every** authoritative boundary: Config-pane saves call it
    /// before persistence so a success message always describes the values
    /// the next session will consume, and [`resolve_todo_tracker_checked`]
    /// calls it on the effective (post-env-override) section so hand-edited
    /// files and `ZEROCODE_todotracker__*` overrides fail visibly instead of
    /// being normalized.
    pub(crate) fn validate(&self) -> std::result::Result<(), UiSectionValidationError> {
        if self.width == 0 || self.max_height == 0 {
            return Err(UiSectionValidationError::PositiveRequired);
        }
        Ok(())
    }

    /// Infallible view used by non-authoritative callers (e.g. the Config
    /// pane's persisted-vs-effective comparison), which must render something
    /// rather than fail.
    ///
    /// The `.max(1)` here is a last-resort clamp so a zero can never collapse
    /// the panel if some future caller bypasses validation — it is **not** the
    /// authority on validity. An explicit zero from the file or the
    /// environment must fail visibly rather than be silently normalized to
    /// `1`, which is enforced by [`ZerocodeConfig::validate_todo_tracker`];
    /// every session boundary runs it via [`resolve_todo_tracker_checked`].
    pub(crate) fn resolve(&self) -> TodoTrackerSettings {
        TodoTrackerSettings {
            enabled: self.enabled,
            enabled_at_start: self.enabled_at_start,
            location: match self.location {
                TodoTrackerLocation::Bottom => TodoLocation::Bottom,
                TodoTrackerLocation::Left => TodoLocation::Left,
                TodoTrackerLocation::Right => TodoLocation::Right,
            },
            width: self.width.max(1),
            max_height: self.max_height.max(1),
        }
    }
}

/// Where the todo tracker renders, as consumed by the
/// [`TodoTracker`](crate::todo_tracker::TodoTracker) widget. This is the
/// runtime mirror of [`TodoTrackerLocation`] (the serde section enum);
/// keeping them distinct lets the widget stay independent of the on-disk
/// serialization shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoLocation {
    Bottom,
    Left,
    Right,
}

/// Runtime `[todotracker]` settings, resolved from the config section by
/// [`ZerocodeConfig::resolve_todo_tracker`]. Values are validated at that
/// boundary, so downstream consumers can trust them as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TodoTrackerSettings {
    pub enabled: bool,
    pub enabled_at_start: bool,
    pub location: TodoLocation,
    pub width: u16,
    pub max_height: u16,
}

impl Default for TodoTrackerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            enabled_at_start: false,
            location: TodoLocation::Right,
            width: default_todotracker_width(),
            max_height: default_todotracker_max_height(),
        }
    }
}

/// Validation failures shared by the local UI config sections. User-facing
/// wording is supplied by the Config pane's Fluent catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiSectionValidationError {
    PositiveRequired,
}

impl std::fmt::Display for UiSectionValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PositiveRequired => f.write_str("numeric values must be greater than zero"),
        }
    }
}

impl std::error::Error for UiSectionValidationError {}

// ── Default helpers ───────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn default_todotracker_width() -> u16 {
    32
}

fn default_todotracker_max_height() -> u16 {
    5
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
    #[serde(default)]
    pub todotracker: TodoTrackerSection,
}

impl Default for ZerocodeConfig {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            theme: ThemeSection::default(),
            connection: ConnectionSection::default(),
            keybindings: HashMap::new(),
            todotracker: TodoTrackerSection::default(),
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
        let name = self.theme.name.trim();
        if name.is_empty() {
            return theme::theme_by_name(theme::DEFAULT_THEME_NAME)
                .context("default theme missing from registry");
        }
        // Unknown theme name (e.g. a config written by a newer build, or a
        // typo) falls back to the inherit-shell `terminal` theme rather than
        // aborting the TUI. The fallback is always present in the registry.
        Ok(theme::theme_by_name(name).unwrap_or_else(theme::fallback_theme))
    }

    pub fn resolve_agent_theme(&self, alias: &str) -> Result<Option<Theme>> {
        let Some(over) = self.theme.agent_override.get(alias) else {
            return Ok(None);
        };
        let name = over.name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            theme::theme_by_name(name).unwrap_or_else(theme::fallback_theme),
        ))
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

    /// Convert the `[todotracker]` section into the runtime settings type
    /// used by [`TodoTracker`](crate::todo_tracker::TodoTracker).
    ///
    /// This is the infallible view. Validity is enforced separately by
    /// [`Self::validate_todo_tracker`] at the authoritative boundaries, so
    /// an explicit zero surfaces an error instead of being normalized.
    pub fn resolve_todo_tracker(&self) -> TodoTrackerSettings {
        self.todotracker.resolve()
    }

    /// Validate the effective `[todotracker]` section, whatever its origin
    /// (file, defaults, or `ZEROCODE_todotracker__*` override).
    pub(crate) fn validate_todo_tracker(
        &self,
    ) -> std::result::Result<(), UiSectionValidationError> {
        self.todotracker.validate()
    }
}

pub(crate) fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE_NAME)
}

/// The effective config: on-disk values with `ZEROCODE_*` env overrides
/// applied on top. This is what runtime consumers (sessions, rendering) read.
///
/// Env overrides are process-transient and must never be written back to disk,
/// so anything that *edits and saves* config must use [`load_persisted`]
/// instead — otherwise saving one field would bake an env-injected value for
/// an unrelated field into `zerocode-config.toml`.
pub(crate) fn ensure_and_load(config_dir: &Path) -> Result<ZerocodeConfig> {
    let mut config = load_persisted(config_dir)?;
    apply_env_overrides(&mut config)?;
    Ok(config)
}

/// Resolve the effective `[todotracker]` settings for a session boundary,
/// treating a **present-but-malformed** `[todotracker]` section as an error
/// rather than silently falling back to defaults.
///
/// [`load_persisted`] deliberately tolerates a malformed section (so one bad
/// block cannot blank unrelated config), which means a botched manual edit
/// would otherwise resolve to defaults and silently reset a user's live
/// tracker on restart/switch. This variant re-parses that one section strictly
/// so the caller can preserve the current settings on failure; a *valid* or
/// *absent* section resolves exactly as `ensure_and_load` would, with
/// `ZEROCODE_todotracker__*` overrides applied.
///
/// The effective section is validated **after** env overrides are applied, so
/// an explicit zero `width`/`max_height` fails visibly whether it came from
/// the file or from the canonical environment surface, rather than being
/// normalized to `1`. Callers keep their current settings on error, so an
/// invalid value never resets a live tracker to defaults.
pub(crate) fn resolve_todo_tracker_checked(config_dir: &Path) -> Result<TodoTrackerSettings> {
    let path = config_path(config_dir);
    if path.exists() {
        let doc = load_document(&path)?;
        if let Some(v) = doc.get("todotracker") {
            // Strict parse: a malformed section is an error here, unlike the
            // tolerant `load_persisted` path.
            v.clone()
                .try_into::<TodoTrackerSection>()
                .with_context(|| format!("[todotracker] in {} is malformed", path.display()))?;
        }
    }
    let config = ensure_and_load(config_dir)?;
    config
        .validate_todo_tracker()
        .map_err(|e| anyhow::Error::msg(format!("[todotracker] is invalid: {e}")))?;
    Ok(config.resolve_todo_tracker())
}

/// The persisted `[todotracker]` section, parsed **strictly**.
///
/// [`load_persisted`] tolerates a malformed section by substituting defaults so
/// one bad block cannot blank unrelated config. That is right for rendering,
/// but wrong for an *editor*: editing a phantom default and saving it would
/// overwrite the user's unparseable canonical text — the very text they need in
/// order to repair it, on the supported manual-upgrade path. The Config pane
/// uses this instead so it can refuse edits and surface the error.
///
/// `Ok(None)` means the section is absent (a fresh file), which is editable
/// from defaults; `Err` means present-but-malformed.
pub(crate) fn load_persisted_todotracker_strict(
    config_dir: &Path,
) -> Result<Option<TodoTrackerSection>> {
    let path = config_path(config_dir);
    if !path.exists() {
        return Ok(None);
    }
    let doc = load_document(&path)?;
    let Some(v) = doc.get("todotracker") else {
        return Ok(None);
    };
    v.clone()
        .try_into::<TodoTrackerSection>()
        .map(Some)
        .with_context(|| format!("[todotracker] in {} is malformed", path.display()))
}

/// The on-disk config exactly as persisted, with **no** env overrides applied.
/// This is the correct base for read-modify-write edits (the Config pane), so
/// a transient `ZEROCODE_*` value can never leak into the saved file.
pub(crate) fn load_persisted(config_dir: &Path) -> Result<ZerocodeConfig> {
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("creating config dir {}", config_dir.display()))?;

    let path = config_path(config_dir);
    if !path.exists() {
        let default = ZerocodeConfig::default();
        let body = toml::to_string_pretty(&default).context("serializing default config")?;
        std::fs::write(&path, body)
            .with_context(|| format!("writing default {}", path.display()))?;
    }

    let mut doc = load_document(&path)?;
    let mut config = ZerocodeConfig::default();
    let mut migrated_keybindings = false;
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
            Ok(mut rows) => {
                if migrate_legacy_help_binding(&mut rows) {
                    let key = GlobalAction::Help.action_key();
                    let value = toml::Value::try_from(
                        rows.get(&key)
                            .expect("migrated Help binding remains present")
                            .clone(),
                    )
                    .context("serializing migrated Help binding")?;
                    doc.get_mut("keybindings")
                        .and_then(toml::Value::as_table_mut)
                        .expect("parsed keybindings remain a table")
                        .insert(key, value);
                    migrated_keybindings = true;
                }
                if migrate_legacy_copy_binding(&mut rows) {
                    let key = ChatTabAction::CopySelection.action_key();
                    let value = toml::Value::try_from(ChordSpec::Many(
                        ChatTabAction::CopySelection.default_chords(),
                    ))
                    .context("serializing migrated Copy binding")?;
                    let bindings = doc
                        .get_mut("keybindings")
                        .and_then(toml::Value::as_table_mut)
                        .context("accessing parsed keybindings table")?;
                    bindings.insert(key, value);
                    migrated_keybindings = true;
                }
                for (action, legacy) in [
                    (ChatTabAction::JumpStart, Chord::char('g')),
                    (ChatTabAction::JumpEnd, Chord::char('G')),
                ] {
                    if !migrate_legacy_chat_jump_binding(&mut rows, action, legacy) {
                        continue;
                    }
                    let key = action.action_key();
                    let value = toml::Value::try_from(ChordSpec::Many(action.default_chords()))
                        .context("serializing migrated chat jump binding")?;
                    let bindings = doc
                        .get_mut("keybindings")
                        .and_then(toml::Value::as_table_mut)
                        .context("accessing parsed keybindings table")?;
                    bindings.insert(key, value);
                    migrated_keybindings = true;
                }
                config.keybindings = rows;
            }
            Err(e) => eprintln!(
                "zerocode: ignoring [keybindings] in {} ({e}); using defaults",
                path.display()
            ),
        }
    }
    if let Some(v) = doc.get("todotracker") {
        match v.clone().try_into::<TodoTrackerSection>() {
            Ok(section) => config.todotracker = section,
            Err(e) => eprintln!(
                "zerocode: ignoring [todotracker] in {} ({e}); using default",
                path.display()
            ),
        }
    }

    if migrated_keybindings {
        write_document(&path, &doc)?;
    }

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

/// Persist the entire `[todotracker]` section, editing only that section.
/// Other sections (theme, keybindings, connection, etc.) are preserved.
///
/// This is the owning read-modify-write boundary, so it enforces the
/// preservation invariant for the section it replaces: if a `[todotracker]`
/// section is currently present but is not *valid* — either unparseable or
/// parseable-but-invalid, such as a zero dimension — the write is refused
/// unless the caller passes a field-scoped repair `intent`, below.
///
/// A caller's snapshot of "the section was fine" can be arbitrarily old — a
/// Config pane may stay open indefinitely while an external editor rewrites
/// the file — so validating the *candidate* alone is not enough. Only the
/// latest document, which this function already loads, can answer whether the
/// value about to be overwritten is the user's invalid canonical data.
pub(crate) fn persist_todotracker(config_dir: &Path, section: &TodoTrackerSection) -> Result<()> {
    persist_todotracker_with_intent(config_dir, section, TrackerWriteIntent::PreserveInvalid)
        .map(|_| ())
}

/// One of the two editable numeric dimensions, used to scope repair authority
/// to exactly the field the user typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackerNumericField {
    Width,
    MaxHeight,
}

impl TrackerNumericField {
    fn get(self, section: &TodoTrackerSection) -> u16 {
        match self {
            Self::Width => section.width,
            Self::MaxHeight => section.max_height,
        }
    }

    fn set(self, section: &mut TodoTrackerSection, value: u16) {
        match self {
            Self::Width => section.width = value,
            Self::MaxHeight => section.max_height = value,
        }
    }

    /// The repaired field must itself be usable, whatever the rest of the
    /// section looks like: repair authority tolerates *other* fields being
    /// invalid, never a fresh zero in the field being written.
    fn validate_in(
        self,
        section: &TodoTrackerSection,
    ) -> std::result::Result<(), UiSectionValidationError> {
        if self.get(section) == 0 {
            return Err(UiSectionValidationError::PositiveRequired);
        }
        Ok(())
    }
}

/// Whether a tracker write is allowed to replace an invalid current section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackerWriteIntent {
    /// Default. Refuse to replace a current section that is unparseable or
    /// invalid, so an unrelated edit cannot silently erase it.
    PreserveInvalid,
    /// The user is explicitly repairing one numeric field, so replacing *that
    /// field's* invalid current value is the whole point. Authority stops
    /// there: every other field keeps its latest on-disk value, so a stale
    /// caller snapshot cannot erase an invalid value the user never touched.
    /// An *unparseable* section is still refused outright: it cannot be shown
    /// in the editor, so there is nothing the user could knowingly replace.
    RepairField(TrackerNumericField),
}

/// Write the `[todotracker]` section and return the section as it was
/// *actually* written, which under [`TrackerWriteIntent::RepairField`] is the
/// latest on-disk section with only the repaired field applied — not
/// necessarily the caller's candidate. Callers must report status against the
/// returned value, never against what they proposed.
pub(crate) fn persist_todotracker_with_intent(
    config_dir: &Path,
    section: &TodoTrackerSection,
    intent: TrackerWriteIntent,
) -> Result<TodoTrackerSection> {
    // The whole candidate must be valid, except during an explicit repair:
    // when several fields are invalid on disk, fixing them one at a time
    // means every intermediate section is still partially invalid. Rejecting
    // those would make such a section permanently unrepairable, so a repair
    // validates only the field it is authorized to write. The caller is
    // responsible for not reporting success until the result is valid.
    match intent {
        TrackerWriteIntent::PreserveInvalid => section.validate()?,
        TrackerWriteIntent::RepairField(field) => field.validate_in(section)?,
    }
    let path = config_path(config_dir);
    let mut doc = load_document(&path)?;
    // Re-read the section as it exists *now*, immediately before replacing
    // it: a caller's "the section was fine" snapshot can be arbitrarily old,
    // since a Config pane may stay open indefinitely while an external editor
    // rewrites the file. An unparseable section is refused under either
    // intent — the user was never shown that text, so they cannot knowingly
    // be replacing it.
    let current = match doc.get("todotracker") {
        Some(value) => Some(
            value
                .clone()
                .try_into::<TodoTrackerSection>()
                .with_context(|| {
                    format!(
                        "refusing to overwrite the malformed [todotracker] section in {}",
                        path.display()
                    )
                })?,
        ),
        None => None,
    };
    let to_write = match (current, intent) {
        // Nothing on disk to preserve: the candidate stands as written.
        (None, _) => section.clone(),
        // An ordinary edit may not replace a current section that parses but
        // is not usable — a zero dimension is explicitly invalid canonical
        // data, and a type check alone lets it through.
        (Some(current), TrackerWriteIntent::PreserveInvalid) => {
            current.validate().with_context(|| {
                format!(
                    "refusing to overwrite the invalid [todotracker] section in {}",
                    path.display()
                )
            })?;
            section.clone()
        }
        // A repair rebases onto the latest section and applies only the one
        // field the user explicitly edited. Any other field — including one
        // that is invalid, and including one an external editor changed after
        // the caller's snapshot — is preserved exactly as it is on disk.
        (Some(current), TrackerWriteIntent::RepairField(field)) => {
            let mut merged = current;
            field.set(&mut merged, field.get(section));
            merged
        }
    };
    let serialized = toml::Value::try_from(&to_write)
        .context("serializing todotracker section")?
        .as_table()
        .cloned()
        .unwrap_or_default();
    doc.insert("todotracker".to_string(), toml::Value::Table(serialized));
    write_document(&path, &doc)?;
    Ok(to_write)
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
///
/// Canonical spelling: the `ZEROCODE_` prefix followed by the lowercase dotted
/// config path with `.` written as `__` — e.g. `ZEROCODE_todotracker__enabled=false`
/// sets `todotracker.enabled`. This mirrors the daemon's `ZEROCLAW_<path>` form.
/// Overrides are process-transient and are never written back to disk.
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
/// The string value is parsed into the correct TOML type by inspecting the
/// existing field value (string → string, bool → bool, integer → integer,
/// etc.).
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

    // Parse the string into the correct TOML type by inspecting the existing
    // field value. This lets env overrides like `ZEROCODE_todotracker__enabled=false`
    // set a boolean without the caller knowing the field type.
    let existing = table[*leaf].clone();
    let new_value = match existing {
        toml::Value::String(_) => toml::Value::String(value.to_string()),
        toml::Value::Boolean(_) => toml::Value::Boolean(
            value
                .parse()
                .with_context(|| format!("failed to parse '{value}' as bool for {path}"))?,
        ),
        toml::Value::Integer(_) => toml::Value::Integer(
            value
                .parse()
                .with_context(|| format!("failed to parse '{value}' as integer for {path}"))?,
        ),
        toml::Value::Float(_) => toml::Value::Float(
            value
                .parse()
                .with_context(|| format!("failed to parse '{value}' as float for {path}"))?,
        ),
        toml::Value::Array(_) => {
            let parsed: Vec<toml::Value> = toml::from_str(value)
                .with_context(|| format!("failed to parse '{value}' as array for {path}"))?;
            toml::Value::Array(parsed)
        }
        toml::Value::Datetime(_) => toml::Value::Datetime(
            value
                .parse()
                .with_context(|| format!("failed to parse '{value}' as datetime for {path}"))?,
        ),
        toml::Value::Table(_) => {
            anyhow::bail!("cannot set a table field via set_prop: {path}")
        }
    };
    table.insert((*leaf).to_string(), new_value);

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
    fn resolve_unknown_theme_falls_back_to_terminal() {
        let c = ZerocodeConfig {
            theme: ThemeSection {
                name: "bogus".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = c
            .resolve_theme()
            .expect("unknown theme falls back, never errors");
        assert_eq!(resolved.title, theme::fallback_theme().title);
        assert_eq!(resolved.background, theme::fallback_theme().background);
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
    fn agent_override_unknown_theme_falls_back_to_terminal() {
        let body = "[theme.agent_override.coder]\nname = \"no_such_theme\"\n";
        let c: ZerocodeConfig = toml::from_str(body).unwrap();
        let t = c
            .resolve_agent_theme("coder")
            .expect("unknown override falls back, never errors")
            .expect("override present");
        assert_eq!(t.title, theme::fallback_theme().title);
        assert_eq!(t.background, theme::fallback_theme().background);
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
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        persist_agent_theme(dir.path(), "coder", "dracula").unwrap();
        let cfg = ensure_and_load(dir.path()).unwrap();
        let t = cfg.resolve_agent_theme("coder").unwrap().unwrap();
        assert_eq!(t.title, theme::theme_by_name("dracula").unwrap().title);
    }

    #[test]
    fn persist_agent_theme_clear_removes_entry() {
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
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
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
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
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
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
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
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
    fn legacy_help_defaults_migrate_without_touching_other_config() {
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"global.help\" = [\"?\", \"f1\", \"ctrl+f1\"]\n\"dashboard.up\" = [\"k\"]\n\n[future]\nkeep = 1\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(
            resolved["global"]["help"],
            vec![Chord::char('?'), Chord::ctrl('g')]
        );
        assert_eq!(resolved["dashboard"]["up"], vec![Chord::char('k')]);

        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        let help: Vec<Chord> = doc["keybindings"]["global.help"]
            .clone()
            .try_into()
            .unwrap();
        assert_eq!(help, vec![Chord::char('?'), Chord::ctrl('g')]);
        assert_eq!(doc["future"]["keep"].as_integer(), Some(1));
    }

    #[test]
    fn customized_help_binding_is_not_migrated() {
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"global.help\" = [\"?\", \"ctrl+h\"]\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(
            resolved["global"]["help"],
            vec![Chord::char('?'), Chord::ctrl('h')]
        );
        assert!(read(dir.path()).contains("ctrl+h"));
    }

    #[test]
    fn legacy_copy_default_migrates_without_touching_other_config() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"chat.copy_selection\" = [\"y\"]\n\"dashboard.up\" = [\"k\"]\n\n[future]\nkeep = 1\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(
            resolved["chat"]["copy_selection"],
            vec![
                Chord::char('y'),
                Chord::with(KeyCode::Char('c'), KeyModifiers::SUPER),
            ]
        );
        assert_eq!(resolved["dashboard"]["up"], vec![Chord::char('k')]);

        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        let copy: Vec<Chord> = doc["keybindings"]["chat.copy_selection"]
            .clone()
            .try_into()
            .unwrap();
        assert_eq!(copy, ChatTabAction::CopySelection.default_chords());
        assert_eq!(doc["future"]["keep"].as_integer(), Some(1));
    }

    #[test]
    fn customized_copy_binding_is_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"chat.copy_selection\" = [\"y\", \"alt+c\"]\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(
            resolved["chat"]["copy_selection"],
            vec![Chord::char('y'), "alt+c".parse::<Chord>().unwrap()]
        );
        assert!(read(dir.path()).contains("alt+c"));
    }

    #[test]
    fn bare_copy_binding_is_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"chat.copy_selection\" = \"y\"\n\"global.help\" = [\"?\", \"f1\", \"ctrl+f1\"]\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(resolved["chat"]["copy_selection"], vec![Chord::char('y')]);
        assert_eq!(
            resolved["global"]["help"],
            vec![Chord::char('?'), Chord::ctrl('g')]
        );

        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(
            doc["keybindings"]["chat.copy_selection"].as_str(),
            Some("y")
        );
    }

    #[test]
    fn legacy_chat_jump_defaults_migrate_without_touching_other_config() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"chat.jump_start\" = [\"g\"]\n\"chat.jump_end\" = [\"G\"]\n\"dashboard.up\" = [\"k\"]\n\n[future]\nkeep = 1\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(
            resolved["chat"]["jump_start"],
            ChatTabAction::JumpStart.default_chords()
        );
        assert_eq!(
            resolved["chat"]["jump_end"],
            ChatTabAction::JumpEnd.default_chords()
        );
        assert_eq!(resolved["dashboard"]["up"], vec![Chord::char('k')]);

        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["future"]["keep"].as_integer(), Some(1));
    }

    #[test]
    fn customized_chat_jump_bindings_are_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[keybindings]\n\"chat.jump_start\" = [\"alt+home\"]\n\"chat.jump_end\" = \"G\"\n",
        );

        let cfg = ensure_and_load(dir.path()).unwrap();
        let resolved = cfg.resolve_keybindings().unwrap();
        assert_eq!(
            resolved["chat"]["jump_start"],
            vec!["alt+home".parse::<Chord>().unwrap()]
        );
        assert_eq!(resolved["chat"]["jump_end"], vec![Chord::char('G')]);
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
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
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
        // Reads the environment via `ensure_and_load`; serialize against the
        // env-mutating tests so a stray override cannot leak in.
        let _guard = env_test_lock();
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

    // ── Todo tracker persistence tests ──────────────────────────────────────

    #[test]
    fn persist_todotracker_writes_section_and_preserves_others() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"nord\"\n\n[future]\nkeep = true\n",
        );
        let section = TodoTrackerSection {
            enabled: false,
            enabled_at_start: true,
            location: TodoTrackerLocation::Left,
            width: 40,
            max_height: 8,
        };
        persist_todotracker(dir.path(), &section).unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));
        assert_eq!(doc["todotracker"]["enabled"].as_bool(), Some(false));
        assert_eq!(doc["todotracker"]["enabled_at_start"].as_bool(), Some(true));
        assert_eq!(doc["todotracker"]["location"].as_str(), Some("left"));
        assert_eq!(doc["todotracker"]["width"].as_integer(), Some(40));
        assert_eq!(doc["todotracker"]["max_height"].as_integer(), Some(8));
    }

    #[test]
    fn persist_todotracker_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        let section = TodoTrackerSection {
            enabled: false,
            enabled_at_start: true,
            location: TodoTrackerLocation::Bottom,
            width: 48,
            max_height: 10,
        };
        persist_todotracker(dir.path(), &section).unwrap();
        let cfg = load_persisted(dir.path()).unwrap();
        assert!(!cfg.todotracker.enabled);
        assert!(cfg.todotracker.enabled_at_start);
        assert_eq!(cfg.todotracker.location, TodoTrackerLocation::Bottom);
        assert_eq!(cfg.todotracker.width, 48);
        assert_eq!(cfg.todotracker.max_height, 10);
    }

    #[test]
    fn persist_todotracker_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let section = TodoTrackerSection {
            enabled: false,
            ..Default::default()
        };
        persist_todotracker(dir.path(), &section).unwrap();
        let doc: toml::Table = toml::from_str(&read(dir.path())).unwrap();
        assert_eq!(doc["todotracker"]["enabled"].as_bool(), Some(false));
    }

    #[test]
    fn bad_todotracker_does_not_blank_theme() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"dracula\"\n\n[todotracker]\nwidth = \"not_a_number\"\n",
        );
        // Asserts on-disk parsing/fallback, so read the persisted view: the
        // effective view would also layer in any ambient `ZEROCODE_*` override.
        let cfg = load_persisted(dir.path()).unwrap();
        assert_eq!(cfg.theme.name, "dracula");
        assert_eq!(cfg.todotracker.width, default_todotracker_width());
    }

    // The tolerant `load_persisted` path substitutes defaults for a malformed
    // `[todotracker]`, but the transition resolver must instead see an error so
    // it can preserve the live tracker rather than silently reset to defaults.
    #[test]
    fn resolve_todo_tracker_checked_errors_on_malformed_section() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            "[theme]\nname = \"dracula\"\n\n[todotracker]\nwidth = \"not_a_number\"\n",
        );
        let err = resolve_todo_tracker_checked(dir.path())
            .expect_err("a malformed [todotracker] must surface as an error");
        assert!(
            format!("{err:#}").contains("[todotracker]")
                && format!("{err:#}").contains("malformed"),
            "error should name the malformed section, got: {err:#}"
        );
    }

    // A valid section resolves normally through the checked path.
    #[test]
    fn resolve_todo_tracker_checked_ok_on_valid_section() {
        // Resolves through `ensure_and_load`, which *reads* the environment, so
        // this must serialize against the env-mutating tests below even though
        // it sets nothing itself.
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), "[todotracker]\nwidth = 41\nmax_height = 6\n");
        let settings =
            resolve_todo_tracker_checked(dir.path()).expect("a valid [todotracker] must resolve");
        assert_eq!(settings.width, 41);
        assert_eq!(settings.max_height, 6);
    }

    // An absent section resolves to defaults (not an error).
    #[test]
    fn resolve_todo_tracker_checked_ok_when_section_absent() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), "[theme]\nname = \"dracula\"\n");
        let settings = resolve_todo_tracker_checked(dir.path())
            .expect("an absent [todotracker] must resolve to defaults");
        assert_eq!(settings.width, default_todotracker_width());
    }

    // ── Env override tests ──────────────────────────────────────────────────

    #[test]
    fn set_prop_todotracker_enabled() {
        let mut c = ZerocodeConfig::default();
        set_prop(&mut c, "todotracker.enabled", "false").unwrap();
        assert!(!c.todotracker.enabled);
    }

    #[test]
    fn set_prop_todotracker_width() {
        let mut c = ZerocodeConfig::default();
        set_prop(&mut c, "todotracker.width", "50").unwrap();
        assert_eq!(c.todotracker.width, 50);
    }

    #[test]
    fn set_prop_todotracker_location() {
        let mut c = ZerocodeConfig::default();
        set_prop(&mut c, "todotracker.location", "left").unwrap();
        assert_eq!(c.todotracker.location, TodoTrackerLocation::Left);
    }

    // ── Resolver validation / normalization (untrusted config boundary) ──────

    // ── Zero-dimension rejection ────────────────────────────────────────────
    //
    // Explicit zero `width`/`max_height` must fail *visibly* at the session
    // boundary rather than being silently normalized to 1 — for file values
    // and for canonical environment overrides alike. The Config-pane edit
    // path is covered separately by the pane's own save tests.

    #[test]
    fn resolve_todo_tracker_checked_rejects_zero_width_from_file() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_path(dir.path()),
            "[todotracker]\nwidth = 0\nmax_height = 5\n",
        )
        .unwrap();

        let err = resolve_todo_tracker_checked(dir.path())
            .expect_err("an explicit width = 0 in the file must fail, not normalize to 1");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("todotracker"),
            "error must name the offending section, got: {msg}"
        );
    }

    #[test]
    fn resolve_todo_tracker_checked_rejects_zero_max_height_from_file() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_path(dir.path()),
            "[todotracker]\nwidth = 32\nmax_height = 0\n",
        )
        .unwrap();

        assert!(
            resolve_todo_tracker_checked(dir.path()).is_err(),
            "an explicit max_height = 0 in the file must fail, not normalize to 1"
        );
    }

    #[test]
    fn resolve_todo_tracker_checked_rejects_zero_width_from_env() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        // File is valid; only the canonical env override carries the zero, so
        // this pins that validation happens *after* overrides are applied.
        std::fs::write(
            config_path(dir.path()),
            "[todotracker]\nwidth = 32\nmax_height = 5\n",
        )
        .unwrap();
        let _v = EnvVarGuard::set("ZEROCODE_todotracker__width", "0");

        assert!(
            resolve_todo_tracker_checked(dir.path()).is_err(),
            "ZEROCODE_todotracker__width=0 must fail visibly, not normalize to 1"
        );
    }

    #[test]
    fn resolve_todo_tracker_checked_rejects_zero_max_height_from_env() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let _v = EnvVarGuard::set("ZEROCODE_todotracker__max_height", "0");

        assert!(
            resolve_todo_tracker_checked(dir.path()).is_err(),
            "ZEROCODE_todotracker__max_height=0 must fail visibly, not normalize to 1"
        );
    }

    #[test]
    fn resolve_todo_tracker_checked_accepts_positive_dimensions() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_path(dir.path()),
            "[todotracker]\nwidth = 44\nmax_height = 9\n",
        )
        .unwrap();

        let s = resolve_todo_tracker_checked(dir.path())
            .expect("a valid section must still resolve unchanged");
        assert_eq!(s.width, 44);
        assert_eq!(s.max_height, 9);
    }

    // ── Single source of truth ──────────────────────────────────────────────
    //
    // `zerocode-config.toml` is the live source of truth for the tracker's
    // display settings: a value persisted by the Config pane must be visible
    // to the next session that resolves the file, never shadowed by a copy
    // cached at first-session start. Sessions resolve the file at each session
    // boundary, so this exercises that contract at the config layer: load,
    // persist a change (as the Config pane does), load again.
    #[test]
    fn resolved_settings_track_edits_across_sessions() {
        // Resolves the effective view, so serialize against the env-mutating
        // tests below: `std::env` is process-global.
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();

        // "Session 1" resolves the defaults.
        let cfg1 = ensure_and_load(dir.path()).unwrap();
        let t1 = cfg1.resolve_todo_tracker();

        // A Config-pane save persists a changed field to the same file.
        let mut tracker = cfg1.todotracker.clone();
        tracker.width = t1.width + 7;
        persist_todotracker(dir.path(), &tracker).unwrap();

        // "Session 2" must resolve the *edited* value, not a cached copy.
        let cfg2 = ensure_and_load(dir.path()).unwrap();
        let t2 = cfg2.resolve_todo_tracker();
        assert_eq!(
            t2.width,
            t1.width + 7,
            "second session must see the persisted tracker width edit"
        );
    }
    // ── Environment override contract ───────────────────────────────────────
    //
    // Canonical spelling is `ZEROCODE_<lowercase dotted path with __>`, e.g.
    // `ZEROCODE_todotracker__enabled`. These exercise real environment
    // variables through `ensure_and_load` rather than calling `set_prop`
    // directly, so the public spelling stays covered.

    use crate::test_support::{EnvVarGuard, env_test_lock};

    #[test]
    fn canonical_env_spelling_overrides_tracker_field() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let _v = EnvVarGuard::set("ZEROCODE_todotracker__enabled", "false");

        let cfg = ensure_and_load(dir.path()).unwrap();
        assert!(
            !cfg.todotracker.enabled,
            "ZEROCODE_todotracker__enabled must reach the parser and win over the file"
        );
    }

    #[test]
    fn canonical_env_spelling_overrides_tracker_width() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let _v = EnvVarGuard::set("ZEROCODE_todotracker__width", "41");

        let cfg = ensure_and_load(dir.path()).unwrap();
        assert_eq!(cfg.resolve_todo_tracker().width, 41);
    }

    #[test]
    fn unknown_env_path_is_rejected() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let _v = EnvVarGuard::set("ZEROCODE_todotracker__nope", "1");

        let err = ensure_and_load(dir.path()).expect_err("unknown path must hard-error");
        assert!(format!("{err:#}").contains("did not resolve to a config field"));
    }

    // Env overrides are process-transient: `load_persisted` is the on-disk
    // truth and must not see them, otherwise a Config-pane save of one field
    // would bake an env-injected value for another field into the file.
    #[test]
    fn env_overrides_do_not_reach_the_persisted_view() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let on_disk = TodoTrackerSection {
            width: 30,
            ..TodoTrackerSection::default()
        };
        persist_todotracker(dir.path(), &on_disk).unwrap();

        let _v = EnvVarGuard::set("ZEROCODE_todotracker__width", "77");

        // Effective view honors the override...
        assert_eq!(ensure_and_load(dir.path()).unwrap().todotracker.width, 77);
        // ...while the persisted view keeps the real file contents.
        assert_eq!(load_persisted(dir.path()).unwrap().todotracker.width, 30);
    }

    // Saving an unrelated field must rewrite the section from *disk* state, so
    // an env-injected value for a different field is never written through.
    #[test]
    fn saving_unrelated_field_does_not_persist_env_value() {
        let _guard = env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let on_disk = TodoTrackerSection {
            width: 30,
            max_height: 5,
            ..TodoTrackerSection::default()
        };
        persist_todotracker(dir.path(), &on_disk).unwrap();

        let _v = EnvVarGuard::set("ZEROCODE_todotracker__width", "77");

        // Edit an unrelated field starting from the persisted section.
        let mut candidate = load_persisted(dir.path()).unwrap().todotracker;
        candidate.max_height = 9;
        persist_todotracker(dir.path(), &candidate).unwrap();

        let reloaded = load_persisted(dir.path()).unwrap().todotracker;
        assert_eq!(reloaded.max_height, 9, "the edited field must be saved");
        assert_eq!(
            reloaded.width, 30,
            "the env-injected width must not be written to disk"
        );
    }
}
