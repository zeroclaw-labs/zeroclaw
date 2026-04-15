// Workflow Scaffolder — voice utterance → YAML workflow draft (v3.0 Section D-2)
//
// Given a user utterance like "의뢰인 전화 끝나면 매번 상담일지 자동으로 써줘",
// the scaffolder:
// 1. Gathers context: available tools, similar workflows (few-shot), seed category hint
// 2. Calls Claude Opus to draft a YAML workflow
// 3. Validates via parse_spec (JSON schema + ID uniqueness + cost limits)
// 4. Injects safety defaults (cost caps) if missing
// 5. Returns draft for user confirmation

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::parser::{parse_spec, WorkflowSpec};
use super::registry::ToolRegistry;
use crate::providers::traits::Provider;

/// Input for scaffolding a new workflow.
#[derive(Debug, Clone)]
pub struct ScaffoldRequest {
    /// The user's voice utterance (after STT).
    pub utterance: String,
    /// Hinted category (may be None — Scaffolder infers from utterance).
    pub category_hint: Option<String>,
    /// Few-shot examples of similar workflows (YAML text).
    pub similar_examples: Vec<String>,
}

/// Result of scaffolding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldResponse {
    /// The generated YAML (validated).
    pub yaml: String,
    /// Parsed spec for display/confirmation.
    pub spec: WorkflowSpec,
    /// Estimated token cost per run.
    pub estimated_cost_tokens: u32,
    /// Any warnings or caveats (e.g. "required tool not whitelisted").
    pub warnings: Vec<String>,
}

/// Default cost limits when the LLM doesn't specify them.
const DEFAULT_MAX_TOKENS: u32 = 30_000;
const DEFAULT_MAX_LLM_CALLS: u32 = 5;

/// Generate a workflow YAML draft from a voice utterance.
pub async fn scaffold(
    request: &ScaffoldRequest,
    registry: &ToolRegistry,
    provider: &dyn Provider,
    model: &str,
) -> Result<ScaffoldResponse> {
    let system_prompt = build_system_prompt(registry, &request.similar_examples);

    let user_prompt = format!(
        "사용자 요청: {}\n\n\
         카테고리 힌트: {}\n\n\
         위 요청을 수행하는 YAML 워크플로우를 생성하세요. \
         YAML만 출력하고 다른 설명은 포함하지 마세요.",
        request.utterance,
        request.category_hint.as_deref().unwrap_or("(미지정)")
    );

    let raw_response = provider
        .chat_with_system(Some(&system_prompt), &user_prompt, model, 0.2)
        .await
        .context("scaffolder LLM call failed")?;

    // Extract YAML from response (strip markdown code fences if present)
    let yaml = extract_yaml(&raw_response);

    // Validate and inject defaults
    let (final_yaml, spec, warnings) = validate_and_fix(&yaml, registry)?;

    // Estimate cost (rough: prompt length × step count)
    let estimated_cost_tokens = estimate_cost(&spec);

    Ok(ScaffoldResponse {
        yaml: final_yaml,
        spec,
        estimated_cost_tokens,
        warnings,
    })
}

/// Build the system prompt with tool registry + few-shot examples.
fn build_system_prompt(registry: &ToolRegistry, examples: &[String]) -> String {
    let mut prompt = String::from(
        "You are a workflow YAML generator for MoA (a legal/personal AI assistant). \
         Given a user's natural-language request, output a YAML workflow spec.\n\n\
         Schema requirements:\n\
         - Top-level keys: name, parent_category, steps, limits (required)\n\
         - parent_category must be one of: daily, shopping, document, coding, interpret, phone, image, music, video\n\
         - Each step has a 'type' field and unique 'id'\n\
         - Step types: memory_recall, memory_store, llm, tool_call, file_write, calendar_add, phone_action, shell, conditional, loop, user_confirm\n\
         - Templates use {{var}} syntax, with {{input.NAME}} for inputs and {{step_id.output}} for step outputs\n\
         - limits.max_tokens_per_run and limits.max_llm_calls_per_run are REQUIRED and must be > 0\n\n\
         Available tools by category:\n",
    );
    for cat in registry.categories() {
        let tools = registry.tools_for_category(cat);
        prompt.push_str(&format!("  {cat}: {}\n", tools.join(", ")));
    }

    if !examples.is_empty() {
        prompt.push_str("\nSimilar workflow examples (for reference):\n");
        for (i, ex) in examples.iter().take(3).enumerate() {
            prompt.push_str(&format!("--- Example {}: ---\n{}\n", i + 1, ex));
        }
    }

    prompt.push_str(
        "\nOutput ONLY valid YAML, no markdown fences, no prose. \
         Start directly with 'name:'.",
    );

    prompt
}

/// Extract YAML from LLM response (strips ```yaml fences and leading prose).
fn extract_yaml(response: &str) -> String {
    let trimmed = response.trim();

    // Strip markdown code fences
    if let Some(start) = trimmed.find("```yaml") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.rfind("```") {
            return after[..end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.rfind("```") {
            return after[..end].trim().to_string();
        }
    }

    // If no fences, find first line starting with "name:"
    for (i, line) in trimmed.lines().enumerate() {
        if line.trim_start().starts_with("name:") {
            return trimmed.lines().skip(i).collect::<Vec<_>>().join("\n");
        }
    }

    trimmed.to_string()
}

/// Validate the YAML, injecting safety defaults when missing, and check tool permissions.
fn validate_and_fix(
    yaml: &str,
    registry: &ToolRegistry,
) -> Result<(String, WorkflowSpec, Vec<String>)> {
    let mut warnings = Vec::new();

    // First pass: try to parse
    let mut spec = match parse_spec(yaml) {
        Ok(spec) => spec,
        Err(e) => {
            let err_msg = format!("{e:#}");
            // Try auto-repair: inject missing limits
            let limits_missing = err_msg.contains("max_tokens_per_run")
                || err_msg.contains("missing field `limits`")
                || err_msg.contains("limits");
            if limits_missing {
                let repaired = inject_default_limits(yaml);
                warnings.push("limits 누락 → 기본값 주입".to_string());
                parse_spec(&repaired).context("scaffolder YAML invalid even after repair")?
            } else {
                return Err(e.context("scaffolder YAML validation failed"));
            }
        }
    };

    // Ensure safety caps are reasonable
    if spec.limits.max_tokens_per_run == 0 {
        spec.limits.max_tokens_per_run = DEFAULT_MAX_TOKENS;
        warnings.push("max_tokens_per_run 0 → 기본값으로 조정".to_string());
    }
    if spec.limits.max_llm_calls_per_run == 0 {
        spec.limits.max_llm_calls_per_run = DEFAULT_MAX_LLM_CALLS;
        warnings.push("max_llm_calls_per_run 0 → 기본값으로 조정".to_string());
    }

    // Check tool permissions
    check_tool_permissions(&spec, registry, &mut warnings);

    // Re-serialize
    let final_yaml = serde_yaml::to_string(&spec).context("re-serialize failed")?;

    Ok((final_yaml, spec, warnings))
}

/// Inject default `limits` block if missing.
fn inject_default_limits(yaml: &str) -> String {
    if yaml.contains("limits:") {
        return yaml.to_string();
    }
    format!(
        "{yaml}\nlimits:\n  max_tokens_per_run: {DEFAULT_MAX_TOKENS}\n  max_llm_calls_per_run: {DEFAULT_MAX_LLM_CALLS}\n"
    )
}

/// Collect warnings for any tool_call step that references an unpermitted tool.
fn check_tool_permissions(
    spec: &WorkflowSpec,
    registry: &ToolRegistry,
    warnings: &mut Vec<String>,
) {
    check_steps_permissions(&spec.steps, &spec.parent_category, registry, warnings);
}

fn check_steps_permissions(
    steps: &[super::parser::Step],
    category: &str,
    registry: &ToolRegistry,
    warnings: &mut Vec<String>,
) {
    use super::parser::Step;
    for step in steps {
        match step {
            Step::ToolCall(s) => {
                if registry.check_permission(&s.tool, category).is_err() {
                    warnings.push(format!(
                        "도구 '{}' 가 카테고리 '{}' 에서 허용되지 않음",
                        s.tool, category
                    ));
                }
            }
            Step::Conditional(s) => {
                check_steps_permissions(&s.then, category, registry, warnings);
                if let Some(ref else_) = s.else_ {
                    check_steps_permissions(else_, category, registry, warnings);
                }
            }
            Step::Loop(s) => {
                check_steps_permissions(&s.body, category, registry, warnings);
            }
            _ => {}
        }
    }
}

/// Rough cost estimate: sum of step type base costs.
fn estimate_cost(spec: &WorkflowSpec) -> u32 {
    use super::parser::Step;
    let mut total = 0u32;
    for step in &spec.steps {
        total += step_cost(step);
    }
    total.min(spec.limits.max_tokens_per_run)
}

fn step_cost(step: &super::parser::Step) -> u32 {
    use super::parser::Step;
    match step {
        Step::Llm(_) => 2000, // typical Claude call
        Step::MemoryRecall(_) => 100,
        Step::MemoryStore(_) => 50,
        Step::ToolCall(_) => 500,
        Step::FileWrite(_) => 100,
        Step::CalendarAdd(_) => 50,
        Step::PhoneAction(_) => 100,
        Step::Shell(_) => 200,
        Step::Conditional(c) => {
            c.then.iter().map(step_cost).sum::<u32>()
                + c.else_.as_ref().map_or(0, |e| e.iter().map(step_cost).sum())
        }
        Step::Loop(l) => {
            // Assume ~5 iterations
            5 * l.body.iter().map(step_cost).sum::<u32>()
        }
        Step::UserConfirm(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_yaml_from_fenced() {
        let response = "Here is the workflow:\n```yaml\nname: test\nparent_category: daily\n```\n";
        let yaml = extract_yaml(response);
        assert!(yaml.starts_with("name: test"));
        assert!(!yaml.contains("```"));
    }

    #[test]
    fn extract_yaml_bare() {
        let response = "name: test\nparent_category: daily";
        let yaml = extract_yaml(response);
        assert_eq!(yaml, response);
    }

    #[test]
    fn extract_yaml_with_prose_before() {
        let response = "I'll create that for you:\n\nname: test\nparent_category: daily";
        let yaml = extract_yaml(response);
        assert!(yaml.starts_with("name: test"));
    }

    #[test]
    fn inject_default_limits_missing() {
        let yaml = "name: test\nparent_category: daily\nsteps:\n  - type: llm\n    id: a\n    model: m\n    user: u";
        let fixed = inject_default_limits(yaml);
        assert!(fixed.contains("limits:"));
        assert!(fixed.contains("max_tokens_per_run"));
    }

    #[test]
    fn inject_default_limits_preserves_existing() {
        let yaml = "name: test\nlimits:\n  max_tokens_per_run: 100";
        let fixed = inject_default_limits(yaml);
        assert_eq!(fixed, yaml);
    }

    #[test]
    fn cost_estimate_basic() {
        let spec = parse_spec(
            r#"
name: test
parent_category: daily
steps:
  - type: llm
    id: a
    model: test
    user: hi
  - type: memory_recall
    id: b
    query: q
limits:
  max_tokens_per_run: 10000
  max_llm_calls_per_run: 5
"#,
        )
        .unwrap();
        let cost = estimate_cost(&spec);
        assert!(cost >= 2000); // at least one LLM call
        assert!(cost <= 10000); // bounded by limit
    }

    #[test]
    fn build_system_prompt_includes_tools() {
        let reg = ToolRegistry::with_defaults();
        let prompt = build_system_prompt(&reg, &[]);
        assert!(prompt.contains("daily:"));
        assert!(prompt.contains("phone:"));
        assert!(prompt.contains("Schema requirements"));
    }

    #[test]
    fn build_system_prompt_includes_examples() {
        let reg = ToolRegistry::with_defaults();
        let examples = vec![
            "name: ex1\nparent_category: daily\nsteps: []".to_string(),
            "name: ex2\nparent_category: phone\nsteps: []".to_string(),
        ];
        let prompt = build_system_prompt(&reg, &examples);
        assert!(prompt.contains("Example 1"));
        assert!(prompt.contains("ex1"));
        assert!(prompt.contains("ex2"));
    }

    #[test]
    fn validate_injects_missing_limits() {
        let reg = ToolRegistry::with_defaults();
        let yaml = "name: test\nparent_category: daily\nsteps:\n  - type: llm\n    id: a\n    model: m\n    user: u";
        let (final_yaml, _spec, warnings) = validate_and_fix(yaml, &reg).unwrap();
        assert!(final_yaml.contains("limits:"));
        assert!(!warnings.is_empty());
    }

    #[test]
    fn validate_flags_disallowed_tool() {
        let reg = ToolRegistry::with_defaults();
        let yaml = r#"
name: bad
parent_category: daily
steps:
  - type: tool_call
    id: bad_call
    tool: shell
    args: {}
limits:
  max_tokens_per_run: 100
  max_llm_calls_per_run: 1
"#;
        let (_yaml, _spec, warnings) = validate_and_fix(yaml, &reg).unwrap();
        assert!(warnings.iter().any(|w| w.contains("shell") || w.contains("허용되지")));
    }
}
