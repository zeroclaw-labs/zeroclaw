//! Shared request/response types for the ZeroClaw RPC + gateway API surface.
//!
//! **Single source of truth.** Every domain's wire types live here.
//! The RPC dispatcher, the HTTP gateway, and the TUI client all
//! import from this module. No ad-hoc `json!()`, no duplicated structs.
//!
//! ## Conventions
//!
//! - All structs derive `Debug, Clone, Serialize, Deserialize`.
//! - All structs use `#[serde(rename_all = "snake_case")]`.
//! - Optional fields use `#[serde(default, skip_serializing_if = "Option::is_none")]`.
//! - Types that already exist elsewhere (`MemoryEntry`, `CronJob`,
//!   `CostSummary`, `SkillFrontmatter`) are re-exported, not re-defined.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Re-exports: types that already derive Serialize + Deserialize ────
// Consumers can `use zeroclaw_runtime::rpc::types::*` and get everything.

pub use crate::cron::{CronJob, CronJobPatch, CronRun, DeliveryConfig, Schedule};
pub use crate::doctor::{DiagResult, Severity as DoctorSeverity};
pub use crate::rpc::session::SessionOverrides;
pub use crate::skills::frontmatter::SkillFrontmatter;
pub use zeroclaw_api::memory_traits::{MemoryCategory, MemoryEntry};
pub use zeroclaw_config::cost::types::CostSummary;
pub use zeroclaw_config::traits::{ConfigFieldEntry, PropKind};

// ── Derive helper ────────────────────────────────────────────────────

macro_rules! rpc_type {
    (
        $(#[$meta:meta])*
        pub struct $name:ident { $($body:tt)* }
    ) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        $(#[$meta])*
        pub struct $name { $($body)* }
    };
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($body:tt)* }
    ) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        $(#[$meta])*
        pub enum $name { $($body)* }
    };
}

// ══════════════════════════════════════════════════════════════════════
// ── Core ─────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct InitializeParams {
        #[serde(default = "default_protocol_version")]
        pub protocol_version: u64,
        /// TUI ID from a previous connection (reconnection).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tui_id: Option<String>,
        /// HMAC signature proving ownership of the claimed TUI ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tui_sig: Option<String>,
        /// Shell environment from the TUI process, used to forward the user's
        /// real env (PATH, credentials, etc.) to subprocesses spawned by the
        /// daemon on their behalf. Omitted by older clients; defaults to empty.
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        pub env: std::collections::HashMap<String, String>,
        /// Optional client-side capabilities the TUI advertises during the
        /// handshake. Today the only inspected sub-key is `elicitation`,
        /// parsed by `zeroclaw_api::elicitation::ElicitationCapabilities`
        /// so the per-session `RpcApprovalChannel` can speak the ACP
        /// `elicitation/create` RFD when the TUI signals support. The field
        /// is a JSON pass-through so future capabilities can be added
        /// without bumping the wire schema.
        ///
        /// Sourcing is camelCase to match the ACP convention used by the
        /// elicitation RFD (`clientCapabilities.elicitation`); the runtime
        /// dispatcher is the canonical owner of the parsed value for the
        /// lifetime of the connection. Source of truth: the `initialize`
        /// payload itself — the dispatcher caches the *parsed* form but
        /// never copies the raw JSON.
        #[serde(
            default,
            rename = "clientCapabilities",
            skip_serializing_if = "Option::is_none"
        )]
        pub client_capabilities: Option<serde_json::Value>,
    }
}

fn default_protocol_version() -> u64 {
    1
}

rpc_type! {
    pub struct InitializeResult {
        pub protocol_version: u64,
        pub server_version: String,
        /// Assigned TUI session UID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tui_id: Option<String>,
        /// HMAC signature for reconnection. Pass back in next initialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tui_sig: Option<String>,
        /// Supported RPC method names (e.g. "session/prompt", "memory/list").
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub capabilities: Vec<String>,
    }
}

rpc_type! {
    pub struct StatusResult {
        pub server_version: String,
        pub protocol_version: u64,
        pub active_sessions: usize,
        pub session_ids: Vec<String>,
    }
}

// Health: no params, result is `Value` from `health::snapshot_json()`.

rpc_type! {
    pub struct DoctorSummary {
        pub ok: usize,
        pub warnings: usize,
        pub errors: usize,
    }
}

rpc_type! {
    pub struct DoctorRunResult {
        pub results: Vec<DiagResult>,
        pub summary: DoctorSummary,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── TUI ──────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct TuiListEntry {
        pub tui_id: String,
        /// RFC 3339 timestamp (for gateway API / web frontend).
        pub connected_at: String,
        /// Unix epoch seconds (for TUI client relative-time display
        /// without requiring chrono).
        pub connected_at_unix: i64,
        pub peer_label: String,
        /// Transport protocol: `"unix"` or `"wss"`.
        pub transport: String,
    }
}

rpc_type! {
    pub struct TuiListResult {
        pub tuis: Vec<TuiListEntry>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Sessions ─────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    /// Shared param for methods that only need a session ID:
    /// `session/close`, `session/cancel`, `session/messages`,
    /// `session/state`, `session/delete`.
    pub struct SessionIdParams {
        pub session_id: String,
    }
}

rpc_type! {
    pub struct SessionNewParams {
        pub agent_alias: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tui_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub exclude_memory: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub chat_mode: Option<ChatMode>,
    }
}

rpc_type! {
    #[derive(PartialEq, Eq)]
    pub enum ChatMode {
        Chat,
        Acp,
    }
}

rpc_type! {
    pub struct SessionNewResult {
        pub session_id: String,
        pub agent_alias: String,
        pub message_count: usize,
        pub workspace_dir: String,
    }
}

rpc_type! {
    pub struct SessionCloseResult {
        pub session_id: String,
        pub closed: bool,
    }
}

rpc_type! {
    pub struct SessionKillParams {
        pub session_id: String,
    }
}

rpc_type! {
    pub struct SessionKillResult {
        pub session_id: String,
        pub killed: bool,
    }
}

rpc_type! {
    pub struct SessionPromptParams {
        pub session_id: String,
        pub prompt: String,
        /// Inline file attachments. Processed identically to `file/attach`
        /// entries — markers are appended to the prompt before the turn runs.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub attachments: Vec<FileEntry>,
    }
}

rpc_type! {
    pub struct SessionPromptResult {
        pub session_id: String,
        pub stop_reason: String,
        pub content: String,
    }
}

rpc_type! {
    pub struct SessionConfigureParams {
        pub session_id: String,
        #[serde(default)]
        pub overrides: SessionOverrides,
    }
}

rpc_type! {
    pub struct SessionConfigureResult {
        pub session_id: String,
        pub overrides: SessionOverrides,
    }
}

rpc_type! {
    pub struct SessionCancelResult {
        pub session_id: String,
        pub cancelled: bool,
    }
}

rpc_type! {
    pub struct SessionGitBranchResult {
        pub session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub hash: Option<String>,
    }
}

rpc_type! {
    pub struct SessionListParams {
        /// Full-text search query. When present, only sessions whose message
        /// content matches (via FTS5) are returned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<usize>,
    }
}

rpc_type! {
    pub struct SessionListResult {
        pub sessions: Vec<SessionEntry>,
    }
}

rpc_type! {
    pub struct SessionEntry {
        pub session_id: String,
        pub session_key: String,
        pub created_at: String,
        pub last_activity: String,
        pub message_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent_alias: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub channel_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
    }
}

rpc_type! {
    pub struct SessionMessagesResult {
        pub session_id: String,
        pub messages: Vec<MessageEntry>,
        /// Total messages persisted for this session. Lets the TUI
        /// know how many pages remain before it reaches the head.
        #[serde(default)]
        pub total: usize,
        /// Index of the first message in `messages` relative to the
        /// full persisted history. Pair with `total` to compute
        /// "page N of M" / "load older" affordances.
        #[serde(default)]
        pub start: usize,
    }
}

rpc_type! {
    /// Params for `session/messages`. `limit` + `before_index`
    /// page-window the load so a long session doesn't slurp every
    /// message into client memory at once. Both default to the
    /// legacy "load everything" behaviour for callers that pre-date
    /// the pagination change.
    pub struct SessionMessagesParams {
        pub session_id: String,
        #[serde(default)]
        pub limit: Option<usize>,
        #[serde(default)]
        pub before_index: Option<usize>,
    }
}

rpc_type! {
    pub struct MessageEntry {
        pub role: String,
        pub content: String,
    }
}

rpc_type! {
    pub struct SessionStateResult {
        pub session_id: String,
        pub state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub turn_started_at: Option<String>,
    }
}

rpc_type! {
    pub struct SessionDeleteResult {
        pub session_id: String,
        pub deleted: bool,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Memory ───────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    /// Params for `memory/list`. Consolidates gateway `MemoryQuery` (list mode).
    pub struct MemoryListParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
    }
}

rpc_type! {
    pub struct MemoryListResult {
        pub entries: Vec<MemoryEntry>,
        pub count: usize,
    }
}

rpc_type! {
    /// Params for `memory/search`. Consolidates gateway `MemoryQuery` (search mode).
    pub struct MemorySearchParams {
        pub query: String,
        #[serde(default = "default_search_limit")]
        pub limit: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub since: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub until: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
    }
}

fn default_search_limit() -> usize {
    10
}

rpc_type! {
    pub struct MemorySearchResult {
        pub entries: Vec<MemoryEntry>,
        pub count: usize,
    }
}

rpc_type! {
    /// `memory/get` params — fetch one entry's full content by key.
    pub struct MemoryGetParams {
        pub key: String,
    }
}

rpc_type! {
    /// `memory/get` result. `entry` carries the full content
    /// the Memory pane only renders inside the detail modal —
    /// list rows store preview-only data.
    pub struct MemoryGetResult {
        pub entry: Option<MemoryEntry>,
    }
}

rpc_type! {
    /// Params for `memory/store`. Consolidates gateway `MemoryStoreBody`.
    pub struct MemoryStoreParams {
        pub key: String,
        pub content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub category: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
    }
}

rpc_type! {
    pub struct MemoryStoreResult {
        pub key: String,
        pub stored: bool,
    }
}

rpc_type! {
    /// Params for `memory/delete`. Consolidates gateway `MemoryDeleteQuery`.
    pub struct MemoryDeleteParams {
        pub key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
    }
}

rpc_type! {
    pub struct MemoryDeleteResult {
        pub key: String,
        pub deleted: bool,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Cron ─────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct CronListResult {
        pub jobs: Vec<CronJob>,
    }
}

rpc_type! {
    pub struct CronIdParams {
        pub id: String,
    }
}

rpc_type! {
    /// Params for `cron/add`. Consolidates gateway `CronAddBody`.
    pub struct CronAddParams {
        pub agent: String,
        pub schedule: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tz: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub job_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delivery: Option<DeliveryConfig>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub allowed_tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delete_after_run: Option<bool>,
    }
}

rpc_type! {
    /// Params for `cron/patch`. Consolidates gateway `CronPatchBody`.
    pub struct CronPatchParams {
        pub id: String,
        pub agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub schedule: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tz: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub clear_tz: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prompt: Option<String>,
    }
}

rpc_type! {
    pub struct CronDeleteResult {
        pub id: String,
        pub deleted: bool,
    }
}

rpc_type! {
    pub struct CronRunsParams {
        pub id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<u32>,
    }
}

rpc_type! {
    pub struct CronRunsResult {
        pub runs: Vec<CronRun>,
    }
}

rpc_type! {
    pub struct CronTriggerResult {
        pub id: String,
        pub success: bool,
        pub status: String,
        pub output: String,
        pub duration_ms: i64,
        pub started_at: String,
        pub finished_at: String,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Config ───────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct ConfigGetParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prop: Option<String>,
    }
}

rpc_type! {
    /// Returned when `config/get` is called with a specific `prop`.
    pub struct ConfigGetPropResult {
        pub prop: String,
        pub value: String,
    }
}

// Full config read returns `Value` (masked) — inherently untyped.

rpc_type! {
    /// Value is polymorphic: a JSON string passes through as-is (backward
    /// compat); any other JSON type is coerced via `coerce_for_set_prop`.
    pub struct ConfigSetParams {
        pub prop: String,
        pub value: Value,
    }
}

rpc_type! {
    pub struct ConfigSetResult {
        pub prop: String,
        pub set: bool,
    }
}

rpc_type! {
    pub struct ConfigValidateResult {
        pub valid: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
    }
}

rpc_type! {
    pub struct ConfigReloadResult {
        pub reloading: bool,
    }
}

rpc_type! {
    pub struct ConfigListParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prefix: Option<String>,
    }
}

rpc_type! {
    pub struct ConfigListResult {
        pub entries: Vec<ConfigFieldEntry>,
    }
}

rpc_type! {
    pub struct ConfigDeleteParams {
        pub prop: String,
    }
}

rpc_type! {
    pub struct ConfigDeleteResult {
        pub prop: String,
        pub deleted: bool,
    }
}

rpc_type! {
    pub struct ConfigMapKeysParams {
        pub path: String,
    }
}

rpc_type! {
    pub struct ConfigMapKeysResult {
        pub path: String,
        pub keys: Vec<String>,
    }
}

rpc_type! {
    pub struct ConfigResolveAliasSourceParams {
        pub source: zeroclaw_config::traits::AliasSource,
    }
}

rpc_type! {
    pub struct ConfigResolveAliasSourceResult {
        pub source: zeroclaw_config::traits::AliasSource,
        pub values: Vec<String>,
    }
}

rpc_type! {
    pub struct ConfigMapKeyCreateParams {
        pub path: String,
        pub key: String,
    }
}

rpc_type! {
    pub struct ConfigMapKeyCreateResult {
        pub path: String,
        pub key: String,
        pub created: bool,
    }
}

rpc_type! {
    pub struct ConfigMapKeyDeleteParams {
        pub path: String,
        pub key: String,
    }
}

rpc_type! {
    pub struct ConfigMapKeyDeleteResult {
        pub path: String,
        pub key: String,
        pub deleted: bool,
    }
}

rpc_type! {
    pub struct ConfigMapKeyRenameParams {
        pub path: String,
        pub from: String,
        pub to: String,
    }
}

rpc_type! {
    pub struct ConfigMapKeyRenameResult {
        pub path: String,
        pub from: String,
        pub to: String,
        pub renamed: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub warnings: Vec<String>,
    }
}

rpc_type! {
    /// Owned wire representation of a [`zeroclaw_config::traits::MapKeySection`].
    /// The upstream type uses `&'static str` fields that can't round-trip
    /// through `Deserialize`, so this owned copy serves as the wire format.
    pub struct ConfigTemplateEntry {
        pub path: String,
        pub kind: zeroclaw_config::traits::MapKeyKind,
        pub value_type: String,
        pub description: String,
    }
}

impl From<zeroclaw_config::traits::MapKeySection> for ConfigTemplateEntry {
    fn from(s: zeroclaw_config::traits::MapKeySection) -> Self {
        Self {
            path: s.path.to_string(),
            kind: s.kind,
            value_type: s.value_type.to_string(),
            description: s.description.to_string(),
        }
    }
}

rpc_type! {
    pub struct ConfigTemplatesResult {
        pub templates: Vec<ConfigTemplateEntry>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Agents ───────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct AgentEntry {
        pub alias: String,
        pub enabled: bool,
        pub channels: Vec<String>,
    }
}

rpc_type! {
    pub struct AgentsListResult {
        pub agents: Vec<AgentEntry>,
    }
}

rpc_type! {
    pub struct AgentStatusEntry {
        pub alias: String,
        pub enabled: bool,
        #[serde(default)]
        pub live_sessions: usize,
        #[serde(default)]
        pub persisted_sessions: usize,
        #[serde(default)]
        pub channels: Vec<String>,
    }
}

rpc_type! {
    pub struct AgentsStatusResult {
        pub agents: Vec<AgentStatusEntry>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Cost ─────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    /// Params for `cost/query`. Consolidates gateway `CostQuery`.
    pub struct CostQueryParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub to: Option<String>,
    }
}

// Result is `CostSummary` directly (already Serialize).

// ══════════════════════════════════════════════════════════════════════
// ── Skills ───────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    /// Wire representation of a skill bundle. Consolidates gateway `BundleEntry`.
    pub struct SkillBundleEntry {
        pub alias: String,
        pub directory: String,
        pub include: Vec<String>,
        pub exclude: Vec<String>,
    }
}

rpc_type! {
    pub struct SkillsBundlesResult {
        pub bundles: Vec<SkillBundleEntry>,
    }
}

rpc_type! {
    pub struct SkillsListParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub bundle: Option<String>,
    }
}

rpc_type! {
    /// Wire representation of a skill in a list. Consolidates gateway `SkillEntry`.
    pub struct SkillListEntry {
        pub bundle: String,
        pub name: String,
        pub directory: String,
        pub frontmatter: SkillFrontmatter,
    }
}

rpc_type! {
    pub struct SkillsListResult {
        pub skills: Vec<SkillListEntry>,
    }
}

rpc_type! {
    /// One skill in an agent's *effective* set (the runtime's four-source
    /// union), with provenance — for `GET /api/agents/{alias}/skills` (#7757).
    /// Distinct from [`SkillListEntry`] (bundle-editor wire type); the two must
    /// not be conflated. `origin` is the discriminant; `plugin`/`bundle` carry
    /// the source detail; `editable` is `true` only for `origin == "bundle"`.
    pub struct AgentSkillEntry {
        pub name: String,
        pub description: String,
        /// `"workspace"` | `"open-skills"` | `"plugin"` | `"bundle"`.
        pub origin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub plugin: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub bundle: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub directory: Option<String>,
        pub editable: bool,
        /// Lower-precedence same-name skills this one shadows. Empty normally;
        /// additive so old clients ignore it. (#7963)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub shadowed: Vec<ShadowedSkillEntry>,
    }
}

rpc_type! {
    /// A lower-precedence same-name skill shadowed by a winning skill (#7963).
    pub struct ShadowedSkillEntry {
        pub name: String,
        /// `"workspace"` | `"open-skills"` | `"plugin"` | `"bundle"`.
        pub origin: String,
    }
}

rpc_type! {
    /// A candidate skill the audited resolver dropped (security audit failed,
    /// unauditable, or manifest parse error) (#7963).
    pub struct DroppedSkillEntry {
        pub name: String,
        pub origin: String,
        /// `"audit_findings"` | `"audit_error"` | `"manifest_parse_error"`.
        pub reason_kind: String,
        /// Human-readable detail (the audit summary / error text).
        pub reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub directory: Option<String>,
    }
}

rpc_type! {
    pub struct AgentSkillsResult {
        pub agent: String,
        pub skills: Vec<AgentSkillEntry>,
        /// Audit-dropped candidates the resolver skipped. Empty normally;
        /// additive so old clients ignore it. (#7963)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub dropped: Vec<DroppedSkillEntry>,
    }
}

rpc_type! {
    pub struct SkillsReadParams {
        pub bundle: String,
        pub name: String,
    }
}

rpc_type! {
    /// Consolidates gateway `SkillReadResponse`.
    pub struct SkillsReadResult {
        pub bundle: String,
        pub name: String,
        pub frontmatter: SkillFrontmatter,
        pub body: String,
    }
}

rpc_type! {
    pub struct SkillsWriteParams {
        pub bundle: String,
        pub name: String,
        pub frontmatter: SkillFrontmatter,
        #[serde(default)]
        pub body: String,
    }
}

rpc_type! {
    pub struct SkillsWriteResult {
        pub bundle: String,
        pub name: String,
        pub written: bool,
    }
}

rpc_type! {
    pub struct SkillsDeleteParams {
        pub bundle: String,
        pub name: String,
    }
}

rpc_type! {
    pub struct SkillsDeleteResult {
        pub bundle: String,
        pub name: String,
        pub deleted: bool,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Personality ──────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct PersonalityListParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
    }
}

rpc_type! {
    /// Consolidates gateway `PersonalityIndexEntry`.
    pub struct PersonalityFileEntry {
        pub filename: String,
        pub exists: bool,
        #[serde(default)]
        pub size: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mtime_ms: Option<i64>,
    }
}

rpc_type! {
    /// Consolidates gateway `PersonalityIndex`.
    pub struct PersonalityListResult {
        pub files: Vec<PersonalityFileEntry>,
        pub max_chars: usize,
    }
}

rpc_type! {
    pub struct PersonalityGetParams {
        pub agent: String,
        pub filename: String,
    }
}

rpc_type! {
    /// Consolidates gateway `PersonalityFileResponse`.
    pub struct PersonalityGetResult {
        pub filename: String,
        #[serde(default)]
        pub content: Option<String>,
        pub exists: bool,
        #[serde(default)]
        pub truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mtime_ms: Option<i64>,
    }
}

rpc_type! {
    pub struct PersonalityPutParams {
        pub agent: String,
        pub filename: String,
        pub content: String,
    }
}

rpc_type! {
    /// Consolidates gateway `PersonalityPutResponse`.
    pub struct PersonalityPutResult {
        pub bytes_written: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mtime_ms: Option<i64>,
    }
}

rpc_type! {
    pub struct PersonalityTemplatesParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
    }
}

rpc_type! {
    /// Consolidates gateway `TemplateFile`.
    pub struct TemplateFileEntry {
        pub filename: String,
        pub content: String,
    }
}

rpc_type! {
    /// Consolidates gateway `TemplateResponse`.
    pub struct PersonalityTemplatesResult {
        pub preset: String,
        pub files: Vec<TemplateFileEntry>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Config introspection (sections, catalog, status) ─────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    /// Consolidates gateway `CatalogModelProvider`.
    pub struct CatalogModelProvider {
        pub name: String,
        pub display_name: String,
        pub local: bool,
    }
}

rpc_type! {
    /// Consolidates gateway `CatalogResponse`.
    pub struct CatalogResponse {
        pub model_providers: Vec<CatalogModelProvider>,
    }
}

rpc_type! {
    pub struct CatalogModelsParams {
        /// Accepts `model_provider` or aliased `provider` (gateway compat).
        #[serde(alias = "provider")]
        pub model_provider: String,
    }
}

rpc_type! {
    /// Consolidates gateway `ModelsResponse`.
    pub struct CatalogModelsResult {
        pub model_provider: String,
        pub models: Vec<String>,
        /// Optional pricing data keyed by model id. Populated when the
        /// provider's `/models` endpoint returns pricing (Kilo Gateway,
        /// OpenRouter, etc.). Absent for catalog fallbacks without pricing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pricing: Option<std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing>>,
        pub local: bool,
        pub live: bool,
    }
}

rpc_type! {
    /// A config section entry for the dashboard sidebar / TUI section list.
    pub struct ConfigSectionEntry {
        pub key: String,
        pub label: String,
        pub help: String,
        pub has_picker: bool,
        pub completed: bool,
        /// Whether the section currently has enough usable config for the
        /// first-run path.
        #[serde(default)]
        pub ready: bool,
        /// Display group for the dashboard sidebar.
        #[serde(default)]
        pub group: String,
        /// `true` when this section is part of the canonical Quickstart list.
        #[serde(default)]
        pub is_quickstart: bool,
        /// Editor shape (direct form / one-tier alias map / typed-family map /
        /// backend picker).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub shape: Option<zeroclaw_config::sections::SectionShape>,
        #[serde(default)]
        pub cost_category: String,
    }
}

rpc_type! {
    /// Response for `config/sections`.
    pub struct ConfigSectionsResult {
        pub sections: Vec<ConfigSectionEntry>,
    }
}

rpc_type! {
    /// Config readiness status for the dashboard/TUI.
    pub struct ConfigStatusResult {
        pub needs_quickstart: bool,
        pub reason: String,
        pub has_partial_state: bool,
        pub missing: Vec<String>,
    }
}

rpc_type! {
    /// Consolidates gateway `PickerItem`.
    pub struct PickerItem {
        pub key: String,
        pub label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub badge: Option<String>,
    }
}

rpc_type! {
    /// Consolidates gateway `PickerResponse`.
    pub struct PickerResponse {
        pub section: String,
        pub items: Vec<PickerItem>,
        pub help: String,
    }
}

rpc_type! {
    pub struct SectionSelectParams {
        pub section: String,
        pub key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub alias: Option<String>,
    }
}

rpc_type! {
    /// Consolidates gateway `SelectItemResponse`.
    pub struct SelectItemResponse {
        pub fields_prefix: String,
        pub created: bool,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── File attachments ─────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Source hint for how the client obtained the file.
pub enum FileSource {
    Clipboard,
    #[default]
    File,
}

rpc_type! {
    /// A single file entry in a `file/attach` request. Either `path` (daemon
    /// reads from local disk — Unix socket only) or `data_b64` (client sends
    /// base64-encoded bytes) must be present.
    pub struct FileEntry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub data_b64: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mime_type: Option<String>,
        #[serde(default)]
        pub source: FileSource,
    }
}

rpc_type! {
    pub struct FileAttachParams {
        pub session_id: String,
        pub files: Vec<FileEntry>,
    }
}

rpc_type! {
    /// Result for a single file in a `file/attach` response.
    pub struct FileEntryResult {
        pub ref_id: String,
        pub marker: String,
        pub workspace_path: String,
        pub size_bytes: u64,
        pub deduplicated: bool,
    }
}

rpc_type! {
    pub struct FileAttachResult {
        pub files: Vec<FileEntryResult>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Session approval ─────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct SessionApproveParams {
        pub session_id: String,
        pub request_id: String,
        pub decision: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub replacement: Option<String>,
    }
}

rpc_type! {
    pub struct SessionApproveResult {
        pub session_id: String,
        pub request_id: String,
        pub acknowledged: bool,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Logs ─────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct LogsSubscribeResult {
        pub subscribed: bool,
    }
}

rpc_type! {
    pub struct LogsQueryParams {
        #[serde(default)]
        pub since_ts: Option<String>,
        #[serde(default)]
        pub until_ts: Option<String>,
        #[serde(default)]
        pub until_id: Option<String>,
        /// Byte offset to resume reading from. Set from the previous
        /// `LogsQueryResult::next_cursor_line_offset` for deterministic
        /// pagination regardless of id ordering.
        #[serde(default)]
        pub until_line_offset: Option<u64>,
        #[serde(default)]
        pub severity_min: Option<u8>,
        #[serde(default)]
        pub q: Option<String>,
        #[serde(default)]
        pub category: Option<String>,
        #[serde(default)]
        pub action: Option<String>,
        #[serde(default)]
        pub outcome: Option<String>,
        #[serde(default)]
        pub trace_id: Option<String>,
        #[serde(default)]
        pub hide_internal: bool,
        #[serde(default)]
        pub limit: Option<usize>,
    }
}

rpc_type! {
    pub struct LogsQueryResult {
        pub events: Vec<serde_json::Value>,
        /// Legacy cursor. Deprecated since 0.8.0; tracked for removal in
        /// <https://github.com/zeroclaw-labs/zeroclaw/issues/8012>.
        #[deprecated(
            since = "0.8.0",
            note = "tie-breaks by lexicographic id and can silently drop events; \
                    use `next_cursor_line_offset` / `until_line_offset` instead. \
                    Removal tracked in zeroclaw-labs/zeroclaw#8012."
        )]
        pub next_cursor: Option<(String, String)>,
        /// Byte offset past the last event on this page. Callers should
        /// pass this back as `until_line_offset` on the next request to
        /// resume without re-scanning already-read bytes.
        pub next_cursor_line_offset: Option<u64>,
        pub at_end: bool,
    }
}

rpc_type! {
    /// `logs/get` params — fetch a single event by id.
    pub struct LogsGetParams {
        pub id: String,
    }
}

rpc_type! {
    /// `logs/get` result. `event` is the full `LogEvent` payload
    /// (attributes, attribution map, span ids, …) that the Logs pane
    /// only renders inside the detail modal — list rows store
    /// preview-only data.
    pub struct LogsGetResult {
        pub event: serde_json::Value,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Session update notifications ─────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

/// Typed session update events pushed via `session/update` notifications.
/// Replaces the hand-built `notification_for_turn_event` function.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionUpdateEvent {
    AgentMessageChunk {
        session_id: String,
        text: String,
    },
    AgentThoughtChunk {
        session_id: String,
        text: String,
    },
    ToolCall {
        session_id: String,
        tool_call_id: String,
        name: String,
        raw_input: Value,
    },
    ToolResult {
        session_id: String,
        tool_call_id: String,
        name: String,
        raw_output: String,
    },
    ApprovalRequest {
        session_id: String,
        request_id: String,
        tool_name: String,
        arguments_summary: String,
        timeout_secs: u64,
    },
    /// Per-LLM-call token usage. `input_tokens` is the cumulative context size
    /// for this turn; `max_context_tokens` is the configured limit. Both may be
    /// absent when the provider doesn't report usage.
    ContextUsage {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_context_tokens: Option<u64>,
    },
    /// Terminal event for a turn. Replaces the response of `session/prompt`.
    /// `outcome` distinguishes a clean finish from a user-initiated cancel.
    TurnComplete {
        session_id: String,
        outcome: TurnCompletionOutcome,
        /// Final assistant text (Completed) or partial accumulated text
        /// at cancel point (Cancelled).
        content: String,
    },
    /// Emitted whenever older whole turns were dropped from the context window
    /// to fit the token budget. Surfaces a user-visible "context was cut here"
    /// marker so trimming is never silent. `dropped_messages` is the count of
    /// conversation messages removed; `kept_turns` is how many whole turns
    /// remained after the cut.
    HistoryTrimmed {
        session_id: String,
        dropped_messages: usize,
        kept_turns: usize,
        reason: String,
    },
}

/// Wire-stable subset of [`crate::rpc::turn::TurnOutcome`] for
/// `TurnComplete`. `messages` is intentionally not on the wire — the TUI
/// rebuilds from streamed chunks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnCompletionOutcome {
    Completed,
    Cancelled,
    Failed,
}

// ══════════════════════════════════════════════════════════════════════
// ── Quickstart ───────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════
//
// RPC mirror of the HTTP `/api/quickstart/*` routes in
// `zeroclaw-gateway`. The wire shapes are deliberately identical so the
// drift test in `tests/quickstart_drift.rs` can submit the same fixture
// `BuilderSubmission` through both transports and assert identical
// on-disk delta + identical response shape.

pub use crate::quickstart::{
    AppliedAgent, FieldDescriptor, FieldSection, QuickstartError, QuickstartStep, Surface,
};
pub use zeroclaw_config::presets::BuilderSubmission;

rpc_type! {
    /// Mirrors `zeroclaw_gateway::api_quickstart::QuickstartState`.
    pub struct QuickstartStateResult {
        pub quickstart_completed: bool,
        pub agents: Vec<String>,
        pub risk_profiles: Vec<String>,
        pub runtime_profiles: Vec<String>,
        /// `<provider_type>.<alias>` refs.
        pub model_providers: Vec<String>,
        /// `<channel_type>.<alias>` refs.
        pub channels: Vec<String>,
        /// Subset of `channels` not yet bound to any agent — safe to
        /// reuse without breaking the one-channel-one-agent invariant.
        #[serde(default)]
        pub unassigned_channels: Vec<String>,
        /// `<storage_type>.<alias>` refs.
        pub storage: Vec<String>,
        /// Picker rows for "Create new model provider" — sourced from
        /// the canonical `zeroclaw_providers::list_model_providers()`
        /// registry by [`crate::quickstart::snapshot_state`].
        pub model_provider_types: Vec<QuickstartTypeOption>,
        /// Picker rows for "Create new channel" — sourced from the
        /// schema's `ChannelsConfig` by walking its serialised
        /// top-level keys, so adding a channel family in the schema
        /// surfaces here automatically.
        pub channel_types: Vec<QuickstartTypeOption>,
    }
}

rpc_type! {
    /// One row in the Quickstart "Create new …" picker. The TUI and
    /// web surfaces both render this list as-is — no hardcoded
    /// option lists on either side.
    pub struct QuickstartTypeOption {
        /// Canonical kebab-case identifier written into config
        /// (`anthropic`, `telegram`, `wecom-ws`, …).
        pub kind: String,
        /// Human-readable picker label.
        pub display_name: String,
        /// `true` when the entry runs locally and needs no remote
        /// credential. Always `false` for channels.
        pub local: bool,
    }
}

rpc_type! {
    pub struct QuickstartValidateParams {
        pub submission: BuilderSubmission,
    }
}

rpc_type! {
    pub struct QuickstartFieldsParams {
        pub section: FieldSection,
        pub type_key: String,
    }
}

rpc_type! {
    pub struct QuickstartFieldsResult {
        pub fields: Vec<FieldDescriptor>,
    }
}

/// Tagged enum — matches the HTTP route's `ValidateResult` shape so
/// the drift test can compare bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuickstartValidateResult {
    Ok,
    Errors { errors: Vec<QuickstartError> },
}

rpc_type! {
    pub struct QuickstartApplyParams {
        pub submission: BuilderSubmission,
    }
}

/// Tagged enum — matches the HTTP route's `ApplyResult` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuickstartApplyResult {
    Applied {
        agent: AppliedAgent,
        /// `true` when the in-place daemon reload was signalled.
        /// `false` when no reload tx was attached (e.g. test harness)
        /// — caller must restart the daemon manually to pick up the
        /// change.
        daemon_restarted: bool,
    },
    Errors {
        errors: Vec<QuickstartError>,
    },
}

rpc_type! {
    pub struct QuickstartDismissParams {
        pub run_id: String,
        /// Surface that emitted the dismissal. Deserialised straight
        /// into the typed enum — no string-match at the boundary.
        pub surface: Surface,
        #[serde(default)]
        pub last_step: Option<QuickstartStep>,
    }
}

rpc_type! {
    pub struct QuickstartDismissResult {
        pub recorded: bool,
    }
}

#[cfg(test)]
mod tests {
    //! Lock the wire shape down to what callers actually depend on:
    //! - `#[serde(rename_all = "snake_case")]` on enums renames *variants*
    //!   (and fields, when present). Changing the case convention would
    //!   break every JSON-RPC client silently — these tests catch that.
    //! - `#[serde(tag = "kind", ...)]` on tagged enums (`Quickstart*Result`)
    //!   decides whether the discriminant is adjacent or inline; drift
    //!   here breaks the web quickstart surface that consumes them.
    //! - `#[serde(default, skip_serializing_if = "Option::is_none")]` on
    //!   request/result optionals must stay symmetric so the dispatcher
    //!   never sends an explicit `null` that older clients can't parse.

    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn chat_mode_serializes_as_snake_case() {
        assert_eq!(serde_json::to_value(ChatMode::Chat).unwrap(), json!("chat"));
        assert_eq!(serde_json::to_value(ChatMode::Acp).unwrap(), json!("acp"));
    }

    #[test]
    fn chat_mode_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_value::<ChatMode>(json!("chat")).unwrap(),
            ChatMode::Chat
        );
        assert_eq!(
            serde_json::from_value::<ChatMode>(json!("acp")).unwrap(),
            ChatMode::Acp
        );
    }

    #[test]
    fn file_source_default_is_file() {
        // `FileSource` does not derive `PartialEq`; assert via the wire
        // spelling instead. Default is `File` per the `#[default]`
        // attribute — drift here would change file-attach defaults for
        // every caller.
        assert_eq!(
            serde_json::to_value(FileSource::default()).unwrap(),
            json!("file")
        );
        assert_eq!(
            serde_json::to_value(FileSource::File).unwrap(),
            json!("file")
        );
        assert_eq!(
            serde_json::to_value(FileSource::Clipboard).unwrap(),
            json!("clipboard")
        );
    }

    #[test]
    fn turn_completion_outcome_round_trips_each_variant() {
        for variant in [
            TurnCompletionOutcome::Completed,
            TurnCompletionOutcome::Cancelled,
            TurnCompletionOutcome::Failed,
        ] {
            let s = serde_json::to_value(variant).unwrap();
            let back: TurnCompletionOutcome = serde_json::from_value(s.clone()).unwrap();
            assert_eq!(back, variant, "round-trip failed for {s:?}");
        }
        // Lock the wire spelling — older clients string-match on these.
        assert_eq!(
            serde_json::to_value(TurnCompletionOutcome::Completed).unwrap(),
            json!("completed")
        );
    }

    #[test]
    fn session_update_event_uses_snake_case_variants() {
        // Variants stay PascalCase → wire is snake_case, including the
        // multi-word ones that historically drifted when serde flattened
        // them. The discriminant lives under `"type"` (adjacent tagging)
        // — a change here would break every TUI that subscribes.
        let evt = SessionUpdateEvent::AgentMessageChunk {
            session_id: "s".into(),
            text: "t".into(),
        };
        let v = serde_json::to_value(evt).unwrap();
        assert_eq!(v["type"], json!("agent_message_chunk"));
        assert_eq!(v["session_id"], json!("s"));
        assert_eq!(v["text"], json!("t"));

        let evt = SessionUpdateEvent::ApprovalRequest {
            session_id: "s".into(),
            request_id: "r".into(),
            tool_name: "shell".into(),
            arguments_summary: "ls".into(),
            timeout_secs: 30,
        };
        let v = serde_json::to_value(evt).unwrap();
        assert_eq!(v["type"], json!("approval_request"));
        assert!(v.get("tool_name").is_some(), "got: {v}");
    }

    #[test]
    fn quickstart_validate_result_ok_variant_uses_kind_tag() {
        let v = serde_json::to_value(QuickstartValidateResult::Ok).unwrap();
        assert_eq!(v, json!({"kind": "ok"}));
    }

    #[test]
    fn quickstart_validate_result_errors_variant_carries_payload() {
        // Just smoke-test the field structure — `QuickstartError` is owned
        // by `quickstart` and has its own coverage there.
        let v =
            serde_json::to_value(QuickstartValidateResult::Errors { errors: Vec::new() }).unwrap();
        assert_eq!(v["kind"], json!("errors"));
        assert!(v["errors"].is_array(), "got: {v}");
    }

    #[test]
    fn quickstart_apply_result_applied_variant_carries_daemon_flag() {
        // `daemon_restarted: false` is the test-harness contract — the web
        // surface reads this to decide whether to tell the user to restart
        // manually. Lock it. The variant is tagged (`"kind": "applied"`),
        // and the agent payload is snake_case.
        let v = serde_json::to_value(QuickstartApplyResult::Applied {
            agent: AppliedAgent {
                alias: "primary".into(),
                model_provider: "anthropic.claude".into(),
                risk_profile: "standard".into(),
                runtime_profile: "default".into(),
                channels: vec!["telegram.main".into()],
                memory_backend: "sqlite".into(),
            },
            daemon_restarted: false,
        })
        .unwrap();
        assert_eq!(v["kind"], json!("applied"));
        assert_eq!(v["daemon_restarted"], json!(false));
        assert_eq!(v["agent"]["alias"], json!("primary"));
    }

    #[test]
    fn initialize_params_defaults_protocol_version_to_one() {
        // Older clients omit `protocol_version`; the runtime must default
        // to `1` so the handshake succeeds without an explicit version.
        let p: InitializeParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p.protocol_version, 1);
    }

    #[test]
    fn initialize_params_accepts_snake_case_field_names() {
        let p: InitializeParams = serde_json::from_value(json!({
            "protocol_version": 2,
            "tui_id": "abc",
            "tui_sig": "sig",
            "env": {"PATH": "/bin"}
        }))
        .unwrap();
        assert_eq!(p.protocol_version, 2);
        assert_eq!(p.tui_id.as_deref(), Some("abc"));
        assert_eq!(p.env.get("PATH").map(String::as_str), Some("/bin"));
    }

    #[test]
    fn initialize_params_renames_client_capabilities_field() {
        // The ACP elicitation RFD uses camelCase on the wire; the rename
        // is a one-shot field-level override — losing it would silently
        // break elicitation handshake for every TUI that speaks it.
        let p: InitializeParams = serde_json::from_value(json!({
            "clientCapabilities": {"elicitation": {"form": true}}
        }))
        .unwrap();
        let caps = p.client_capabilities.expect("rename lost the field");
        assert_eq!(caps["elicitation"]["form"], json!(true));
    }

    #[test]
    fn file_entry_skips_none_optional_fields_in_output() {
        // `skip_serializing_if = "Option::is_none"` is what keeps the
        // wire format tight for older clients that don't understand the
        // `data_b64` field. Symmetric with the deserialize side.
        let entry = FileEntry {
            path: Some("/tmp/x".into()),
            data_b64: None,
            filename: None,
            mime_type: None,
            source: FileSource::File,
        };
        let v = serde_json::to_value(entry).unwrap();
        assert_eq!(v["path"], json!("/tmp/x"));
        assert_eq!(v["source"], json!("file"));
        assert!(
            v.as_object().unwrap().get("data_b64").is_none(),
            "None data_b64 leaked into wire: {v}"
        );
        assert!(
            v.as_object().unwrap().get("filename").is_none(),
            "None filename leaked into wire: {v}"
        );
    }

    #[test]
    fn file_entry_deserializes_when_only_path_is_present() {
        // The contract is "path OR data_b64"; the schema must accept
        // path-only entries without forcing the caller to send nulls.
        let entry: FileEntry = serde_json::from_value(json!({"path": "/tmp/a"})).unwrap();
        assert_eq!(entry.path.as_deref(), Some("/tmp/a"));
        assert!(entry.data_b64.is_none());
        assert_eq!(serde_json::to_value(entry.source).unwrap(), json!("file"));
    }

    #[test]
    fn tui_list_entry_round_trip_preserves_all_fields() {
        let entry = TuiListEntry {
            tui_id: "tui-1".into(),
            connected_at: "2026-06-29T10:00:00Z".into(),
            connected_at_unix: 1_750_000_000,
            peer_label: "desktop".into(),
            transport: "wss".into(),
        };
        let v: Value = serde_json::to_value(&entry).unwrap();
        let back: TuiListEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back.tui_id, entry.tui_id);
        assert_eq!(back.connected_at_unix, entry.connected_at_unix);
        assert_eq!(back.transport, entry.transport);
    }

    #[test]
    fn quickstart_type_option_round_trip() {
        let opt = QuickstartTypeOption {
            kind: "anthropic".into(),
            display_name: "Anthropic".into(),
            local: false,
        };
        let v = serde_json::to_value(&opt).unwrap();
        assert_eq!(v["kind"], json!("anthropic"));
        assert_eq!(v["local"], json!(false));
        let back: QuickstartTypeOption = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind, opt.kind);
        assert_eq!(back.local, opt.local);
    }

    #[test]
    fn quickstart_dismiss_params_deserializes_with_optional_last_step() {
        // `last_step` is `#[serde(default)] Option<QuickstartStep>` — older
        // dismiss payloads omit it. Must default to `None` without error.
        let params: QuickstartDismissParams = serde_json::from_value(json!({
            "run_id": "r1",
            "surface": "tui"
        }))
        .unwrap();
        assert_eq!(params.run_id, "r1");
        assert_eq!(params.surface, Surface::Tui);
        assert!(params.last_step.is_none());
    }
}
