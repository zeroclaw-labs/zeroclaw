//! Soul reflection pipeline — analyzes conversation history and updates the soul model.
//!
//! The reflection pipeline:
//! 1. Takes a list of conversation messages
//! 2. Extracts discovered capabilities, relationships, and financial traits
//! 3. Merges them into the existing SoulModel
//! 4. Writes the updated SOUL.md back to disk

use super::model::SoulModel;
use super::parser::parse_soul_file;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

/// A reflection insight extracted from conversation analysis.
#[derive(Debug, Clone, Default)]
pub struct ReflectionInsights {
    /// Newly discovered capabilities (e.g. "deployed a web server")
    pub new_capabilities: Vec<String>,
    /// Newly discovered or updated relationships
    pub new_relationships: HashMap<String, String>,
    /// Updated financial character traits
    pub financial_updates: HashMap<String, String>,
    /// Updated personality traits
    pub personality_updates: HashMap<String, String>,
}

impl ReflectionInsights {
    /// Returns true if no insights were extracted.
    pub fn is_empty(&self) -> bool {
        self.new_capabilities.is_empty()
            && self.new_relationships.is_empty()
            && self.financial_updates.is_empty()
            && self.personality_updates.is_empty()
    }

    /// Count total number of insights.
    pub fn count(&self) -> usize {
        self.new_capabilities.len()
            + self.new_relationships.len()
            + self.financial_updates.len()
            + self.personality_updates.len()
    }
}

/// Apply reflection insights to a soul model (merge, not overwrite).
pub fn apply_insights(soul: &mut SoulModel, insights: &ReflectionInsights) {
    // Merge capabilities (deduplicate)
    for cap in &insights.new_capabilities {
        if !soul.capabilities.iter().any(|c| c == cap) {
            soul.capabilities.push(cap.clone());
        }
    }

    // Merge relationships (update existing, add new)
    for (key, value) in &insights.new_relationships {
        soul.relationships.insert(key.clone(), value.clone());
    }

    // Merge financial character
    for (key, value) in &insights.financial_updates {
        soul.financial_character.insert(key.clone(), value.clone());
    }

    // Merge personality
    for (key, value) in &insights.personality_updates {
        soul.personality.insert(key.clone(), value.clone());
    }
}

/// Write a SoulModel back to SOUL.md format.
pub fn write_soul_file(path: &Path, soul: &SoulModel) -> Result<()> {
    let content = render_soul_md(soul);

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create soul directory: {}", parent.display()))?;
    }

    std::fs::write(path, &content)
        .with_context(|| format!("Failed to write soul file: {}", path.display()))?;

    Ok(())
}

/// Render a SoulModel into SOUL.md format string.
pub fn render_soul_md(soul: &SoulModel) -> String {
    let mut out = String::new();

    // Frontmatter
    out.push_str("---\n");
    let _ = writeln!(out, "soul_version: 1");
    if !soul.name.is_empty() {
        let _ = writeln!(out, "name: {}", soul.name);
    }
    out.push_str("---\n");

    // Values
    if !soul.values.is_empty() {
        out.push_str("\n## Values\n");
        for v in &soul.values {
            let _ = writeln!(out, "- {v}");
        }
    }

    // Personality
    if !soul.personality.is_empty() {
        out.push_str("\n## Personality\n");
        let mut keys: Vec<_> = soul.personality.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(out, "- {}: {}", k, soul.personality[k]);
        }
    }

    // Boundaries
    if !soul.boundaries.is_empty() {
        out.push_str("\n## Boundaries\n");
        for b in &soul.boundaries {
            let _ = writeln!(out, "- {b}");
        }
    }

    // Capabilities
    if !soul.capabilities.is_empty() {
        out.push_str("\n## Capabilities\n");
        for c in &soul.capabilities {
            let _ = writeln!(out, "- {c}");
        }
    }

    // Relationships
    if !soul.relationships.is_empty() {
        out.push_str("\n## Relationships\n");
        let mut keys: Vec<_> = soul.relationships.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(out, "- {}: {}", k, soul.relationships[k]);
        }
    }

    // Financial Character
    if !soul.financial_character.is_empty() {
        out.push_str("\n## Financial Character\n");
        let mut keys: Vec<_> = soul.financial_character.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(out, "- {}: {}", k, soul.financial_character[k]);
        }
    }

    // Bio
    if let Some(ref bio) = soul.bio {
        if !bio.is_empty() {
            let _ = write!(out, "\n## Bio\n{bio}\n");
        }
    }

    // Genesis Prompt
    if let Some(ref genesis) = soul.genesis_prompt {
        if !genesis.is_empty() {
            let _ = write!(out, "\n## Genesis Prompt\n{genesis}\n");
        }
    }

    out
}

/// Load, apply insights, and save a soul file (full reflection cycle).
pub fn reflect_and_save(soul_path: &Path, insights: &ReflectionInsights) -> Result<SoulModel> {
    let mut soul = if soul_path.exists() {
        parse_soul_file(soul_path)?
    } else {
        SoulModel::default()
    };

    apply_insights(&mut soul, insights);
    write_soul_file(soul_path, &soul)?;

    Ok(soul)
}

/// Memory token budgets for tiered memory management.
#[derive(Debug, Clone)]
pub struct MemoryTokenBudgets {
    /// Working memory (session context) — token budget
    pub working: usize,
    /// Episodic memory (events/experiences) — token budget
    pub episodic: usize,
    /// Semantic memory (knowledge/facts) — token budget
    pub semantic: usize,
    /// Procedural memory (skills/procedures) — token budget
    pub procedural: usize,
}

impl Default for MemoryTokenBudgets {
    fn default() -> Self {
        Self {
            working: 4000,
            episodic: 2000,
            semantic: 2000,
            procedural: 1000,
        }
    }
}

impl MemoryTokenBudgets {
    /// Total token budget across all tiers.
    pub fn total(&self) -> usize {
        self.working + self.episodic + self.semantic + self.procedural
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_soul() -> SoulModel {
        SoulModel {
            name: "TestAgent".into(),
            values: vec!["honesty".into()],
            capabilities: vec!["coding".into()],
            relationships: {
                let mut m = HashMap::new();
                m.insert("creator".into(), "zeroclaw_user".into());
                m
            },
            ..Default::default()
        }
    }

    #[test]
    fn empty_insights_is_empty() {
        let insights = ReflectionInsights::default();
        assert!(insights.is_empty());
        assert_eq!(insights.count(), 0);
    }

    #[test]
    fn apply_insights_adds_new_capabilities() {
        let mut soul = test_soul();
        let insights = ReflectionInsights {
            new_capabilities: vec!["research".into(), "deployment".into()],
            ..Default::default()
        };

        apply_insights(&mut soul, &insights);
        assert_eq!(soul.capabilities.len(), 3);
        assert!(soul.capabilities.contains(&"coding".into()));
        assert!(soul.capabilities.contains(&"research".into()));
        assert!(soul.capabilities.contains(&"deployment".into()));
    }

    #[test]
    fn apply_insights_deduplicates_capabilities() {
        let mut soul = test_soul();
        let insights = ReflectionInsights {
            new_capabilities: vec!["coding".into(), "research".into()],
            ..Default::default()
        };

        apply_insights(&mut soul, &insights);
        // "coding" already existed, should not be duplicated
        assert_eq!(soul.capabilities.len(), 2);
    }

    #[test]
    fn apply_insights_merges_relationships() {
        let mut soul = test_soul();
        let insights = ReflectionInsights {
            new_relationships: {
                let mut m = HashMap::new();
                m.insert("collaborator".into(), "zeroclaw_agent_b".into());
                m
            },
            ..Default::default()
        };

        apply_insights(&mut soul, &insights);
        assert_eq!(soul.relationships.len(), 2);
        assert_eq!(
            soul.relationships.get("collaborator").unwrap(),
            "zeroclaw_agent_b"
        );
    }

    #[test]
    fn apply_insights_updates_existing_relationships() {
        let mut soul = test_soul();
        let insights = ReflectionInsights {
            new_relationships: {
                let mut m = HashMap::new();
                m.insert("creator".into(), "zeroclaw_operator".into());
                m
            },
            ..Default::default()
        };

        apply_insights(&mut soul, &insights);
        assert_eq!(soul.relationships.len(), 1);
        assert_eq!(
            soul.relationships.get("creator").unwrap(),
            "zeroclaw_operator"
        );
    }

    #[test]
    fn render_and_reparse_roundtrip() {
        let soul = SoulModel {
            name: "ZeroClaw".into(),
            values: vec!["autonomy".into(), "truth".into()],
            personality: {
                let mut m = HashMap::new();
                m.insert("curiosity".into(), "high".into());
                m
            },
            boundaries: vec!["never leak secrets".into()],
            capabilities: vec!["coding".into(), "research".into()],
            relationships: {
                let mut m = HashMap::new();
                m.insert("creator".into(), "zeroclaw_user".into());
                m
            },
            financial_character: {
                let mut m = HashMap::new();
                m.insert("risk_tolerance".into(), "conservative".into());
                m
            },
            bio: Some("An autonomous agent.".into()),
            genesis_prompt: Some("You are ZeroClaw.".into()),
        };

        let rendered = render_soul_md(&soul);
        let reparsed = super::super::parser::parse_soul_content(&rendered).unwrap();

        assert_eq!(reparsed.name, "ZeroClaw");
        assert_eq!(reparsed.values, vec!["autonomy", "truth"]);
        assert_eq!(reparsed.personality.get("curiosity").unwrap(), "high");
        assert_eq!(reparsed.boundaries, vec!["never leak secrets"]);
        assert_eq!(reparsed.capabilities.len(), 2);
        assert_eq!(
            reparsed.relationships.get("creator").unwrap(),
            "zeroclaw_user"
        );
        assert_eq!(
            reparsed.financial_character.get("risk_tolerance").unwrap(),
            "conservative"
        );
        assert!(reparsed.bio.as_ref().unwrap().contains("autonomous agent"));
        assert!(reparsed
            .genesis_prompt
            .as_ref()
            .unwrap()
            .contains("ZeroClaw"));
    }

    #[test]
    fn write_and_read_soul_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        let soul = test_soul();

        write_soul_file(&soul_path, &soul).unwrap();
        assert!(soul_path.exists());

        let loaded = parse_soul_file(&soul_path).unwrap();
        assert_eq!(loaded.name, "TestAgent");
        assert_eq!(loaded.values, vec!["honesty"]);
        assert_eq!(loaded.capabilities, vec!["coding"]);
    }

    #[test]
    fn reflect_and_save_creates_new_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        let insights = ReflectionInsights {
            new_capabilities: vec!["web_scraping".into()],
            ..Default::default()
        };

        let result = reflect_and_save(&soul_path, &insights).unwrap();
        assert!(soul_path.exists());
        assert!(result.capabilities.contains(&"web_scraping".into()));
    }

    #[test]
    fn reflect_and_save_updates_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul_path = tmp.path().join("SOUL.md");

        // Create initial soul
        let initial = test_soul();
        write_soul_file(&soul_path, &initial).unwrap();

        // Apply insights
        let insights = ReflectionInsights {
            new_capabilities: vec!["devops".into()],
            personality_updates: {
                let mut m = HashMap::new();
                m.insert("patience".into(), "high".into());
                m
            },
            ..Default::default()
        };

        let result = reflect_and_save(&soul_path, &insights).unwrap();
        assert!(result.capabilities.contains(&"coding".into()));
        assert!(result.capabilities.contains(&"devops".into()));
        assert_eq!(result.personality.get("patience").unwrap(), "high");
    }

    #[test]
    fn memory_token_budgets_default() {
        let budgets = MemoryTokenBudgets::default();
        assert_eq!(budgets.total(), 9000);
        assert_eq!(budgets.working, 4000);
    }

    #[test]
    fn insights_count() {
        let insights = ReflectionInsights {
            new_capabilities: vec!["a".into(), "b".into()],
            new_relationships: {
                let mut m = HashMap::new();
                m.insert("x".into(), "y".into());
                m
            },
            ..Default::default()
        };
        assert_eq!(insights.count(), 3);
        assert!(!insights.is_empty());
    }
}
