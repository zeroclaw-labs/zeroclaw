// Autonomous skill creation from successful multi-step task executions.
//
// After the agent completes a multi-step tool-call sequence, this module
// can persist the execution as a reusable skill definition (SKILL.toml)
// under `~/.zeroclaw/workspace/skills/<slug>/`.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_config::schema::{SkillCreationConfig, SkillImprovementConfig};
use zeroclaw_memory::embeddings::EmbeddingProvider;
use zeroclaw_memory::vector::cosine_similarity;

/// A record of a single tool call executed during a task.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub args: serde_json::Value,
}

/// Creates reusable skill definitions from successful multi-step executions.
/// When a similar skill already exists, delegates to [`super::improver::SkillImprover`]
/// for incremental improvement instead of creating a duplicate.
pub struct SkillCreator {
    workspace_dir: PathBuf,
    config: SkillCreationConfig,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    improver: Option<tokio::sync::Mutex<super::improver::SkillImprover>>,
}

impl SkillCreator {
    pub fn new(workspace_dir: PathBuf, config: SkillCreationConfig) -> Self {
        Self {
            workspace_dir,
            config,
            embedding_provider: None,
            improver: None,
        }
    }

    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    pub fn with_improver(mut self, config: SkillImprovementConfig) -> Self {
        self.improver = Some(tokio::sync::Mutex::new(
            super::improver::SkillImprover::new(self.workspace_dir.clone(), config),
        ));
        self
    }

    /// Attempt to create a skill from a successful multi-step task execution.
    /// Returns `Ok(Some(slug))` if a skill was created or improved,
    /// `Ok(None)` if skipped (disabled, cooldown active, or insufficient tool calls).
    ///
    /// When a similar skill already exists (embedding-based dedup), the method
    /// delegates to [`super::improver::SkillImprover`] for incremental improvement
    /// rather than silently skipping.
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

        let effective_provider: Option<&dyn EmbeddingProvider> =
            embedding_provider.or_else(|| self.embedding_provider.as_deref());

        if let Some(provider) = effective_provider
            && provider.name() != "none"
        {
            if let Some(existing_slug) = self.find_similar_skill(task_description, provider).await?
            {
                return self
                    .try_improve_existing(&existing_slug, task_description, tool_calls)
                    .await;
            }
        }

        self.create_new_skill(task_description, tool_calls).await
    }

    /// Create a brand-new skill (no similar skill exists).
    async fn create_new_skill(
        &self,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
    ) -> Result<Option<String>> {
        let slug = Self::generate_slug(task_description);
        if !Self::validate_slug(&slug) {
            return Ok(None);
        }

        self.enforce_lru_limit().await?;

        let skill_dir = self.skills_dir().join(&slug);
        tokio::fs::create_dir_all(&skill_dir)
            .await
            .with_context(|| {
                format!("Failed to create skill directory: {}", skill_dir.display())
            })?;

        let toml_content = Self::generate_skill_toml(&slug, task_description, tool_calls);
        if let Err(reason) = validate_generated_content(&toml_content) {
            tracing::warn!(
                slug,
                reason,
                "Generated skill content rejected by write guard"
            );
            let _ = tokio::fs::remove_dir_all(&skill_dir).await;
            return Ok(None);
        }
        let toml_path = skill_dir.join("SKILL.toml");
        tokio::fs::write(&toml_path, toml_content.as_bytes())
            .await
            .with_context(|| format!("Failed to write {}", toml_path.display()))?;

        match super::audit::audit_skill_directory(&skill_dir) {
            Ok(report) if !report.is_clean() => {
                tracing::warn!(
                    slug,
                    findings = %report.summary(),
                    "Auto-created skill failed security audit — removing"
                );
                let _ = tokio::fs::remove_dir_all(&skill_dir).await;
                return Ok(None);
            }
            Err(e) => {
                tracing::warn!(slug, error = %e, "Skill audit error — removing as precaution");
                let _ = tokio::fs::remove_dir_all(&skill_dir).await;
                return Ok(None);
            }
            Ok(_) => {}
        }

        tracing::info!(slug, "Created new skill");
        Ok(Some(slug))
    }

    /// Attempt to improve an existing skill with new tool calls.
    /// Falls back to `Ok(None)` if the improver is not configured or cooldown is active.
    async fn try_improve_existing(
        &self,
        existing_slug: &str,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
    ) -> Result<Option<String>> {
        let Some(ref improver_mutex) = self.improver else {
            tracing::debug!(
                slug = existing_slug,
                "Similar skill found but no improver configured — skipping"
            );
            return Ok(None);
        };

        let improved_toml = Self::generate_skill_toml(existing_slug, task_description, tool_calls);
        if let Err(reason) = validate_generated_content(&improved_toml) {
            tracing::warn!(
                slug = existing_slug,
                reason,
                "Improved skill content rejected by write guard"
            );
            return Ok(None);
        }
        let reason = format!(
            "Re-observed execution with {} tool calls for: {}",
            tool_calls.len(),
            task_description
        );

        let mut improver = improver_mutex.lock().await;
        match improver
            .improve_skill(existing_slug, &improved_toml, &reason)
            .await
        {
            Ok(Some(slug)) => {
                tracing::info!(slug, "Improved existing skill");
                Ok(Some(slug))
            }
            Ok(None) => {
                tracing::debug!(
                    slug = existing_slug,
                    "Skill improvement skipped (cooldown or disabled)"
                );
                Ok(None)
            }
            Err(e) => {
                tracing::warn!(
                    slug = existing_slug,
                    error = %e,
                    "Skill improvement failed — leaving existing skill untouched"
                );
                Ok(None)
            }
        }
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
            // Find the nearest valid character boundary at or before 64 bytes.
            let safe_index = trimmed
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= 64)
                .last()
                .unwrap_or(0);
            let truncated = &trimmed[..safe_index];
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

    /// Find the slug of the most similar existing skill, if any exceeds the
    /// configured similarity threshold. Returns `None` when no match is found.
    async fn find_similar_skill(
        &self,
        description: &str,
        embedding_provider: &dyn EmbeddingProvider,
    ) -> Result<Option<String>> {
        let new_embedding = embedding_provider.embed_one(description).await?;
        if new_embedding.is_empty() {
            return Ok(None);
        }

        let skills_dir = self.skills_dir();
        if !skills_dir.exists() {
            return Ok(None);
        }

        let mut best_slug: Option<String> = None;
        let mut best_similarity: f64 = 0.0;

        let mut entries = tokio::fs::read_dir(&skills_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let toml_path = entry.path().join("SKILL.toml");
            if !toml_path.exists() {
                continue;
            }

            let slug = entry.file_name().to_str().unwrap_or_default().to_string();

            let content = tokio::fs::read_to_string(&toml_path).await?;
            if let Some(desc) = extract_description_from_toml(&content) {
                let existing_embedding = embedding_provider.embed_one(&desc).await?;
                if !existing_embedding.is_empty() {
                    #[allow(clippy::cast_possible_truncation)]
                    let similarity =
                        f64::from(cosine_similarity(&new_embedding, &existing_embedding));
                    if similarity > self.config.similarity_threshold && similarity > best_similarity
                    {
                        best_similarity = similarity;
                        best_slug = Some(slug);
                    }
                }
            }
        }

        Ok(best_slug)
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

/// Maximum allowed size for generated skill TOML content (64 KiB).
const MAX_SKILL_CONTENT_BYTES: usize = 64 * 1024;

/// Maximum number of tool entries in a single skill.
const MAX_TOOL_ENTRIES: usize = 50;

/// Validate generated skill content before writing to disk.
/// Returns `Ok(())` if the content passes all checks, or `Err(reason)` if rejected.
fn validate_generated_content(content: &str) -> std::result::Result<(), String> {
    if content.len() > MAX_SKILL_CONTENT_BYTES {
        return Err(format!(
            "content exceeds size limit ({} > {})",
            content.len(),
            MAX_SKILL_CONTENT_BYTES
        ));
    }

    let tool_count = content.matches("[[tools]]").count();
    if tool_count > MAX_TOOL_ENTRIES {
        return Err(format!(
            "too many tool entries ({tool_count} > {MAX_TOOL_ENTRIES})"
        ));
    }

    if content.contains('\0') {
        return Err("content contains null bytes".into());
    }

    Ok(())
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
    history: &[zeroclaw_providers::ChatMessage],
) -> Vec<ToolCallRecord> {
    let mut records = Vec::new();

    for msg in history {
        if msg.role != "assistant" {
            continue;
        }

        // Try parsing as JSON (native tool_calls format).
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content)
            && let Some(tool_calls) = value.get("tool_calls").and_then(|v| v.as_array())
        {
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
                            && !args.as_object().is_none_or(|o| o.is_empty())
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
    use async_trait::async_trait;
    use zeroclaw_memory::embeddings::{EmbeddingProvider, NoopEmbedding};

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

        // High similarity provider -> should find similar skill.
        let provider = MockEmbeddingProvider::new(0.95);
        let creator = SkillCreator::new(dir.path().to_path_buf(), config.clone());
        let found = creator
            .find_similar_skill("Build the project", &provider)
            .await
            .unwrap();
        assert!(found.is_some(), "Expected to find a similar skill");
        assert_eq!(found.unwrap(), "existing-skill");

        // Low similarity provider -> no match.
        let provider_low = MockEmbeddingProvider::new(0.3);
        let creator2 = SkillCreator::new(dir.path().to_path_buf(), config);
        let found = creator2
            .find_similar_skill("Completely different task", &provider_low)
            .await
            .unwrap();
        assert!(found.is_none(), "Expected no similar skill");
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
        use zeroclaw_providers::ChatMessage;
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

    // ── C5: Post-creation audit tests ────────────────────────────

    #[tokio::test]
    async fn create_from_execution_runs_audit_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            ..Default::default()
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

        let result = creator
            .create_from_execution("Build and test project", &calls, None)
            .await
            .unwrap();

        // Should succeed since the generated SKILL.toml is clean.
        assert!(result.is_some());
        let slug = result.unwrap();

        // Verify the skill directory and SKILL.toml exist.
        let skill_dir = dir.path().join("skills").join(&slug);
        assert!(skill_dir.join("SKILL.toml").exists());
    }

    #[tokio::test]
    async fn create_from_execution_skips_when_disabled() {
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
    async fn create_from_execution_skips_with_too_few_tool_calls() {
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
            .create_from_execution("Single command", &calls, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    // ── C1: extract_tool_calls_from_history tests ────────────────

    fn make_msg(role: &str, content: &str) -> zeroclaw_providers::ChatMessage {
        zeroclaw_providers::ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn extract_tool_calls_json_format() {
        let history = vec![
            make_msg("user", "Build my project"),
            make_msg(
                "assistant",
                r#"{"tool_calls":[{"id":"1","function":{"name":"shell","arguments":"{\"command\":\"cargo build\"}"}}]}"#,
            ),
            make_msg("tool", r#"{"tool_call_id":"1","content":"ok"}"#),
        ];

        let calls = extract_tool_calls_from_history(&history);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args["command"], "cargo build");
    }

    #[test]
    fn extract_tool_calls_xml_format() {
        let history = vec![
            make_msg("user", "Check files"),
            make_msg("assistant", r#"<shell>{"command": "ls -la"}</shell>"#),
        ];

        let calls = extract_tool_calls_from_history(&history);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].args["command"], "ls -la");
    }

    #[test]
    fn extract_tool_calls_skips_user_messages() {
        let history = vec![make_msg("user", r#"<shell>{"command": "ls"}</shell>"#)];

        let calls = extract_tool_calls_from_history(&history);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_tool_calls_multiple_calls_in_one_message() {
        let history = vec![make_msg(
            "assistant",
            r#"{"tool_calls":[
                {"id":"1","function":{"name":"shell","arguments":"{\"command\":\"cargo build\"}"}},
                {"id":"2","function":{"name":"shell","arguments":"{\"command\":\"cargo test\"}"}}
            ]}"#,
        )];

        let calls = extract_tool_calls_from_history(&history);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args["command"], "cargo build");
        assert_eq!(calls[1].args["command"], "cargo test");
    }

    #[test]
    fn extract_tool_calls_empty_history() {
        let calls = extract_tool_calls_from_history(&[]);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_tool_calls_no_tool_calls_in_assistant_message() {
        let history = vec![make_msg("assistant", "I'll help you with that.")];
        let calls = extract_tool_calls_from_history(&history);
        assert!(calls.is_empty());
    }

    // ── Improve-or-create flow ──────────────────────────────────

    fn sample_tool_calls() -> Vec<ToolCallRecord> {
        vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo build"}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo test"}),
            },
        ]
    }

    /// Helper to set up a skill directory with a valid SKILL.toml for testing.
    async fn setup_existing_skill(dir: &std::path::Path, slug: &str) {
        let skill_dir = dir.join("skills").join(slug);
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let content = format!(
            "[skill]\nname = \"{slug}\"\ndescription = \"Auto-generated: Build the project\"\nversion = \"0.1.0\"\nauthor = \"zeroclaw-auto\"\ntags = [\"auto-generated\"]\n"
        );
        tokio::fs::write(skill_dir.join("SKILL.toml"), content)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_from_execution_improves_when_similar_found() {
        let dir = tempfile::tempdir().unwrap();
        setup_existing_skill(dir.path(), "build-project").await;

        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };
        let improve_config = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 0,
        };

        let provider = Arc::new(MockEmbeddingProvider::new(0.95));
        let creator = SkillCreator::new(dir.path().to_path_buf(), config)
            .with_embedding_provider(provider)
            .with_improver(improve_config);

        let result = creator
            .create_from_execution("Build the project", &sample_tool_calls(), None)
            .await
            .unwrap();

        assert_eq!(result, Some("build-project".to_string()));

        // Verify the file was updated with improvement metadata.
        let content = tokio::fs::read_to_string(dir.path().join("skills/build-project/SKILL.toml"))
            .await
            .unwrap();
        assert!(content.contains("updated_at"), "Should contain updated_at");
        assert!(
            content.contains("improvement_reason"),
            "Should contain improvement_reason"
        );
        assert!(
            content.contains("Re-observed execution"),
            "Should contain reason text"
        );
    }

    #[tokio::test]
    async fn create_from_execution_creates_new_when_no_similar() {
        let dir = tempfile::tempdir().unwrap();

        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };
        let improve_config = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 0,
        };

        let provider = Arc::new(MockEmbeddingProvider::new(0.3));
        let creator = SkillCreator::new(dir.path().to_path_buf(), config)
            .with_embedding_provider(provider)
            .with_improver(improve_config);

        let result = creator
            .create_from_execution("Deploy release", &sample_tool_calls(), None)
            .await
            .unwrap();

        assert!(result.is_some());
        let slug = result.unwrap();
        assert_eq!(slug, "deploy-release");

        // Verify new skill directory was created.
        let toml_path = dir.path().join("skills").join(&slug).join("SKILL.toml");
        assert!(toml_path.exists());
    }

    #[tokio::test]
    async fn create_from_execution_skips_when_similar_but_no_improver() {
        let dir = tempfile::tempdir().unwrap();
        setup_existing_skill(dir.path(), "build-project").await;

        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };

        // No .with_improver() call — improver is None.
        let provider = Arc::new(MockEmbeddingProvider::new(0.95));
        let creator =
            SkillCreator::new(dir.path().to_path_buf(), config).with_embedding_provider(provider);

        let result = creator
            .create_from_execution("Build the project", &sample_tool_calls(), None)
            .await
            .unwrap();

        // Without improver, finding a similar skill returns None (skip).
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn create_from_execution_skips_improvement_during_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        setup_existing_skill(dir.path(), "build-project").await;

        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };
        let improve_config = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 99999,
        };

        let provider = Arc::new(MockEmbeddingProvider::new(0.95));
        let creator = SkillCreator::new(dir.path().to_path_buf(), config)
            .with_embedding_provider(provider.clone())
            .with_improver(improve_config);

        // First call should improve.
        let result = creator
            .create_from_execution("Build the project", &sample_tool_calls(), None)
            .await
            .unwrap();
        assert_eq!(result, Some("build-project".to_string()));

        // Second call should be blocked by cooldown. Need a fresh provider
        // because MockEmbeddingProvider's call_count is exhausted.
        let provider2 = Arc::new(MockEmbeddingProvider::new(0.95));
        let creator2 = SkillCreator::new(
            dir.path().to_path_buf(),
            SkillCreationConfig {
                enabled: true,
                max_skills: 500,
                similarity_threshold: 0.85,
            },
        )
        .with_embedding_provider(provider2)
        .with_improver(zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 99999,
        });

        // Pre-seed the cooldown by directly locking the improver.
        {
            let improver_lock = creator2.improver.as_ref().unwrap();
            let mut imp = improver_lock.lock().await;
            imp.cooldowns
                .insert("build-project".to_string(), std::time::Instant::now());
        }

        let result2 = creator2
            .create_from_execution("Build the project", &sample_tool_calls(), None)
            .await
            .unwrap();
        assert!(result2.is_none(), "Should be blocked by cooldown");
    }

    #[tokio::test]
    async fn create_from_execution_skips_improvement_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        setup_existing_skill(dir.path(), "build-project").await;

        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 500,
            similarity_threshold: 0.85,
        };
        let improve_config = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: false,
            cooldown_secs: 0,
        };

        let provider = Arc::new(MockEmbeddingProvider::new(0.95));
        let creator = SkillCreator::new(dir.path().to_path_buf(), config)
            .with_embedding_provider(provider)
            .with_improver(improve_config);

        let result = creator
            .create_from_execution("Build the project", &sample_tool_calls(), None)
            .await
            .unwrap();

        // Improvement is disabled -> should return None.
        assert!(result.is_none());
    }

    // ── Write guard (P1-5) ──────────────────────────────────────

    #[test]
    fn validate_generated_content_accepts_normal() {
        let content = "[skill]\nname = \"test\"\ndescription = \"A skill\"\nversion = \"0.1.0\"\n";
        assert!(super::validate_generated_content(content).is_ok());
    }

    #[test]
    fn validate_generated_content_rejects_oversized() {
        let content = "x".repeat(super::MAX_SKILL_CONTENT_BYTES + 1);
        assert!(super::validate_generated_content(&content).is_err());
    }

    #[test]
    fn validate_generated_content_rejects_too_many_tools() {
        let mut content = "[skill]\nname = \"test\"\n".to_string();
        for _ in 0..=super::MAX_TOOL_ENTRIES {
            content.push_str("[[tools]]\nname = \"t\"\n");
        }
        assert!(super::validate_generated_content(&content).is_err());
    }

    #[test]
    fn validate_generated_content_rejects_null_bytes() {
        let content = "[skill]\nname = \"test\"\x00\n";
        assert!(super::validate_generated_content(content).is_err());
    }
}
