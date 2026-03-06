//! CLI handlers for team management commands.
//!
//! This module provides handlers for:
//! - `zeroclaw teams list` - List all registered teams
//! - `zeroclaw teams show <name>` - Show team details
//! - `zeroclaw teams reload` - Reload team definitions
//! - `zeroclaw teams run <name> <prompt>` - Run a team with a task

use crate::agent::team_registry::TeamRegistry;
use crate::agent::TeamTopologyType;
use crate::config::Config;
use crate::TeamCommands;
use anyhow::{bail, Result};
use std::sync::Arc;
use tracing::info;

/// Handle team management commands
pub async fn handle_command(command: TeamCommands, config: &Config) -> Result<()> {
    match command {
        TeamCommands::List => handle_list(config),
        TeamCommands::Show { name } => handle_show(config, &name),
        TeamCommands::Reload => handle_reload(config),
        TeamCommands::Run { name, prompt } => handle_run(config, &name, &prompt).await,
    }
}

/// Handle `zeroclaw teams list` command
fn handle_list(config: &Config) -> Result<()> {
    use tokio::runtime::Runtime;

    let teams_dir = config.workspace_dir.join("teams");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = TeamRegistry::new(teams_dir, security)?;

    let rt = Runtime::new()?;
    let count = rt.block_on(registry.discover())?;

    if count == 0 {
        println!("No team definitions found.");
        println!("Create team definitions in: {}", teams_dir.display());
        return Ok(());
    }

    let ids = rt.block_on(registry.list())?;
    println!("Registered teams ({}):\n", count);

    for id in &ids {
        if let Some(team) = rt.block_on(registry.get(id)) {
            println!("  ID:          {}", team.id());
            println!("  Name:        {}", team.name());
            println!("  Version:     {}", team.team.version);
            println!("  Description: {}", team.team.description);
            println!("  Topology:    {:?}", team.topology_type());
            println!("  Members:     {}", team.members.len());
            if let Some(lead) = team.lead_agent_id() {
                println!("  Lead:        {}", lead);
            }
            println!();
        }
    }

    println!("Teams directory: {}", teams_dir.display());
    Ok(())
}

/// Handle `zeroclaw teams show <name>` command
fn handle_show(config: &Config, name: &str) -> Result<()> {
    use tokio::runtime::Runtime;

    let teams_dir = config.workspace_dir.join("teams");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = TeamRegistry::new(teams_dir, security)?;

    let rt = Runtime::new()?;
    rt.block_on(registry.discover())?;

    // Try to find by exact ID match first
    let team = if rt.block_on(registry.contains(name)) {
        rt.block_on(registry.get(name))
            .ok_or_else(|| anyhow::anyhow!("Team '{}' disappeared after discovery", name))?
    } else {
        // Try case-insensitive search
        let ids = rt.block_on(registry.list())?;
        let found = ids
            .iter()
            .find(|id| id.eq_ignore_ascii_case(name))
            .or_else(|| {
                // Search by name
                ids.iter().find(|id| {
                    rt.block_on(registry.get(id))
                        .map(|t| t.name().eq_ignore_ascii_case(name))
                        .unwrap_or(false)
                })
            });

        match found {
            Some(id) => rt
                .block_on(registry.get(id))
                .ok_or_else(|| anyhow::anyhow!("Team '{}' disappeared after discovery", id))?,
            None => {
                bail!(
                    "Team '{}' not found. Use 'zeroclaw teams list' to see available teams.",
                    name
                );
            }
        }
    };

    println!("Team Details:\n");
    println!("  ID:          {}", team.id());
    println!("  Name:        {}", team.name());
    println!("  Version:     {}", team.team.version);
    println!("  Description: {}", team.team.description);
    println!("  Topology:    {:?}", team.topology_type());
    println!();

    // Members
    println!("Members ({}):", team.members.len());
    for member in &team.members {
        println!("  - Agent:             {}", member.agent_id);
        println!("    Role:              {:?}", member.role);
        println!("    Max Concurrent:    {}", member.max_concurrent_tasks);
        if !member.capabilities.is_empty() {
            println!("    Capabilities:      {}", member.capabilities.join(", "));
        }
    }
    println!();

    // Topology settings
    println!("Topology:");
    println!("  Type:              {:?}", team.topology.topology_type);
    if let Some(lead) = &team.topology.lead {
        println!("  Lead Agent:        {}", lead.agent_id);
        println!("  Max Delegates:     {}", lead.max_delegates);
        println!(
            "  Handoff Timeout:   {} seconds",
            lead.handoff_timeout_seconds
        );
    }
    println!();

    // Coordination settings
    println!("Coordination:");
    println!("  Protocol:          {:?}", team.coordination.protocol);
    println!(
        "  Max Round Trips:   {}",
        team.coordination.max_round_trips
    );
    println!(
        "  Sync Interval:     {} ms",
        team.coordination.sync_interval_ms
    );
    println!(
        "  Message Budget:    {} per task",
        team.coordination.message_budget_per_task
    );
    println!();

    // Budget settings
    println!("Budget:");
    println!("  Tier:              {:?}", team.budget.tier);
    println!(
        "  Summary Cap:       {} tokens",
        team.budget.summary_cap_tokens
    );
    println!("  Max Workers:       {}", team.budget.max_workers);
    println!();

    // Workload settings
    println!("Workload:");
    println!("  Type:              {:?}", team.workload.workload_type);
    println!();

    // Degradation settings
    println!("Degradation:");
    println!("  Policy:            {:?}", team.degradation.policy);
    println!(
        "  Max Downgrades:    {}",
        team.degradation.max_topology_downgrades
    );

    Ok(())
}

/// Handle `zeroclaw teams reload` command
fn handle_reload(config: &Config) -> Result<()> {
    use tokio::runtime::Runtime;

    let teams_dir = config.workspace_dir.join("teams");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = TeamRegistry::new(teams_dir.clone(), security)?;

    info!("Reloading team definitions from: {}", teams_dir.display());
    let rt = Runtime::new()?;
    let count = rt.block_on(registry.reload())?;

    println!("Reloaded {} team definition(s).", count);

    if count > 0 {
        let ids = rt.block_on(registry.list())?;
        println!("\nAvailable teams:");
        for id in ids {
            println!("  - {}", id);
        }
    }

    Ok(())
}

/// Handle `zeroclaw teams run <name> <prompt>` command
async fn handle_run(config: &Config, name: &str, prompt: &str) -> Result<()> {
    let teams_dir = config.workspace_dir.join("teams");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = TeamRegistry::new(teams_dir, security)?;

    // Discover teams
    registry.discover().await?;

    // Try to find by exact ID match first
    let team = if registry.contains(name).await {
        registry
            .get(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Team '{}' disappeared after discovery", name))?
    } else {
        // Try case-insensitive search
        let ids = registry.list().await;
        let found = ids
            .iter()
            .find(|id| id.eq_ignore_ascii_case(name))
            .or_else(|| {
                // Search by name
                ids.iter().find(|id| {
                    registry
                        .get(id)
                        .map(|t| t.name().eq_ignore_ascii_case(name))
                        .unwrap_or(false)
                })
            });

        match found {
            Some(id) => registry
                .get(id)
                .await
                .ok_or_else(|| anyhow::anyhow!("Team '{}' disappeared after discovery", id))?,
            None => {
                bail!(
                    "Team '{}' not found. Use 'zeroclaw teams list' to see available teams.",
                    name
                );
            }
        }
    };

    // Validate team before running
    if let Err(e) = team.validate() {
        bail!("Invalid team definition: {}", e);
    }

    info!("Running team '{}' with prompt: {}", team.id(), prompt);

    println!("Running team: {}", team.name());
    println!("Topology: {:?}", team.topology_type());
    println!("Members: {}", team.members.len());
    println!();

    // Determine execution strategy based on topology
    match team.topology_type() {
        TeamTopologyType::Single => {
            // Run single agent
            if let Some(agent_id) = team.member_ids().first() {
                println!("Executing with single agent: {}", agent_id);
                return run_single_agent(config, agent_id, prompt).await;
            }
            bail!("Team has no agents to run");
        }
        TeamTopologyType::LeadSubagent => {
            // Run lead agent with orchestration
            if let Some(lead_id) = team.lead_agent_id() {
                println!("Executing with lead agent: {}", lead_id);
                return run_with_orchestration(config, &team, lead_id, prompt).await;
            }
            bail!("LeadSubagent topology requires a lead agent");
        }
        TeamTopologyType::StarTeam | TeamTopologyType::MeshTeam => {
            // Run with full team orchestration
            if let Some(lead_id) = team.lead_agent_id() {
                println!("Executing with team orchestration, lead: {}", lead_id);
                return run_with_orchestration(config, &team, lead_id, prompt).await;
            }
            // Fall back to first member if no explicit lead
            if let Some(first_id) = team.member_ids().first() {
                println!("Executing with team orchestration, lead: {}", first_id);
                return run_with_orchestration(config, &team, first_id, prompt).await;
            }
            bail!("Team has no agents to run");
        }
    }
}

/// Run a single agent for the team.
async fn run_single_agent(config: &Config, agent_id: &str, prompt: &str) -> Result<()> {
    // Load agent definition
    let agents_dir = config.workspace_dir.join("agents");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let agent_registry = crate::agent::AgentRegistry::new(agents_dir, security)?;

    // Discover agents
    agent_registry.discover()?;

    // Get the agent definition
    let agent_def = agent_registry
        .get(agent_id)
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent_id))?;

    // Convert to delegate config
    let delegate_config: crate::config::schema::DelegateAgentConfig = (&agent_def).into();

    let provider = delegate_config.provider;
    let model = delegate_config.model;
    let temperature = delegate_config.temperature.unwrap_or(0.7);

    crate::agent::run(
        config.clone(),
        Some(prompt.to_string()),
        Some(provider),
        Some(model),
        temperature,
        vec![],
        true,
        None,
    )
    .await
    .map(|_| ())
}

/// Run with team orchestration.
async fn run_with_orchestration(
    config: &Config,
    team: &crate::agent::TeamDefinition,
    lead_id: &str,
    prompt: &str,
) -> Result<()> {
    // For now, run the lead agent with team context
    // Full orchestration would require spawning subagents

    println!("Team members:");
    for agent_id in team.member_ids() {
        println!("  - {}", agent_id);
    }
    println!();

    // Run the lead agent with the prompt
    run_single_agent(config, lead_id, prompt).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exists() {
        // Basic test to ensure module compiles
        assert!(true);
    }
}
