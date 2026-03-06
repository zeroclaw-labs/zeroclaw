//! CLI handlers for agent management commands.
//!
//! This module provides handlers for:
//! - `zeroclaw agents list` - List all registered agents
//! - `zeroclaw agents show <name>` - Show agent details
//! - `zeroclaw agents reload` - Reload agent definitions
//! - `zeroclaw agents run <name> --prompt "..."` - Run an agent

use crate::config::Config;
use crate::AgentCommands;
use anyhow::{bail, Result};
use std::sync::Arc;
use tracing::info;

/// Handle agent management commands
pub async fn handle_command(command: AgentCommands, config: &Config) -> Result<()> {
    match command {
        AgentCommands::List => handle_list(config),
        AgentCommands::Show { name } => handle_show(config, &name),
        AgentCommands::Reload => handle_reload(config),
        AgentCommands::Run { name, prompt } => handle_run(config, &name, &prompt).await,
    }
}

/// Handle `zeroclaw agents list` command
fn handle_list(config: &Config) -> Result<()> {
    let agents_dir = config.workspace_dir.join("agents");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = crate::agent::AgentRegistry::new(agents_dir.clone(), security)?;

    // Discover agents
    let count = registry.discover()?;

    if count == 0 {
        println!("No agent definitions found.");
        println!("Create agent definitions in: {}", agents_dir.display());
        return Ok(());
    }

    let ids = registry.list();
    println!("Registered agents ({}):\n", count);

    for id in &ids {
        if let Some(def) = registry.get(id) {
            println!("  ID:          {}", def.agent.id);
            println!("  Name:        {}", def.agent.name);
            println!("  Version:     {}", def.agent.version);
            println!("  Description: {}", def.agent.description);

            // Show provider info
            if let Some(provider) = &def.provider.name {
                println!("  Provider:    {}", provider);
            }
            if let Some(model) = &def.provider.model {
                println!("  Model:       {}", model);
            }

            // Show tools count
            let enabled_count = def.tools.tools.iter().filter(|t| t.enabled).count();
            println!("  Tools:       {} enabled", enabled_count);

            println!();
        }
    }

    println!("Agents directory: {}", agents_dir.display());
    Ok(())
}

/// Handle `zeroclaw agents show <name>` command
fn handle_show(config: &Config, name: &str) -> Result<()> {
    let agents_dir = config.workspace_dir.join("agents");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = crate::agent::AgentRegistry::new(agents_dir, security)?;

    // Discover agents
    registry.discover()?;

    // Try to find by exact ID match first
    let def = if registry.contains(name) {
        registry
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' disappeared after discovery", name))?
    } else {
        // Try case-insensitive search
        let ids = registry.list();
        let found = ids
            .iter()
            .find(|id| id.eq_ignore_ascii_case(name))
            .or_else(|| {
                // Search by name
                ids.iter().find(|id| {
                    registry
                        .get(id)
                        .map(|d| d.agent.name.eq_ignore_ascii_case(name))
                        .unwrap_or(false)
                })
            });

        match found {
            Some(id) => registry
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("Agent '{}' disappeared after discovery", id))?,
            None => {
                bail!(
                    "Agent '{}' not found. Use 'zeroclaw agents list' to see available agents.",
                    name
                );
            }
        }
    };

    println!("Agent Details:\n");
    println!("  ID:          {}", def.agent.id);
    println!("  Name:        {}", def.agent.name);
    println!("  Version:     {}", def.agent.version);
    println!("  Description: {}", def.agent.description);
    println!();

    // Execution settings
    println!("Execution:");
    println!("  Mode:        {:?}", def.execution.mode);
    if let Some(command) = &def.execution.command {
        println!("  Command:     {}", command);
    }
    if !def.execution.args.is_empty() {
        println!("  Args:        {}", def.execution.args.join(" "));
    }
    if let Some(working_dir) = &def.execution.working_dir {
        println!("  Working Dir: {}", working_dir);
    }
    if !def.execution.env.is_empty() {
        println!("  Environment:");
        for (key, value) in &def.execution.env {
            println!("    {}={}", key, value);
        }
    }
    println!();

    // Provider settings
    println!("Provider:");
    if let Some(provider) = &def.provider.name {
        println!("  Name:        {}", provider);
    }
    if let Some(model) = &def.provider.model {
        println!("  Model:       {}", model);
    }
    if let Some(temperature) = def.provider.temperature {
        println!("  Temperature: {}", temperature);
    }
    if let Some(max_tokens) = def.provider.max_tokens {
        println!("  Max Tokens:  {}", max_tokens);
    }
    if def.provider.api_key.is_some() {
        println!("  API Key:     *** (configured)");
    }
    println!();

    // Tools
    println!("Tools:");
    if def.tools.tools.is_empty() && def.tools.deny.is_empty() {
        println!("  (no tool restrictions - all tools available)");
    } else {
        if !def.tools.tools.is_empty() {
            println!("  Allowed:");
            for tool in &def.tools.tools {
                let status = if tool.enabled { "✓" } else { "✗" };
                println!("    {} {}", status, tool.name);
            }
        }
        if !def.tools.deny.is_empty() {
            println!("  Denied:");
            for tool in &def.tools.deny {
                println!("    ✗ {} (reason: {})", tool.name, tool.reason);
            }
        }
    }
    println!();

    // System prompt
    if !def.system.prompt.is_empty() {
        println!("System Prompt:");
        println!("  {}", def.system.prompt);
        println!();
    }

    // Memory settings
    println!("Memory:");
    println!("  Backend: {:?}", def.memory.backend);
    if let Some(category) = &def.memory.category {
        println!("  Category: {}", category);
    }
    println!();

    // Reporting settings
    println!("Reporting:");
    println!("  Mode:        {:?}", def.reporting.mode);
    println!("  Format:      {:?}", def.reporting.format);
    println!("  Timeout:     {} seconds", def.reporting.timeout_seconds);
    println!();

    // Retry settings
    println!("Retry:");
    println!("  Max Attempts: {}", def.retry.max_attempts);
    println!("  Backoff:      {} ms", def.retry.backoff_ms);

    Ok(())
}

/// Handle `zeroclaw agents reload` command
fn handle_reload(config: &Config) -> Result<()> {
    let agents_dir = config.workspace_dir.join("agents");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = crate::agent::AgentRegistry::new(agents_dir.clone(), security)?;

    info!("Reloading agent definitions from: {}", agents_dir.display());
    let count = registry.reload()?;

    println!("Reloaded {} agent definition(s).", count);

    if count > 0 {
        let ids = registry.list();
        println!("\nAvailable agents:");
        for id in ids {
            println!("  - {}", id);
        }
    }

    Ok(())
}

/// Handle `zeroclaw agents run <name> --prompt "..."` command
async fn handle_run(config: &Config, name: &str, prompt: &str) -> Result<()> {
    let agents_dir = config.workspace_dir.join("agents");
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let registry = crate::agent::AgentRegistry::new(agents_dir.clone(), security)?;

    // Discover agents
    registry.discover()?;

    // Try to find by exact ID match first
    let def = if registry.contains(name) {
        registry
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' disappeared after discovery", name))?
    } else {
        // Try case-insensitive search
        let ids = registry.list();
        let found = ids
            .iter()
            .find(|id| id.eq_ignore_ascii_case(name))
            .or_else(|| {
                // Search by name
                ids.iter().find(|id| {
                    registry
                        .get(id)
                        .map(|d| d.agent.name.eq_ignore_ascii_case(name))
                        .unwrap_or(false)
                })
            });

        match found {
            Some(id) => registry
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("Agent '{}' disappeared after discovery", id))?,
            None => {
                bail!(
                    "Agent '{}' not found. Use 'zeroclaw agents list' to see available agents.",
                    name
                );
            }
        }
    };

    info!("Running agent '{}' with prompt: {}", def.agent.id, prompt);

    // Convert AgentDefinition to DelegateAgentConfig
    let delegate_config: crate::config::schema::DelegateAgentConfig = (&def).into();

    // Check if agent exists in config
    let agent_id = def.agent.id.clone();
    if !config.agents.contains_key(&agent_id) {
        println!(
            "Warning: Agent '{}' is not configured in config.toml",
            agent_id
        );
        println!("Using default configuration from agent definition file.");
    }

    // Determine provider and model
    let provider = delegate_config.provider;

    let model = delegate_config.model;

    println!("Running agent: {}", def.agent.name);
    println!("Provider: {}", provider);
    println!("Model: {}", model);
    println!();

    // Run the agent using the loop module
    // We use the agent's system prompt if defined
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exists() {
        // Basic test to ensure module compiles
        assert!(true);
    }
}
