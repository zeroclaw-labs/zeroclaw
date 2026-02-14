//! Team orchestration engine.
//!
//! The `TeamEngine` dispatches team execution to the appropriate collaboration
//! mode based on the `TeamMode` configuration. It manages the execution
//! context, shared memory, and timeout enforcement.

use anyhow::{bail, Result};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::aria::db::AriaDb;
use crate::aria::types::{TeamMode, TeamResult};
use super::modes;
use super::types::{TeamExecutionContext, TeamMemberRuntime};

/// Core team orchestration engine that coordinates multi-agent collaboration.
///
/// The engine creates a shared execution context and dispatches to the
/// appropriate collaboration mode. It enforces timeout constraints and
/// manages the shared memory that agents use to communicate.
pub struct TeamEngine {
    #[allow(dead_code)]
    db: AriaDb,
}

impl TeamEngine {
    /// Create a new `TeamEngine` with the given database handle.
    pub fn new(db: AriaDb) -> Self {
        Self { db }
    }

    /// Execute a team task using the specified collaboration mode.
    ///
    /// # Arguments
    /// * `team_id` - Unique identifier for the team
    /// * `tenant_id` - Tenant identifier for multi-tenancy isolation
    /// * `input` - The task/prompt to process
    /// * `mode` - The collaboration mode to use
    /// * `members` - Runtime-resolved team members with their capabilities
    /// * `timeout` - Optional overall timeout for the team execution
    /// * `max_rounds` - Optional maximum number of rounds (for iterative modes)
    ///
    /// # Returns
    /// A `TeamResult` containing individual agent results and the combined output.
    pub async fn execute(
        &self,
        team_id: &str,
        tenant_id: &str,
        input: &str,
        mode: &TeamMode,
        members: &[TeamMemberRuntime],
        timeout: Option<Duration>,
        max_rounds: Option<u32>,
    ) -> Result<TeamResult> {
        if members.is_empty() {
            bail!("Cannot execute team with no members");
        }

        let ctx = TeamExecutionContext {
            team_id: team_id.to_string(),
            tenant_id: tenant_id.to_string(),
            input: input.to_string(),
            shared_memory: Arc::new(Mutex::new(Vec::new())),
            timeout,
        };

        let execution = async {
            match mode {
                TeamMode::Coordinator => modes::run_coordinator(&ctx, members, max_rounds).await,
                TeamMode::RoundRobin => modes::run_round_robin(&ctx, members, max_rounds).await,
                TeamMode::DelegateToBest => modes::run_delegate_to_best(&ctx, members).await,
                TeamMode::Parallel => modes::run_parallel(&ctx, members).await,
                TeamMode::Sequential => modes::run_sequential(&ctx, members).await,
            }
        };

        // Apply timeout if configured
        if let Some(duration) = timeout {
            match tokio::time::timeout(duration, execution).await {
                Ok(result) => result,
                Err(_) => Ok(TeamResult {
                    success: false,
                    result: None,
                    error: Some(format!(
                        "Team execution timed out after {}ms",
                        duration.as_millis()
                    )),
                    agent_results: Vec::new(),
                    mode: format!("{:?}", mode).to_lowercase(),
                    duration_ms: Some(duration.as_millis() as u64),
                    metadata: None,
                }),
            }
        } else {
            execution.await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> TeamEngine {
        let db = AriaDb::open_in_memory().unwrap();
        TeamEngine::new(db)
    }

    fn make_members(count: usize) -> Vec<TeamMemberRuntime> {
        (0..count)
            .map(|i| TeamMemberRuntime {
                agent_id: format!("agent-{i}"),
                agent_name: format!("Agent{i}"),
                role: Some(if i == 0 {
                    "coordinator".into()
                } else {
                    format!("worker-{i}")
                }),
                capabilities: vec!["general".into()],
                weight: 1.0,
            })
            .collect()
    }

    #[tokio::test]
    async fn execute_coordinator_mode() {
        let engine = setup();
        let members = make_members(3);
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "Do something",
                &TeamMode::Coordinator,
                &members,
                None,
                Some(1),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "coordinator");
        assert!(!result.agent_results.is_empty());
    }

    #[tokio::test]
    async fn execute_round_robin_mode() {
        let engine = setup();
        let members = make_members(3);
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "Collaborate",
                &TeamMode::RoundRobin,
                &members,
                None,
                Some(2),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "round_robin");
    }

    #[tokio::test]
    async fn execute_delegate_mode() {
        let engine = setup();
        let members = make_members(3);
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "Find the answer",
                &TeamMode::DelegateToBest,
                &members,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "delegate_to_best");
        assert_eq!(result.agent_results.len(), 1);
    }

    #[tokio::test]
    async fn execute_parallel_mode() {
        let engine = setup();
        let members = make_members(3);
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "Analyze from all angles",
                &TeamMode::Parallel,
                &members,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "parallel");
        assert_eq!(result.agent_results.len(), 3);
    }

    #[tokio::test]
    async fn execute_sequential_mode() {
        let engine = setup();
        let members = make_members(3);
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "Chain of thought",
                &TeamMode::Sequential,
                &members,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "sequential");
        assert_eq!(result.agent_results.len(), 3);
    }

    #[tokio::test]
    async fn execute_empty_members_fails() {
        let engine = setup();
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "task",
                &TeamMode::Coordinator,
                &[],
                None,
                None,
            )
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_with_timeout() {
        let engine = setup();
        let members = make_members(2);
        let result = engine
            .execute(
                "team-1",
                "tenant-1",
                "Quick task",
                &TeamMode::Sequential,
                &members,
                Some(Duration::from_secs(30)),
                None,
            )
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn all_modes_produce_duration() {
        let engine = setup();
        let members = make_members(2);

        for mode in &[
            TeamMode::Coordinator,
            TeamMode::RoundRobin,
            TeamMode::DelegateToBest,
            TeamMode::Parallel,
            TeamMode::Sequential,
        ] {
            let result = engine
                .execute("t", "ten", "test", mode, &members, None, Some(1))
                .await
                .unwrap();
            assert!(
                result.duration_ms.is_some(),
                "Mode {:?} missing duration",
                mode
            );
        }
    }
}
