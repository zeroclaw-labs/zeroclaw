// Autonomous skill creation from successful multi-step task executions.
//
// After the agent completes a multi-step tool-call sequence, this module
// can persist the execution as a reusable skill definition (SKILL.toml)
// under `~/.zeroclaw/workspace/skills/<slug>/`.

use crate::config::SkillCreationConfig;
use crate::memory::embeddings::EmbeddingProvider;
use crate::memory::vector::cosine_similarity;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// A record of a single tool call executed during a task.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub args: serde_json::Value,
}

/// Creates reusable skill definitions from successful multi-step executions.
pub struct SkillCreator {
    workspace_dir: PathBuf,
    config: SkillCreationConfig,
}

impl SkillCreator {
    pub fn new(workspace_dir: PathBuf, config: SkillCreationConfig) -> Self {
        Self {
            workspace_dir,
            config,
        }
    }

    /// Attempt to create a skill from a successful multi-step task execution.
    /// Returns `Ok(Some(slug))` if a skill was created, `Ok(None)` if skipped
    /// (disabled, duplicate, or insufficient tool calls).
    pub async fn create_from_execution(
        &self,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
        embedding_provider: Option<&dyn EmbeddingProvider>,
    ) -> Result<Option<String>> {
        if !self.config.enabled {
            return Ok(None);
        }

        if tool_calls.len() < 2 {
            return Ok(None);
        }

        // Deduplicate via embeddings when an embedding provider is available.
        if let Some(provider) = embedding_provider {
            if provider.name() != "none" && self.is_duplicate(task_description, provider).await? {
                return Ok(None);
            }
        }

        let slug = Self::generate_slug(task_description);
        if !Self::validate_slug(&slug) {
            return Ok(None);
        }

        // Enforce LRU limit before writing a new skill.
        self.enforce_lru_limit().await?;

        let skill_dir = self.skills_dir().join(&slug);
        tokio::fs::create_dir_all(&skill_dir)
            .await
            .with_context(|| {
                format!("Failed to create skill directory: {}", skill_dir.display())
            })?;

        let toml_content = Self::generate_skill_toml(&slug, task_description, tool_calls);
        let toml_path = skill_dir.join("SKILL.toml");
        tokio::fs::write(&toml_path, toml_content.as_bytes())
            .await
            .with_context(|| format!("Failed to write {}", toml_path.display()))?;

        Ok(Some(slug))
    }

    /// Generate a URL-safe slug from a task description.
    /// Alphanumeric and hyphens only, max 64 characters.
    fn generate_slug(description: &str) -> String {
        let slug: String = description
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();

        // Collapse consecutive hyphens.
        let mut collapsed = String::with_capacity(slug.len());
        let mut prev_hyphen = false;
        for c in slug.chars() {
            if c == '-' {
                if !prev_hyphen {
                    collapsed.push('-');
                }
                prev_hyphen = true;
            } else {
                collapsed.push(c);
                prev_hyphen = false;
            }
        }

        // Trim leading/trailing hyphens, then truncate.
        let trimmed = collapsed.trim_matches('-');
        if trimmed.len() > 64 {
            // Truncate at a hyphen boundary if possible.
            let truncated = &trimmed[..64];
            truncated.trim_end_matches('-').to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Validate that a slug is non-empty, alphanumeric + hyphens, max 64 chars.
    fn validate_slug(slug: &str) -> bool {
        !slug.is_empty()
            && slug.len() <= 64
            && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !slug.starts_with('-')
            && !slug.ends_with('-')
    }

    /// Generate SKILL.toml content from task execution data.
    fn generate_skill_toml(slug: &str, description: &str, tool_calls: &[ToolCallRecord]) -> String {
        use std::fmt::Write;
        let mut toml = String::new();
        toml.push_str("[skill]\n");
        let _ = writeln!(toml, "name = {}", toml_escape(slug));
        let _ = writeln!(
            toml,
            "description = {}",
            toml_escape(&format!("Auto-generated: {description}"))
        );
        toml.push_str("version = \"0.1.0\"\n");
        toml.push_str("author = \"zeroclaw-auto\"\n");
        toml.push_str("tags = [\"auto-generated\"]\n");

        for call in tool_calls {
            toml.push('\n');
            toml.push_str("[[tools]]\n");
            let _ = writeln!(toml, "name = {}", toml_escape(&call.name));
            let _ = writeln!(
                toml,
                "description = {}",
                toml_escape(&format!("Tool used in task: {}", call.name))
            );
            toml.push_str("kind = \"shell\"\n");

            // Extract the command from args if available, otherwise use the tool name.
            let command = call
                .args
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&call.name);
            let _ = writeln!(toml, "command = {}", toml_escape(command));
        }

        toml
    }

    /// Check if a skill with a similar description already exists.
    async fn is_duplicate(
        &self,
        description: &str,
        embedding_provider: &dyn EmbeddingProvider,
    ) -> Result<bool> {
        let new_embedding = embedding_provider.embed_one(description).await?;
        if new_embedding.is_empty() {
            return Ok(false);
        }

        let skills_dir = self.skills_dir();
        if !skills_dir.exists() {
            return Ok(false);
        }

        let mut entries = tokio::fs::read_dir(&skills_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let toml_path = entry.path().join("SKILL.toml");
            if !toml_path.exists() {
                continue;
            }

            let content = tokio::fs::read_to_string(&toml_path).await?;
            // Extract description from the TOML to compare.
            if let Some(desc) = extract_description_from_toml(&content) {
                let existing_embedding = embedding_provider.embed_one(&desc).await?;
                if !existing_embedding.is_empty() {
                    #[allow(clippy::cast_possible_truncation)]
                    let similarity =
                        f64::from(cosine_similarity(&new_embedding, &existing_embedding));
                    if similarity > self.config.similarity_threshold {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    /// Remove the oldest auto-generated skill when we exceed `max_skills`.
    async fn enforce_lru_limit(&self) -> Result<()> {
        let skills_dir = self.skills_dir();
        if !skills_dir.exists() {
            return Ok(());
        }

        let mut auto_skills: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

        let mut entries = tokio::fs::read_dir(&skills_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let toml_path = entry.path().join("SKILL.toml");
            if !toml_path.exists() {
                continue;
            }

            let content = tokio::fs::read_to_string(&toml_path).await?;
            if content.contains("\"zeroclaw-auto\"") || content.contains("\"auto-generated\"") {
                let modified = tokio::fs::metadata(&toml_path)
                    .await?
                    .modified()
                    .unwrap_or(std::time::UNIX_EPOCH);
                auto_skills.push((entry.path(), modified));
            }
        }

        // If at or above the limit, remove the oldest.
        if auto_skills.len() >= self.config.max_skills {
            auto_skills.sort_by_key(|(_, modified)| *modified);
            if let Some((oldest_dir, _)) = auto_skills.first() {
                tokio::fs::remove_dir_all(oldest_dir)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to remove oldest auto-generated skill: {}",
                            oldest_dir.display()
                        )
                    })?;
            }
        }

        Ok(())
    }

    fn skills_dir(&self) -> PathBuf {
        self.workspace_dir.join("skills")
    }
}

/// Escape a string for TOML value (double-quoted).
fn toml_escape(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// Extract the description field from a SKILL.toml string.
fn extract_description_from_toml(content: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Partial {
        skill: PartialSkill,
    }
    #[derive(serde::Deserialize)]
    struct PartialSkill {
        description: Option<String>,
    }
    toml::from_str::<Partial>(content)
        .ok()
        .and_then(|p| p.skill.description)
}

/// Extract `ToolCallRecord`s from the agent conversation history.
///
/// Scans assistant messages for tool call patterns (both JSON and XML formats)
/// and returns records for each unique tool invocation.
pub fn extract_tool_calls_from_history(
    history: &[crate::providers::ChatMessage],
) -> Vec<ToolCallRecord> {
    let mut records = Vec::new();

    for msg in history {
        if msg.role != "assistant" {
            continue;
        }

        // Try parsing as JSON (native tool_calls format).
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
            if let Some(tool_calls) = value.get("tool_calls").and_then(|v| v.as_array()) {
                for call in tool_calls {
                    if let Some(function) = call.get("function") {
                        let name = function
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let args_str = function
                            .get("arguments")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("{}");
                        let args = serde_json::from_str(args_str).unwrap_or_default();
                        if !name.is_empty() {
                            records.push(ToolCallRecord { name, args });
                        }
                    }
                }
            }
        }

        // Also try XML tool call format: <tool_name>...</tool_name>
        // Simple extraction for `<shell>{"command":"..."}</shell>` style tags.
        let content = &msg.content;
        let mut pos = 0;
        while pos < content.len() {
            if let Some(start) = content[pos..].find('<') {
                let abs_start = pos + start;
                if let Some(end) = content[abs_start..].find('>') {
                    let tag = &content[abs_start + 1..abs_start + end];
                    // Skip closing tags and meta tags.
                    if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
                        pos = abs_start + end + 1;
                        continue;
                    }
                    let tag_name = tag.split_whitespace().next().unwrap_or(tag);
                    let close_tag = format!("</{tag_name}>");
                    if let Some(close_pos) = content[abs_start + end + 1..].find(&close_tag) {
                        let inner = &content[abs_start + end + 1..abs_start + end + 1 + close_pos];
                        let args: serde_json::Value =
                            serde_json::from_str(inner.trim()).unwrap_or_default();
                        // Only add if it looks like a tool call (not HTML/formatting tags).
                        if tag_name != "tool_result"
                            && tag_name != "tool_results"
                            && !tag_name.contains(':')
                            && args.is_object()
                            && !args.as_object().map_or(true, |o| o.is_empty())
                        {
                            records.push(ToolCallRecord {
                                name: tag_name.to_string(),
                                args,
                            });
                        }
                        pos = abs_start + end + 1 + close_pos + close_tag.len();
                    } else {
                        pos = abs_start + end + 1;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }

    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embeddings::{EmbeddingProvider, NoopEmbedding};
    use async_trait::async_trait;

    // ── Slug generation ──────────────────────────────────────────

    #[test]
    fn slug_basic() {
        assert_eq!(
            SkillCreator::generate_slug("Deploy to production"),
            "deploy-to-production"
        );
    }

    #[test]
    fn slug_special_characters() {
        assert_eq!(
            SkillCreator::generate_slug("Build & test (CI/CD) pipeline!"),
            "build-test-ci-cd-pipeline"
        );
    }

    #[test]
    fn slug_max_length() {
        let long_desc = "a".repeat(100);
        let slug = SkillCreator::generate_slug(&long_desc);
        assert!(slug.len() <= 64);
    }

    #[test]
    fn slug_leading_trailing_hyphens() {
        let slug = SkillCreator::generate_slug("---hello world---");
        assert!(!slug.starts_with('-'));
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn slug_consecutive_spaces() {
        assert_eq!(SkillCreator::generate_slug("hello    world"), "hello-world");
    }

    #[test]
    fn slug_empty_input() {
        let slug = SkillCreator::generate_slug("");
        assert!(slug.is_empty());
    }

    #[test]
    fn slug_only_symbols() {
        let slug = SkillCreator::generate_slug("!@#$%^&*()");
        assert!(slug.is_empty());
    }

    #[test]
    fn slug_unicode() {
        let slug = SkillCreator::generate_slug("Deploy cafe app");
        assert_eq!(slug, "deploy-cafe-app");
    }

    // ── Slug validation ──────────────────────────────────────────

    #[test]
    fn validate_slug_valid() {
        assert!(SkillCreator::validate_slug("deploy-to-production"));
        assert!(SkillCreator::validate_slug("a"));
        assert!(SkillCreator::validate_slug("abc123"));
    }

    #[test]
    fn validate_slug_invalid() {
        assert!(!SkillCreator::validate_slug(""));
        assert!(!SkillCreator::validate_slug("-starts-with-hyphen"));
        assert!(!SkillCreator::validate_slug("ends-with-hyphen-"));
        assert!(!SkillCreator::validate_slug("has spaces"));
        assert!(!SkillCreator::validate_slug("has_underscores"));
        assert!(!SkillCreator::validate_slug(&"a".repeat(65)));
    }

    // ── TOML generation ──────────────────────────────────────────

    #[test]
    fn toml_generation_valid_format() {
        let calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo build"}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo test"}),
            },
        ];
        let toml_str = SkillCreator::generate_skill_toml(
            "build-and-test",
            "Build and test the project",
            &calls,
        );

        // Should parse as valid TOML.
        let parsed: toml::Value =
            toml::from_str(&toml_str).expect("Generated TOML should be valid");
        let skill = parsed.get("skill").expect("Should have [skill] section");
        assert_eq!(
            skill.get("name").and_then(toml::Value::as_str),
            Some("build-and-test")
        );
        assert_eq!(
            skill.get("author").and_then(toml::Value::as_str),
            Some("zeroclaw-auto")
        );
        assert_eq!(
            skill.get("version").and_then(toml::Value::as_str),
            Some("0.1.0")
        );

        let tools = parsed.get("tools").and_then(toml::Value::as_array).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[0].get("command").and_then(toml::Value::as_str),
            Some("cargo build")
        );
    }

    #[test]
    fn toml_generation_escapes_quotes() {
        let calls = vec![ToolCallRecord {
            name: "shell".into(),
            args: serde_json::json!({"command": "echo \"hello\""}),
        }];
        let toml_str =
            SkillCreator::generate_skill_toml("echo-test", "Test \"quoted\" description", &calls);
        let parsed: toml::Value =
            toml::from_str(&toml_str).expect("TOML with quotes should be valid");
        let desc = parsed
            .get("skill")
            .and_then(|s| s.get("description"))
            .and_then(toml::Value::as_str)
            .unwrap();
        assert!(desc.contains("quoted"));
    }

    #[test]
    fn toml_generation_no_command_arg() {
        let calls = vec![ToolCallRecord {
            name: "memory_store".into(),
            args: serde_json::json!({"key": "foo", "value": "bar"}),
        }];
        let toml_str = SkillCreator::generate_skill_toml("memory-op", "Store to memory", &calls);
        let parsed: toml::Value = toml::from_str(&toml_str).expect("TOML should be valid");
        let tools = parsed.get("tools").and_then(toml::Value::as_array).unwrap();
        // When no "command" arg exists, falls back to tool name.
        assert_eq!(
            tools[0].get("command").and_then(toml::Value::as_str),
            Some("memory_store")
        );
    }

    // ── TOML description extraction ──────────────────────────────

    #[test]
    fn extract_description_from_valid_toml() {
        let content = r#"
[skill]
name = "test"
description = "Auto-generated: Build project"
version = "0.1.0"
"#;
        assert_eq!(
            extract_description_from_toml(content),
            Some("Auto-generated: Build project".into())
        );
    }

    #[test]
    fn extract_description_from_invalid_toml() {
        assert_eq!(extract_description_from_toml("not valid toml {{"), None);
    }

    // ── Deduplication ────────────────────────────────────────────

    /// A mock embedding provider that returns deterministic embeddings.
    ///
    /// The "new" description (first text embedded) always gets `[1, 0, 0]`.
    /// The "existing" skill description (second text embedded) gets a vector
    /// whose cosine similarity with `[1, 0, 0]` equals `self.similarity`.
    struct MockEmbeddingProvider {
        similarity: f32,
        call_count: std::sync::atomic::AtomicUsize,
    }

    impl MockEmbeddingProvider {
        fn new(similarity: f32) -> Self {
            Self {
                similarity,
                call_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        fn name(&self) -> &str {
            "mock"
        }
        fn dimensions(&self) -> usize {
            3
        }
        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|_| {
                    let call = self
                        .call_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if call == 0 {
                        // First call: the "new" description.
                        vec![1.0, 0.0, 0.0]
                    } else {
                        // Subsequent calls: existing skill descriptions.
                        // Produce a vector with the configured cosine similarity to [1,0,0].
                        vec![
                            self.similarity,
                            (1.0 - self.similarity * self.similarity).sqrt(),
                            0.0,
                        ]
                    }
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn dedup_skips_similar_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills").join("existing-skill");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        tokio::fs::write(
            skills_dir.join("SKILL.toml"),
            r#"
[skill]
name = "existing-skill"
description = "Auto-generated: Build the project"
version = "0.1.0"
author = "zeroclaw-auto"
tags = ["auto-generated"]
"#,
        )
        .await
        .unwrap();

        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };

        // High similarity provider -> should detect as duplicate.
        let provider = MockEmbeddingProvider::new(0.95);
        let creator = SkillCreator::new(dir.path().to_path_buf(), config.clone());
        assert!(creator
            .is_duplicate("Build the project", &provider)
            .await
            .unwrap());

        // Low similarity provider -> not a duplicate.
        let provider_low = MockEmbeddingProvider::new(0.3);
        let creator2 = SkillCreator::new(dir.path().to_path_buf(), config);
        assert!(!creator2
            .is_duplicate("Completely different task", &provider_low)
            .await
            .unwrap());
    }

    // ── LRU eviction ─────────────────────────────────────────────

    #[tokio::test]
    async fn lru_eviction_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 2,
            similarity_threshold: 0.85,
        };

        let skills_dir = dir.path().join("skills");

        // Create two auto-generated skills with different timestamps.
        for (i, name) in ["old-skill", "new-skill"].iter().enumerate() {
            let skill_dir = skills_dir.join(name);
            tokio::fs::create_dir_all(&skill_dir).await.unwrap();
            tokio::fs::write(
                skill_dir.join("SKILL.toml"),
                format!(
                    r#"[skill]
name = "{name}"
description = "Auto-generated: Skill {i}"
version = "0.1.0"
author = "zeroclaw-auto"
tags = ["auto-generated"]
"#
                ),
            )
            .await
            .unwrap();
            // Small delay to ensure different timestamps.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let creator = SkillCreator::new(dir.path().to_path_buf(), config);
        creator.enforce_lru_limit().await.unwrap();

        // The oldest skill should have been removed.
        assert!(!skills_dir.join("old-skill").exists());
        assert!(skills_dir.join("new-skill").exists());
    }

    // ── End-to-end: create_from_execution ────────────────────────

    #[tokio::test]
    async fn create_from_execution_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: false,
            ..Default::default()
        };
        let creator = SkillCreator::new(dir.path().to_path_buf(), config);
        let calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "ls"}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "pwd"}),
            },
        ];
        let result = creator
            .create_from_execution("List files", &calls, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn create_from_execution_insufficient_steps() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            ..Default::default()
        };
        let creator = SkillCreator::new(dir.path().to_path_buf(), config);
        let calls = vec![ToolCallRecord {
            name: "shell".into(),
            args: serde_json::json!({"command": "ls"}),
        }];
        let result = creator
            .create_from_execution("List files", &calls, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn create_from_execution_success() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };
        let creator = SkillCreator::new(dir.path().to_path_buf(), config);
        let calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo build"}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo test"}),
            },
        ];

        // Use noop embedding (no deduplication).
        let noop = NoopEmbedding;
        let result = creator
            .create_from_execution("Build and test", &calls, Some(&noop))
            .await
            .unwrap();
        assert_eq!(result, Some("build-and-test".into()));

        // Verify the skill directory and TOML were created.
        let skill_dir = dir.path().join("skills").join("build-and-test");
        assert!(skill_dir.exists());
        let toml_content = tokio::fs::read_to_string(skill_dir.join("SKILL.toml"))
            .await
            .unwrap();
        assert!(toml_content.contains("build-and-test"));
        assert!(toml_content.contains("zeroclaw-auto"));
    }

    #[tokio::test]
    async fn create_from_execution_with_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };

        // First, create an existing skill.
        let skills_dir = dir.path().join("skills").join("existing");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        tokio::fs::write(
            skills_dir.join("SKILL.toml"),
            r#"[skill]
name = "existing"
description = "Auto-generated: Build and test"
version = "0.1.0"
author = "zeroclaw-auto"
tags = ["auto-generated"]
"#,
        )
        .await
        .unwrap();

        // High similarity provider -> should skip.
        let provider = MockEmbeddingProvider::new(0.95);
        let creator = SkillCreator::new(dir.path().to_path_buf(), config);
        let calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo build"}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo test"}),
            },
        ];
        let result = creator
            .create_from_execution("Build and test", &calls, Some(&provider))
            .await
            .unwrap();
        assert!(result.is_none());
    }

    // ── Tool call extraction from history ────────────────────────

    #[test]
    fn extract_from_empty_history() {
        let history = vec![];
        let records = extract_tool_calls_from_history(&history);
        assert!(records.is_empty());
    }

    #[test]
    fn extract_from_user_messages_only() {
        use crate::providers::ChatMessage;
        let history = vec![ChatMessage::user("hello"), ChatMessage::user("world")];
        let records = extract_tool_calls_from_history(&history);
        assert!(records.is_empty());
    }

    // ── Fuzz-like tests for slug ─────────────────────────────────

    #[test]
    fn slug_fuzz_various_inputs() {
        let inputs = [
            "",
            " ",
            "---",
            "a",
            "hello world!",
            "UPPER CASE",
            "with-hyphens-already",
            "with__underscores",
            "123 numbers 456",
            "emoji: cafe",
            &"x".repeat(200),
            "a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q-r-s-t-u-v-w-x-y-z-0-1-2-3-4-5",
        ];

        for input in &inputs {
            let slug = SkillCreator::generate_slug(input);
            // Slug should always pass validation (or be empty for degenerate input).
            if !slug.is_empty() {
                assert!(
                    SkillCreator::validate_slug(&slug),
                    "Generated slug '{slug}' from '{input}' failed validation"
                );
            }
        }
    }

    // ── Fuzz-like tests for TOML generation ──────────────────────

    #[test]
    fn toml_fuzz_various_inputs() {
        let descriptions = [
            "simple task",
            "task with \"quotes\" and \\ backslashes",
            "task with\nnewlines\r\nand tabs\there",
            "",
            &"long ".repeat(100),
        ];

        let args_variants = [
            serde_json::json!({}),
            serde_json::json!({"command": "echo hello"}),
            serde_json::json!({"command": "echo \"hello world\"", "extra": 42}),
        ];

        for desc in &descriptions {
            for args in &args_variants {
                let calls = vec![
                    ToolCallRecord {
                        name: "tool1".into(),
                        args: args.clone(),
                    },
                    ToolCallRecord {
                        name: "tool2".into(),
                        args: args.clone(),
                    },
                ];
                let toml_str = SkillCreator::generate_skill_toml("test-slug", desc, &calls);
                // Must always produce valid TOML.
                let _parsed: toml::Value = toml::from_str(&toml_str)
                    .unwrap_or_else(|e| panic!("Invalid TOML for desc '{desc}': {e}\n{toml_str}"));
            }
        }
    }
}
