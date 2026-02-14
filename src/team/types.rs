use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Context passed to team execution containing shared state and configuration.
#[derive(Debug, Clone)]
pub struct TeamExecutionContext {
    /// Unique identifier for this team
    pub team_id: String,
    /// Tenant identifier for multi-tenancy isolation
    pub tenant_id: String,
    /// The input task/prompt to be processed by the team
    pub input: String,
    /// Shared message log accessible by all team members during execution
    pub shared_memory: Arc<Mutex<Vec<TeamMessage>>>,
    /// Optional timeout for the entire team execution
    pub timeout: Option<Duration>,
}

/// A message exchanged between team members during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessage {
    /// ID of the agent that produced this message
    pub agent_id: String,
    /// Human-readable name of the agent
    pub agent_name: String,
    /// Role of the agent (e.g., "coordinator", "analyst")
    pub role: String,
    /// The message content
    pub content: String,
    /// Unix timestamp in milliseconds when the message was created
    pub timestamp: i64,
}

/// Runtime representation of a team member with resolved agent metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberRuntime {
    /// Unique agent identifier
    pub agent_id: String,
    /// Human-readable agent name
    pub agent_name: String,
    /// Optional role description for this agent within the team
    pub role: Option<String>,
    /// List of capabilities this agent can perform
    pub capabilities: Vec<String>,
    /// Weight for selection/scoring algorithms (higher = more preferred)
    pub weight: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_message_serializes() {
        let msg = TeamMessage {
            agent_id: "a1".into(),
            agent_name: "Analyst".into(),
            role: "analyst".into(),
            content: "Analysis complete.".into(),
            timestamp: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"agent_id\":\"a1\""));
        let parsed: TeamMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_name, "Analyst");
    }

    #[test]
    fn team_member_runtime_serializes() {
        let member = TeamMemberRuntime {
            agent_id: "a1".into(),
            agent_name: "Writer".into(),
            role: Some("content creator".into()),
            capabilities: vec!["writing".into(), "editing".into()],
            weight: 1.5,
        };
        let json = serde_json::to_string(&member).unwrap();
        let parsed: TeamMemberRuntime = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.capabilities.len(), 2);
        assert!((parsed.weight - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn shared_memory_is_thread_safe() {
        let ctx = TeamExecutionContext {
            team_id: "team-1".into(),
            tenant_id: "tenant-1".into(),
            input: "test input".into(),
            shared_memory: Arc::new(Mutex::new(Vec::new())),
            timeout: Some(Duration::from_secs(30)),
        };
        let mem = ctx.shared_memory.clone();
        mem.lock().unwrap().push(TeamMessage {
            agent_id: "a1".into(),
            agent_name: "Test".into(),
            role: "tester".into(),
            content: "hello".into(),
            timestamp: 0,
        });
        assert_eq!(ctx.shared_memory.lock().unwrap().len(), 1);
    }
}
