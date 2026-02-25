use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosmicNode {
    pub id: String,
    pub content: String,
    pub category: String,
    pub embedding: Vec<f32>,
    pub created_at: DateTime<Utc>,
    pub access_count: u64,
    pub activation: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosmicEdge {
    pub from: String,
    pub to: String,
    pub strength: f32,
    pub edge_type: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosmicMemoryGraph {
    nodes: HashMap<String, CosmicNode>,
    edges: Vec<CosmicEdge>,
    max_nodes: usize,
}

impl CosmicMemoryGraph {
    pub fn new(max_nodes: usize) -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            max_nodes,
        }
    }

    pub fn insert_node(
        &mut self,
        id: String,
        content: String,
        category: String,
        embedding: Vec<f32>,
    ) -> bool {
        if self.nodes.len() >= self.max_nodes && !self.nodes.contains_key(&id) {
            return false;
        }
        let node = CosmicNode {
            id: id.clone(),
            content,
            category,
            embedding,
            created_at: Utc::now(),
            access_count: 0,
            activation: 0.0,
        };
        self.nodes.insert(id, node);
        true
    }

    pub fn insert_edge(&mut self, from: String, to: String, strength: f32, edge_type: String) {
        let edge = CosmicEdge {
            from,
            to,
            strength,
            edge_type,
            created_at: Utc::now(),
        };
        self.edges.push(edge);
    }

    pub fn get_node(&self, id: &str) -> Option<&CosmicNode> {
        self.nodes.get(id)
    }

    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut CosmicNode> {
        self.nodes.get_mut(id)
    }

    pub fn neighbors(&self, id: &str) -> Vec<(&CosmicEdge, &CosmicNode)> {
        let mut result = Vec::new();
        for edge in &self.edges {
            if edge.from == id {
                if let Some(node) = self.nodes.get(&edge.to) {
                    result.push((edge, node));
                }
            } else if edge.to == id {
                if let Some(node) = self.nodes.get(&edge.from) {
                    result.push((edge, node));
                }
            }
        }
        result
    }

    pub fn strongest_path(&self, from: &str, to: &str) -> Option<Vec<String>> {
        if !self.nodes.contains_key(from) || !self.nodes.contains_key(to) {
            return None;
        }
        if from == to {
            return Some(vec![from.to_string()]);
        }

        let mut best_min_strength: HashMap<String, f32> = HashMap::new();
        best_min_strength.insert(from.to_string(), f32::INFINITY);

        let mut parent: HashMap<String, String> = HashMap::new();

        let mut queue: VecDeque<(String, f32)> = VecDeque::new();
        queue.push_back((from.to_string(), f32::INFINITY));

        while let Some((current, path_min)) = queue.pop_front() {
            if path_min
                < best_min_strength
                    .get(&current)
                    .copied()
                    .unwrap_or(f32::NEG_INFINITY)
            {
                continue;
            }

            for edge in &self.edges {
                let (neighbor_id, strength) = if edge.from == current {
                    (&edge.to, edge.strength)
                } else if edge.to == current {
                    (&edge.from, edge.strength)
                } else {
                    continue;
                };

                let new_min = path_min.min(strength);
                let prev_best = best_min_strength
                    .get(neighbor_id.as_str())
                    .copied()
                    .unwrap_or(f32::NEG_INFINITY);

                if new_min > prev_best {
                    best_min_strength.insert(neighbor_id.clone(), new_min);
                    parent.insert(neighbor_id.clone(), current.clone());
                    queue.push_back((neighbor_id.clone(), new_min));
                }
            }
        }

        if !parent.contains_key(to) {
            return None;
        }

        let mut path = vec![to.to_string()];
        let mut current = to.to_string();
        while current != from {
            let prev = parent.get(&current)?.clone();
            path.push(prev.clone());
            current = prev;
        }
        path.reverse();
        Some(path)
    }

    pub fn prune_weakest(&mut self, keep: usize) {
        if self.nodes.len() <= keep {
            return;
        }

        let mut entries: Vec<(String, u64)> = self
            .nodes
            .iter()
            .map(|(id, node)| (id.clone(), node.access_count))
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));

        let remove_count = self.nodes.len() - keep;
        let to_remove: HashSet<String> = entries
            .into_iter()
            .take(remove_count)
            .map(|(id, _)| id)
            .collect();

        for id in &to_remove {
            self.nodes.remove(id);
        }
        self.edges
            .retain(|e| !to_remove.contains(&e.from) && !to_remove.contains(&e.to));
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn hub_nodes(&self, top_n: usize) -> Vec<&CosmicNode> {
        let mut degree: HashMap<&str, usize> = HashMap::new();
        for edge in &self.edges {
            *degree.entry(edge.from.as_str()).or_default() += 1;
            *degree.entry(edge.to.as_str()).or_default() += 1;
        }

        let mut ranked: Vec<(&str, usize)> = degree.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));

        ranked
            .into_iter()
            .take(top_n)
            .filter_map(|(id, _)| self.nodes.get(id))
            .collect()
    }
}

pub fn spreading_activation(
    graph: &mut CosmicMemoryGraph,
    seed_ids: &[String],
    initial_energy: f32,
    decay: f32,
    max_hops: u32,
) {
    for node in graph.nodes.values_mut() {
        node.activation = 0.0;
    }

    let mut queue: VecDeque<(String, f32, u32)> = VecDeque::new();
    for seed in seed_ids {
        if let Some(node) = graph.nodes.get_mut(seed) {
            node.activation = initial_energy;
            node.access_count += 1;
            queue.push_back((seed.clone(), initial_energy, 0));
        }
    }

    let mut visited: HashSet<String> = seed_ids.iter().cloned().collect();

    while let Some((current_id, energy, hop)) = queue.pop_front() {
        if hop >= max_hops {
            continue;
        }

        let neighbor_ids: Vec<(String, f32)> = graph
            .edges
            .iter()
            .filter_map(|edge| {
                if edge.from == current_id {
                    Some((edge.to.clone(), edge.strength))
                } else if edge.to == current_id {
                    Some((edge.from.clone(), edge.strength))
                } else {
                    None
                }
            })
            .collect();

        for (neighbor_id, strength) in neighbor_ids {
            let spread = energy * decay * strength;
            if spread < 0.001 {
                continue;
            }

            if let Some(node) = graph.nodes.get_mut(&neighbor_id) {
                node.activation += spread;
                node.access_count += 1;

                if visited.insert(neighbor_id.clone()) {
                    queue.push_back((neighbor_id, spread, hop + 1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_graph() -> CosmicMemoryGraph {
        let mut g = CosmicMemoryGraph::new(10);
        g.insert_node("a".into(), "node_a".into(), "cat1".into(), vec![1.0, 0.0]);
        g.insert_node("b".into(), "node_b".into(), "cat1".into(), vec![0.0, 1.0]);
        g.insert_node("c".into(), "node_c".into(), "cat2".into(), vec![1.0, 1.0]);
        g.insert_edge("a".into(), "b".into(), 0.9, "related".into());
        g.insert_edge("b".into(), "c".into(), 0.5, "related".into());
        g
    }

    #[test]
    fn insert_and_retrieve_node() {
        let mut g = CosmicMemoryGraph::new(5);
        assert!(g.insert_node("n1".into(), "content".into(), "cat".into(), vec![0.5]));
        let node = g.get_node("n1").unwrap();
        assert_eq!(node.content, "content");
        assert_eq!(node.category, "cat");
        assert_eq!(node.embedding, vec![0.5]);
    }

    #[test]
    fn capacity_limit_respected() {
        let mut g = CosmicMemoryGraph::new(2);
        assert!(g.insert_node("a".into(), "a".into(), "c".into(), vec![]));
        assert!(g.insert_node("b".into(), "b".into(), "c".into(), vec![]));
        assert!(!g.insert_node("c".into(), "c".into(), "c".into(), vec![]));
        assert_eq!(g.node_count(), 2);
    }

    #[test]
    fn overwrite_existing_node_within_capacity() {
        let mut g = CosmicMemoryGraph::new(2);
        assert!(g.insert_node("a".into(), "v1".into(), "c".into(), vec![]));
        assert!(g.insert_node("b".into(), "v2".into(), "c".into(), vec![]));
        assert!(g.insert_node("a".into(), "v3".into(), "c".into(), vec![]));
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.get_node("a").unwrap().content, "v3");
    }

    #[test]
    fn insert_edges_and_check_neighbors() {
        let g = make_graph();
        let neighbors = g.neighbors("b");
        assert_eq!(neighbors.len(), 2);

        let neighbor_ids: HashSet<&str> = neighbors.iter().map(|(_, n)| n.id.as_str()).collect();
        assert!(neighbor_ids.contains("a"));
        assert!(neighbor_ids.contains("c"));
    }

    #[test]
    fn neighbors_of_leaf_node() {
        let g = make_graph();
        let neighbors = g.neighbors("a");
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].1.id, "b");
    }

    #[test]
    fn spreading_activation_decays() {
        let mut g = make_graph();
        spreading_activation(&mut g, &["a".into()], 1.0, 0.5, 3);

        let a_act = g.get_node("a").unwrap().activation;
        let b_act = g.get_node("b").unwrap().activation;
        let c_act = g.get_node("c").unwrap().activation;

        assert!(
            a_act >= 1.0,
            "seed node should have at least initial energy"
        );
        assert!(
            b_act > 0.0 && b_act < a_act,
            "neighbor should activate less than seed"
        );
        assert!(
            c_act > 0.0 && c_act < b_act,
            "2-hop should activate less than 1-hop"
        );
    }

    #[test]
    fn spreading_activation_respects_max_hops() {
        let mut g = make_graph();
        spreading_activation(&mut g, &["a".into()], 1.0, 0.8, 1);

        let b_act = g.get_node("b").unwrap().activation;
        let c_act = g.get_node("c").unwrap().activation;

        assert!(b_act > 0.0);
        assert!((c_act - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn hub_nodes_finds_most_connected() {
        let mut g = CosmicMemoryGraph::new(10);
        for id in ["hub", "a", "b", "c", "d"] {
            g.insert_node(id.into(), id.into(), "cat".into(), vec![]);
        }
        g.insert_edge("hub".into(), "a".into(), 1.0, "link".into());
        g.insert_edge("hub".into(), "b".into(), 1.0, "link".into());
        g.insert_edge("hub".into(), "c".into(), 1.0, "link".into());
        g.insert_edge("hub".into(), "d".into(), 1.0, "link".into());
        g.insert_edge("a".into(), "b".into(), 0.5, "link".into());

        let hubs = g.hub_nodes(1);
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].id, "hub");
    }

    #[test]
    fn prune_removes_lowest_access_nodes() {
        let mut g = CosmicMemoryGraph::new(5);
        g.insert_node("a".into(), "a".into(), "c".into(), vec![]);
        g.insert_node("b".into(), "b".into(), "c".into(), vec![]);
        g.insert_node("c".into(), "c".into(), "c".into(), vec![]);
        g.insert_edge("a".into(), "b".into(), 1.0, "link".into());
        g.insert_edge("b".into(), "c".into(), 1.0, "link".into());

        g.get_node_mut("b").unwrap().access_count = 10;
        g.get_node_mut("c").unwrap().access_count = 5;

        g.prune_weakest(1);

        assert_eq!(g.node_count(), 1);
        assert!(g.get_node("b").is_some());
        assert!(g.get_node("a").is_none());
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn strongest_path_finds_route() {
        let g = make_graph();
        let path = g.strongest_path("a", "c").unwrap();
        assert_eq!(path, vec!["a", "b", "c"]);
    }

    #[test]
    fn strongest_path_returns_none_for_disconnected() {
        let mut g = CosmicMemoryGraph::new(5);
        g.insert_node("x".into(), "x".into(), "c".into(), vec![]);
        g.insert_node("y".into(), "y".into(), "c".into(), vec![]);
        assert!(g.strongest_path("x", "y").is_none());
    }

    #[test]
    fn strongest_path_same_node() {
        let g = make_graph();
        let path = g.strongest_path("a", "a").unwrap();
        assert_eq!(path, vec!["a"]);
    }

    #[test]
    fn strongest_path_prefers_stronger_edges() {
        let mut g = CosmicMemoryGraph::new(10);
        for id in ["s", "m1", "m2", "t"] {
            g.insert_node(id.into(), id.into(), "c".into(), vec![]);
        }
        g.insert_edge("s".into(), "m1".into(), 0.1, "weak".into());
        g.insert_edge("m1".into(), "t".into(), 0.1, "weak".into());
        g.insert_edge("s".into(), "m2".into(), 0.9, "strong".into());
        g.insert_edge("m2".into(), "t".into(), 0.8, "strong".into());

        let path = g.strongest_path("s", "t").unwrap();
        assert_eq!(path, vec!["s", "m2", "t"]);
    }

    #[test]
    fn node_and_edge_counts() {
        let g = make_graph();
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 2);
    }
}
