// Workflow YAML Parser — YAML text → WorkflowSpec IR
//
// Validates step ID uniqueness, cost limits, and basic structure.
// JSON Schema validation (06_workflow_schema.json) is separate.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level workflow specification (parsed from YAML).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkflowSpec {
    pub name: String,
    pub parent_category: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub inputs: Vec<InputDef>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub post_hooks: Vec<PostHook>,
    pub limits: Limits,
}

/// Input parameter definition.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InputDef {
    pub name: String,
    #[serde(rename = "type")]
    pub input_type: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub autofill_from: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Cost and resource limits (mandatory).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Limits {
    pub max_tokens_per_run: u32,
    pub max_llm_calls_per_run: u32,
    #[serde(default)]
    pub max_runtime_sec: Option<u32>,
}

/// Post-execution hook.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostHook {
    #[serde(rename = "type")]
    pub hook_type: String,
    pub args: serde_json::Value,
}

/// A single workflow step (tagged union by "type" field).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    MemoryRecall(MemoryRecallStep),
    MemoryStore(MemoryStoreStep),
    Llm(LlmStep),
    ToolCall(ToolCallStep),
    FileWrite(FileWriteStep),
    CalendarAdd(CalendarAddStep),
    PhoneAction(PhoneActionStep),
    Shell(ShellStep),
    Conditional(ConditionalStep),
    Loop(LoopStep),
    UserConfirm(UserConfirmStep),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryRecallStep {
    pub id: String,
    pub query: String,
    #[serde(default = "default_rrf")]
    pub search_mode: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}
fn default_rrf() -> String { "rrf".to_string() }
fn default_top_k() -> usize { 20 }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryStoreStep {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub timeline_event_type: Option<String>,
    #[serde(default)]
    pub link_to_ontology: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmStep {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub user_template: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallStep {
    pub id: String,
    pub tool: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileWriteStep {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub content_from: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CalendarAddStep {
    pub id: String,
    pub title: String,
    pub date: String,
    #[serde(default)]
    pub duration_min: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhoneActionStep {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShellStep {
    pub id: String,
    pub command: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConditionalStep {
    pub id: String,
    pub cond: String,
    pub then: Vec<Step>,
    #[serde(default, rename = "else")]
    pub else_: Option<Vec<Step>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoopStep {
    pub id: String,
    pub over: String,
    #[serde(rename = "as")]
    pub as_var: String,
    pub body: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserConfirmStep {
    pub id: String,
    pub message: String,
    #[serde(default)]
    pub timeout_sec: Option<u32>,
}

/// Get the step ID from any step variant.
pub fn step_id(step: &Step) -> &str {
    match step {
        Step::MemoryRecall(s) => &s.id,
        Step::MemoryStore(s) => &s.id,
        Step::Llm(s) => &s.id,
        Step::ToolCall(s) => &s.id,
        Step::FileWrite(s) => &s.id,
        Step::CalendarAdd(s) => &s.id,
        Step::PhoneAction(s) => &s.id,
        Step::Shell(s) => &s.id,
        Step::Conditional(s) => &s.id,
        Step::Loop(s) => &s.id,
        Step::UserConfirm(s) => &s.id,
    }
}

/// Parse and validate a YAML workflow spec.
pub fn parse_spec(yaml: &str) -> Result<WorkflowSpec> {
    let spec: WorkflowSpec =
        serde_yaml::from_str(yaml).context("failed to parse workflow YAML")?;
    validate_spec(&spec)?;
    Ok(spec)
}

fn validate_spec(spec: &WorkflowSpec) -> Result<()> {
    // 1) Step ID uniqueness
    let mut ids = std::collections::HashSet::new();
    collect_step_ids(&spec.steps, &mut ids)?;

    // 2) Cost limits must be positive
    if spec.limits.max_tokens_per_run == 0 || spec.limits.max_llm_calls_per_run == 0 {
        bail!("limits.max_tokens_per_run and max_llm_calls_per_run must be > 0");
    }

    // 3) Name must not be empty
    if spec.name.trim().is_empty() {
        bail!("workflow name must not be empty");
    }

    // 4) At least one step
    if spec.steps.is_empty() {
        bail!("workflow must have at least one step");
    }

    Ok(())
}

fn collect_step_ids<'a>(
    steps: &'a [Step],
    ids: &mut std::collections::HashSet<&'a str>,
) -> Result<()> {
    for step in steps {
        let id = step_id(step);
        if !ids.insert(id) {
            bail!("duplicate step id: {id}");
        }
        // Recurse into nested steps
        match step {
            Step::Conditional(s) => {
                collect_step_ids(&s.then, ids)?;
                if let Some(ref else_steps) = s.else_ {
                    collect_step_ids(else_steps, ids)?;
                }
            }
            Step::Loop(s) => {
                collect_step_ids(&s.body, ids)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
name: "상담일지 작성"
parent_category: "document"
inputs:
  - name: client_name
    type: string
    required: true
steps:
  - type: memory_recall
    id: fetch
    query: "{{input.client_name}} 상담 내역"
    search_mode: rrf
    top_k: 10
  - type: llm
    id: draft
    model: claude-opus-4-6
    system: "법률 문서 보조"
    user_template: "의뢰인: {{input.client_name}}\n자료: {{fetch.results}}"
limits:
  max_tokens_per_run: 30000
  max_llm_calls_per_run: 5
"#;

    #[test]
    fn parses_sample() {
        let spec = parse_spec(SAMPLE_YAML).expect("parse");
        assert_eq!(spec.name, "상담일지 작성");
        assert_eq!(spec.steps.len(), 2);
        assert_eq!(spec.parent_category, "document");
        assert_eq!(spec.inputs.len(), 1);
        assert!(spec.inputs[0].required);
    }

    #[test]
    fn rejects_duplicate_step_ids() {
        let yaml = SAMPLE_YAML.replace("id: draft", "id: fetch");
        assert!(parse_spec(&yaml).is_err());
    }

    #[test]
    fn rejects_zero_token_limit() {
        let yaml = SAMPLE_YAML.replace("max_tokens_per_run: 30000", "max_tokens_per_run: 0");
        assert!(parse_spec(&yaml).is_err());
    }

    #[test]
    fn rejects_empty_name() {
        let yaml = SAMPLE_YAML.replace("name: \"상담일지 작성\"", "name: \"\"");
        assert!(parse_spec(&yaml).is_err());
    }

    #[test]
    fn rejects_empty_steps() {
        let yaml = r#"
name: "test"
parent_category: "daily"
steps: []
limits:
  max_tokens_per_run: 1000
  max_llm_calls_per_run: 5
"#;
        assert!(parse_spec(yaml).is_err());
    }

    #[test]
    fn step_id_extraction() {
        let spec = parse_spec(SAMPLE_YAML).unwrap();
        assert_eq!(step_id(&spec.steps[0]), "fetch");
        assert_eq!(step_id(&spec.steps[1]), "draft");
    }

    #[test]
    fn conditional_step_parses() {
        let yaml = r#"
name: "conditional test"
parent_category: "daily"
steps:
  - type: conditional
    id: check
    cond: "{{input.value}} > 10"
    then:
      - type: llm
        id: high
        model: claude-haiku-4-5-20251001
        user: "high value"
    else:
      - type: llm
        id: low
        model: claude-haiku-4-5-20251001
        user: "low value"
limits:
  max_tokens_per_run: 1000
  max_llm_calls_per_run: 5
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.steps.len(), 1);
    }

    #[test]
    fn loop_step_parses() {
        let yaml = r#"
name: "loop test"
parent_category: "daily"
steps:
  - type: loop
    id: iter
    over: "{{items}}"
    as: item
    body:
      - type: llm
        id: process
        model: claude-haiku-4-5-20251001
        user: "process {{item}}"
limits:
  max_tokens_per_run: 1000
  max_llm_calls_per_run: 5
"#;
        let spec = parse_spec(yaml).unwrap();
        assert_eq!(spec.steps.len(), 1);
    }

    #[test]
    fn nested_duplicate_ids_rejected() {
        let yaml = r#"
name: "nested dup"
parent_category: "daily"
steps:
  - type: conditional
    id: check
    cond: "true"
    then:
      - type: llm
        id: check
        model: test
        user: "dup"
limits:
  max_tokens_per_run: 1000
  max_llm_calls_per_run: 5
"#;
        assert!(parse_spec(yaml).is_err());
    }
}
