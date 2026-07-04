use std::sync::Arc;
use zeroclaw_api::memory_traits::{Memory, MemoryStrategy};
use zeroclaw_api::model_provider::ModelProvider;

/// Default memory strategy that delegates to existing implementations.
///
/// A thin wrapper over the memory lifecycle: it calls
/// `consolidation::consolidate_turn` and `hygiene::run_if_due` directly.
/// Context assembly is not the strategy's job; the turn engine renders the
/// memory-context preamble via `agent::memory_inject`, keyed on `TurnOrigin`.
pub struct DefaultMemoryStrategy {
    memory: Arc<dyn Memory>,
    memory_config: zeroclaw_config::schema::MemoryConfig,
    workspace_dir: std::path::PathBuf,
}

impl DefaultMemoryStrategy {
    pub fn new(
        memory: Arc<dyn Memory>,
        memory_config: zeroclaw_config::schema::MemoryConfig,
        workspace_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        // #6722: rerank_enabled is declared on the config schema but the
        // retrieval-pipeline rerank stage was never landed (PR #4245 closed
        // unmerged).  Emit a one-time warning so operators who set these
        // fields know they currently have no effect.
        if memory_config.rerank_enabled {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "rerank_enabled": true,
                        "rerank_threshold": memory_config.rerank_threshold,
                    })),
                "memory.rerank_enabled is set but the rerank stage is not yet implemented; this setting currently has no effect"
            );
        }
        Self {
            memory,
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
            &self.memory_config,
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
