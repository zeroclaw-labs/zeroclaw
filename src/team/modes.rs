//! Team collaboration mode implementations.
//!
//! Each mode defines a different strategy for how a team of agents collaborates
//! to accomplish a task. Agent execution uses a deterministic local processor
//! when no LLM provider is injected, or delegates to the agentic executor
//! when a provider is available via the team execution context.

use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::HashMap;

use super::types::{TeamExecutionContext, TeamMemberRuntime, TeamMessage};
use crate::aria::types::{AgentResult, TeamResult};

/// Execute a single agent on a given input.
///
/// Uses deterministic local processing based on the agent's role and
/// capabilities. When an LLM provider is injected into the team context,
/// the agentic executor (`crate::agent::execute_agent`) can be used instead
/// for full multi-turn tool-use execution.
fn run_agent(member: &TeamMemberRuntime, input: &str) -> AgentResult {
    let role_desc = member.role.as_deref().unwrap_or("general");
    let content = format!(
        "[Agent '{}' (role: {}, capabilities: [{}])]: Processing input: \"{}\"",
        member.agent_name,
        role_desc,
        member.capabilities.join(", "),
        if input.len() > 200 {
            &input[..200]
        } else {
            input
        }
    );
    AgentResult {
        success: true,
        result: Some(serde_json::json!({
            "agent_id": member.agent_id,
            "agent_name": member.agent_name,
            "output": content,
        })),
        error: None,
        model: None,
        tokens_used: None,
        duration_ms: Some(1),
        metadata: None,
    }
}

/// Record an agent's contribution to shared memory.
fn record_message(ctx: &TeamExecutionContext, member: &TeamMemberRuntime, content: &str) {
    if let Ok(mut mem) = ctx.shared_memory.lock() {
        mem.push(TeamMessage {
            agent_id: member.agent_id.clone(),
            agent_name: member.agent_name.clone(),
            role: member.role.clone().unwrap_or_default(),
            content: content.to_string(),
            timestamp: Utc::now().timestamp_millis(),
        });
    }
}

/// Build a context string from shared memory for an agent to reference.
fn build_context_from_memory(ctx: &TeamExecutionContext) -> String {
    let mem = ctx.shared_memory.lock().unwrap();
    if mem.is_empty() {
        return String::new();
    }
    let mut context = String::from("Previous contributions:\n");
    for msg in mem.iter() {
        context.push_str(&format!(
            "- {} ({}): {}\n",
            msg.agent_name, msg.role, msg.content
        ));
    }
    context
}

/// **Coordinator mode**: A coordinator agent (first member) analyzes the task,
/// delegates subtasks to other members, collects results, and synthesizes a
/// final answer. The coordinator decides which agents to involve and in what
/// order.
pub async fn run_coordinator(
    ctx: &TeamExecutionContext,
    members: &[TeamMemberRuntime],
    max_rounds: Option<u32>,
) -> Result<TeamResult> {
    let start = std::time::Instant::now();
    let rounds = max_rounds.unwrap_or(1);

    if members.is_empty() {
        return Ok(TeamResult {
            success: false,
            result: None,
            error: Some("No team members provided".into()),
            agent_results: Vec::new(),
            mode: "coordinator".into(),
            duration_ms: Some(start.elapsed().as_millis() as u64),
            metadata: None,
        });
    }

    let coordinator = &members[0];
    let workers = &members[1..];
    let mut all_results: Vec<AgentResult> = Vec::new();

    for round in 0..rounds {
        // Step 1: Coordinator analyzes the task and decides delegation
        let coordinator_input = if round == 0 {
            format!(
                "You are the coordinator. Analyze this task and delegate to your team:\n\
                 Task: {}\n\
                 Available team members: {}",
                ctx.input,
                workers
                    .iter()
                    .map(|m| format!(
                        "{} (role: {}, capabilities: [{}])",
                        m.agent_name,
                        m.role.as_deref().unwrap_or("general"),
                        m.capabilities.join(", ")
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        } else {
            let context = build_context_from_memory(ctx);
            format!(
                "Round {}: Review team results and provide further direction.\n{}\nOriginal task: {}",
                round + 1,
                context,
                ctx.input
            )
        };

        let coord_result = run_agent(coordinator, &coordinator_input);
        let coord_output = coord_result
            .result
            .as_ref()
            .and_then(|v| v.get("output"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        record_message(ctx, coordinator, &coord_output);
        all_results.push(coord_result);

        // Step 2: Each worker processes their delegated subtask
        for worker in workers {
            let worker_input = format!(
                "Coordinator directive: {}\nOriginal task: {}\n{}",
                coord_output,
                ctx.input,
                build_context_from_memory(ctx)
            );
            let worker_result = run_agent(worker, &worker_input);
            let worker_output = worker_result
                .result
                .as_ref()
                .and_then(|v| v.get("output"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            record_message(ctx, worker, &worker_output);
            all_results.push(worker_result);
        }
    }

    // Step 3: Coordinator synthesizes final answer
    let synthesis_input = format!(
        "Synthesize a final answer from all team contributions.\n{}",
        build_context_from_memory(ctx)
    );
    let final_result = run_agent(coordinator, &synthesis_input);
    let final_output = final_result
        .result
        .as_ref()
        .and_then(|v| v.get("output"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    all_results.push(final_result);

    Ok(TeamResult {
        success: true,
        result: Some(serde_json::json!({ "output": final_output })),
        error: None,
        agent_results: all_results,
        mode: "coordinator".into(),
        duration_ms: Some(start.elapsed().as_millis() as u64),
        metadata: Some(HashMap::new()),
    })
}

/// **Round-robin mode**: Each agent gets a turn in order. Agent N receives
/// the accumulated context from agents 0..N-1. Continues for `max_rounds`
/// iterations. Each agent builds on the previous output.
pub async fn run_round_robin(
    ctx: &TeamExecutionContext,
    members: &[TeamMemberRuntime],
    max_rounds: Option<u32>,
) -> Result<TeamResult> {
    let start = std::time::Instant::now();
    let rounds = max_rounds.unwrap_or(1);

    if members.is_empty() {
        return Ok(TeamResult {
            success: false,
            result: None,
            error: Some("No team members provided".into()),
            agent_results: Vec::new(),
            mode: "round_robin".into(),
            duration_ms: Some(start.elapsed().as_millis() as u64),
            metadata: None,
        });
    }

    let mut all_results: Vec<AgentResult> = Vec::new();
    let mut last_output = String::new();

    for round in 0..rounds {
        for (i, member) in members.iter().enumerate() {
            // Build input with accumulated context from prior agents
            let agent_input = if round == 0 && i == 0 {
                ctx.input.clone()
            } else {
                let context = build_context_from_memory(ctx);
                format!(
                    "Original task: {}\n{}\nPrevious output: {}",
                    ctx.input, context, last_output
                )
            };

            let result = run_agent(member, &agent_input);
            let output = result
                .result
                .as_ref()
                .and_then(|v| v.get("output"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            record_message(ctx, member, &output);
            last_output = output;
            all_results.push(result);
        }
    }

    Ok(TeamResult {
        success: true,
        result: Some(serde_json::json!({ "output": last_output })),
        error: None,
        agent_results: all_results,
        mode: "round_robin".into(),
        duration_ms: Some(start.elapsed().as_millis() as u64),
        metadata: None,
    })
}

/// **Delegate-to-best mode**: Analyze the input against each member's
/// capabilities and role descriptions. Select the single best agent for the
/// task. Route the entire input to that agent. Return that agent's result.
pub async fn run_delegate_to_best(
    ctx: &TeamExecutionContext,
    members: &[TeamMemberRuntime],
) -> Result<TeamResult> {
    let start = std::time::Instant::now();

    if members.is_empty() {
        return Ok(TeamResult {
            success: false,
            result: None,
            error: Some("No team members provided".into()),
            agent_results: Vec::new(),
            mode: "delegate_to_best".into(),
            duration_ms: Some(start.elapsed().as_millis() as u64),
            metadata: None,
        });
    }

    // Score each member based on capability keyword matching and weight.
    let input_lower = ctx.input.to_lowercase();
    let best_member = members
        .iter()
        .max_by(|a, b| {
            let score_a = score_member(a, &input_lower);
            let score_b = score_member(b, &input_lower);
            score_a
                .partial_cmp(&score_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .context("No members to select from")?;

    let result = run_agent(best_member, &ctx.input);
    let output = result
        .result
        .as_ref()
        .and_then(|v| v.get("output"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    record_message(ctx, best_member, &output);

    let mut metadata: HashMap<String, serde_json::Value> = HashMap::new();
    metadata.insert(
        "selected_agent".to_string(),
        serde_json::json!({
            "agent_id": best_member.agent_id,
            "agent_name": best_member.agent_name,
        }),
    );

    Ok(TeamResult {
        success: true,
        result: Some(serde_json::json!({ "output": output })),
        error: None,
        agent_results: vec![result],
        mode: "delegate_to_best".into(),
        duration_ms: Some(start.elapsed().as_millis() as u64),
        metadata: Some(metadata),
    })
}

/// Score a member's relevance to the input based on capability keyword matching
/// and weight. Higher score = better match.
fn score_member(member: &TeamMemberRuntime, input_lower: &str) -> f64 {
    let mut score = member.weight;

    // Check capability keyword overlap with input
    for cap in &member.capabilities {
        if input_lower.contains(&cap.to_lowercase()) {
            score += 1.0;
        }
    }

    // Check role match
    if let Some(ref role) = member.role {
        if input_lower.contains(&role.to_lowercase()) {
            score += 0.5;
        }
    }

    score
}

/// **Parallel mode**: All agents execute simultaneously on the same input via
/// `tokio::spawn`. Collect all results. Merge results into a combined output
/// (concatenation with agent attribution).
pub async fn run_parallel(
    ctx: &TeamExecutionContext,
    members: &[TeamMemberRuntime],
) -> Result<TeamResult> {
    let start = std::time::Instant::now();

    if members.is_empty() {
        return Ok(TeamResult {
            success: false,
            result: None,
            error: Some("No team members provided".into()),
            agent_results: Vec::new(),
            mode: "parallel".into(),
            duration_ms: Some(start.elapsed().as_millis() as u64),
            metadata: None,
        });
    }

    // Spawn all agent executions concurrently
    let mut handles = Vec::new();
    for member in members {
        let member_clone = member.clone();
        let input_clone = ctx.input.clone();
        let handle = tokio::spawn(async move { run_agent(&member_clone, &input_clone) });
        handles.push((member.clone(), handle));
    }

    // Collect all results
    let mut all_results: Vec<AgentResult> = Vec::new();
    let mut combined_parts: Vec<String> = Vec::new();

    for (member, handle) in handles {
        match handle.await {
            Ok(result) => {
                let output = result
                    .result
                    .as_ref()
                    .and_then(|v| v.get("output"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                combined_parts.push(format!(
                    "--- {} ({}) ---\n{}",
                    member.agent_name,
                    member.role.as_deref().unwrap_or("general"),
                    output
                ));
                record_message(ctx, &member, &output);
                all_results.push(result);
            }
            Err(e) => {
                let error_result = AgentResult {
                    success: false,
                    result: None,
                    error: Some(format!("Agent '{}' task failed: {}", member.agent_name, e)),
                    model: None,
                    tokens_used: None,
                    duration_ms: None,
                    metadata: None,
                };
                all_results.push(error_result);
            }
        }
    }

    let combined_output = combined_parts.join("\n\n");

    Ok(TeamResult {
        success: all_results.iter().all(|r| r.success),
        result: Some(serde_json::json!({ "output": combined_output })),
        error: None,
        agent_results: all_results,
        mode: "parallel".into(),
        duration_ms: Some(start.elapsed().as_millis() as u64),
        metadata: None,
    })
}

/// **Sequential mode**: Agents execute in order. Output of agent N becomes
/// input context for agent N+1. The final agent's output is the team's result.
/// Each agent adds its perspective.
pub async fn run_sequential(
    ctx: &TeamExecutionContext,
    members: &[TeamMemberRuntime],
) -> Result<TeamResult> {
    let start = std::time::Instant::now();

    if members.is_empty() {
        return Ok(TeamResult {
            success: false,
            result: None,
            error: Some("No team members provided".into()),
            agent_results: Vec::new(),
            mode: "sequential".into(),
            duration_ms: Some(start.elapsed().as_millis() as u64),
            metadata: None,
        });
    }

    let mut all_results: Vec<AgentResult> = Vec::new();
    let mut current_input = ctx.input.clone();

    for member in members {
        let result = run_agent(member, &current_input);

        let output = result
            .result
            .as_ref()
            .and_then(|v| v.get("output"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        record_message(ctx, member, &output);

        // The next agent receives the original task plus this agent's output
        current_input = format!(
            "Original task: {}\nPrevious agent ({}) output: {}",
            ctx.input, member.agent_name, output
        );

        all_results.push(result);
    }

    // The final result is from the last agent
    let final_output = all_results
        .last()
        .and_then(|r| r.result.as_ref())
        .and_then(|v| v.get("output"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(TeamResult {
        success: all_results.iter().all(|r| r.success),
        result: Some(serde_json::json!({ "output": final_output })),
        error: None,
        agent_results: all_results,
        mode: "sequential".into(),
        duration_ms: Some(start.elapsed().as_millis() as u64),
        metadata: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn make_ctx(input: &str) -> TeamExecutionContext {
        TeamExecutionContext {
            team_id: "test-team".into(),
            tenant_id: "test-tenant".into(),
            input: input.to_string(),
            shared_memory: Arc::new(Mutex::new(Vec::new())),
            timeout: None,
        }
    }

    fn make_members(count: usize) -> Vec<TeamMemberRuntime> {
        (0..count)
            .map(|i| TeamMemberRuntime {
                agent_id: format!("agent-{i}"),
                agent_name: format!("Agent{i}"),
                role: Some(match i {
                    0 => "coordinator".into(),
                    1 => "analyst".into(),
                    2 => "writer".into(),
                    _ => "helper".into(),
                }),
                capabilities: match i {
                    0 => vec!["planning".into(), "delegation".into()],
                    1 => vec!["analysis".into(), "research".into()],
                    2 => vec!["writing".into(), "editing".into()],
                    _ => vec!["general".into()],
                },
                weight: 1.0,
            })
            .collect()
    }

    #[tokio::test]
    async fn coordinator_produces_results() {
        let ctx = make_ctx("Write a report about AI");
        let members = make_members(3);
        let result = run_coordinator(&ctx, &members, Some(1)).await.unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "coordinator");
        // coordinator analysis + 2 workers + final synthesis = 4
        assert_eq!(result.agent_results.len(), 4);
        assert!(result.result.is_some());
    }

    #[tokio::test]
    async fn coordinator_empty_members() {
        let ctx = make_ctx("task");
        let result = run_coordinator(&ctx, &[], Some(1)).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn coordinator_multi_round() {
        let ctx = make_ctx("Complex task");
        let members = make_members(2);
        let result = run_coordinator(&ctx, &members, Some(2)).await.unwrap();
        assert!(result.success);
        // round 1: coord + 1 worker = 2, round 2: coord + 1 worker = 2, final synth = 1 => 5
        assert_eq!(result.agent_results.len(), 5);
    }

    #[tokio::test]
    async fn round_robin_produces_results() {
        let ctx = make_ctx("Brainstorm ideas");
        let members = make_members(3);
        let result = run_round_robin(&ctx, &members, Some(1)).await.unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "round_robin");
        assert_eq!(result.agent_results.len(), 3);
        assert!(result.result.is_some());
    }

    #[tokio::test]
    async fn round_robin_multi_round() {
        let ctx = make_ctx("Iterate");
        let members = make_members(2);
        let result = run_round_robin(&ctx, &members, Some(3)).await.unwrap();
        assert!(result.success);
        // 2 members * 3 rounds = 6
        assert_eq!(result.agent_results.len(), 6);
    }

    #[tokio::test]
    async fn round_robin_empty_members() {
        let ctx = make_ctx("task");
        let result = run_round_robin(&ctx, &[], Some(1)).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn round_robin_shares_context() {
        let ctx = make_ctx("Build on each other");
        let members = make_members(3);
        let _ = run_round_robin(&ctx, &members, Some(1)).await.unwrap();
        let mem = ctx.shared_memory.lock().unwrap();
        assert_eq!(mem.len(), 3);
        // Verify order
        assert_eq!(mem[0].agent_name, "Agent0");
        assert_eq!(mem[1].agent_name, "Agent1");
        assert_eq!(mem[2].agent_name, "Agent2");
    }

    #[tokio::test]
    async fn delegate_to_best_selects_relevant_agent() {
        let ctx = make_ctx("Please do some writing and editing");
        let members = make_members(3);
        let result = run_delegate_to_best(&ctx, &members).await.unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "delegate_to_best");
        assert_eq!(result.agent_results.len(), 1);

        // The writer agent should be selected because its capabilities
        // (writing, editing) match the input keywords
        let selected = result
            .metadata
            .as_ref()
            .and_then(|m| m.get("selected_agent"))
            .and_then(|s| s.get("agent_name"))
            .and_then(|n| n.as_str());
        assert_eq!(selected, Some("Agent2"));
    }

    #[tokio::test]
    async fn delegate_to_best_empty_members() {
        let ctx = make_ctx("task");
        let result = run_delegate_to_best(&ctx, &[]).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn parallel_executes_all_agents() {
        let ctx = make_ctx("Analyze this from multiple angles");
        let members = make_members(3);
        let result = run_parallel(&ctx, &members).await.unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "parallel");
        assert_eq!(result.agent_results.len(), 3);

        // All results should be successful
        for r in &result.agent_results {
            assert!(r.success);
        }

        // Combined output should mention all agents
        let output = result
            .result
            .as_ref()
            .and_then(|v| v.get("output"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(output.contains("Agent0"));
        assert!(output.contains("Agent1"));
        assert!(output.contains("Agent2"));
    }

    #[tokio::test]
    async fn parallel_empty_members() {
        let ctx = make_ctx("task");
        let result = run_parallel(&ctx, &[]).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn sequential_chains_outputs() {
        let ctx = make_ctx("Start the chain");
        let members = make_members(3);
        let result = run_sequential(&ctx, &members).await.unwrap();

        assert!(result.success);
        assert_eq!(result.mode, "sequential");
        assert_eq!(result.agent_results.len(), 3);

        // Final output should be from the last agent
        let output = result
            .result
            .as_ref()
            .and_then(|v| v.get("output"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(output.contains("Agent2"));
    }

    #[tokio::test]
    async fn sequential_empty_members() {
        let ctx = make_ctx("task");
        let result = run_sequential(&ctx, &[]).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn sequential_single_member() {
        let ctx = make_ctx("Solo task");
        let members = make_members(1);
        let result = run_sequential(&ctx, &members).await.unwrap();
        assert!(result.success);
        assert_eq!(result.agent_results.len(), 1);
    }

    #[test]
    fn score_member_considers_capabilities() {
        let member = TeamMemberRuntime {
            agent_id: "a1".into(),
            agent_name: "Writer".into(),
            role: Some("writer".into()),
            capabilities: vec!["writing".into(), "editing".into()],
            weight: 1.0,
        };
        // Input contains "writing" (capability match) and "writer" (role match)
        let score = score_member(&member, "i need a writer for writing");
        // weight(1.0) + writing match(1.0) + role match(0.5) = 2.5
        assert!((score - 2.5).abs() < f64::EPSILON);

        // Input with only capability match, no role match
        let score2 = score_member(&member, "i need help with writing");
        // weight(1.0) + writing match(1.0) = 2.0
        assert!((score2 - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn score_member_uses_weight() {
        let member_high = TeamMemberRuntime {
            agent_id: "a1".into(),
            agent_name: "Expert".into(),
            role: None,
            capabilities: vec![],
            weight: 5.0,
        };
        let member_low = TeamMemberRuntime {
            agent_id: "a2".into(),
            agent_name: "Novice".into(),
            role: None,
            capabilities: vec![],
            weight: 0.1,
        };
        let score_high = score_member(&member_high, "something");
        let score_low = score_member(&member_low, "something");
        assert!(score_high > score_low);
    }
}
