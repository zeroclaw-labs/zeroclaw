//! Soul model — structured identity for an autonomous agent.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Core soul identity for an autonomous agent.
///
/// Captures name, values, personality, boundaries, capabilities,
/// relationships, and financial character. Loaded from `SOUL.md`
/// via the parser module.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SoulModel {
    /// Agent's display name
    #[serde(default)]
    pub name: String,

    /// Core values that guide behavior (e.g. "autonomy", "truth-seeking")
    #[serde(default)]
    pub values: Vec<String>,

    /// Personality traits as key-value pairs (e.g. "curiosity" -> "high")
    #[serde(default)]
    pub personality: HashMap<String, String>,

    /// Hard boundaries the agent must never cross
    #[serde(default)]
    pub boundaries: Vec<String>,

    /// Known capabilities (self-reported or discovered)
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Named relationships (e.g. "creator" -> "Ricardo")
    #[serde(default)]
    pub relationships: HashMap<String, String>,

    /// Financial character traits (e.g. "risk_tolerance" -> "conservative")
    #[serde(default)]
    pub financial_character: HashMap<String, String>,

    /// Free-form bio / origin story
    #[serde(default)]
    pub bio: Option<String>,

    /// The original genesis prompt text (used for alignment tracking)
    #[serde(default)]
    pub genesis_prompt: Option<String>,
}

impl SoulModel {
    /// Render the soul model into a markdown string suitable for system prompt injection.
    pub fn to_prompt_section(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();

        if !self.name.is_empty() {
            let _ = writeln!(out, "**Name:** {}", self.name);
        }

        if let Some(ref bio) = self.bio {
            if !bio.is_empty() {
                let _ = writeln!(out, "**Bio:** {bio}");
            }
        }

        if !self.values.is_empty() {
            out.push_str("\n**Core Values:**\n");
            for v in &self.values {
                let _ = writeln!(out, "- {v}");
            }
        }

        if !self.personality.is_empty() {
            out.push_str("\n**Personality:**\n");
            let mut keys: Vec<_> = self.personality.keys().collect();
            keys.sort();
            for k in keys {
                let _ = writeln!(out, "- {}: {}", k, self.personality[k]);
            }
        }

        if !self.boundaries.is_empty() {
            out.push_str("\n**Boundaries (never cross):**\n");
            for b in &self.boundaries {
                let _ = writeln!(out, "- {b}");
            }
        }

        if !self.capabilities.is_empty() {
            out.push_str("\n**Capabilities:**\n");
            for c in &self.capabilities {
                let _ = writeln!(out, "- {c}");
            }
        }

        if !self.relationships.is_empty() {
            out.push_str("\n**Relationships:**\n");
            let mut keys: Vec<_> = self.relationships.keys().collect();
            keys.sort();
            for k in keys {
                let _ = writeln!(out, "- {}: {}", k, self.relationships[k]);
            }
        }

        if !self.financial_character.is_empty() {
            out.push_str("\n**Financial Character:**\n");
            let mut keys: Vec<_> = self.financial_character.keys().collect();
            keys.sort();
            for k in keys {
                let _ = writeln!(out, "- {}: {}", k, self.financial_character[k]);
            }
        }

        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_soul_model_is_empty() {
        let soul = SoulModel::default();
        assert!(soul.name.is_empty());
        assert!(soul.values.is_empty());
        assert!(soul.to_prompt_section().is_empty());
    }

    #[test]
    fn soul_model_renders_prompt_section() {
        let soul = SoulModel {
            name: "ZeroClaw".into(),
            values: vec!["autonomy".into(), "truth".into()],
            personality: {
                let mut m = HashMap::new();
                m.insert("curiosity".into(), "high".into());
                m
            },
            boundaries: vec!["never leak secrets".into()],
            capabilities: vec!["coding".into()],
            relationships: HashMap::new(),
            financial_character: HashMap::new(),
            bio: Some("An autonomous agent runtime.".into()),
            genesis_prompt: None,
        };

        let rendered = soul.to_prompt_section();
        assert!(rendered.contains("**Name:** ZeroClaw"));
        assert!(rendered.contains("**Bio:** An autonomous agent runtime."));
        assert!(rendered.contains("- autonomy"));
        assert!(rendered.contains("- truth"));
        assert!(rendered.contains("- curiosity: high"));
        assert!(rendered.contains("- never leak secrets"));
        assert!(rendered.contains("- coding"));
    }

    #[test]
    fn soul_model_serde_roundtrip() {
        let soul = SoulModel {
            name: "TestAgent".into(),
            values: vec!["honesty".into()],
            ..Default::default()
        };

        let json = serde_json::to_string(&soul).unwrap();
        let parsed: SoulModel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "TestAgent");
        assert_eq!(parsed.values, vec!["honesty"]);
    }

    #[test]
    fn prompt_section_sorts_hashmap_keys() {
        let soul = SoulModel {
            personality: {
                let mut m = HashMap::new();
                m.insert("zeal".into(), "medium".into());
                m.insert("ambition".into(), "high".into());
                m
            },
            ..Default::default()
        };

        let rendered = soul.to_prompt_section();
        let ambition_pos = rendered.find("ambition").unwrap();
        let zeal_pos = rendered.find("zeal").unwrap();
        assert!(ambition_pos < zeal_pos);
    }
}
