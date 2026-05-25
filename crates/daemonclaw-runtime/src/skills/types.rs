use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Regex pattern for valid skill names per agentskills.io spec.
const SKILL_NAME_PATTERN: &str = r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$";
const SKILL_NAME_MAX_LEN: usize = 64;
const SKILL_DESCRIPTION_MAX_LEN: usize = 1024;

/// Origin of a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    Autonomous,
    Manual,
    Imported,
}

impl std::fmt::Display for SkillSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Autonomous => write!(f, "autonomous"),
            Self::Manual => write!(f, "manual"),
            Self::Imported => write!(f, "imported"),
        }
    }
}

/// Category that determines which directory a skill lives in and
/// whether the curator can modify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillCategory {
    Bundled,
    Imported,
    Agent,
}

impl std::fmt::Display for SkillCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bundled => write!(f, "bundled"),
            Self::Imported => write!(f, "imported"),
            Self::Agent => write!(f, "agent"),
        }
    }
}

/// DaemonClaw-specific metadata extensions stored in the YAML
/// frontmatter's `metadata` key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkillMeta {
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default = "default_version_str")]
    pub version: String,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default = "default_source")]
    pub source: SkillSource,
    #[serde(default)]
    pub tool_call_count: usize,
    #[serde(default)]
    pub usage_count: u64,
    #[serde(default)]
    pub last_used: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

fn default_author() -> String {
    "daemonclaw-agent".into()
}

fn default_version_str() -> String {
    "1".into()
}

fn default_source() -> SkillSource {
    SkillSource::Autonomous
}

impl Default for AgentSkillMeta {
    fn default() -> Self {
        Self {
            author: default_author(),
            version: default_version_str(),
            created: None,
            updated: None,
            source: default_source(),
            tool_call_count: 0,
            usage_count: 0,
            last_used: None,
            pinned: false,
        }
    }
}

/// YAML frontmatter parsed from a SKILL.md file.
/// Conforms to the agentskills.io specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub metadata: AgentSkillMeta,
}

/// A fully loaded agent skill — frontmatter + markdown body + filesystem location.
#[derive(Debug, Clone)]
pub struct AgentSkill {
    pub frontmatter: AgentSkillFrontmatter,
    pub body: String,
    pub category: SkillCategory,
    pub dir_path: PathBuf,
}

impl AgentSkill {
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }

    pub fn description(&self) -> &str {
        &self.frontmatter.description
    }

    pub fn meta(&self) -> &AgentSkillMeta {
        &self.frontmatter.metadata
    }

    pub fn meta_mut(&mut self) -> &mut AgentSkillMeta {
        &mut self.frontmatter.metadata
    }
}

/// Validation errors for skill frontmatter.
#[derive(Debug, Clone)]
pub enum SkillValidationError {
    NameTooLong { len: usize, max: usize },
    NameInvalid { name: String, reason: String },
    DescriptionEmpty,
    DescriptionTooLong { len: usize, max: usize },
    NameDirMismatch { name: String, dir_name: String },
}

impl std::fmt::Display for SkillValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameTooLong { len, max } => {
                write!(f, "skill name is {len} chars (max {max})")
            }
            Self::NameInvalid { name, reason } => {
                write!(f, "invalid skill name '{name}': {reason}")
            }
            Self::DescriptionEmpty => write!(f, "skill description is empty"),
            Self::DescriptionTooLong { len, max } => {
                write!(f, "skill description is {len} chars (max {max})")
            }
            Self::NameDirMismatch { name, dir_name } => {
                write!(
                    f,
                    "skill name '{name}' does not match directory name '{dir_name}'"
                )
            }
        }
    }
}

impl std::error::Error for SkillValidationError {}

/// Validate a skill name against the agentskills.io spec.
pub fn validate_skill_name(name: &str) -> Result<(), SkillValidationError> {
    if name.len() > SKILL_NAME_MAX_LEN {
        return Err(SkillValidationError::NameTooLong {
            len: name.len(),
            max: SKILL_NAME_MAX_LEN,
        });
    }
    let re = regex::Regex::new(SKILL_NAME_PATTERN).unwrap();
    if !re.is_match(name) {
        return Err(SkillValidationError::NameInvalid {
            name: name.to_string(),
            reason: "must be lowercase alphanumeric with hyphens, no leading/trailing/consecutive hyphens".into(),
        });
    }
    if name.contains("--") {
        return Err(SkillValidationError::NameInvalid {
            name: name.to_string(),
            reason: "consecutive hyphens not allowed".into(),
        });
    }
    Ok(())
}

/// Validate the full frontmatter.
pub fn validate_frontmatter(
    fm: &AgentSkillFrontmatter,
    dir_name: Option<&str>,
) -> Vec<SkillValidationError> {
    let mut errors = Vec::new();
    if let Err(e) = validate_skill_name(&fm.name) {
        errors.push(e);
    }
    if fm.description.trim().is_empty() {
        errors.push(SkillValidationError::DescriptionEmpty);
    } else if fm.description.len() > SKILL_DESCRIPTION_MAX_LEN {
        errors.push(SkillValidationError::DescriptionTooLong {
            len: fm.description.len(),
            max: SKILL_DESCRIPTION_MAX_LEN,
        });
    }
    if let Some(dir) = dir_name {
        if dir != fm.name {
            errors.push(SkillValidationError::NameDirMismatch {
                name: fm.name.clone(),
                dir_name: dir.to_string(),
            });
        }
    }
    errors
}

/// Parse a SKILL.md file: extract YAML frontmatter between `---` delimiters,
/// return (frontmatter, body).
pub fn parse_skill_md(content: &str) -> anyhow::Result<(AgentSkillFrontmatter, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        anyhow::bail!("SKILL.md must start with YAML frontmatter (--- delimiter)");
    }
    let after_first = &trimmed[3..];
    let end_idx = after_first
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("missing closing --- delimiter in SKILL.md frontmatter"))?;
    let yaml_str = &after_first[..end_idx];
    let body_start = end_idx + 4; // skip "\n---"
    let body = if body_start < after_first.len() {
        after_first[body_start..].trim_start_matches('\n').to_string()
    } else {
        String::new()
    };
    let frontmatter: AgentSkillFrontmatter = serde_yaml::from_str(yaml_str)
        .map_err(|e| anyhow::anyhow!("invalid YAML frontmatter: {e}"))?;
    Ok((frontmatter, body))
}

/// Render a SKILL.md file from frontmatter + body.
pub fn render_skill_md(fm: &AgentSkillFrontmatter, body: &str) -> anyhow::Result<String> {
    let yaml = serde_yaml::to_string(fm)?;
    Ok(format!("---\n{yaml}---\n\n{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_skill_names() {
        assert!(validate_skill_name("deploy-nginx").is_ok());
        assert!(validate_skill_name("a").is_ok());
        assert!(validate_skill_name("fix-auth-bug").is_ok());
        assert!(validate_skill_name("my-skill-123").is_ok());
    }

    #[test]
    fn invalid_skill_names() {
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("-leading").is_err());
        assert!(validate_skill_name("trailing-").is_err());
        assert!(validate_skill_name("has--double").is_err());
        assert!(validate_skill_name("Has_Upper").is_err());
        assert!(validate_skill_name("has spaces").is_err());
        assert!(validate_skill_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn parse_and_render_roundtrip() {
        let input = r#"---
name: deploy-nginx
description: Deploy an nginx config and reload the service.
metadata:
  author: daemonclaw-agent
  version: "1"
  source: autonomous
  tool_call_count: 7
  usage_count: 3
  pinned: false
---

# Deploy Nginx

## When to Use
When the user asks to deploy or update an nginx configuration.

## Procedure
1. Read the current nginx config.
2. Edit the config file.
3. Test with nginx -t.
4. Reload nginx.
"#;
        let (fm, body) = parse_skill_md(input).unwrap();
        assert_eq!(fm.name, "deploy-nginx");
        assert_eq!(fm.metadata.tool_call_count, 7);
        assert_eq!(fm.metadata.usage_count, 3);
        assert!(body.contains("# Deploy Nginx"));
        assert!(body.contains("## Procedure"));

        let rendered = render_skill_md(&fm, &body).unwrap();
        let (fm2, body2) = parse_skill_md(&rendered).unwrap();
        assert_eq!(fm2.name, fm.name);
        assert_eq!(fm2.description, fm.description);
        assert_eq!(fm2.metadata.tool_call_count, fm.metadata.tool_call_count);
        assert!(body2.contains("## Procedure"));
    }

    #[test]
    fn parse_missing_frontmatter_delimiter() {
        let input = "# No frontmatter\nJust body.";
        assert!(parse_skill_md(input).is_err());
    }

    #[test]
    fn validate_frontmatter_catches_errors() {
        let fm = AgentSkillFrontmatter {
            name: "Bad--Name".into(),
            description: String::new(),
            license: None,
            metadata: AgentSkillMeta::default(),
        };
        let errors = validate_frontmatter(&fm, Some("different-dir"));
        assert!(errors.len() >= 3); // name invalid, description empty, dir mismatch
    }

    #[test]
    fn validate_frontmatter_clean() {
        let fm = AgentSkillFrontmatter {
            name: "good-skill".into(),
            description: "A useful skill.".into(),
            license: None,
            metadata: AgentSkillMeta::default(),
        };
        let errors = validate_frontmatter(&fm, Some("good-skill"));
        assert!(errors.is_empty());
    }
}
