/// Sentinel rendered for unset / `None` / empty config values during display.
/// Never a valid stored value: the write path rejects it so it cannot round-trip
/// into persisted config.
pub const UNSET_DISPLAY: &str = "<unset>";

/// Return whether a submitted display value represents no configured value.
#[must_use]
pub fn is_unset_display_value(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || value == UNSET_DISPLAY
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AliasSource {
    ModelProviders,
    TtsProviders,
    TranscriptionProviders,
    Channels,
    RiskProfiles,
    RuntimeProfiles,
    Agents,
    SkillBundles,
    KnowledgeBundles,
    McpBundles,
}

impl AliasSource {
    #[must_use]
    pub const fn section_path(self) -> &'static str {
        match self {
            Self::ModelProviders => "providers.models",
            Self::TtsProviders => "providers.tts",
            Self::TranscriptionProviders => "providers.transcription",
            Self::Channels => "channels",
            Self::RiskProfiles => "risk_profiles",
            Self::RuntimeProfiles => "runtime_profiles",
            Self::Agents => "agents",
            Self::SkillBundles => "skill_bundles",
            Self::KnowledgeBundles => "knowledge_bundles",
            Self::McpBundles => "mcp_bundles",
        }
    }

    #[must_use]
    pub const fn is_two_tier(self) -> bool {
        matches!(
            self,
            Self::ModelProviders
                | Self::TtsProviders
                | Self::TranscriptionProviders
                | Self::Channels
        )
    }
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
    /// A reference to a configured alias; `alias_source` names the namespace.
    AliasRef,
    /// A `Vec<String>` field; set via comma-separated input.
    StringArray,
    ObjectArray,
    Object,
}

/// Maps Rust types to PropKind at compile time.
/// Scalars have explicit impls; the blanket impl catches everything
/// else as `PropKind::Enum`.
pub trait HasPropKind {
    const PROP_KIND: PropKind;

    const ALIAS_SOURCE: Option<AliasSource> = None;

    /// Terminal field names whose values must be redacted when this type is
    /// displayed as an object/object-array prop. Most prop kinds have no
    /// nested secret surface; Configurable object-array element types can
    /// override this by delegating to their generated `secret_field_terminals`.
    fn display_secret_terminals() -> Vec<&'static str> {
        Vec::new()
    }
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
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::ModelProviders);
}
impl HasPropKind for Vec<crate::providers::ModelProviderRef> {
    const PROP_KIND: PropKind = PropKind::StringArray;
}
impl HasPropKind for crate::providers::TtsProviderRef {
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::TtsProviders);
}
impl HasPropKind for crate::providers::TranscriptionProviderRef {
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::TranscriptionProviders);
}
impl HasPropKind for crate::providers::ChannelRef {
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::Channels);
}
impl HasPropKind for crate::providers::RiskProfileRef {
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::RiskProfiles);
}
impl HasPropKind for crate::providers::RuntimeProfileRef {
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::RuntimeProfiles);
}
impl HasPropKind for Vec<crate::providers::ChannelRef> {
    const PROP_KIND: PropKind = PropKind::StringArray;
}

// Multi-agent typed primitives. AgentAlias / PeerGroupName /
// PeerUsername round-trip as plain strings; AccessMode and
// MemoryBackendKind are enums.
impl HasPropKind for crate::multi_agent::AgentAlias {
    const PROP_KIND: PropKind = PropKind::AliasRef;
    const ALIAS_SOURCE: Option<AliasSource> = Some(AliasSource::Agents);
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
impl HasPropKind for crate::multi_agent::OutputModality {
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

impl HasPropKind for crate::scattered_types::EmailOAuth2Config {
    const PROP_KIND: PropKind = PropKind::Object;
}

impl HasPropKind for Vec<crate::schema::ClassificationRule> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::EmbeddingRouteConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;

    fn display_secret_terminals() -> Vec<&'static str> {
        crate::schema::EmbeddingRouteConfig::secret_field_terminals()
    }
}
impl HasPropKind for Vec<crate::schema::GoogleWorkspaceAllowedOperation> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for Vec<crate::schema::McpServerConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;

    fn display_secret_terminals() -> Vec<&'static str> {
        crate::schema::McpServerConfig::secret_field_terminals()
    }
}
impl HasPropKind for Vec<crate::schema::ModelRouteConfig> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;

    fn display_secret_terminals() -> Vec<&'static str> {
        crate::schema::ModelRouteConfig::secret_field_terminals()
    }
}
impl HasPropKind for Vec<crate::schema::ExternalRegistry> {
    const PROP_KIND: PropKind = PropKind::ObjectArray;
}
impl HasPropKind for crate::schema::DelegateExecutionMode {
    const PROP_KIND: PropKind = PropKind::Enum;
}
impl HasPropKind for Vec<crate::schema::DelegateTargetConfig> {
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

/// Security classification for credential-shaped config surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSurfaceClass {
    EncryptedSecret,
    PathOnlyReference,
    PublicValue,
    ExternalAuthStore,
    LegacyEnvPath,
    RequiresFollowUp,
}

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
    /// Explicit security classification for credential-shaped surfaces.
    pub credential_class: Option<CredentialSurfaceClass>,
    /// Tab grouping for this field. `ConfigTab::None` when the field has
    /// no tab annotation (flat display, no tab bar).
    pub tab: ConfigTab,
    /// Alias namespace for `PropKind::AliasRef` fields; `None` otherwise.
    pub alias_source: Option<AliasSource>,
    /// Whether this field is marked `#[multiline]`, a hint that surfaces
    /// should render a multi-line text area (e.g. a PEM key body) rather
    /// than a single-line input.
    pub multiline: bool,
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
            Self::AliasRef => "alias_ref",
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
            .field("credential_class", &self.credential_class)
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

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

impl<T: MaskSecrets> MaskSecrets for Vec<T> {
    fn mask_secrets(&mut self) {
        for v in self.iter_mut() {
            v.mask_secrets();
        }
    }
    fn restore_secrets_from(&mut self, current: &Self) {
        for (v, cur) in self.iter_mut().zip(current.iter()) {
            v.restore_secrets_from(cur);
        }
    }
}

pub const MASKED_SECRET: &str = "***MASKED***";

pub fn is_masked_secret(value: &str) -> bool {
    value == MASKED_SECRET
}

pub trait SecretField {
    /// Replace each non-empty inner string with [`MASKED_SECRET`].
    fn mask(&mut self);

    /// Restore inner strings that currently equal [`MASKED_SECRET`] from the
    /// matching position in `current`. The dashboard write path relies on this
    /// so re-posting an already-displayed masked value doesn't overwrite the
    /// real secret in config.
    fn restore_from(&mut self, current: &Self);

    /// Encrypt every non-empty, not-already-encrypted inner string.
    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()>;

    /// Inverse of [`Self::encrypt_in_place`].
    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()>;

    /// Whether the field carries at least one non-empty inner string. Reported
    /// back through [`SecretFieldInfo::is_set`].
    fn is_set(&self) -> bool;
}

impl SecretField for String {
    fn mask(&mut self) {
        if !self.is_empty() {
            *self = MASKED_SECRET.to_string();
        }
    }

    fn restore_from(&mut self, current: &Self) {
        if is_masked_secret(self) {
            self.clone_from(current);
        }
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        // `is_encrypted` also includes external secret references (`op://`):
        // encryption must preserve them, while decryption resolves them for use.
        if !self.is_empty() && !crate::security::SecretStore::is_encrypted(self) {
            *self = store
                .encrypt(self)
                .with_context(|| format!("Failed to encrypt {field}"))?;
        }
        Ok(())
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        if crate::security::SecretStore::is_encrypted(self) {
            *self = store
                .decrypt(self)
                .with_context(|| format!("Failed to decrypt {field}"))?;
        }
        Ok(())
    }

    fn is_set(&self) -> bool {
        !self.is_empty()
    }
}

impl SecretField for Option<String> {
    fn mask(&mut self) {
        if let Some(inner) = self {
            inner.mask();
        }
    }

    fn restore_from(&mut self, current: &Self) {
        if let (Some(inner), Some(cur)) = (self.as_mut(), current.as_ref()) {
            inner.restore_from(cur);
        }
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        match self {
            Some(inner) => inner.encrypt_in_place(store, field),
            None => Ok(()),
        }
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        match self {
            Some(inner) => inner.decrypt_in_place(store, field),
            None => Ok(()),
        }
    }

    fn is_set(&self) -> bool {
        self.as_ref().is_some_and(|v| !v.is_empty())
    }
}

impl SecretField for std::path::PathBuf {
    fn mask(&mut self) {
        let mut s = self.to_string_lossy().into_owned();
        if !s.is_empty() {
            s.mask();
            *self = std::path::PathBuf::from(s);
        }
    }

    fn restore_from(&mut self, current: &Self) {
        let mut s = self.to_string_lossy().into_owned();
        let cur = current.to_string_lossy().into_owned();
        s.restore_from(&cur);
        *self = std::path::PathBuf::from(s);
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        let mut s = self.to_string_lossy().into_owned();
        s.encrypt_in_place(store, field)?;
        *self = std::path::PathBuf::from(s);
        Ok(())
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        let mut s = self.to_string_lossy().into_owned();
        s.decrypt_in_place(store, field)?;
        *self = std::path::PathBuf::from(s);
        Ok(())
    }

    fn is_set(&self) -> bool {
        !self.as_os_str().is_empty()
    }
}

impl SecretField for Option<std::path::PathBuf> {
    fn mask(&mut self) {
        if let Some(inner) = self {
            inner.mask();
        }
    }

    fn restore_from(&mut self, current: &Self) {
        if let (Some(inner), Some(cur)) = (self.as_mut(), current.as_ref()) {
            inner.restore_from(cur);
        }
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        match self {
            Some(inner) => inner.encrypt_in_place(store, field),
            None => Ok(()),
        }
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        match self {
            Some(inner) => inner.decrypt_in_place(store, field),
            None => Ok(()),
        }
    }

    fn is_set(&self) -> bool {
        self.as_ref().is_some_and(|v| !v.as_os_str().is_empty())
    }
}

impl SecretField for Vec<String> {
    fn mask(&mut self) {
        for element in self.iter_mut() {
            element.mask();
        }
    }

    fn restore_from(&mut self, current: &Self) {
        for (element, cur) in self.iter_mut().zip(current.iter()) {
            element.restore_from(cur);
        }
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        for (idx, element) in self.iter_mut().enumerate() {
            element.encrypt_in_place(store, &format!("{field}[{idx}]"))?;
        }
        Ok(())
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        for (idx, element) in self.iter_mut().enumerate() {
            element.decrypt_in_place(store, &format!("{field}[{idx}]"))?;
        }
        Ok(())
    }

    fn is_set(&self) -> bool {
        !self.is_empty()
    }
}

impl SecretField for std::collections::HashMap<String, String> {
    fn mask(&mut self) {
        for value in self.values_mut() {
            value.mask();
        }
    }

    fn restore_from(&mut self, current: &Self) {
        for (key, value) in self.iter_mut() {
            if let Some(cur) = current.get(key) {
                value.restore_from(cur);
            }
        }
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        for (key, value) in self.iter_mut() {
            value.encrypt_in_place(store, &format!("{field}.{key}"))?;
        }
        Ok(())
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        for (key, value) in self.iter_mut() {
            value.decrypt_in_place(store, &format!("{field}.{key}"))?;
        }
        Ok(())
    }

    fn is_set(&self) -> bool {
        self.values().any(|v| !v.is_empty())
    }
}

impl SecretField for Option<std::collections::HashMap<String, String>> {
    fn mask(&mut self) {
        if let Some(inner) = self {
            inner.mask();
        }
    }

    fn restore_from(&mut self, current: &Self) {
        if let (Some(inner), Some(cur)) = (self.as_mut(), current.as_ref()) {
            inner.restore_from(cur);
        }
    }

    fn encrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        match self {
            Some(inner) => inner.encrypt_in_place(store, field),
            None => Ok(()),
        }
    }

    fn decrypt_in_place(
        &mut self,
        store: &crate::security::SecretStore,
        field: &str,
    ) -> anyhow::Result<()> {
        match self {
            Some(inner) => inner.decrypt_in_place(store, field),
            None => Ok(()),
        }
    }

    fn is_set(&self) -> bool {
        self.as_ref()
            .is_some_and(|m| m.values().any(|v| !v.is_empty()))
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
    /// Optional natural key used to address entries in list-backed sections.
    pub natural_key: Option<&'static str>,
    /// Whether this section's map key is a `#[resource_key]` — a value
    /// drawn from another domain (a model id, tool name, …) that may
    /// itself contain dots, rather than a short operator-chosen alias
    /// validated by `validate_alias_key`. Consumers that split a dotted
    /// prop path into "map key" + "field name" by first-dot-boundary
    /// MUST exclude `resource_key` sections: naive splitting corrupts a
    /// resource key like `gpt-4.1` into a bogus alias `gpt-4` plus a
    /// nonsense tail `1`.
    pub resource_key: bool,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_source: Option<AliasSource>,
    /// Surface hint from `#[multiline]`: render a multi-line text area
    /// instead of a single-line input.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiline: bool,
}

impl ConfigFieldEntry {
    /// Convert a [`PropFieldInfo`] (server-side introspection) into its wire
    /// representation. Secrets are masked (value omitted). The caller supplies
    /// `is_env_overridden` from `Config::prop_is_env_overridden`.
    pub fn from_prop_field(info: PropFieldInfo, is_env_overridden: bool) -> Self {
        let populated = info.display_value != crate::traits::UNSET_DISPLAY;
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
            alias_source: info.alias_source,
            multiline: info.multiline,
        }
    }
}

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

/// Metadata for one channel type, as returned by [`crate::schema::ChannelsConfig::channels`].
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    /// Canonical kebab-case identifier used in config TOML
    /// (`[channels.<kind>]`). Matches the field name on
    /// `ChannelsConfig` so Quickstart and other surfaces can
    /// reuse the schema's own labeling without a parallel map.
    pub kind: &'static str,
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

#[cfg(test)]
mod secret_field_tests {
    use super::{MASKED_SECRET, SecretField};
    use crate::security::SecretStore;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn store() -> (TempDir, SecretStore) {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        (tmp, store)
    }

    #[test]
    fn string_roundtrip_and_idempotent() {
        let (_tmp, store) = store();
        let mut s = String::from("sk-abc");
        s.encrypt_in_place(&store, "test.s").unwrap();
        assert!(SecretStore::is_encrypted(&s));
        let enc1 = s.clone();
        // idempotent: encrypting again must not double-wrap
        s.encrypt_in_place(&store, "test.s").unwrap();
        assert_eq!(s, enc1);
        s.decrypt_in_place(&store, "test.s").unwrap();
        assert_eq!(s, "sk-abc");
    }

    #[test]
    fn string_op_reference_is_preserved_by_encrypt_in_place() {
        let (_tmp, store) = store();
        let mut s = String::from("op://vault/item/field");

        s.encrypt_in_place(&store, "test.s").unwrap();

        assert_eq!(s, "op://vault/item/field");
    }

    #[test]
    fn string_empty_stays_empty() {
        let (_tmp, store) = store();
        let mut s = String::new();
        s.encrypt_in_place(&store, "test.s").unwrap();
        assert_eq!(s, "");
        assert!(!s.is_set());
    }

    #[test]
    fn string_mask_and_restore() {
        let mut s = String::from("Bearer xyz");
        let cur = String::from("Bearer xyz");
        s.mask();
        assert_eq!(s, MASKED_SECRET);
        s.restore_from(&cur);
        assert_eq!(s, "Bearer xyz");
    }

    #[test]
    fn option_string_none_is_noop() {
        let (_tmp, store) = store();
        let mut v: Option<String> = None;
        v.encrypt_in_place(&store, "test.o").unwrap();
        v.decrypt_in_place(&store, "test.o").unwrap();
        v.mask();
        assert_eq!(v, None);
        assert!(!v.is_set());
    }

    #[test]
    fn option_string_some_roundtrip() {
        let (_tmp, store) = store();
        let mut v: Option<String> = Some("Bearer xyz".into());
        v.encrypt_in_place(&store, "test.o").unwrap();
        assert!(SecretStore::is_encrypted(v.as_ref().unwrap()));
        v.decrypt_in_place(&store, "test.o").unwrap();
        assert_eq!(v.as_deref(), Some("Bearer xyz"));
        assert!(v.is_set());
    }

    #[test]
    fn vec_string_roundtrip_per_element() {
        let (_tmp, store) = store();
        let mut v: Vec<String> = vec!["one".into(), "".into(), "two".into()];
        v.encrypt_in_place(&store, "test.v").unwrap();
        assert!(SecretStore::is_encrypted(&v[0]));
        assert_eq!(v[1], "", "empty element must stay empty");
        assert!(SecretStore::is_encrypted(&v[2]));
        v.decrypt_in_place(&store, "test.v").unwrap();
        assert_eq!(v, vec!["one", "", "two"]);
    }

    #[test]
    fn hashmap_string_string_roundtrip_per_value() {
        let (_tmp, store) = store();
        let mut h: HashMap<String, String> = HashMap::from([
            ("Authorization".into(), "Bearer sk-abc".into()),
            ("X-Trace".into(), "req-123".into()),
        ]);
        h.encrypt_in_place(&store, "mcp.servers.foo.headers")
            .unwrap();
        for v in h.values() {
            assert!(SecretStore::is_encrypted(v));
        }
        h.decrypt_in_place(&store, "mcp.servers.foo.headers")
            .unwrap();
        assert_eq!(
            h.get("Authorization").map(String::as_str),
            Some("Bearer sk-abc")
        );
        assert_eq!(h.get("X-Trace").map(String::as_str), Some("req-123"));
        assert!(h.is_set());
    }

    #[test]
    fn hashmap_string_string_mask_and_restore() {
        let mut h: HashMap<String, String> =
            HashMap::from([("Authorization".into(), "Bearer xyz".into())]);
        let cur = h.clone();
        h.mask();
        assert_eq!(
            h.get("Authorization").map(String::as_str),
            Some(MASKED_SECRET)
        );
        h.restore_from(&cur);
        assert_eq!(
            h.get("Authorization").map(String::as_str),
            Some("Bearer xyz")
        );
    }

    #[test]
    fn option_hashmap_none_is_noop() {
        let (_tmp, store) = store();
        let mut v: Option<HashMap<String, String>> = None;
        v.encrypt_in_place(&store, "test.oh").unwrap();
        v.decrypt_in_place(&store, "test.oh").unwrap();
        v.mask();
        assert!(v.is_none());
        assert!(!v.is_set());
    }

    #[test]
    fn option_hashmap_some_roundtrip() {
        let (_tmp, store) = store();
        let mut v: Option<HashMap<String, String>> =
            Some(HashMap::from([("k".into(), "secret".into())]));
        v.encrypt_in_place(&store, "test.oh").unwrap();
        assert!(SecretStore::is_encrypted(
            v.as_ref().unwrap().get("k").unwrap()
        ));
        v.decrypt_in_place(&store, "test.oh").unwrap();
        assert_eq!(
            v.as_ref().unwrap().get("k").map(String::as_str),
            Some("secret")
        );
        assert!(v.is_set());
    }

    #[test]
    fn hashmap_empty_is_not_set() {
        let h: HashMap<String, String> = HashMap::new();
        assert!(!h.is_set());
        let oh: Option<HashMap<String, String>> = Some(HashMap::new());
        assert!(!oh.is_set());
    }

    #[test]
    fn hashmap_with_only_empty_values_is_not_set() {
        let h: HashMap<String, String> = HashMap::from([
            ("Authorization".into(), String::new()),
            ("X-Trace".into(), String::new()),
        ]);
        assert!(!h.is_set());

        let oh: Option<HashMap<String, String>> =
            Some(HashMap::from([("Authorization".into(), String::new())]));
        assert!(!oh.is_set());

        let mixed: HashMap<String, String> = HashMap::from([
            ("Authorization".into(), "Bearer xyz".into()),
            ("X-Trace".into(), String::new()),
        ]);
        assert!(mixed.is_set(), "any non-empty value makes the map set");
    }

    #[test]
    fn encrypt_decrypt_failure_message_includes_field_path() {
        let tmp = TempDir::new().unwrap();
        let bad_store = SecretStore::new(tmp.path(), true);
        // Construct a malformed enc2 string that will fail to decrypt.
        let mut s = String::from("enc2:not-valid-hex");
        let err = s
            .decrypt_in_place(&bad_store, "mcp.servers.foo.headers.Authorization")
            .expect_err("malformed ciphertext must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("mcp.servers.foo.headers.Authorization"),
            "error must include field path; got: {msg}"
        );
    }
}

#[cfg(test)]
mod resource_key_tests {
    // Pins `MapKeySection::resource_key`, the discriminator that
    // `ensure_map_key_for_prop_path` (in the `zeroclawlabs` binary crate)
    // filters on: `true` for sections keyed by a value drawn from another
    // domain (a model id, tool name, …) that may itself contain dots;
    // `false` for sections keyed by a short operator-chosen alias.
    use crate::schema::Config;

    fn resource_key_of(path: &str) -> bool {
        Config::map_key_sections()
            .into_iter()
            .find(|section| section.path == path)
            .unwrap_or_else(|| panic!("no map_key_sections() entry for `{path}`"))
            .resource_key
    }

    #[test]
    fn cost_rate_sections_are_resource_keyed() {
        for path in [
            "cost.rates.tools",
            "cost.rates.providers.models.openai",
            "cost.rates.providers.tts.openai",
            "cost.rates.providers.transcription.openai",
        ] {
            assert!(
                resource_key_of(path),
                "`{path}` prices a resource id (model/voice/tool name), \
                 so its map key must be marked #[resource_key]"
            );
        }
    }

    #[test]
    fn alias_keyed_sections_are_not_resource_keyed() {
        for path in [
            "risk_profiles",
            "peer_groups",
            "channels.telegram",
            "providers.models.openai",
        ] {
            assert!(
                !resource_key_of(path),
                "`{path}` is keyed by an operator-chosen alias, not a \
                 resource id, so it must not be marked #[resource_key]"
            );
        }
    }
}
