//! Research cascade for autonomous knowledge exploration.
//!
//! The researcher performs depth-limited exploration from seed concepts,
//! traversing the graph to discover related knowledge and identify gaps.
//! Budget-gated to prevent runaway LLM costs.

#[cfg(feature = "memory-graph")]
use cozo::DbInstance;
#[cfg(feature = "memory-graph")]
use std::collections::{BTreeMap, HashSet};

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

/// Maximum default research depth to prevent infinite recursion.
const DEFAULT_MAX_DEPTH: usize = 3;

/// Research cascade result.
#[derive(Debug, Clone)]
pub struct ResearchResult {
    /// Nodes discovered during research.
    pub discovered_nodes: Vec<String>,
    /// New connections identified.
    pub new_connections: usize,
    /// Depth reached in the cascade.
    pub depth_reached: usize,
    /// Tokens consumed by the research.
    pub tokens_consumed: u64,
}

/// Autonomous research cascade.
pub struct Researcher {
    #[cfg(feature = "memory-graph")]
    db: Arc<DbInstance>,
    budget: Arc<BudgetController>,
    max_depth: usize,
}

impl Researcher {
    #[cfg(feature = "memory-graph")]
    pub fn new(db: Arc<DbInstance>, budget: Arc<BudgetController>, max_depth: usize) -> Self {
        Self {
            db,
            budget,
            max_depth: if max_depth == 0 {
                DEFAULT_MAX_DEPTH
            } else {
                max_depth
            },
        }
    }

    #[cfg(not(feature = "memory-graph"))]
    pub fn new(budget: Arc<BudgetController>, max_depth: usize) -> Self {
        Self {
            budget,
            max_depth: if max_depth == 0 {
                DEFAULT_MAX_DEPTH
            } else {
                max_depth
            },
        }
    }

    /// Run a research cascade from a seed concept.
    ///
    /// Explores the graph outward from the seed, identifying:
    /// - Related concepts within N hops
    /// - Hypotheses that need validation
    /// - Knowledge gaps (hot nodes with few connections)
    #[allow(clippy::unused_async)]
    pub async fn research_from_seed(&self, seed_id: &str) -> anyhow::Result<ResearchResult> {
        #[cfg(not(feature = "memory-graph"))]
        let _ = seed_id;
        #[allow(unused_mut)]
        let mut result = ResearchResult {
            discovered_nodes: Vec::new(),
            new_connections: 0,
            depth_reached: 0,
            tokens_consumed: 0,
        };

        #[cfg(feature = "memory-graph")]
        {
            let mut visited = HashSet::new();
            visited.insert(seed_id.to_string());

            let mut frontier = vec![seed_id.to_string()];

            for depth in 0..self.max_depth {
                if frontier.is_empty() {
                    break;
                }

                // Check budget before each depth level
                let estimated_tokens = 200 * frontier.len() as u64;
                if !self.budget.can_spend(estimated_tokens, 0.0) {
                    tracing::info!(
                        "Research cascade budget-gated at depth {depth} (frontier: {})",
                        frontier.len()
                    );
                    break;
                }

                let mut next_frontier = Vec::new();

                for node_id in &frontier {
                    let escaped: String = node_id.replace('\'', "\\'");

                    // Find neighbors
                    let neighbor_query = format!(
                        r#"
                        neighbors[id, name] :=
                            *relates_to{{from_id: '{escaped}', to_id: id}},
                            *concept{{id, name}}
                        neighbors[id, name] :=
                            *relates_to{{to_id: '{escaped}', from_id: id}},
                            *concept{{id, name}}
                        ?[id, name] := neighbors[id, name]
                        :limit 10
                        "#
                    );

                    if let Ok(neighbors) = self.db.run_script(
                        &neighbor_query,
                        BTreeMap::default(),
                        cozo::ScriptMutability::Immutable,
                    ) {
                        for row in &neighbors.rows {
                            if let Some(nid) = row.first().and_then(datavalue_as_str) {
                                if visited.insert(nid.to_string()) {
                                    result.discovered_nodes.push(nid.to_string());
                                    next_frontier.push(nid.to_string());
                                }
                            }
                        }
                    }

                    // Find open hypotheses related to this node
                    let hyp_query = format!(
                        r#"
                        ?[id, claim] :=
                            *hypothesis{{id, claim, status}},
                            status == 'open',
                            *supports{{fact_id: '{escaped}', hypothesis_id: id}}
                        :limit 5
                        "#
                    );

                    if let Ok(hypotheses) = self.db.run_script(
                        &hyp_query,
                        BTreeMap::default(),
                        cozo::ScriptMutability::Immutable,
                    ) {
                        for row in &hypotheses.rows {
                            if let Some(hid) = row.first().and_then(datavalue_as_str) {
                                if visited.insert(hid.to_string()) {
                                    result.discovered_nodes.push(hid.to_string());
                                }
                            }
                        }
                    }
                }

                self.budget.record_spend(estimated_tokens);
                result.tokens_consumed += estimated_tokens;
                result.depth_reached = depth + 1;
                frontier = next_frontier;
            }
        }

        Ok(result)
    }

    /// Find knowledge gaps: hot concepts with few connections.
    #[allow(clippy::unused_async)]
    pub async fn find_knowledge_gaps(&self) -> anyhow::Result<Vec<String>> {
        #[cfg(feature = "memory-graph")]
        {
            let query = r#"
                degree[id, cnt] := *relates_to{from_id: id}, cnt = count(id)
                degree[id, 0] := *concept{id}, not *relates_to{from_id: id}
                ?[id, name, heat, deg] :=
                    *concept{id, name, heat},
                    degree[id, deg],
                    heat > 0.3,
                    deg < 2
                :order -heat
                :limit 10
            "#;

            match self.db.run_script(
                query,
                BTreeMap::default(),
                cozo::ScriptMutability::Immutable,
            ) {
                Ok(result) => {
                    return Ok(result
                        .rows
                        .iter()
                        .filter_map(|row| row.first().and_then(datavalue_as_str).map(String::from))
                        .collect());
                }
                Err(e) => {
                    tracing::debug!("Knowledge gap query failed: {e}");
                }
            }
        }

        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_result_defaults() {
        let result = ResearchResult {
            discovered_nodes: Vec::new(),
            new_connections: 0,
            depth_reached: 0,
            tokens_consumed: 0,
        };
        assert_eq!(result.depth_reached, 0);
    }

    #[test]
    fn max_depth_defaults_when_zero() {
        #[cfg(not(feature = "memory-graph"))]
        {
            let budget = Arc::new(BudgetController::new(50_000, 0.0));
            let researcher = Researcher::new(budget, 0);
            assert_eq!(researcher.max_depth, DEFAULT_MAX_DEPTH);
        }
    }
}
