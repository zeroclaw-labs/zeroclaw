// Workflow Execution Engine
//
// Executes a parsed WorkflowSpec with given inputs, tracking costs and
// enforcing limits. Each step is dispatched by type.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::parser::{step_id, Limits, Step, WorkflowSpec};
use super::registry::ToolRegistry;

/// Execution context threaded through all steps.
pub struct ExecContext {
    pub device_id: String,
    pub vars: HashMap<String, serde_json::Value>,
    pub cost: CostTracker,
    pub limits: Limits,
}

/// Tracks token and LLM call costs during execution.
#[derive(Default, Debug, Clone, Serialize)]
pub struct CostTracker {
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub llm_calls: u32,
}

impl CostTracker {
    /// Check if we've exceeded any budget limits.
    pub fn check(&self, limits: &Limits) -> Result<()> {
        if self.tokens_in + self.tokens_out > limits.max_tokens_per_run {
            bail!("token budget exceeded ({} > {})",
                self.tokens_in + self.tokens_out, limits.max_tokens_per_run);
        }
        if self.llm_calls > limits.max_llm_calls_per_run {
            bail!("LLM call budget exceeded ({} > {})",
                self.llm_calls, limits.max_llm_calls_per_run);
        }
        Ok(())
    }
}

/// Result of a workflow execution.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunResult {
    pub run_uuid: String,
    pub input_sha256: String,
    pub output_sha256: String,
    pub output: serde_json::Value,
    pub cost: CostTracker,
}

/// Execute a workflow spec with given inputs.
pub async fn execute(
    spec: &WorkflowSpec,
    inputs: serde_json::Value,
    _tools: &ToolRegistry,
    device_id: &str,
) -> Result<WorkflowRunResult> {
    let mut ctx = ExecContext {
        device_id: device_id.to_string(),
        vars: HashMap::new(),
        cost: CostTracker::default(),
        limits: spec.limits.clone(),
    };

    let run_uuid = uuid::Uuid::new_v4().to_string();
    let input_sha256 = sha256_hex(&serde_json::to_vec(&inputs)?);

    // Populate vars from inputs
    if let Some(obj) = inputs.as_object() {
        for (k, v) in obj {
            ctx.vars.insert(format!("input.{k}"), v.clone());
        }
    }

    // Execute steps sequentially
    for step in &spec.steps {
        execute_step(step, &mut ctx)
            .await
            .with_context(|| format!("step failed: {}", step_id(step)))?;
        ctx.cost.check(&ctx.limits)?;
    }

    let output = serde_json::to_value(&ctx.vars)?;
    let output_sha256 = sha256_hex(&serde_json::to_vec(&output)?);

    Ok(WorkflowRunResult {
        run_uuid,
        input_sha256,
        output_sha256,
        output,
        cost: ctx.cost,
    })
}

fn execute_step<'a>(
    step: &'a Step,
    ctx: &'a mut ExecContext,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move { execute_step_inner(step, ctx).await })
}

async fn execute_step_inner(step: &Step, ctx: &mut ExecContext) -> Result<()> {
    match step {
        Step::MemoryRecall(s) => {
            let query = render_template(&s.query, &ctx.vars)?;
            // In production, this would call hybrid_search.
            // Store placeholder results for now.
            ctx.vars.insert(
                format!("{}.results", s.id),
                serde_json::json!({ "query": query, "top_k": s.top_k }),
            );
        }
        Step::MemoryStore(s) => {
            let content = render_template(&s.content, &ctx.vars)?;
            ctx.vars.insert(
                format!("{}.stored", s.id),
                serde_json::Value::String(content),
            );
        }
        Step::Llm(s) => {
            ctx.cost.llm_calls += 1;
            let _system = s
                .system
                .as_deref()
                .map(|t| render_template(t, &ctx.vars))
                .transpose()?;
            let _user = s
                .user_template
                .as_deref()
                .or(s.user.as_deref())
                .map(|t| render_template(t, &ctx.vars))
                .transpose()?;
            // In production, this would call the provider.
            // Store placeholder output.
            if let Some(ref output_key) = s.output {
                ctx.vars.insert(
                    output_key.clone(),
                    serde_json::Value::String("[LLM response placeholder]".to_string()),
                );
            }
            ctx.vars.insert(
                format!("{}.output", s.id),
                serde_json::Value::String("[LLM response placeholder]".to_string()),
            );
        }
        Step::ToolCall(s) => {
            // In production: tools.check_permission + tools.invoke
            ctx.vars.insert(
                format!("{}.output", s.id),
                serde_json::json!({ "tool": s.tool, "status": "pending" }),
            );
        }
        Step::FileWrite(s) => {
            let _content = s
                .content_from
                .as_deref()
                .map(|t| render_template(t, &ctx.vars))
                .transpose()?;
            ctx.vars.insert(
                format!("{}.path", s.id),
                serde_json::Value::String(s.path.clone()),
            );
        }
        Step::CalendarAdd(s) => {
            ctx.vars.insert(
                format!("{}.added", s.id),
                serde_json::json!({ "title": s.title, "date": s.date }),
            );
        }
        Step::PhoneAction(s) => {
            ctx.vars.insert(
                format!("{}.result", s.id),
                serde_json::json!({ "action": s.action }),
            );
        }
        Step::Shell(s) => {
            let _command = render_template(&s.command, &ctx.vars)?;
            ctx.vars.insert(
                format!("{}.output", s.id),
                serde_json::Value::String("[shell output placeholder]".to_string()),
            );
        }
        Step::Conditional(s) => {
            // Simple condition evaluation: check if a referenced var is truthy
            let cond_val = render_template(&s.cond, &ctx.vars)?;
            let is_true = !cond_val.is_empty()
                && cond_val != "false"
                && cond_val != "0"
                && cond_val != "null";
            let branch = if is_true {
                &s.then
            } else {
                match &s.else_ {
                    Some(steps) => steps.as_slice(),
                    None => return Ok(()),
                }
            };
            for nested in branch {
                execute_step(nested, ctx).await?;
                ctx.cost.check(&ctx.limits)?;
            }
        }
        Step::Loop(s) => {
            let items_str = render_template(&s.over, &ctx.vars)?;
            // Try to parse as JSON array
            if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&items_str) {
                for item in &items {
                    ctx.vars.insert(s.as_var.clone(), item.clone());
                    for nested in &s.body {
                        execute_step(nested, ctx).await?;
                        ctx.cost.check(&ctx.limits)?;
                    }
                }
            }
        }
        Step::UserConfirm(_s) => {
            // In production: send confirmation to Tauri IPC, wait for response
            // For now, auto-approve
        }
    }
    Ok(())
}

/// Simple `{{var}}` template rendering.
/// Fails if a referenced variable is not found in the context.
pub fn render_template(
    tpl: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> Result<String> {
    let mut out = String::with_capacity(tpl.len());
    // Manual left-to-right scan rather than repeated `out.find("{{")`. The
    // old implementation rescanned from the start on every iteration and,
    // worse, replaced unresolved `{{key}}` with `{{unresolved:key}}` — which
    // still contains `{{`, triggering an infinite loop on any missing var.
    // A forward scan with a running cursor is O(n) and immune to the
    // self-reinjection problem since we never re-examine what we've
    // already written.
    let mut cursor = 0usize;
    let bytes = tpl.as_bytes();
    while cursor < tpl.len() {
        let Some(rel_start) = tpl[cursor..].find("{{") else {
            out.push_str(&tpl[cursor..]);
            break;
        };
        let start = cursor + rel_start;
        out.push_str(&tpl[cursor..start]);
        let end = tpl[start + 2..]
            .find("}}")
            .map(|e| start + 2 + e + 2)
            .context("unclosed {{ in template")?;
        let key = tpl[start + 2..end - 2].trim();
        let replacement = match vars.get(key) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => {
                // Allow unresolved templates to pass through (callers may
                // resolve them later in a second pass). The marker uses
                // square brackets so it doesn't re-trigger the `{{` scan.
                format!("[[unresolved:{key}]]")
            }
        };
        out.push_str(&replacement);
        cursor = end;
    }
    // `bytes` only participates in the debug_assert below — keep it alive.
    debug_assert_eq!(bytes.len(), tpl.len());
    Ok(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_tracker_within_budget() {
        let limits = Limits {
            max_tokens_per_run: 1000,
            max_llm_calls_per_run: 10,
            max_runtime_sec: None,
        };
        let cost = CostTracker {
            tokens_in: 300,
            tokens_out: 200,
            llm_calls: 5,
        };
        assert!(cost.check(&limits).is_ok());
    }

    #[test]
    fn cost_tracker_token_exceeded() {
        let limits = Limits {
            max_tokens_per_run: 100,
            max_llm_calls_per_run: 10,
            max_runtime_sec: None,
        };
        let cost = CostTracker {
            tokens_in: 80,
            tokens_out: 80,
            llm_calls: 1,
        };
        assert!(cost.check(&limits).is_err());
    }

    #[test]
    fn cost_tracker_llm_calls_exceeded() {
        let limits = Limits {
            max_tokens_per_run: 10000,
            max_llm_calls_per_run: 2,
            max_runtime_sec: None,
        };
        let cost = CostTracker {
            tokens_in: 0,
            tokens_out: 0,
            llm_calls: 3,
        };
        assert!(cost.check(&limits).is_err());
    }

    #[test]
    fn render_template_basic() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), serde_json::json!("Alice"));
        let result = render_template("Hello {{name}}!", &vars).unwrap();
        assert_eq!(result, "Hello Alice!");
    }

    #[test]
    fn render_template_multiple_vars() {
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), serde_json::json!("X"));
        vars.insert("b".to_string(), serde_json::json!("Y"));
        let result = render_template("{{a}} and {{b}}", &vars).unwrap();
        assert_eq!(result, "X and Y");
    }

    #[test]
    fn render_template_number() {
        let mut vars = HashMap::new();
        vars.insert("count".to_string(), serde_json::json!(42));
        let result = render_template("items: {{count}}", &vars).unwrap();
        assert_eq!(result, "items: 42");
    }

    #[test]
    fn render_template_unresolved() {
        let vars = HashMap::new();
        let result = render_template("{{missing}}", &vars).unwrap();
        assert!(result.contains("unresolved:missing"));
    }

    #[test]
    fn sha256_deterministic() {
        let a = sha256_hex(b"hello");
        let b = sha256_hex(b"hello");
        assert_eq!(a, b);
        assert_ne!(sha256_hex(b"hello"), sha256_hex(b"world"));
    }

    #[tokio::test]
    async fn execute_simple_workflow() {
        let spec = crate::workflow::parse_spec(
            r#"
name: "test"
parent_category: "daily"
steps:
  - type: memory_recall
    id: fetch
    query: "test query"
    top_k: 5
  - type: llm
    id: draft
    model: test-model
    user: "summarize"
limits:
  max_tokens_per_run: 10000
  max_llm_calls_per_run: 5
"#,
        )
        .unwrap();

        let tools = ToolRegistry::new();
        let inputs = serde_json::json!({"query": "test"});
        let result = execute(&spec, inputs, &tools, "dev1").await.unwrap();
        assert!(!result.run_uuid.is_empty());
        assert!(!result.input_sha256.is_empty());
        assert_eq!(result.cost.llm_calls, 1);
    }
}
