//! Core shared types for the Aria SDK surface area.
//!
//! Defines result types, parameter definitions, sandbox config,
//! decorator metadata keys, and feed card types used across all registries.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Result Types ─────────────────────────────────────────────────

/// Generic result wrapper for tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult<T = serde_json::Value> {
    pub success: bool,
    pub result: Option<T>,
    pub error: Option<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Generic result wrapper for agent execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult<T = serde_json::Value> {
    pub success: bool,
    pub result: Option<T>,
    pub error: Option<String>,
    pub model: Option<String>,
    pub tokens_used: Option<u64>,
    pub duration_ms: Option<u64>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Generic result wrapper for team execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamResult<T = serde_json::Value> {
    pub success: bool,
    pub result: Option<T>,
    pub error: Option<String>,
    pub agent_results: Vec<AgentResult<T>>,
    pub mode: String,
    pub duration_ms: Option<u64>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Generic result wrapper for pipeline execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult<T = serde_json::Value> {
    pub success: bool,
    pub result: Option<T>,
    pub error: Option<String>,
    pub step_results: Vec<StepResult<T>>,
    pub duration_ms: Option<u64>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Result of a single pipeline step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult<T = serde_json::Value> {
    pub step_id: String,
    pub step_name: String,
    pub success: bool,
    pub result: Option<T>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    pub retries: u32,
}

// ── Parameter Definitions ────────────────────────────────────────

/// Defines a parameter for a tool or agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub param_type: ParameterType,
    pub required: bool,
    pub default: Option<serde_json::Value>,
    pub enum_values: Option<Vec<serde_json::Value>>,
}

/// Supported parameter types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParameterType {
    String,
    Number,
    Integer,
    Boolean,
    Array,
    Object,
}

// ── Aria Configuration ───────────────────────────────────────────

/// Top-level Aria SDK configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaConfig {
    /// Database path for the shared `SQLite` registry
    pub db_path: String,
    /// Quilt API URL for sandboxed execution
    pub quilt_api_url: Option<String>,
    /// Quilt API key
    pub quilt_api_key: Option<String>,
    /// Default memory limit for containers (MB)
    #[serde(default = "default_memory_limit")]
    pub default_memory_limit_mb: u32,
    /// Default CPU limit for containers (percent)
    #[serde(default = "default_cpu_limit")]
    pub default_cpu_limit_percent: u32,
    /// Max concurrent task executions
    #[serde(default = "default_max_concurrent_tasks")]
    pub max_concurrent_tasks: u32,
    /// Task polling interval in seconds
    #[serde(default = "default_task_poll_interval")]
    pub task_poll_interval_secs: u64,
    /// Container idle timeout for pruning (hours)
    #[serde(default = "default_idle_hours")]
    pub container_idle_hours: u32,
    /// Container max age for pruning (days)
    #[serde(default = "default_max_age_days")]
    pub container_max_age_days: u32,
}

fn default_memory_limit() -> u32 {
    4096
}
fn default_cpu_limit() -> u32 {
    100
}
fn default_max_concurrent_tasks() -> u32 {
    4
}
fn default_task_poll_interval() -> u64 {
    5
}
fn default_idle_hours() -> u32 {
    24
}
fn default_max_age_days() -> u32 {
    7
}

impl Default for AriaConfig {
    fn default() -> Self {
        Self {
            db_path: String::new(),
            quilt_api_url: None,
            quilt_api_key: None,
            default_memory_limit_mb: default_memory_limit(),
            default_cpu_limit_percent: default_cpu_limit(),
            max_concurrent_tasks: default_max_concurrent_tasks(),
            task_poll_interval_secs: default_task_poll_interval(),
            container_idle_hours: default_idle_hours(),
            container_max_age_days: default_max_age_days(),
        }
    }
}

// ── Sandbox Configuration ────────────────────────────────────────

/// Configuration for sandboxed container execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub image: String,
    pub memory_limit_mb: Option<u32>,
    pub cpu_limit_percent: Option<u32>,
    pub setup_command: Option<String>,
    pub environment: HashMap<String, String>,
    pub volumes: Vec<ContainerVolumeMount>,
    pub ports: Vec<ContainerPort>,
    pub workspace_access: bool,
    pub workspace_paths: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            image: "node:20-slim".into(),
            memory_limit_mb: Some(4096),
            cpu_limit_percent: Some(100),
            setup_command: None,
            environment: HashMap::new(),
            volumes: Vec::new(),
            ports: Vec::new(),
            workspace_access: false,
            workspace_paths: Vec::new(),
        }
    }
}

// ── Container Types ──────────────────────────────────────────────

/// Volume mount specification for containers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerVolumeMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// Port mapping specification for containers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerPort {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: PortProtocol,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

// ── Container Configuration ─────────────────────────────────────

/// Full container configuration for Quilt lifecycle management
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContainerConfig {
    pub image: String,
    pub name: Option<String>,
    pub command: Option<Vec<String>>,
    pub environment: HashMap<String, String>,
    pub volumes: Vec<ContainerVolumeMount>,
    pub ports: Vec<ContainerPort>,
    pub memory_limit_mb: Option<u32>,
    pub cpu_limit_percent: Option<u32>,
    pub restart_policy: RestartPolicy,
    pub labels: HashMap<String, String>,
    pub network: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum RestartPolicy {
    #[default]
    No,
    Always,
    OnFailure,
    UnlessStopped,
}

// ── Network Configuration ────────────────────────────────────────

/// Network configuration for multi-container networking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub name: String,
    pub driver: NetworkDriver,
    pub isolation: NetworkIsolation,
    pub ipv6: bool,
    pub dns: Option<NetworkDnsConfig>,
    pub labels: HashMap<String, String>,
    pub options: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum NetworkDriver {
    #[default]
    Bridge,
    Host,
    Overlay,
    Macvlan,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum NetworkIsolation {
    #[default]
    Default,
    Isolated,
}

/// DNS configuration for container networks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkDnsConfig {
    pub nameservers: Vec<String>,
    pub search: Vec<String>,
    pub options: Vec<String>,
}

// ── Feed Types ───────────────────────────────────────────────────

/// All 24 supported feed card types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedCardType {
    Text,
    Image,
    Link,
    Quote,
    Code,
    Markdown,
    Chart,
    Table,
    List,
    Metric,
    Alert,
    Weather,
    Stock,
    News,
    Social,
    Calendar,
    Task,
    File,
    Audio,
    Video,
    Map,
    Poll,
    Embed,
    Custom,
}

/// Feed content categories
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedCategory {
    News,
    Social,
    Productivity,
    Finance,
    Weather,
    Health,
    Entertainment,
    Technology,
    Sports,
    Science,
    Education,
    Custom(String),
}

/// A single feed item produced by feed execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItem {
    pub card_type: FeedCardType,
    pub title: String,
    pub body: Option<String>,
    pub source: Option<String>,
    pub url: Option<String>,
    pub metadata: Option<HashMap<String, serde_json::Value>>,
    pub timestamp: Option<i64>,
}

/// Result of a feed execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedResult<T = serde_json::Value> {
    pub success: bool,
    pub items: Vec<FeedItem>,
    pub summary: Option<String>,
    pub metadata: Option<T>,
    pub error: Option<String>,
}

/// Feed configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedConfig {
    pub name: String,
    pub description: Option<String>,
    pub schedule: String,
    pub refresh_seconds: Option<u64>,
    pub category: Option<FeedCategory>,
    pub retention: Option<FeedRetention>,
    pub display: Option<FeedDisplay>,
    pub sandbox: Option<SandboxConfig>,
}

/// Feed retention policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedRetention {
    pub max_items: Option<u32>,
    pub max_age_days: Option<u32>,
}

/// Feed display configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedDisplay {
    pub priority: Option<u32>,
    pub icon: Option<String>,
}

/// Feed execution context passed to handlers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedContext {
    pub feed_id: String,
    pub tenant_id: String,
    pub run_id: String,
    pub previous_items: Vec<FeedItem>,
}

// ── Cron Types ───────────────────────────────────────────────────

/// Flexible cron schedule — one-time, interval, or cron expression
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CronSchedule {
    /// One-time execution at a specific timestamp (ms)
    At { at_ms: i64 },
    /// Recurring interval (ms) with optional anchor
    Every {
        every_ms: u64,
        anchor_ms: Option<i64>,
    },
    /// Standard cron expression with optional timezone
    Cron { expr: String, tz: Option<String> },
}

/// Session target for cron job execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum CronSessionTarget {
    #[default]
    Main,
    Isolated,
}

/// Wake mode for cron execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum CronWakeMode {
    #[default]
    NextHeartbeat,
    Now,
}

/// Payload types for cron job execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CronPayload {
    /// Inject a system event into the session
    SystemEvent { text: String },
    /// Run an agent turn
    AgentTurn {
        message: String,
        model: Option<String>,
        thinking: Option<String>,
        timeout_seconds: Option<u64>,
        deliver: Option<bool>,
        channel: Option<CronMessageChannel>,
        to: Option<String>,
        best_effort_deliver: Option<bool>,
    },
}

/// Channel target for cron agent turn delivery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronMessageChannel {
    pub channel_type: String,
    pub channel_id: Option<String>,
}

/// Isolation settings for cron jobs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronIsolation {
    pub post_to_main_prefix: Option<String>,
    pub post_to_main_mode: Option<PostToMainMode>,
    pub post_to_main_max_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PostToMainMode {
    Summary,
    Full,
}

// ── Cron Configuration ───────────────────────────────────────────

/// Cron function configuration from decorator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronConfig {
    pub name: String,
    pub description: Option<String>,
    pub schedule: CronSchedule,
    pub session_target: CronSessionTarget,
    pub wake_mode: CronWakeMode,
    pub payload: CronPayload,
    pub isolation: Option<CronIsolation>,
    pub enabled: bool,
    pub delete_after_run: bool,
}

// ── Memory Types ─────────────────────────────────────────────────

/// Memory storage tiers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MemoryTier {
    /// Session-scoped, cleared when session ends
    Scratchpad,
    /// TTL-based, auto-expires
    Ephemeral,
    /// Persistent, never expires (default)
    #[default]
    Longterm,
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scratchpad => write!(f, "scratchpad"),
            Self::Ephemeral => write!(f, "ephemeral"),
            Self::Longterm => write!(f, "longterm"),
        }
    }
}

/// Memory configuration from decorator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoreConfig {
    pub name: String,
    pub tier: MemoryTier,
    pub namespace: Option<String>,
    pub ttl_seconds: Option<u64>,
}

/// Options for memory store operations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryStoreOptions {
    pub tier: Option<MemoryTier>,
    pub ttl_seconds: Option<u64>,
    pub namespace: Option<String>,
    pub session_id: Option<String>,
}

// ── Tool Configuration ───────────────────────────────────────────

/// Tool configuration from decorator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ParameterDefinition>,
    pub sandbox: Option<SandboxConfig>,
}

// ── Agent Configuration ──────────────────────────────────────────

/// Agent configuration from decorator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub thinking: Option<ThinkingLevel>,
    pub max_retries: Option<u32>,
    pub timeout_seconds: Option<u64>,
    pub sandbox: Option<SandboxConfig>,
}

/// Agent thinking/reasoning level
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    None,
    Low,
    Medium,
    High,
}

/// Runtime options that can be injected into agents
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRuntimeOptions {
    pub model_override: Option<String>,
    pub temperature_override: Option<f64>,
    pub max_tokens: Option<u64>,
    pub stop_sequences: Option<Vec<String>>,
    pub injections: Vec<RuntimeInjection>,
}

// ── Team Configuration ───────────────────────────────────────────

/// Team collaboration modes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TeamMode {
    /// A coordinator agent delegates to team members
    #[default]
    Coordinator,
    /// Agents take turns in order
    RoundRobin,
    /// Route to the best agent for the task
    DelegateToBest,
    /// All agents work simultaneously
    Parallel,
    /// Agents execute in strict sequence, each building on prior output
    Sequential,
}

/// Configuration for a team member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberConfig {
    pub agent_id: String,
    pub role: Option<String>,
    pub capabilities: Vec<String>,
    pub weight: Option<f64>,
}

/// Team configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    pub description: Option<String>,
    pub mode: TeamMode,
    pub members: Vec<TeamMemberConfig>,
    pub shared_context: Option<SharedContextConfig>,
    pub timeout_seconds: Option<u64>,
    pub max_rounds: Option<u32>,
}

/// Shared context configuration for team members
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SharedContextConfig {
    pub enabled: bool,
    pub max_history: Option<u32>,
    pub include_tool_results: bool,
}

// ── Pipeline Configuration ───────────────────────────────────────

/// Pipeline step definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub id: String,
    pub name: String,
    pub step_type: PipelineStepType,
    pub agent_id: Option<String>,
    pub tool_id: Option<String>,
    pub team_id: Option<String>,
    pub input_mapping: Option<HashMap<String, String>>,
    pub output_key: Option<String>,
    pub condition: Option<String>,
    pub dependencies: Vec<String>,
    pub retry_policy: Option<RetryPolicy>,
    pub timeout_seconds: Option<u64>,
}

/// Type of pipeline step
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PipelineStepType {
    Agent,
    Tool,
    Team,
    Condition,
    Transform,
}

/// Retry policy for pipeline steps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f64,
    pub retry_on: Vec<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 30_000,
            backoff_multiplier: 2.0,
            retry_on: vec!["error".into(), "timeout".into()],
        }
    }
}

/// Pipeline configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<PipelineStep>,
    pub variables: HashMap<String, serde_json::Value>,
    pub timeout_seconds: Option<u64>,
    pub max_parallel: Option<u32>,
}

// ── Runtime Injection ────────────────────────────────────────────

/// Types of platform capabilities that can be injected into agents
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum InjectionType {
    Logging,
    Memory,
    Tasks,
    Database,
    FileSystem,
    Network,
    Notifications,
    Scheduler,
    Analytics,
    Secrets,
    Config,
    Events,
}

/// A runtime injection binding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInjection {
    pub injection_type: InjectionType,
    pub config: Option<serde_json::Value>,
}

// ── Metadata Keys ────────────────────────────────────────────────

/// All decorator metadata keys used by the Aria SDK
pub struct MetadataKeys;

impl MetadataKeys {
    pub const TOOL: &'static str = "aria:tool";
    pub const AGENT: &'static str = "aria:agent";
    pub const TEAM: &'static str = "aria:team";
    pub const PIPELINE: &'static str = "aria:pipeline";
    pub const FEED: &'static str = "aria:feed";
    pub const CRON: &'static str = "aria:cron";
    pub const MEMORY: &'static str = "aria:memory";
    pub const CONTAINER: &'static str = "aria:container";
    pub const NETWORK: &'static str = "aria:network";
    pub const INJECTION: &'static str = "aria:injection";
    pub const PARAMETER: &'static str = "aria:parameter";
}

// ── Entity Status ────────────────────────────────────────────────

/// Status for registry entities (tools, agents, feeds, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum EntityStatus {
    #[default]
    Active,
    Paused,
    Deleted,
}

impl std::fmt::Display for EntityStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Paused => write!(f, "paused"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

/// Task-specific status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TaskStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Container runtime state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ContainerState {
    #[default]
    Pending,
    Running,
    Stopped,
    Exited,
    Error,
}

impl std::fmt::Display for ContainerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Exited => write!(f, "exited"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_serializes_correctly() {
        let result: ToolResult = ToolResult {
            success: true,
            result: Some(serde_json::json!({"data": "hello"})),
            error: None,
            metadata: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"data\":\"hello\""));
    }

    #[test]
    fn agent_result_serializes_correctly() {
        let result: AgentResult = AgentResult {
            success: true,
            result: Some(serde_json::json!("done")),
            error: None,
            model: Some("claude-3".into()),
            tokens_used: Some(100),
            duration_ms: Some(500),
            metadata: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: AgentResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.model.as_deref(), Some("claude-3"));
    }

    #[test]
    fn parameter_definition_roundtrip() {
        let param = ParameterDefinition {
            name: "query".into(),
            description: "Search query".into(),
            param_type: ParameterType::String,
            required: true,
            default: None,
            enum_values: None,
        };
        let json = serde_json::to_string(&param).unwrap();
        let parsed: ParameterDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "query");
        assert_eq!(parsed.param_type, ParameterType::String);
    }

    #[test]
    fn cron_schedule_variants_serialize() {
        let at = CronSchedule::At {
            at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&at).unwrap();
        assert!(json.contains("\"kind\":\"at\""));

        let every = CronSchedule::Every {
            every_ms: 60_000,
            anchor_ms: None,
        };
        let json = serde_json::to_string(&every).unwrap();
        assert!(json.contains("\"kind\":\"every\""));

        let cron_expr = CronSchedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        };
        let json = serde_json::to_string(&cron_expr).unwrap();
        assert!(json.contains("\"kind\":\"cron\""));
    }

    #[test]
    fn memory_tier_default_is_longterm() {
        assert_eq!(MemoryTier::default(), MemoryTier::Longterm);
    }

    #[test]
    fn team_mode_variants() {
        let modes = vec![
            TeamMode::Coordinator,
            TeamMode::RoundRobin,
            TeamMode::DelegateToBest,
            TeamMode::Parallel,
            TeamMode::Sequential,
        ];
        for mode in modes {
            let json = serde_json::to_string(&mode).unwrap();
            let parsed: TeamMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn entity_status_display() {
        assert_eq!(EntityStatus::Active.to_string(), "active");
        assert_eq!(EntityStatus::Paused.to_string(), "paused");
        assert_eq!(EntityStatus::Deleted.to_string(), "deleted");
    }

    #[test]
    fn task_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::Running.to_string(), "running");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
        assert_eq!(TaskStatus::Failed.to_string(), "failed");
        assert_eq!(TaskStatus::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn container_state_display() {
        assert_eq!(ContainerState::Pending.to_string(), "pending");
        assert_eq!(ContainerState::Running.to_string(), "running");
        assert_eq!(ContainerState::Error.to_string(), "error");
    }

    #[test]
    fn feed_card_types_serialize() {
        let cards = vec![
            FeedCardType::Text,
            FeedCardType::Image,
            FeedCardType::Chart,
            FeedCardType::Custom,
        ];
        for card in cards {
            let json = serde_json::to_string(&card).unwrap();
            let parsed: FeedCardType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, card);
        }
    }

    #[test]
    fn pipeline_step_type_variants() {
        let types = vec![
            PipelineStepType::Agent,
            PipelineStepType::Tool,
            PipelineStepType::Team,
            PipelineStepType::Condition,
            PipelineStepType::Transform,
        ];
        for t in types {
            let json = serde_json::to_string(&t).unwrap();
            let parsed: PipelineStepType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, t);
        }
    }

    #[test]
    fn metadata_keys_are_namespaced() {
        assert!(MetadataKeys::TOOL.starts_with("aria:"));
        assert!(MetadataKeys::AGENT.starts_with("aria:"));
        assert!(MetadataKeys::TEAM.starts_with("aria:"));
        assert!(MetadataKeys::PIPELINE.starts_with("aria:"));
    }

    #[test]
    fn aria_config_defaults() {
        let config = AriaConfig::default();
        assert_eq!(config.default_memory_limit_mb, 4096);
        assert_eq!(config.default_cpu_limit_percent, 100);
        assert_eq!(config.max_concurrent_tasks, 4);
    }

    #[test]
    fn retry_policy_defaults() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.backoff_multiplier, 2.0);
    }

    #[test]
    fn injection_types_serialize() {
        let types = vec![
            InjectionType::Logging,
            InjectionType::Memory,
            InjectionType::Tasks,
            InjectionType::Database,
            InjectionType::FileSystem,
            InjectionType::Network,
            InjectionType::Notifications,
            InjectionType::Scheduler,
            InjectionType::Analytics,
            InjectionType::Secrets,
            InjectionType::Config,
            InjectionType::Events,
        ];
        assert_eq!(types.len(), 12);
        for t in types {
            let json = serde_json::to_string(&t).unwrap();
            let parsed: InjectionType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, t);
        }
    }

    #[test]
    fn sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.image, "node:20-slim");
        assert_eq!(config.memory_limit_mb, Some(4096));
        assert!(config.volumes.is_empty());
    }

    #[test]
    fn network_config_serializes() {
        let config = NetworkConfig {
            name: "test-net".into(),
            driver: NetworkDriver::Bridge,
            isolation: NetworkIsolation::Isolated,
            ipv6: false,
            dns: Some(NetworkDnsConfig {
                nameservers: vec!["8.8.8.8".into()],
                search: vec![],
                options: vec![],
            }),
            labels: HashMap::new(),
            options: HashMap::new(),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("test-net"));
        assert!(json.contains("bridge"));
    }

    #[test]
    fn cron_payload_variants() {
        let sys = CronPayload::SystemEvent {
            text: "wake up".into(),
        };
        let json = serde_json::to_string(&sys).unwrap();
        assert!(json.contains("systemEvent"));

        let agent = CronPayload::AgentTurn {
            message: "do task".into(),
            model: None,
            thinking: None,
            timeout_seconds: Some(30),
            deliver: None,
            channel: None,
            to: None,
            best_effort_deliver: None,
        };
        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains("agentTurn"));
    }

    #[test]
    fn cron_session_target_default() {
        assert_eq!(CronSessionTarget::default(), CronSessionTarget::Main);
    }
}
