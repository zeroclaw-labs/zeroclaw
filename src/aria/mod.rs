//! Aria SDK — the complete registry, execution, and orchestration surface area.
//!
//! All 11 registries share a single `SQLite` database initialized via `AriaRegistries::new()`.
//! The registries provide tenant-isolated persistence with in-memory caching.

pub mod db;
pub mod types;

pub mod agent_registry;
pub mod container_registry;
pub mod cron_registry;
pub mod feed_registry;
pub mod kv_registry;
pub mod memory_registry;
pub mod network_registry;
pub mod pipeline_registry;
pub mod task_registry;
pub mod team_registry;
pub mod tool_registry;

// cron_bridge depends on crate::cron which references binary-only modules (CronCommands, health).
// It is available in the binary crate but not in the library crate.
// pub mod cron_bridge;
pub mod hooks;

use db::AriaDb;
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// All 11 Aria registries, backed by a shared `SQLite` database.
pub struct AriaRegistries {
    pub db: AriaDb,
    pub tools: tool_registry::AriaToolRegistry,
    pub agents: agent_registry::AriaAgentRegistry,
    pub memory: memory_registry::AriaMemoryRegistry,
    pub tasks: task_registry::AriaTaskRegistry,
    pub feeds: feed_registry::AriaFeedRegistry,
    pub cron_functions: cron_registry::AriaCronFunctionRegistry,
    pub kv: kv_registry::AriaKvRegistry,
    pub teams: team_registry::AriaTeamRegistry,
    pub pipelines: pipeline_registry::AriaPipelineRegistry,
    pub containers: container_registry::AriaContainerRegistry,
    pub networks: network_registry::AriaNetworkRegistry,
}

impl AriaRegistries {
    /// Create all registries from a shared database handle.
    pub fn new(db: AriaDb) -> Self {
        Self {
            tools: tool_registry::AriaToolRegistry::new(db.clone()),
            agents: agent_registry::AriaAgentRegistry::new(db.clone()),
            memory: memory_registry::AriaMemoryRegistry::new(db.clone()),
            tasks: task_registry::AriaTaskRegistry::new(db.clone()),
            feeds: feed_registry::AriaFeedRegistry::new(db.clone()),
            cron_functions: cron_registry::AriaCronFunctionRegistry::new(db.clone()),
            kv: kv_registry::AriaKvRegistry::new(db.clone()),
            teams: team_registry::AriaTeamRegistry::new(db.clone()),
            pipelines: pipeline_registry::AriaPipelineRegistry::new(db.clone()),
            containers: container_registry::AriaContainerRegistry::new(db.clone()),
            networks: network_registry::AriaNetworkRegistry::new(db.clone()),
            db,
        }
    }
}

/// Global singleton for the registries instance.
static REGISTRIES: OnceLock<Arc<AriaRegistries>> = OnceLock::new();

/// Initialize the Aria registries singleton with the given database path.
/// Returns the existing instance if already initialized.
pub fn initialize_aria_registries(db_path: &Path) -> anyhow::Result<Arc<AriaRegistries>> {
    if let Some(existing) = REGISTRIES.get() {
        return Ok(existing.clone());
    }

    let db = AriaDb::open(db_path)?;
    let registries = Arc::new(AriaRegistries::new(db));

    let _ = REGISTRIES.set(registries.clone());

    Ok(REGISTRIES.get().cloned().unwrap_or(registries))
}

/// Get the global registries instance. Panics if not initialized.
pub fn get_aria_registries() -> Arc<AriaRegistries> {
    REGISTRIES
        .get()
        .cloned()
        .expect("Aria registries not initialized — call initialize_aria_registries first")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aria_registries_new_from_in_memory_db() {
        let db = AriaDb::open_in_memory().unwrap();
        let _registries = AriaRegistries::new(db);
    }
}
