use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub category: String,
    pub importance: f64,
    pub access_count: u32,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPattern {
    pub id: String,
    pub description: String,
    pub frequency: u32,
    pub source_ids: Vec<String>,
    pub confidence: f64,
    pub discovered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationResult {
    pub merged_count: usize,
    pub patterns_found: usize,
    pub pruned_count: usize,
    pub total_remaining: usize,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ConsolidationEngine {
    entries: Vec<MemoryEntry>,
    patterns: Vec<MemoryPattern>,
    similarity_threshold: f64,
}

impl ConsolidationEngine {
    pub fn new(similarity_threshold: f64) -> Self {
        Self {
            entries: Vec::new(),
            patterns: Vec::new(),
            similarity_threshold,
        }
    }

    pub fn add_entry(&mut self, entry: MemoryEntry) {
        self.entries.push(entry);
    }

    pub fn consolidate(&mut self) -> ConsolidationResult {
        let merged_count = self.merge_similar();
        let patterns = self.extract_patterns();
        let patterns_found = patterns.len();
        let pruned_count = self.prune_redundant(0.3);

        ConsolidationResult {
            merged_count,
            patterns_found,
            pruned_count,
            total_remaining: self.entries.len(),
            completed_at: Utc::now(),
        }
    }

    pub fn merge_similar(&mut self) -> usize {
        let mut merged_count = 0usize;
        let mut to_remove: HashSet<usize> = HashSet::new();

        for i in 0..self.entries.len() {
            if to_remove.contains(&i) {
                continue;
            }
            for j in (i + 1)..self.entries.len() {
                if to_remove.contains(&j) {
                    continue;
                }
                let sim = jaccard_similarity(&self.entries[i].content, &self.entries[j].content);
                if sim >= self.similarity_threshold {
                    let combined_access =
                        self.entries[i].access_count + self.entries[j].access_count;
                    let higher_importance =
                        self.entries[i].importance.max(self.entries[j].importance);
                    self.entries[i].access_count = combined_access;
                    self.entries[i].importance = higher_importance;
                    to_remove.insert(j);
                    merged_count += 1;
                }
            }
        }

        let mut idx = 0;
        self.entries.retain(|_| {
            let keep = !to_remove.contains(&idx);
            idx += 1;
            keep
        });

        merged_count
    }

    pub fn extract_patterns(&mut self) -> Vec<MemoryPattern> {
        let mut category_keyword_map: HashMap<(String, String), Vec<String>> = HashMap::new();

        for entry in &self.entries {
            let words: Vec<&str> = entry.content.split_whitespace().collect();
            for word in words {
                let key = (entry.category.clone(), word.to_lowercase());
                category_keyword_map
                    .entry(key)
                    .or_default()
                    .push(entry.id.clone());
            }
        }

        let mut new_patterns = Vec::new();
        for ((category, keyword), source_ids) in &category_keyword_map {
            if source_ids.len() >= 3 {
                let pattern = MemoryPattern {
                    id: format!("pat_{}_{}", category, keyword),
                    description: format!("{category}:{keyword}"),
                    #[allow(clippy::cast_possible_truncation)]
                    frequency: source_ids.len() as u32,
                    source_ids: source_ids.clone(),
                    confidence: (source_ids.len() as f64 / self.entries.len().max(1) as f64)
                        .min(1.0),
                    discovered_at: Utc::now(),
                };
                new_patterns.push(pattern);
            }
        }

        self.patterns.extend(new_patterns.clone());
        new_patterns
    }

    pub fn prune_redundant(&mut self, min_importance: f64) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|e| e.importance >= min_importance || e.access_count >= 2);
        before - self.entries.len()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    pub fn entries_by_category(&self, category: &str) -> Vec<&MemoryEntry> {
        self.entries
            .iter()
            .filter(|e| e.category == category)
            .collect()
    }

    pub fn top_patterns(&self, n: usize) -> Vec<&MemoryPattern> {
        let mut sorted: Vec<&MemoryPattern> = self.patterns.iter().collect();
        sorted.sort_by(|a, b| {
            b.frequency.cmp(&a.frequency).then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        sorted.truncate(n);
        sorted
    }
}

fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str, content: &str, category: &str, importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            category: category.to_string(),
            importance,
            access_count: 0,
            created_at: Utc::now(),
            last_accessed: Utc::now(),
        }
    }

    #[test]
    fn add_entries() {
        let mut engine = ConsolidationEngine::new(0.5);
        engine.add_entry(make_entry("e1", "hello world", "general", 0.5));
        engine.add_entry(make_entry("e2", "test data", "general", 0.5));
        assert_eq!(engine.entry_count(), 2);
    }

    #[test]
    fn merge_similar_combines_correctly() {
        let mut engine = ConsolidationEngine::new(0.6);
        engine.add_entry(make_entry("e1", "the quick brown fox", "general", 0.5));
        engine.add_entry(make_entry("e2", "the quick brown dog", "general", 0.8));
        let merged = engine.merge_similar();
        assert_eq!(merged, 1);
        assert_eq!(engine.entry_count(), 1);
        let remaining = &engine.entries_by_category("general")[0];
        assert!((remaining.importance - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn extract_patterns_finds_recurring_themes() {
        let mut engine = ConsolidationEngine::new(0.5);
        engine.add_entry(make_entry("e1", "system error detected", "logs", 0.5));
        engine.add_entry(make_entry("e2", "critical error found", "logs", 0.5));
        engine.add_entry(make_entry("e3", "another error reported", "logs", 0.5));
        let patterns = engine.extract_patterns();
        let error_pattern = patterns.iter().find(|p| p.description.contains("error"));
        assert!(error_pattern.is_some());
        assert!(error_pattern.unwrap().frequency >= 3);
    }

    #[test]
    fn prune_removes_low_importance() {
        let mut engine = ConsolidationEngine::new(0.5);
        engine.add_entry(make_entry("e1", "important data", "general", 0.8));
        engine.add_entry(make_entry("e2", "trivial noise", "general", 0.1));
        let pruned = engine.prune_redundant(0.3);
        assert_eq!(pruned, 1);
        assert_eq!(engine.entry_count(), 1);
    }

    #[test]
    fn consolidate_runs_full_pipeline() {
        let mut engine = ConsolidationEngine::new(0.6);
        engine.add_entry(make_entry("e1", "the quick brown fox", "general", 0.5));
        engine.add_entry(make_entry("e2", "the quick brown dog", "general", 0.8));
        engine.add_entry(make_entry("e3", "system error log", "logs", 0.1));
        let result = engine.consolidate();
        assert!(result.merged_count > 0 || result.pruned_count > 0 || result.total_remaining > 0);
        assert!(result.total_remaining <= 3);
    }

    #[test]
    fn entries_by_category_filters() {
        let mut engine = ConsolidationEngine::new(0.5);
        engine.add_entry(make_entry("e1", "data a", "alpha", 0.5));
        engine.add_entry(make_entry("e2", "data b", "beta", 0.5));
        engine.add_entry(make_entry("e3", "data c", "alpha", 0.5));
        let alpha = engine.entries_by_category("alpha");
        assert_eq!(alpha.len(), 2);
        let gamma = engine.entries_by_category("gamma");
        assert_eq!(gamma.len(), 0);
    }

    #[test]
    fn top_patterns_sorts_by_frequency() {
        let mut engine = ConsolidationEngine::new(0.5);
        for i in 0..5 {
            engine.add_entry(make_entry(
                &format!("e{i}"),
                "recurring keyword data",
                "cat",
                0.5,
            ));
        }
        for i in 5..8 {
            engine.add_entry(make_entry(&format!("e{i}"), "other term info", "cat", 0.5));
        }
        engine.extract_patterns();
        let top = engine.top_patterns(1);
        assert!(!top.is_empty());
        if engine.top_patterns(2).len() >= 2 {
            assert!(top[0].frequency >= engine.top_patterns(2)[1].frequency);
        }
    }

    #[test]
    fn empty_engine_safe() {
        let mut engine = ConsolidationEngine::new(0.5);
        assert_eq!(engine.entry_count(), 0);
        assert_eq!(engine.pattern_count(), 0);
        let result = engine.consolidate();
        assert_eq!(result.merged_count, 0);
        assert_eq!(result.patterns_found, 0);
        assert_eq!(result.pruned_count, 0);
        assert_eq!(result.total_remaining, 0);
    }

    #[test]
    fn similarity_threshold_affects_merging() {
        let mut strict = ConsolidationEngine::new(0.99);
        strict.add_entry(make_entry("e1", "the quick brown fox", "general", 0.5));
        strict.add_entry(make_entry("e2", "the quick brown dog", "general", 0.5));
        let strict_merged = strict.merge_similar();

        let mut loose = ConsolidationEngine::new(0.3);
        loose.add_entry(make_entry("e1", "the quick brown fox", "general", 0.5));
        loose.add_entry(make_entry("e2", "the quick brown dog", "general", 0.5));
        let loose_merged = loose.merge_similar();

        assert!(loose_merged >= strict_merged);
    }

    #[test]
    fn duplicate_entries_merge() {
        let mut engine = ConsolidationEngine::new(0.5);
        engine.add_entry(make_entry("e1", "exact same content here", "general", 0.4));
        engine.add_entry(make_entry("e2", "exact same content here", "general", 0.6));
        let merged = engine.merge_similar();
        assert_eq!(merged, 1);
        assert_eq!(engine.entry_count(), 1);
        let remaining = &engine.entries_by_category("general")[0];
        assert!((remaining.importance - 0.6).abs() < f64::EPSILON);
        assert_eq!(remaining.access_count, 0);
    }

    #[test]
    fn prune_spares_frequently_accessed() {
        let mut engine = ConsolidationEngine::new(0.5);
        let mut entry = make_entry("e1", "low importance data", "general", 0.1);
        entry.access_count = 5;
        engine.add_entry(entry);
        engine.add_entry(make_entry("e2", "also low importance", "general", 0.1));
        let pruned = engine.prune_redundant(0.3);
        assert_eq!(pruned, 1);
        assert_eq!(engine.entry_count(), 1);
        assert_eq!(engine.entries_by_category("general")[0].id, "e1");
    }

    #[test]
    fn jaccard_identical_strings() {
        let sim = jaccard_similarity("hello world foo", "hello world foo");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_strings() {
        let sim = jaccard_similarity("alpha beta gamma", "delta epsilon zeta");
        assert!((sim - 0.0).abs() < f64::EPSILON);
    }
}
