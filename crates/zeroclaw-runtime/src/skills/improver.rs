use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;
use zeroclaw_config::schema::SkillImprovementConfig;
use zeroclaw_providers::ChatMessage;

const FRONT_MATTER_DELIM: &str = "---";

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

    pub fn should_improve_skill(&self, slug: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        if let Some(last) = self.cooldowns.get(slug) {
            let elapsed = Instant::now().saturating_duration_since(*last);
            if elapsed.as_secs() < self.config.cooldown_secs {
                return false;
            }
        }
        if self.is_on_disk_cooldown(slug) {
            return false;
        }
        true
    }

    // SKILL.md front-matter's `updated_at:` is bumped on every successful
    // improvement, so its age is a durable proxy for "improved recently."
    fn is_on_disk_cooldown(&self, slug: &str) -> bool {
        let md_path = self.skills_dir().join(slug).join("SKILL.md");
        let Ok(content) = std::fs::read_to_string(&md_path) else {
            return false;
        };
        let Some((front, _)) = split_front_matter(&content) else {
            return false;
        };
        let Some(value) = front_matter_value(&front, "updated_at") else {
            return false;
        };
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(value.trim()) else {
            return false;
        };
        let elapsed = chrono::Utc::now().signed_duration_since(ts);
        elapsed.num_seconds() < self.config.cooldown_secs as i64
    }

    pub async fn improve_skill(
        &mut self,
        slug: &str,
        improved_content: &str,
        improvement_reason: &str,
    ) -> Result<Option<String>> {
        validate_skill_content(improved_content)?;

        let skill_dir = self.skills_dir().join(slug);
        let md_path = skill_dir.join("SKILL.md");

        if !md_path.exists() {
            bail!("Skill file not found: {}", md_path.display());
        }

        // Read existing content to preserve audit trail comments.
        let existing = tokio::fs::read_to_string(&md_path)
            .await
            .with_context(|| format!("Failed to read {}", md_path.display()))?;

        // Build updated content with audit metadata embedded in YAML front-matter
        // plus an HTML audit-trail comment at the file's tail.
        let now = chrono::Utc::now().to_rfc3339();
        let single_line_reason = improvement_reason.replace('\n', " ");
        let audit_entry = format!("\n<!-- Improvement: {now} | Reason: {single_line_reason} -->\n");

        let updated = append_improvement_metadata(improved_content, &now, improvement_reason);

        // Preserve any existing audit trail from the original file.
        let audit_trail = extract_audit_trail(&existing);
        let final_content = if audit_trail.is_empty() {
            format!("{updated}{audit_entry}")
        } else {
            format!("{updated}\n{audit_trail}{audit_entry}")
        };

        // Atomic write: temp file → validate → rename.
        let temp_path = skill_dir.join(".SKILL.md.tmp");
        tokio::fs::write(&temp_path, final_content.as_bytes())
            .await
            .with_context(|| format!("Failed to write temp file: {}", temp_path.display()))?;

        // Validate the temp file is readable and valid.
        let written = tokio::fs::read_to_string(&temp_path).await?;
        if let Err(e) = validate_skill_content(&written) {
            let _ = tokio::fs::remove_file(&temp_path).await;
            bail!("Validation failed after write: {e}");
        }

        // Rename atomically (same filesystem).
        tokio::fs::rename(&temp_path, &md_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to rename {} to {}",
                    temp_path.display(),
                    md_path.display()
                )
            })?;

        self.cooldowns.insert(slug.to_string(), Instant::now());

        Ok(Some(slug.to_string()))
    }

    fn skills_dir(&self) -> PathBuf {
        self.workspace_dir.join("skills")
    }
}

/// Validate skill content: must be non-empty, have a parseable YAML front-matter
/// block with a non-empty `name` field.
pub fn validate_skill_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        bail!("Skill content is empty");
    }

    let Some((front, _body)) = split_front_matter(content) else {
        bail!("Skill content is missing YAML front-matter (expected `---` delimited block at top)");
    };

    let name = front_matter_value(&front, "name").unwrap_or_default();
    if name.trim().is_empty() {
        bail!("Skill front-matter missing required `name` field");
    }

    Ok(())
}

/// Split a SKILL.md into (front_matter_text, body_text).
/// Returns `None` if the file doesn't start with `---\n` or has no closing
/// `---` delimiter.
fn split_front_matter(content: &str) -> Option<(String, String)> {
    let normalized = content.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    if let Some(idx) = rest.find("\n---\n") {
        Some((rest[..idx].to_string(), rest[idx + 5..].to_string()))
    } else {
        rest.strip_suffix("\n---")
            .map(|front| (front.to_string(), String::new()))
    }
}

/// Look up a top-level key in a YAML front-matter string. Returns the raw
/// value text (still wrapped in quotes if quoted). Only handles flat `key: value`
/// — does not descend into nested mappings.
fn front_matter_value(front: &str, key: &str) -> Option<String> {
    for line in front.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue; // Nested entry — skip.
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim() == key {
            let v = v.trim();
            let unquoted = v.trim_matches('"').trim_matches('\'');
            return Some(unquoted.to_string());
        }
    }
    None
}

/// Rewrite the YAML front-matter so it carries `updated_at` and
/// `improvement_reason`, stripping any prior occurrences so the resulting
/// block stays valid YAML.
fn append_improvement_metadata(content: &str, timestamp: &str, reason: &str) -> String {
    let normalized = content.replace("\r\n", "\n");
    let Some((front, body)) = split_front_matter(&normalized) else {
        // No front matter — emit a fresh one. Rare path; primarily for tests
        // and tools that feed us body-only content.
        let yaml = format!(
            "name: \"unknown\"\nupdated_at: \"{timestamp}\"\nimprovement_reason: \"{}\"\n",
            yaml_escape(reason)
        );
        return format!("{FRONT_MATTER_DELIM}\n{yaml}{FRONT_MATTER_DELIM}\n{normalized}");
    };

    // Strip any existing top-level `updated_at:` / `improvement_reason:` lines.
    let stripped: Vec<&str> = front
        .lines()
        .filter(|line| {
            // Only strip TOP-LEVEL occurrences (no leading whitespace) — leaves
            // any nested `metadata.updated_at` etc. alone.
            if line.starts_with(' ') || line.starts_with('\t') {
                return true;
            }
            let trimmed = line.trim_start();
            !trimmed.starts_with("updated_at:") && !trimmed.starts_with("improvement_reason:")
        })
        .collect();

    let mut new_front = stripped.join("\n");
    if !new_front.ends_with('\n') {
        new_front.push('\n');
    }
    new_front.push_str(&format!(
        "updated_at: \"{timestamp}\"\nimprovement_reason: \"{}\"\n",
        yaml_escape(reason)
    ));

    format!("{FRONT_MATTER_DELIM}\n{new_front}{FRONT_MATTER_DELIM}\n{body}")
}

/// Escape a string for inclusion in a YAML double-quoted scalar.
fn yaml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push(' '),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Extract HTML audit-trail comments (`<!-- Improvement: ... -->`) appended
/// to the bottom of SKILL.md. These persist across patches so a reader can
/// see the history of why a skill changed.
fn extract_audit_trail(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("<!-- Improvement:") && trimmed.ends_with("-->")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Heuristic: does tool-result content look like a failure?
fn looks_like_failure(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("exception")
        || lower.contains("not found")
        || lower.starts_with("exit code")
}

pub fn extract_skill_executions_from_history(history: &[ChatMessage]) -> Vec<(String, bool)> {
    let mut results: Vec<(String, bool)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (i, msg) in history.iter().enumerate() {
        let content = &msg.content;

        // Format 1: XML <tool_result name="slug.tool">…</tool_result>.
        let open_marker = "<tool_result name=\"";
        let close_marker = "</tool_result>";
        let mut pos = 0;
        while pos < content.len() {
            let Some(start) = content[pos..].find(open_marker) else {
                break;
            };
            let abs = pos + start + open_marker.len();
            let Some(end) = content[abs..].find('"') else {
                break;
            };
            let name = &content[abs..abs + end];
            if let Some(dot_pos) = name.find('.') {
                let slug = name[..dot_pos].to_string();
                let after_tag = abs + end + 1;
                let body_start = content[after_tag..].find('>').map(|p| after_tag + p + 1);
                let body_end = content[after_tag..].find(close_marker);
                let body = match (body_start, body_end) {
                    (Some(s), Some(e)) if s <= after_tag + e => &content[s..after_tag + e],
                    _ => "",
                };
                let succeeded = !looks_like_failure(body);
                let key = (slug.clone(), succeeded);
                if seen.insert(key) {
                    results.push((slug, succeeded));
                }
            }
            pos = abs + end + 1;
        }

        // Format 2: native tool-role message preceded by an assistant message
        // whose JSON tool-call carries a dotted `"name": "slug.tool"`.
        if msg.role == "tool" && i > 0 {
            let prev = &history[i - 1];
            if prev.role == "assistant" {
                let prev_content = &prev.content;
                let name_marker = "\"name\"";
                let mut pos = 0;
                while pos < prev_content.len() {
                    let Some(start) = prev_content[pos..].find(name_marker) else {
                        break;
                    };
                    let after = pos + start + name_marker.len();
                    let rest = prev_content[after..].trim_start();
                    let Some(rest) = rest.strip_prefix(':') else {
                        pos = after + 1;
                        continue;
                    };
                    let rest = rest.trim_start();
                    let Some(rest) = rest.strip_prefix('"') else {
                        pos = after + 1;
                        continue;
                    };
                    let Some(end) = rest.find('"') else {
                        break;
                    };
                    let name = &rest[..end];
                    if let Some(dot_pos) = name.find('.') {
                        let slug = name[..dot_pos].to_string();
                        let succeeded = !looks_like_failure(content);
                        let key = (slug.clone(), succeeded);
                        if seen.insert(key) {
                            results.push((slug, succeeded));
                        }
                    }
                    let consumed = prev_content.len() - rest.len() + end + 1;
                    pos = consumed;
                }
            }
        }
    }

    results
}

/// Unique skill slugs seen in `history`, regardless of success/failure.
pub fn extract_skill_slugs_from_history(history: &[ChatMessage]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    extract_skill_executions_from_history(history)
        .into_iter()
        .filter_map(|(slug, _)| {
            if seen.insert(slug.clone()) {
                Some(slug)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, cooldown_secs: u64) -> SkillImprovementConfig {
        SkillImprovementConfig {
            enabled,
            cooldown_secs,
            ..Default::default()
        }
    }

    const VALID_SKILL: &str = "---\nname: test-skill\ndescription: A test skill\nversion: \"0.1.0\"\n---\n\n# Test skill\nDoes stuff.\n";

    // ── Validation ──────────────────────────────────────────

    #[test]
    fn validate_empty_content_rejected() {
        assert!(validate_skill_content("").is_err());
        assert!(validate_skill_content("   \n  ").is_err());
    }

    #[test]
    fn validate_no_front_matter_rejected() {
        assert!(validate_skill_content("# Just a heading, no YAML front-matter\n").is_err());
    }

    #[test]
    fn validate_missing_name_rejected() {
        let content = "---\ndescription: no name field\nversion: \"0.1.0\"\n---\n\n# Body\n";
        assert!(validate_skill_content(content).is_err());
    }

    #[test]
    fn validate_valid_content_accepted() {
        assert!(validate_skill_content(VALID_SKILL).is_ok());
    }

    #[test]
    fn validate_handles_quoted_name() {
        let content = "---\nname: \"quoted-name\"\n---\n\nBody\n";
        assert!(validate_skill_content(content).is_ok());
    }

    // ── Cooldown enforcement ────────────────────────────────

    #[test]
    fn cooldown_allows_first_improvement() {
        let improver = SkillImprover::new(PathBuf::from("/tmp/test"), cfg(true, 3600));
        assert!(improver.should_improve_skill("test-skill"));
    }

    #[test]
    fn cooldown_blocks_recent_improvement() {
        let mut improver = SkillImprover::new(PathBuf::from("/tmp/test"), cfg(true, 3600));
        improver
            .cooldowns
            .insert("test-skill".to_string(), Instant::now());
        assert!(!improver.should_improve_skill("test-skill"));
    }

    #[test]
    fn cooldown_disabled_blocks_all() {
        let improver = SkillImprover::new(PathBuf::from("/tmp/test"), cfg(false, 0));
        assert!(!improver.should_improve_skill("test-skill"));
    }

    // ── On-disk cooldown via `updated_at` ───────────────────

    #[tokio::test]
    async fn should_improve_blocks_when_updated_at_recent() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let recent = chrono::Utc::now().to_rfc3339();
        let md = format!("---\nname: test-skill\nupdated_at: \"{recent}\"\n---\n\nBody\n");
        tokio::fs::write(skill_dir.join("SKILL.md"), md)
            .await
            .unwrap();

        let improver = SkillImprover::new(dir.path().to_path_buf(), cfg(true, 9999));
        assert!(!improver.should_improve_skill("test-skill"));
    }

    #[tokio::test]
    async fn should_improve_allows_when_updated_at_stale() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let stale = (chrono::Utc::now() - chrono::Duration::seconds(10_000)).to_rfc3339();
        let md = format!("---\nname: test-skill\nupdated_at: \"{stale}\"\n---\n\nBody\n");
        tokio::fs::write(skill_dir.join("SKILL.md"), md)
            .await
            .unwrap();

        let improver = SkillImprover::new(dir.path().to_path_buf(), cfg(true, 3600));
        assert!(improver.should_improve_skill("test-skill"));
    }

    // ── Atomic write ────────────────────────────────────────

    #[tokio::test]
    async fn improve_skill_atomic_write_preserves_body() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();

        let original = "---\nname: test-skill\ndescription: Original description\nversion: \"0.1.0\"\n---\n\n# Test skill\nLine 1 of body.\nLine 2 of body.\n";
        tokio::fs::write(skill_dir.join("SKILL.md"), original)
            .await
            .unwrap();

        let mut improver = SkillImprover::new(dir.path().to_path_buf(), cfg(true, 0));

        let improved = "---\nname: test-skill\ndescription: Improved description with better steps\nversion: \"0.1.1\"\n---\n\n# Test skill\nLine 1 of body.\nLine 2 of body.\nLine 3 of body, added by improvement.\n";

        let result = improver
            .improve_skill("test-skill", improved, "Added better step descriptions")
            .await
            .unwrap();
        assert_eq!(result, Some("test-skill".to_string()));

        let content = tokio::fs::read_to_string(skill_dir.join("SKILL.md"))
            .await
            .unwrap();
        assert!(content.contains("Improved description"));
        assert!(content.contains("updated_at:"));
        assert!(content.contains("improvement_reason:"));
        assert!(content.contains("Line 3 of body"));
        assert!(content.contains("<!-- Improvement:"));

        // Temp file cleaned up.
        assert!(!skill_dir.join(".SKILL.md.tmp").exists());
    }

    #[tokio::test]
    async fn improve_skill_invalid_content_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();

        let mut improver = SkillImprover::new(dir.path().to_path_buf(), cfg(true, 0));

        // No front-matter → validation rejects.
        let result = improver
            .improve_skill("test-skill", "just markdown, no front matter\n", "bad")
            .await;
        assert!(result.is_err());

        // Original file untouched.
        let content = tokio::fs::read_to_string(skill_dir.join("SKILL.md"))
            .await
            .unwrap();
        assert_eq!(content, VALID_SKILL);
    }

    #[tokio::test]
    async fn improve_skill_writes_when_cooldown_not_checked_by_caller() {
        // `improve_skill` is caller-gated: it writes whenever given valid
        // content, even if `should_improve_skill` would return false.
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("test-skill");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();

        let mut improver = SkillImprover::new(dir.path().to_path_buf(), cfg(true, 9999));
        improver
            .cooldowns
            .insert("test-skill".to_string(), Instant::now());

        let result = improver
            .improve_skill(
                "test-skill",
                "---\nname: test-skill\ndescription: better\n---\n\nBody\n",
                "test",
            )
            .await
            .unwrap();
        assert_eq!(result, Some("test-skill".to_string()));
    }

    // ── Metadata appending ──────────────────────────────────

    #[test]
    fn append_metadata_adds_fields() {
        let result =
            append_improvement_metadata(VALID_SKILL, "2026-01-01T00:00:00Z", "Better steps");
        assert!(result.contains("updated_at: \"2026-01-01T00:00:00Z\""));
        assert!(result.contains("improvement_reason: \"Better steps\""));
    }

    #[test]
    fn append_metadata_preserves_body() {
        let content = "---\nname: test\nversion: \"0.1.0\"\n---\n\n# Heading\n\nBody paragraph with **bold** and `code`.\n";
        let result = append_improvement_metadata(content, "2026-01-01T00:00:00Z", "x");
        assert!(result.contains("# Heading"));
        assert!(result.contains("Body paragraph with **bold** and `code`."));
    }

    #[test]
    fn append_metadata_replaces_existing_fields() {
        // A previously-improved skill carries both keys. Appending again must
        // strip both before emitting new values so YAML stays valid.
        let already_improved = "---\nname: test\nupdated_at: \"2025-12-01T00:00:00Z\"\nimprovement_reason: \"first pass\"\n---\n\nBody\n";
        let result =
            append_improvement_metadata(already_improved, "2026-01-01T00:00:00Z", "second pass");
        let front = result.split("\n---\n").next().unwrap_or("");
        assert_eq!(front.matches("updated_at:").count(), 1);
        assert_eq!(front.matches("improvement_reason:").count(), 1);
        assert!(front.contains("2026-01-01T00:00:00Z"));
        assert!(front.contains("second pass"));
        assert!(!front.contains("first pass"));
    }

    #[test]
    fn append_metadata_does_not_touch_nested_keys() {
        // A `metadata:` sub-block with its own `updated_at` should be left
        // alone — we only strip top-level (zero-indent) occurrences.
        let content = "---\nname: test\nmetadata:\n  updated_at: \"nested\"\n  improvement_reason: \"nested-reason\"\n---\n\nBody\n";
        let result = append_improvement_metadata(content, "2026-01-01T00:00:00Z", "fresh");
        assert!(result.contains("  updated_at: \"nested\""));
        assert!(result.contains("  improvement_reason: \"nested-reason\""));
        assert!(result.contains("updated_at: \"2026-01-01T00:00:00Z\""));
    }

    // ── Audit trail extraction ──────────────────────────────

    #[test]
    fn extract_audit_trail_picks_html_comments() {
        let content = "---\nname: test\n---\n\n# Body\n\n<!-- Improvement: 2026-01-01T00:00:00Z | Reason: first -->\n<!-- Improvement: 2026-02-01T00:00:00Z | Reason: second -->\n";
        let trail = extract_audit_trail(content);
        assert!(trail.contains("first"));
        assert!(trail.contains("second"));
        assert_eq!(trail.lines().count(), 2);
    }

    #[test]
    fn extract_audit_trail_empty_when_none() {
        let trail = extract_audit_trail(VALID_SKILL);
        assert!(trail.is_empty());
    }

    // ── YAML escaping ───────────────────────────────────────

    #[test]
    fn yaml_escape_handles_quotes_and_backslashes() {
        assert_eq!(yaml_escape("plain"), "plain");
        assert_eq!(yaml_escape("he said \"hi\""), "he said \\\"hi\\\"");
        assert_eq!(yaml_escape("back\\slash"), "back\\\\slash");
        assert_eq!(yaml_escape("multi\nline"), "multi line");
    }

    // ── Failure heuristic ───────────────────────────────────

    #[test]
    fn looks_like_failure_detects_common_shapes() {
        assert!(looks_like_failure("Error: file not found"));
        assert!(looks_like_failure("Command failed with status 1"));
        assert!(looks_like_failure("thread 'main' panicked at ..."));
        assert!(looks_like_failure("Exception in user code"));
        assert!(looks_like_failure("not found"));
        assert!(looks_like_failure("exit code 137"));
    }

    #[test]
    fn looks_like_failure_passes_clean_output() {
        assert!(!looks_like_failure("Done. Wrote 12 lines."));
        assert!(!looks_like_failure("ok"));
        assert!(!looks_like_failure(""));
    }

    // ── History extraction ──────────────────────────────────

    #[test]
    fn extract_executions_xml_marks_failure() {
        let history = vec![
            ChatMessage::user("run my-skill"),
            ChatMessage::assistant(
                "<tool_result name=\"my-skill.run\">Error: command not found</tool_result>",
            ),
        ];
        let executions = extract_skill_executions_from_history(&history);
        assert_eq!(executions, vec![("my-skill".to_string(), false)]);
    }

    #[test]
    fn extract_executions_xml_marks_success() {
        let history = vec![
            ChatMessage::user("run my-skill"),
            ChatMessage::assistant(
                "<tool_result name=\"my-skill.run\">Done. Wrote 3 files.</tool_result>",
            ),
        ];
        let executions = extract_skill_executions_from_history(&history);
        assert_eq!(executions, vec![("my-skill".to_string(), true)]);
    }

    #[test]
    fn extract_executions_native_format() {
        let history = vec![
            ChatMessage::user("run it"),
            ChatMessage::assistant("{\"tool_calls\": [{\"name\": \"deploy.run\", \"args\": {}}]}"),
            ChatMessage {
                role: "tool".into(),
                content: "Error: connection refused".into(),
            },
        ];
        let executions = extract_skill_executions_from_history(&history);
        assert_eq!(executions, vec![("deploy".to_string(), false)]);
    }

    #[test]
    fn extract_slugs_dedupes() {
        let history = vec![
            ChatMessage::user("run my-skill"),
            ChatMessage::assistant(
                "<tool_result name=\"my-skill.run\">ok</tool_result>\
                 <tool_result name=\"my-skill.run\">Error</tool_result>",
            ),
        ];
        let slugs = extract_skill_slugs_from_history(&history);
        assert_eq!(slugs, vec!["my-skill".to_string()]);
    }
}
