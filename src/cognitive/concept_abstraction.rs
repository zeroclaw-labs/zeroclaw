use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concept {
    pub name: String,
    pub abstraction_level: u32,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConceptHierarchy {
    pub concepts: HashMap<String, Concept>,
}

impl ConceptHierarchy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: &str, level: u32, parent: Option<&str>) {
        if let Some(parent_name) = parent {
            if let Some(p) = self.concepts.get_mut(parent_name) {
                p.children.push(name.to_string());
            }
        }
        self.concepts.insert(
            name.to_string(),
            Concept {
                name: name.to_string(),
                abstraction_level: level,
                parent: parent.map(|s| s.to_string()),
                children: Vec::new(),
                source_id: String::new(),
                timestamp: 0,
                confidence: 1.0,
            },
        );
    }

    pub fn generalize(&self, name: &str) -> Option<&Concept> {
        let concept = self.concepts.get(name)?;
        concept.parent.as_ref().and_then(|p| self.concepts.get(p))
    }

    pub fn most_abstract(&self) -> Option<&Concept> {
        self.concepts.values().max_by_key(|c| c.abstraction_level)
    }

    pub fn children_of(&self, name: &str) -> Vec<&Concept> {
        self.concepts
            .get(name)
            .map(|c| {
                c.children
                    .iter()
                    .filter_map(|ch| self.concepts.get(ch))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy_traversal() {
        let mut h = ConceptHierarchy::new();
        h.add("entity", 3, None);
        h.add("animal", 2, Some("entity"));
        h.add("dog", 1, Some("animal"));
        let parent = h.generalize("dog").unwrap();
        assert_eq!(parent.name, "animal");
        let top = h.most_abstract().unwrap();
        assert_eq!(top.name, "entity");
        let kids = h.children_of("entity");
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "animal");
    }
}
