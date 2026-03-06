#[allow(clippy::module_inception)]
pub mod agent;
pub mod classifier;
pub mod dispatcher;
pub mod loop_;
pub mod memory_loader;
pub mod prompt;
pub mod quota_aware;
pub mod registry;
pub mod research;
pub mod session;
pub mod team_definition;
pub mod team_orchestration;
pub mod team_registry;
pub mod watcher;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder};
#[allow(unused_imports)]
pub use loop_::{process_message, process_message_with_session, run, run_tool_call_loop};

// Re-export registry types for convenience
#[allow(unused_imports)]
pub use registry::{
    AgentDefinition, AgentExecution, AgentMemory, AgentMetadata, AgentProvider, AgentRegistry,
    AgentReporting, AgentRetry, AgentSystem, AgentTeamMembership, AgentToolConfig, AgentToolDeny,
    AgentTools, ExecutionMode, MemoryBackend, OutputFormat, ReportingMode, TeamMembershipRole,
};

// Re-export watcher
#[allow(unused_imports)]
pub use watcher::AgentWatcher;

// Re-export team definition types for convenience
#[allow(unused_imports)]
pub use team_definition::{
    AgentRole, BudgetTier, CoordinationProtocol, DegradationPolicyType, LeadAgentConfig,
    TeamCoordination, TeamBudget, TeamDefinition, TeamDegradation, TeamMemberRef, TeamMetadata,
    TeamTopologyConfig, TeamTopologyType, TeamWorkload, WorkloadType,
};

// Re-export team registry
#[allow(unused_imports)]
pub use team_registry::{TeamRegistry, TeamStats};
