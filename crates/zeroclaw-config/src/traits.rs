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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropKind {
    String,
    Bool,
    Integer,
    Float,
    /// An enum or other serde-serializable type (parsed as TOML string).
    Enum,
    /// A `Vec<String>` field; set via comma-separated input.
    StringArray,
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

/// Describes a single property field discovered via `#[derive(Configurable)]`.
#[derive(Clone)]
pub struct PropFieldInfo {
    /// Full dotted name (e.g. `channels.telegram.draft-update-interval-ms`).
    /// Owned so the `HashMap<String, T>` branch of the derive can inject the
    /// runtime map key into the path (`providers.models.anthropic.api-key`)
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
    /// Optional onboard section binding (`#[onboard_section = "providers"]`).
    /// `None` for fields not surfaced by the onboarding wizard.
    /// Web onboarding uses this to group fields into per-section forms;
    /// schemars emits it as `x-zeroclaw-section` for client-side form generation.
    pub onboard_section: Option<&'static str>,
    /// Whether this field's value is derived from a secret (`#[derived_from_secret]`).
    /// Subject to the same write-only / no-readback rules as `#[secret]`.
    /// Reserved for future schema additions; currently no fields are derived.
    pub derived_from_secret: bool,
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

/// The trait for describing a channel
pub trait ChannelConfig {
    /// human-readable name
    fn name() -> &'static str;
    /// short description
    fn desc() -> &'static str;
}

// Maybe there should be a `&self` as parameter for custom channel/info or what...

pub trait ConfigHandle {
    fn name(&self) -> &'static str;
    fn desc(&self) -> &'static str;
}

/// A menu item for `OnboardUi::select`, with an optional status badge
/// (e.g. `[configured]` / `[not set]`) that backends render next to the label.
#[derive(Debug, Clone)]
pub struct SelectItem {
    pub label: String,
    pub badge: Option<String>,
}

impl SelectItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            badge: None,
        }
    }

    pub fn with_badge(label: impl Into<String>, badge: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            badge: Some(badge.into()),
        }
    }
}

/// Result of a single prompt — either the value the user chose, or a
/// navigation signal. Backends return `Answer::Back` when the user presses
/// the backend's back key (Esc on ratatui / dialoguer). Callers rewind.
#[derive(Debug, Clone)]
pub enum Answer<T> {
    Value(T),
    Back,
}

/// Prompt-surface the onboard orchestrator drives.
///
/// Async is deliberate: the orchestrator is already async (Config::load_or_init,
/// Config::save), and a future gateway-backed onboarder (WebSocket → browser)
/// needs to await network I/O per prompt. A sync trait would force that
/// backend to bridge sync↔async via blocking threads and channels, which
/// starves the tokio runtime under concurrent onboarding sessions. Blocking
/// backends (dialoguer) wrap their calls in `tokio::task::spawn_blocking`.
///
/// Idempotency contract: prompts accept a `current` value and pre-populate it
/// as the default. `secret(has_current=true)` returns `None` when the user
/// declines to rotate; callers then skip the write. The orchestrator never
/// calls `config.set_prop` unless the new value differs from `current`.
#[async_trait::async_trait]
pub trait OnboardUi: Send {
    async fn confirm(&mut self, prompt: &str, default: bool) -> anyhow::Result<Answer<bool>>;

    async fn string(
        &mut self,
        prompt: &str,
        current: Option<&str>,
    ) -> anyhow::Result<Answer<String>>;

    /// `Answer::Value(Some(v))` = new secret entered. `Answer::Value(None)` =
    /// user declined to update an existing secret (only when `has_current`).
    /// `Answer::Back` = rewind.
    async fn secret(
        &mut self,
        prompt: &str,
        has_current: bool,
    ) -> anyhow::Result<Answer<Option<String>>>;

    async fn select(
        &mut self,
        prompt: &str,
        items: &[SelectItem],
        current: Option<usize>,
    ) -> anyhow::Result<Answer<usize>>;

    async fn editor(&mut self, hint: &str, initial: &str) -> anyhow::Result<Answer<String>>;

    /// Announce a new section or subsection. `level == 1` = section
    /// (Providers, Channels, …). `level == 2` = subsection within a section
    /// (Hardware › Transport). Backends render these persistently so every
    /// prompt remains anchored to its phase — rendered like Markdown
    /// headings. `level == 1` resets any prior subsection.
    fn heading(&mut self, level: u8, text: &str);
    fn note(&mut self, msg: &str);
    fn status(&mut self, msg: &str);
    fn warn(&mut self, msg: &str);
}
