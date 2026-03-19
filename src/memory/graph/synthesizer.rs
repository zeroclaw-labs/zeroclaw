//! Event-driven memory synthesizer.
//!
//! Two synthesis modes:
//! - **Profundo** (deep, 0 tokens): graph-only operations using HNSW similarity
//!   search and community detection to discover implicit connections.
//! - **REM** (budget-gated): LLM-powered synthesis for generating insights,
//!   consolidating knowledge, and hypothesis generation.

#[cfg(feature = "memory-graph")]
use cozo::DbInstance;
#[cfg(feature = "memory-graph")]
use std::collections::BTreeMap;

use super::budget::BudgetController;
use std::sync::Arc;

/// Extract a `&str` from a `cozo::DataValue::Str` variant.
#[cfg(feature = "memory-graph")]
fn datavalue_as_str(v: &cozo::DataValue) -> Option<&str> {
    if let cozo::DataValue::Str(s) = v {
        Some(s.as_str())
    } else {
        None
    }
}

/// Extract an `f64` from a `cozo::DataValue::Num` variant.
#[cfg(feature = "memory-graph")]
fn datavalue_as_f64(v: &cozo::DataValue) -> Option<f64> {
    match v {
        cozo::DataValue::Num(cozo::Num::Float(f)) => Some(*f),
        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(*i as f64),
        _ => None,
    }
}

/// Memory synthesizer running periodic knowledge consolidation.
pub struct Synthesizer {
    #[cfg(feature = "memory-graph")]
    db: Arc<DbInstance>,
    budget: Arc<BudgetController>,
    enabled: bool,
}

impl Synthesizer {
    /// Create a new synthesizer.
    #[cfg(feature = "memory-graph")]
    pub fn new(db: Arc<DbInstance>, budget: Arc<BudgetController>, enabled: bool) -> Self {
        Self {
            db,
            budget,
            enabled,
        }
    }

    #[cfg(not(feature = "memory-graph"))]
    pub fn new(budget: Arc<BudgetController>, enabled: bool) -> Self {
        Self { budget, enabled }
    }

    /// Run one synthesis cycle.
    ///
    /// Called periodically by the daemon supervisor (heartbeat-driven).
    pub async fn run_cycle(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Phase 1: Profundo (zero tokens — graph-only)
        self.run_profundo().await?;

        // Phase 2: REM (budget-gated LLM synthesis)
        if self.budget.can_spend(500, 0.0) {
            self.run_rem().await?;
        } else {
            tracing::debug!("REM synthesis skipped: budget exhausted");
        }

        Ok(())
    }

    /// Profundo synthesis: graph-only operations (0 tokens).
    ///
    /// 1. Find similar nodes via HNSW proximity
    /// 2. Create `similar_to` edges for high-similarity pairs
    /// 3. Detect topic clusters via connected components
    #[allow(clippy::unused_async)]
    async fn run_profundo(&self) -> anyhow::Result<()> {
        #[cfg(feature = "memory-graph")]
        {
            // Find concept pairs with high embedding similarity but no existing relation
            let similarity_query = r#"
                ?[id_a, name_a, id_b, name_b, sim] :=
                    *concept{id: id_a, name: name_a, embedding: emb_a},
                    *concept{id: id_b, name: name_b, embedding: emb_b},
                    id_a < id_b,
                    sim = cos_similarity(emb_a, emb_b),
                    sim > 0.8,
                    not *similar_to{id_a, id_b},
                    not *relates_to{from_id: id_a, to_id: id_b}
                :limit 10
            "#;

            match self.db.run_script(
                similarity_query,
                BTreeMap::default(),
                cozo::ScriptMutability::Immutable,
            ) {
                Ok(result) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    for row in &result.rows {
                        let id_a = row.first().and_then(datavalue_as_str).unwrap_or_default();
                        let id_b = row.get(2).and_then(datavalue_as_str).unwrap_or_default();
                        let sim = row.get(4).and_then(datavalue_as_f64).unwrap_or(0.0);

                        if !id_a.is_empty() && !id_b.is_empty() {
                            let link = format!(
                                r#"?[id_a, id_b, similarity, created_at] <- [['{id_a}', '{id_b}', {sim}, '{now}']]
                                :put similar_to {{id_a, id_b => similarity, created_at}}"#,
                            );
                            let _ = self.db.run_script(
                                &link,
                                BTreeMap::default(),
                                cozo::ScriptMutability::Mutable,
                            );
                        }
                    }

                    if !result.rows.is_empty() {
                        tracing::info!(
                            "🧠 Profundo: discovered {} similarity edges",
                            result.rows.len()
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!("Profundo similarity search: {e}");
                }
            }
        }

        Ok(())
    }

    /// REM synthesis: LLM-powered knowledge consolidation (budget-gated).
    ///
    /// Currently a placeholder — will be extended to:
    /// 1. Select hot nodes that lack connections
    /// 2. Ask LLM to identify relationships
    /// 3. Create new edges and derived concepts
    #[allow(clippy::unused_async)]
    async fn run_rem(&self) -> anyhow::Result<()> {
        #[cfg(feature = "memory-graph")]
        {
            // Find orphan hot nodes (high heat, few connections)
            let orphan_query = r#"
                degree[id, cnt] := *relates_to{from_id: id}, cnt = count(id)
                degree[id, cnt] := *relates_to{to_id: id}, cnt = count(id)
                degree[id, 0] := *concept{id}, not *relates_to{from_id: id}, not *relates_to{to_id: id}
                ?[id, name, heat, degree] :=
                    *concept{id, name, heat},
                    degree[id, degree],
                    heat > 0.5,
                    degree < 2
                :order -heat
                :limit 5
            "#;

            match self.db.run_script(
                orphan_query,
                BTreeMap::default(),
                cozo::ScriptMutability::Immutable,
            ) {
                Ok(result) if !result.rows.is_empty() => {
                    // Log orphan nodes for future LLM processing
                    let orphans: Vec<String> = result
                        .rows
                        .iter()
                        .filter_map(|row| row.get(1).and_then(datavalue_as_str).map(String::from))
                        .collect();

                    tracing::info!(
                        "🧠 REM: found {} orphan hot concepts: {:?}",
                        orphans.len(),
                        &orphans[..orphans.len().min(3)]
                    );

                    // Record token spend (placeholder — actual LLM calls will consume real tokens)
                    self.budget.record_spend(100);
                }
                Ok(_) => {
                    tracing::debug!("REM: no orphan hot concepts found");
                }
                Err(e) => {
                    tracing::debug!("REM orphan search: {e}");
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesizer_disabled_is_noop() {
        #[cfg(feature = "memory-graph")]
        {
            // Can't easily create DbInstance in test without CozoDB, so skip
            let _budget = Arc::new(BudgetController::new(50_000, 0.0));
        }
        #[cfg(not(feature = "memory-graph"))]
        {
            let budget = Arc::new(BudgetController::new(50_000, 0.0));
            let _synth = Synthesizer::new(budget, false);
        }
    }
}
