use crate::config::{AgentTeamsConfig, Config, SubAgentsConfig};
use std::path::Path;

/// Load orchestration settings from `config.toml` for runtime hot-apply.
///
/// This intentionally reads only config data and does not mutate global state.
pub fn load_orchestration_settings(
    config_path: &Path,
) -> anyhow::Result<(AgentTeamsConfig, SubAgentsConfig)> {
    let contents = std::fs::read_to_string(config_path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", config_path.display()))?;
    let parsed: Config = toml::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", config_path.display()))?;
    Ok((parsed.agent.teams, parsed.agent.subagents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_orchestration_settings_reads_agent_controls() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7

[agent.teams]
enabled = false
auto_activate = false
max_agents = 3

[agent.subagents]
enabled = true
auto_activate = false
max_concurrent = 2
"#,
        )
        .unwrap();

        let (teams, subagents) = load_orchestration_settings(&path).unwrap();
        assert!(!teams.enabled);
        assert!(!teams.auto_activate);
        assert_eq!(teams.max_agents, 3);
        assert!(subagents.enabled);
        assert!(!subagents.auto_activate);
        assert_eq!(subagents.max_concurrent, 2);
    }
}
