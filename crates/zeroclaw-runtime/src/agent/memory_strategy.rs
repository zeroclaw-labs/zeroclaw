use std::sync::Arc;
use zeroclaw_api::memory_traits::{Memory, MemoryStrategy};
use zeroclaw_api::model_provider::ModelProvider;

use crate::agent::memory_loader::{DefaultMemoryLoader, MemoryLoader};

/// Default memory strategy that delegates to existing implementations.
///
/// Phase 1: This is a thin wrapper. It does not duplicate logic;
/// it calls `DefaultMemoryLoader`, `consolidation::consolidate_turn`,
/// and `hygiene::run_if_due` directly, preserving current behavior
/// byte-for-byte.
pub struct DefaultMemoryStrategy {
    memory: Arc<dyn Memory>,
    limit: usize,
    min_relevance_score: f64,
    memory_config: zeroclaw_config::schema::MemoryConfig,
    workspace_dir: std::path::PathBuf,
}

impl DefaultMemoryStrategy {
    pub fn new(
        memory: Arc<dyn Memory>,
        memory_config: zeroclaw_config::schema::MemoryConfig,
        workspace_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            memory,
            limit: 5,
            min_relevance_score: memory_config.min_relevance_score,
            memory_config,
            workspace_dir: workspace_dir.into(),
        }
    }

    /// Convenience constructor that takes the live `MemoryConfig` so
    /// `run_governance` uses the operator's actual settings (archive
    /// windows, hygiene toggle, etc.) rather than hardcoded defaults.
    pub fn with_config(
        memory: Arc<dyn Memory>,
        memory_config: zeroclaw_config::schema::MemoryConfig,
        workspace_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new(memory, memory_config, workspace_dir)
    }
}

#[async_trait::async_trait]
impl MemoryStrategy for DefaultMemoryStrategy {
    async fn load_context(&self, query: &str, session_id: Option<&str>) -> anyhow::Result<String> {
        let loader = DefaultMemoryLoader::new(self.limit, self.min_relevance_score);
        loader
            .load_context(self.memory.as_ref(), query, session_id)
            .await
    }

    async fn consolidate_turn(
        &self,
        user_message: &str,
        assistant_response: &str,
        provider: &dyn ModelProvider,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<()> {
        zeroclaw_memory::consolidation::consolidate_turn(
            provider,
            model,
            temperature,
            self.memory.as_ref(),
            user_message,
            assistant_response,
        )
        .await
    }

    async fn run_governance(&self) -> anyhow::Result<()> {
        // Delegate to the existing hygiene routine.
        // Phase 1: `hygiene::run_if_due` returns `Result<()>`.
        // A structured report will be wired in a follow-up when hygiene
        // exposes per-action counters.
        zeroclaw_memory::hygiene::run_if_due(&self.memory_config, &self.workspace_dir)
    }
}
