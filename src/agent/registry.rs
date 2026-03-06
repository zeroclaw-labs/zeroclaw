//! Agent Registry — discovers and manages agent definitions from YAML files.
//!
//! The registry provides:
//! - Automatic discovery of agent definition files from `agents/` directories
//! - YAML file parsing and validation
//! - Hot reload support for configuration changes
//! - Thread-safe access to agent definitions
//!
//! ## File Format
//!
//! Agent definitions can be stored in two YAML formats:
//!
//! ### 1. Standard Format (nested)
//!
//! ```yaml
//! # agents/researcher.yaml
//! agent:
//!   id: "researcher"
//!   name: "Research Agent"
//!   version: "1.0.0"
//!   description: "Conducts research on given topics"
//!
//! execution:
//!   mode: "subprocess"
//!   command: "zeroclaw"
//!   args: ["agent", "run", "--agent-id", "{agent_id}"]
//!
//! provider:
//!   name: "openrouter"
//!   model: "anthropic/claude-sonnet-4-6"
//!
//! tools:
//!   - name: "web_search"
//!     enabled: true
//!   - name: "memory_read"
//!     enabled: true
//!
//! system:
//!   prompt: "You are a research agent..."
//! ```
//!
//! ### 2. Simplified Format (flat)
//!
//! ```yaml
//! # agents/researcher.agent.yaml
//! name: researcher
//! version: 1.0.0
//! description: Research-focused agent
//! provider:
//!   name: anthropic
//!   model: claude-sonnet-4-6
//! system_prompt: |
//!   You are a Research Agent...
//! config:
//!   agentic: true
//!   allowed_tools:
//!     - browser
//!     - web_search
//! ```

use crate::config::schema::DelegateAgentConfig;
use crate::security::SecurityPolicy;
use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

/// Agent definition loaded from YAML file
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentDefinition {
    /// Agent metadata
    pub agent: AgentMetadata,

    /// Execution configuration
    #[serde(default)]
    pub execution: AgentExecution,

    /// Provider configuration (optional overrides)
    #[serde(default)]
    pub provider: AgentProvider,

    /// Tools configuration
    #[serde(default)]
    pub tools: AgentTools,

    /// System prompt
    #[serde(default)]
    pub system: AgentSystem,

    /// Memory configuration
    #[serde(default)]
    pub memory: AgentMemory,

    /// Reporting configuration
    #[serde(default)]
    pub reporting: AgentReporting,

    /// Retry configuration
    #[serde(default)]
    pub retry: AgentRetry,

    /// Agentic mode flag (from flat format, ignored in full format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agentic: Option<bool>,

    /// Max delegation depth (from flat format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,

    /// Max tool iterations (from flat format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,

    /// Team membership (optional, can be specified in agent file)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<AgentTeamMembership>,
}

/// Agent metadata
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentMetadata {
    /// Unique agent identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Semantic version
    #[serde(default = "default_version")]
    pub version: String,

    /// Description of agent's purpose
    pub description: String,
}

/// Execution configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentExecution {
    /// Execution mode
    #[serde(default = "default_execution_mode")]
    pub mode: ExecutionMode,

    /// Command template
    #[serde(default)]
    pub command: Option<String>,

    /// Command arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Working directory template
    #[serde(default)]
    pub working_dir: Option<String>,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Execution mode
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Spawn as subprocess
    Subprocess,
    /// Run as WebAssembly module
    Wasm,
    /// Run in Docker container
    Docker,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        Self::Subprocess
    }
}

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentProvider {
    /// Provider name
    #[serde(default)]
    pub name: Option<String>,

    /// Model identifier
    #[serde(default)]
    pub model: Option<String>,

    /// API key override (null = inherit)
    #[serde(default)]
    pub api_key: Option<String>,

    /// Temperature
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Max tokens
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

/// Tools configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentTools {
    /// Allowed tools
    #[serde(default)]
    pub tools: Vec<AgentToolConfig>,

    /// Denied tools
    #[serde(default)]
    pub deny: Vec<AgentToolDeny>,
}

/// Tool configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolConfig {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
}

/// Tool denial with reason
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolDeny {
    pub name: String,
    pub reason: String,
}

/// System prompt configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentSystem {
    #[serde(default)]
    pub prompt: String,
}

/// Memory configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentMemory {
    #[serde(default = "default_memory_backend")]
    pub backend: MemoryBackend,
    #[serde(default)]
    pub category: Option<String>,
}

/// Memory backend type
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBackend {
    /// Share main agent's memory
    Shared,
    /// Isolated memory per agent
    Isolated,
    /// No memory
    None,
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::Shared
    }
}

/// Reporting configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentReporting {
    /// Reporting mode
    #[serde(default = "default_reporting_mode")]
    pub mode: ReportingMode,

    /// Output format
    #[serde(default = "default_output_format")]
    pub format: OutputFormat,

    /// Execution timeout
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

impl Default for AgentReporting {
    fn default() -> Self {
        Self {
            mode: default_reporting_mode(),
            format: default_output_format(),
            timeout_seconds: default_timeout(),
        }
    }
}

/// Reporting mode
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportingMode {
    /// Report via stdout
    Stdout,
    /// Write to result file
    File,
    /// Inter-process communication
    Ipc,
    /// HTTP callback
    Http,
}

impl Default for ReportingMode {
    fn default() -> Self {
        Self::Ipc
    }
}

/// Output format
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Json,
    Markdown,
    Both,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Json
    }
}

/// Retry configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentRetry {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,

    #[serde(default = "default_backoff")]
    pub backoff_ms: u64,
}

impl Default for AgentRetry {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff_ms: default_backoff(),
        }
    }
}

/// Team membership information for an agent.
///
/// This lightweight struct indicates which team(s) an agent belongs to
/// and what role they play in that team. An agent can belong to multiple
/// teams through multiple AgentTeamMembership entries.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentTeamMembership {
    /// Team identifier this agent belongs to
    pub team_id: String,

    /// Agent's role within the team
    #[serde(default)]
    pub role: TeamMembershipRole,

    /// Priority within the team (lower = higher priority)
    #[serde(default)]
    pub priority: u32,
}

/// Agent role within a team context.
///
/// Simplified role enum for agent team membership.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TeamMembershipRole {
    /// Standard worker agent
    #[default]
    Worker,
    /// Lead/coordination agent
    Lead,
    /// Specialist agent with domain expertise
    Specialist,
}

impl TeamMembershipRole {
    /// Returns the string representation
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Lead => "lead",
            Self::Specialist => "specialist",
        }
    }
}

impl std::fmt::Display for TeamMembershipRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// Default functions

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_execution_mode() -> ExecutionMode {
    ExecutionMode::Subprocess
}

fn default_memory_backend() -> MemoryBackend {
    MemoryBackend::Shared
}

fn default_reporting_mode() -> ReportingMode {
    ReportingMode::Ipc
}

fn default_output_format() -> OutputFormat {
    OutputFormat::Json
}

fn default_timeout() -> u64 {
    300
}

fn default_max_attempts() -> u32 {
    3
}

fn default_backoff() -> u64 {
    1000
}

// Default functions for FlatConfig
fn default_flat_max_depth() -> u32 {
    3
}

fn default_flat_max_tool_iterations() -> usize {
    10
}

// ── Simplified Agent File Format ────────────────────────────────────────

/// Simplified agent file format (flat structure)
/// This matches the format used in config/agents/*.agent.yaml
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct FlatAgentFile {
    /// Agent name (used as ID if id not specified)
    #[serde(default)]
    pub name: String,

    /// Semantic version
    #[serde(default = "default_version")]
    pub version: String,

    /// Description of agent's purpose
    pub description: String,

    /// Provider configuration
    #[serde(default)]
    pub provider: FlatProvider,

    /// System prompt
    #[serde(default)]
    pub system_prompt: String,

    /// Agent configuration
    #[serde(default)]
    pub config: FlatConfig,

    /// Metadata (author, tags, categories)
    #[serde(default)]
    pub metadata: FlatMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
struct FlatProvider {
    /// Provider name
    #[serde(default)]
    pub name: Option<String>,

    /// Model identifier
    #[serde(default)]
    pub model: Option<String>,

    /// API key environment variable
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Temperature
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
struct FlatConfig {
    /// Temperature
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Max recursion depth
    #[serde(default = "default_flat_max_depth")]
    pub max_depth: u32,

    /// Enable agentic mode
    #[serde(default)]
    pub agentic: bool,

    /// Allowed tools
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Max iterations
    #[serde(default = "default_flat_max_tool_iterations")]
    pub max_iterations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
struct FlatMetadata {
    /// Author
    #[serde(default)]
    pub author: Option<String>,

    /// Tags
    #[serde(default)]
    pub tags: Vec<String>,

    /// Categories
    #[serde(default)]
    pub categories: Vec<String>,
}

impl TryFrom<FlatAgentFile> for AgentDefinition {
    type Error = anyhow::Error;

    fn try_from(flat: FlatAgentFile) -> Result<Self> {
        let id = flat.name.clone();
        Ok(Self {
            agent: AgentMetadata {
                id: id.clone(),
                name: flat.name.clone(),
                version: flat.version,
                description: flat.description,
            },
            execution: AgentExecution {
                mode: ExecutionMode::Subprocess,
                command: Some("zeroclaw".to_string()),
                args: vec![],
                working_dir: None,
                env: HashMap::new(),
            },
            provider: AgentProvider {
                name: flat.provider.name.clone(),
                model: flat.provider.model.clone(),
                api_key: None, // API key from env var is handled elsewhere
                temperature: flat.provider.temperature.or(flat.config.temperature),
                max_tokens: None,
            },
            tools: {
                let tools = flat
                    .config
                    .allowed_tools
                    .iter()
                    .map(|name| AgentToolConfig {
                        name: name.clone(),
                        enabled: true,
                    })
                    .collect();
                AgentTools {
                    tools,
                    deny: vec![],
                }
            },
            system: AgentSystem {
                prompt: flat.system_prompt,
            },
            memory: AgentMemory {
                backend: MemoryBackend::Shared,
                category: Some(id.clone()),
            },
            reporting: AgentReporting {
                mode: ReportingMode::Ipc,
                format: OutputFormat::Json,
                timeout_seconds: 300,
            },
            retry: AgentRetry {
                max_attempts: 3,
                backoff_ms: 1000,
            },
            agentic: Some(flat.config.agentic),
            max_depth: Some(flat.config.max_depth),
            max_iterations: Some(flat.config.max_iterations),
            team: None, // Flat format doesn't support team membership
        })
    }
}

impl TryFrom<&FlatAgentFile> for DelegateAgentConfig {
    type Error = anyhow::Error;

    fn try_from(flat: &FlatAgentFile) -> Result<Self> {
        Ok(Self {
            provider: flat
                .provider
                .name
                .clone()
                .unwrap_or_else(|| "openrouter".to_string()),
            model: flat
                .provider
                .model
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            system_prompt: if flat.system_prompt.is_empty() {
                None
            } else {
                Some(flat.system_prompt.clone())
            },
            api_key: None, // API key from env var is handled elsewhere
            enabled: true,
            capabilities: Vec::new(),
            priority: 0,
            temperature: flat.provider.temperature.or(flat.config.temperature),
            max_depth: flat.config.max_depth,
            agentic: flat.config.agentic,
            allowed_tools: flat.config.allowed_tools.clone(),
            max_iterations: flat.config.max_iterations,
        })
    }
}

/// Registry for discovering and managing agent definitions
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    /// Base directory for agent definitions
    agents_dir: PathBuf,

    /// Additional search directories
    search_dirs: Vec<PathBuf>,

    /// Loaded agent definitions
    agents: Arc<RwLock<HashMap<String, AgentDefinition>>>,

    /// Security policy for agent execution
    security: Arc<SecurityPolicy>,
}

impl AgentRegistry {
    /// Create a new registry from the agents directory
    pub fn new(agents_dir: PathBuf, security: Arc<SecurityPolicy>) -> Result<Self> {
        let registry = Self {
            agents_dir,
            search_dirs: Vec::new(),
            agents: Arc::new(RwLock::new(HashMap::new())),
            security,
        };
        Ok(registry)
    }

    /// Create a new registry with additional search directories
    pub fn with_search_dirs(
        agents_dir: PathBuf,
        search_dirs: Vec<PathBuf>,
        security: Arc<SecurityPolicy>,
    ) -> Result<Self> {
        let registry = Self {
            agents_dir,
            search_dirs,
            agents: Arc::new(RwLock::new(HashMap::new())),
            security,
        };
        Ok(registry)
    }

    /// Discover all agent definitions in the agents directories
    pub fn discover(&self) -> Result<usize> {
        let mut count = 0;

        // Search primary agents directory
        if self.agents_dir.exists() {
            count += self.discover_in_dir(&self.agents_dir)?;
        }

        // Search additional directories
        for dir in &self.search_dirs {
            if dir.exists() {
                count += self.discover_in_dir(dir)?;
            }
        }

        info!("Discovered {} agent definitions", count);
        Ok(count)
    }

    /// Discover agent definitions in a specific directory
    fn discover_in_dir(&self, dir: &Path) -> Result<usize> {
        let entries = fs::read_dir(dir)
            .with_context(|| format!("Failed to read agents directory: {}", dir.display()))?;

        let mut count = 0;
        let mut agents = self.agents.write().unwrap();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            // Check for YAML files (.yaml, .yml)
            let ext = path.extension().and_then(|s| s.to_str());
            if ext == Some("yaml") || ext == Some("yml") {
                match self.load_definition(&path) {
                    Ok(def) => {
                        let id = def.agent.id.clone();
                        // Validate before inserting
                        if let Err(e) = self.validate(&def) {
                            warn!(
                                "Skipping invalid agent definition '{}': {}",
                                path.display(),
                                e
                            );
                            continue;
                        }
                        let is_new = !agents.contains_key(&id);
                        agents.insert(id.clone(), def);
                        if is_new {
                            debug!("Loaded agent definition: {}", id);
                            count += 1;
                        } else {
                            debug!("Updated agent definition: {}", id);
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to load agent definition '{}': {}",
                            path.display(),
                            e
                        );
                    }
                }
            }
        }

        Ok(count)
    }

    /// Load a single agent definition from file
    /// Supports both standard nested format and simplified flat format
    pub fn load_definition(&self, path: &Path) -> Result<AgentDefinition> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read agent file {}", path.display()))?;

        // Try standard format first
        if let Ok(def) = serde_yaml::from_str::<AgentDefinition>(&content) {
            return Ok(def);
        }

        // Try simplified flat format
        if let Ok(flat) = serde_yaml::from_str::<FlatAgentFile>(&content) {
            return flat.try_into().with_context(|| {
                format!("Failed to convert agent file '{}' to standard format", path.display())
            });
        }

        bail!(
            "Failed to parse agent file '{}': neither standard nor flat format matched",
            path.display()
        );
    }

    /// Validate an agent definition
    fn validate(&self, def: &AgentDefinition) -> Result<()> {
        // Check ID format
        if def.agent.id.is_empty() {
            bail!("Agent ID cannot be empty");
        }

        if def.agent.id.contains('/') || def.agent.id.contains('\\') {
            bail!(
                "Invalid agent ID '{}': cannot contain path separators",
                def.agent.id
            );
        }

        // Check name
        if def.agent.name.is_empty() {
            bail!("Agent '{}' has empty name", def.agent.id);
        }

        // Check description
        if def.agent.description.is_empty() {
            bail!("Agent '{}' has empty description", def.agent.id);
        }

        // Check command for subprocess mode
        if def.execution.mode == ExecutionMode::Subprocess {
            let command_empty = def.execution.command.as_ref().map_or(true, |c| c.is_empty());
            if command_empty {
                bail!(
                    "Agent '{}' requires 'command' for subprocess mode",
                    def.agent.id
                );
            }
        }

        // Check for conflicting tool permissions
        let allowed: HashSet<_> = def
            .tools
            .tools
            .iter()
            .filter(|t| t.enabled)
            .map(|t| &t.name)
            .collect();

        for denied in &def.tools.deny {
            if allowed.contains(&denied.name) {
                bail!(
                    "Agent '{}' has tool '{}' both allowed and denied",
                    def.agent.id,
                    denied.name
                );
            }
        }

        // Validate timeout
        if def.reporting.timeout_seconds == 0 {
            bail!("Agent '{}' has invalid timeout (must be > 0)", def.agent.id);
        }

        // Validate retry settings
        if def.retry.max_attempts == 0 {
            bail!(
                "Agent '{}' has invalid max_attempts (must be > 0)",
                def.agent.id
            );
        }

        Ok(())
    }

    /// Get an agent definition by ID
    pub fn get(&self, id: &str) -> Option<AgentDefinition> {
        self.agents.read().unwrap().get(id).cloned()
    }

    /// List all available agent IDs
    pub fn list(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.agents.read().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Check if an agent exists
    pub fn contains(&self, id: &str) -> bool {
        self.agents.read().unwrap().contains_key(id)
    }

    /// Get the number of loaded agents
    pub fn count(&self) -> usize {
        self.agents.read().unwrap().len()
    }

    /// Reload all agent definitions (hot reload)
    pub fn reload(&self) -> Result<usize> {
        // Clear existing definitions
        self.agents.write().unwrap().clear();

        // Rediscover
        let count = self.discover()?;

        info!("Reloaded {} agent definitions", count);
        Ok(count)
    }

    /// Add an additional search directory
    pub fn add_search_dir(&mut self, dir: PathBuf) {
        self.search_dirs.push(dir);
    }

    /// Get the agents directory
    pub fn agents_dir(&self) -> &Path {
        &self.agents_dir
    }

    /// Get all agent definitions
    pub fn all(&self) -> HashMap<String, AgentDefinition> {
        self.agents.read().unwrap().clone()
    }

    /// Get all agents as DelegateAgentConfig for use with DelegateTool
    pub fn all_as_delegate_configs(&self) -> HashMap<String, DelegateAgentConfig> {
        self.agents
            .read()
            .unwrap()
            .iter()
            .map(|(id, def)| (id.clone(), def.into()))
            .collect()
    }

    /// Create an AgentRegistry from a workspace directory.
    ///
    /// This is a convenience function for creating a registry that points to
    /// the `agents/` subdirectory of the workspace.
    pub fn from_workspace_dir(workspace_dir: &Path) -> Result<Self> {
        let agents_dir = workspace_dir.join("agents");
        let security = Arc::new(SecurityPolicy::default());
        Self::new(agents_dir, security)
    }

    /// Create and discover agents from a workspace directory.
    ///
    /// This is a convenience function that creates the registry and runs
    /// discovery in one step.
    pub fn from_workspace_and_discover(workspace_dir: &Path) -> Result<(Self, usize)> {
        let registry = Self::from_workspace_dir(workspace_dir)?;
        let count = registry.discover()?;
        Ok((registry, count))
    }

    // ── Team Membership Methods ─────────────────────────────────────────────

    /// Get all agents that are members of a specific team.
    ///
    /// Returns a vector of (agent_id, AgentDefinition) tuples for agents
    /// that are referenced in the team's members list.
    ///
    /// # Arguments
    ///
    /// * `team_member_ids` - Slice of agent IDs that are members of the team
    ///
    /// # Returns
    ///
    /// Vector of tuples containing agent ID and definition
    pub fn get_team_members(
        &self,
        team_member_ids: &[&str],
    ) -> Vec<(String, AgentDefinition)> {
        let agents = self.agents.read().unwrap();
        let mut result = Vec::new();

        for agent_id in team_member_ids {
            if let Some(def) = agents.get(*agent_id) {
                result.push((agent_id.to_string(), def.clone()));
            }
        }

        result
    }

    /// Get agents from a team that have a specific role.
    ///
    /// This requires the TeamDefinition to determine which agents have
    /// which roles. The method filters the team's members by role and
    /// returns the matching agent definitions.
    ///
    /// # Arguments
    ///
    /// * `team_members` - Slice of (agent_id, role) tuples from a TeamDefinition
    ///
    /// # Returns
    ///
    /// Vector of tuples containing agent ID, AgentDefinition, and role
    pub fn get_by_team_role(
        &self,
        team_members: &[(String, crate::agent::team_definition::AgentRole)],
    ) -> Vec<(String, AgentDefinition, crate::agent::team_definition::AgentRole)> {
        let agents = self.agents.read().unwrap();
        let mut result = Vec::new();

        for (agent_id, role) in team_members {
            if let Some(def) = agents.get(agent_id) {
                result.push((agent_id.clone(), def.clone(), *role));
            }
        }

        result
    }

    /// Get agents from multiple teams by their member IDs.
    ///
    /// Useful for collecting all agents that participate in any of the
    /// specified teams.
    ///
    /// # Arguments
    ///
    /// * `teams` - HashMap of team_id -> vector of member agent IDs
    ///
    /// # Returns
    ///
    /// HashMap mapping team_id to vector of (agent_id, AgentDefinition) tuples
    pub fn get_team_agents(
        &self,
        teams: &HashMap<String, Vec<String>>,
    ) -> HashMap<String, Vec<(String, AgentDefinition)>> {
        let agents = self.agents.read().unwrap();
        let mut result = HashMap::new();

        for (team_id, member_ids) in teams {
            let mut team_agents = Vec::new();
            for agent_id in member_ids {
                if let Some(def) = agents.get(agent_id) {
                    team_agents.push((agent_id.clone(), def.clone()));
                }
            }
            result.insert(team_id.clone(), team_agents);
        }

        result
    }

    /// Validate that all agents referenced in a team exist in the registry.
    ///
    /// Returns a vector of warnings for any missing agents.
    ///
    /// # Arguments
    ///
    /// * `member_ids` - Slice of agent IDs to validate
    ///
    /// # Returns
    ///
    /// Vector of warning messages for missing agents
    pub fn validate_team_members(&self, member_ids: &[&str]) -> Vec<String> {
        let agents = self.agents.read().unwrap();
        let mut warnings = Vec::new();

        for agent_id in member_ids {
            if !agents.contains_key(*agent_id) {
                warnings.push(format!("Agent '{}' not found in registry", agent_id));
            }
        }

        warnings
    }
}

/// Convert AgentDefinition to DelegateAgentConfig
impl From<&AgentDefinition> for DelegateAgentConfig {
    fn from(def: &AgentDefinition) -> Self {
        Self {
            provider: def
                .provider
                .name
                .clone()
                .unwrap_or_else(|| "openrouter".to_string()),
            model: def
                .provider
                .model
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            system_prompt: if def.system.prompt.is_empty() {
                None
            } else {
                Some(def.system.prompt.clone())
            },
            api_key: def.provider.api_key.clone(),
            enabled: true,
            capabilities: Vec::new(),
            priority: 0,
            temperature: def.provider.temperature,
            max_depth: def.max_depth.unwrap_or(3),
            agentic: def.agentic.unwrap_or(true),
            allowed_tools: def
                .tools
                .tools
                .iter()
                .filter(|t| t.enabled)
                .map(|t| t.name.clone())
                .collect(),
            max_iterations: def.max_iterations.unwrap_or(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper to create a temporary agent definition file
    fn create_agent_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let file_path = dir.join(name);
        fs::write(&file_path, content).unwrap();
        file_path
    }

    /// Helper to create a valid minimal agent definition
    fn valid_agent_yaml(id: &str, name: &str) -> String {
        format!(
            r#"
agent:
  id: "{}"
  name: "{}"
  version: "1.0.0"
  description: "A test agent"

execution:
  mode: subprocess
  command: "test"
  args: []

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"

tools: {{}}

system:
  prompt: "You are a test agent"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 3
  backoff_ms: 1000
"#,
            id, name
        )
    }

    #[test]
    fn test_registry_creation() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security);

        assert!(registry.is_ok());
        let registry = registry.unwrap();
        assert_eq!(registry.agents_dir(), tmp_dir.path());
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_load_valid_agent_definition() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = valid_agent_yaml("test-agent", "Test Agent");
        create_agent_file(tmp_dir.path(), "test.yaml", &yaml);

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();

        let def = registry.load_definition(&tmp_dir.path().join("test.yaml"));

        assert!(def.is_ok());
        let def = def.unwrap();
        assert_eq!(def.agent.id, "test-agent");
        assert_eq!(def.agent.name, "Test Agent");
    }

    #[test]
    fn test_discover_agents_in_directory() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        // Create multiple agent files
        create_agent_file(
            tmp_dir.path(),
            "agent1.yaml",
            &valid_agent_yaml("agent1", "Agent 1"),
        );
        create_agent_file(
            tmp_dir.path(),
            "agent2.yaml",
            &valid_agent_yaml("agent2", "Agent 2"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let count = registry.discover();

        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 2);
        assert_eq!(registry.count(), 2);
    }

    #[test]
    fn test_discover_supports_yml_extension() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "agent.yml",
            &valid_agent_yaml("agent-yml", "Agent YML"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let count = registry.discover();

        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 1);
        assert!(registry.contains("agent-yml"));
    }

    #[test]
    fn test_get_agent_definition() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "test.yaml",
            &valid_agent_yaml("my-agent", "My Agent"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        let def = registry.get("my-agent");
        assert!(def.is_some());
        assert_eq!(def.unwrap().agent.name, "My Agent");

        let missing = registry.get("nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn test_list_agent_ids() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(tmp_dir.path(), "zebra.yaml", &valid_agent_yaml("z", "Z"));
        create_agent_file(tmp_dir.path(), "alpha.yaml", &valid_agent_yaml("a", "A"));
        create_agent_file(tmp_dir.path(), "beta.yaml", &valid_agent_yaml("b", "B"));

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        let ids = registry.list();
        assert_eq!(ids.len(), 3);
        // Should be sorted
        assert_eq!(ids, vec!["a", "b", "z"]);
    }

    #[test]
    fn test_contains_agent() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "test.yaml",
            &valid_agent_yaml("exists", "Exists"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        assert!(registry.contains("exists"));
        assert!(!registry.contains("does-not-exist"));
    }

    #[test]
    fn test_invalid_yaml_fails_gracefully() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(tmp_dir.path(), "invalid.yaml", "not: valid: yaml: [:");

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let result = registry.load_definition(&tmp_dir.path().join("invalid.yaml"));

        assert!(result.is_err());
    }

    #[test]
    fn test_validation_empty_id_rejected() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: ""
  name: "Test"
  version: "1.0.0"
  description: "Test"
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("ID cannot be empty"));
    }

    #[test]
    fn test_validation_id_with_path_separator_rejected() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "bad/id"
  name: "Test"
  version: "1.0.0"
  description: "Test"
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path separator"));
    }

    #[test]
    fn test_validation_empty_name_rejected() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "test"
  name: ""
  version: "1.0.0"
  description: "Test"
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty name"));
    }

    #[test]
    fn test_validation_empty_description_rejected() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: ""
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("empty description"));
    }

    #[test]
    fn test_validation_subprocess_requires_command() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"

execution:
  mode: subprocess
  command: ""
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[test]
    fn test_validation_conflicting_tool_permissions() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"

execution:
  mode: wasm

tools:
  tools:
    - name: "shell"
      enabled: true
  deny:
    - name: "shell"
      reason: "Test conflict"
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("both allowed and denied"));
    }

    #[test]
    fn test_validation_invalid_timeout() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"

execution:
  mode: subprocess
  command: "test"

reporting:
  timeout_seconds: 0
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[test]
    fn test_validation_invalid_max_attempts() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"

execution:
  mode: subprocess
  command: "test"

retry:
  max_attempts: 0
"#;

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        let result = registry.validate(&def);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_attempts"));
    }

    #[test]
    fn test_reload_clears_and_rediscover() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "agent1.yaml",
            &valid_agent_yaml("agent1", "Agent 1"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();
        assert_eq!(registry.count(), 1);

        // Add another file
        create_agent_file(
            tmp_dir.path(),
            "agent2.yaml",
            &valid_agent_yaml("agent2", "Agent 2"),
        );

        // Reload
        let count = registry.reload().unwrap();
        assert_eq!(count, 2);
        assert_eq!(registry.count(), 2);
        assert!(registry.contains("agent1"));
        assert!(registry.contains("agent2"));
    }

    #[test]
    fn test_with_search_dirs() {
        let tmp_dir = TempDir::new().unwrap();
        let extra_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "main.yaml",
            &valid_agent_yaml("main", "Main"),
        );
        create_agent_file(
            extra_dir.path(),
            "extra.yaml",
            &valid_agent_yaml("extra", "Extra"),
        );

        let registry = AgentRegistry::with_search_dirs(
            tmp_dir.path().to_path_buf(),
            vec![extra_dir.path().to_path_buf()],
            security,
        )
        .unwrap();

        let count = registry.discover().unwrap();
        assert_eq!(count, 2);
        assert!(registry.contains("main"));
        assert!(registry.contains("extra"));
    }

    #[test]
    fn test_invalid_files_skipped_during_discovery() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        // Valid file
        create_agent_file(
            tmp_dir.path(),
            "valid.yaml",
            &valid_agent_yaml("valid", "Valid"),
        );

        // Invalid YAML
        create_agent_file(tmp_dir.path(), "invalid.yaml", "bad: yaml: [:");

        // Non-YAML file
        create_agent_file(tmp_dir.path(), "readme.txt", "This is not YAML");

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let count = registry.discover().unwrap();

        // Should only load the valid file
        assert_eq!(count, 1);
        assert!(registry.contains("valid"));
        assert!(!registry.contains("invalid"));
    }

    #[test]
    fn test_nonexistent_directory_returns_zero() {
        let security = Arc::new(SecurityPolicy::default());

        let registry = AgentRegistry::new(PathBuf::from("/nonexistent/path"), security).unwrap();
        let count = registry.discover();

        assert!(count.is_ok());
        assert_eq!(count.unwrap(), 0);
    }

    #[test]
    fn test_get_all_agents() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(tmp_dir.path(), "a.yaml", &valid_agent_yaml("a", "A"));
        create_agent_file(tmp_dir.path(), "b.yaml", &valid_agent_yaml("b", "B"));

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        let all = registry.all();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key("a"));
        assert!(all.contains_key("b"));
    }

    #[test]
    fn test_add_search_dir() {
        let tmp_dir = TempDir::new().unwrap();
        let extra_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let mut registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();

        create_agent_file(
            extra_dir.path(),
            "extra.yaml",
            &valid_agent_yaml("extra", "Extra"),
        );

        registry.add_search_dir(extra_dir.path().to_path_buf());

        let count = registry.discover().unwrap();
        assert_eq!(count, 1);
        assert!(registry.contains("extra"));
    }

    #[test]
    fn test_convert_to_delegate_agent_config() {
        let yaml = r#"
agent:
  id: "test"
  name: "Test Agent"
  version: "1.0.0"
  description: "A test agent"

execution:
  mode: subprocess
  command: "test"
  args: []

provider:
  name: "openrouter"
  model: "custom-model"
  temperature: 0.5

tools:
  tools:
    - name: "shell"
      enabled: true
    - name: "file_read"
      enabled: false

system:
  prompt: "Custom prompt"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 3
  backoff_ms: 1000
"#;

        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        let config: DelegateAgentConfig = (&def).into();

        assert_eq!(config.provider, "openrouter");
        assert_eq!(config.model, "custom-model");
        assert_eq!(config.system_prompt, Some("Custom prompt".to_string()));
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.allowed_tools, vec!["shell"]);
        assert!(config.agentic);
    }

    #[test]
    fn test_execution_mode_variants() {
        let yaml_subprocess = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: subprocess
  command: "test"
"#;

        let yaml_wasm = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
"#;

        let yaml_docker = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: docker
"#;

        for yaml in &[yaml_subprocess, yaml_wasm, yaml_docker] {
            let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
            match yaml {
                y if *y == yaml_subprocess => {
                    assert_eq!(def.execution.mode, ExecutionMode::Subprocess)
                }
                y if *y == yaml_wasm => assert_eq!(def.execution.mode, ExecutionMode::Wasm),
                y if *y == yaml_docker => assert_eq!(def.execution.mode, ExecutionMode::Docker),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn test_memory_backend_variants() {
        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
memory:
  backend: shared
"#;

        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.memory.backend, MemoryBackend::Shared);

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
memory:
  backend: isolated
"#;

        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.memory.backend, MemoryBackend::Isolated);

        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
memory:
  backend: none
"#;

        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.memory.backend, MemoryBackend::None);
    }

    #[test]
    fn test_reporting_mode_variants() {
        let variants = vec![
            ("stdout", ReportingMode::Stdout),
            ("file", ReportingMode::File),
            ("ipc", ReportingMode::Ipc),
            ("http", ReportingMode::Http),
        ];

        for (variant_str, expected) in variants {
            let yaml = format!(
                r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
reporting:
  mode: {}
  format: json
  timeout_seconds: 300
"#,
                variant_str
            );

            let def: AgentDefinition = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(def.reporting.mode, expected);
        }
    }

    #[test]
    fn test_output_format_variants() {
        let variants = vec![
            ("json", OutputFormat::Json),
            ("markdown", OutputFormat::Markdown),
            ("both", OutputFormat::Both),
        ];

        for (variant_str, expected) in variants {
            let yaml = format!(
                r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
reporting:
  mode: ipc
  format: {}
  timeout_seconds: 300
"#,
                variant_str
            );

            let def: AgentDefinition = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(def.reporting.format, expected);
        }
    }

    #[test]
    fn test_default_values_are_applied() {
        let yaml = r#"
agent:
  id: "test"
  name: "Test"
  description: "Test"
execution:
  mode: subprocess
  command: "test"
"#;

        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(def.agent.version, "0.1.0");
        assert_eq!(def.execution.mode, ExecutionMode::Subprocess);
        assert_eq!(def.memory.backend, MemoryBackend::Shared);
        assert_eq!(def.reporting.mode, ReportingMode::Ipc);
        assert_eq!(def.reporting.format, OutputFormat::Json);
        assert_eq!(def.reporting.timeout_seconds, 300);
        assert_eq!(def.retry.max_attempts, 3);
        assert_eq!(def.retry.backoff_ms, 1000);
    }

    #[test]
    fn test_empty_directory_discoveries() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let count = registry.discover().unwrap();

        assert_eq!(count, 0);
        assert_eq!(registry.list().len(), 0);
    }

    // ── Flat Format Tests ───────────────────────────────────────────────

    #[test]
    fn test_flat_format_parsing() {
        let yaml = r#"
name: test-agent
version: 2.0.0
description: A test agent in flat format
provider:
  name: openrouter
  model: custom-model
  temperature: 0.3
system_prompt: |
  You are a test agent.
config:
  agentic: true
  allowed_tools:
    - shell
    - file_read
  max_depth: 5
  max_iterations: 20
"#;

        let flat: FlatAgentFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(flat.name, "test-agent");
        assert_eq!(flat.version, "2.0.0");
        assert_eq!(flat.description, "A test agent in flat format");
        assert_eq!(flat.provider.name, Some("openrouter".to_string()));
        assert_eq!(flat.provider.model, Some("custom-model".to_string()));
        assert_eq!(flat.provider.temperature, Some(0.3));
        assert!(flat.config.agentic);
        assert_eq!(flat.config.allowed_tools, vec!["shell", "file_read"]);
        assert_eq!(flat.config.max_depth, 5);
        assert_eq!(flat.config.max_iterations, 20);
    }

    #[test]
    fn test_flat_format_to_agent_definition() {
        let yaml = r#"
name: flat-agent
version: 1.5.0
description: Flat to AgentDefinition conversion test
provider:
  name: anthropic
  model: claude-sonnet-4-6
  temperature: 0.5
system_prompt: "Custom system prompt"
config:
  agentic: true
  allowed_tools:
    - browser
    - web_search
"#;

        let flat: FlatAgentFile = serde_yaml::from_str(yaml).unwrap();
        let def: AgentDefinition = flat.try_into().unwrap();

        assert_eq!(def.agent.id, "flat-agent");
        assert_eq!(def.agent.name, "flat-agent");
        assert_eq!(def.agent.version, "1.5.0");
        assert_eq!(def.agent.description, "Flat to AgentDefinition conversion test");
        assert_eq!(def.provider.name, Some("anthropic".to_string()));
        assert_eq!(def.provider.model, Some("claude-sonnet-4-6".to_string()));
        assert_eq!(def.provider.temperature, Some(0.5));
        assert_eq!(def.system.prompt, "Custom system prompt");
        assert_eq!(def.tools.tools.len(), 2);
        assert_eq!(def.tools.tools[0].name, "browser");
        assert_eq!(def.tools.tools[1].name, "web_search");
    }

    #[test]
    fn test_flat_format_to_delegate_config() {
        let yaml = r#"
name: delegate-test
version: 1.0.0
description: Flat to DelegateAgentConfig conversion test
provider:
  name: ollama
  model: llama3
  temperature: 0.7
system_prompt: "System prompt for delegate"
config:
  agentic: true
  max_depth: 3
  allowed_tools:
    - shell
  max_iterations: 15
"#;

        let flat: FlatAgentFile = serde_yaml::from_str(yaml).unwrap();
        let config: DelegateAgentConfig = (&flat).try_into().unwrap();

        assert_eq!(config.provider, "ollama");
        assert_eq!(config.model, "llama3");
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.system_prompt, Some("System prompt for delegate".to_string()));
        assert!(config.agentic);
        assert_eq!(config.max_depth, 3);
        assert_eq!(config.allowed_tools, vec!["shell"]);
        assert_eq!(config.max_iterations, 15);
    }

    #[test]
    fn test_registry_loads_flat_format() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let yaml_content = r#"
name: researcher
version: 1.0.0
description: Research agent
provider:
  name: anthropic
  model: claude-sonnet-4-6
system_prompt: "You are a research agent."
config:
  agentic: true
  allowed_tools:
    - web_search
    - browser
"#;

        let file_path = tmp_dir.path().join("researcher.yaml");
        fs::write(&file_path, yaml_content).unwrap();

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let count = registry.discover().unwrap();

        assert_eq!(count, 1);
        assert!(registry.contains("researcher"));

        let def = registry.get("researcher").unwrap();
        assert_eq!(def.agent.id, "researcher");
        assert_eq!(def.agent.name, "researcher");
        assert_eq!(def.provider.name, Some("anthropic".to_string()));
        assert_eq!(def.provider.model, Some("claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn test_registry_falls_back_to_flat_format() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        // This is the flat format used in config/agents/*.agent.yaml
        let yaml_content = r#"
name: coder
version: 1.0.0
description: Coding agent
provider:
  name: openrouter
  model: anthropic/claude-sonnet-4-6
system_prompt: |
  You are a coding assistant.
config:
  agentic: true
  allowed_tools:
    - file_read
    - file_write
    - shell
  max_depth: 4
"#;

        let file_path = tmp_dir.path().join("coder.agent.yaml");
        fs::write(&file_path, yaml_content).unwrap();

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let count = registry.discover().unwrap();

        assert_eq!(count, 1);
        assert!(registry.contains("coder"));

        let def = registry.get("coder").unwrap();
        assert_eq!(def.agent.id, "coder");
        assert_eq!(def.agent.name, "coder");
        assert_eq!(def.provider.name, Some("openrouter".to_string()));

        // Test conversion to DelegateAgentConfig
        let configs = registry.all_as_delegate_configs();
        assert!(configs.contains_key("coder"));

        let config = &configs["coder"];
        assert_eq!(config.provider, "openrouter");
        assert_eq!(config.model, "anthropic/claude-sonnet-4-6");
        assert!(config.agentic);
        assert_eq!(config.max_depth, 4);
        assert_eq!(config.allowed_tools.len(), 3);
    }

    #[test]
    fn test_all_as_delegate_configs_from_flat_format() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        // Create two agents in flat format
        let agent1 = r#"
name: agent1
version: 1.0.0
description: First agent
provider:
  name: "provider1"
  model: "model1"
system_prompt: "Prompt 1"
config:
  agentic: false
"#;

        let agent2 = r#"
name: agent2
version: 1.0.0
description: Second agent
provider:
  name: "provider2"
  model: "model2"
system_prompt: "Prompt 2"
config:
  agentic: true
  allowed_tools:
    - shell
"#;

        fs::write(tmp_dir.path().join("agent1.yaml"), agent1).unwrap();
        fs::write(tmp_dir.path().join("agent2.yaml"), agent2).unwrap();

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        let configs = registry.all_as_delegate_configs();
        assert_eq!(configs.len(), 2);

        let config1 = &configs["agent1"];
        assert_eq!(config1.provider, "provider1");
        assert_eq!(config1.model, "model1");
        assert!(!config1.agentic);

        let config2 = &configs["agent2"];
        assert_eq!(config2.provider, "provider2");
        assert_eq!(config2.model, "model2");
        assert!(config2.agentic);
        assert_eq!(config2.allowed_tools.len(), 1);
    }

    // ── Team Membership Tests ─────────────────────────────────────────────

    #[test]
    fn test_get_team_members() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        // Create agents
        create_agent_file(
            tmp_dir.path(),
            "lead.yaml",
            &valid_agent_yaml("team-lead", "Team Lead"),
        );
        create_agent_file(
            tmp_dir.path(),
            "worker1.yaml",
            &valid_agent_yaml("worker-1", "Worker 1"),
        );
        create_agent_file(
            tmp_dir.path(),
            "worker2.yaml",
            &valid_agent_yaml("worker-2", "Worker 2"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        // Get team members
        let member_ids = vec!["team-lead", "worker-1", "worker-2"];
        let members = registry.get_team_members(&member_ids);

        assert_eq!(members.len(), 3);
        assert!(members.iter().any(|(id, _)| id == "team-lead"));
        assert!(members.iter().any(|(id, _)| id == "worker-1"));
        assert!(members.iter().any(|(id, _)| id == "worker-2"));
    }

    #[test]
    fn test_get_team_members_with_missing_agent() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "agent1.yaml",
            &valid_agent_yaml("agent-1", "Agent 1"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        // Include a non-existent agent ID
        let member_ids = vec!["agent-1", "non-existent"];
        let members = registry.get_team_members(&member_ids);

        // Should only return the existing agent
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].0, "agent-1");
    }

    #[test]
    fn test_get_by_team_role() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "lead.yaml",
            &valid_agent_yaml("lead-agent", "Lead"),
        );
        create_agent_file(
            tmp_dir.path(),
            "worker.yaml",
            &valid_agent_yaml("worker-agent", "Worker"),
        );
        create_agent_file(
            tmp_dir.path(),
            "specialist.yaml",
            &valid_agent_yaml("specialist-agent", "Specialist"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        // Create team members with roles
        use crate::agent::team_definition::AgentRole;
        let team_members = vec![
            ("lead-agent".to_string(), AgentRole::Lead),
            ("worker-agent".to_string(), AgentRole::Worker),
            ("specialist-agent".to_string(), AgentRole::Specialist),
        ];

        // Get all team members with roles
        let result = registry.get_by_team_role(&team_members);
        assert_eq!(result.len(), 3);

        // Verify roles are preserved
        assert!(result.iter().any(|(id, _, role)| {
            id == "lead-agent" && *role == AgentRole::Lead
        }));
        assert!(result.iter().any(|(id, _, role)| {
            id == "worker-agent" && *role == AgentRole::Worker
        }));
        assert!(result.iter().any(|(id, _, role)| {
            id == "specialist-agent" && *role == AgentRole::Specialist
        }));

        // Get only lead agents
        let leads: Vec<_> = result
            .iter()
            .filter(|(_, _, role)| *role == AgentRole::Lead)
            .collect();
        assert_eq!(leads.len(), 1);
    }

    #[test]
    fn test_get_team_agents() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "agent1.yaml",
            &valid_agent_yaml("agent-1", "Agent 1"),
        );
        create_agent_file(
            tmp_dir.path(),
            "agent2.yaml",
            &valid_agent_yaml("agent-2", "Agent 2"),
        );
        create_agent_file(
            tmp_dir.path(),
            "agent3.yaml",
            &valid_agent_yaml("agent-3", "Agent 3"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        // Create teams map
        let mut teams = std::collections::HashMap::new();
        teams.insert(
            "team-a".to_string(),
            vec!["agent-1".to_string(), "agent-2".to_string()],
        );
        teams.insert(
            "team-b".to_string(),
            vec!["agent-2".to_string(), "agent-3".to_string()],
        );

        let result = registry.get_team_agents(&teams);

        assert_eq!(result.len(), 2);
        assert_eq!(result["team-a"].len(), 2);
        assert_eq!(result["team-b"].len(), 2);

        // Verify agent-2 is in both teams
        assert!(result["team-a"].iter().any(|(id, _)| id == "agent-2"));
        assert!(result["team-b"].iter().any(|(id, _)| id == "agent-2"));
    }

    #[test]
    fn test_validate_team_members_all_exist() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "agent1.yaml",
            &valid_agent_yaml("agent-1", "Agent 1"),
        );
        create_agent_file(
            tmp_dir.path(),
            "agent2.yaml",
            &valid_agent_yaml("agent-2", "Agent 2"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        let member_ids = vec!["agent-1", "agent-2"];
        let warnings = registry.validate_team_members(&member_ids);

        assert_eq!(warnings.len(), 0);
    }

    #[test]
    fn test_validate_team_members_with_missing() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        create_agent_file(
            tmp_dir.path(),
            "agent1.yaml",
            &valid_agent_yaml("agent-1", "Agent 1"),
        );

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        registry.discover().unwrap();

        let member_ids = vec!["agent-1", "missing-agent", "another-missing"];
        let warnings = registry.validate_team_members(&member_ids);

        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("missing-agent")));
        assert!(warnings.iter().any(|w| w.contains("another-missing")));
    }

    #[test]
    fn test_get_team_members_empty_list() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();

        let members = registry.get_team_members(&[]);
        assert_eq!(members.len(), 0);
    }

    #[test]
    fn test_get_team_agents_empty_teams() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());

        let registry = AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap();
        let teams = std::collections::HashMap::new();

        let result = registry.get_team_agents(&teams);
        assert_eq!(result.len(), 0);
    }
}
