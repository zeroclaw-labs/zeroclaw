// Skill self-improvement: atomically updates existing skill documents
// after the agent uses them successfully.
//
// Gated behind `#[cfg(feature = "skill-creation")]` at the module level
// in `src/skills/mod.rs`.

use crate::config::SkillImprovementConfig;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

/// Manages skill self-improvement with cooldown tracking.
pub struct SkillImprover {
    workspace_dir: PathBuf,
    config: SkillImprovementConfig,
    cooldowns: HashMap<String, Instant>,
}

impl SkillImprover {
    pub fn new(workspace_dir: PathBuf, config: SkillImprovementConfig) -> Self {
        Self {
            workspace_dir,
            config,
            cooldowns: HashMap::new(),
        }
    }

    /// Check whether a skill is eligible for improvement (enabled + cooldown expired).
    pub fn should_improve_skill(&self, slug: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        if let Some(last) = self.cooldowns.get(slug) {
            let elapsed = Instant::now().saturating_duration_since(*last);
            elapsed.as_secs() >= self.config.cooldown_secs
        } else {
            true
        }
    }

    /// Improve an existing skill file atomically.
    ///
    /// Writes to a temp file first, validates, then renames over the original.
    /// Returns `Ok(Some(slug))` if the skill was improved, `Ok(None)` if skipped
    /// (disabled, cooldown active, or validation failed).
    pub async fn improve_skill(
        &mut self,
        slug: &str,
        improved_content: &str,
        improvement_reason: &str,
    ) -> Result<Option<String>> {
        if !self.should_improve_skill(slug) {
            return Ok(None);
        }

        // Validate the improved content before writing.
        validate_skill_content(improved_content)?;

        let skill_dir = self.skills_dir().join(slug);
        let toml_path = skill_dir.join("SKILL.toml");

        if !toml_path.exists() {
            bail!("Skill file not found: {}", toml_path.display());
        }

        // Read existing content to preserve audit trail.
        let existing = tokio::fs::read_to_string(&toml_path)
            .await
            .with_context(|| format!("Failed to read {}", toml_path.display()))?;

        // Build the updated content with audit metadata appended.
        let now = chrono::Utc::now().to_rfc3339();
        let audit_entry = format!(
            "\n# Improvement: {now}\n# Reason: {}\n",
            improvement_reason.replace('\n', " ")
        );

        let updated = append_improvement_metadata(improved_content, &now, improvement_reason);

        // Preserve any existing audit trail from the original file.
        let audit_trail = extract_audit_trail(&existing);
        let final_content = if audit_trail.is_empty() {
            format!("{updated}{audit_entry}")
        } else {
            format!("{updated}\n{audit_trail}{audit_entry}")
        };

        // Atomic write: temp file → validate → rename.
        let temp_path = skill_dir.join(".SKILL.toml.tmp");
        tokio::fs::write(&temp_path, final_content.as_bytes())
            .await
            .with_context(|| format!("Failed to write temp file: {}", temp_path.display()))?;

        // Validate the temp file is readable and valid.
        let written = tokio::fs::read_to_string(&temp_path).await?;
        if let Err(e) = validate_skill_content(&written) {
            // Clean up temp file and abort.
            let _ = tokio::fs::remove_file(&temp_path).await;
            bail!("Validation failed after write: {e}");
        }

        // Rename atomically (same filesystem).
        tokio::fs::rename(&temp_path, &toml_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to rename {} to {}",
                    temp_path.display(),
                    toml_path.display()
                )
            })?;

        // Record cooldown.
        self.cooldowns.insert(slug.to_string(), Instant::now());

        Ok(Some(slug.to_string()))
    }

    fn skills_dir(&self) -> PathBuf {
        self.workspace_dir.join("skills")
    }
}

/// Validate skill content: must be non-empty, valid UTF-8 (already a &str),
/// and contain parseable TOML front-matter with a [skill] section.
pub fn validate_skill_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        bail!("Skill content is empty");
    }

    // Must contain a [skill] section.
    #[derive(serde::Deserialize)]
    struct Partial {
        skill: PartialSkill,
    }
    #[derive(serde::Deserialize)]
    struct PartialSkill {
        name: Option<String>,
    }

    // Try parsing as TOML. Strip trailing comment lines that aren't valid TOML.
    let toml_portion = strip_trailing_comments(content);
    let parsed: Partial = toml::from_str(&toml_portion)
        .with_context(|| "Skill content contains malformed TOML front-matter")?;

    if parsed.skill.name.as_deref().unwrap_or("").is_empty() {
        bail!("Skill TOML missing required 'name' field");
    }

    Ok(())
}

/// Append updated_at and improvement_reason to the [skill] section's front-matter.
fn append_improvement_metadata(content: &str, timestamp: &str, reason: &str) -> String {
    // Find the end of the [skill] section (before the first [[tools]] or end of file).
    let tools_pos = content.find("[[tools]]");
    let (skill_section, rest) = match tools_pos {
        Some(pos) => (&content[..pos], &content[pos..]),
        None => (content, ""),
    };

    // Check if updated_at already exists; if so, replace it.
    let skill_section = if skill_section.contains("updated_at") {
        let mut lines: Vec<&str> = skill_section.lines().collect();
        lines.retain(|line| !line.trim_start().starts_with("updated_at"));
        lines.join("\n") + "\n"
    } else {
        skill_section.to_string()
    };

    let escaped_reason = reason.replace('"', "\\\"").replace('\n', " ");
    format!(
        "{skill_section}updated_at = \"{timestamp}\"\nimprovement_reason = \"{escaped_reason}\"\n{rest}"
    )
}

/// Extract existing audit trail comments (lines starting with `# Improvement:` or `# Reason:`).
fn extract_audit_trail(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("# Improvement:") || trimmed.starts_with("# Reason:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip trailing comment-only lines that would break TOML parsing.
fn strip_trailing_comments(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut end = lines.len();
    while end > 0 {
        let line = lines[end - 1].trim();
        if line.is_empty() || line.starts_with('#') {
            end -= 1;
        } else {
            break;
        }
    }
    lines[..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Validation ──────────────────────────────────────────

    #[test]
    fn validate_empty_content_rejected() {
        assert!(validate_skill_content("").is_err());
        assert!(validate_skill_content("   \n  ").is_err());
    }

    #[test]
    fn validate_malformed_toml_rejected() {
        assert!(validate_skill_content("not valid toml {{").is_err());
    }

    #[test]
    fn validate_missing_name_rejected() {
        let content = r#"
[skill]
description = "no name field"
version = "0.1.0"
"#;
        assert!(validate_skill_content(content).is_err());
    }

    #[test]
    fn validate_valid_content_accepted() {
        let content = r#"
[skill]
name = "test-skill"
description = "A test skill"
version = "0.1.0"
"#;
        assert!(validate_skill_content(content).is_ok());
    }

    // ── Cooldown enforcement ────────────────────────────────

    #[test]
    fn cooldown_allows_first_improvement() {
        let improver = SkillImprover::new(
            PathBuf::from("/tmp/test"),
            SkillImprovementConfig {
                enabled: true,
                cooldown_secs: 3600,
            },
        );
        assert!(improver.should_improve_skill("test-skill"));
    }

    #[test]
    fn cooldown_blocks_recent_improvement() {
        let mut improver = SkillImprover::new(
            PathBuf::from("/tmp/test"),
            SkillImprovementConfig {
                enabled: true,
                cooldown_secs: 3600,
            },
        );
        improver
            .cooldowns
            .insert("test-skill".to_string(), Instant::now());
        assert!(!improver.should_improve_skill("test-skill"));
    }

    #[test]
    fn cooldown_disabled_blocks_all() {
        let improver = SkillImprover::new(
            PathBuf::from("/tmp/test"),
            SkillImprovementConfig {
                enabled: false,
                cooldown_secs: 0,
            },
        );
        assert!(!improver.should_improve_skill("test-skill"));
    }

    // ── Atomic write ────────────────────────────────────────

    #[tokio::test]
    async fn improve_skill_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();

        let original = r#"[skill]
name = "test-skill"
description = "Original description"
version = "0.1.0"
author = "zeroclaw-auto"
tags = ["auto-generated"]
"#;
        tokio::fs::write(skill_dir.join("SKILL.toml"), original)
            .await
            .unwrap();

        let mut improver = SkillImprover::new(
            dir.path().to_path_buf(),
            SkillImprovementConfig {
                enabled: true,
                cooldown_secs: 0,
            },
        );

        let improved = r#"[skill]
name = "test-skill"
description = "Improved description with better steps"
version = "0.1.1"
author = "zeroclaw-auto"
tags = ["auto-generated", "improved"]
"#;

        let result = improver
            .improve_skill("test-skill", improved, "Added better step descriptions")
            .await
            .unwrap();
        assert_eq!(result, Some("test-skill".to_string()));

        // Verify the file was updated.
        let content = tokio::fs::read_to_string(skill_dir.join("SKILL.toml"))
            .await
            .unwrap();
        assert!(content.contains("Improved description"));
        assert!(content.contains("updated_at"));
        assert!(content.contains("improvement_reason"));

        // Verify temp file was cleaned up.
        assert!(!skill_dir.join(".SKILL.toml.tmp").exists());
    }

    #[tokio::test]
    async fn improve_skill_invalid_content_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();

        let original = r#"[skill]
name = "test-skill"
description = "Original"
version = "0.1.0"
"#;
        tokio::fs::write(skill_dir.join("SKILL.toml"), original)
            .await
            .unwrap();

        let mut improver = SkillImprover::new(
            dir.path().to_path_buf(),
            SkillImprovementConfig {
                enabled: true,
                cooldown_secs: 0,
            },
        );

        // Empty content should fail validation.
        let result = improver
            .improve_skill("test-skill", "", "bad improvement")
            .await;
        assert!(result.is_err());

        // Original file should be untouched.
        let content = tokio::fs::read_to_string(skill_dir.join("SKILL.toml"))
            .await
            .unwrap();
        assert!(content.contains("Original"));
    }

    #[tokio::test]
    async fn improve_skill_cooldown_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.toml"),
            "[skill]\nname = \"test-skill\"\n",
        )
        .await
        .unwrap();

        let mut improver = SkillImprover::new(
            dir.path().to_path_buf(),
            SkillImprovementConfig {
                enabled: true,
                cooldown_secs: 9999,
            },
        );
        // Record a recent cooldown.
        improver
            .cooldowns
            .insert("test-skill".to_string(), Instant::now());

        let result = improver
            .improve_skill(
                "test-skill",
                "[skill]\nname = \"test-skill\"\ndescription = \"better\"\n",
                "test",
            )
            .await
            .unwrap();
        assert!(result.is_none());
    }

    // ── Metadata appending ──────────────────────────────────

    #[test]
    fn append_metadata_adds_fields() {
        let content = r#"[skill]
name = "test"
description = "A skill"
version = "0.1.0"
"#;
        let result = append_improvement_metadata(content, "2026-01-01T00:00:00Z", "Better steps");
        assert!(result.contains("updated_at = \"2026-01-01T00:00:00Z\""));
        assert!(result.contains("improvement_reason = \"Better steps\""));
    }

    #[test]
    fn append_metadata_preserves_tools() {
        let content = r#"[skill]
name = "test"
description = "A skill"
version = "0.1.0"

[[tools]]
name = "action"
kind = "shell"
command = "echo hello"
"#;
        let result = append_improvement_metadata(content, "2026-01-01T00:00:00Z", "Improved");
        assert!(result.contains("[[tools]]"));
        assert!(result.contains("echo hello"));
    }

    // ── Audit trail extraction ──────────────────────────────

    #[test]
    fn extract_audit_trail_from_content() {
        let content = r#"[skill]
name = "test"
# Improvement: 2026-01-01T00:00:00Z
# Reason: First improvement
# Improvement: 2026-02-01T00:00:00Z
# Reason: Second improvement
"#;
        let trail = extract_audit_trail(content);
        assert!(trail.contains("First improvement"));
        assert!(trail.contains("Second improvement"));
        assert_eq!(trail.lines().count(), 4);
    }

    #[test]
    fn extract_audit_trail_empty_when_none() {
        let content = "[skill]\nname = \"test\"\n";
        let trail = extract_audit_trail(content);
        assert!(trail.is_empty());
    }
}
