//! Team Definition Schema — YAML-based team configuration for multi-agent orchestration.
//!
//! This module provides the schema structures for loading team definitions from YAML files.
//! Team definitions specify:
//! - Team metadata (id, name, version, description)
//! - Topology configuration (single, lead-subagent, star-team, mesh-team)
//! - Coordination settings (protocol, timeouts, budgets)
//! - Budget constraints (tier, summary caps, worker limits)
//! - Workload specialization (implementation, debugging, research)
//! - Degradation policies (auto, none, aggressive)
//! - Team member references (agent IDs, roles, capabilities)
//!
//! ## File Format
//!
//! ```yaml
//! # teams/research-team.yaml
//! team:
//!   id: "research-team"
//!   name: "Research Team"
//!   version: "1.0.0"
//!   description: "Multi-agent team for research tasks"
//!
//! topology:
//!   type: "star_team"
//!   lead:
//!     agent_id: "research-lead"
//!     max_delegates: 3
//!     handoff_timeout_seconds: 60
//!
//! coordination:
//!   protocol: "a2a_lite"
//!   max_round_trips: 5
//!   sync_interval_ms: 1000
//!   message_budget_per_task: 20
//!
//! budget:
//!   tier: "medium"
//!   summary_cap_tokens: 120
//!   max_workers: 5
//!
//! workload:
//!   type: "research"
//!
//! degradation:
//!   policy: "auto"
//!   max_topology_downgrades: 2
//!
//! members:
//!   - agent_id: "research-lead"
//!     role: "lead"
//!     max_concurrent_tasks: 3
//!     capabilities: ["web_search", "memory_read"]
//!   - agent_id: "data-analyst"
//!     role: "specialist"
//!     max_concurrent_tasks: 2
//!     capabilities: ["data_processing"]
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Team Metadata
// ============================================================================

/// Team metadata
///
/// Identifies and describes the team configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TeamMetadata {
    /// Unique team identifier (e.g., "research-team")
    pub id: String,
    /// Human-readable team name
    pub name: String,
    /// Semantic version
    #[serde(default = "default_team_version")]
    pub version: String,
    /// Description of the team's purpose
    pub description: String,
}

fn default_team_version() -> String {
    "1.0.0".to_string()
}

// ============================================================================
// Topology Configuration
// ============================================================================

/// Team topology type matching existing TeamTopology
///
/// Defines the communication pattern for agent collaboration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TeamTopologyType {
    /// Single agent (no delegation)
    #[default]
    Single,
    /// Lead agent with single subagent
    LeadSubagent,
    /// Star topology: lead coordinates multiple workers
    StarTeam,
    /// Mesh topology: all agents can coordinate directly
    MeshTeam,
}

impl TeamTopologyType {
    /// Returns all topology variants
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Single, Self::LeadSubagent, Self::StarTeam, Self::MeshTeam]
    }

    /// Returns the string representation
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::LeadSubagent => "lead_subagent",
            Self::StarTeam => "star_team",
            Self::MeshTeam => "mesh_team",
        }
    }

    /// Returns the number of participants for a given max worker count
    #[must_use]
    pub const fn participants(self, max_workers: usize) -> usize {
        match self {
            Self::Single => 1,
            Self::LeadSubagent => 2,
            Self::StarTeam | Self::MeshTeam => {
                if max_workers == 0 {
                    1
                } else if max_workers > 5 {
                    6
                } else {
                    max_workers + 1
                }
            }
        }
    }
}

/// Lead agent configuration
///
/// Specifies settings for the lead/coordination agent in multi-agent topologies.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LeadAgentConfig {
    /// Agent ID of the lead agent
    pub agent_id: String,
    /// Maximum number of delegates the lead can manage
    #[serde(default = "default_max_delegates")]
    pub max_delegates: usize,
    /// Timeout for handoff operations in seconds
    #[serde(default = "default_handoff_timeout")]
    pub handoff_timeout_seconds: u64,
}

fn default_max_delegates() -> usize {
    3
}

fn default_handoff_timeout() -> u64 {
    60
}

/// Team topology configuration
///
/// Defines the communication pattern and lead agent settings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TeamTopologyConfig {
    /// Topology type (single, lead_subagent, star_team, mesh_team)
    #[serde(rename = "type")]
    pub topology_type: TeamTopologyType,
    /// Lead agent configuration (required for multi-agent topologies)
    pub lead: Option<LeadAgentConfig>,
}

// ============================================================================
// Coordination Settings
// ============================================================================

/// Coordination protocol
///
/// Defines the message-passing protocol for agent coordination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationProtocol {
    /// Lightweight A2A protocol for simple coordination
    #[default]
    A2aLite,
    /// Full transcript sharing for complex collaboration
    Transcript,
}

impl CoordinationProtocol {
    /// Returns all protocol variants
    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::A2aLite, Self::Transcript]
    }

    /// Returns the string representation
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A2aLite => "a2a_lite",
            Self::Transcript => "transcript",
        }
    }
}

/// Team coordination settings
///
/// Controls how agents communicate and synchronize during task execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TeamCoordination {
    /// Coordination protocol to use
    #[serde(default)]
    pub protocol: CoordinationProtocol,
    /// Maximum round trips before forcing resolution
    #[serde(default = "default_max_round_trips")]
    pub max_round_trips: u32,
    /// Synchronization interval in milliseconds
    #[serde(default = "default_sync_interval")]
    pub sync_interval_ms: u64,
    /// Message budget per task to prevent infinite loops
    #[serde(default = "default_message_budget")]
    pub message_budget_per_task: u32,
}

fn default_max_round_trips() -> u32 {
    5
}

fn default_sync_interval() -> u64 {
    1000
}

fn default_message_budget() -> u32 {
    20
}

// ============================================================================
// Budget Settings
// ============================================================================

/// Budget tier
///
/// Defines the resource allocation tier for the team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum BudgetTier {
    /// Balanced resource allocation
    #[default]
    Medium,
    /// Minimal resource usage
    Low,
    /// Maximum resource allocation
    High,
}

impl BudgetTier {
    /// Returns all tier variants
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Low, Self::Medium, Self::High]
    }

    /// Returns the string representation
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Returns the maximum number of workers allowed for this tier
    #[must_use]
    pub const fn max_workers(self) -> usize {
        match self {
            Self::Low => 2,
            Self::Medium => 5,
            Self::High => 10,
        }
    }

    /// Returns the summary cap in tokens for this tier
    #[must_use]
    pub const fn summary_cap_tokens(self) -> u32 {
        match self {
            Self::Low => 80,
            Self::Medium => 120,
            Self::High => 200,
        }
    }
}

/// Team budget settings
///
/// Controls resource allocation and usage limits for the team.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TeamBudget {
    /// Budget tier (low, medium, high)
    #[serde(default)]
    pub tier: BudgetTier,
    /// Maximum tokens for agent summaries
    #[serde(default = "default_summary_cap")]
    pub summary_cap_tokens: u32,
    /// Maximum concurrent workers
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,
}

fn default_summary_cap() -> u32 {
    120
}

fn default_max_workers() -> usize {
    5
}

// ============================================================================
// Workload Settings
// ============================================================================

/// Workload type
///
/// Specifies the primary workload type for the team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadType {
    /// Mixed workload (general purpose)
    #[default]
    Mixed,
    /// Implementation-focused (coding, refactoring)
    Implementation,
    /// Debugging-focused (investigation, fixes)
    Debugging,
    /// Research-focused (information gathering, analysis)
    Research,
}

impl WorkloadType {
    /// Returns all workload type variants
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Mixed, Self::Implementation, Self::Debugging, Self::Research]
    }

    /// Returns the string representation
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Implementation => "implementation",
            Self::Debugging => "debugging",
            Self::Research => "research",
        }
    }
}

/// Team workload settings
///
/// Defines the specialization and primary work type for the team.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TeamWorkload {
    /// Workload type
    #[serde(default, rename = "type")]
    pub workload_type: WorkloadType,
}

// ============================================================================
// Degradation Settings
// ============================================================================

/// Degradation policy
///
/// Defines how the team handles resource constraints or failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DegradationPolicyType {
    /// Automatic degradation based on conditions
    #[default]
    Auto,
    /// No degradation allowed
    None,
    /// Aggressive degradation to conserve resources
    Aggressive,
}

impl DegradationPolicyType {
    /// Returns all policy variants
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::None, Self::Auto, Self::Aggressive]
    }

    /// Returns the string representation
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Auto => "auto",
            Self::Aggressive => "aggressive",
        }
    }
}

/// Team degradation settings
///
/// Controls how the team degrades under resource pressure.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct TeamDegradation {
    /// Degradation policy
    #[serde(default)]
    pub policy: DegradationPolicyType,
    /// Maximum number of topology downgrades allowed
    #[serde(default = "default_max_downgrades")]
    pub max_topology_downgrades: u32,
}

fn default_max_downgrades() -> u32 {
    2
}

// ============================================================================
// Team Member References
// ============================================================================

/// Agent role within team
///
/// Defines the functional role of an agent in the team structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Standard worker agent
    #[default]
    Worker,
    /// Lead/coordination agent
    Lead,
    /// Specialist agent with specific capabilities
    Specialist,
}

impl AgentRole {
    /// Returns all role variants
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Worker, Self::Lead, Self::Specialist]
    }

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

/// Team member reference
///
/// References an agent definition with role-specific configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TeamMemberRef {
    /// Agent ID (must match an agent definition in the registry)
    pub agent_id: String,
    /// Role within the team
    #[serde(default)]
    pub role: AgentRole,
    /// Maximum concurrent tasks this member can handle
    #[serde(default = "default_member_max_tasks")]
    pub max_concurrent_tasks: usize,
    /// Specialized capabilities/tools this member provides
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_member_max_tasks() -> usize {
    1
}

// ============================================================================
// Complete Team Definition
// ============================================================================

/// Complete team definition loaded from YAML
///
/// This is the top-level structure for team configuration files.
/// It encompasses all aspects of team behavior: metadata, topology,
/// coordination, budget, workload, degradation, and member composition.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TeamDefinition {
    /// Team identification and metadata
    pub team: TeamMetadata,
    /// Topology configuration
    pub topology: TeamTopologyConfig,
    /// Coordination settings (optional, uses defaults if not specified)
    #[serde(default)]
    pub coordination: TeamCoordination,
    /// Budget constraints (optional, uses defaults if not specified)
    #[serde(default)]
    pub budget: TeamBudget,
    /// Workload specialization (optional, uses defaults if not specified)
    #[serde(default)]
    pub workload: TeamWorkload,
    /// Degradation policy (optional, uses defaults if not specified)
    #[serde(default)]
    pub degradation: TeamDegradation,
    /// Team member references
    pub members: Vec<TeamMemberRef>,
}

impl TeamDefinition {
    /// Returns the team ID
    #[must_use]
    pub fn id(&self) -> &str {
        &self.team.id
    }

    /// Returns the team name
    #[must_use]
    pub fn name(&self) -> &str {
        &self.team.name
    }

    /// Returns the topology type
    #[must_use]
    pub fn topology_type(&self) -> TeamTopologyType {
        self.topology.topology_type
    }

    /// Returns the number of participants based on topology and budget
    #[must_use]
    pub fn participant_count(&self) -> usize {
        self.topology.topology_type.participants(self.budget.max_workers)
    }

    /// Returns the lead agent ID if topology requires one
    #[must_use]
    pub fn lead_agent_id(&self) -> Option<&str> {
        match self.topology.topology_type {
            TeamTopologyType::Single => None,
            TeamTopologyType::LeadSubagent | TeamTopologyType::StarTeam | TeamTopologyType::MeshTeam => {
                self.topology.lead.as_ref().map(|lead| lead.agent_id.as_str())
            }
        }
    }

    /// Returns all member agent IDs
    #[must_use]
    pub fn member_ids(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.agent_id.as_str()).collect()
    }

    /// Returns members with a specific role
    #[must_use]
    pub fn members_by_role(&self, role: AgentRole) -> Vec<&TeamMemberRef> {
        self.members.iter().filter(|m| m.role == role).collect()
    }

    /// Validates the team definition
    ///
    /// Returns an error if the definition is invalid.
    pub fn validate(&self) -> anyhow::Result<()> {
        // Validate team ID is non-empty
        if self.team.id.is_empty() {
            anyhow::bail!("Team ID cannot be empty");
        }

        // Validate topology has lead if required
        match self.topology.topology_type {
            TeamTopologyType::Single => {
                if self.topology.lead.is_some() {
                    anyhow::bail!("Single topology should not have a lead config");
                }
            }
            TeamTopologyType::LeadSubagent | TeamTopologyType::StarTeam | TeamTopologyType::MeshTeam => {
                if self.topology.lead.is_none() {
                    anyhow::bail!(
                        "{:?} topology requires a lead agent configuration",
                        self.topology.topology_type
                    );
                }
            }
        }

        // Validate at least one member
        if self.members.is_empty() {
            anyhow::bail!("Team must have at least one member");
        }

        // Validate lead agent exists in members
        if let Some(lead) = &self.topology.lead {
            if !self.members.iter().any(|m| m.agent_id == lead.agent_id) {
                anyhow::bail!(
                    "Lead agent '{}' not found in members list",
                    lead.agent_id
                );
            }
        }

        // Validate member IDs are unique
        let mut seen = std::collections::HashSet::new();
        for member in &self.members {
            if !seen.insert(&member.agent_id) {
                anyhow::bail!("Duplicate member ID: {}", member.agent_id);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topology_participants() {
        assert_eq!(TeamTopologyType::Single.participants(5), 1);
        assert_eq!(TeamTopologyType::LeadSubagent.participants(5), 2);
        assert_eq!(TeamTopologyType::StarTeam.participants(3), 4);
        assert_eq!(TeamTopologyType::MeshTeam.participants(5), 6);
    }

    #[test]
    fn test_budget_tier_limits() {
        assert_eq!(BudgetTier::Low.max_workers(), 2);
        assert_eq!(BudgetTier::Medium.max_workers(), 5);
        assert_eq!(BudgetTier::High.max_workers(), 10);
    }

    #[test]
    fn test_role_strings() {
        assert_eq!(AgentRole::Worker.as_str(), "worker");
        assert_eq!(AgentRole::Lead.as_str(), "lead");
        assert_eq!(AgentRole::Specialist.as_str(), "specialist");
    }

    #[test]
    fn test_validation_valid_team() {
        let team = TeamDefinition {
            team: TeamMetadata {
                id: "test-team".to_string(),
                name: "Test Team".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
            },
            topology: TeamTopologyConfig {
                topology_type: TeamTopologyType::StarTeam,
                lead: Some(LeadAgentConfig {
                    agent_id: "lead-1".to_string(),
                    max_delegates: 3,
                    handoff_timeout_seconds: 60,
                }),
            },
            coordination: TeamCoordination::default(),
            budget: TeamBudget::default(),
            workload: TeamWorkload::default(),
            degradation: TeamDegradation::default(),
            members: vec![
                TeamMemberRef {
                    agent_id: "lead-1".to_string(),
                    role: AgentRole::Lead,
                    max_concurrent_tasks: 3,
                    capabilities: vec!["coordination".to_string()],
                },
                TeamMemberRef {
                    agent_id: "worker-1".to_string(),
                    role: AgentRole::Worker,
                    max_concurrent_tasks: 1,
                    capabilities: vec![],
                },
            ],
        };

        assert!(team.validate().is_ok());
    }

    #[test]
    fn test_validation_empty_id() {
        let team = TeamDefinition {
            team: TeamMetadata {
                id: "".to_string(),
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
            },
            topology: TeamTopologyConfig {
                topology_type: TeamTopologyType::Single,
                lead: None,
            },
            coordination: TeamCoordination::default(),
            budget: TeamBudget::default(),
            workload: TeamWorkload::default(),
            degradation: TeamDegradation::default(),
            members: vec![],
        };

        assert!(team.validate().is_err());
    }

    #[test]
    fn test_validation_missing_lead() {
        let team = TeamDefinition {
            team: TeamMetadata {
                id: "test-team".to_string(),
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
            },
            topology: TeamTopologyConfig {
                topology_type: TeamTopologyType::StarTeam,
                lead: None,
            },
            coordination: TeamCoordination::default(),
            budget: TeamBudget::default(),
            workload: TeamWorkload::default(),
            degradation: TeamDegradation::default(),
            members: vec![],
        };

        assert!(team.validate().is_err());
    }
}
