//! SOUL.md parser — loads soul/v1 format (YAML frontmatter + markdown sections).
//!
//! Format:
//! ```text
//! ---
//! soul_version: 1
//! name: AgentName
//! ---
//!
//! ## Values
//! - autonomy
//! - truth-seeking
//!
//! ## Personality
//! - curiosity: high
//! - patience: medium
//!
//! ## Boundaries
//! - Never leak secrets
//!
//! ## Capabilities
//! - coding
//! - research
//!
//! ## Relationships
//! - creator: Ricardo
//!
//! ## Financial Character
//! - risk_tolerance: conservative
//!
//! ## Bio
//! Free-form origin story or biography text.
//!
//! ## Genesis Prompt
//! The original prompt that defined this agent's soul.
//! ```

use super::model::SoulModel;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Parse a SOUL.md file into a `SoulModel`.
pub fn parse_soul_file(path: &Path) -> Result<SoulModel> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read soul file: {}", path.display()))?;
    parse_soul_content(&content)
}

/// Parse soul content string into a `SoulModel`.
pub fn parse_soul_content(content: &str) -> Result<SoulModel> {
    let mut soul = SoulModel::default();

    let body = extract_frontmatter(content, &mut soul);
    parse_sections(body, &mut soul);

    Ok(soul)
}

/// Extract YAML frontmatter (between `---` delimiters) and return the remaining body.
fn extract_frontmatter<'a>(content: &'a str, soul: &mut SoulModel) -> &'a str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }

    // Find the closing `---`
    let after_open = &trimmed[3..];
    let close_pos = match after_open.find("\n---") {
        Some(pos) => pos,
        None => return content,
    };

    let frontmatter = &after_open[..close_pos];
    let body_start = 3 + close_pos + 4; // skip opening --- + content + \n---
    let body = if body_start < trimmed.len() {
        &trimmed[body_start..]
    } else {
        ""
    };

    // Parse simple YAML key-value pairs from frontmatter
    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => soul.name = value.to_string(),
                // soul_version is validated but not stored — we only support v1
                "soul_version" | "version" => {
                    if value != "1" {
                        tracing::warn!("Unsupported soul version: {value}, expected 1");
                    }
                }
                _ => {
                    tracing::debug!("Unknown frontmatter key: {key}");
                }
            }
        }
    }

    body
}

/// Parse markdown sections (## headers) into the soul model.
fn parse_sections(body: &str, soul: &mut SoulModel) {
    let mut current_section: Option<&str> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in body.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            // Flush previous section
            if let Some(section) = current_section {
                apply_section(soul, section, &current_lines);
            }
            current_section = Some(header.trim());
            current_lines.clear();
        } else {
            current_lines.push(line);
        }
    }

    // Flush final section
    if let Some(section) = current_section {
        apply_section(soul, section, &current_lines);
    }
}

/// Apply parsed lines to the appropriate soul model field.
fn apply_section(soul: &mut SoulModel, section: &str, lines: &[&str]) {
    let section_lower = section.to_lowercase();
    match section_lower.as_str() {
        "values" => {
            soul.values = parse_list_items(lines);
        }
        "personality" => {
            soul.personality = parse_key_value_items(lines);
        }
        "boundaries" => {
            soul.boundaries = parse_list_items(lines);
        }
        "capabilities" => {
            soul.capabilities = parse_list_items(lines);
        }
        "relationships" => {
            soul.relationships = parse_key_value_items(lines);
        }
        "financial character" => {
            soul.financial_character = parse_key_value_items(lines);
        }
        "bio" => {
            let text = lines.join("\n").trim().to_string();
            if !text.is_empty() {
                soul.bio = Some(text);
            }
        }
        "genesis prompt" => {
            let text = lines.join("\n").trim().to_string();
            if !text.is_empty() {
                soul.genesis_prompt = Some(text);
            }
        }
        _ => {
            tracing::debug!("Unknown soul section: {section}");
        }
    }
}

/// Parse markdown list items (`- value`).
fn parse_list_items(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse markdown list items with key-value pairs (`- key: value`).
fn parse_key_value_items(lines: &[&str]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in lines {
        let trimmed = line.trim();
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "));
        if let Some(item) = item {
            if let Some((key, value)) = item.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                if !key.is_empty() && !value.is_empty() {
                    map.insert(key, value);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_soul() {
        let content = r#"---
soul_version: 1
name: TestAgent
---

## Values
- honesty
- curiosity
"#;

        let soul = parse_soul_content(content).unwrap();
        assert_eq!(soul.name, "TestAgent");
        assert_eq!(soul.values, vec!["honesty", "curiosity"]);
    }

    #[test]
    fn parse_full_soul() {
        let content = r#"---
soul_version: 1
name: ZeroClaw
---

## Values
- autonomy
- truth-seeking
- efficiency

## Personality
- curiosity: high
- patience: medium
- assertiveness: low

## Boundaries
- Never leak secrets
- Never bypass oversight

## Capabilities
- coding
- research
- system administration

## Relationships
- creator: zeroclaw_user

## Financial Character
- risk_tolerance: conservative
- spending_style: frugal

## Bio
An autonomous agent runtime built in Rust. Born from the desire
for zero-overhead, zero-compromise AI assistance.

## Genesis Prompt
You are ZeroClaw, an autonomous agent focused on efficiency and truth.
"#;

        let soul = parse_soul_content(content).unwrap();
        assert_eq!(soul.name, "ZeroClaw");
        assert_eq!(soul.values.len(), 3);
        assert!(soul.values.contains(&"autonomy".to_string()));
        assert_eq!(soul.personality.get("curiosity").unwrap(), "high");
        assert_eq!(soul.boundaries.len(), 2);
        assert_eq!(soul.capabilities.len(), 3);
        assert_eq!(soul.relationships.get("creator").unwrap(), "zeroclaw_user");
        assert_eq!(
            soul.financial_character.get("risk_tolerance").unwrap(),
            "conservative"
        );
        assert!(soul.bio.as_ref().unwrap().contains("autonomous agent"));
        assert!(soul.genesis_prompt.as_ref().unwrap().contains("ZeroClaw"));
    }

    #[test]
    fn parse_without_frontmatter() {
        let content = r#"## Values
- simplicity
"#;

        let soul = parse_soul_content(content).unwrap();
        assert!(soul.name.is_empty());
        assert_eq!(soul.values, vec!["simplicity"]);
    }

    #[test]
    fn parse_empty_content() {
        let soul = parse_soul_content("").unwrap();
        assert!(soul.name.is_empty());
        assert!(soul.values.is_empty());
    }

    #[test]
    fn parse_frontmatter_only() {
        let content = "---\nname: JustName\n---\n";
        let soul = parse_soul_content(content).unwrap();
        assert_eq!(soul.name, "JustName");
        assert!(soul.values.is_empty());
    }

    #[test]
    fn parse_list_items_handles_asterisks() {
        let lines = vec!["  - alpha", "  * beta", "  - gamma"];
        let items = parse_list_items(&lines);
        assert_eq!(items, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn parse_key_value_items_skips_empty() {
        let lines = vec!["  - key1: value1", "  - : empty_key", "  - no_colon"];
        let items = parse_key_value_items(&lines);
        assert_eq!(items.len(), 1);
        assert_eq!(items.get("key1").unwrap(), "value1");
    }

    #[test]
    fn parse_soul_file_not_found() {
        let result = parse_soul_file(Path::new("/nonexistent/SOUL.md"));
        assert!(result.is_err());
    }
}
