//! Hermes-style **reflection**: turn a successful tool-using task into a `SKILL.md` via LLM,
//! optionally followed by an **evolver** pass that tightens the document.

use std::path::Path;

use anyhow::Context;
use serde_json::json;
use zeroclaw_providers::Provider;

use super::creator::ToolCallRecord;

const REFLECTION_SYSTEM: &str = r#"You are writing a reusable agent skill as Markdown (agentskills.io / SKILL.md style).

Given the user's task, the tool trace, and the assistant's final answer, produce ONE skill document.

Rules:
- Start with YAML frontmatter: `name`, `description`, `version` (semver), `author: zeroclaw-auto`, `tags` including `auto-generated` and `zeroclaw-reflection`.
- Use the provided `slug` as the `name` field (hyphenated identifier).
- Sections after frontmatter: `## When to use`, `## Procedure` (numbered steps referencing real tool names from the trace), `## Pitfalls`, `## Verification`.
- Be concise and actionable. Do not invent tools that were not in the trace.
- Output ONLY the markdown file content (no preamble or commentary)."#;

const EVOLVER_SYSTEM: &str = r#"You refine an existing SKILL.md for a coding agent.

Improve clarity, remove redundancy, strengthen Pitfalls with real edge cases, and make Verification concrete.
Preserve YAML frontmatter; keep `author: zeroclaw-auto` and tags including `zeroclaw-reflection`.
Output ONLY the full markdown file."#;

/// Remove optional ``` / ```markdown fences if the model wrapped the file.
pub fn strip_markdown_fences(raw: &str) -> String {
    let t = raw.trim();
    if !t.starts_with("```") {
        return t.to_string();
    }
    let after_open = t.strip_prefix("```").unwrap_or(t);
    let body = if let Some(idx) = after_open.find('\n') {
        &after_open[idx + 1..]
    } else {
        after_open
    };
    if let Some(end) = body.rfind("```") {
        body[..end].trim().to_string()
    } else {
        body.trim().to_string()
    }
}

/// Run reflection (and optional evolver) and return final markdown body.
pub async fn reflect_skill_markdown(
    provider: &dyn Provider,
    model: &str,
    temperature: f64,
    evolver: bool,
    slug: &str,
    task: &str,
    tool_calls: &[ToolCallRecord],
    final_answer: &str,
) -> anyhow::Result<String> {
    let trace: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|t| json!({ "name": t.name, "args": t.args }))
        .collect();
    let trace_json = serde_json::to_string_pretty(&trace).unwrap_or_else(|_| "[]".to_string());

    let user = format!(
        "Skill slug (use as frontmatter `name`): {slug}\n\n\
         User task:\n{task}\n\n\
         Tool call trace (JSON):\n{trace_json}\n\n\
         Final assistant answer:\n{final_answer}\n"
    );

    let mut md = provider
        .chat_with_system(Some(REFLECTION_SYSTEM), &user, model, temperature)
        .await
        .context("reflection LLM call failed")?;

    if evolver {
        md = provider
            .chat_with_system(
                Some(EVOLVER_SYSTEM),
                &format!(
                    "Improve this skill. Output the full file only.\n\n{}",
                    strip_markdown_fences(&md)
                ),
                model,
                temperature,
            )
            .await
            .context("evolver LLM call failed")?;
    }

    let md = strip_markdown_fences(&md);
    if md.trim().is_empty() {
        anyhow::bail!("reflection produced empty skill markdown");
    }
    Ok(md)
}

/// Write `SKILL.md` under `skill_dir` atomically via a temp file.
pub async fn write_skill_md_atomic(skill_dir: &Path, content: &str) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(skill_dir)
        .await
        .with_context(|| format!("create skill dir {}", skill_dir.display()))?;
    let tmp = skill_dir.join(".SKILL.md.tmp");
    let final_path = skill_dir.join("SKILL.md");
    tokio::fs::write(&tmp, content.as_bytes())
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, &final_path)
        .await
        .with_context(|| format!("rename to {}", final_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::strip_markdown_fences;

    #[test]
    fn strip_fences_plain() {
        let s = "---\nname: x\n---\n# Hi";
        assert_eq!(strip_markdown_fences(s).trim(), s.trim());
    }

    #[test]
    fn strip_fences_markdown_lang() {
        let s = "```markdown\n---\nname: x\n---\nbody\n```";
        let out = strip_markdown_fences(s);
        assert!(out.contains("name: x"));
        assert!(!out.contains("```"));
    }
}
