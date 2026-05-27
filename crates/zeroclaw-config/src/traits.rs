/// Describes a single secret field discovered via `#[derive(Configurable)]`.
#[derive(Debug, Clone)]
pub struct SecretFieldInfo {
    /// Full dotted name (e.g. `channels.matrix.access-token`)
    pub name: &'static str,
    /// Category for grouping in `zeroclaw config list`
    pub category: &'static str,
    /// Whether this field currently has a non-empty value
    pub is_set: bool,
}

/// Runtime type classification for config property values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropKind {
    String,
    Bool,
    Integer,
    Float,
    /// An enum or other serde-serializable type (parsed as TOML string).
    Enum,
    /// A `Vec<String>` field; set via comma-separated input.
    StringArray,
    /// A `Vec<T>` field where `T` is a serializable struct (e.g. `Vec<McpServerConfig>`,
    /// `Vec<PeripheralBoardConfig>`). Round-tripped on the wire as a JSON array of
    /// objects; the dashboard renders a per-row sub-form using the JSON Schema
    /// from `OPTIONS /api/config` to discover the element type's field shape.
    /// Schema v3 / #5947 will migrate the load-bearing ones (mcp.servers etc.)
    /// to `HashMap<String, T>` keyed tables; until then this kind covers them.
    ObjectArray,
    /// A struct-shaped scalar field (e.g. `Option<ModelPricing>`). Round-tripped
    /// on the wire as a JSON object; the dashboard renders a sub-form for the
    /// inner fields using the JSON Schema from `OPTIONS /api/config`. Distinct
    /// from `String`, which inserts the raw value as a TOML string and breaks
    /// the serde round-trip for typed structs.
    Object,
}

/// Maps Rust types to PropKind at compile time.
/// Scalars have explicit impls; the blanket impl catches everything
/// else as `PropKind::Enum`.
pub trait HasPropKind {
    const PROP_KIND: PropKind;
}

macro_rules! impl_prop_kind {
    ($kind:expr, $($ty:ty),+) => {
        $(impl HasPropKind for $ty { const PROP_KIND: PropKind = $kind; })+
    };
}

impl_prop_kind!(PropKind::Bool, bool);
impl_prop_kind!(PropKind::String, String);
impl_prop_kind!(PropKind::Float, f64, f32);
impl_prop_kind!(
    PropKind::Integer,
    u8,
    u16,
    u32,
    u64,
    usize,
    i8,
    i16,
    i32,
    i64,
    isize
);
impl HasPropKind for Vec<String> {
    const PROP_KIND: PropKind = PropKind::StringArray;
}

// The per-category provider-ref newtypes (defined in `crate::providers`)
// serialize as plain strings; the schema-tooling layer treats them as
// strings too.
impl HasPropKind for crate::providers::ModelProviderRef {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for crate::providers::TtsProviderRef {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for crate::providers::TranscriptionProviderRef {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for crate::providers::ChannelRef {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for Vec<crate::providers::ChannelRef> {
    const PROP_KIND: PropKind = PropKind::StringArray;
}

// Multi-agent typed primitives. AgentAlias / PeerGroupName /
// PeerUsername round-trip as plain strings; AccessMode and
// MemoryBackendKind are enums.
impl HasPropKind for crate::multi_agent::AgentAlias {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for crate::multi_agent::PeerGroupName {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for crate::multi_agent::PeerUsername {
    const PROP_KIND: PropKind = PropKind::String;
}
impl HasPropKind for crate::multi_agent::AccessMode {
    const PROP_KIND: PropKind = PropKind::Enum;
}
impl HasPropKind for crate::multi_agent::MemoryBackendKind {
    const PROP_KIND: PropKind = PropKind::Enum;
}
impl HasPropKind for Vec<crate::multi_agent::AgentAlias> {
    const PROP_KIND: PropKind = PropKind::StringArray;
}
impl HasPropKind for Vec<crate::multi_agent::PeerUsername> {
    const PROP_KIND: PropKind = PropKind::StringArray;
}
impl HasPropKind
    for std::collections::BTreeMap<crate::multi_agent::AgentAlias, crate::multi_agent::AccessMode>
{
    // Serialized as a TOML inline table: `{ beta = "read", gamma = "read_write" }`.
    const PROP_KIND: PropKind = PropKind::Object;
}

// Vec<struct> fields are surfaced as PropKind::ObjectArray — each
// element renders as a per-row sub-form on the dashboard rather than a
// chip. The Configurable derive routes `<Vec<T> as HasPropKind>::PROP_KIND`
// for every Vec field, so a missing impl here surfaces as a "trait bound
// not satisfied" compile error pointing at the field. Add the impl in
// the same module that defines the type if traits.rs's crate scope is
// too narrow.
impl HasPropKind for Vec<crate::schema::ClassificationRule> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::EmbeddingRouteConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::GoogleWorkspaceAllowedOperation> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::McpServerConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::ModelRouteConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::NevisRoleMappingConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::PeripheralBoardConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::ToolFilterGroup> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}

/// Tab grouping for config fields and UI surfaces. Each variant maps to a
/// tab in the TUI and gateway dashboard. Serializes to its PascalCase
/// variant name on the wire.
///
/// Field-partition tabs (`Connection`, `Model`, …) are used as `#[tab(...)]`
/// annotations on schema structs. Composite tabs (`Personality`, `Skills`,
/// `PeerGroups`, `Costs`) are rendered by dedicated UI components but share
/// the same enum so both frontends speak one vocabulary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum ConfigTab {
    #[default]
    /// No tab grouping — field appears in a flat list.
    None,

    // ── Shared (providers + channels) ──
    Connection,
    Advanced,

    // ── Providers ──
    Model,

    // ── Channels ──
    Behavior,

    // ── Agents: field partitions ──
    General,
    Channels,
    Providers,
    Bundles,
    Cron,
    Tuning,
    Workspace,
    Memory,

    // ── Agents: composite (custom-component) tabs ──
    PeerGroups,
    Personality,

    // ── MCP ──
    Settings,
    Servers,

    // ── Cost ──
    Limits,
    Costs,

    // ── Skill bundles ──
    Skills,
    Aliases,
}

impl ConfigTab {
    /// Display label for the tab bar. Returns `""` for `None`.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Connection => "Connection",
            Self::Advanced => "Advanced",
            Self::Model => "Model",
            Self::Behavior => "Behavior",
            Self::General => "General",
            Self::Channels => "Channels",
            Self::Providers => "Providers",
            Self::Bundles => "Bundles",
            Self::Cron => "Cron",
            Self::Tuning => "Tuning",
            Self::Workspace => "Workspace",
            Self::Memory => "Memory",
            Self::PeerGroups => "Peer Groups",
            Self::Personality => "Personality",
            Self::Settings => "Settings",
            Self::Servers => "Servers",
            Self::Limits => "Limits",
            Self::Costs => "Costs",
            Self::Skills => "Skills",
            Self::Aliases => "Aliases",
        }
    }

    /// `true` when this is the `None` variant (no tab grouping).
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

impl std::fmt::Display for ConfigTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Describes a single property field discovered via `#[derive(Configurable)]`.
#[derive(Clone)]
pub struct PropFieldInfo {
    /// Full dotted name (e.g. `channels.telegram.draft-update-interval-ms`).
    /// Owned so the `HashMap<String, T>` branch of the derive can inject the
    /// runtime map key into the path (`model_providers.anthropic.api-key`)
    /// — `&'static str` can't carry user-supplied keys.
    pub name: String,
    /// Category for grouping in property listings
    pub category: &'static str,
    /// Current value formatted for display (secrets show `"****"`)
    pub display_value: String,
    /// Raw Rust type string for display (e.g. `"bool"`, `"u64"`, `"Option<StreamMode>"`)
    pub type_hint: &'static str,
    /// Runtime type classification
    pub kind: PropKind,
    /// Whether this field is marked `#[secret]`
    pub is_secret: bool,
    /// Returns valid variant names for enum fields (None for non-enum fields)
    pub enum_variants: Option<fn() -> Vec<String>>,
    /// Field's `///` doc comment, flattened to a single line. Empty string
    /// when the field has no doc comment. Onboard uses this as human-readable
    /// prompt text instead of the raw kebab-case field name.
    pub description: &'static str,
    /// Whether this field's value is derived from a secret (`#[derived_from_secret]`).
    /// Subject to the same write-only / no-readback rules as `#[secret]`.
    /// Reserved for future schema additions; currently no fields are derived.
    pub derived_from_secret: bool,
    /// Tab grouping for this field. `ConfigTab::None` when the field has
    /// no tab annotation (flat display, no tab bar).
    pub tab: ConfigTab,
}

impl PropKind {
    /// Stable lowercase-kebab wire name matching the serde serialization.
    /// Useful when consumers need the tag as a `&'static str` without
    /// going through serde round-trip.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Enum => "enum",
            Self::StringArray => "string_array",
            Self::ObjectArray => "object_array",
            Self::Object => "object",
        }
    }
}

impl PropFieldInfo {
    pub fn is_enum(&self) -> bool {
        self.enum_variants.is_some()
    }
}

impl std::fmt::Debug for PropFieldInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PropFieldInfo")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("is_secret", &self.is_secret)
            .finish_non_exhaustive()
    }
}

/// Mask and restore secret fields on config structs.
///
/// Automatically implemented by `#[derive(Configurable)]` for any struct that
/// has fields annotated with `#[secret]` or `#[nested]`. A blanket impl covers
/// `HashMap<String, T: MaskSecrets>` so the trait propagates through alias maps
/// without any per-type boilerplate.
pub trait MaskSecrets {
    fn mask_secrets(&mut self);
    fn restore_secrets_from(&mut self, current: &Self);
}

impl<T: MaskSecrets> MaskSecrets for std::collections::HashMap<String, T> {
    fn mask_secrets(&mut self) {
        for v in self.values_mut() {
            v.mask_secrets();
        }
    }
    fn restore_secrets_from(&mut self, current: &Self) {
        for (k, v) in self.iter_mut() {
            if let Some(cur) = current.get(k) {
                v.restore_secrets_from(cur);
            }
        }
    }
}

pub const MASKED_SECRET: &str = "***MASKED***";

pub fn is_masked_secret(value: &str) -> bool {
    value == MASKED_SECRET
}

pub fn mask_optional_secret(value: &mut Option<String>) {
    if value.is_some() {
        *value = Some(MASKED_SECRET.to_string());
    }
}

pub fn mask_required_secret(value: &mut String) {
    if !value.is_empty() {
        *value = MASKED_SECRET.to_string();
    }
}

#[allow(clippy::ref_option)]
pub fn restore_optional_secret(value: &mut Option<String>, current: &Option<String>) {
    if value.as_deref().is_some_and(is_masked_secret) {
        *value = current.clone();
    }
}

pub fn restore_required_secret(value: &mut String, current: &str) {
    if is_masked_secret(value) {
        *value = current.to_string();
    }
}

/// Stable wire-form for an addable section — a `HashMap<String, T>` (Map) or
/// `Vec<T>` (List) field whose value type implements `Configurable`. The
/// dashboard / CLI use this to surface `+ Add` affordances without
/// hardcoding the section list. Auto-discovered by the `Configurable` derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MapKeyKind {
    /// `HashMap<String, T>` — key is user-supplied; new value is default.
    Map,
    /// `Vec<T>` — entries are appended; the user-supplied "key" is stored
    /// in the value type's natural identifier field (e.g. `name`, `hint`).
    List,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MapKeySection {
    /// Dotted section path, e.g. `providers.models`, `mcp.servers`.
    pub path: &'static str,
    /// Whether the section is a map or a list.
    pub kind: MapKeyKind,
    /// Rust type name of the value, e.g. `ModelProviderConfig`. For display only.
    pub value_type: &'static str,
    /// Doc comment on the field (flattened to one line). What the user sees
    /// when picking which kind of thing to add.
    pub description: &'static str,
}

/// Serializable wire representation of a config field for API consumers
/// (RPC dispatch, gateway, TUI). Single source of truth — replaces the
/// gateway's local `ListEntry` and the RPC dispatch's ad-hoc JSON.
///
/// Built from [`PropFieldInfo`] via [`ConfigFieldEntry::from_prop_field`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigFieldEntry {
    pub path: String,
    pub category: String,
    pub kind: PropKind,
    pub type_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    pub populated: bool,
    pub is_secret: bool,
    #[serde(default)]
    pub is_env_overridden: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_variants: Vec<String>,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Tab grouping. `ConfigTab::None` = no tab grouping (flat display).
    #[serde(default, skip_serializing_if = "ConfigTab::is_none")]
    pub tab: ConfigTab,
}

impl ConfigFieldEntry {
    /// Convert a [`PropFieldInfo`] (server-side introspection) into its wire
    /// representation. Secrets are masked (value omitted). The caller supplies
    /// `is_env_overridden` from `Config::prop_is_env_overridden`.
    pub fn from_prop_field(info: PropFieldInfo, is_env_overridden: bool) -> Self {
        let populated = info.display_value != "<unset>";
        let is_sensitive = info.is_secret || info.derived_from_secret;
        let value = if is_sensitive {
            None
        } else {
            Some(serde_json::Value::String(info.display_value))
        };
        let enum_variants = info.enum_variants.map(|f| f()).unwrap_or_default();
        let section = crate::sections::Section::from_key(info.name.split('.').next().unwrap_or(""))
            .map(|s| s.as_str().to_string());

        Self {
            path: info.name,
            category: info.category.to_string(),
            kind: info.kind,
            type_hint: info.type_hint.to_string(),
            value,
            populated,
            is_secret: is_sensitive,
            is_env_overridden,
            enum_variants,
            description: info.description.to_string(),
            section,
            tab: info.tab,
        }
    }
}

/// One row emitted by the `Configurable` derive's `nested_option_entries()`
/// method — every `#[nested] Option<XConfig>` field on a struct shows up here
/// with its `present` bit and the per-field `#[display_name = "..."]` /
/// `#[description = "..."]` metadata. The integrations registry consumes
/// this verbatim instead of carrying its own per-field hand-list.
#[derive(Debug, Clone, Copy)]
pub struct NestedOptionEntry {
    /// snake_case field name on the parent struct (e.g. `"telegram"`,
    /// `"voice_duplex"`).
    pub field: &'static str,
    /// `true` when the parent struct's field is `Some(_)`.
    pub present: bool,
    /// Display name from `#[display_name = "..."]`; falls back to a
    /// title-cased rendering of the snake_case field name when the
    /// attribute is absent.
    pub display_name: &'static str,
    /// One-line summary from `#[description = "..."]`. Empty when the
    /// attribute is absent.
    pub description: &'static str,
}

/// One row emitted by the `Configurable` derive's `integration_descriptor()`
/// method on structs annotated with `#[integration(...)]`. Used for nested
/// toggleable configs (e.g. `BrowserConfig`, `CronConfig`) where the
/// integration is "active" iff a named bool field on the struct is `true`.
#[derive(Debug, Clone, Copy)]
pub struct IntegrationDescriptor {
    pub display_name: &'static str,
    pub description: &'static str,
    /// Free-form category label (e.g. `"ToolsAutomation"`). The
    /// integrations registry maps this string to its own
    /// `IntegrationCategory` enum so the schema crate doesn't have to
    /// depend on it.
    pub category: &'static str,
    /// Snapshot of the named status field at the moment this descriptor
    /// was built (`status_field = "enabled"` ⇒ `self.enabled`).
    pub active: bool,
}

/// Metadata for one channel type, as returned by [`ChannelsConfig::channels`].
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub name: &'static str,
    pub desc: &'static str,
    pub configured: bool,
}

/// The trait for describing a channel
pub trait ChannelConfig {
    /// human-readable name
    fn name() -> &'static str;
    /// short description
    fn desc() -> &'static str;
}
