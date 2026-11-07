//! Gateway composition of agent lifecycle operations and session storage.

pub use zeroclaw_runtime::agent_lifecycle::*;

use std::sync::Arc;
use zeroclaw_api::{memory_traits::Memory, session_events::management::AgentSessionPort};
use zeroclaw_config::schema::Config;

fn sessions(config: &Config) -> Result<Arc<dyn AgentSessionPort>, String> {
    zeroclaw_session_host::open_repository(config)
        .map(|store| Arc::new(store) as Arc<dyn AgentSessionPort>)
        .map_err(|error| error.to_string())
}

async fn open_sessions(config: &Config) -> Result<Arc<dyn AgentSessionPort>, String> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || sessions(&config))
        .await
        .map_err(|error| error.to_string())?
}

pub fn live_acp_session_count(config: &Config, alias: &str) -> anyhow::Result<usize> {
    sessions(config)
        .map_err(anyhow::Error::msg)?
        .count_live(alias)
}

pub async fn cascade_owned_state(
    config: &Config,
    memory: &Arc<dyn Memory>,
    alias: &str,
    archive_dir: &std::path::Path,
) -> OwnedStateReport {
    let sessions = open_sessions(config).await;
    zeroclaw_runtime::agent_lifecycle::cascade_owned_state(
        config,
        memory,
        alias,
        archive_dir,
        sessions,
    )
    .await
}

pub async fn cascade_rename_agent(
    config: &Config,
    memory: Option<&Arc<dyn Memory>>,
    from: &str,
    to: &str,
) -> RenameStateReport {
    let sessions = open_sessions(config).await;
    zeroclaw_runtime::agent_lifecycle::cascade_rename_agent(config, memory, from, to, sessions)
        .await
}
