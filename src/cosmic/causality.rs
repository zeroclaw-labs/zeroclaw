use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEvent {
    pub source: String,
    pub target: String,
    pub source_delta: f64,
    pub target_delta: f64,
    pub latency_ms: u64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEdge {
    pub from: String,
    pub to: String,
    pub strength: f64,
    pub transfer_entropy: f64,
    pub event_count: u64,
    pub last_event: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalLoop {
    pub nodes: Vec<String>,
    pub min_strength: f64,
    pub integrated_phi: f64,
}

const EMA_ALPHA: f64 = 0.15;

#[derive(Debug, Clone)]
pub struct CausalGraph {
    edges: HashMap<(String, String), CausalEdge>,
    events: VecDeque<CausalEvent>,
    capacity: usize,
}

impl CausalGraph {
    pub fn new(capacity: usize) -> Self {
        Self {
            edges: HashMap::new(),
            events: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn record_event(
        &mut self,
        source: &str,
        target: &str,
        source_delta: f64,
        target_delta: f64,
        latency_ms: u64,
    ) {
        let event = CausalEvent {
            source: source.to_string(),
            target: target.to_string(),
            source_delta,
            target_delta,
            latency_ms,
            timestamp: Utc::now(),
        };

        self.events.push_back(event);
        if self.events.len() > self.capacity {
            self.events.pop_front();
        }

        let coupling = if source_delta.abs() > f64::EPSILON {
            (target_delta.abs() / source_delta.abs()).min(1.0)
        } else {
            0.0
        };

        let te = self.compute_transfer_entropy(source, target);

        let key = (source.to_string(), target.to_string());
        let edge = self.edges.entry(key).or_insert_with(|| CausalEdge {
            from: source.to_string(),
            to: target.to_string(),
            strength: 0.0,
            transfer_entropy: 0.0,
            event_count: 0,
            last_event: Utc::now(),
        });

        edge.strength = EMA_ALPHA * coupling + (1.0 - EMA_ALPHA) * edge.strength;
        edge.transfer_entropy = te;
        edge.event_count += 1;
        edge.last_event = Utc::now();
    }

    fn compute_transfer_entropy(&self, source: &str, target: &str) -> f64 {
        let relevant: Vec<&CausalEvent> = self
            .events
            .iter()
            .filter(|e| e.source == source && e.target == target)
            .collect();

        if relevant.len() < 2 {
            return 0.0;
        }

        let responsive = relevant
            .iter()
            .filter(|e| e.target_delta.abs() > f64::EPSILON)
            .count();

        let ratio = responsive as f64 / relevant.len() as f64;
        if ratio >= 0.99 {
            return 3.0;
        }
        if ratio < f64::EPSILON {
            return 0.0;
        }
        let raw = -(1.0 - ratio).log2();
        raw.clamp(0.0, 3.0)
    }

    pub fn causal_strength(&self, from: &str, to: &str) -> f64 {
        self.edges
            .get(&(from.to_string(), to.to_string()))
            .map_or(0.0, |e| e.strength)
    }

    pub fn transfer_entropy(&self, from: &str, to: &str) -> f64 {
        self.edges
            .get(&(from.to_string(), to.to_string()))
            .map_or(0.0, |e| e.transfer_entropy)
    }

    pub fn find_loops(&self, max_length: usize) -> Vec<CausalLoop> {
        let nodes: HashSet<&str> = self
            .edges
            .keys()
            .flat_map(|(f, t)| [f.as_str(), t.as_str()])
            .collect();

        let mut loops = Vec::new();

        for &start in &nodes {
            self.dfs_loops(
                start,
                start,
                &mut vec![start.to_string()],
                max_length,
                &mut loops,
            );
        }

        deduplicate_loops(&mut loops);
        loops
    }

    fn dfs_loops(
        &self,
        start: &str,
        current: &str,
        path: &mut Vec<String>,
        max_length: usize,
        result: &mut Vec<CausalLoop>,
    ) {
        if path.len() > max_length {
            return;
        }

        for ((from, to), edge) in &self.edges {
            if from != current || edge.strength < 0.01 {
                continue;
            }

            if to == start && path.len() >= 3 {
                let min_strength = self.loop_min_strength(path, start);
                let phi = self.loop_phi(path, start);
                result.push(CausalLoop {
                    nodes: path.clone(),
                    min_strength,
                    integrated_phi: phi,
                });
            } else if !path.contains(to) && path.len() < max_length {
                path.push(to.clone());
                self.dfs_loops(start, to, path, max_length, result);
                path.pop();
            }
        }
    }

    fn loop_min_strength(&self, path: &[String], back_to: &str) -> f64 {
        let mut min = f64::INFINITY;
        for i in 0..path.len() {
            let next = if i + 1 < path.len() {
                &path[i + 1]
            } else {
                back_to
            };
            let s = self.causal_strength(&path[i], next);
            if s < min {
                min = s;
            }
        }
        if min.is_infinite() {
            0.0
        } else {
            min
        }
    }

    fn loop_phi(&self, path: &[String], back_to: &str) -> f64 {
        let mut product = 1.0f64;
        let mut count = 0u32;

        for i in 0..path.len() {
            let next = if i + 1 < path.len() {
                &path[i + 1]
            } else {
                back_to
            };
            let key = (path[i].clone(), next.to_string());
            if let Some(edge) = self.edges.get(&key) {
                let weighted = edge.strength * (1.0 + edge.transfer_entropy);
                product *= weighted;
                count += 1;
            }
        }

        if count == 0 {
            return 0.0;
        }

        product.powf(1.0 / f64::from(count))
    }

    pub fn integrated_phi(&self, max_loop_length: usize) -> f64 {
        let loops = self.find_loops(max_loop_length);
        if loops.is_empty() {
            return 0.0;
        }

        let sum: f64 = loops.iter().map(|l| l.integrated_phi).sum();
        sum / loops.len() as f64
    }

    pub fn weakest_causal_link(&self) -> Option<(String, String, f64)> {
        self.edges
            .values()
            .filter(|e| e.event_count > 0)
            .min_by(|a, b| {
                a.strength
                    .partial_cmp(&b.strength)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|e| (e.from.clone(), e.to.clone(), e.strength))
    }

    pub fn intervention_candidates(&self, top_n: usize) -> Vec<(String, f64)> {
        let nodes: HashSet<String> = self
            .edges
            .keys()
            .flat_map(|(f, t)| [f.clone(), t.clone()])
            .collect();

        let mut scores: Vec<(String, f64)> = nodes
            .into_iter()
            .map(|node| {
                let outgoing: f64 = self
                    .edges
                    .values()
                    .filter(|e| e.from == node)
                    .map(|e| e.strength)
                    .sum();
                let incoming: f64 = self
                    .edges
                    .values()
                    .filter(|e| e.to == node)
                    .map(|e| e.strength)
                    .sum();
                let score = if incoming > f64::EPSILON {
                    outgoing / incoming
                } else if outgoing > f64::EPSILON {
                    outgoing * 10.0
                } else {
                    0.0
                };
                (node, score)
            })
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_n);
        scores
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let edges: Vec<&CausalEdge> = self.edges.values().collect();
        serde_json::json!({
            "edges": edges,
            "events": self.events.iter().collect::<Vec<_>>(),
            "capacity": self.capacity,
        })
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }
}

fn normalize_loop(nodes: &[String]) -> Vec<String> {
    if nodes.is_empty() {
        return vec![];
    }
    let min_pos = nodes
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.cmp(b.1))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut rotated: Vec<String> = nodes[min_pos..].to_vec();
    rotated.extend_from_slice(&nodes[..min_pos]);
    rotated
}

fn deduplicate_loops(loops: &mut Vec<CausalLoop>) {
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    loops.retain(|l| {
        let normalized = normalize_loop(&l.nodes);
        seen.insert(normalized)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle_graph() -> CausalGraph {
        let mut g = CausalGraph::new(100);
        for _ in 0..5 {
            g.record_event("a", "b", 1.0, 0.8, 10);
            g.record_event("b", "c", 0.8, 0.6, 15);
            g.record_event("c", "a", 0.6, 0.5, 20);
        }
        g
    }

    #[test]
    fn record_event_creates_directed_edge() {
        let mut g = CausalGraph::new(100);
        g.record_event("x", "y", 1.0, 0.5, 10);
        assert!(g.causal_strength("x", "y") > 0.0);
        assert!((g.causal_strength("y", "x") - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn causal_strength_converges_with_ema() {
        let mut g = CausalGraph::new(100);
        for _ in 0..20 {
            g.record_event("a", "b", 1.0, 0.9, 5);
        }
        let s = g.causal_strength("a", "b");
        assert!(s > 0.7, "strength should converge near 0.9, got {s}");
        assert!(s <= 1.0);
    }

    #[test]
    fn transfer_entropy_responsive_vs_unresponsive() {
        let mut g = CausalGraph::new(100);
        for _ in 0..10 {
            g.record_event("a", "b", 1.0, 0.8, 5);
        }
        let te_responsive = g.transfer_entropy("a", "b");

        let mut g2 = CausalGraph::new(100);
        for _ in 0..10 {
            g2.record_event("a", "b", 1.0, 0.0, 5);
        }
        let te_silent = g2.transfer_entropy("a", "b");

        assert!(
            te_responsive > te_silent,
            "responsive TE={te_responsive} should exceed silent TE={te_silent}"
        );
    }

    #[test]
    fn find_loops_detects_triangle() {
        let g = triangle_graph();
        let loops = g.find_loops(4);
        assert!(
            !loops.is_empty(),
            "should detect at least one loop in triangle"
        );
        let has_three_node_loop = loops.iter().any(|l| l.nodes.len() == 3);
        assert!(has_three_node_loop, "should find a 3-node loop");
    }

    #[test]
    fn find_loops_empty_for_chain() {
        let mut g = CausalGraph::new(100);
        for _ in 0..5 {
            g.record_event("a", "b", 1.0, 0.8, 10);
            g.record_event("b", "c", 0.8, 0.6, 10);
        }
        let loops = g.find_loops(4);
        assert!(loops.is_empty(), "chain should have no loops");
    }

    #[test]
    fn integrated_phi_positive_for_loops() {
        let g = triangle_graph();
        let phi = g.integrated_phi(4);
        assert!(phi > 0.0, "triangle should have positive phi, got {phi}");
    }

    #[test]
    fn integrated_phi_zero_for_chain() {
        let mut g = CausalGraph::new(100);
        for _ in 0..5 {
            g.record_event("a", "b", 1.0, 0.8, 10);
            g.record_event("b", "c", 0.8, 0.6, 10);
        }
        assert!(
            (g.integrated_phi(4) - 0.0).abs() < f64::EPSILON,
            "chain should have zero phi"
        );
    }

    #[test]
    fn weakest_causal_link_finds_minimum() {
        let mut g = CausalGraph::new(100);
        for _ in 0..10 {
            g.record_event("a", "b", 1.0, 0.9, 5);
            g.record_event("b", "c", 1.0, 0.1, 5);
        }
        let (from, to, strength) = g.weakest_causal_link().unwrap();
        assert_eq!(from, "b");
        assert_eq!(to, "c");
        assert!(strength < g.causal_strength("a", "b"));
    }

    #[test]
    fn intervention_candidates_ranks_sources_first() {
        let mut g = CausalGraph::new(100);
        for _ in 0..10 {
            g.record_event("driver", "follower1", 1.0, 0.9, 5);
            g.record_event("driver", "follower2", 1.0, 0.8, 5);
        }
        let candidates = g.intervention_candidates(3);
        assert_eq!(
            candidates[0].0, "driver",
            "driver should be top intervention candidate"
        );
    }

    #[test]
    fn ring_buffer_prunes_old_events() {
        let mut g = CausalGraph::new(3);
        for i in 0..5 {
            g.record_event("a", "b", 1.0, f64::from(i) * 0.1, 5);
        }
        assert!(g.event_count() <= 3);
    }

    #[test]
    fn empty_graph_returns_defaults() {
        let g = CausalGraph::new(100);
        assert!((g.causal_strength("x", "y") - 0.0).abs() < f64::EPSILON);
        assert!((g.integrated_phi(4) - 0.0).abs() < f64::EPSILON);
        assert!(g.weakest_causal_link().is_none());
        assert!(g.find_loops(4).is_empty());
        assert!(g.intervention_candidates(5).is_empty());
    }

    #[test]
    fn loop_phi_uses_geometric_mean() {
        let g = triangle_graph();
        let loops = g.find_loops(4);
        for l in &loops {
            assert!(
                l.integrated_phi > 0.0,
                "loop phi should be positive: {:?}",
                l.nodes
            );
            assert!(l.min_strength > 0.0, "loop min_strength should be positive");
        }
    }

    #[test]
    fn deduplication_removes_rotated_duplicates() {
        let mut g = CausalGraph::new(100);
        for _ in 0..5 {
            g.record_event("a", "b", 1.0, 0.8, 10);
            g.record_event("b", "c", 0.8, 0.6, 10);
            g.record_event("c", "a", 0.6, 0.5, 10);
        }
        let loops = g.find_loops(4);
        let three_node: Vec<&CausalLoop> = loops.iter().filter(|l| l.nodes.len() == 3).collect();
        assert_eq!(
            three_node.len(),
            1,
            "should have exactly one 3-node loop after dedup, got {}",
            three_node.len()
        );
    }
}
