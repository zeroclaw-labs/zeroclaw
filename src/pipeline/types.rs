use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Context for pipeline execution, carrying variables and accumulated step outputs.
#[derive(Debug, Clone)]
pub struct PipelineExecutionContext {
    /// Unique identifier for this pipeline
    pub pipeline_id: String,
    /// Tenant identifier for multi-tenancy isolation
    pub tenant_id: String,
    /// Initial variables available to all steps
    pub variables: HashMap<String, serde_json::Value>,
    /// Accumulated outputs from completed steps, keyed by output_key
    pub step_outputs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    /// Optional overall timeout for the entire pipeline execution
    pub timeout: Option<Duration>,
}

/// Result of executing a single pipeline step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// Unique identifier of the step
    pub step_id: String,
    /// Whether the step completed successfully
    pub success: bool,
    /// Output value produced by the step (if successful)
    pub output: Option<serde_json::Value>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Wall-clock duration of the step in milliseconds
    pub duration_ms: u64,
    /// Number of retry attempts used before success or final failure
    pub retries_used: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_execution_result_serializes() {
        let result = StepExecutionResult {
            step_id: "step-1".into(),
            success: true,
            output: Some(serde_json::json!({"data": "hello"})),
            error: None,
            duration_ms: 150,
            retries_used: 0,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: StepExecutionResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.step_id, "step-1");
        assert_eq!(parsed.duration_ms, 150);
    }

    #[test]
    fn pipeline_context_step_outputs_thread_safe() {
        let ctx = PipelineExecutionContext {
            pipeline_id: "p1".into(),
            tenant_id: "t1".into(),
            variables: HashMap::new(),
            step_outputs: Arc::new(Mutex::new(HashMap::new())),
            timeout: Some(Duration::from_secs(60)),
        };
        let outputs = ctx.step_outputs.clone();
        outputs
            .lock()
            .unwrap()
            .insert("key".into(), serde_json::json!("value"));
        assert_eq!(ctx.step_outputs.lock().unwrap().len(), 1);
    }

    #[test]
    fn step_execution_result_with_error() {
        let result = StepExecutionResult {
            step_id: "step-err".into(),
            success: false,
            output: None,
            error: Some("connection refused".into()),
            duration_ms: 5000,
            retries_used: 3,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: StepExecutionResult = serde_json::from_str(&json).unwrap();
        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("connection refused"));
        assert_eq!(parsed.retries_used, 3);
    }
}
