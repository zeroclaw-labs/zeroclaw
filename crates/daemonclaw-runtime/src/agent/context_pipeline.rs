//! Ordered context-preparation pipeline for the agent tool-call loop.
//!
//! Runs before each LLM call to ensure history fits within the context window.
//! Stages are ordered cheap→expensive so lightweight passes (trim, prune) can
//! resolve overflow without reaching costly LLM-based compaction.
//!
//! New stages slot in by priority — lower numbers run first.

use daemonclaw_providers::ChatMessage;

/// Parameters available to every pipeline stage.
pub struct PipelineContext<'a> {
    /// Effective context window in tokens.
    pub token_budget: usize,
    /// Current loop iteration (0-indexed).
    pub iteration: usize,
    /// Model name (for provider-specific decisions).
    pub model: &'a str,
}

/// Result from a single pipeline stage.
#[derive(Debug, Default)]
pub struct StageResult {
    /// Whether this stage modified history.
    pub modified: bool,
    /// Estimated tokens freed (0 if unknown or not applicable).
    pub tokens_freed: usize,
}

/// A named, ordered pipeline stage.
pub struct Stage {
    pub name: &'static str,
    pub priority: u32,
    pub run: StageFn,
}

/// Stage function signature — takes history and context, returns result.
pub type StageFn = fn(&mut Vec<ChatMessage>, &PipelineContext<'_>) -> StageResult;

/// The context pipeline: an ordered sequence of named stages.
pub struct ContextPipeline {
    stages: Vec<Stage>,
}

impl ContextPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// Build the default pipeline with the standard preemptive stages.
    pub fn default_preemptive() -> Self {
        let mut pipeline = Self::new();
        pipeline.add(Stage {
            name: "microcompact",
            priority: 50,
            run: stage_microcompact,
        });
        pipeline.add(Stage {
            name: "fast_trim",
            priority: 100,
            run: stage_fast_trim,
        });
        pipeline.add(Stage {
            name: "history_prune",
            priority: 200,
            run: stage_history_prune,
        });
        pipeline.add(Stage {
            name: "orphan_repair",
            priority: 300,
            run: stage_orphan_repair,
        });
        pipeline.add(Stage {
            name: "normalize_system",
            priority: 400,
            run: stage_normalize_system,
        });
        pipeline
    }

    /// Insert a stage, maintaining priority order.
    pub fn add(&mut self, stage: Stage) {
        let pos = self
            .stages
            .iter()
            .position(|s| s.priority > stage.priority)
            .unwrap_or(self.stages.len());
        self.stages.insert(pos, stage);
    }

    /// Run all stages in priority order. Returns total tokens freed.
    pub fn run(&self, history: &mut Vec<ChatMessage>, ctx: &PipelineContext<'_>) -> usize {
        let mut total_freed = 0;
        for stage in &self.stages {
            let result = (stage.run)(history, ctx);
            total_freed += result.tokens_freed;
            tracing::trace!(
                stage = stage.name,
                modified = result.modified,
                tokens_freed = result.tokens_freed,
                "context pipeline stage complete"
            );
        }
        total_freed
    }
}

// ── Built-in stages ─────────────────────────────────────────────────────

const MICROCOMPACT_CLEARED: &str = "[Tool result cleared]";
const MICROCOMPACT_PROTECT_LAST: usize = 6;
const MICROCOMPACT_MIN_CHARS: usize = 500;

fn stage_microcompact(history: &mut Vec<ChatMessage>, ctx: &PipelineContext<'_>) -> StageResult {
    let estimated = super::history::estimate_history_tokens(history);
    if estimated <= ctx.token_budget {
        return StageResult::default();
    }
    let cutoff = history.len().saturating_sub(MICROCOMPACT_PROTECT_LAST);
    let mut chars_saved: usize = 0;
    for msg in &mut history[..cutoff] {
        if msg.role == "tool" && msg.content.len() > MICROCOMPACT_MIN_CHARS {
            chars_saved += msg.content.len() - MICROCOMPACT_CLEARED.len();
            msg.content = MICROCOMPACT_CLEARED.to_string();
        }
    }
    if chars_saved > 0 {
        tracing::info!(
            chars_saved,
            cleared_before = cutoff,
            "context pipeline: microcompact cleared old tool results"
        );
    }
    StageResult {
        modified: chars_saved > 0,
        tokens_freed: chars_saved / 4,
    }
}

fn stage_fast_trim(history: &mut Vec<ChatMessage>, ctx: &PipelineContext<'_>) -> StageResult {
    let estimated = super::history::estimate_history_tokens(history);
    if estimated <= ctx.token_budget {
        return StageResult::default();
    }
    let chars_saved = super::history::fast_trim_tool_results(history, 4);
    if chars_saved > 0 {
        tracing::info!(chars_saved, "context pipeline: fast_trim applied");
    }
    StageResult {
        modified: chars_saved > 0,
        tokens_freed: chars_saved / 4,
    }
}

fn stage_history_prune(history: &mut Vec<ChatMessage>, ctx: &PipelineContext<'_>) -> StageResult {
    let estimated = super::history::estimate_history_tokens(history);
    if estimated <= ctx.token_budget {
        return StageResult::default();
    }
    let before = estimated;
    let stats = super::history_pruner::prune_history(
        history,
        &super::history_pruner::HistoryPrunerConfig {
            enabled: true,
            max_tokens: ctx.token_budget,
            keep_recent: 4,
            collapse_tool_results: true,
        },
    );
    let modified = stats.dropped_messages > 0 || stats.collapsed_pairs > 0;
    if modified {
        tracing::info!(
            collapsed = stats.collapsed_pairs,
            dropped = stats.dropped_messages,
            "context pipeline: history_prune applied"
        );
    }
    let after = super::history::estimate_history_tokens(history);
    StageResult {
        modified,
        tokens_freed: before.saturating_sub(after),
    }
}

fn stage_orphan_repair(history: &mut Vec<ChatMessage>, _ctx: &PipelineContext<'_>) -> StageResult {
    let before = history.len();
    super::history_pruner::remove_orphaned_tool_messages(history);
    StageResult {
        modified: history.len() != before,
        tokens_freed: 0,
    }
}

fn stage_normalize_system(
    history: &mut Vec<ChatMessage>,
    _ctx: &PipelineContext<'_>,
) -> StageResult {
    super::history::normalize_system_messages(history);
    StageResult {
        modified: false,
        tokens_freed: 0,
    }
}
