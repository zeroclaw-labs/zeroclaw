use anyhow::{Context, Result};
use std::path::PathBuf;
use zeroclaw_api::model_provider::{ChatMessage, ChatRequest, ModelProvider};
use zeroclaw_config::schema::SkillCreationConfig;
use zeroclaw_memory::embeddings::EmbeddingProvider;
use zeroclaw_memory::vector::cosine_similarity;

use super::document::SkillDocument;

/// System prompt for the reflection path. Constrains the model to emit a
/// single canonical `SKILL.md` and nothing else.
const REFLECTION_SYSTEM_PROMPT: &str = "You convert a completed task execution into a reusable agent skill.\n\
Return ONLY a single standard SKILL.md document and nothing else.\n\
The document MUST begin with a YAML frontmatter block delimited by `---` lines containing at least:\n\
  name: <short-kebab-case-name>\n\
  description: <one sentence: what the skill does and when to use it>\n\
After the closing `---`, write a concise Markdown body of reusable, generalized instructions for accomplishing this kind of task.\n\
Do not wrap the output in code fences. Do not include backticked ```markdown fences. Do not add commentary before or after the document.\n\
Generalize away one-off specifics (concrete file paths, ids, secrets, timestamps); describe the repeatable procedure.";

/// Per-tool-call cap on serialized args inside the reflection trace, applied
/// before the whole trace is bounded by `max_tool_trace_chars`.
const MAX_TOOL_ARG_CHARS: usize = 500;

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
        let Some((slug, skill_dir)) = self
            .prepare_skill_dir(task_description, tool_calls, embedding_provider)
            .await?
        else {
            return Ok(None);
        };

        self.write_skill_toml(&skill_dir, &slug, task_description, tool_calls)
            .await?;

        Ok(Some(slug))
    }

    /// Like [`Self::create_from_execution`], but synthesizes a canonical `SKILL.md`
    /// from a *bounded* slice of the execution via a model-provider reflection
    /// call. If reflection fails — provider error, malformed output, empty
    /// body — it falls back to the deterministic `SKILL.toml` generator so a
    /// creation attempt is never left half-done.
    ///
    /// Gating (enabled flag, minimum tool calls, dedup, slug, LRU) is shared
    /// with [`Self::create_from_execution`].
    pub async fn create_from_execution_reflected(
        &self,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
        final_answer: &str,
        embedding_provider: Option<&dyn EmbeddingProvider>,
        provider_name: &str,
        model_provider: &dyn ModelProvider,
        model: &str,
    ) -> Result<Option<String>> {
        let Some((slug, skill_dir)) = self
            .prepare_skill_dir(task_description, tool_calls, embedding_provider)
            .await?
        else {
            return Ok(None);
        };

        match self
            .reflect_skill_md(
                &slug,
                task_description,
                tool_calls,
                final_answer,
                provider_name,
                model_provider,
                model,
            )
            .await
        {
            Ok(md_content) => {
                let md_path = skill_dir.join("SKILL.md");
                tokio::fs::write(&md_path, md_content.as_bytes())
                    .await
                    .with_context(|| format!("Failed to write {}", md_path.display()))?;
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "slug": slug,
                            "error": format!("{err}"),
                        })),
                    "Skill reflection failed; falling back to deterministic SKILL.toml"
                );
                self.write_skill_toml(&skill_dir, &slug, task_description, tool_calls)
                    .await?;
            }
        }

        Ok(Some(slug))
    }

    async fn prepare_skill_dir(
        &self,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
        embedding_provider: Option<&dyn EmbeddingProvider>,
    ) -> Result<Option<(String, PathBuf)>> {
        if !self.config.enabled {
            return Ok(None);
        }

        if tool_calls.len() < 2 {
            return Ok(None);
        }

        // Deduplicate via embeddings when an embedding model_provider is available.
        if let Some(model_provider) = embedding_provider
            && model_provider.name() != "none"
            && self.is_duplicate(task_description, model_provider).await?
        {
            return Ok(None);
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

        Ok(Some((slug, skill_dir)))
    }

    /// Write the deterministic `SKILL.toml` representation into `skill_dir`.
    async fn write_skill_toml(
        &self,
        skill_dir: &std::path::Path,
        slug: &str,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
    ) -> Result<()> {
        let toml_content = Self::generate_skill_toml(slug, task_description, tool_calls);
        let toml_path = skill_dir.join("SKILL.toml");
        tokio::fs::write(&toml_path, toml_content.as_bytes())
            .await
            .with_context(|| format!("Failed to write {}", toml_path.display()))?;
        Ok(())
    }

    /// Run the bounded reflection prompt through the model provider and return
    /// a normalized, validated `SKILL.md` string. Errors on provider failure
    /// or unusable output; callers fall back to `SKILL.toml`.
    async fn reflect_skill_md(
        &self,
        slug: &str,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
        final_answer: &str,
        provider_name: &str,
        model_provider: &dyn ModelProvider,
        model: &str,
    ) -> Result<String> {
        let prompt = self.build_reflection_prompt(slug, task_description, tool_calls, final_answer);
        let messages = [
            ChatMessage::system(REFLECTION_SYSTEM_PROMPT),
            ChatMessage::user(prompt),
        ];
        let access = crate::agent::loop_::ResolvedModelAccess {
            model_provider,
            provider_name,
            model,
            temperature: None,
        };
        let resp = access
            .run_model_query(ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            })
            .await
            .context("reflection provider call failed")?;
        let raw = resp.text.unwrap_or_default();
        Self::normalize_reflected_md(slug, &raw)
    }

    fn build_reflection_prompt(
        &self,
        slug: &str,
        task_description: &str,
        tool_calls: &[ToolCallRecord],
        final_answer: &str,
    ) -> String {
        let task = truncate_chars(&scrub_secrets(task_description), self.config.max_task_chars);
        // `render_tool_trace` is already budget-aware (per-call arg cap + early
        // stop) and scrubs each call's serialized args before they enter the
        // trace, so it never builds a large or secret-bearing intermediate
        // string; the outer truncate is the final hard cap.
        let trace = truncate_chars(
            &Self::render_tool_trace(tool_calls, self.config.max_tool_trace_chars),
            self.config.max_tool_trace_chars,
        );
        let answer = truncate_chars(
            &scrub_secrets(final_answer),
            self.config.max_final_answer_chars,
        );
        // The slug is derived from the task description, so it can carry the
        // same credential-shaped content; scrub the copy that enters the prompt
        // too. (The local `SKILL.md` frontmatter `name` keeps the raw slug via
        // `normalize_reflected_md`, which never leaves the host.)
        let safe_slug = scrub_secrets(slug);

        format!(
            "Suggested skill name (use as the frontmatter `name`): {safe_slug}\n\n\
             ## Task the user asked for\n{task}\n\n\
             ## Tools the agent used, in order\n{trace}\n\n\
             ## Final answer the agent produced\n{answer}\n\n\
             Write the SKILL.md now.",
        )
    }

    fn render_tool_trace(tool_calls: &[ToolCallRecord], budget: usize) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (i, call) in tool_calls.iter().enumerate() {
            if out.len() >= budget {
                let _ = writeln!(out, "…[{} more tool call(s) omitted]", tool_calls.len() - i);
                break;
            }
            let name = scrub_secrets(&call.name);
            let args = truncate_chars(
                &scrub_secrets(&serde_json::to_string(&call.args).unwrap_or_default()),
                MAX_TOOL_ARG_CHARS,
            );
            let _ = writeln!(out, "{}. {} {}", i + 1, name, args);
        }
        out
    }

    fn normalize_reflected_md(slug: &str, raw: &str) -> Result<String> {
        let content = extract_frontmatter_block(raw);
        let mut doc = SkillDocument::parse(&content).context("reflected SKILL.md is invalid")?;
        if doc.body.trim().is_empty() {
            anyhow::bail!("reflected SKILL.md has an empty body");
        }
        // Deterministic identity + auto-generated markers.
        doc.frontmatter.name = slug.to_string();
        doc.frontmatter.author = Some("zeroclaw-auto".to_string());
        if doc.frontmatter.version.is_none() {
            doc.frontmatter.version = Some("0.1.0".to_string());
        }
        Ok(doc.serialize())
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
            // Compare against both emit formats: deterministic `SKILL.toml` and
            // reflected `SKILL.md`.
            let Some(manifest) = skill_manifest_path(&entry.path()) else {
                continue;
            };

            let content = tokio::fs::read_to_string(&manifest).await?;
            if let Some(desc) = extract_skill_description(&manifest, &content) {
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
            let Some(manifest) = skill_manifest_path(&entry.path()) else {
                continue;
            };

            let content = tokio::fs::read_to_string(&manifest).await?;
            // The auto marker appears as `author = "zeroclaw-auto"` in TOML and
            // `author: zeroclaw-auto` in reflected `SKILL.md`; match the bare
            // token so both formats are evicted.
            if content.contains("zeroclaw-auto") || content.contains("auto-generated") {
                let modified = tokio::fs::metadata(&manifest)
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

/// Locate a skill directory's manifest, preferring the deterministic
/// `SKILL.toml` over a reflected `SKILL.md` when both happen to exist.
fn skill_manifest_path(skill_dir: &std::path::Path) -> Option<PathBuf> {
    let toml_path = skill_dir.join("SKILL.toml");
    if toml_path.exists() {
        return Some(toml_path);
    }
    let md_path = skill_dir.join("SKILL.md");
    if md_path.exists() {
        return Some(md_path);
    }
    None
}

/// Extract a skill's description from a manifest, dispatching on file type:
/// YAML frontmatter for `SKILL.md`, TOML `[skill]` for `SKILL.toml`.
fn extract_skill_description(manifest: &std::path::Path, content: &str) -> Option<String> {
    if manifest.extension().and_then(|ext| ext.to_str()) == Some("md") {
        SkillDocument::parse(content)
            .ok()
            .map(|doc| doc.frontmatter.description)
    } else {
        extract_description_from_toml(content)
    }
}

fn scrub_secrets(s: &str) -> String {
    match crate::security::LeakDetector::new().scan(s) {
        crate::security::LeakResult::Clean => s.to_string(),
        crate::security::LeakResult::Detected { redacted, .. } => redacted,
    }
}

/// Truncate `s` to at most `max` characters (not bytes), appending a marker
/// when content was dropped. Char-boundary safe for multi-byte UTF-8.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max).collect();
    format!("{truncated}\n…[truncated]")
}

/// Best-effort extraction of a canonical `SKILL.md` from raw model output.
/// Strips a single enclosing Markdown code fence and any preamble before the
/// leading `---` frontmatter delimiter so [`SkillDocument::parse`] (which
/// requires the document to start with `---\n`) sees clean input.
fn extract_frontmatter_block(raw: &str) -> String {
    let mut text = raw.trim();

    // Drop a single wrapping ``` / ```markdown fence if present.
    if let Some(rest) = text.strip_prefix("```") {
        let after_lang = rest.split_once('\n').map_or("", |(_, body)| body);
        text = after_lang
            .trim()
            .strip_suffix("```")
            .unwrap_or(after_lang)
            .trim();
    }

    // Skip any preamble before the first `---` frontmatter line.
    if text.starts_with("---\n") || text == "---" {
        return text.to_string();
    }
    if let Some(idx) = text.find("\n---\n") {
        return text[idx + 1..].to_string();
    }
    text.to_string()
}

/// Extract `ToolCallRecord`s from the agent conversation history.
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
            ..Default::default()
        };

        // High similarity model_provider -> should detect as duplicate.
        let model_provider = MockEmbeddingProvider::new(0.95);
        let creator = SkillCreator::new(dir.path().to_path_buf(), config.clone());
        assert!(
            creator
                .is_duplicate("Build the project", &model_provider)
                .await
                .unwrap()
        );

        // Low similarity model_provider -> not a duplicate.
        let provider_low = MockEmbeddingProvider::new(0.3);
        let creator2 = SkillCreator::new(dir.path().to_path_buf(), config);
        assert!(
            !creator2
                .is_duplicate("Completely different task", &provider_low)
                .await
                .unwrap()
        );
    }

    // ── LRU eviction ─────────────────────────────────────────────

    #[tokio::test]
    async fn lru_eviction_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 2,
            similarity_threshold: 0.85,
            ..Default::default()
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
            ..Default::default()
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

        // High similarity model_provider -> should skip.
        let model_provider = MockEmbeddingProvider::new(0.95);
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
            .create_from_execution("Build and test", &calls, Some(&model_provider))
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

    // ── Reflection: SKILL.md synthesis ───────────────────────────

    enum MockReply {
        Md(String),
        Fail,
    }

    /// Minimal `ModelProvider` for reflection tests: returns a canned reply or
    /// fails, counting calls so "skipped" paths can assert the provider was
    /// never invoked and capturing the last user prompt so privacy tests can
    /// assert exactly what content was sent.
    struct MockModelProvider {
        reply: MockReply,
        calls: std::sync::atomic::AtomicUsize,
        last_prompt: std::sync::Mutex<Option<String>>,
    }

    impl MockModelProvider {
        fn replying(md: impl Into<String>) -> Self {
            Self {
                reply: MockReply::Md(md.into()),
                calls: std::sync::atomic::AtomicUsize::new(0),
                last_prompt: std::sync::Mutex::new(None),
            }
        }
        fn failing() -> Self {
            Self {
                reply: MockReply::Fail,
                calls: std::sync::atomic::AtomicUsize::new(0),
                last_prompt: std::sync::Mutex::new(None),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
        /// The user prompt passed to the most recent `chat_with_system` call.
        fn last_prompt(&self) -> Option<String> {
            self.last_prompt.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.last_prompt.lock().unwrap() = Some(message.to_string());
            match &self.reply {
                MockReply::Md(s) => Ok(s.clone()),
                MockReply::Fail => anyhow::bail!("mock provider failure"),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    fn two_calls() -> Vec<ToolCallRecord> {
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

    fn reflect_config() -> SkillCreationConfig {
        SkillCreationConfig {
            enabled: true,
            reflection_enabled: true,
            ..Default::default()
        }
    }

    /// Assert the fallback `SKILL.toml` is valid TOML with the expected name,
    /// so a broken fallback (e.g. empty file) cannot pass a mere existence check.
    async fn assert_valid_toml_skill(skill_dir: &std::path::Path, expected_name: &str) {
        let content = tokio::fs::read_to_string(skill_dir.join("SKILL.toml"))
            .await
            .unwrap();
        let parsed: toml::Value = toml::from_str(&content).expect("fallback SKILL.toml is valid");
        assert_eq!(
            parsed["skill"]["name"].as_str(),
            Some(expected_name),
            "fallback skill name should match the prepared slug"
        );
    }

    #[tokio::test]
    async fn reflected_writes_skill_md_on_success() {
        let dir = tempfile::tempdir().unwrap();
        // The model proposes its own name; normalization must override it with
        // the deterministic slug and stamp the auto-generated author.
        let provider = MockModelProvider::replying(
            "---\nname: model-picked-name\ndescription: Build and test the project.\n---\n\n# Build & Test\n\nRun the build, then the tests.\n",
        );
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let slug = creator
            .create_from_execution_reflected(
                "Build and test the project",
                &two_calls(),
                "All tests passed.",
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap()
            .expect("a skill should be created");

        assert_eq!(slug, "build-and-test-the-project");
        let skill_dir = dir.path().join("skills").join(&slug);
        assert!(skill_dir.join("SKILL.md").exists());
        assert!(!skill_dir.join("SKILL.toml").exists());

        let content = tokio::fs::read_to_string(skill_dir.join("SKILL.md"))
            .await
            .unwrap();
        let doc = SkillDocument::parse(&content).unwrap();
        assert_eq!(doc.frontmatter.name, slug);
        assert_eq!(doc.frontmatter.author.as_deref(), Some("zeroclaw-auto"));
        // The model's body must be preserved through normalization, not dropped.
        assert!(doc.body.contains("# Build & Test"));
        assert!(doc.body.contains("Run the build, then the tests."));
    }

    #[tokio::test]
    async fn reflected_falls_back_to_toml_on_provider_error() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockModelProvider::failing();
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let slug = creator
            .create_from_execution_reflected(
                "Deploy the service",
                &two_calls(),
                "Deployed.",
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap()
            .expect("fallback should still create a skill");

        let skill_dir = dir.path().join("skills").join(&slug);
        assert!(skill_dir.join("SKILL.toml").exists());
        assert!(!skill_dir.join("SKILL.md").exists());
        assert_valid_toml_skill(&skill_dir, &slug).await;
    }

    #[tokio::test]
    async fn reflected_over_budget_skips_provider_call() {
        use crate::agent::cost::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
        use crate::cost::CostTracker;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        // A provider that WOULD succeed if reached; the already-exceeded budget
        // must block the reflection call before it happens.
        let provider = MockModelProvider::replying(
            "---\nname: model-picked\ndescription: Build and test the project.\n---\n\n# X\n\nBody.\n",
        );
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let cost_dir = tempfile::tempdir().unwrap();
        let cost_config = zeroclaw_config::schema::CostConfig {
            enabled: true,
            daily_limit_usd: 0.001, // very low limit
            ..zeroclaw_config::schema::CostConfig::default()
        };
        let tracker = Arc::new(CostTracker::new(cost_config, cost_dir.path()).unwrap());
        // Pre-record usage that already exceeds the daily limit.
        tracker
            .record_usage(crate::cost::types::TokenUsage::new(
                "mock-model",
                100_000,
                50_000,
                0,
                1.0,
                1.0,
                0.0,
            ))
            .unwrap();
        let before = tracker.get_summary().unwrap().request_count;
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(std::collections::HashMap::new()),
        );

        // Drive the reflected path under the exceeded budget scope. It returns
        // Ok because reflection fails closed to the deterministic SKILL.toml.
        let slug = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(ctx),
                creator.create_from_execution_reflected(
                    "Build and test the project",
                    &two_calls(),
                    "All tests passed.",
                    None,
                    "mock-provider",
                    &provider,
                    "mock-model",
                ),
            )
            .await
            .expect("reflection returns Ok even over budget (TOML fallback)")
            .expect("skill is still created via the deterministic TOML fallback");

        // The over-budget guard blocked the provider call entirely ...
        assert_eq!(
            provider.call_count(),
            0,
            "over-budget reflection must not reach the provider"
        );
        // ... recorded no new usage ...
        assert_eq!(tracker.get_summary().unwrap().request_count, before);
        // ... and still produced a skill deterministically via SKILL.toml.
        let skill_dir = dir.path().join("skills").join(&slug);
        assert!(skill_dir.join("SKILL.toml").exists());
        assert!(!skill_dir.join("SKILL.md").exists());
    }

    #[tokio::test]
    async fn reflected_falls_back_to_toml_on_invalid_output() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockModelProvider::replying("I could not produce a skill, sorry.");
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let slug = creator
            .create_from_execution_reflected(
                "Summarize the logs",
                &two_calls(),
                "Done.",
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap()
            .expect("fallback should still create a skill");

        let skill_dir = dir.path().join("skills").join(&slug);
        assert!(skill_dir.join("SKILL.toml").exists());
        assert!(!skill_dir.join("SKILL.md").exists());
        assert_valid_toml_skill(&skill_dir, &slug).await;
    }

    #[tokio::test]
    async fn reflected_falls_back_to_toml_on_empty_body() {
        let dir = tempfile::tempdir().unwrap();
        // Valid frontmatter, but no body — must be rejected and fall back.
        let provider = MockModelProvider::replying("---\nname: x\ndescription: A thing.\n---\n");
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let slug = creator
            .create_from_execution_reflected(
                "Empty body case",
                &two_calls(),
                "Done.",
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap()
            .expect("fallback should still create a skill");

        let skill_dir = dir.path().join("skills").join(&slug);
        assert!(skill_dir.join("SKILL.toml").exists());
        assert!(!skill_dir.join("SKILL.md").exists());
        assert_valid_toml_skill(&skill_dir, &slug).await;
    }

    #[tokio::test]
    async fn reflected_skips_when_creation_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockModelProvider::replying("---\nname: x\ndescription: y\n---\n# Body\n");
        let config = SkillCreationConfig {
            enabled: false,
            reflection_enabled: true,
            ..Default::default()
        };
        let creator = SkillCreator::new(dir.path().to_path_buf(), config);

        let result = creator
            .create_from_execution_reflected(
                "Anything",
                &two_calls(),
                "answer",
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap();
        assert!(result.is_none());
        // Disabled creation must not even reach the provider.
        assert_eq!(provider.call_count(), 0);
        assert!(!dir.path().join("skills").exists());
    }

    #[tokio::test]
    async fn reflected_skips_with_too_few_tool_calls() {
        let dir = tempfile::tempdir().unwrap();
        let provider = MockModelProvider::replying("---\nname: x\ndescription: y\n---\n# Body\n");
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());
        let one_call = vec![ToolCallRecord {
            name: "shell".into(),
            args: serde_json::json!({"command": "ls"}),
        }];

        let result = creator
            .create_from_execution_reflected(
                "Just one step",
                &one_call,
                "answer",
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap();
        assert!(result.is_none());
        assert_eq!(provider.call_count(), 0);
    }

    #[test]
    fn reflection_prompt_bounds_each_input() {
        let config = SkillCreationConfig {
            enabled: true,
            reflection_enabled: true,
            max_task_chars: 5,
            max_tool_trace_chars: 5,
            max_final_answer_chars: 10,
            ..Default::default()
        };
        let creator = SkillCreator::new(std::path::PathBuf::from("/tmp"), config);
        let long_answer = "ANSWERX".repeat(50); // 350 chars, well over the 10-char bound
        let prompt = creator.build_reflection_prompt(
            "slug",
            &"TASKWORD".repeat(20),
            &two_calls(),
            &long_answer,
        );
        assert!(prompt.contains("…[truncated]"));
        // The full, unbounded answer must never appear verbatim.
        assert!(!prompt.contains(&long_answer));
        // Clean content must survive the scrub: the bounded prefixes of the
        // (secret-free) task and answer are still present, so scrubbing did not
        // wrongly destroy non-credential input.
        assert!(
            prompt.contains("TASKW"),
            "clean task prefix dropped: {prompt}"
        );
        assert!(
            prompt.contains("ANSWER"),
            "clean answer prefix dropped: {prompt}"
        );
    }

    #[test]
    fn reflection_prompt_scrubs_credential_shaped_inputs() {
        // Credential-shaped values in the task, tool args, and final answer must
        // be redacted by `build_reflection_prompt` itself — the prompt is what
        // `reflect_skill_md` sends to the provider, so the secrets must be gone
        // before the string is ever composed.
        let creator = SkillCreator::new(std::path::PathBuf::from("/tmp"), reflect_config());

        let aws_key = "AKIAIOSFODNN7EXAMPLE";
        let anthropic_key = "sk-ant-api03-AbC123dEf456GhI789jKl012MnO345pQr678";
        let db_url = "postgres://admin:hunter2pw@db.internal:5432/prod";

        let calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": format!("export ANTHROPIC_API_KEY={anthropic_key}")}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": format!("psql {db_url} -c 'select 1'")}),
            },
        ];

        let prompt = creator.build_reflection_prompt(
            "deploy-the-service",
            &format!("Deploy using key {aws_key}"),
            &calls,
            &format!("Authenticated with {aws_key} against {db_url}."),
        );

        // No raw secret may survive into the prompt sent to the provider.
        assert!(!prompt.contains(aws_key), "AWS key leaked: {prompt}");
        assert!(
            !prompt.contains(anthropic_key),
            "Anthropic key leaked: {prompt}"
        );
        assert!(!prompt.contains(db_url), "database URL leaked: {prompt}");
        // ...and each surface's specific marker is present, so a future regex
        // regression that silently stops matching one pattern is caught rather
        // than masked by another pattern's marker.
        assert!(
            prompt.contains("[REDACTED_AWS_CREDENTIAL]"),
            "AWS key (task + answer) not redacted: {prompt}"
        );
        assert!(
            prompt.contains("[REDACTED_API_KEY]"),
            "Anthropic key (tool args) not redacted: {prompt}"
        );
        assert!(
            prompt.contains("[REDACTED_DATABASE_URL]"),
            "database URL (tool args + answer) not redacted: {prompt}"
        );
    }

    #[test]
    fn reflection_prompt_scrubs_credential_shaped_slug() {
        let creator = SkillCreator::new(std::path::PathBuf::from("/tmp"), reflect_config());
        let key_slug = "sk-ant-api03-AbC123dEf456GhI789jKl012MnO345pQr678";

        let prompt = creator.build_reflection_prompt(
            key_slug,
            "A clean task",
            &two_calls(),
            "Clean answer.",
        );

        assert!(
            !prompt.contains(key_slug),
            "credential-shaped slug leaked into prompt: {prompt}"
        );
        assert!(
            prompt.contains("[REDACTED_API_KEY]"),
            "slug not redacted: {prompt}"
        );
    }

    #[tokio::test]
    async fn reflected_scrubs_secrets_before_provider_call() {
        // End-to-end: drive the reflected creation path and assert the prompt
        // the provider actually received carries no raw credential-shaped data.
        let dir = tempfile::tempdir().unwrap();
        let aws_key = "AKIAIOSFODNN7EXAMPLE";
        let anthropic_key = "sk-ant-api03-AbC123dEf456GhI789jKl012MnO345pQr678";

        let provider = MockModelProvider::replying(
            "---\nname: x\ndescription: Deploy the service.\n---\n# Deploy\n\nSteps here.\n",
        );
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": format!("export ANTHROPIC_API_KEY={anthropic_key}")}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({"command": "deploy --prod"}),
            },
        ];

        creator
            .create_from_execution_reflected(
                "Deploy the service",
                &calls,
                &format!("Done. Authenticated with {aws_key}."),
                None,
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap()
            .expect("a skill should be created");

        let sent = provider
            .last_prompt()
            .expect("the provider should have been called");
        assert!(
            !sent.contains(aws_key),
            "AWS key reached the provider: {sent}"
        );
        assert!(
            !sent.contains(anthropic_key),
            "Anthropic key reached the provider: {sent}"
        );
        // Pin the specific markers: the AWS key (final answer) and the Anthropic
        // key (tool args) must each be individually redacted, so dropping either
        // pattern is caught rather than hidden behind the other's marker.
        assert!(
            sent.contains("[REDACTED_AWS_CREDENTIAL]"),
            "AWS key not redacted in provider prompt: {sent}"
        );
        assert!(
            sent.contains("[REDACTED_API_KEY]"),
            "Anthropic key not redacted in provider prompt: {sent}"
        );
    }

    #[test]
    fn scrub_secrets_redacts_and_preserves() {
        // Direct contract for the redaction helper: a known credential is
        // replaced with its marker, clean text passes through byte-for-byte, and
        // the empty string is handled.
        let redacted = scrub_secrets("key AKIAIOSFODNN7EXAMPLE here");
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(redacted.contains("[REDACTED_AWS_CREDENTIAL]"));

        assert_eq!(
            scrub_secrets("just a normal sentence"),
            "just a normal sentence"
        );
        assert_eq!(scrub_secrets(""), "");
    }

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        // Multi-byte characters must not be split mid-codepoint.
        let out = truncate_chars("héllo wörld", 4);
        assert!(out.starts_with("héll"));
        assert!(out.contains("…[truncated]"));
        // No marker when content is within budget.
        assert_eq!(truncate_chars("short", 100), "short");
    }

    #[test]
    fn extract_frontmatter_block_strips_code_fence() {
        let raw = "```markdown\n---\nname: x\ndescription: y\n---\n# Body\n```";
        let out = extract_frontmatter_block(raw);
        assert!(out.starts_with("---\n"));
        let doc = SkillDocument::parse(&out).unwrap();
        assert_eq!(doc.frontmatter.name, "x");
    }

    #[test]
    fn extract_frontmatter_block_skips_preamble() {
        let raw = "Sure! Here is your skill:\n\n---\nname: x\ndescription: y\n---\n# Body\n";
        let out = extract_frontmatter_block(raw);
        let doc = SkillDocument::parse(&out).unwrap();
        assert_eq!(doc.frontmatter.description, "y");
    }

    #[tokio::test]
    async fn dedup_detects_existing_reflected_md() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills").join("existing");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: existing\ndescription: Build the project\nauthor: zeroclaw-auto\n---\n# Build\n\nBuilds it.\n",
        )
        .await
        .unwrap();

        let provider = MockEmbeddingProvider::new(0.95);
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());
        assert!(
            creator
                .is_duplicate("Build the project", &provider)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn lru_eviction_recognizes_reflected_md() {
        let dir = tempfile::tempdir().unwrap();
        let config = SkillCreationConfig {
            enabled: true,
            max_skills: 2,
            ..Default::default()
        };
        let skills_dir = dir.path().join("skills");

        // Two auto-generated reflected (SKILL.md) skills with distinct mtimes.
        for name in ["old-md", "new-md"] {
            let skill_dir = skills_dir.join(name);
            tokio::fs::create_dir_all(&skill_dir).await.unwrap();
            tokio::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: A skill\nauthor: zeroclaw-auto\n---\n# {name}\n\nBody.\n"),
            )
            .await
            .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let creator = SkillCreator::new(dir.path().to_path_buf(), config);
        creator.enforce_lru_limit().await.unwrap();

        // The auto-generated SKILL.md skills must be recognized and the oldest evicted.
        assert!(!skills_dir.join("old-md").exists());
        assert!(skills_dir.join("new-md").exists());
    }

    // ── Reflection helpers: direct unit tests ────────────────────

    #[test]
    fn render_tool_trace_formats_numbered_calls() {
        let trace = SkillCreator::render_tool_trace(&two_calls(), 4000);
        assert!(trace.starts_with("1. shell "));
        assert!(trace.contains("\n2. shell "));
        assert!(trace.contains("cargo build"));
        assert!(trace.contains("cargo test"));
    }

    #[test]
    fn render_tool_trace_bounds_huge_and_numerous_args() {
        // 50 calls each with ~5k-char args would naively serialize to ~250k chars.
        let big = serde_json::json!({"command": "x".repeat(5000)});
        let calls: Vec<ToolCallRecord> = (0..50)
            .map(|i| ToolCallRecord {
                name: format!("tool{i}"),
                args: big.clone(),
            })
            .collect();

        let trace = SkillCreator::render_tool_trace(&calls, 1000);

        // Each call's args are capped, and rendering stops near the budget, so the
        // intermediate string stays small (budget + at most one over-budget call).
        assert!(
            trace.chars().count() < 2500,
            "trace not bounded: {} chars",
            trace.chars().count()
        );
        assert!(trace.contains("…[truncated]"));
        assert!(trace.contains("more tool call(s) omitted"));
    }

    #[test]
    fn normalize_overrides_name_and_stamps_author() {
        let md = "---\nname: model-name\ndescription: Does things.\n---\n# Body\n\nStuff.\n";
        let out = SkillCreator::normalize_reflected_md("forced-slug", md).unwrap();
        let doc = SkillDocument::parse(&out).unwrap();
        assert_eq!(doc.frontmatter.name, "forced-slug");
        assert_eq!(doc.frontmatter.author.as_deref(), Some("zeroclaw-auto"));
        // version is defaulted when the model omits it.
        assert_eq!(doc.frontmatter.version.as_deref(), Some("0.1.0"));
        assert!(doc.body.contains("Stuff."));
    }

    #[test]
    fn normalize_preserves_existing_version() {
        let md = "---\nname: x\ndescription: y\nversion: 9.9.9\n---\n# B\n\nbody\n";
        let out = SkillCreator::normalize_reflected_md("slug", md).unwrap();
        let doc = SkillDocument::parse(&out).unwrap();
        assert_eq!(doc.frontmatter.version.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn normalize_rejects_missing_frontmatter_and_empty_body() {
        assert!(SkillCreator::normalize_reflected_md("slug", "no frontmatter at all").is_err());
        assert!(
            SkillCreator::normalize_reflected_md("slug", "---\nname: x\ndescription: y\n---\n")
                .is_err()
        );
    }

    #[tokio::test]
    async fn skill_manifest_prefers_toml_over_md() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("both");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.toml"),
            "[skill]\nname = \"both\"\ndescription = \"toml desc\"\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: both\ndescription: md desc\n---\n# B\n\nbody\n",
        )
        .await
        .unwrap();

        let manifest = skill_manifest_path(&skill_dir).expect("a manifest");
        assert!(manifest.ends_with("SKILL.toml"));
        let content = std::fs::read_to_string(&manifest).unwrap();
        assert_eq!(
            extract_skill_description(&manifest, &content).as_deref(),
            Some("toml desc")
        );
    }

    #[tokio::test]
    async fn reflected_dedup_skips_before_provider_call() {
        let dir = tempfile::tempdir().unwrap();
        // An existing auto skill gives dedup something to match against.
        let existing = dir.path().join("skills").join("existing");
        tokio::fs::create_dir_all(&existing).await.unwrap();
        tokio::fs::write(
            existing.join("SKILL.toml"),
            "[skill]\nname = \"existing\"\ndescription = \"Auto-generated: Build the project\"\nauthor = \"zeroclaw-auto\"\n",
        )
        .await
        .unwrap();

        let provider = MockModelProvider::replying("---\nname: x\ndescription: y\n---\n# B\n");
        let embed = MockEmbeddingProvider::new(0.95);
        let creator = SkillCreator::new(dir.path().to_path_buf(), reflect_config());

        let result = creator
            .create_from_execution_reflected(
                "Build the project",
                &two_calls(),
                "answer",
                Some(&embed),
                "mock-provider",
                &provider,
                "test-model",
            )
            .await
            .unwrap();

        // Dedup must short-circuit before spending a reflection (LLM) call.
        assert!(result.is_none());
        assert_eq!(provider.call_count(), 0);
    }
}
