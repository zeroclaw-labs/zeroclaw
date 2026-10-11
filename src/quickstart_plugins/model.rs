//! Pure half of the Quickstart plugin step.
//!
//! Everything here is a function of values the caller already holds: the
//! package catalog, a manifest's `config_schema`, its text and the trusted
//! publisher keys, an instance's config row, the activation flags and the
//! installed package list. Nothing reads the terminal, the network, the
//! plugins directory or the config file, so every rule is unit tested
//! directly. The interactive half in the parent module owns each prompt and
//! each write.

use std::collections::BTreeSet;

use serde_json::Value;
use zeroclaw::plugins::catalog::PluginCatalogEntry;
use zeroclaw::plugins::registry::PluginRegistryEntry;
use zeroclaw::plugins::signature::{VerificationResult, verify_manifest};
use zeroclaw::plugins::{PluginCapability, PluginInfo, PluginManifest, PluginPermission};
use zeroclaw_config::traits::PropKind;
use zeroclaw_runtime::quickstart::FieldDescriptor;

use crate::plugins::channel_instance::RequiredKey;

/// The longest piece of publisher text Quickstart prints in one place, in
/// characters.
pub(crate) const TERMINAL_TEXT_MAX_CHARS: usize = 200;

/// The longest diagnostic Quickstart prints on one line, in characters. Error
/// chains quote manifest fields, so they are cleaned like publisher text, with
/// room for a whole load-check diagnostic.
pub(crate) const TERMINAL_DETAIL_MAX_CHARS: usize = 2000;

/// How deep a `$ref` chain is followed to find a property's type; the same
/// bound admission applies to the schema.
const MAX_REF_DEPTH: usize = 32;

/// How much of a verified publisher key the package summary prints, in
/// characters: enough to tell trusted keys apart at a glance.
pub(crate) const PUBLISHER_KEY_DISPLAY_CHARS: usize = 16;

/// Make publisher-controlled text safe to print on the operator's terminal.
///
/// Registry and manifest text (names, descriptions, authors, property names,
/// declared hosts) comes from whoever published the package. Printed raw, an
/// escape sequence in it could recolor text, move the cursor, rewrite earlier
/// lines or set the window title, and a bidirectional override could make one
/// name read as another. This removes escape sequences whole (not just the
/// escape byte, which would leave `[31m` behind), drops the remaining control
/// characters and invisible formatting characters, collapses each run of
/// whitespace to one space, trims both ends, and caps the result at
/// [`TERMINAL_TEXT_MAX_CHARS`] characters with a trailing ellipsis.
#[must_use]
pub(crate) fn terminal_safe(text: &str) -> String {
    clean_for_terminal(text, TERMINAL_TEXT_MAX_CHARS)
}

/// [`terminal_safe`] for a diagnostic: the same cleaning, capped at
/// [`TERMINAL_DETAIL_MAX_CHARS`], with line breaks folded into spaces.
#[must_use]
pub(crate) fn terminal_safe_detail(text: &str) -> String {
    clean_for_terminal(text, TERMINAL_DETAIL_MAX_CHARS)
}

fn clean_for_terminal(text: &str, max_chars: usize) -> String {
    let mut cleaned = String::with_capacity(text.len().min(max_chars.saturating_mul(4)));
    let mut chars = text.chars().peekable();
    let mut gap = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => skip_escape_sequence(&mut chars),
            // The 8-bit forms of CSI, DCS, SOS, OSC, PM and APC.
            '\u{9b}' => skip_control_sequence(&mut chars),
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => skip_control_string(&mut chars),
            c if c.is_whitespace() || c.is_control() => gap = true,
            c if is_invisible_format(c) => {}
            c => {
                if gap && !cleaned.is_empty() {
                    cleaned.push(' ');
                }
                gap = false;
                cleaned.push(c);
            }
        }
    }
    cap_chars(cleaned, max_chars)
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Skip what follows an ESC: a control sequence, a control string, or a short
/// escape with optional intermediate bytes.
fn skip_escape_sequence(chars: &mut Chars<'_>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_control_sequence(chars);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            skip_control_string(chars);
        }
        Some(_) => {
            while chars.next_if(|c| (' '..='/').contains(c)).is_some() {}
            chars.next();
        }
        None => {}
    }
}

/// Skip a control sequence's parameter and intermediate bytes, then its final
/// byte when one follows.
fn skip_control_sequence(chars: &mut Chars<'_>) {
    while chars.next_if(|c| (' '..='?').contains(c)).is_some() {}
    chars.next_if(|c| ('@'..='~').contains(c));
}

/// Skip a control string through its terminator: BEL, the 8-bit string
/// terminator, or ESC `\`. An unterminated string runs to the end of the text.
fn skip_control_string(chars: &mut Chars<'_>) {
    while let Some(c) = chars.next() {
        match c {
            '\u{7}' | '\u{9c}' => return,
            '\u{1b}' => {
                chars.next_if_eq(&'\\');
                return;
            }
            _ => {}
        }
    }
}

/// Characters that render as nothing but change how the text around them
/// displays: bidirectional marks, embeddings, overrides and isolates, and
/// zero-width characters.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{61c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

fn cap_chars(text: String, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        None => text,
        Some((cut, _)) => {
            let mut capped = text[..cut].trim_end().to_string();
            capped.push('…');
            capped
        }
    }
}

/// How a package offered in the Plugins row relates to this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChoiceState {
    /// In the registry and not installed.
    Available,
    /// Installed, and the registry lists the same version or none at all.
    Installed,
    /// Installed, and the registry lists a different version. Quickstart
    /// never upgrades: the installed version is the one it activates.
    InstalledOtherVersion {
        /// The version an unpinned `plugin install` would resolve.
        registry_version: String,
    },
}

/// One row of the Plugins multi-select.
///
/// This is a per-visit view of the catalog, never persisted: Quickstart stores
/// no selection record, and at Create the engine re-reads the plugins
/// directory rather than trusting `state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginChoice {
    pub(crate) name: String,
    /// The installed version when installed, else the registry version.
    pub(crate) version: String,
    pub(crate) description: Option<String>,
    pub(crate) state: ChoiceState,
    /// The entry an unpinned `plugin install` would resolve, when listed.
    pub(crate) registry_entry: Option<PluginRegistryEntry>,
}

impl PluginChoice {
    /// Whether the package was installed when the row was opened.
    #[must_use]
    pub(crate) fn is_installed(&self) -> bool {
        !matches!(self.state, ChoiceState::Available)
    }
}

/// The Plugins row's choices: one per package that is, or may be, a tool.
///
/// Built from the package catalog, so package identity and registry version
/// selection are the catalog's own. An installed package counts by its
/// admitted manifest. A registry-only package counts unless its registry
/// metadata lists capabilities without `tool`; that metadata is only a hint,
/// so the downloaded manifest is checked again before anything is installed.
#[must_use]
pub(crate) fn plugin_choices(catalog: &[PluginCatalogEntry<'_>]) -> Vec<PluginChoice> {
    catalog
        .iter()
        .filter_map(|entry| {
            let installed = entry.installed();
            let available = entry.available();
            let is_tool = match (installed, available) {
                (Some(info), _) => info.capabilities.contains(&PluginCapability::Tool),
                (None, Some(listed)) => {
                    listed.capabilities.is_empty()
                        || listed
                            .capabilities
                            .iter()
                            .any(|capability| capability.eq_ignore_ascii_case("tool"))
                }
                (None, None) => false,
            };
            if !is_tool {
                return None;
            }
            let (version, state) = match (installed, available) {
                (Some(info), Some(listed)) if info.version != listed.version => (
                    info.version.clone(),
                    ChoiceState::InstalledOtherVersion {
                        registry_version: listed.version.clone(),
                    },
                ),
                (Some(info), _) => (info.version.clone(), ChoiceState::Installed),
                (None, Some(listed)) => (listed.version.clone(), ChoiceState::Available),
                (None, None) => return None,
            };
            let description = installed
                .and_then(|info| info.description.clone())
                .or_else(|| available.and_then(|listed| listed.description.clone()));
            Some(PluginChoice {
                name: entry.name().to_string(),
                version,
                description,
                state,
                registry_entry: available.cloned(),
            })
        })
        .collect()
}

/// The JSON type a config property resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueKind {
    String,
    Boolean,
    Integer,
    Number,
    Array,
    Object,
}

impl ValueKind {
    fn from_schema_type(kind: &str) -> Option<Self> {
        match kind {
            "string" => Some(Self::String),
            "boolean" => Some(Self::Boolean),
            "integer" => Some(Self::Integer),
            "number" => Some(Self::Number),
            "array" => Some(Self::Array),
            "object" => Some(Self::Object),
            _ => None,
        }
    }
}

/// One configurable property of a plugin instance, as Quickstart prompts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigField {
    /// The property name, a portable plugin key.
    pub(crate) key: String,
    pub(crate) kind: ValueKind,
    pub(crate) required: bool,
    /// Marked `x-secret`: prompted masked and stored encrypted.
    pub(crate) secret: bool,
    /// Closed set of stored values to pick from, the default first.
    pub(crate) choices: Option<Vec<String>>,
    /// Terminal-safe description from the schema.
    pub(crate) description: Option<String>,
    /// The schema's scalar default, as it would be stored.
    pub(crate) default: Option<String>,
}

/// The root properties of a `config_schema`, split into the ones Quickstart
/// prompts for and the ones it cannot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigFields {
    /// Required properties first, then optional ones, each group by name.
    pub(crate) fields: Vec<ConfigField>,
    /// Terminal-safe names of properties that are not portable plugin keys or
    /// whose type does not resolve; they are reported, never prompted.
    pub(crate) unsupported: Vec<String>,
    /// Terminal-safe names in the schema's root `required` list that no field
    /// prompts for: a property in `unsupported`, or a name the root
    /// `properties` map does not declare. While any is listed, no answers to
    /// the prompts can satisfy the schema. Sorted, without duplicates.
    pub(crate) required_unprompted: Vec<String>,
    /// The names in `required_unprompted` that the root `properties` map does
    /// not declare. Admission requires `additionalProperties = false` and the
    /// resolver refuses a property the map does not declare, so a row can
    /// neither hold such a name nor leave it out: while any is listed, no
    /// config entry satisfies the schema, whatever `config set` writes.
    /// Sorted, without duplicates.
    pub(crate) required_undeclared: Vec<String>,
}

/// Map a manifest `config_schema` to the fields Quickstart prompts for.
///
/// Only root `properties` whose names pass the portable plugin key grammar
/// are prompted. Those are the names Quickstart prints a `config set
/// plugins.entries.<key>.config.<name>` command for, since they are safe to
/// show on a terminal and to paste into a shell, and the only names a secret
/// property can have. `config set` itself accepts any declared name the
/// operator quotes.
#[must_use]
pub(crate) fn config_fields(schema: &Value) -> ConfigFields {
    let required: BTreeSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = ConfigFields::default();
    let properties = schema.get("properties").and_then(Value::as_object);
    for (name, property) in properties.into_iter().flatten() {
        let resolved = resolve_ref(schema, property);
        let kind = resolved
            .and_then(|node| node.get("type"))
            .and_then(Value::as_str)
            .and_then(ValueKind::from_schema_type);
        let (true, Some(node), Some(kind)) = (
            zeroclaw_api::plugin_key::is_valid_portable_plugin_key(name),
            resolved,
            kind,
        ) else {
            out.unsupported.push(terminal_safe(name));
            continue;
        };
        let secret = property.get("x-secret") == Some(&Value::Bool(true));
        let default = if secret {
            None
        } else {
            property
                .get("default")
                .or_else(|| node.get("default"))
                .and_then(stored_scalar)
                .filter(|value| is_terminal_safe(value))
        };
        let choices = choice_values(node, kind, default.as_deref());
        let description = property
            .get("description")
            .or_else(|| node.get("description"))
            .and_then(Value::as_str)
            .map(terminal_safe)
            .filter(|text| !text.is_empty());
        out.fields.push(ConfigField {
            key: name.clone(),
            kind,
            required: required.contains(name.as_str()),
            secret,
            choices,
            description,
            default,
        });
    }
    out.fields.sort_by(|left, right| {
        right
            .required
            .cmp(&left.required)
            .then_with(|| left.key.cmp(&right.key))
    });
    out.unsupported.sort();
    let prompted: BTreeSet<&str> = out.fields.iter().map(|field| field.key.as_str()).collect();
    let unprompted: Vec<&str> = required
        .into_iter()
        .filter(|name| !prompted.contains(name))
        .collect();
    let required_unprompted: BTreeSet<String> =
        unprompted.iter().copied().map(terminal_safe).collect();
    let required_undeclared: BTreeSet<String> = unprompted
        .into_iter()
        .filter(|name| !properties.is_some_and(|declared| declared.contains_key(*name)))
        .map(terminal_safe)
        .collect();
    out.required_unprompted = required_unprompted.into_iter().collect();
    out.required_undeclared = required_undeclared.into_iter().collect();
    out
}

/// Follow a property's local `$ref` chain to the node that declares its type.
fn resolve_ref<'a>(root: &'a Value, property: &'a Value) -> Option<&'a Value> {
    let mut node = property;
    for _ in 0..=MAX_REF_DEPTH {
        if node.get("type").is_some() {
            return Some(node);
        }
        let pointer = node.get("$ref")?.as_str()?.strip_prefix('#')?;
        node = root.pointer(pointer)?;
    }
    None
}

/// The string a scalar schema value is stored as, or `None` for a value that
/// is not a scalar.
fn stored_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn is_terminal_safe(text: &str) -> bool {
    terminal_safe(text) == text
}

/// The closed set a property is picked from: its `enum`, or both booleans.
///
/// A set with a value that is not a scalar, or that would not print as
/// itself, is not offered as a list; the value is typed instead and the
/// schema still validates it.
fn choice_values(node: &Value, kind: ValueKind, default: Option<&str>) -> Option<Vec<String>> {
    let mut values: Vec<String> = match node.get("enum").and_then(Value::as_array) {
        Some(listed) => listed.iter().map(stored_scalar).collect::<Option<_>>()?,
        None if kind == ValueKind::Boolean => vec!["true".to_string(), "false".to_string()],
        None => return None,
    };
    if values.is_empty() || !values.iter().all(|value| is_terminal_safe(value)) {
        return None;
    }
    // The field prompt preselects the first choice, so the default leads.
    if let Some(default) = default
        && let Some(at) = values.iter().position(|value| value == default)
    {
        let value = values.remove(at);
        values.insert(0, value);
    }
    Some(values)
}

/// The shared Quickstart field prompt's descriptor for one property.
///
/// `hint` is appended to the schema description; the caller passes the
/// localized "enter JSON" hint for array and object properties.
#[must_use]
pub(crate) fn field_descriptor(field: &ConfigField, hint: Option<&str>) -> FieldDescriptor {
    let help = match (field.description.as_deref(), hint) {
        (Some(description), Some(hint)) => format!("{description} {hint}"),
        (Some(text), None) | (None, Some(text)) => text.to_string(),
        (None, None) => String::new(),
    };
    let kind = if field.choices.is_some() {
        PropKind::Enum
    } else {
        match field.kind {
            ValueKind::Integer => PropKind::Integer,
            ValueKind::Number => PropKind::Float,
            ValueKind::String | ValueKind::Boolean | ValueKind::Array | ValueKind::Object => {
                PropKind::String
            }
        }
    };
    FieldDescriptor {
        key: field.key.clone(),
        label: field.key.clone(),
        help,
        kind,
        is_secret: field.secret,
        enum_variants: field.choices.clone(),
        required: field.required,
        default: field.default.clone(),
    }
}

/// The string `zeroclaw config set plugins.entries.<key>.config.<name>` stores
/// for one answer, or `None` when the answer leaves the property unset.
///
/// Instance config is a string map typed at resolution time: a string
/// property is stored as typed, and every other type is JSON text (`true`,
/// `42`, `1.5`, `["a", "b"]`, `{"k": "v"}`) the resolver parses against the
/// schema. Surrounding whitespace is dropped from JSON text, and from a
/// secret, which `config set` reads through a masked prompt and trims, so a
/// pasted token's stray space or line break never becomes part of it. Any
/// other string keeps it.
#[must_use]
pub(crate) fn encode_value(field: &ConfigField, raw: &str) -> Option<String> {
    let value = match field.kind {
        ValueKind::String if field.secret => raw.trim(),
        ValueKind::String => raw,
        ValueKind::Boolean
        | ValueKind::Integer
        | ValueKind::Number
        | ValueKind::Array
        | ValueKind::Object => raw.trim(),
    };
    (!value.is_empty()).then(|| value.to_string())
}

/// What an instance's config row leaves unmet of the settings its schema
/// requires, split the way the status after Create prints it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnmetRequiredSettings {
    /// Required settings the row leaves unset whose names the root
    /// `properties` map declares and that are portable plugin keys, so
    /// Quickstart prints the `config set plugins.entries.<key>.config.<name>`
    /// command for each one.
    pub(crate) portable: Vec<String>,
    /// Required settings the row leaves unset that the root `properties` map
    /// declares under a name outside the portable plugin key grammar.
    ///
    /// `config set` writes such a setting when the operator quotes its path,
    /// and the resolver reads it like any other. Quickstart prints commands
    /// only for portable names, the ones that are safe to show on a terminal
    /// and to paste into a shell, so it names these without a command: the
    /// operator sets them in the config file or with a `config set` command
    /// they quote themselves. Publisher text, so each is made terminal-safe.
    pub(crate) nonportable: Vec<String>,
    /// The names the schema's root `required` list holds that its root
    /// `properties` map does not declare, set or not.
    ///
    /// The resolver refuses a row holding a name the map does not declare,
    /// and the schema refuses a row without a required one, so no row
    /// satisfies the schema and nothing the operator sets makes the instance
    /// usable. Quickstart names them without a command. Publisher text, so
    /// each is made terminal-safe.
    pub(crate) undeclared: Vec<String>,
}

/// Split `keys`, what [`crate::plugins::channel_instance::required_keys`]
/// reports for `manifest`'s schema and an instance's row, into what the row
/// leaves unmet.
///
/// The per-key facts are that report's, which the channel binding ceremony
/// prints from too: whether the row holds the key, whatever its value, and
/// whether the name is a portable plugin key. This adds only whether the
/// root `properties` map declares the name. Values are never read here. Each
/// list is sorted, without duplicates.
///
/// This only names what to set. Whether the instance can use its row is the
/// runtime resolver's decision: a row this reports complete can still hold a
/// value its schema rejects.
#[must_use]
pub(crate) fn unmet_required_settings(
    manifest: &PluginManifest,
    keys: &[RequiredKey],
) -> UnmetRequiredSettings {
    let properties = manifest
        .config_schema
        .as_ref()
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object);
    let mut portable = BTreeSet::new();
    let mut nonportable = BTreeSet::new();
    let mut undeclared = BTreeSet::new();
    for key in keys {
        if !properties.is_some_and(|properties| properties.contains_key(&key.name)) {
            undeclared.insert(terminal_safe(&key.name));
        } else if !key.set {
            if key.addressable {
                portable.insert(key.name.clone());
            } else {
                nonportable.insert(terminal_safe(&key.name));
            }
        }
    }
    UnmetRequiredSettings {
        portable: portable.into_iter().collect(),
        nonportable: nonportable.into_iter().collect(),
        undeclared: undeclared.into_iter().collect(),
    }
}

/// Snake-case names of a manifest's capabilities, as manifests spell them.
#[must_use]
pub(crate) fn capability_names(capabilities: &[PluginCapability]) -> Vec<String> {
    capabilities.iter().filter_map(serde_name).collect()
}

/// Snake-case names of a manifest's requested permissions.
#[must_use]
pub(crate) fn permission_names(permissions: &[PluginPermission]) -> Vec<String> {
    permissions.iter().filter_map(serde_name).collect()
}

fn serde_name(value: &impl serde::Serialize) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// What a package's manifest signature says about who published it, as the
/// package summary reports it.
///
/// The registry's archive digest proves only that the download matches the
/// index. Who built the package is the manifest signature's question, and
/// Quickstart answers it for every package it summarizes, whatever
/// `plugins.security.signature_mode` is. The mode still decides what
/// admission refuses; this decides what the operator is told and whether they
/// are asked before an install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignatureVerdict {
    /// Signed by a key in `plugins.security.trusted_publisher_keys`, and the
    /// manifest verifies against the signature.
    Verified {
        /// The trusted key, in the lowercase hex the verifier normalizes it
        /// to.
        publisher_key: String,
    },
    /// No signature, or no publisher key to check one against: admission
    /// reads either as unsigned.
    Unsigned,
    /// Signed under a publisher key the trusted list does not hold. Any key
    /// can sign, so the signature itself is not checked: it shows nothing
    /// about who published the package.
    Untrusted,
    /// Signed under a trusted key, but the manifest does not verify against
    /// the signature it carries: it changed after signing, or the signature
    /// or the key is malformed.
    Invalid,
}

impl SignatureVerdict {
    /// Whether a trusted publisher's signature covers the manifest, which
    /// makes the summary's author line more than the publisher's own claim.
    #[must_use]
    pub(crate) fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    fn from_verification(result: VerificationResult) -> Self {
        match result {
            VerificationResult::Valid { publisher_key } => Self::Verified { publisher_key },
            VerificationResult::Unsigned => Self::Unsigned,
            VerificationResult::Untrusted => Self::Untrusted,
            VerificationResult::Invalid { .. } => Self::Invalid,
        }
    }
}

/// The [`SignatureVerdict`] for `manifest` under `trusted_keys`, the
/// operator's `plugins.security.trusted_publisher_keys`.
///
/// `manifest_toml` is the text `manifest` was parsed from, the bytes
/// admission read, so the verdict covers exactly what gets installed. A
/// manifest without both signature fields is unsigned, as the admission
/// policy reads it; any other is decided by the plugin crate's own verifier.
#[must_use]
pub(crate) fn signature_verdict(
    manifest: &PluginManifest,
    manifest_toml: &str,
    trusted_keys: &[String],
) -> SignatureVerdict {
    match (
        manifest.signature.as_deref(),
        manifest.publisher_key.as_deref(),
    ) {
        (Some(signature), Some(publisher_key)) => SignatureVerdict::from_verification(
            verify_manifest(manifest_toml, signature, publisher_key, trusted_keys),
        ),
        _ => SignatureVerdict::Unsigned,
    }
}

/// A verified publisher key as the package summary prints it: terminal-safe,
/// cut to its first [`PUBLISHER_KEY_DISPLAY_CHARS`] characters with an
/// ellipsis.
#[must_use]
pub(crate) fn short_publisher_key(publisher_key: &str) -> String {
    clean_for_terminal(publisher_key, PUBLISHER_KEY_DISPLAY_CHARS)
}

/// An enabled `[channels.plugin.<alias>]` declaration whose package is an
/// installed channel plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelBinding {
    pub(crate) alias: String,
    pub(crate) package: String,
}

/// What the activation preview reads: canonical flags and installed packages.
pub(crate) struct ActivationInputs<'a> {
    pub(crate) plugins_enabled: bool,
    pub(crate) auto_discover: bool,
    pub(crate) installed: &'a [PluginInfo],
    pub(crate) channel_bindings: &'a [ChannelBinding],
}

/// One plugin instance that turning activation on would make active.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActivatedInstance {
    Tool { package: String },
    Skill { package: String },
    Channel { alias: String, package: String },
}

impl ActivatedInstance {
    /// The installed package the instance runs.
    #[must_use]
    pub(crate) fn package(&self) -> &str {
        match self {
            Self::Tool { package } | Self::Skill { package } | Self::Channel { package, .. } => {
                package
            }
        }
    }
}

/// The flag changes activation needs, and everything they would activate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivationPreview {
    /// `plugins.enabled` is off and would be turned on.
    pub(crate) enable_plugins: bool,
    /// `plugins.auto_discover` is off and would be turned on.
    pub(crate) enable_auto_discover: bool,
    /// Every instance the flag changes would activate, sorted.
    pub(crate) activated: Vec<ActivatedInstance>,
}

impl ActivationPreview {
    /// Nothing to ask: both flags are already on.
    #[must_use]
    pub(crate) fn changes_nothing(&self) -> bool {
        !self.enable_plugins && !self.enable_auto_discover
    }

    /// Whether everything this would activate is a package in `packages`.
    ///
    /// When it is, the list equals what the operator just selected and
    /// activation is the expected next step. When it is not, turning the flags
    /// on would also wake packages or channels installed earlier, so the
    /// consent prompt defaults to no.
    #[must_use]
    pub(crate) fn activates_only(&self, packages: &BTreeSet<String>) -> bool {
        self.activated.iter().all(|instance| match instance {
            ActivatedInstance::Tool { package } | ActivatedInstance::Skill { package } => {
                packages.contains(package)
            }
            ActivatedInstance::Channel { .. } => false,
        })
    }
}

/// What turning plugin activation on would change, from canonical flags plus
/// the installed packages.
///
/// `plugins.auto_discover` is one global switch: turning it on activates every
/// installed tool and skill package, not only the ones selected in this run,
/// so the preview lists all of them. While either flag is off no
/// auto-discovered instance is active, so all of them are new. Turning
/// `plugins.enabled` on also brings up every enabled channel declaration.
/// The shared `plugins.max_active_instances` ceiling can still hold some
/// back; the per-package readiness report after Create names the exact
/// verdict.
#[must_use]
pub(crate) fn activation_preview(inputs: &ActivationInputs<'_>) -> ActivationPreview {
    let enable_plugins = !inputs.plugins_enabled;
    let enable_auto_discover = !inputs.auto_discover;
    let mut activated = Vec::new();
    if enable_plugins || enable_auto_discover {
        for info in inputs.installed {
            if info.capabilities.contains(&PluginCapability::Tool) {
                activated.push(ActivatedInstance::Tool {
                    package: info.name.clone(),
                });
            }
            if info.capabilities.contains(&PluginCapability::Skill) {
                activated.push(ActivatedInstance::Skill {
                    package: info.name.clone(),
                });
            }
        }
    }
    if enable_plugins {
        activated.extend(inputs.channel_bindings.iter().map(|binding| {
            ActivatedInstance::Channel {
                alias: binding.alias.clone(),
                package: binding.package.clone(),
            }
        }));
    }
    activated.sort();
    ActivationPreview {
        enable_plugins,
        enable_auto_discover,
        activated,
    }
}

/// Why Quickstart would not install a selected package. Nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The registry entry carries no archive digest to verify against.
    NoIntegrityHash,
    /// The downloaded manifest does not provide a tool.
    NotATool,
    /// Not installed any more, and not in the registry.
    NotAvailable,
}

impl Refusal {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoIntegrityHash => "no_integrity_hash",
            Self::NotATool => "not_a_tool",
            Self::NotAvailable => "not_available",
        }
    }
}

/// The step a package failed at. Nothing changed for the package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureStage {
    Download,
    Admission,
    LoadCheck,
    Publish,
}

impl FailureStage {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Admission => "admission",
            Self::LoadCheck => "load_check",
            Self::Publish => "publish",
        }
    }
}

/// What the Create-time plugin phase did with one selected package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackageOutcome {
    /// This run published it through the canonical publish-and-seed
    /// transaction.
    Installed { name: String },
    /// It was installed before this run and left as it was; `seeded_row` is
    /// whether this run created its missing config row.
    AlreadyInstalled { name: String, seeded_row: bool },
    /// It was installed before this run without its config row, and the
    /// operator skipped creating one. Nothing changed: it stays installed
    /// without the row.
    AlreadyInstalledSkipped { name: String },
    /// It was installed before this run, and setting it up failed: its
    /// missing config row could not be created, or no plugin host could be
    /// built to check it. Nothing changed: it stays installed as it was.
    AlreadyInstalledSetupFailed { name: String },
    /// The operator chose not to install it. Nothing changed.
    Skipped { name: String },
    /// Quickstart would not install it. Nothing changed.
    Refused { name: String, reason: Refusal },
    /// A step failed before anything durable happened for it.
    Failed { name: String, stage: FailureStage },
    /// Its publish-and-seed failed and undoing the publish failed too, so
    /// the package stays in the plugins directory, possibly without its
    /// config row, until it is removed by hand. A change this run made.
    RollbackFailed { name: String },
}

impl PackageOutcome {
    #[must_use]
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Installed { name }
            | Self::AlreadyInstalled { name, .. }
            | Self::AlreadyInstalledSkipped { name }
            | Self::AlreadyInstalledSetupFailed { name }
            | Self::Skipped { name }
            | Self::Refused { name, .. }
            | Self::Failed { name, .. }
            | Self::RollbackFailed { name } => name,
        }
    }

    /// Whether the package is installed after this run, whoever installed it.
    ///
    /// A package whose rollback failed is left out: it is on disk, but its
    /// install failed, so it is reported only as a change to remove, and it
    /// never brings up the activation question by itself.
    #[must_use]
    pub(crate) fn is_installed(&self) -> bool {
        matches!(
            self,
            Self::Installed { .. }
                | Self::AlreadyInstalled { .. }
                | Self::AlreadyInstalledSkipped { .. }
                | Self::AlreadyInstalledSetupFailed { .. }
        )
    }

    /// Whether the package was installed before this run, which therefore
    /// never ran the install-time load check on it.
    #[must_use]
    pub(crate) fn installed_before_run(&self) -> bool {
        matches!(
            self,
            Self::AlreadyInstalled { .. }
                | Self::AlreadyInstalledSkipped { .. }
                | Self::AlreadyInstalledSetupFailed { .. }
        )
    }

    /// Whether the operator went ahead with the package: this run installed
    /// it, or kept it as installed before. Activating exactly these packages
    /// is the expected next step; a skipped one, or one whose setup failed,
    /// is not among them.
    #[must_use]
    pub(crate) fn is_accepted(&self) -> bool {
        matches!(self, Self::Installed { .. } | Self::AlreadyInstalled { .. })
    }

    /// Whether this run changed the plugins directory or the config for it.
    #[must_use]
    pub(crate) fn changed_state(&self) -> bool {
        matches!(
            self,
            Self::Installed { .. }
                | Self::AlreadyInstalled {
                    seeded_row: true,
                    ..
                }
                | Self::RollbackFailed { .. }
        )
    }

    /// Whether this run put the package into the plugins directory, so
    /// removing it undoes the run: it published it, or failed to undo its
    /// publish.
    #[must_use]
    pub(crate) fn published_by_run(&self) -> bool {
        matches!(self, Self::Installed { .. } | Self::RollbackFailed { .. })
    }

    /// Stable label for log events.
    #[must_use]
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Installed { .. } => "installed",
            Self::AlreadyInstalled { .. } => "already_installed",
            Self::AlreadyInstalledSkipped { .. } => "already_installed_skipped",
            Self::AlreadyInstalledSetupFailed { .. } => "already_installed_setup_failed",
            Self::Skipped { .. } => "skipped",
            Self::Refused { reason, .. } => reason.as_str(),
            Self::Failed { stage, .. } => stage.as_str(),
            Self::RollbackFailed { .. } => "rollback_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use zeroclaw::plugins::catalog::package_catalog;
    use zeroclaw::plugins::registry::PluginRegistryIndex;

    use crate::config::schema::PluginEntryConfig;
    use crate::plugins::channel_instance::required_keys;
    use crate::quickstart_plugins::tests::Publisher;

    fn installed(name: &str, version: &str, capabilities: &[PluginCapability]) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            version: version.to_string(),
            description: Some(format!("installed {name}")),
            capabilities: capabilities.to_vec(),
            permissions: Vec::new(),
            wasm_path: Some(PathBuf::from("plugin.wasm")),
            loaded: true,
        }
    }

    fn listed(name: &str, version: &str, capabilities: &[&str]) -> PluginRegistryEntry {
        PluginRegistryEntry {
            name: name.to_string(),
            version: version.to_string(),
            description: Some(format!("listed {name}")),
            author: None,
            capabilities: capabilities.iter().map(|c| (*c).to_string()).collect(),
            url: format!("https://example.invalid/{name}-{version}.zip"),
            sha256: Some("00".repeat(32)),
        }
    }

    #[test]
    fn terminal_safe_removes_escape_sequences_whole_and_control_bytes() {
        assert_eq!(
            terminal_safe("\u{1b}[31mred\u{1b}[0m alert\u{7}\u{0}"),
            "red alert",
            "a color sequence must vanish with its parameters, not leave `[31m` behind"
        );
        assert_eq!(
            terminal_safe("\u{1b}]0;owned title\u{7}name"),
            "name",
            "a window-title string must vanish through its terminator"
        );
        assert_eq!(terminal_safe("\u{9b}2Jclear"), "clear");
        assert_eq!(terminal_safe("tool\u{202e}loot"), "toolloot");
        assert_eq!(terminal_safe("  one\n\ttwo \r\n three  "), "one two three");
    }

    #[test]
    fn terminal_safe_caps_long_text_with_an_ellipsis() {
        let capped = terminal_safe(&"x".repeat(TERMINAL_TEXT_MAX_CHARS * 3));
        assert_eq!(capped.chars().count(), TERMINAL_TEXT_MAX_CHARS + 1);
        assert!(capped.ends_with('…'));
        let exact = "y".repeat(TERMINAL_TEXT_MAX_CHARS);
        assert_eq!(
            terminal_safe(&exact),
            exact,
            "text at the cap is kept whole"
        );
    }

    #[test]
    fn terminal_safe_detail_cleans_a_diagnostic_but_keeps_it_long() {
        let diagnostic = format!(
            "manifest name '\u{1b}[1mevil\u{1b}[0m' does not match:\n{}",
            "d".repeat(TERMINAL_TEXT_MAX_CHARS * 2)
        );
        let cleaned = terminal_safe_detail(&diagnostic);
        assert!(cleaned.starts_with("manifest name 'evil' does not match: d"));
        assert!(!cleaned.contains('\u{1b}') && !cleaned.contains('\n'));
        assert!(
            cleaned.chars().count() > TERMINAL_TEXT_MAX_CHARS,
            "a diagnostic is not cut to the short cap"
        );
        assert_eq!(
            terminal_safe_detail(&"e".repeat(TERMINAL_DETAIL_MAX_CHARS + 5))
                .chars()
                .count(),
            TERMINAL_DETAIL_MAX_CHARS + 1
        );
    }

    #[test]
    fn choices_cover_all_three_states_and_offer_only_tools() {
        let installed = [
            installed("pinned", "1.0.0", &[PluginCapability::Tool]),
            installed("stale", "1.0.0", &[PluginCapability::Tool]),
            installed("local-only", "0.3.0", &[PluginCapability::Tool]),
            installed("chat", "1.0.0", &[PluginCapability::Channel]),
        ];
        let registry = PluginRegistryIndex {
            plugins: vec![
                listed("fresh", "0.2.0", &["tool"]),
                listed("pinned", "1.0.0", &["tool"]),
                listed("stale", "2.0.0", &["tool"]),
                listed("unlabeled", "0.1.0", &[]),
                listed("bridge", "0.1.0", &["channel"]),
            ],
            registry_url: None,
        };

        let catalog = package_catalog(&installed, Some(&registry));
        let choices = plugin_choices(&catalog);
        let summary: Vec<(&str, &str, &ChoiceState)> = choices
            .iter()
            .map(|choice| (choice.name.as_str(), choice.version.as_str(), &choice.state))
            .collect();

        assert_eq!(
            summary,
            vec![
                ("fresh", "0.2.0", &ChoiceState::Available),
                ("local-only", "0.3.0", &ChoiceState::Installed),
                ("pinned", "1.0.0", &ChoiceState::Installed),
                (
                    "stale",
                    "1.0.0",
                    &ChoiceState::InstalledOtherVersion {
                        registry_version: "2.0.0".to_string()
                    }
                ),
                ("unlabeled", "0.1.0", &ChoiceState::Available),
            ],
            "channel packages are not offered; unlabeled registry entries are"
        );
        assert!(choices[0].registry_entry.is_some());
        assert!(!choices[0].is_installed());
        assert!(choices[1].registry_entry.is_none());
        assert!(choices[1].is_installed());
    }

    fn schema() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "required": ["zone", "api_token"],
            "$defs": { "level": { "type": "string", "enum": ["low", "high"] } },
            "properties": {
                "zone": { "type": "string", "description": "Zone\u{1b}[2J name" },
                "api_token": { "type": "string", "x-secret": true, "default": "never-shown" },
                "retries": { "type": "integer", "default": 3 },
                "verbose": { "type": "boolean", "default": true },
                "level": { "$ref": "#/$defs/level", "default": "high" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "bad key": { "type": "string" },
                "mystery": { "description": "no type" }
            }
        })
    }

    #[test]
    fn schema_fields_list_required_first_and_mark_secrets() {
        let fields = config_fields(&schema());
        let keys: Vec<(&str, bool)> = fields
            .fields
            .iter()
            .map(|field| (field.key.as_str(), field.required))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("api_token", true),
                ("zone", true),
                ("level", false),
                ("retries", false),
                ("tags", false),
                ("verbose", false),
            ]
        );
        let token = &fields.fields[0];
        assert!(token.secret);
        assert_eq!(token.default, None, "a secret is never prefilled");
        assert_eq!(
            fields.fields[1].description.as_deref(),
            Some("Zone name"),
            "descriptions are terminal-safe"
        );
    }

    #[test]
    fn schema_fields_exclude_non_portable_and_untyped_properties() {
        let fields = config_fields(&schema());
        assert_eq!(fields.unsupported, vec!["bad key", "mystery"]);
        assert!(
            fields
                .fields
                .iter()
                .all(|field| field.key != "bad key" && field.key != "mystery")
        );
    }

    #[test]
    fn schema_fields_name_the_required_settings_no_prompt_asks_for() {
        let fields = config_fields(&schema());
        assert!(
            fields.required_unprompted.is_empty() && fields.required_undeclared.is_empty(),
            "every required property of the fixture is prompted; the unsupported ones \
             are optional"
        );

        let mut blocked = schema();
        blocked["required"] = json!([
            "zone",
            "bad key",
            "mystery",
            "undeclared",
            "\u{1b}[31mred",
            "bad key"
        ]);
        let fields = config_fields(&blocked);
        assert_eq!(
            fields.required_unprompted,
            vec!["bad key", "mystery", "red", "undeclared"],
            "a required name outside the portable grammar, one whose type does not \
             resolve, and one `properties` does not declare, terminal-safe, once each"
        );
        assert_eq!(
            fields.required_undeclared,
            vec!["red", "undeclared"],
            "only the names `properties` does not declare, which no row can satisfy; \
             a declared name Quickstart cannot prompt for can still be set"
        );
        assert!(
            fields
                .fields
                .iter()
                .any(|field| field.key == "zone" && field.required),
            "the prompted fields are unchanged"
        );

        let mut nonportable = schema();
        nonportable["required"] = json!(["zone", "bad key"]);
        let fields = config_fields(&nonportable);
        assert_eq!(fields.required_unprompted, vec!["bad key"]);
        assert!(
            fields.required_undeclared.is_empty(),
            "a declared name outside the portable grammar is not undeclared"
        );

        let no_properties = json!({ "type": "object", "required": ["zone"] });
        let fields = config_fields(&no_properties);
        assert!(fields.fields.is_empty());
        assert_eq!(fields.required_unprompted, vec!["zone"]);
        assert_eq!(fields.required_undeclared, vec!["zone"]);
    }

    #[test]
    fn schema_fields_offer_enums_and_booleans_as_choices_with_the_default_first() {
        let fields = config_fields(&schema());
        let field = |key: &str| {
            fields
                .fields
                .iter()
                .find(|field| field.key == key)
                .expect("field is mapped")
        };
        let level = field("level");
        assert_eq!(
            level.kind,
            ValueKind::String,
            "the type resolves through $ref"
        );
        assert_eq!(
            level.choices.as_deref(),
            Some(&["high".to_string(), "low".to_string()][..])
        );
        let verbose = field("verbose");
        assert_eq!(
            verbose.choices.as_deref(),
            Some(&["true".to_string(), "false".to_string()][..])
        );
        let retries = field("retries");
        assert_eq!(retries.default.as_deref(), Some("3"));
        assert_eq!(retries.choices, None);

        let descriptor = field_descriptor(level, None);
        assert_eq!(descriptor.kind, PropKind::Enum);
        assert_eq!(descriptor.enum_variants, level.choices);
        let descriptor = field_descriptor(retries, None);
        assert_eq!(descriptor.kind, PropKind::Integer);
        assert_eq!(descriptor.default.as_deref(), Some("3"));
        let descriptor = field_descriptor(field("tags"), Some("Enter JSON."));
        assert_eq!(descriptor.kind, PropKind::String);
        assert_eq!(descriptor.help, "Enter JSON.");
        let descriptor = field_descriptor(field("api_token"), None);
        assert!(descriptor.is_secret);
        assert!(descriptor.required);
    }

    #[test]
    fn answers_encode_as_config_set_stores_them() {
        let fields = config_fields(&schema());
        let field = |key: &str| {
            fields
                .fields
                .iter()
                .find(|field| field.key == key)
                .expect("field is mapped")
        };
        assert_eq!(
            encode_value(field("zone"), " eu west "),
            Some(" eu west ".to_string()),
            "a string is stored exactly as typed"
        );
        // `config set` trims what its masked prompt reads, so a secret loses
        // the space and the line break a paste brings along, and one that is
        // only whitespace leaves the setting unset.
        assert!(field("api_token").secret);
        assert_eq!(
            encode_value(field("api_token"), " tok en \r\n"),
            Some("tok en".to_string())
        );
        assert_eq!(encode_value(field("api_token"), " \n "), None);
        assert_eq!(encode_value(field("retries"), " 5 "), Some("5".to_string()));
        assert_eq!(
            encode_value(field("tags"), " [\"a\", \"b\"] "),
            Some("[\"a\", \"b\"]".to_string())
        );
        assert_eq!(encode_value(field("zone"), ""), None);
        assert_eq!(encode_value(field("retries"), "   "), None);
    }

    fn inputs<'a>(
        plugins_enabled: bool,
        auto_discover: bool,
        installed: &'a [PluginInfo],
        channel_bindings: &'a [ChannelBinding],
    ) -> ActivationInputs<'a> {
        ActivationInputs {
            plugins_enabled,
            auto_discover,
            installed,
            channel_bindings,
        }
    }

    #[test]
    fn activation_preview_is_silent_when_both_flags_are_already_on() {
        let installed = [installed("fresh", "0.1.0", &[PluginCapability::Tool])];
        let preview = activation_preview(&inputs(true, true, &installed, &[]));
        assert!(preview.changes_nothing());
        assert!(preview.activated.is_empty());
    }

    #[test]
    fn activation_preview_names_the_flags_to_flip_and_every_dormant_package() {
        let installed = [
            installed("fresh", "0.1.0", &[PluginCapability::Tool]),
            installed("dormant", "1.0.0", &[PluginCapability::Tool]),
            installed("notes", "1.0.0", &[PluginCapability::Skill]),
            installed("chat", "1.0.0", &[PluginCapability::Channel]),
        ];
        let bindings = [ChannelBinding {
            alias: "team".to_string(),
            package: "chat".to_string(),
        }];
        let selected = BTreeSet::from(["fresh".to_string()]);

        let preview = activation_preview(&inputs(false, false, &installed, &bindings));
        assert!(preview.enable_plugins && preview.enable_auto_discover);
        assert_eq!(
            preview.activated,
            vec![
                ActivatedInstance::Tool {
                    package: "dormant".to_string()
                },
                ActivatedInstance::Tool {
                    package: "fresh".to_string()
                },
                ActivatedInstance::Skill {
                    package: "notes".to_string()
                },
                ActivatedInstance::Channel {
                    alias: "team".to_string(),
                    package: "chat".to_string()
                },
            ]
        );
        assert!(
            !preview.activates_only(&selected),
            "dormant packages and channels make the list differ from the selection"
        );

        // With plugins already enabled only discovery changes, so the channel
        // bindings that are already live are not listed again.
        let preview = activation_preview(&inputs(true, false, &installed[..1], &bindings));
        assert!(!preview.enable_plugins && preview.enable_auto_discover);
        assert_eq!(
            preview.activated,
            vec![ActivatedInstance::Tool {
                package: "fresh".to_string()
            }]
        );
        assert!(preview.activates_only(&selected));
    }

    /// A tool manifest around `config_schema`, parsed as a manifest file is.
    fn manifest_with(config_schema: Option<Value>) -> PluginManifest {
        serde_json::from_value(json!({
            "name": "tool",
            "version": "1.0.0",
            "capabilities": ["tool"],
            "permissions": ["config_read"],
            "config_schema": config_schema,
        }))
        .expect("the manifest parses")
    }

    /// A config row holding `keys`, each set to `value`.
    fn row(keys: &[&str], value: &str) -> PluginEntryConfig {
        PluginEntryConfig {
            name: "zpi1_test".to_string(),
            config: keys
                .iter()
                .map(|key| ((*key).to_string(), value.to_string()))
                .collect(),
            ..PluginEntryConfig::default()
        }
    }

    /// What `manifest` leaves unmet against `entry`, from the readiness
    /// report's own per-key facts, as the status after Create derives it.
    fn unmet(
        manifest: &PluginManifest,
        entry: Option<&PluginEntryConfig>,
    ) -> UnmetRequiredSettings {
        unmet_required_settings(manifest, &required_keys(manifest, entry))
    }

    #[test]
    fn nothing_is_unmet_without_a_schema_or_a_required_list() {
        let no_schema = manifest_with(None);
        assert_eq!(unmet(&no_schema, None), UnmetRequiredSettings::default());
        assert_eq!(
            unmet(&no_schema, Some(&row(&["zone"], "eu"))),
            UnmetRequiredSettings::default()
        );

        let nothing_required = manifest_with(Some(json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "zone": { "type": "string" } }
        })));
        assert_eq!(
            unmet(&nothing_required, None),
            UnmetRequiredSettings::default()
        );
        assert_eq!(
            unmet(&nothing_required, Some(&row(&[], ""))),
            UnmetRequiredSettings::default()
        );
    }

    #[test]
    fn unmet_portable_settings_subtract_the_keys_the_row_holds() {
        let manifest = manifest_with(Some(schema()));
        assert_eq!(
            unmet(&manifest, None).portable,
            vec!["api_token", "zone"],
            "with no row at all, every required setting is unset"
        );
        assert_eq!(
            unmet(&manifest, Some(&row(&[], ""))).portable,
            vec!["api_token", "zone"]
        );
        assert_eq!(
            unmet(&manifest, Some(&row(&["retries", "zone"], "7"))).portable,
            vec!["api_token"],
            "an optional setting does not stand in for a required one"
        );
        assert_eq!(
            unmet(&manifest, Some(&row(&["api_token", "zone"], "x"))),
            UnmetRequiredSettings::default()
        );
        assert_eq!(
            unmet(&manifest, Some(&row(&["api_token", "zone"], ""))),
            UnmetRequiredSettings::default(),
            "a key present with an empty value counts as set: only presence is read"
        );
    }

    #[test]
    fn unmet_portable_settings_name_only_declared_portable_settings() {
        let manifest = manifest_with(Some(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["zone", "bad key", "undeclared", "zone", "api_token"],
            "properties": {
                "api_token": { "type": "string", "x-secret": true },
                "bad key": { "type": "string" },
                "zone": { "type": "string" }
            }
        })));
        assert_eq!(
            unmet(&manifest, None).portable,
            vec!["api_token", "zone"],
            "a non-portable name and a name `properties` does not declare are left out, \
             and the rest are sorted once each"
        );
    }

    /// A schema whose required list holds a portable name, two declared names
    /// outside the portable grammar, one twice, and two names `properties`
    /// does not declare, one of them hostile to a terminal.
    fn mixed_required_manifest() -> PluginManifest {
        manifest_with(Some(json!({
            "type": "object",
            "additionalProperties": false,
            "required": [
                "zone", "bad key", "undeclared", "bad key", "\u{1b}[31mred", "proxy/url"
            ],
            "properties": {
                "bad key": { "type": "string" },
                "proxy/url": { "type": "string" },
                "zone": { "type": "string" }
            }
        })))
    }

    #[test]
    fn unmet_nonportable_settings_name_declared_names_outside_the_grammar() {
        let manifest = mixed_required_manifest();
        assert_eq!(
            unmet(&manifest, None).nonportable,
            vec!["bad key", "proxy/url"],
            "every declared required name outside the portable grammar, once each; \
             portable and undeclared names are left out"
        );
        assert_eq!(
            unmet(&manifest, Some(&row(&["bad key", "zone"], "x"))).nonportable,
            vec!["proxy/url"],
            "a name the row holds is not missing"
        );
        assert!(unmet(&manifest_with(None), None).nonportable.is_empty());
    }

    #[test]
    fn undeclared_required_settings_name_what_the_properties_map_lacks() {
        let manifest = mixed_required_manifest();
        assert_eq!(
            unmet(&manifest, None).undeclared,
            vec!["red", "undeclared"],
            "every required name `properties` does not declare, terminal-safe"
        );
        assert_eq!(
            unmet(&manifest, Some(&row(&["undeclared"], "x"))).undeclared,
            vec!["red", "undeclared"],
            "a row holding an undeclared name does not satisfy it"
        );
        assert!(unmet(&manifest_with(None), None).undeclared.is_empty());
        let no_properties = manifest_with(Some(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["zone"]
        })));
        assert_eq!(
            unmet(&no_properties, None),
            UnmetRequiredSettings {
                undeclared: vec!["zone".to_string()],
                ..UnmetRequiredSettings::default()
            },
            "with no properties map, nothing required is declared"
        );
    }

    #[test]
    fn outcomes_report_what_changed() {
        let installed = PackageOutcome::Installed {
            name: "a".to_string(),
        };
        let seeded = PackageOutcome::AlreadyInstalled {
            name: "b".to_string(),
            seeded_row: true,
        };
        let untouched = PackageOutcome::AlreadyInstalled {
            name: "c".to_string(),
            seeded_row: false,
        };
        let failed = PackageOutcome::Failed {
            name: "d".to_string(),
            stage: FailureStage::Download,
        };
        let skipped_row = PackageOutcome::AlreadyInstalledSkipped {
            name: "e".to_string(),
        };
        let setup_failed = PackageOutcome::AlreadyInstalledSetupFailed {
            name: "g".to_string(),
        };
        assert!(installed.changed_state() && installed.is_installed());
        assert!(seeded.changed_state() && seeded.is_installed());
        assert!(!untouched.changed_state() && untouched.is_installed());
        assert!(!failed.changed_state() && !failed.is_installed());
        assert_eq!(failed.kind(), "download");
        assert_eq!(failed.name(), "d");

        // A package kept by this run is one activation is expected to wake;
        // one whose missing row the operator skipped, or whose setup failed,
        // is installed, but not accepted, and nothing about it changed.
        for accepted in [&installed, &seeded, &untouched] {
            assert!(accepted.is_accepted(), "{accepted:?}");
        }
        for kept_as_was in [&skipped_row, &setup_failed] {
            assert!(
                kept_as_was.is_installed() && !kept_as_was.is_accepted(),
                "{kept_as_was:?}"
            );
            assert!(!kept_as_was.changed_state(), "{kept_as_was:?}");
        }
        assert!(!failed.is_accepted());
        assert_eq!(skipped_row.kind(), "already_installed_skipped");
        assert_eq!(setup_failed.kind(), "already_installed_setup_failed");
        assert_eq!(setup_failed.name(), "g");

        // A publish whose rollback failed left the package behind: a change
        // of this run's, undone by removing it, though its install failed.
        let stranded = PackageOutcome::RollbackFailed {
            name: "f".to_string(),
        };
        assert!(stranded.changed_state() && stranded.published_by_run());
        assert!(!stranded.is_installed() && !stranded.is_accepted());
        assert_eq!(stranded.kind(), "rollback_failed");
        assert!(installed.published_by_run());
        for other in [&seeded, &untouched, &failed, &skipped_row, &setup_failed] {
            assert!(!other.published_by_run(), "{other:?}");
        }

        // Only a package installed before this run missed this run's load
        // check, whatever happened to its row.
        for before in [&seeded, &untouched, &skipped_row, &setup_failed] {
            assert!(before.installed_before_run(), "{before:?}");
        }
        for other in [&installed, &failed, &stranded] {
            assert!(!other.installed_before_run(), "{other:?}");
        }
    }

    #[test]
    fn capability_and_permission_names_use_the_manifest_spelling() {
        assert_eq!(
            capability_names(&[PluginCapability::Tool, PluginCapability::Skill]),
            vec!["tool", "skill"]
        );
        assert_eq!(
            permission_names(&[PluginPermission::HttpClient, PluginPermission::ConfigRead]),
            vec!["http_client", "config_read"]
        );
    }

    /// A tool manifest as its publisher signs it.
    const UNSIGNED_MANIFEST: &str = r#"name = "signed-tool"
version = "1.0.0"
author = "Example Labs"
capabilities = ["tool"]
"#;

    /// The verdict for a manifest file's text, parsed as admission parses it.
    fn verdict_of(text: &str, trusted_keys: &[String]) -> SignatureVerdict {
        let manifest: PluginManifest = toml::from_str(text).expect("the manifest parses");
        signature_verdict(&manifest, text, trusted_keys)
    }

    #[test]
    fn signature_verdicts_follow_the_plugin_crate_verifier() {
        let publisher = Publisher::new();
        let key = publisher.key.clone();
        let other_key = Publisher::new().key;
        let trusted = vec![key.clone()];
        let text = publisher.sign(UNSIGNED_MANIFEST);

        let verified = verdict_of(&text, &trusted);
        assert_eq!(
            verified,
            SignatureVerdict::Verified {
                publisher_key: key.to_lowercase()
            }
        );
        assert!(verified.is_verified());

        for (case, text, trusted_keys, expected) in [
            (
                "no signature fields",
                UNSIGNED_MANIFEST.to_string(),
                trusted.clone(),
                SignatureVerdict::Unsigned,
            ),
            (
                "a signature without a publisher key",
                text.lines()
                    .filter(|line| !line.starts_with("publisher_key"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                trusted.clone(),
                SignatureVerdict::Unsigned,
            ),
            (
                "a publisher key without a signature",
                text.lines()
                    .filter(|line| !line.starts_with("signature"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                trusted.clone(),
                SignatureVerdict::Unsigned,
            ),
            (
                "an empty trusted list",
                text.clone(),
                Vec::new(),
                SignatureVerdict::Untrusted,
            ),
            (
                "a trusted list without the signing key",
                text.clone(),
                vec![other_key.clone()],
                SignatureVerdict::Untrusted,
            ),
            (
                "an author edited after signing",
                text.replace("Example Labs", "Trusted Vendor"),
                trusted.clone(),
                SignatureVerdict::Invalid,
            ),
            (
                "a signature that is not base64url",
                text.lines()
                    .map(|line| {
                        if line.starts_with("signature") {
                            "signature = \"not a signature\""
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                trusted.clone(),
                SignatureVerdict::Invalid,
            ),
            // The verifier checks trust before the signature, so an edit
            // under a key nobody trusts reads as untrusted, never invalid.
            (
                "an edit under an untrusted key",
                text.replace("Example Labs", "Trusted Vendor"),
                vec![other_key.clone()],
                SignatureVerdict::Untrusted,
            ),
        ] {
            let verdict = verdict_of(&text, &trusted_keys);
            assert_eq!(verdict, expected, "{case}");
            assert!(!verdict.is_verified(), "{case}");
        }

        // The verifier never answers unsigned itself; it maps all the same.
        assert_eq!(
            SignatureVerdict::from_verification(VerificationResult::Unsigned),
            SignatureVerdict::Unsigned
        );
    }

    #[test]
    fn a_verified_publisher_key_prints_short_and_terminal_safe() {
        let key = "0123456789abcdef".repeat(4);
        assert_eq!(short_publisher_key(&key), "0123456789abcdef…");
        assert_eq!(
            short_publisher_key("0123456789abcdef"),
            "0123456789abcdef",
            "a key at the cap is printed whole"
        );
        assert_eq!(
            short_publisher_key("\u{1b}[31m0123456789abcdef0123"),
            "0123456789abcdef…",
            "escape sequences go before the key is cut"
        );
    }
}
