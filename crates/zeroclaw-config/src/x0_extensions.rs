//! X0 fork-specific configuration extensions.
//! These types are additive to the upstream zeroclaw-config schema.
//!
//! The `default_*` helper fns below are wired in as `#[serde(default = "fn")]`
//! attributes once the corresponding fork modules complete their V3 port. They
//! are intentionally written ahead so the schema reaches stability before the
//! consumers do. The `dead_code` allow is module-scoped and transitional —
//! remove it as each consumer comes online.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json;
use std::path::PathBuf;

// BotRateLimiter uses atomics but is not serializable — skipped from config.
// We use a placeholder for fields that reference upstream types not available here.

// ── Delegate Agents ──────────────────────────────────────────────

/// Configuration for a delegate sub-agent used by the `delegate` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DelegateAgentConfig {
    /// Provider name (e.g. "ollama", "openrouter", "anthropic")
    pub provider: String,
    /// Model name
    pub model: String,
    /// Optional system prompt for the sub-agent
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional API key override
    #[serde(default)]
    pub api_key: Option<String>,
    /// Temperature override
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Max recursion depth for nested delegation
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

// ── Bot Workspace Isolation ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BotConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub workspace_dir: Option<PathBuf>,
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
    #[serde(default)]
    pub soul: Option<SoulConfig>,
    pub port: u16,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub channels: Option<serde_json::Value>,
    #[serde(default)]
    pub memory: Option<serde_json::Value>,
    #[serde(default)]
    pub max_memory_mb: Option<u64>,
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub max_tokens_per_minute: Option<u64>,
}

/// Simple token-bucket rate limiter for per-bot resource enforcement.
#[derive(Debug)]
pub struct BotRateLimiter {
    max_tokens_per_minute: u64,
    max_concurrent: u32,
    tokens_used: std::sync::atomic::AtomicU64,
    active_requests: std::sync::atomic::AtomicU32,
    window_start: parking_lot::Mutex<std::time::Instant>,
}

impl BotRateLimiter {
    pub fn new(max_tokens_per_minute: u64, max_concurrent: u32) -> Self {
        Self {
            max_tokens_per_minute,
            max_concurrent,
            tokens_used: std::sync::atomic::AtomicU64::new(0),
            active_requests: std::sync::atomic::AtomicU32::new(0),
            window_start: parking_lot::Mutex::new(std::time::Instant::now()),
        }
    }

    pub fn from_config(_config: &crate::schema::Config) -> Self {
        Self::new(100_000, 10)
    }

    pub fn try_acquire(&self) -> bool {
        let current = self
            .active_requests
            .load(std::sync::atomic::Ordering::Relaxed);
        if current >= self.max_concurrent {
            return false;
        }
        self.active_requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        true
    }

    pub fn release(&self) {
        self.active_requests
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn record_tokens(&self, count: u64) -> bool {
        self.maybe_reset_window();
        let prev = self
            .tokens_used
            .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
        prev + count <= self.max_tokens_per_minute
    }

    pub fn tokens_remaining(&self) -> u64 {
        self.maybe_reset_window();
        let used = self.tokens_used.load(std::sync::atomic::Ordering::Relaxed);
        self.max_tokens_per_minute.saturating_sub(used)
    }

    fn maybe_reset_window(&self) {
        let mut start = self.window_start.lock();
        if start.elapsed() >= std::time::Duration::from_secs(60) {
            self.tokens_used
                .store(0, std::sync::atomic::Ordering::Relaxed);
            *start = std::time::Instant::now();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AgentConfig {
    /// When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models.
    #[serde(default)]
    pub compact_context: bool,
    #[serde(default = "default_agent_max_tool_iterations")]
    pub max_tool_iterations: usize,
    #[serde(default = "default_agent_max_history_messages")]
    pub max_history_messages: usize,
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default = "default_agent_tool_dispatcher")]
    pub tool_dispatcher: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            compact_context: false,
            max_tool_iterations: default_agent_max_tool_iterations(),
            max_history_messages: default_agent_max_history_messages(),
            parallel_tools: false,
            tool_dispatcher: default_agent_tool_dispatcher(),
        }
    }
}

// ── Soul system ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SoulConfig {
    /// Enable the soul system (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Soul file format: "soul/v1" (SOUL.md with YAML frontmatter)
    #[serde(default = "default_soul_format")]
    pub format: String,

    /// Path to SOUL.md (relative to workspace, default: "SOUL.md")
    #[serde(default = "default_soul_path")]
    pub soul_path: String,

    /// Expected SHA-256 hash of the constitution laws (verified on load)
    #[serde(default)]
    pub constitution_hash: Option<String>,

    /// Enable alignment tracking against genesis prompt (default: false)
    #[serde(default)]
    pub enable_alignment_tracking: bool,

    /// Enable periodic auto-reflection (default: false)
    #[serde(default)]
    pub enable_auto_reflection: bool,

    /// Trigger reflection every N messages (default: 50)
    #[serde(default = "default_reflection_interval")]
    pub reflection_interval_messages: usize,

    /// Token budgets for tiered memory (working/episodic/semantic/procedural)
    #[serde(default)]
    pub memory_budgets: MemoryTokenBudgetsConfig,
}

/// Token budgets for tiered memory management.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MemoryTokenBudgetsConfig {
    /// Working memory (session context) token budget
    #[serde(default = "default_working_budget")]
    pub working: usize,
    /// Episodic memory (events/experiences) token budget
    #[serde(default = "default_episodic_budget")]
    pub episodic: usize,
    /// Semantic memory (knowledge/facts) token budget
    #[serde(default = "default_semantic_budget")]
    pub semantic: usize,
    /// Procedural memory (skills/procedures) token budget
    #[serde(default = "default_procedural_budget")]
    pub procedural: usize,
}

impl Default for MemoryTokenBudgetsConfig {
    fn default() -> Self {
        Self {
            working: default_working_budget(),
            episodic: default_episodic_budget(),
            semantic: default_semantic_budget(),
            procedural: default_procedural_budget(),
        }
    }
}

// ── Replication ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ReplicationConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_max_children")]
    pub max_children: usize,

    #[serde(default = "default_child_workspace_dir")]
    pub child_workspace_dir: String,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_children: default_max_children(),
            child_workspace_dir: default_child_workspace_dir(),
        }
    }
}

// ── Model Strategy ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TierModelConfig {
    pub tier: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelStrategyConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub tier_models: Vec<TierModelConfig>,

    #[serde(default)]
    pub per_session_budget_usd: Option<f64>,

    #[serde(default)]
    pub per_call_budget_usd: Option<f64>,
}

impl Default for SoulConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format: default_soul_format(),
            soul_path: default_soul_path(),
            constitution_hash: None,
            enable_alignment_tracking: false,
            enable_auto_reflection: false,
            reflection_interval_messages: default_reflection_interval(),
            memory_budgets: MemoryTokenBudgetsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelPricing {
    /// Input price per 1M tokens
    #[serde(default)]
    pub input: f64,

    /// Output price per 1M tokens
    #[serde(default)]
    pub output: f64,
}

/// Configurable survival tier thresholds (in cents).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SurvivalThresholdsConfig {
    /// Balance threshold for High tier (default: 500 cents)
    #[serde(default = "default_survival_high")]
    pub high: i64,
    /// Balance threshold for Normal tier (default: 50 cents)
    #[serde(default = "default_survival_normal")]
    pub normal: i64,
    /// Balance threshold for LowCompute tier (default: 10 cents)
    #[serde(default = "default_survival_low_compute")]
    pub low_compute: i64,
    /// Balance threshold for Critical tier (default: 0 cents)
    #[serde(default)]
    pub critical: i64,
}

impl Default for SurvivalThresholdsConfig {
    fn default() -> Self {
        Self {
            high: default_survival_high(),
            normal: default_survival_normal(),
            low_compute: default_survival_low_compute(),
            critical: 0,
        }
    }
}

// ── Wallet Config ──────────────────────────────────────────────

/// EVM wallet configuration (requires `--features wallet`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct WalletConfig {
    /// Enable wallet functionality (default: false)
    #[serde(default)]
    pub enabled: bool,
    /// Path to wallet key file (default: "wallet" directory under workspace)
    #[serde(default = "default_wallet_path")]
    pub wallet_path: String,
    /// Auto-generate wallet if none exists (default: true)
    #[serde(default = "default_wallet_auto_generate")]
    pub auto_generate: bool,
    /// RPC endpoint URL for on-chain operations (empty = on-chain tools disabled)
    #[serde(default)]
    pub rpc_url: String,
    /// EVM chain ID (default: 11155111 = Sepolia testnet)
    #[serde(default = "default_wallet_chain_id")]
    pub chain_id: u64,
}

impl Default for WalletConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wallet_path: default_wallet_path(),
            auto_generate: true,
            rpc_url: String::new(),
            chain_id: default_wallet_chain_id(),
        }
    }
}

/// Treasury configuration — spending limits for x402 payments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TreasuryConfig {
    /// Max payment per single x402 transaction in cents (default: 100 = $1)
    #[serde(default = "default_max_x402_payment_cents")]
    pub max_x402_payment_cents: u64,
    /// Allowed domains for x402 payments (empty = allow all)
    #[serde(default)]
    pub x402_allowed_domains: Vec<String>,
    /// Max daily x402 spend in cents (default: 500 = $5)
    #[serde(default = "default_max_daily_x402_spend_cents")]
    pub max_daily_spend_cents: u64,
    /// Max monthly x402 spend in cents (default: 5000 = $50)
    #[serde(default = "default_max_monthly_x402_spend_cents")]
    pub max_monthly_spend_cents: u64,
}

impl Default for TreasuryConfig {
    fn default() -> Self {
        Self {
            max_x402_payment_cents: default_max_x402_payment_cents(),
            x402_allowed_domains: vec![],
            max_daily_spend_cents: default_max_daily_x402_spend_cents(),
            max_monthly_spend_cents: default_max_monthly_x402_spend_cents(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StorageProviderSection {
    #[serde(default)]
    pub config: StorageProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StorageProviderConfig {
    /// Storage engine key (e.g. "postgres", "sqlite").
    #[serde(default)]
    pub provider: String,

    /// Connection URL for remote providers.
    /// Accepts legacy aliases: dbURL, database_url, databaseUrl.
    #[serde(
        default,
        alias = "dbURL",
        alias = "database_url",
        alias = "databaseUrl"
    )]
    pub db_url: Option<String>,

    /// Database schema for SQL backends.
    #[serde(default = "default_storage_schema")]
    pub schema: String,

    /// Table name for memory entries.
    #[serde(default = "default_storage_table")]
    pub table: String,

    /// Optional connection timeout in seconds for remote providers.
    #[serde(default)]
    pub connect_timeout_secs: Option<u64>,
}

impl Default for StorageProviderConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            db_url: None,
            schema: default_storage_schema(),
            table: default_storage_table(),
            connect_timeout_secs: None,
        }
    }
}

// ── Autonomy / Security ──────────────────────────────────────────

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AutonomyConfig {
    pub level: String,
    pub workspace_only: bool,
    pub allowed_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,

    /// Require explicit approval for medium-risk shell commands.
    #[serde(default = "default_true")]
    pub require_approval_for_medium_risk: bool,

    /// Block high-risk shell commands even if allowlisted.
    #[serde(default = "default_true")]
    pub block_high_risk_commands: bool,

    /// Tools that never require approval (e.g. read-only tools).
    #[serde(default = "default_auto_approve")]
    pub auto_approve: Vec<String>,

    /// Tools that always require interactive approval, even after "Always".
    #[serde(default = "default_always_ask")]
    pub always_ask: Vec<String>,

    /// Enable the goal-driven autonomy loop (polls approved goals and dispatches them).
    #[serde(default)]
    pub loop_enabled: bool,

    /// How often the autonomy loop polls for approved goals (seconds).
    #[serde(default = "default_autonomy_loop_poll_secs")]
    pub loop_poll_secs: u64,

    /// Maximum wall-clock seconds a single autonomous goal dispatch may run before
    /// the autonomy loop times out and reverts the goal to `approved`.
    #[serde(default = "default_goal_timeout_secs")]
    pub goal_timeout_secs: u64,

    /// Enable drift-driven goal proposal (auto-propose goals when health components churn).
    #[serde(default)]
    pub self_trigger_enabled: bool,

    /// How often the autonomy loop checks for drift (seconds).
    #[serde(default = "default_self_trigger_interval_secs")]
    pub self_trigger_check_interval_secs: u64,

    /// Minimum component restart count required to trigger a recurring-failure goal.
    #[serde(default = "default_self_trigger_restart_threshold")]
    pub self_trigger_restart_threshold: u32,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            level: "supervised".to_string(),
            workspace_only: true,
            allowed_commands: vec![
                "git".into(),
                "npm".into(),
                "cargo".into(),
                "ls".into(),
                "cat".into(),
                "grep".into(),
                "find".into(),
                "echo".into(),
                "pwd".into(),
                "wc".into(),
                "head".into(),
                "tail".into(),
            ],
            forbidden_paths: vec![
                "/etc".into(),
                "/root".into(),
                "/home".into(),
                "/usr".into(),
                "/bin".into(),
                "/sbin".into(),
                "/lib".into(),
                "/opt".into(),
                "/boot".into(),
                "/dev".into(),
                "/proc".into(),
                "/sys".into(),
                "/var".into(),
                "/tmp".into(),
                "~/.ssh".into(),
                "~/.gnupg".into(),
                "~/.aws".into(),
                "~/.config".into(),
            ],
            max_actions_per_hour: 20,
            max_cost_per_day_cents: 500,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            auto_approve: default_auto_approve(),
            always_ask: default_always_ask(),
            loop_enabled: false,
            loop_poll_secs: default_autonomy_loop_poll_secs(),
            goal_timeout_secs: default_goal_timeout_secs(),
            self_trigger_enabled: false,
            self_trigger_check_interval_secs: default_self_trigger_interval_secs(),
            self_trigger_restart_threshold: default_self_trigger_restart_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct WasmRuntimeConfig {
    #[serde(default = "default_wasm_memory_limit_mb")]
    pub memory_limit_mb: u64,

    #[serde(default = "default_wasm_fuel_limit")]
    pub fuel_limit: u64,

    #[serde(default = "default_wasm_tools_dir")]
    pub tools_dir: String,

    #[serde(default)]
    pub allow_workspace_read: bool,

    #[serde(default)]
    pub allow_workspace_write: bool,

    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for WasmRuntimeConfig {
    fn default() -> Self {
        Self {
            memory_limit_mb: default_wasm_memory_limit_mb(),
            fuel_limit: default_wasm_fuel_limit(),
            tools_dir: default_wasm_tools_dir(),
            allow_workspace_read: false,
            allow_workspace_write: false,
            allowed_hosts: Vec::new(),
        }
    }
}

// ── Cron ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CronConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_run_history")]
    pub max_run_history: u32,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_run_history: default_max_run_history(),
        }
    }
}

/// Configuration for voice message handling (STT + TTS) on Telegram.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct VoiceConfig {
    /// Master switch — when false, voice messages are silently ignored.
    #[serde(default)]
    pub enabled: bool,
    /// OpenAI-compatible API key for Whisper/TTS endpoints.
    pub api_key: Option<String>,
    /// Base URL for the STT/TTS API (OpenAI-compatible).
    #[serde(default = "default_voice_api_base_url")]
    pub api_base_url: String,
    /// Model used for speech-to-text transcription.
    #[serde(default = "default_stt_model")]
    pub stt_model: String,
    /// Model used for text-to-speech synthesis.
    #[serde(default = "default_tts_model")]
    pub tts_model: String,
    /// Voice identifier for TTS output.
    #[serde(default = "default_tts_voice")]
    pub tts_voice: String,
    /// When true, send a voice reply alongside the text response.
    #[serde(default = "default_respond_with_voice")]
    pub respond_with_voice: bool,
    /// ISO-639-1 language hint for Whisper (e.g. "en", "es").
    pub language: Option<String>,
    /// Maximum voice message duration in seconds. Longer messages are rejected.
    #[serde(default = "default_max_voice_duration_secs")]
    pub max_duration_secs: u64,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: None,
            api_base_url: default_voice_api_base_url(),
            stt_model: default_stt_model(),
            tts_model: default_tts_model(),
            tts_voice: default_tts_voice(),
            respond_with_voice: default_respond_with_voice(),
            language: None,
            max_duration_secs: default_max_voice_duration_secs(),
        }
    }
}

/// Resource limits for command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ResourceLimitsConfig {
    /// Maximum memory in MB per command
    #[serde(default = "default_max_memory_mb")]
    pub max_memory_mb: u32,

    /// Maximum CPU time in seconds per command
    #[serde(default = "default_max_cpu_time_seconds")]
    pub max_cpu_time_seconds: u64,

    /// Maximum number of subprocesses
    #[serde(default = "default_max_subprocesses")]
    pub max_subprocesses: u32,

    /// Enable memory monitoring
    #[serde(default = "default_memory_monitoring_enabled")]
    pub memory_monitoring: bool,
}

impl Default for ResourceLimitsConfig {
    fn default() -> Self {
        Self {
            max_memory_mb: default_max_memory_mb(),
            max_cpu_time_seconds: default_max_cpu_time_seconds(),
            max_subprocesses: default_max_subprocesses(),
            memory_monitoring: default_memory_monitoring_enabled(),
        }
    }
}

// ── Cosmic Brain Config ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct CosmicBrainConfig {
    pub enabled: bool,
    pub graph_max_nodes: usize,
    pub graph_prune_threshold: usize,
    pub spreading_activation_decay: f32,
    pub spreading_activation_max_hops: u32,
    pub free_energy_capacity: usize,
    pub free_energy_update_threshold: f64,
    pub free_energy_act_threshold: f64,
    pub integration_tick_secs: u32,
    pub persistence_dir: String,
    pub multi_agent_pool_size: usize,
    pub policy_conflict_resolution: String,
    pub counterfactual_max_scenarios: usize,
    pub consolidation_interval_secs: u32,
    pub drift_window_size: usize,
    pub drift_threshold: f64,
    pub thalamus_threshold: f64,
    pub workspace_max_active: usize,
}

impl Default for CosmicBrainConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            graph_max_nodes: 10000,
            graph_prune_threshold: 8000,
            spreading_activation_decay: 0.7,
            spreading_activation_max_hops: 4,
            free_energy_capacity: 1000,
            free_energy_update_threshold: 0.3,
            free_energy_act_threshold: 0.5,
            integration_tick_secs: 60,
            persistence_dir: "data/cosmic".to_string(),
            multi_agent_pool_size: 4,
            policy_conflict_resolution: "highest_layer".to_string(),
            counterfactual_max_scenarios: 10,
            consolidation_interval_secs: 3600,
            drift_window_size: 50,
            drift_threshold: 0.1,
            thalamus_threshold: 0.3,
            workspace_max_active: 5,
        }
    }
}

// ── Consciousness Config ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct ConsciousnessConfig {
    pub enabled: bool,
    pub debate_rounds: usize,
    pub approval_threshold: f64,
    pub bus_capacity: usize,
    pub sync_url: Option<String>,
    pub max_discourse_depth: usize,
}

impl Default for ConsciousnessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debate_rounds: 3,
            approval_threshold: 0.65,
            bus_capacity: 256,
            sync_url: None,
            max_discourse_depth: 5,
        }
    }
}

impl ConsciousnessConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.debate_rounds < 1 || self.debate_rounds > 10 {
            anyhow::bail!(
                "consciousness.debate_rounds must be 1-10, got {}",
                self.debate_rounds
            );
        }
        if !(0.5..=1.0).contains(&self.approval_threshold) {
            anyhow::bail!(
                "consciousness.approval_threshold must be 0.5-1.0, got {}",
                self.approval_threshold
            );
        }
        if self.bus_capacity == 0 {
            anyhow::bail!(
                "consciousness.bus_capacity must be > 0, got {}",
                self.bus_capacity
            );
        }
        if self.max_discourse_depth < 1 || self.max_discourse_depth > 20 {
            anyhow::bail!(
                "consciousness.max_discourse_depth must be 1-20, got {}",
                self.max_discourse_depth
            );
        }
        Ok(())
    }
}

// ── Cognitive Config ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct CognitiveConfig {
    pub enabled: bool,
    pub persistence_path: String,
    pub save_interval: u64,
}

impl Default for CognitiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            persistence_path: "data/cognitive".to_string(),
            save_interval: 10,
        }
    }
}

// ── Life Config ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct LifeConfig {
    pub enabled: bool,
    pub tick_interval_secs: u32,
    pub curiosity_initiative_threshold: f32,
    pub silence_initiative_hours: u32,
    pub initiative_cooldown_minutes: u32,
    pub dream_idle_hours: u32,
    pub emotional_persistence_path: String,
    #[serde(default)]
    pub initiative_model: Option<String>,
    #[serde(default)]
    pub preferred_channel: Option<String>,
}

impl Default for LifeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_secs: 30,
            curiosity_initiative_threshold: 0.75,
            silence_initiative_hours: 8,
            initiative_cooldown_minutes: 30,
            dream_idle_hours: 4,
            emotional_persistence_path: "data/emotional_state.json".to_string(),
            initiative_model: None,
            preferred_channel: None,
        }
    }
}

// ── Conscience Config ───────────────────────────────────────────

/// Pre-action ethical/normative gate (PR-2 wiring).
///
/// When `gate_enabled = true`, every LLM-issued tool call is run
/// through `crate::conscience::evaluate_tool_call` before dispatch.
/// Any verdict other than `Allow` blocks execution and surfaces a
/// gate-specific error to the model. Default is OFF — gate is opt-in
/// for one release, then default-on after a soak window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct ConscienceConfig {
    pub gate_enabled: bool,
    pub allow_threshold: f64,
    pub ask_threshold: f64,
    pub block_threshold: f64,
    /// Normative rules consulted by the gate before dispatching a tool call.
    /// Each entry is a serialised `NormConfig` (name + action + condition +
    /// severity). The defaults forbid the most obviously dangerous shell
    /// patterns (`rm -rf`, `drop table`) so a freshly-enabled gate has a
    /// non-empty rulebook on day zero.
    ///
    /// Sourced from a static default — `default_conscience_norms` —
    /// rather than `Default::default()` so the list survives a
    /// deserialise + reserialise round-trip.
    #[serde(default = "default_conscience_norms")]
    pub default_norms: Vec<NormConfigSerde>,
}

/// Plain-data mirror of `crate::conscience::types::NormConfig`, declared here
/// so `ConscienceConfig` (in `zeroclaw-config`) can carry serialised norms
/// without depending on the X0 fork's binary-side conscience module.
///
/// Field shape kept aligned with `NormConfig`; the binary's gate adapter
/// copies values across at startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct NormConfigSerde {
    pub name: String,
    pub action: NormActionSerde,
    pub condition: String,
    pub severity: f64,
}

/// Plain-data mirror of `crate::conscience::types::NormAction`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum NormActionSerde {
    Allow,
    Forbid,
    Require,
}

/// Ship a non-empty default rulebook so the gate has bite the moment
/// `gate_enabled` flips to `true`. Operators override by setting
/// `[conscience.default_norms]` in `config.toml`.
pub fn default_conscience_norms() -> Vec<NormConfigSerde> {
    vec![
        NormConfigSerde {
            name: "no_rm_rf_root".into(),
            action: NormActionSerde::Forbid,
            condition: "rm -rf /".into(),
            severity: 0.99,
        },
        NormConfigSerde {
            name: "no_rm_rf_home".into(),
            action: NormActionSerde::Forbid,
            condition: "rm -rf ~".into(),
            severity: 0.95,
        },
        NormConfigSerde {
            name: "no_drop_table".into(),
            action: NormActionSerde::Forbid,
            condition: "drop table".into(),
            severity: 0.95,
        },
        NormConfigSerde {
            name: "no_curl_pipe_sh".into(),
            action: NormActionSerde::Forbid,
            condition: "curl | sh".into(),
            severity: 0.85,
        },
    ]
}

impl Default for ConscienceConfig {
    fn default() -> Self {
        // Mirrors crate::conscience::types::Thresholds::default().
        Self {
            gate_enabled: false,
            allow_threshold: 0.80,
            ask_threshold: 0.55,
            block_threshold: 0.45,
            default_norms: default_conscience_norms(),
        }
    }
}

impl ConscienceConfig {
    /// Read-only validation: returns `Err` when thresholds are out of bounds
    /// or in the wrong order. The companion [`Self::normalize`] silently
    /// clamps and reorders instead — call `normalize` on the in-memory
    /// config before serialising, and call `validate` on a loaded config
    /// to surface misconfigurations the user wrote on disk.
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.allow_threshold) {
            return Err(format!(
                "conscience.allow_threshold must be in [0,1], got {}",
                self.allow_threshold
            ));
        }
        if !(0.0..=1.0).contains(&self.ask_threshold) {
            return Err(format!(
                "conscience.ask_threshold must be in [0,1], got {}",
                self.ask_threshold
            ));
        }
        if !(0.0..=1.0).contains(&self.block_threshold) {
            return Err(format!(
                "conscience.block_threshold must be in [0,1], got {}",
                self.block_threshold
            ));
        }
        if self.block_threshold > self.ask_threshold {
            return Err(format!(
                "conscience.block_threshold ({}) must be <= ask_threshold ({})",
                self.block_threshold, self.ask_threshold
            ));
        }
        if self.ask_threshold > self.allow_threshold {
            return Err(format!(
                "conscience.ask_threshold ({}) must be <= allow_threshold ({})",
                self.ask_threshold, self.allow_threshold
            ));
        }
        Ok(())
    }

    /// Clamp threshold values to `[0.0, 1.0]` and enforce
    /// `block_threshold <= ask_threshold <= allow_threshold`.
    ///
    /// Misordered thresholds would otherwise produce verdicts that contradict
    /// the gate's intent (e.g. a score above the block threshold but below the
    /// ask threshold). Returns the count of fields that were adjusted, useful
    /// for caller-side logging.
    ///
    /// Implements Step 5 of `Plans/glimmering-mixing-moore.md` (ISC-C5).
    pub fn normalize(&mut self) -> usize {
        let mut adjusted = 0_usize;

        let clamp = |v: &mut f64, was_adjusted: &mut usize| {
            let new = v.clamp(0.0, 1.0);
            if (new - *v).abs() > f64::EPSILON {
                *was_adjusted += 1;
            }
            *v = new;
        };
        clamp(&mut self.allow_threshold, &mut adjusted);
        clamp(&mut self.ask_threshold, &mut adjusted);
        clamp(&mut self.block_threshold, &mut adjusted);

        // Enforce monotonicity: block <= ask <= allow. Push down rather than up
        // so the more permissive thresholds stay where the operator set them.
        if self.ask_threshold > self.allow_threshold {
            self.ask_threshold = self.allow_threshold;
            adjusted += 1;
        }
        if self.block_threshold > self.ask_threshold {
            self.block_threshold = self.ask_threshold;
            adjusted += 1;
        }
        adjusted
    }
}

#[cfg(test)]
mod conscience_config_tests {
    use super::ConscienceConfig;

    #[test]
    fn validate_clamps_out_of_range_thresholds() {
        let mut cc = ConscienceConfig {
            gate_enabled: true,
            allow_threshold: 1.5,
            ask_threshold: -0.3,
            block_threshold: 0.4,
            default_norms: Vec::new(),
        };
        let adjusted = cc.normalize();
        assert_eq!(cc.allow_threshold, 1.0);
        assert_eq!(cc.ask_threshold, 0.0);
        assert_eq!(cc.block_threshold, 0.0);
        assert!(adjusted >= 2, "at least allow + ask were adjusted");
    }

    #[test]
    fn validate_enforces_block_ask_allow_monotonicity() {
        let mut cc = ConscienceConfig {
            gate_enabled: true,
            allow_threshold: 0.4,
            ask_threshold: 0.6,
            block_threshold: 0.8,
            default_norms: Vec::new(),
        };
        let adjusted = cc.normalize();
        assert_eq!(cc.allow_threshold, 0.4);
        assert_eq!(cc.ask_threshold, 0.4);
        assert_eq!(cc.block_threshold, 0.4);
        assert_eq!(adjusted, 2);
    }

    #[test]
    fn validate_is_noop_for_well_formed_config() {
        let mut cc = ConscienceConfig::default();
        let adjusted = cc.normalize();
        assert_eq!(adjusted, 0);
        assert_eq!(cc.allow_threshold, 0.80);
        assert_eq!(cc.ask_threshold, 0.55);
        assert_eq!(cc.block_threshold, 0.45);
    }

    #[test]
    fn default_norms_ship_a_non_empty_rulebook() {
        let cc = ConscienceConfig::default();
        assert!(
            !cc.default_norms.is_empty(),
            "freshly-enabled gate must have norms"
        );
        // The names below are part of the public default contract — bumping
        // them is a behaviour change operators will see in their generated
        // configs, so it should be deliberate.
        let names: Vec<&str> = cc
            .default_norms
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        for required in [
            "no_rm_rf_root",
            "no_rm_rf_home",
            "no_drop_table",
            "no_curl_pipe_sh",
        ] {
            assert!(
                names.contains(&required),
                "default norms must include {required}"
            );
        }
    }

    #[test]
    fn default_norms_severities_are_in_unit_interval() {
        for norm in ConscienceConfig::default().default_norms {
            assert!(
                (0.0..=1.0).contains(&norm.severity),
                "norm {} has severity {} outside [0,1]",
                norm.name,
                norm.severity,
            );
        }
    }

    #[test]
    fn default_norms_survive_toml_roundtrip() {
        let cc = ConscienceConfig::default();
        let toml = toml::to_string(&cc).expect("serialise");
        let parsed: ConscienceConfig = toml::from_str(&toml).expect("deserialise");
        assert_eq!(parsed.default_norms.len(), cc.default_norms.len());
        for (a, b) in parsed.default_norms.iter().zip(cc.default_norms.iter()) {
            assert_eq!(a, b, "norm round-trip drift");
        }
    }

    #[test]
    fn validate_handles_compound_violation_clamp_then_reorder() {
        let mut cc = ConscienceConfig {
            gate_enabled: true,
            allow_threshold: 2.0,
            ask_threshold: 1.5,
            block_threshold: 1.2,
            default_norms: Vec::new(),
        };
        cc.normalize();
        // All three clamp to 1.0, monotonicity already holds afterwards.
        assert_eq!(cc.allow_threshold, 1.0);
        assert_eq!(cc.ask_threshold, 1.0);
        assert_eq!(cc.block_threshold, 1.0);
    }
}

// ── TaskQueue Config ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct TaskQueueConfig {
    pub enabled: bool,
    pub poll_interval_secs: u64,
    pub max_concurrent: u32,
}

impl Default for TaskQueueConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: 30,
            max_concurrent: 1,
        }
    }
}

// ── SCE Config ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct SceConfig {
    pub enabled: bool,
    pub tick_interval_secs: u64,
}

impl Default for SceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_secs: 60,
        }
    }
}

// ── NVIDIA ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct NvidiaConfig {
    pub triton_url: Option<String>,
    pub tensorrt_url: Option<String>,
    pub enable_gpu_metrics: bool,
}

// --- Helper functions for serde defaults ---

fn default_max_depth() -> u32 {
    3
}

fn default_bot_max_memory_mb() -> u64 {
    512
}

fn default_bot_max_concurrent_requests() -> u32 {
    10
}

fn default_bot_max_tokens_per_minute() -> u64 {
    100_000
}

fn default_baud_rate() -> u32 {
    115_200
}

fn default_agent_max_tool_iterations() -> usize {
    10
}

fn default_agent_max_history_messages() -> usize {
    50
}

fn default_agent_tool_dispatcher() -> String {
    "auto".into()
}

fn default_identity_format() -> String {
    "openclaw".into()
}

fn default_working_budget() -> usize {
    4000
}

fn default_episodic_budget() -> usize {
    2000
}

fn default_semantic_budget() -> usize {
    2000
}

fn default_procedural_budget() -> usize {
    1000
}

fn default_soul_format() -> String {
    "soul/v1".into()
}

fn default_soul_path() -> String {
    "SOUL.md".into()
}

fn default_reflection_interval() -> usize {
    50
}

fn default_max_children() -> usize {
    3
}

fn default_child_workspace_dir() -> String {
    "children".into()
}

fn default_initial_credit_cents() -> i64 {
    1000
}

fn default_survival_high() -> i64 {
    500
}

fn default_survival_normal() -> i64 {
    50
}

fn default_survival_low_compute() -> i64 {
    10
}

fn default_wallet_path() -> String {
    "wallet".to_string()
}

fn default_wallet_auto_generate() -> bool {
    true
}

fn default_wallet_chain_id() -> u64 {
    11_155_111
}

fn default_max_x402_payment_cents() -> u64 {
    100
}

fn default_max_daily_x402_spend_cents() -> u64 {
    500
}

fn default_max_monthly_x402_spend_cents() -> u64 {
    5000
}

fn default_daily_limit() -> f64 {
    10.0
}

fn default_monthly_limit() -> f64 {
    100.0
}

fn default_warn_percent() -> u8 {
    80
}

fn default_peripheral_transport() -> String {
    "serial".into()
}

fn default_peripheral_baud() -> u32 {
    115_200
}

fn default_gateway_port() -> u16 {
    3000
}

fn default_gateway_host() -> String {
    "127.0.0.1".into()
}

fn default_pair_rate_limit() -> u32 {
    10
}

fn default_webhook_rate_limit() -> u32 {
    60
}

fn default_idempotency_ttl_secs() -> u64 {
    300
}

fn default_gateway_rate_limit_max_keys() -> usize {
    10_000
}

fn default_gateway_idempotency_max_keys() -> usize {
    10_000
}

fn default_true() -> bool {
    true
}

fn default_entity_id() -> String {
    "default".into()
}

fn default_browser_computer_use_endpoint() -> String {
    "http://127.0.0.1:8787/v1/actions".into()
}

fn default_browser_computer_use_timeout_ms() -> u64 {
    15_000
}

fn default_browser_backend() -> String {
    "agent_browser".into()
}

fn default_browser_webdriver_url() -> String {
    "http://127.0.0.1:9515".into()
}

fn default_http_max_response_size() -> usize {
    1_000_000 // 1MB
}

fn default_http_timeout_secs() -> u64 {
    30
}

fn default_web_search_provider() -> String {
    "duckduckgo".into()
}

fn default_web_search_max_results() -> usize {
    5
}

fn default_web_search_timeout_secs() -> u64 {
    15
}

fn default_storage_schema() -> String {
    "public".into()
}

fn default_storage_table() -> String {
    "memories".into()
}

fn default_embedding_provider() -> String {
    "none".into()
}

fn default_hygiene_enabled() -> bool {
    true
}

fn default_archive_after_days() -> u32 {
    7
}

fn default_purge_after_days() -> u32 {
    30
}

fn default_conversation_retention_days() -> u32 {
    30
}

fn default_embedding_model() -> String {
    "text-embedding-3-small".into()
}

fn default_embedding_dims() -> usize {
    1536
}

fn default_vector_weight() -> f64 {
    0.7
}

fn default_keyword_weight() -> f64 {
    0.3
}

fn default_min_relevance_score() -> f64 {
    0.4
}

fn default_cache_size() -> usize {
    10_000
}

fn default_chunk_size() -> usize {
    512
}

fn default_response_cache_ttl() -> u32 {
    60
}

fn default_response_cache_max() -> usize {
    5_000
}

fn default_mmr_lambda() -> f64 {
    0.7
}

fn default_auto_approve() -> Vec<String> {
    vec!["file_read".into(), "memory_recall".into()]
}

fn default_always_ask() -> Vec<String> {
    vec![]
}

fn default_autonomy_loop_poll_secs() -> u64 {
    60
}

fn default_goal_timeout_secs() -> u64 {
    600
}

fn default_self_trigger_interval_secs() -> u64 {
    300
}

fn default_self_trigger_restart_threshold() -> u32 {
    5
}

fn default_runtime_kind() -> String {
    "native".into()
}

fn default_docker_image() -> String {
    "alpine:3.20".into()
}

fn default_docker_network() -> String {
    "none".into()
}

fn default_docker_memory_limit_mb() -> Option<u64> {
    Some(512)
}

fn default_docker_cpu_limit() -> Option<f64> {
    Some(1.0)
}

fn default_wasm_memory_limit_mb() -> u64 {
    64
}

fn default_wasm_fuel_limit() -> u64 {
    1_000_000
}

fn default_wasm_tools_dir() -> String {
    "tools/wasm".into()
}

fn default_provider_retries() -> u32 {
    2
}

fn default_provider_backoff_ms() -> u64 {
    500
}

fn default_channel_backoff_secs() -> u64 {
    2
}

fn default_channel_backoff_max_secs() -> u64 {
    60
}

fn default_scheduler_poll_secs() -> u64 {
    15
}

fn default_scheduler_retries() -> u32 {
    2
}

fn default_restart_budget() -> u32 {
    10
}

fn default_restart_budget_window_secs() -> u64 {
    300
}

fn default_scheduler_enabled() -> bool {
    true
}

fn default_scheduler_max_tasks() -> usize {
    64
}

fn default_scheduler_max_concurrent() -> usize {
    4
}

fn default_max_run_history() -> u32 {
    50
}

fn default_draft_update_interval_ms() -> u64 {
    1000
}

fn default_voice_api_base_url() -> String {
    "https://api.openai.com/v1".into()
}

fn default_stt_model() -> String {
    "whisper-1".into()
}

fn default_tts_model() -> String {
    "tts-1".into()
}

fn default_tts_voice() -> String {
    "alloy".into()
}

fn default_respond_with_voice() -> bool {
    true
}

fn default_max_voice_duration_secs() -> u64 {
    120
}

fn default_irc_port() -> u16 {
    6697
}

fn default_max_memory_mb() -> u32 {
    512
}

fn default_max_cpu_time_seconds() -> u64 {
    60
}

fn default_max_subprocesses() -> u32 {
    10
}

fn default_memory_monitoring_enabled() -> bool {
    true
}

fn default_audit_enabled() -> bool {
    true
}

fn default_audit_log_path() -> String {
    "audit.log".to_string()
}

fn default_audit_max_size_mb() -> u32 {
    100
}

fn default_config_and_workspace_dirs() -> anyhow::Result<(PathBuf, PathBuf)> {
    let config_dir = default_config_dir()?;
    Ok((config_dir.clone(), config_dir.join("workspace")))
}

fn default_config_dir() -> anyhow::Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u: directories::UserDirs| u.home_dir().to_path_buf())
        .ok_or_else(|| anyhow::Error::msg("Could not find home directory"))?;
    Ok(home.join(".zeroclaw"))
}

// --- HasPropKind implementations for X0 config types ---
// These are required by the Configurable derive macro on Config.

macro_rules! impl_x0_prop_kind {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl crate::traits::HasPropKind for $ty {
                const PROP_KIND: crate::traits::PropKind = crate::traits::PropKind::Object;
            }
        )+
    };
}

impl_x0_prop_kind!(
    AgentConfig,
    AutonomyConfig,
    BotConfig,
    CognitiveConfig,
    ConscienceConfig,
    ConsciousnessConfig,
    CosmicBrainConfig,
    CronConfig,
    DelegateAgentConfig,
    LifeConfig,
    MemoryTokenBudgetsConfig,
    ModelPricing,
    ModelStrategyConfig,
    NvidiaConfig,
    ReplicationConfig,
    ResourceLimitsConfig,
    SceConfig,
    SoulConfig,
    StorageProviderConfig,
    StorageProviderSection,
    SurvivalThresholdsConfig,
    TaskQueueConfig,
    TierModelConfig,
    TreasuryConfig,
    VoiceConfig,
    WalletConfig,
    WasmRuntimeConfig,
);

impl crate::traits::HasPropKind for Vec<BotConfig> {
    const PROP_KIND: crate::traits::PropKind = crate::traits::PropKind::ObjectArray;
}
