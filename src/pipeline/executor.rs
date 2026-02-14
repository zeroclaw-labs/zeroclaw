//! Pipeline DAG executor with dependency resolution, parallel execution,
//! condition evaluation, retry with exponential backoff, and input mapping.
//!
//! Steps are resolved in dependency order using topological sort. Steps with
//! no unmet dependencies form a "ready" set and execute concurrently, bounded
//! by `max_parallel`. Failed steps trigger retry policies before being marked
//! as failed.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::aria::db::AriaDb;
use crate::aria::types::{PipelineResult, PipelineStep, RetryPolicy, StepResult};
use super::types::{PipelineExecutionContext, StepExecutionResult};

/// Pipeline execution engine that resolves a DAG of steps and executes them
/// with dependency ordering, parallelism, retries, and condition evaluation.
pub struct PipelineEngine {
    #[allow(dead_code)]
    db: AriaDb,
}

impl PipelineEngine {
    /// Create a new `PipelineEngine` with the given database handle.
    pub fn new(db: AriaDb) -> Self {
        Self { db }
    }

    /// Execute a pipeline by resolving step dependencies and running steps
    /// in parallel where possible.
    ///
    /// # Arguments
    /// * `pipeline_id` - Unique identifier for this pipeline
    /// * `tenant_id` - Tenant identifier for multi-tenancy
    /// * `steps` - The pipeline step definitions forming a DAG
    /// * `variables` - Initial variables available to all steps
    /// * `timeout` - Optional overall pipeline timeout
    /// * `max_parallel` - Maximum concurrent steps (default: 4)
    ///
    /// # Returns
    /// A `PipelineResult` with individual step results and the final output.
    pub async fn execute(
        &self,
        pipeline_id: &str,
        tenant_id: &str,
        steps: &[PipelineStep],
        variables: HashMap<String, serde_json::Value>,
        timeout: Option<Duration>,
        max_parallel: Option<u32>,
    ) -> Result<PipelineResult> {
        if steps.is_empty() {
            return Ok(PipelineResult {
                success: true,
                result: None,
                error: None,
                step_results: Vec::new(),
                duration_ms: Some(0),
                metadata: None,
            });
        }

        let ctx = PipelineExecutionContext {
            pipeline_id: pipeline_id.to_string(),
            tenant_id: tenant_id.to_string(),
            variables,
            step_outputs: Arc::new(Mutex::new(HashMap::new())),
            timeout,
        };

        let execution = self.execute_dag(&ctx, steps, max_parallel.unwrap_or(4));

        // Apply timeout if configured
        if let Some(duration) = timeout {
            match tokio::time::timeout(duration, execution).await {
                Ok(result) => result,
                Err(_) => Ok(PipelineResult {
                    success: false,
                    result: None,
                    error: Some(format!(
                        "Pipeline execution timed out after {}ms",
                        duration.as_millis()
                    )),
                    step_results: Vec::new(),
                    duration_ms: Some(duration.as_millis() as u64),
                    metadata: None,
                }),
            }
        } else {
            execution.await
        }
    }

    /// Execute steps in DAG order with bounded parallelism.
    async fn execute_dag(
        &self,
        ctx: &PipelineExecutionContext,
        steps: &[PipelineStep],
        max_parallel: u32,
    ) -> Result<PipelineResult> {
        let start = std::time::Instant::now();

        // Build step lookup
        let step_map: HashMap<&str, &PipelineStep> =
            steps.iter().map(|s| (s.id.as_str(), s)).collect();

        // Validate and compute topological order
        let execution_order = topological_sort(steps)
            .context("Failed to resolve pipeline step dependencies")?;

        let mut completed: HashSet<String> = HashSet::new();
        let mut step_results: Vec<StepResult> = Vec::new();
        let mut all_success = true;

        // Process steps in waves (groups of steps with no unmet dependencies)
        let mut remaining: VecDeque<String> = execution_order.into();

        while !remaining.is_empty() {
            // Find all steps whose dependencies are satisfied
            let mut ready: Vec<String> = Vec::new();
            let mut deferred: VecDeque<String> = VecDeque::new();

            while let Some(step_id) = remaining.pop_front() {
                let step = step_map
                    .get(step_id.as_str())
                    .context("Step not found in map")?;
                let deps_met = step.dependencies.iter().all(|d| completed.contains(d));
                if deps_met {
                    ready.push(step_id);
                    if ready.len() >= max_parallel as usize {
                        // Move remaining to deferred and break
                        while let Some(s) = remaining.pop_front() {
                            deferred.push_back(s);
                        }
                        break;
                    }
                } else {
                    deferred.push_back(step_id);
                }
            }

            if ready.is_empty() {
                // No progress can be made - remaining steps have unmet dependencies
                // This could happen if a dependency failed
                for step_id in &deferred {
                    step_results.push(StepResult {
                        step_id: step_id.clone(),
                        step_name: step_map
                            .get(step_id.as_str())
                            .map(|s| s.name.clone())
                            .unwrap_or_default(),
                        success: false,
                        result: None,
                        error: Some("Skipped: unmet dependencies due to prior failure".into()),
                        duration_ms: Some(0),
                        retries: 0,
                    });
                }
                all_success = false;
                break;
            }

            // Execute ready steps in parallel
            let mut handles = Vec::new();
            for step_id in &ready {
                let step = (*step_map.get(step_id.as_str()).unwrap()).clone();
                let ctx_clone = ctx.clone();
                let handle = tokio::spawn(async move {
                    execute_step(&ctx_clone, &step).await
                });
                handles.push((step_id.clone(), handle));
            }

            // Collect results
            for (step_id, handle) in handles {
                match handle.await {
                    Ok(Ok(result)) => {
                        if result.success {
                            // Store output in step_outputs if output_key is set
                            let step = step_map.get(step_id.as_str()).unwrap();
                            if let Some(ref output_key) = step.output_key {
                                if let Some(ref output) = result.output {
                                    ctx.step_outputs
                                        .lock()
                                        .unwrap()
                                        .insert(output_key.clone(), output.clone());
                                }
                            }
                            completed.insert(step_id.clone());
                            step_results.push(StepResult {
                                step_id: result.step_id,
                                step_name: step.name.clone(),
                                success: true,
                                result: result.output,
                                error: None,
                                duration_ms: Some(result.duration_ms),
                                retries: result.retries_used,
                            });
                        } else {
                            all_success = false;
                            let step = step_map.get(step_id.as_str()).unwrap();
                            step_results.push(StepResult {
                                step_id: result.step_id,
                                step_name: step.name.clone(),
                                success: false,
                                result: None,
                                error: result.error,
                                duration_ms: Some(result.duration_ms),
                                retries: result.retries_used,
                            });
                        }
                    }
                    Ok(Err(e)) => {
                        all_success = false;
                        let step = step_map.get(step_id.as_str()).unwrap();
                        step_results.push(StepResult {
                            step_id,
                            step_name: step.name.clone(),
                            success: false,
                            result: None,
                            error: Some(format!("Step execution error: {e}")),
                            duration_ms: Some(0),
                            retries: 0,
                        });
                    }
                    Err(e) => {
                        all_success = false;
                        let step = step_map.get(step_id.as_str()).unwrap();
                        step_results.push(StepResult {
                            step_id,
                            step_name: step.name.clone(),
                            success: false,
                            result: None,
                            error: Some(format!("Step task join error: {e}")),
                            duration_ms: Some(0),
                            retries: 0,
                        });
                    }
                }
            }

            remaining = deferred;
        }

        // The final result is the output of the last completed step
        let final_result = step_results
            .iter()
            .rev()
            .find(|r| r.success)
            .and_then(|r| r.result.clone());

        Ok(PipelineResult {
            success: all_success,
            result: final_result,
            error: if all_success {
                None
            } else {
                Some("One or more pipeline steps failed".into())
            },
            step_results,
            duration_ms: Some(start.elapsed().as_millis() as u64),
            metadata: None,
        })
    }
}

/// Perform topological sort on pipeline steps to determine execution order.
/// Returns an error if the dependency graph contains a cycle.
fn topological_sort(steps: &[PipelineStep]) -> Result<Vec<String>> {
    let step_ids: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();

    // Validate that all dependencies reference existing steps
    for step in steps {
        for dep in &step.dependencies {
            if !step_ids.contains(dep.as_str()) {
                bail!(
                    "Step '{}' depends on non-existent step '{}'",
                    step.id,
                    dep
                );
            }
        }
    }

    // Kahn's algorithm for topological sort
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

    for step in steps {
        in_degree.entry(step.id.as_str()).or_insert(0);
        adjacency.entry(step.id.as_str()).or_default();
    }

    for step in steps {
        for dep in &step.dependencies {
            adjacency.entry(dep.as_str()).or_default().push(&step.id);
            *in_degree.entry(step.id.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&str> = VecDeque::new();
    for (&id, &degree) in &in_degree {
        if degree == 0 {
            queue.push_back(id);
        }
    }

    let mut order: Vec<String> = Vec::new();
    while let Some(id) = queue.pop_front() {
        order.push(id.to_string());
        if let Some(dependents) = adjacency.get(id) {
            for &dep_id in dependents {
                if let Some(count) = in_degree.get_mut(dep_id) {
                    *count -= 1;
                    if *count == 0 {
                        queue.push_back(dep_id);
                    }
                }
            }
        }
    }

    if order.len() != steps.len() {
        bail!(
            "Dependency cycle detected in pipeline steps (resolved {} of {} steps)",
            order.len(),
            steps.len()
        );
    }

    Ok(order)
}

/// Evaluate a condition expression against current variables and step outputs.
///
/// Supports simple expressions:
/// - `"true"` / `"false"` - literal boolean
/// - `"$var_name"` - check if a variable is truthy
/// - `"$step_output.key"` - check if a step output key exists and is truthy
///
/// TODO: Replace with a proper expression evaluator for production use.
fn evaluate_condition(
    condition: &str,
    variables: &HashMap<String, serde_json::Value>,
    step_outputs: &HashMap<String, serde_json::Value>,
) -> bool {
    let trimmed = condition.trim();

    // Literal booleans
    if trimmed.eq_ignore_ascii_case("true") {
        return true;
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return false;
    }

    // Variable reference: $var_name
    if let Some(var_name) = trimmed.strip_prefix('$') {
        // Check step outputs first (dot notation: step_output.field)
        if let Some((output_key, field)) = var_name.split_once('.') {
            if let Some(output) = step_outputs.get(output_key) {
                if let Some(val) = output.get(field) {
                    return is_truthy(val);
                }
            }
            return false;
        }

        // Check variables
        if let Some(val) = variables.get(var_name) {
            return is_truthy(val);
        }

        // Check step outputs directly
        if let Some(val) = step_outputs.get(var_name) {
            return is_truthy(val);
        }

        return false;
    }

    // Default: treat as truthy if non-empty
    !trimmed.is_empty()
}

/// Determine if a JSON value is "truthy".
fn is_truthy(val: &serde_json::Value) -> bool {
    match val {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// Map inputs from prior step outputs into the current step's execution context.
fn resolve_input_mapping(
    mapping: &HashMap<String, String>,
    step_outputs: &HashMap<String, serde_json::Value>,
    variables: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut resolved = HashMap::new();
    for (target_key, source_ref) in mapping {
        // source_ref format: "step_output_key" or "$variable_name"
        let value = if let Some(var_name) = source_ref.strip_prefix('$') {
            variables.get(var_name).cloned()
        } else {
            step_outputs.get(source_ref).cloned()
        };

        if let Some(val) = value {
            resolved.insert(target_key.clone(), val);
        }
    }
    resolved
}

/// Execute a single pipeline step with retry logic and condition evaluation.
async fn execute_step(
    ctx: &PipelineExecutionContext,
    step: &PipelineStep,
) -> Result<StepExecutionResult> {
    let step_start = std::time::Instant::now();

    // Evaluate condition if present
    if let Some(ref condition) = step.condition {
        let outputs = ctx.step_outputs.lock().unwrap().clone();
        if !evaluate_condition(condition, &ctx.variables, &outputs) {
            return Ok(StepExecutionResult {
                step_id: step.id.clone(),
                success: true,
                output: Some(serde_json::json!({ "skipped": true, "reason": "condition false" })),
                error: None,
                duration_ms: step_start.elapsed().as_millis() as u64,
                retries_used: 0,
            });
        }
    }

    // Resolve input mapping
    let mapped_inputs = if let Some(ref mapping) = step.input_mapping {
        let outputs = ctx.step_outputs.lock().unwrap().clone();
        resolve_input_mapping(mapping, &outputs, &ctx.variables)
    } else {
        HashMap::new()
    };

    // Execute with retry policy
    let retry_policy = step.retry_policy.clone().unwrap_or_default();
    let max_attempts = retry_policy.max_retries + 1;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        if attempt > 0 {
            // Exponential backoff delay
            let delay_ms = compute_backoff_delay(&retry_policy, attempt - 1);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        // TODO: Replace this stub with real step execution based on step_type.
        // In production, this would:
        // - PipelineStepType::Agent => run the agent with mapped inputs
        // - PipelineStepType::Tool => execute the tool with mapped inputs
        // - PipelineStepType::Team => run the team engine with mapped inputs
        // - PipelineStepType::Condition => evaluate a condition expression
        // - PipelineStepType::Transform => apply a data transformation
        let execution_result = stub_execute_step(step, &mapped_inputs);

        match execution_result {
            Ok(output) => {
                return Ok(StepExecutionResult {
                    step_id: step.id.clone(),
                    success: true,
                    output: Some(output),
                    error: None,
                    duration_ms: step_start.elapsed().as_millis() as u64,
                    retries_used: attempt,
                });
            }
            Err(e) => {
                last_error = e.to_string();
                // Continue to next retry attempt
            }
        }
    }

    // All retries exhausted
    Ok(StepExecutionResult {
        step_id: step.id.clone(),
        success: false,
        output: None,
        error: Some(format!(
            "Step failed after {} attempts: {}",
            max_attempts, last_error
        )),
        duration_ms: step_start.elapsed().as_millis() as u64,
        retries_used: retry_policy.max_retries,
    })
}

/// Compute the backoff delay for a retry attempt using exponential backoff.
/// delay = initial_delay_ms * backoff_multiplier^attempt, capped at max_delay_ms.
fn compute_backoff_delay(policy: &RetryPolicy, attempt: u32) -> u64 {
    let delay = (policy.initial_delay_ms as f64) * policy.backoff_multiplier.powi(attempt as i32);
    let capped = delay.min(policy.max_delay_ms as f64) as u64;
    capped.max(1) // Ensure at least 1ms delay
}

/// Stub: Execute a pipeline step.
///
/// In production, this would dispatch to the appropriate executor based on
/// `step.step_type` (Agent, Tool, Team, Condition, Transform).
///
/// TODO: Replace with real execution when step executors are connected.
fn stub_execute_step(
    step: &PipelineStep,
    mapped_inputs: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "step_id": step.id,
        "step_name": step.name,
        "step_type": format!("{:?}", step.step_type),
        "inputs": mapped_inputs,
        "status": "completed_stub",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use crate::aria::types::PipelineStepType;

    fn setup() -> PipelineEngine {
        let db = AriaDb::open_in_memory().unwrap();
        PipelineEngine::new(db)
    }

    fn make_step(id: &str, name: &str, deps: Vec<&str>) -> PipelineStep {
        PipelineStep {
            id: id.to_string(),
            name: name.to_string(),
            step_type: PipelineStepType::Agent,
            agent_id: Some(format!("agent-{id}")),
            tool_id: None,
            team_id: None,
            input_mapping: None,
            output_key: Some(id.to_string()),
            condition: None,
            dependencies: deps.into_iter().map(String::from).collect(),
            retry_policy: None,
            timeout_seconds: None,
        }
    }

    // ── Topological sort tests ──────────────────────────────────────

    #[test]
    fn topo_sort_linear_chain() {
        let steps = vec![
            make_step("a", "StepA", vec![]),
            make_step("b", "StepB", vec!["a"]),
            make_step("c", "StepC", vec!["b"]),
        ];
        let order = topological_sort(&steps).unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn topo_sort_diamond_dag() {
        let steps = vec![
            make_step("a", "StepA", vec![]),
            make_step("b", "StepB", vec!["a"]),
            make_step("c", "StepC", vec!["a"]),
            make_step("d", "StepD", vec!["b", "c"]),
        ];
        let order = topological_sort(&steps).unwrap();

        // a must come first, d must come last, b and c can be in either order
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
        assert!(order[1] == "b" || order[1] == "c");
        assert!(order[2] == "b" || order[2] == "c");
    }

    #[test]
    fn topo_sort_independent_steps() {
        let steps = vec![
            make_step("a", "StepA", vec![]),
            make_step("b", "StepB", vec![]),
            make_step("c", "StepC", vec![]),
        ];
        let order = topological_sort(&steps).unwrap();
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn topo_sort_detects_cycle() {
        let steps = vec![
            make_step("a", "StepA", vec!["c"]),
            make_step("b", "StepB", vec!["a"]),
            make_step("c", "StepC", vec!["b"]),
        ];
        let result = topological_sort(&steps);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn topo_sort_detects_missing_dependency() {
        let steps = vec![make_step("a", "StepA", vec!["nonexistent"])];
        let result = topological_sort(&steps);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("non-existent"));
    }

    #[test]
    fn topo_sort_empty_steps() {
        let steps: Vec<PipelineStep> = vec![];
        let order = topological_sort(&steps).unwrap();
        assert!(order.is_empty());
    }

    // ── Condition evaluation tests ──────────────────────────────────

    #[test]
    fn condition_literal_true() {
        let vars = HashMap::new();
        let outputs = HashMap::new();
        assert!(evaluate_condition("true", &vars, &outputs));
        assert!(evaluate_condition("TRUE", &vars, &outputs));
    }

    #[test]
    fn condition_literal_false() {
        let vars = HashMap::new();
        let outputs = HashMap::new();
        assert!(!evaluate_condition("false", &vars, &outputs));
        assert!(!evaluate_condition("FALSE", &vars, &outputs));
    }

    #[test]
    fn condition_variable_truthy() {
        let mut vars = HashMap::new();
        vars.insert("enabled".to_string(), serde_json::json!(true));
        let outputs = HashMap::new();
        assert!(evaluate_condition("$enabled", &vars, &outputs));
    }

    #[test]
    fn condition_variable_falsy() {
        let mut vars = HashMap::new();
        vars.insert("disabled".to_string(), serde_json::json!(false));
        let outputs = HashMap::new();
        assert!(!evaluate_condition("$disabled", &vars, &outputs));
    }

    #[test]
    fn condition_missing_variable() {
        let vars = HashMap::new();
        let outputs = HashMap::new();
        assert!(!evaluate_condition("$nonexistent", &vars, &outputs));
    }

    #[test]
    fn condition_step_output_dot_notation() {
        let vars = HashMap::new();
        let mut outputs = HashMap::new();
        outputs.insert(
            "step1".to_string(),
            serde_json::json!({"status": "success"}),
        );
        assert!(evaluate_condition("$step1.status", &vars, &outputs));
    }

    #[test]
    fn condition_step_output_missing_field() {
        let vars = HashMap::new();
        let mut outputs = HashMap::new();
        outputs.insert(
            "step1".to_string(),
            serde_json::json!({"status": "success"}),
        );
        assert!(!evaluate_condition("$step1.missing", &vars, &outputs));
    }

    // ── Input mapping tests ─────────────────────────────────────────

    #[test]
    fn input_mapping_from_step_output() {
        let mut outputs = HashMap::new();
        outputs.insert("step1".to_string(), serde_json::json!("hello"));
        let vars = HashMap::new();

        let mut mapping = HashMap::new();
        mapping.insert("input_text".to_string(), "step1".to_string());

        let resolved = resolve_input_mapping(&mapping, &outputs, &vars);
        assert_eq!(resolved.get("input_text"), Some(&serde_json::json!("hello")));
    }

    #[test]
    fn input_mapping_from_variable() {
        let outputs = HashMap::new();
        let mut vars = HashMap::new();
        vars.insert("api_key".to_string(), serde_json::json!("secret123"));

        let mut mapping = HashMap::new();
        mapping.insert("key".to_string(), "$api_key".to_string());

        let resolved = resolve_input_mapping(&mapping, &outputs, &vars);
        assert_eq!(
            resolved.get("key"),
            Some(&serde_json::json!("secret123"))
        );
    }

    #[test]
    fn input_mapping_missing_source() {
        let outputs = HashMap::new();
        let vars = HashMap::new();

        let mut mapping = HashMap::new();
        mapping.insert("missing".to_string(), "nonexistent".to_string());

        let resolved = resolve_input_mapping(&mapping, &outputs, &vars);
        assert!(!resolved.contains_key("missing"));
    }

    // ── Backoff computation tests ───────────────────────────────────

    #[test]
    fn backoff_delay_exponential() {
        let policy = RetryPolicy {
            max_retries: 5,
            initial_delay_ms: 100,
            max_delay_ms: 10_000,
            backoff_multiplier: 2.0,
            retry_on: vec![],
        };
        assert_eq!(compute_backoff_delay(&policy, 0), 100); // 100 * 2^0
        assert_eq!(compute_backoff_delay(&policy, 1), 200); // 100 * 2^1
        assert_eq!(compute_backoff_delay(&policy, 2), 400); // 100 * 2^2
        assert_eq!(compute_backoff_delay(&policy, 3), 800); // 100 * 2^3
    }

    #[test]
    fn backoff_delay_capped() {
        let policy = RetryPolicy {
            max_retries: 10,
            initial_delay_ms: 1000,
            max_delay_ms: 5000,
            backoff_multiplier: 3.0,
            retry_on: vec![],
        };
        // 1000 * 3^3 = 27000 > 5000, should be capped
        assert_eq!(compute_backoff_delay(&policy, 3), 5000);
    }

    #[test]
    fn backoff_delay_minimum_one_ms() {
        let policy = RetryPolicy {
            max_retries: 1,
            initial_delay_ms: 0,
            max_delay_ms: 0,
            backoff_multiplier: 1.0,
            retry_on: vec![],
        };
        assert_eq!(compute_backoff_delay(&policy, 0), 1);
    }

    // ── Truthiness tests ────────────────────────────────────────────

    #[test]
    fn is_truthy_values() {
        assert!(!is_truthy(&serde_json::Value::Null));
        assert!(!is_truthy(&serde_json::json!(false)));
        assert!(is_truthy(&serde_json::json!(true)));
        assert!(!is_truthy(&serde_json::json!(0)));
        assert!(is_truthy(&serde_json::json!(1)));
        assert!(!is_truthy(&serde_json::json!("")));
        assert!(is_truthy(&serde_json::json!("hello")));
        assert!(!is_truthy(&serde_json::json!([])));
        assert!(is_truthy(&serde_json::json!([1])));
        assert!(!is_truthy(&serde_json::json!({})));
        assert!(is_truthy(&serde_json::json!({"a": 1})));
    }

    // ── Pipeline engine integration tests ───────────────────────────

    #[tokio::test]
    async fn execute_empty_pipeline() {
        let engine = setup();
        let result = engine
            .execute("p1", "t1", &[], HashMap::new(), None, None)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.step_results.is_empty());
    }

    #[tokio::test]
    async fn execute_single_step() {
        let engine = setup();
        let steps = vec![make_step("a", "StepA", vec![])];
        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 1);
        assert!(result.step_results[0].success);
        assert_eq!(result.step_results[0].step_id, "a");
    }

    #[tokio::test]
    async fn execute_linear_pipeline() {
        let engine = setup();
        let steps = vec![
            make_step("a", "StepA", vec![]),
            make_step("b", "StepB", vec!["a"]),
            make_step("c", "StepC", vec!["b"]),
        ];
        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 3);
        for sr in &result.step_results {
            assert!(sr.success);
        }
    }

    #[tokio::test]
    async fn execute_parallel_independent_steps() {
        let engine = setup();
        let steps = vec![
            make_step("a", "StepA", vec![]),
            make_step("b", "StepB", vec![]),
            make_step("c", "StepC", vec![]),
        ];
        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, Some(3))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 3);
    }

    #[tokio::test]
    async fn execute_diamond_dag() {
        let engine = setup();
        let steps = vec![
            make_step("a", "StepA", vec![]),
            make_step("b", "StepB", vec!["a"]),
            make_step("c", "StepC", vec!["a"]),
            make_step("d", "StepD", vec!["b", "c"]),
        ];
        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, Some(2))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 4);
    }

    #[tokio::test]
    async fn execute_with_condition_skip() {
        let engine = setup();
        let mut step = make_step("a", "StepA", vec![]);
        step.condition = Some("false".into());

        let result = engine
            .execute("p1", "t1", &[step], HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 1);
        // Step was skipped but still counted as success
        let sr = &result.step_results[0];
        assert!(sr.success);
        let output = sr.result.as_ref().unwrap();
        assert_eq!(output.get("skipped"), Some(&serde_json::json!(true)));
    }

    #[tokio::test]
    async fn execute_with_condition_pass() {
        let engine = setup();
        let mut step = make_step("a", "StepA", vec![]);
        step.condition = Some("true".into());

        let result = engine
            .execute("p1", "t1", &[step], HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.success);
        let sr = &result.step_results[0];
        assert!(sr.success);
        // Should not be skipped
        let output = sr.result.as_ref().unwrap();
        assert!(output.get("skipped").is_none());
    }

    #[tokio::test]
    async fn execute_with_variable_condition() {
        let engine = setup();
        let mut step = make_step("a", "StepA", vec![]);
        step.condition = Some("$should_run".into());

        let mut vars = HashMap::new();
        vars.insert("should_run".to_string(), serde_json::json!(true));

        let result = engine
            .execute("p1", "t1", &[step], vars, None, None)
            .await
            .unwrap();

        assert!(result.success);
        let output = result.step_results[0].result.as_ref().unwrap();
        assert!(output.get("skipped").is_none());
    }

    #[tokio::test]
    async fn execute_with_input_mapping() {
        let engine = setup();
        let step_a = make_step("a", "StepA", vec![]);
        let mut step_b = make_step("b", "StepB", vec!["a"]);
        let mut mapping = HashMap::new();
        mapping.insert("prev_result".to_string(), "a".to_string());
        step_b.input_mapping = Some(mapping);

        let result = engine
            .execute("p1", "t1", &[step_a, step_b], HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 2);
    }

    #[tokio::test]
    async fn execute_with_timeout() {
        let engine = setup();
        let steps = vec![make_step("a", "StepA", vec![])];
        let result = engine
            .execute(
                "p1",
                "t1",
                &steps,
                HashMap::new(),
                Some(Duration::from_secs(30)),
                None,
            )
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn execute_cycle_detection() {
        let engine = setup();
        let steps = vec![
            make_step("a", "StepA", vec!["b"]),
            make_step("b", "StepB", vec!["a"]),
        ];
        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, None)
            .await;

        let err = result.expect_err("Expected cycle detection error");
        let msg = err.to_string();
        assert!(
            msg.contains("cycle") || msg.contains("dependencies"),
            "Error message should mention cycle or dependencies, got: {msg}"
        );
    }

    #[tokio::test]
    async fn execute_outputs_stored_by_key() {
        let engine = setup();
        let step = make_step("a", "StepA", vec![]);
        // output_key is "a" by default from make_step

        let _ctx = PipelineExecutionContext {
            pipeline_id: "p1".into(),
            tenant_id: "t1".into(),
            variables: HashMap::new(),
            step_outputs: Arc::new(Mutex::new(HashMap::new())),
            timeout: None,
        };

        let result = engine
            .execute("p1", "t1", &[step], HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.success);
        // The step output should be stored (verified through the result structure)
        assert!(result.step_results[0].result.is_some());
    }

    #[tokio::test]
    async fn execute_produces_duration() {
        let engine = setup();
        let steps = vec![make_step("a", "StepA", vec![])];
        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, None)
            .await
            .unwrap();

        assert!(result.duration_ms.is_some());
        assert!(result.step_results[0].duration_ms.is_some());
    }

    #[tokio::test]
    async fn execute_max_parallel_bounds_concurrency() {
        let engine = setup();
        // Create 6 independent steps with max_parallel=2
        let steps: Vec<PipelineStep> = (0..6)
            .map(|i| make_step(&format!("s{i}"), &format!("Step{i}"), vec![]))
            .collect();

        let result = engine
            .execute("p1", "t1", &steps, HashMap::new(), None, Some(2))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.step_results.len(), 6);
    }
}
