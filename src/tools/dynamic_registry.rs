//! Core types, error model, persistence, and runtime registry for the dynamic
//! tool/hook system.
//!
//! This module defines the foundational data structures used by the dynamic
//! registry system: persisted tool definitions, hook definitions with
//! phase/effect validation, atomic JSON file persistence, and the central
//! [`DynamicRegistry`] container that manages static + dynamic tools/hooks
//! with versioned [`RegistrySnapshot`] isolation.
//!
//! # Serde Compatibility
//!
//! All types use permissive deserialization (no `deny_unknown_fields`) so that
//! older binaries can read files written by newer versions without error.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::channels::traits::ChannelMessage;
use crate::config::DynamicRegistryConfig;
use crate::hooks::{HookHandler, HookResult};
use crate::tools::dynamic_factories::{
    default_factory_registry, DynamicToolFactory, ToolBuildContext,
};
use crate::tools::traits::{Tool, ToolSpec};

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
// RegistrySnapshot — immutable view captured per agent turn
// ---------------------------------------------------------------------------

/// An immutable, cheaply clonable snapshot of the registry state at a point
/// in time. The agent loop captures one snapshot per turn so that tool lists
/// and hook chains are stable for the duration of a single LLM call.
pub struct RegistrySnapshot {
    /// Monotonic counter bumped on any mutation (tool or hook).
    pub state_revision: u64,
    /// Monotonic counter bumped only on tool mutations.
    pub tool_revision: u64,
    /// Monotonic counter bumped only on hook mutations.
    pub hook_revision: u64,
    /// Static tools first (insertion order), then enabled dynamic tools sorted by name.
    pub all_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// Precomputed [`ToolSpec`] list matching `all_tools` order.
    pub tool_specs: Arc<Vec<ToolSpec>>,
    /// Enabled dynamic hooks sorted by (priority desc, id asc).
    pub dynamic_hooks: Arc<Vec<Arc<dyn HookHandler>>>,
}

// ---------------------------------------------------------------------------
// DynamicRegistryState — interior mutable state behind Mutex
// ---------------------------------------------------------------------------

struct DynamicRegistryState {
    tools: BTreeMap<String, DynamicToolDef>,
    hooks: BTreeMap<String, DynamicHookDef>,
    tool_instances: HashMap<String, Arc<dyn Tool>>,
    hook_instances: HashMap<String, Arc<DynamicHookInstance>>,
    state_revision: u64,
    tool_revision: u64,
    hook_revision: u64,
}

// ---------------------------------------------------------------------------
// DynamicRegistry — the central runtime container
// ---------------------------------------------------------------------------

/// Central runtime container that manages static + dynamic tools/hooks with
/// versioned snapshots. Thread-safe: snapshot reads are lock-free (`Arc` clone),
/// mutations are serialized via an interior `Mutex`.
pub struct DynamicRegistry {
    static_tools: Vec<Arc<dyn Tool>>,
    static_tool_names: HashSet<String>,
    snapshot: RwLock<Arc<RegistrySnapshot>>,
    state: Mutex<DynamicRegistryState>,
    persistence_path: PathBuf,
    factories: HashMap<String, Box<dyn DynamicToolFactory>>,
    config: DynamicRegistryConfig,
    #[allow(dead_code)]
    build_ctx: Option<ToolBuildContext>,
}

impl DynamicRegistry {
    /// Create a new registry with static tools and load persisted dynamic state.
    pub fn new(
        static_tools: Vec<Arc<dyn Tool>>,
        config: DynamicRegistryConfig,
        persistence_path: PathBuf,
        build_ctx: ToolBuildContext,
    ) -> Self {
        let static_tool_names: HashSet<String> =
            static_tools.iter().map(|t| t.name().to_string()).collect();
        let factories = default_factory_registry();

        // Load persisted state (quarantines corrupt files).
        let persisted = PersistedRegistry::try_load_or_quarantine(&persistence_path);

        let mut tool_defs = BTreeMap::new();
        let mut tool_instances = HashMap::new();
        let mut hook_defs = BTreeMap::new();
        let mut hook_instances = HashMap::new();

        // Restore persisted tools.
        for def in persisted.tools {
            // Validate kind is allowed.
            if !config.allowed_tool_kinds.contains(&def.kind) {
                tracing::warn!(
                    id = %def.id,
                    kind = %def.kind,
                    "skipping persisted tool: kind not in allowed_tool_kinds"
                );
                continue;
            }
            // Build instance via factory.
            match factories.get(&def.kind) {
                Some(factory) => match factory.build(&def, &build_ctx) {
                    Ok(instance) => {
                        tool_instances.insert(def.id.clone(), instance);
                        tool_defs.insert(def.id.clone(), def);
                    }
                    Err(e) => {
                        tracing::warn!(
                            id = %def.id,
                            error = %e,
                            "skipping persisted tool: factory build failed"
                        );
                    }
                },
                None => {
                    tracing::warn!(
                        id = %def.id,
                        kind = %def.kind,
                        "skipping persisted tool: no factory for kind"
                    );
                }
            }
        }

        // Restore persisted hooks.
        for def in persisted.hooks {
            if let Err(e) = validate_phase_effect(&def.phase, &def.effect) {
                tracing::warn!(
                    id = %def.id,
                    error = %e,
                    "skipping persisted hook: phase/effect validation failed"
                );
                continue;
            }
            let instance = Arc::new(DynamicHookInstance::new(def.clone()));
            hook_instances.insert(def.id.clone(), instance);
            hook_defs.insert(def.id.clone(), def);
        }

        let internal_state = DynamicRegistryState {
            tools: tool_defs,
            hooks: hook_defs,
            tool_instances,
            hook_instances,
            state_revision: 0,
            tool_revision: 0,
            hook_revision: 0,
        };

        let snapshot = Self::build_snapshot(&static_tools, &internal_state);

        Self {
            static_tools,
            static_tool_names,
            snapshot: RwLock::new(Arc::new(snapshot)),
            state: Mutex::new(internal_state),
            persistence_path,
            factories,
            config,
            build_ctx: Some(build_ctx),
        }
    }

    /// Create an empty registry for testing (no static tools, temp path).
    pub fn new_empty(config: DynamicRegistryConfig) -> Self {
        let static_tools: Vec<Arc<dyn Tool>> = Vec::new();
        let static_tool_names = HashSet::new();
        let factories = default_factory_registry();

        let internal_state = DynamicRegistryState {
            tools: BTreeMap::new(),
            hooks: BTreeMap::new(),
            tool_instances: HashMap::new(),
            hook_instances: HashMap::new(),
            state_revision: 0,
            tool_revision: 0,
            hook_revision: 0,
        };

        let snapshot = Self::build_snapshot(&static_tools, &internal_state);

        // Use a path that won't collide; callers should provide a real temp path
        // for persistence tests.
        let persistence_path = std::env::temp_dir().join("zeroclaw_test_registry.json");

        Self {
            static_tools,
            static_tool_names,
            snapshot: RwLock::new(Arc::new(snapshot)),
            state: Mutex::new(internal_state),
            persistence_path,
            factories,
            config,
            build_ctx: None,
        }
    }

    /// Get the current snapshot (cheap `Arc` clone).
    pub fn snapshot(&self) -> Arc<RegistrySnapshot> {
        self.snapshot
            .read()
            .expect("snapshot RwLock poisoned")
            .clone()
    }

    /// Current tool revision counter.
    pub fn tool_revision(&self) -> u64 {
        self.state
            .lock()
            .expect("state Mutex poisoned")
            .tool_revision
    }

    // ── Tool mutations ──────────────────────────────────────────────

    /// Add a dynamic tool definition. Returns the new `state_revision`.
    ///
    /// If `expected_revision` is `Some(r)` and `state_revision != r`,
    /// returns `RevisionConflict`.
    pub fn add_tool(
        &self,
        def: DynamicToolDef,
        expected_revision: Option<u64>,
    ) -> Result<u64, DynamicRegistryError> {
        let mut state = self.state.lock().expect("state Mutex poisoned");

        // Optimistic concurrency check.
        if let Some(expected) = expected_revision {
            if state.state_revision != expected {
                return Err(DynamicRegistryError::RevisionConflict {
                    expected,
                    actual: state.state_revision,
                });
            }
        }

        // Quota check.
        if state.tools.len() >= self.config.max_tools {
            return Err(DynamicRegistryError::QuotaExceeded {
                kind: "tools".into(),
                limit: self.config.max_tools,
            });
        }

        // Kind check.
        if !self.config.allowed_tool_kinds.contains(&def.kind) {
            return Err(DynamicRegistryError::UnknownKind(def.kind.clone()));
        }

        // Name collision with static tools.
        if self.static_tool_names.contains(&def.name) {
            return Err(DynamicRegistryError::NameCollision(def.name.clone()));
        }

        // Name collision with existing dynamic tools.
        for existing in state.tools.values() {
            if existing.name == def.name {
                return Err(DynamicRegistryError::NameCollision(def.name.clone()));
            }
        }

        // Validate config via factory.
        let factory = self
            .factories
            .get(&def.kind)
            .ok_or_else(|| DynamicRegistryError::UnknownKind(def.kind.clone()))?;
        factory
            .validate(&def.config)
            .map_err(|e| DynamicRegistryError::ValidationFailed(e.to_string()))?;

        // Build tool instance.
        let instance = if let Some(ref ctx) = self.build_ctx {
            factory
                .build(&def, ctx)
                .map_err(|e| DynamicRegistryError::ValidationFailed(e.to_string()))?
        } else {
            // Test path: create a stub tool for registries without build_ctx.
            Arc::new(StubDynamicTool {
                name: def.name.clone(),
                description: def.description.clone(),
            })
        };

        // Insert.
        state.tool_instances.insert(def.id.clone(), instance);
        state.tools.insert(def.id.clone(), def);

        // Bump revisions.
        state.state_revision += 1;
        state.tool_revision += 1;
        let new_revision = state.state_revision;

        // Persist.
        self.persist(&state);

        // Rebuild snapshot.
        let snap = Self::build_snapshot(&self.static_tools, &state);
        drop(state);
        *self.snapshot.write().expect("snapshot RwLock poisoned") = Arc::new(snap);

        Ok(new_revision)
    }

    /// Remove a dynamic tool by ID. Returns the new `state_revision`.
    pub fn remove_tool(&self, id: &str) -> Result<u64, DynamicRegistryError> {
        let mut state = self.state.lock().expect("state Mutex poisoned");

        if state.tools.remove(id).is_none() {
            return Err(DynamicRegistryError::NotFound(id.to_string()));
        }
        state.tool_instances.remove(id);

        state.state_revision += 1;
        state.tool_revision += 1;
        let new_revision = state.state_revision;

        self.persist(&state);

        let snap = Self::build_snapshot(&self.static_tools, &state);
        drop(state);
        *self.snapshot.write().expect("snapshot RwLock poisoned") = Arc::new(snap);

        Ok(new_revision)
    }

    /// Enable or disable a dynamic tool.
    pub fn enable_tool(&self, id: &str, enabled: bool) -> Result<(), DynamicRegistryError> {
        let mut state = self.state.lock().expect("state Mutex poisoned");

        let def = state
            .tools
            .get_mut(id)
            .ok_or_else(|| DynamicRegistryError::NotFound(id.to_string()))?;
        def.enabled = enabled;
        def.updated_at = Utc::now();

        state.state_revision += 1;
        state.tool_revision += 1;

        self.persist(&state);

        let snap = Self::build_snapshot(&self.static_tools, &state);
        drop(state);
        *self.snapshot.write().expect("snapshot RwLock poisoned") = Arc::new(snap);

        Ok(())
    }

    /// List all dynamic tool definitions (enabled and disabled).
    pub fn list_tools(&self) -> Vec<DynamicToolDef> {
        let state = self.state.lock().expect("state Mutex poisoned");
        state.tools.values().cloned().collect()
    }

    /// Get a specific dynamic tool definition by ID.
    pub fn get_tool(&self, id: &str) -> Option<DynamicToolDef> {
        let state = self.state.lock().expect("state Mutex poisoned");
        state.tools.get(id).cloned()
    }

    // ── Hook mutations ──────────────────────────────────────────────

    /// Add a dynamic hook definition. Returns the new `state_revision`.
    pub fn add_hook(
        &self,
        def: DynamicHookDef,
        expected_revision: Option<u64>,
    ) -> Result<u64, DynamicRegistryError> {
        let mut state = self.state.lock().expect("state Mutex poisoned");

        // Optimistic concurrency check.
        if let Some(expected) = expected_revision {
            if state.state_revision != expected {
                return Err(DynamicRegistryError::RevisionConflict {
                    expected,
                    actual: state.state_revision,
                });
            }
        }

        // Quota check.
        if state.hooks.len() >= self.config.max_hooks {
            return Err(DynamicRegistryError::QuotaExceeded {
                kind: "hooks".into(),
                limit: self.config.max_hooks,
            });
        }

        // Validate phase/effect combination.
        validate_phase_effect(&def.phase, &def.effect)?;

        // Build instance.
        let instance = Arc::new(DynamicHookInstance::new(def.clone()));

        // Insert.
        state.hook_instances.insert(def.id.clone(), instance);
        state.hooks.insert(def.id.clone(), def);

        // Bump revisions (hook mutations bump state_revision + hook_revision, NOT tool_revision).
        state.state_revision += 1;
        state.hook_revision += 1;
        let new_revision = state.state_revision;

        self.persist(&state);

        let snap = Self::build_snapshot(&self.static_tools, &state);
        drop(state);
        *self.snapshot.write().expect("snapshot RwLock poisoned") = Arc::new(snap);

        Ok(new_revision)
    }

    /// Remove a dynamic hook by ID. Returns the new `state_revision`.
    pub fn remove_hook(&self, id: &str) -> Result<u64, DynamicRegistryError> {
        let mut state = self.state.lock().expect("state Mutex poisoned");

        if state.hooks.remove(id).is_none() {
            return Err(DynamicRegistryError::NotFound(id.to_string()));
        }
        state.hook_instances.remove(id);

        state.state_revision += 1;
        state.hook_revision += 1;
        let new_revision = state.state_revision;

        self.persist(&state);

        let snap = Self::build_snapshot(&self.static_tools, &state);
        drop(state);
        *self.snapshot.write().expect("snapshot RwLock poisoned") = Arc::new(snap);

        Ok(new_revision)
    }

    /// Enable or disable a dynamic hook.
    pub fn enable_hook(&self, id: &str, enabled: bool) -> Result<(), DynamicRegistryError> {
        let mut state = self.state.lock().expect("state Mutex poisoned");

        let def = state
            .hooks
            .get_mut(id)
            .ok_or_else(|| DynamicRegistryError::NotFound(id.to_string()))?;
        def.enabled = enabled;
        def.updated_at = Utc::now();

        // Rebuild the hook instance so it picks up the new enabled state.
        let instance = Arc::new(DynamicHookInstance::new(def.clone()));
        state.hook_instances.insert(id.to_string(), instance);

        state.state_revision += 1;
        state.hook_revision += 1;

        self.persist(&state);

        let snap = Self::build_snapshot(&self.static_tools, &state);
        drop(state);
        *self.snapshot.write().expect("snapshot RwLock poisoned") = Arc::new(snap);

        Ok(())
    }

    /// List all dynamic hook definitions (enabled and disabled).
    pub fn list_hooks(&self) -> Vec<DynamicHookDef> {
        let state = self.state.lock().expect("state Mutex poisoned");
        state.hooks.values().cloned().collect()
    }

    /// Get a specific dynamic hook definition by ID.
    pub fn get_hook(&self, id: &str) -> Option<DynamicHookDef> {
        let state = self.state.lock().expect("state Mutex poisoned");
        state.hooks.get(id).cloned()
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Build a [`RegistrySnapshot`] from current state.
    fn build_snapshot(
        static_tools: &[Arc<dyn Tool>],
        state: &DynamicRegistryState,
    ) -> RegistrySnapshot {
        // Collect static tools in insertion order.
        let mut all_tools: Vec<Arc<dyn Tool>> = static_tools.to_vec();

        // Collect enabled dynamic tools sorted by name.
        let mut dynamic_entries: Vec<(&String, &Arc<dyn Tool>)> = state
            .tools
            .iter()
            .filter(|(_, def)| def.enabled)
            .filter_map(|(id, _)| state.tool_instances.get(id).map(|inst| (id, inst)))
            .collect();
        // Sort by tool name (from the def, not the id).
        dynamic_entries.sort_by(|(id_a, _), (id_b, _)| {
            let name_a = state
                .tools
                .get(*id_a)
                .map(|d| d.name.as_str())
                .unwrap_or("");
            let name_b = state
                .tools
                .get(*id_b)
                .map(|d| d.name.as_str())
                .unwrap_or("");
            name_a.cmp(name_b)
        });
        for (_, instance) in dynamic_entries {
            all_tools.push(instance.clone());
        }

        // Precompute tool specs.
        let tool_specs: Vec<ToolSpec> = all_tools.iter().map(|t| t.spec()).collect();

        // Collect enabled dynamic hooks sorted by (priority desc, id asc).
        let mut hook_entries: Vec<(&String, &Arc<DynamicHookInstance>)> = state
            .hooks
            .iter()
            .filter(|(_, def)| def.enabled)
            .filter_map(|(id, _)| state.hook_instances.get(id).map(|inst| (id, inst)))
            .collect();
        hook_entries.sort_by(|(id_a, inst_a), (id_b, inst_b)| {
            // Priority descending.
            let pri_cmp = inst_b.priority().cmp(&inst_a.priority());
            if pri_cmp != std::cmp::Ordering::Equal {
                return pri_cmp;
            }
            // ID ascending for stable tie-breaking.
            id_a.cmp(id_b)
        });
        let dynamic_hooks: Vec<Arc<dyn HookHandler>> = hook_entries
            .into_iter()
            .map(|(_, inst)| inst.clone() as Arc<dyn HookHandler>)
            .collect();

        RegistrySnapshot {
            state_revision: state.state_revision,
            tool_revision: state.tool_revision,
            hook_revision: state.hook_revision,
            all_tools: Arc::new(all_tools),
            tool_specs: Arc::new(tool_specs),
            dynamic_hooks: Arc::new(dynamic_hooks),
        }
    }

    /// Persist current state to disk. Logs warnings on failure (never panics).
    fn persist(&self, state: &DynamicRegistryState) {
        let persisted = PersistedRegistry {
            schema_version: 1,
            tools: state.tools.values().cloned().collect(),
            hooks: state.hooks.values().cloned().collect(),
        };
        if let Err(e) = persisted.save_to_file(&self.persistence_path) {
            tracing::warn!(
                path = %self.persistence_path.display(),
                error = %e,
                "failed to persist dynamic registry"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// StubDynamicTool — minimal Tool impl for test registries without build_ctx
// ---------------------------------------------------------------------------

/// A no-op tool used internally when `DynamicRegistry::new_empty()` creates
/// tools without a real `ToolBuildContext`.
struct StubDynamicTool {
    name: String,
    description: String,
}

#[async_trait]
impl Tool for StubDynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
    ) -> anyhow::Result<crate::tools::traits::ToolResult> {
        Ok(crate::tools::traits::ToolResult {
            success: true,
            output: "stub".into(),
            error: None,
        })
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
        let io_err = std::io::Error::other("disk full");
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
    fn pre_tool_call_hook(effect: HookEffect, filter: Option<HookFilter>) -> DynamicHookDef {
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
        let result = hook.before_tool_call("shell".into(), original_args).await;

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

        let result = hook
            .before_prompt_build("You are a helpful agent.".into())
            .await;

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
        let mut def = pre_tool_call_hook(HookEffect::Cancel("should not fire".into()), None);
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
        let def = pre_tool_call_hook(HookEffect::Cancel("should not fire".into()), None);
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

    // ==================================================================
    // DynamicRegistry tests
    // ==================================================================

    /// Helper: create a DynamicRegistry backed by a temp dir with a persistence path.
    fn make_test_registry(dir: &TempDir) -> DynamicRegistry {
        let path = dir.path().join("registry.json");
        let mut reg = DynamicRegistry::new_empty(DynamicRegistryConfig::default());
        reg.persistence_path = path;
        reg
    }

    /// Helper: create a DynamicRegistry with one static tool.
    fn make_test_registry_with_static(dir: &TempDir) -> DynamicRegistry {
        let static_tool: Arc<dyn Tool> = Arc::new(StubDynamicTool {
            name: "static_shell".into(),
            description: "A static shell tool".into(),
        });
        let path = dir.path().join("registry.json");
        let config = DynamicRegistryConfig::default();
        let factories = default_factory_registry();
        let static_tool_names: HashSet<String> =
            vec!["static_shell".to_string()].into_iter().collect();

        let internal_state = DynamicRegistryState {
            tools: BTreeMap::new(),
            hooks: BTreeMap::new(),
            tool_instances: HashMap::new(),
            hook_instances: HashMap::new(),
            state_revision: 0,
            tool_revision: 0,
            hook_revision: 0,
        };

        let static_tools = vec![static_tool];
        let snapshot = DynamicRegistry::build_snapshot(&static_tools, &internal_state);

        DynamicRegistry {
            static_tools,
            static_tool_names,
            snapshot: RwLock::new(Arc::new(snapshot)),
            state: Mutex::new(internal_state),
            persistence_path: path,
            factories,
            config,
            build_ctx: None,
        }
    }

    /// Helper: build a shell_command tool def for DynamicRegistry tests.
    fn registry_tool_def(id: &str, name: &str) -> DynamicToolDef {
        DynamicToolDef {
            id: id.into(),
            name: name.into(),
            description: format!("Dynamic tool {name}"),
            kind: "shell_command".into(),
            config: serde_json::json!({
                "command": "echo",
                "args": [{"Fixed": "hello"}]
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: Some("zeroclaw_user".into()),
        }
    }

    /// Helper: build a Pre/ToolCall Cancel hook def for DynamicRegistry tests.
    fn registry_hook_def(id: &str, name: &str, priority: i32) -> DynamicHookDef {
        DynamicHookDef {
            id: id.into(),
            name: name.into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority,
            enabled: true,
            filter: None,
            effect: HookEffect::Cancel("blocked".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ------------------------------------------------------------------
    // R1. registry_add_tool_increments_revision
    // ------------------------------------------------------------------
    #[test]
    fn registry_add_tool_increments_revision() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        assert_eq!(reg.tool_revision(), 0);

        let rev = reg
            .add_tool(registry_tool_def("t1", "tool_alpha"), None)
            .unwrap();
        assert_eq!(rev, 1);
        assert_eq!(reg.tool_revision(), 1);

        let snap = reg.snapshot();
        assert_eq!(snap.state_revision, 1);
        assert_eq!(snap.tool_revision, 1);
    }

    // ------------------------------------------------------------------
    // R2. registry_add_hook_does_not_increment_tool_revision
    // ------------------------------------------------------------------
    #[test]
    fn registry_add_hook_does_not_increment_tool_revision() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        let rev = reg
            .add_hook(registry_hook_def("h1", "hook_a", 10), None)
            .unwrap();
        assert_eq!(rev, 1);

        let snap = reg.snapshot();
        assert_eq!(snap.state_revision, 1);
        assert_eq!(snap.hook_revision, 1);
        // tool_revision should NOT have changed.
        assert_eq!(snap.tool_revision, 0);
    }

    // ------------------------------------------------------------------
    // R3. registry_name_collision_with_static_rejected
    // ------------------------------------------------------------------
    #[test]
    fn registry_name_collision_with_static_rejected() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry_with_static(&dir);

        // Try to add a dynamic tool with the same name as the static tool.
        let result = reg.add_tool(registry_tool_def("t1", "static_shell"), None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DynamicRegistryError::NameCollision(_)),
            "expected NameCollision, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // R4. registry_name_collision_with_dynamic_rejected
    // ------------------------------------------------------------------
    #[test]
    fn registry_name_collision_with_dynamic_rejected() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        reg.add_tool(registry_tool_def("t1", "tool_alpha"), None)
            .unwrap();

        // Try to add another tool with the same name but different ID.
        let result = reg.add_tool(registry_tool_def("t2", "tool_alpha"), None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DynamicRegistryError::NameCollision(_)),
            "expected NameCollision, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // R5. registry_quota_enforced
    // ------------------------------------------------------------------
    #[test]
    fn registry_quota_enforced() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.json");
        let config = DynamicRegistryConfig {
            max_tools: 2,
            max_hooks: 20,
            allowed_tool_kinds: vec!["shell_command".into(), "http_endpoint".into()],
        };
        let mut reg = DynamicRegistry::new_empty(config);
        reg.persistence_path = path;

        reg.add_tool(registry_tool_def("t1", "tool_a"), None)
            .unwrap();
        reg.add_tool(registry_tool_def("t2", "tool_b"), None)
            .unwrap();

        // Third tool should fail quota.
        let result = reg.add_tool(registry_tool_def("t3", "tool_c"), None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DynamicRegistryError::QuotaExceeded { .. }),
            "expected QuotaExceeded, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // R6. registry_optimistic_concurrency_conflict
    // ------------------------------------------------------------------
    #[test]
    fn registry_optimistic_concurrency_conflict() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        // Advance revision to 1.
        reg.add_tool(registry_tool_def("t1", "tool_a"), None)
            .unwrap();

        // Try to add with expected_revision=0 (stale).
        let result = reg.add_tool(registry_tool_def("t2", "tool_b"), Some(0));
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            DynamicRegistryError::RevisionConflict { expected, actual } => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1);
            }
            other => panic!("expected RevisionConflict, got: {other}"),
        }
    }

    // ------------------------------------------------------------------
    // R7. registry_optimistic_concurrency_passes_when_matching
    // ------------------------------------------------------------------
    #[test]
    fn registry_optimistic_concurrency_passes_when_matching() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        let rev = reg
            .add_tool(registry_tool_def("t1", "tool_a"), None)
            .unwrap();
        assert_eq!(rev, 1);

        // Now add with expected_revision=1 (current).
        let rev2 = reg
            .add_tool(registry_tool_def("t2", "tool_b"), Some(1))
            .unwrap();
        assert_eq!(rev2, 2);
    }

    // ------------------------------------------------------------------
    // R8. registry_snapshot_shows_tools_in_order
    // ------------------------------------------------------------------
    #[test]
    fn registry_snapshot_shows_tools_in_order() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry_with_static(&dir);

        // Add dynamic tools in reverse alphabetical order.
        reg.add_tool(registry_tool_def("t2", "zeta_tool"), None)
            .unwrap();
        reg.add_tool(registry_tool_def("t1", "alpha_tool"), None)
            .unwrap();

        let snap = reg.snapshot();
        let names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();

        // Static tool first, then dynamic sorted by name.
        assert_eq!(names, vec!["static_shell", "alpha_tool", "zeta_tool"]);

        // tool_specs should match.
        let spec_names: Vec<&str> = snap.tool_specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(spec_names, vec!["static_shell", "alpha_tool", "zeta_tool"]);
    }

    // ------------------------------------------------------------------
    // R9. registry_remove_tool_removes_from_snapshot
    // ------------------------------------------------------------------
    #[test]
    fn registry_remove_tool_removes_from_snapshot() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        reg.add_tool(registry_tool_def("t1", "tool_a"), None)
            .unwrap();
        reg.add_tool(registry_tool_def("t2", "tool_b"), None)
            .unwrap();

        assert_eq!(reg.snapshot().all_tools.len(), 2);

        reg.remove_tool("t1").unwrap();

        let snap = reg.snapshot();
        assert_eq!(snap.all_tools.len(), 1);
        assert_eq!(snap.all_tools[0].name(), "tool_b");
    }

    // ------------------------------------------------------------------
    // R10. registry_enable_disable_tool
    // ------------------------------------------------------------------
    #[test]
    fn registry_enable_disable_tool() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        reg.add_tool(registry_tool_def("t1", "tool_a"), None)
            .unwrap();
        assert_eq!(reg.snapshot().all_tools.len(), 1);

        // Disable.
        reg.enable_tool("t1", false).unwrap();
        assert_eq!(
            reg.snapshot().all_tools.len(),
            0,
            "disabled tool should not appear in snapshot"
        );

        // The def should still be listed.
        assert_eq!(reg.list_tools().len(), 1);
        assert!(!reg.get_tool("t1").unwrap().enabled);

        // Re-enable.
        reg.enable_tool("t1", true).unwrap();
        assert_eq!(reg.snapshot().all_tools.len(), 1);
        assert!(reg.get_tool("t1").unwrap().enabled);
    }

    // ------------------------------------------------------------------
    // R11. registry_add_list_get_hook
    // ------------------------------------------------------------------
    #[test]
    fn registry_add_list_get_hook() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        reg.add_hook(registry_hook_def("h1", "hook_a", 10), None)
            .unwrap();
        reg.add_hook(registry_hook_def("h2", "hook_b", 5), None)
            .unwrap();

        let hooks = reg.list_hooks();
        assert_eq!(hooks.len(), 2);

        let h1 = reg.get_hook("h1").unwrap();
        assert_eq!(h1.name, "hook_a");
        assert_eq!(h1.priority, 10);

        let h2 = reg.get_hook("h2").unwrap();
        assert_eq!(h2.name, "hook_b");
        assert_eq!(h2.priority, 5);

        // Snapshot hooks should be sorted by priority desc, then id asc.
        let snap = reg.snapshot();
        assert_eq!(snap.dynamic_hooks.len(), 2);
        assert_eq!(snap.dynamic_hooks[0].name(), "hook_a"); // priority 10
        assert_eq!(snap.dynamic_hooks[1].name(), "hook_b"); // priority 5
    }

    // ------------------------------------------------------------------
    // R12. registry_persistence_round_trip
    // ------------------------------------------------------------------
    #[test]
    fn registry_persistence_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.json");

        // Create first registry, add tools and hooks.
        {
            let config = DynamicRegistryConfig::default();
            let mut reg = DynamicRegistry::new_empty(config);
            reg.persistence_path = path.clone();

            reg.add_tool(registry_tool_def("t1", "tool_a"), None)
                .unwrap();
            reg.add_hook(registry_hook_def("h1", "hook_a", 10), None)
                .unwrap();
        }

        // Verify file was written.
        assert!(path.exists(), "persistence file should exist");

        // Load into a new registry using PersistedRegistry directly
        // (since new_empty doesn't load from disk, and `new()` needs ToolBuildContext).
        let loaded = PersistedRegistry::load_from_file(&path).unwrap();
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools[0].name, "tool_a");
        assert_eq!(loaded.tools[0].id, "t1");
        assert_eq!(loaded.hooks.len(), 1);
        assert_eq!(loaded.hooks[0].name, "hook_a");
        assert_eq!(loaded.hooks[0].id, "h1");
    }

    // ------------------------------------------------------------------
    // R13. registry_unknown_kind_rejected
    // ------------------------------------------------------------------
    #[test]
    fn registry_unknown_kind_rejected() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        let mut def = registry_tool_def("t1", "tool_a");
        def.kind = "magic_wand".into();

        let result = reg.add_tool(def, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, DynamicRegistryError::UnknownKind(_)),
            "expected UnknownKind, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // R14. registry_disabled_tool_not_in_snapshot
    // ------------------------------------------------------------------
    #[test]
    fn registry_disabled_tool_not_in_snapshot() {
        let dir = TempDir::new().unwrap();
        let reg = make_test_registry(&dir);

        let mut def = registry_tool_def("t1", "tool_a");
        def.enabled = false;
        reg.add_tool(def, None).unwrap();

        let snap = reg.snapshot();
        assert_eq!(
            snap.all_tools.len(),
            0,
            "disabled tool should not appear in snapshot"
        );
        assert_eq!(snap.tool_specs.len(), 0);

        // But the def is still listed.
        assert_eq!(reg.list_tools().len(), 1);
        assert!(!reg.get_tool("t1").unwrap().enabled);
    }
}
