//! Graph memory backend configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration for the CozoDB graph memory backend (`[memory.graph]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphConfig {
    /// Path to the CozoDB SQLite file (relative to workspace dir).
    #[serde(default = "default_db_path")]
    pub db_path: String,

    /// Embedding vector dimension for HNSW indexes.
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,

    /// Heat decay lambda (per-day exponential decay rate).
    #[serde(default = "default_heat_lambda")]
    pub heat_lambda: f64,

    /// Initial heat value for newly created nodes.
    #[serde(default = "default_initial_heat")]
    pub initial_heat: f64,

    /// Hot-node threshold — nodes above this heat are considered "hot".
    #[serde(default = "default_hot_threshold")]
    pub hot_threshold: f64,

    /// Maximum graph traversal hops for recall queries.
    #[serde(default = "default_max_hops")]
    pub max_hops: usize,

    /// Enable the autonomous synthesizer (profundo + REM).
    #[serde(default)]
    pub synthesis_enabled: bool,

    /// Daily token budget for REM synthesis (LLM calls).
    #[serde(default = "default_rem_daily_budget")]
    pub rem_daily_budget_tokens: u64,

    /// Maximum research cascade depth.
    #[serde(default = "default_max_research_depth")]
    pub max_research_depth: usize,

    /// Daily cost cap (USD) for budget controller — 0.0 = unlimited.
    #[serde(default)]
    pub daily_cost_cap_usd: f64,
}

fn default_db_path() -> String {
    "knowledge.db".into()
}

fn default_embedding_dim() -> usize {
    384
}

fn default_heat_lambda() -> f64 {
    0.05
}

fn default_initial_heat() -> f64 {
    1.0
}

fn default_hot_threshold() -> f64 {
    0.3
}

fn default_max_hops() -> usize {
    2
}

fn default_rem_daily_budget() -> u64 {
    50_000
}

fn default_max_research_depth() -> usize {
    3
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            embedding_dim: default_embedding_dim(),
            heat_lambda: default_heat_lambda(),
            initial_heat: default_initial_heat(),
            hot_threshold: default_hot_threshold(),
            max_hops: default_max_hops(),
            synthesis_enabled: false,
            rem_daily_budget_tokens: default_rem_daily_budget(),
            max_research_depth: default_max_research_depth(),
            daily_cost_cap_usd: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let cfg = GraphConfig::default();
        assert_eq!(cfg.db_path, "knowledge.db");
        assert_eq!(cfg.embedding_dim, 384);
        assert!(cfg.heat_lambda > 0.0);
        assert!(cfg.initial_heat > 0.0);
        assert!(!cfg.synthesis_enabled);
    }
}
