use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Pending,
    Ready,
    InProgress,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNode {
    pub id: String,
    pub action: String,
    pub dependencies: Vec<String>,
    pub status: NodeStatus,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanningGraph {
    pub nodes: HashMap<String, PlanNode>,
}

impl PlanningGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, id: &str, action: &str, dependencies: Vec<String>) {
        self.nodes.insert(
            id.to_string(),
            PlanNode {
                id: id.to_string(),
                action: action.to_string(),
                dependencies,
                status: NodeStatus::Pending,
                source_id: String::new(),
                timestamp: 0,
                confidence: 1.0,
            },
        );
    }

    pub fn ready_nodes(&self) -> Vec<&PlanNode> {
        self.nodes
            .values()
            .filter(|n| {
                n.status == NodeStatus::Pending
                    && n.dependencies.iter().all(|dep| {
                        self.nodes
                            .get(dep)
                            .map_or(true, |d| d.status == NodeStatus::Complete)
                    })
            })
            .collect()
    }

    pub fn mark_complete(&mut self, id: &str) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.status = NodeStatus::Complete;
        }
    }

    pub fn mark_failed(&mut self, id: &str) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.status = NodeStatus::Failed;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.nodes
            .values()
            .all(|n| n.status == NodeStatus::Complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dag_dependency_resolution() {
        let mut pg = PlanningGraph::new();
        pg.add_node("build", "compile code", vec![]);
        pg.add_node("test", "run tests", vec!["build".into()]);
        pg.add_node("deploy", "deploy to prod", vec!["test".into()]);

        let ready = pg.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "build");

        pg.mark_complete("build");
        let ready = pg.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "test");

        pg.mark_complete("test");
        let ready = pg.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "deploy");

        pg.mark_complete("deploy");
        assert!(pg.is_complete());
    }

    #[test]
    fn parallel_nodes_both_ready() {
        let mut pg = PlanningGraph::new();
        pg.add_node("a", "step a", vec![]);
        pg.add_node("b", "step b", vec![]);
        pg.add_node("c", "step c", vec!["a".into(), "b".into()]);
        let ready = pg.ready_nodes();
        assert_eq!(ready.len(), 2);
    }
}
