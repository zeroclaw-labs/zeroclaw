//! Core types, error model, and persistence for the dynamic tool/hook registry.
//!
//! This module defines the foundational data structures used by the dynamic
//! registry system: persisted tool definitions, hook definitions with
//! phase/effect validation, and atomic JSON file persistence.
//!
//! # Serde Compatibility
//!
//! All types use permissive deserialization (no `deny_unknown_fields`) so that
//! older binaries can read files written by newer versions without error.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::channels::traits::ChannelMessage;
use crate::hooks::{HookHandler, HookResult};

// ---------------------------------------------------------------------------
// Error model
// ---------------------------------------------------------------------------

/// Errors originating from dynamic registry operations.
#[derive(Debug)]
pub enum DynamicRegistryError {
    /// A definition failed structural or semantic validation.
    ValidationFailed(String),
    /// A tool or hook name collides with an existing registration.
    NameCollision(String),
    /// The requested tool or hook was not found.
    NotFound(String),
    /// Optimistic-concurrency revision mismatch.
    RevisionConflict { expected: u64, actual: u64 },
    /// Filesystem I/O error during load or save.
    PersistenceError(std::io::Error),
    /// A restart of an underlying subsystem failed.
    RestartFailed(String),
    /// The operation was denied by security policy.
    PolicyDenied(String),
    /// A resource quota has been exceeded.
    QuotaExceeded { kind: String, limit: usize },
    /// The requested tool/hook kind is not recognized.
    UnknownKind(String),
}

impl fmt::Display for DynamicRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed(msg) => write!(f, "validation failed: {msg}"),
            Self::NameCollision(name) => write!(f, "name collision: '{name}' already registered"),
            Self::NotFound(id) => write!(f, "not found: '{id}'"),
            Self::RevisionConflict { expected, actual } => {
                write!(f, "revision conflict: expected {expected}, actual {actual}")
            }
            Self::PersistenceError(err) => write!(f, "persistence error: {err}"),
            Self::RestartFailed(msg) => write!(f, "restart failed: {msg}"),
            Self::PolicyDenied(msg) => write!(f, "policy denied: {msg}"),
            Self::QuotaExceeded { kind, limit } => {
                write!(f, "quota exceeded: {kind} limit is {limit}")
            }
            Self::UnknownKind(kind) => write!(f, "unknown kind: '{kind}'"),
        }
    }
}

impl std::error::Error for DynamicRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PersistenceError(err) => Some(err),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Hook enums and filter/effect types
// ---------------------------------------------------------------------------

/// Whether a hook fires before or after the target event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookPhase {
    Pre,
    Post,
}

/// The event point a hook attaches to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HookPoint {
    ToolCall,
    LlmCall,
    MessageReceived,
    MessageSending,
    PromptBuild,
}

/// Optional filter narrowing which events trigger a hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookFilter {
    pub channel: Option<String>,
    pub tool_name: Option<String>,
}

/// The side-effect a hook produces when triggered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookEffect {
    InjectPromptSuffix(String),
    ModifyArgs(serde_json::Value),
    Cancel(String),
    LogToChannel(String),
}

// ---------------------------------------------------------------------------
// Phase-effect validation
// ---------------------------------------------------------------------------

/// Validate that the given `effect` is allowed for the given `phase`.
///
/// Rules:
/// - `Pre` allows `ModifyArgs`, `InjectPromptSuffix`, `Cancel`.
/// - `Post` allows `LogToChannel`.
/// - All other combinations are rejected.
pub fn validate_phase_effect(
    phase: &HookPhase,
    effect: &HookEffect,
) -> Result<(), DynamicRegistryError> {
    let allowed = match phase {
        HookPhase::Pre => matches!(
            effect,
            HookEffect::ModifyArgs(_) | HookEffect::InjectPromptSuffix(_) | HookEffect::Cancel(_)
        ),
        HookPhase::Post => matches!(effect, HookEffect::LogToChannel(_)),
    };

    if allowed {
        Ok(())
    } else {
        let phase_label = match phase {
            HookPhase::Pre => "Pre",
            HookPhase::Post => "Post",
        };
        let effect_label = match effect {
            HookEffect::InjectPromptSuffix(_) => "InjectPromptSuffix",
            HookEffect::ModifyArgs(_) => "ModifyArgs",
            HookEffect::Cancel(_) => "Cancel",
            HookEffect::LogToChannel(_) => "LogToChannel",
        };
        Err(DynamicRegistryError::ValidationFailed(format!(
            "effect {effect_label} is not allowed in {phase_label} phase"
        )))
    }
}

// ---------------------------------------------------------------------------
// Persisted definitions
// ---------------------------------------------------------------------------

/// A dynamically registered tool definition, persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolDef {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<String>,
}

/// A dynamically registered hook definition, persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicHookDef {
    pub id: String,
    pub name: String,
    pub phase: HookPhase,
    pub target: HookPoint,
    pub priority: i32,
    pub enabled: bool,
    pub filter: Option<HookFilter>,
    pub effect: HookEffect,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// PersistedRegistry — top-level file format
// ---------------------------------------------------------------------------

/// The top-level structure serialized to the dynamic registry JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedRegistry {
    pub schema_version: u32,
    pub tools: Vec<DynamicToolDef>,
    pub hooks: Vec<DynamicHookDef>,
}

impl Default for PersistedRegistry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            tools: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

impl PersistedRegistry {
    /// Load a registry from a JSON file.
    ///
    /// - If the file does not exist, returns an empty default registry.
    /// - If the file cannot be parsed, returns `ValidationFailed`.
    /// - Other I/O errors become `PersistenceError`.
    pub fn load_from_file(path: &Path) -> Result<Self, DynamicRegistryError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(DynamicRegistryError::PersistenceError(e)),
        };

        let registry: Self = serde_json::from_slice(&bytes).map_err(|e| {
            DynamicRegistryError::ValidationFailed(format!("failed to parse registry JSON: {e}"))
        })?;

        // Validate every hook's phase/effect combination on load.
        for hook in &registry.hooks {
            validate_phase_effect(&hook.phase, &hook.effect)?;
        }

        Ok(registry)
    }

    /// Persist the registry to a JSON file atomically.
    ///
    /// 1. Create parent directories if missing.
    /// 2. Write pretty-printed JSON to a `.tmp` sibling.
    /// 3. `fsync` the temp file.
    /// 4. Rename (atomic on same filesystem) to the target path.
    pub fn save_to_file(&self, path: &Path) -> Result<(), DynamicRegistryError> {
        // Validate every hook's phase/effect combination before save.
        for hook in &self.hooks {
            validate_phase_effect(&hook.phase, &hook.effect)?;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(DynamicRegistryError::PersistenceError)?;
        }

        let json = serde_json::to_string_pretty(self).map_err(|e| {
            DynamicRegistryError::PersistenceError(std::io::Error::other(format!(
                "JSON serialization failed: {e}"
            )))
        })?;

        let tmp_path = path.with_extension("tmp");

        let mut file =
            fs::File::create(&tmp_path).map_err(DynamicRegistryError::PersistenceError)?;
        file.write_all(json.as_bytes())
            .map_err(DynamicRegistryError::PersistenceError)?;
        file.sync_all()
            .map_err(DynamicRegistryError::PersistenceError)?;

        fs::rename(&tmp_path, path).map_err(DynamicRegistryError::PersistenceError)?;

        Ok(())
    }

    /// Try to load the registry; quarantine corrupt files instead of failing.
    ///
    /// - Missing file: return empty default.
    /// - Parse/validation error: move the bad file to
    ///   `{path}.corrupt.{unix_timestamp}`, emit a `tracing::warn`, and return
    ///   empty default.
    /// - Never panics.
    pub fn try_load_or_quarantine(path: &Path) -> Self {
        match Self::load_from_file(path) {
            Ok(registry) => registry,
            Err(DynamicRegistryError::PersistenceError(ref e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Self::default()
            }
            Err(err) => {
                // Quarantine the corrupt file.
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let corrupt_name = format!("{}.corrupt.{timestamp}", path.display());
                let corrupt_path = Path::new(&corrupt_name);

                if let Err(rename_err) = fs::rename(path, corrupt_path) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %rename_err,
                        "failed to quarantine corrupt dynamic registry file"
                    );
                } else {
                    tracing::warn!(
                        original = %path.display(),
                        quarantined = %corrupt_path.display(),
                        error = %err,
                        "quarantined corrupt dynamic registry file"
                    );
                }

                Self::default()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DynamicHookInstance — runtime HookHandler backed by a DynamicHookDef
// ---------------------------------------------------------------------------

/// A runtime wrapper that implements [`HookHandler`] for a dynamically
/// created hook definition. Phase, target, filter, and effect are all
/// resolved from the underlying [`DynamicHookDef`].
pub struct DynamicHookInstance {
    def: DynamicHookDef,
}

impl DynamicHookInstance {
    /// Create a new instance from a persisted hook definition.
    pub fn new(def: DynamicHookDef) -> Self {
        Self { def }
    }

    /// Returns `true` if the hook's optional tool-name filter matches
    /// `tool_name`, or if no tool-name filter is set.
    fn matches_tool_filter(&self, tool_name: &str) -> bool {
        match &self.def.filter {
            Some(filter) => filter.tool_name.as_ref().map_or(true, |p| p == tool_name),
            None => true,
        }
    }

    /// Returns `true` if the hook's optional channel filter matches
    /// `channel`, or if no channel filter is set.
    fn matches_channel_filter(&self, channel: &str) -> bool {
        match &self.def.filter {
            Some(filter) => filter.channel.as_ref().map_or(true, |c| c == channel),
            None => true,
        }
    }
}

#[async_trait]
impl HookHandler for DynamicHookInstance {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn priority(&self) -> i32 {
        self.def.priority
    }

    // --- Modifying (Pre) hooks ---

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        if !self.def.enabled
            || self.def.phase != HookPhase::Pre
            || self.def.target != HookPoint::ToolCall
            || !self.matches_tool_filter(&name)
        {
            return HookResult::Continue((name, args));
        }

        match &self.def.effect {
            HookEffect::Cancel(reason) => HookResult::Cancel(reason.clone()),
            HookEffect::ModifyArgs(overrides) => {
                // Shallow object merge: override keys from `overrides` into `args`.
                let mut merged = args;
                if let (Some(base), Some(patch)) = (merged.as_object_mut(), overrides.as_object()) {
                    for (k, v) in patch {
                        base.insert(k.clone(), v.clone());
                    }
                }
                HookResult::Continue((name, merged))
            }
            // InjectPromptSuffix is wrong target — pass through.
            _ => HookResult::Continue((name, args)),
        }
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        if !self.def.enabled
            || self.def.phase != HookPhase::Pre
            || self.def.target != HookPoint::PromptBuild
        {
            return HookResult::Continue(prompt);
        }

        match &self.def.effect {
            HookEffect::InjectPromptSuffix(suffix) => {
                let mut extended = prompt;
                extended.push_str(suffix);
                HookResult::Continue(extended)
            }
            HookEffect::Cancel(reason) => HookResult::Cancel(reason.clone()),
            _ => HookResult::Continue(prompt),
        }
    }

    async fn before_llm_call(
        &self,
        messages: Vec<crate::providers::traits::ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<crate::providers::traits::ChatMessage>, String)> {
        if !self.def.enabled
            || self.def.phase != HookPhase::Pre
            || self.def.target != HookPoint::LlmCall
        {
            return HookResult::Continue((messages, model));
        }

        match &self.def.effect {
            HookEffect::Cancel(reason) => HookResult::Cancel(reason.clone()),
            _ => HookResult::Continue((messages, model)),
        }
    }

    async fn on_message_received(&self, message: ChannelMessage) -> HookResult<ChannelMessage> {
        if !self.def.enabled
            || self.def.phase != HookPhase::Pre
            || self.def.target != HookPoint::MessageReceived
            || !self.matches_channel_filter(&message.channel)
        {
            return HookResult::Continue(message);
        }

        match &self.def.effect {
            HookEffect::Cancel(reason) => HookResult::Cancel(reason.clone()),
            _ => HookResult::Continue(message),
        }
    }

    async fn on_message_sending(
        &self,
        channel: String,
        recipient: String,
        content: String,
    ) -> HookResult<(String, String, String)> {
        if !self.def.enabled
            || self.def.phase != HookPhase::Pre
            || self.def.target != HookPoint::MessageSending
            || !self.matches_channel_filter(&channel)
        {
            return HookResult::Continue((channel, recipient, content));
        }

        match &self.def.effect {
            HookEffect::Cancel(reason) => HookResult::Cancel(reason.clone()),
            _ => HookResult::Continue((channel, recipient, content)),
        }
    }

    // --- Void (Post) hooks ---

    async fn on_after_tool_call(
        &self,
        tool: &str,
        _result: &crate::tools::traits::ToolResult,
        _duration: std::time::Duration,
    ) {
        if !self.def.enabled
            || self.def.phase != HookPhase::Post
            || self.def.target != HookPoint::ToolCall
            || !self.matches_tool_filter(tool)
        {
            return;
        }

        if let HookEffect::LogToChannel(channel) = &self.def.effect {
            tracing::info!(
                hook = %self.def.name,
                tool = %tool,
                channel = %channel,
                "dynamic post-tool hook fired"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: build a sample tool definition for tests.
    fn sample_tool() -> DynamicToolDef {
        DynamicToolDef {
            id: "tool-001".into(),
            name: "echo_tool".into(),
            description: "Echoes back input".into(),
            kind: "shell".into(),
            config: serde_json::json!({"command": "echo"}),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: Some("zeroclaw_user".into()),
        }
    }

    /// Helper: build a sample hook definition for tests.
    fn sample_hook() -> DynamicHookDef {
        DynamicHookDef {
            id: "hook-001".into(),
            name: "pre_tool_cancel".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: Some(HookFilter {
                channel: None,
                tool_name: Some("shell".into()),
            }),
            effect: HookEffect::Cancel("blocked by policy".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ------------------------------------------------------------------
    // 1. persisted_registry_serde_round_trip
    // ------------------------------------------------------------------
    #[test]
    fn persisted_registry_serde_round_trip() {
        let registry = PersistedRegistry {
            schema_version: 1,
            tools: vec![sample_tool()],
            hooks: vec![sample_hook()],
        };

        let json = serde_json::to_string_pretty(&registry).unwrap();
        let parsed: PersistedRegistry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.tools[0].id, "tool-001");
        assert_eq!(parsed.tools[0].name, "echo_tool");
        assert_eq!(parsed.tools[0].kind, "shell");
        assert!(parsed.tools[0].enabled);
        assert_eq!(parsed.tools[0].created_by.as_deref(), Some("zeroclaw_user"));

        assert_eq!(parsed.hooks.len(), 1);
        assert_eq!(parsed.hooks[0].id, "hook-001");
        assert_eq!(parsed.hooks[0].phase, HookPhase::Pre);
        assert_eq!(parsed.hooks[0].target, HookPoint::ToolCall);
        assert_eq!(parsed.hooks[0].priority, 10);
    }

    // ------------------------------------------------------------------
    // 2. all_hook_phase_variants_serde
    // ------------------------------------------------------------------
    #[test]
    fn all_hook_phase_variants_serde() {
        for phase in &[HookPhase::Pre, HookPhase::Post] {
            let json = serde_json::to_string(phase).unwrap();
            let parsed: HookPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, phase);
        }
    }

    // ------------------------------------------------------------------
    // 3. all_hook_point_variants_serde
    // ------------------------------------------------------------------
    #[test]
    fn all_hook_point_variants_serde() {
        let variants = [
            HookPoint::ToolCall,
            HookPoint::LlmCall,
            HookPoint::MessageReceived,
            HookPoint::MessageSending,
            HookPoint::PromptBuild,
        ];
        for point in &variants {
            let json = serde_json::to_string(point).unwrap();
            let parsed: HookPoint = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, point);
        }
    }

    // ------------------------------------------------------------------
    // 4. all_hook_effect_variants_serde
    // ------------------------------------------------------------------
    #[test]
    fn all_hook_effect_variants_serde() {
        let variants: Vec<HookEffect> = vec![
            HookEffect::InjectPromptSuffix("suffix".into()),
            HookEffect::ModifyArgs(serde_json::json!({"key": "val"})),
            HookEffect::Cancel("reason".into()),
            HookEffect::LogToChannel("general".into()),
        ];
        for effect in &variants {
            let json = serde_json::to_string(effect).unwrap();
            let parsed: HookEffect = serde_json::from_str(&json).unwrap();
            // Compare via round-trip JSON equality.
            assert_eq!(
                serde_json::to_value(&parsed).unwrap(),
                serde_json::to_value(effect).unwrap()
            );
        }
    }

    // ------------------------------------------------------------------
    // 5. hook_phase_effect_validation_pre_allowed
    // ------------------------------------------------------------------
    #[test]
    fn hook_phase_effect_validation_pre_allowed() {
        let pre = HookPhase::Pre;
        assert!(
            validate_phase_effect(&pre, &HookEffect::ModifyArgs(serde_json::json!({}))).is_ok()
        );
        assert!(validate_phase_effect(&pre, &HookEffect::InjectPromptSuffix("s".into())).is_ok());
        assert!(validate_phase_effect(&pre, &HookEffect::Cancel("r".into())).is_ok());
    }

    // ------------------------------------------------------------------
    // 6. hook_phase_effect_validation_pre_denied
    // ------------------------------------------------------------------
    #[test]
    fn hook_phase_effect_validation_pre_denied() {
        let pre = HookPhase::Pre;
        let result = validate_phase_effect(&pre, &HookEffect::LogToChannel("ch".into()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LogToChannel") && msg.contains("Pre"),
            "expected meaningful error, got: {msg}"
        );
    }

    // ------------------------------------------------------------------
    // 7. hook_phase_effect_validation_post_allowed
    // ------------------------------------------------------------------
    #[test]
    fn hook_phase_effect_validation_post_allowed() {
        let post = HookPhase::Post;
        assert!(validate_phase_effect(&post, &HookEffect::LogToChannel("ch".into())).is_ok());
    }

    // ------------------------------------------------------------------
    // 8. hook_phase_effect_validation_post_denied
    // ------------------------------------------------------------------
    #[test]
    fn hook_phase_effect_validation_post_denied() {
        let post = HookPhase::Post;

        let denied_effects: Vec<HookEffect> = vec![
            HookEffect::Cancel("r".into()),
            HookEffect::ModifyArgs(serde_json::json!({})),
            HookEffect::InjectPromptSuffix("s".into()),
        ];

        for effect in &denied_effects {
            let result = validate_phase_effect(&post, effect);
            assert!(result.is_err(), "expected Post to deny {effect:?}");
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("Post"),
                "error should mention Post phase: {msg}"
            );
        }
    }

    // ------------------------------------------------------------------
    // 9. persistence_round_trip
    // ------------------------------------------------------------------
    #[test]
    fn persistence_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.json");

        let registry = PersistedRegistry {
            schema_version: 1,
            tools: vec![sample_tool()],
            hooks: vec![sample_hook()],
        };

        registry.save_to_file(&path).unwrap();
        let loaded = PersistedRegistry::load_from_file(&path).unwrap();

        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools[0].name, "echo_tool");
        assert_eq!(loaded.hooks.len(), 1);
        assert_eq!(loaded.hooks[0].name, "pre_tool_cancel");
    }

    // ------------------------------------------------------------------
    // 10. load_missing_file_returns_empty
    // ------------------------------------------------------------------
    #[test]
    fn load_missing_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does_not_exist.json");

        let registry = PersistedRegistry::load_from_file(&path).unwrap();
        assert_eq!(registry.schema_version, 1);
        assert!(registry.tools.is_empty());
        assert!(registry.hooks.is_empty());
    }

    // ------------------------------------------------------------------
    // 11. try_load_corrupt_file_quarantines
    // ------------------------------------------------------------------
    #[test]
    fn try_load_corrupt_file_quarantines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.json");

        // Write invalid JSON.
        fs::write(&path, b"NOT VALID JSON {{{{").unwrap();
        assert!(path.exists());

        let registry = PersistedRegistry::try_load_or_quarantine(&path);

        // Should return empty default.
        assert_eq!(registry.schema_version, 1);
        assert!(registry.tools.is_empty());
        assert!(registry.hooks.is_empty());

        // Original file should be gone.
        assert!(!path.exists(), "original file should have been renamed");

        // A .corrupt.* file should exist in the same directory.
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt."))
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one quarantined file, found: {entries:?}"
        );
    }

    // ------------------------------------------------------------------
    // 12. dynamic_registry_error_display
    // ------------------------------------------------------------------
    #[test]
    fn dynamic_registry_error_display() {
        let cases: Vec<(DynamicRegistryError, &str)> = vec![
            (
                DynamicRegistryError::ValidationFailed("bad input".into()),
                "validation failed: bad input",
            ),
            (
                DynamicRegistryError::NameCollision("my_tool".into()),
                "name collision: 'my_tool' already registered",
            ),
            (
                DynamicRegistryError::NotFound("id-99".into()),
                "not found: 'id-99'",
            ),
            (
                DynamicRegistryError::RevisionConflict {
                    expected: 3,
                    actual: 5,
                },
                "revision conflict: expected 3, actual 5",
            ),
            (
                DynamicRegistryError::PersistenceError(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "access denied",
                )),
                "persistence error: access denied",
            ),
            (
                DynamicRegistryError::RestartFailed("timeout".into()),
                "restart failed: timeout",
            ),
            (
                DynamicRegistryError::PolicyDenied("not allowed".into()),
                "policy denied: not allowed",
            ),
            (
                DynamicRegistryError::QuotaExceeded {
                    kind: "tools".into(),
                    limit: 50,
                },
                "quota exceeded: tools limit is 50",
            ),
            (
                DynamicRegistryError::UnknownKind("magic".into()),
                "unknown kind: 'magic'",
            ),
        ];

        for (error, expected) in &cases {
            let display = format!("{error}");
            assert_eq!(&display, expected, "Display mismatch for {error:?}");
        }
    }

    // ------------------------------------------------------------------
    // Extra: forward-compatible serde (unknown fields are ignored)
    // ------------------------------------------------------------------
    #[test]
    fn serde_ignores_unknown_fields() {
        // Simulate a registry file written by a newer version with an extra field.
        let json = r#"{
            "schema_version": 1,
            "tools": [],
            "hooks": [],
            "future_field": "should be ignored"
        }"#;
        let parsed: PersistedRegistry = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.tools.is_empty());
    }

    // ------------------------------------------------------------------
    // Extra: save_to_file creates parent directories
    // ------------------------------------------------------------------
    #[test]
    fn save_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("registry.json");

        let registry = PersistedRegistry::default();
        registry.save_to_file(&nested).unwrap();
        assert!(nested.exists());
    }

    // ------------------------------------------------------------------
    // Extra: PersistenceError source chain
    // ------------------------------------------------------------------
    #[test]
    fn persistence_error_has_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "disk full");
        let err = DynamicRegistryError::PersistenceError(io_err);
        assert!(std::error::Error::source(&err).is_some());

        // Non-IO variants have no source.
        let err2 = DynamicRegistryError::NotFound("x".into());
        assert!(std::error::Error::source(&err2).is_none());
    }

    // ==================================================================
    // DynamicHookInstance tests
    // ==================================================================

    use crate::channels::traits::ChannelMessage;
    use crate::hooks::{HookHandler, HookResult};
    use crate::tools::traits::ToolResult;
    use std::time::Duration;

    /// Helper: build a DynamicHookInstance from a hook def.
    fn make_hook_instance(def: DynamicHookDef) -> DynamicHookInstance {
        DynamicHookInstance::new(def)
    }

    /// Helper: build a Pre/ToolCall hook def with given effect and filter.
    fn pre_tool_call_hook(
        effect: HookEffect,
        filter: Option<HookFilter>,
    ) -> DynamicHookDef {
        DynamicHookDef {
            id: "hook-test".into(),
            name: "test_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 5,
            enabled: true,
            filter,
            effect,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ------------------------------------------------------------------
    // 1. dynamic_hook_pre_tool_call_cancel
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_pre_tool_call_cancel() {
        let def = pre_tool_call_hook(
            HookEffect::Cancel("blocked".into()),
            Some(HookFilter {
                channel: None,
                tool_name: Some("shell".into()),
            }),
        );
        let hook = make_hook_instance(def);

        let result = hook
            .before_tool_call("shell".into(), serde_json::json!({"cmd": "ls"}))
            .await;

        assert!(result.is_cancel());
        match result {
            HookResult::Cancel(reason) => assert_eq!(reason, "blocked"),
            HookResult::Continue(_) => panic!("expected Cancel"),
        }
    }

    // ------------------------------------------------------------------
    // 2. dynamic_hook_pre_tool_call_filter_no_match
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_pre_tool_call_filter_no_match() {
        let def = pre_tool_call_hook(
            HookEffect::Cancel("blocked".into()),
            Some(HookFilter {
                channel: None,
                tool_name: Some("shell".into()),
            }),
        );
        let hook = make_hook_instance(def);

        // Call with a different tool name — filter should not match.
        let result = hook
            .before_tool_call("file_read".into(), serde_json::json!({}))
            .await;

        assert!(!result.is_cancel());
        match result {
            HookResult::Continue((name, _)) => assert_eq!(name, "file_read"),
            HookResult::Cancel(_) => panic!("expected Continue"),
        }
    }

    // ------------------------------------------------------------------
    // 3. dynamic_hook_pre_tool_call_modify_args
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_pre_tool_call_modify_args() {
        let def = pre_tool_call_hook(
            HookEffect::ModifyArgs(serde_json::json!({"timeout": 30, "verbose": true})),
            None, // no filter — matches all tools
        );
        let hook = make_hook_instance(def);

        let original_args = serde_json::json!({"cmd": "ls", "timeout": 10});
        let result = hook
            .before_tool_call("shell".into(), original_args)
            .await;

        match result {
            HookResult::Continue((name, args)) => {
                assert_eq!(name, "shell");
                // "timeout" overridden to 30, "verbose" injected, "cmd" preserved.
                assert_eq!(args["cmd"], "ls");
                assert_eq!(args["timeout"], 30);
                assert_eq!(args["verbose"], true);
            }
            HookResult::Cancel(_) => panic!("expected Continue with merged args"),
        }
    }

    // ------------------------------------------------------------------
    // 4. dynamic_hook_pre_prompt_build_inject_suffix
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_pre_prompt_build_inject_suffix() {
        let def = DynamicHookDef {
            id: "hook-prompt".into(),
            name: "prompt_suffix_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::PromptBuild,
            priority: 1,
            enabled: true,
            filter: None,
            effect: HookEffect::InjectPromptSuffix("\nAlways respond in JSON.".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let hook = make_hook_instance(def);

        let result = hook.before_prompt_build("You are a helpful agent.".into()).await;

        match result {
            HookResult::Continue(prompt) => {
                assert_eq!(prompt, "You are a helpful agent.\nAlways respond in JSON.");
            }
            HookResult::Cancel(_) => panic!("expected Continue with appended suffix"),
        }
    }

    // ------------------------------------------------------------------
    // 5. dynamic_hook_post_tool_call_log
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_post_tool_call_log() {
        let def = DynamicHookDef {
            id: "hook-post".into(),
            name: "post_log_hook".into(),
            phase: HookPhase::Post,
            target: HookPoint::ToolCall,
            priority: 0,
            enabled: true,
            filter: None,
            effect: HookEffect::LogToChannel("audit".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let hook = make_hook_instance(def);

        let tool_result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };

        // Should not panic; just logs via tracing.
        hook.on_after_tool_call("shell", &tool_result, Duration::from_millis(10))
            .await;
    }

    // ------------------------------------------------------------------
    // 6. dynamic_hook_disabled_passes_through
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_disabled_passes_through() {
        let mut def = pre_tool_call_hook(
            HookEffect::Cancel("should not fire".into()),
            None,
        );
        def.enabled = false;
        let hook = make_hook_instance(def);

        let result = hook
            .before_tool_call("shell".into(), serde_json::json!({"cmd": "rm -rf /"}))
            .await;

        assert!(!result.is_cancel());
        match result {
            HookResult::Continue((name, _)) => assert_eq!(name, "shell"),
            HookResult::Cancel(_) => panic!("expected Continue — hook is disabled"),
        }
    }

    // ------------------------------------------------------------------
    // 7. dynamic_hook_wrong_target_passes_through
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_wrong_target_passes_through() {
        // Hook targets ToolCall, but we call before_prompt_build.
        let def = pre_tool_call_hook(
            HookEffect::Cancel("should not fire".into()),
            None,
        );
        let hook = make_hook_instance(def);

        let result = hook.before_prompt_build("original prompt".into()).await;

        match result {
            HookResult::Continue(prompt) => assert_eq!(prompt, "original prompt"),
            HookResult::Cancel(_) => panic!("expected Continue — wrong target"),
        }
    }

    // ------------------------------------------------------------------
    // 8. dynamic_hook_channel_filter_match
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_channel_filter_match() {
        let def = DynamicHookDef {
            id: "hook-msg".into(),
            name: "msg_cancel_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::MessageReceived,
            priority: 0,
            enabled: true,
            filter: Some(HookFilter {
                channel: Some("telegram".into()),
                tool_name: None,
            }),
            effect: HookEffect::Cancel("telegram blocked".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let hook = make_hook_instance(def);

        let msg = ChannelMessage {
            id: "m1".into(),
            sender: "zeroclaw_user".into(),
            reply_target: "zeroclaw_user".into(),
            content: "hello".into(),
            channel: "telegram".into(),
            timestamp: 1000,
            thread_ts: None,
        };

        let result = hook.on_message_received(msg).await;

        assert!(result.is_cancel());
        match result {
            HookResult::Cancel(reason) => assert_eq!(reason, "telegram blocked"),
            HookResult::Continue(_) => panic!("expected Cancel"),
        }
    }

    // ------------------------------------------------------------------
    // 9. dynamic_hook_channel_filter_no_match
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn dynamic_hook_channel_filter_no_match() {
        let def = DynamicHookDef {
            id: "hook-msg2".into(),
            name: "msg_cancel_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::MessageReceived,
            priority: 0,
            enabled: true,
            filter: Some(HookFilter {
                channel: Some("telegram".into()),
                tool_name: None,
            }),
            effect: HookEffect::Cancel("telegram blocked".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let hook = make_hook_instance(def);

        let msg = ChannelMessage {
            id: "m2".into(),
            sender: "zeroclaw_user".into(),
            reply_target: "zeroclaw_user".into(),
            content: "hello".into(),
            channel: "discord".into(), // different channel — should not match
            timestamp: 1000,
            thread_ts: None,
        };

        let result = hook.on_message_received(msg).await;

        assert!(!result.is_cancel());
        match result {
            HookResult::Continue(m) => assert_eq!(m.channel, "discord"),
            HookResult::Cancel(_) => panic!("expected Continue — channel filter did not match"),
        }
    }
}
