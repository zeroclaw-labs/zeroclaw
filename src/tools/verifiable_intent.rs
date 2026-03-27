//! Verifiable Intent tool — exposes VI verification and constraint evaluation
//! to the agent orchestration loop.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};
use crate::verifiable_intent::error::ViError;
use crate::verifiable_intent::types::{Constraint, Fulfillment};
use crate::verifiable_intent::verification::{
    check_constraints, verify_sd_hash_binding, verify_timestamps, ConstraintCheckResult,
    StrictnessMode,
};

/// Tool for verifying Verifiable Intent credential chains and evaluating
/// constraints against fulfillment data.
pub struct VerifiableIntentTool {
    security: Arc<SecurityPolicy>,
    strictness: StrictnessMode,
}

impl VerifiableIntentTool {
    pub fn new(security: Arc<SecurityPolicy>, strictness: StrictnessMode) -> Self {
        Self {
            security,
            strictness,
        }
    }
}

#[async_trait]
impl Tool for VerifiableIntentTool {
    fn name(&self) -> &str {
        "vi_verify"
    }

    fn description(&self) -> &str {
        "Verify a Verifiable Intent credential chain. Supports two operations: \
         'verify_binding' checks sd_hash binding between credential layers; \
         'evaluate_constraints' validates constraints against fulfillment data."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["verify_binding", "evaluate_constraints", "verify_timestamps"],
                    "description": "The VI operation to perform."
                },
                "sd_hash": {
                    "type": "string",
                    "description": "Expected sd_hash value (for verify_binding)."
                },
                "serialized_parent": {
                    "type": "string",
                    "description": "Serialized parent SD-JWT (for verify_binding)."
                },
                "iat": {
                    "type": "integer",
                    "description": "Issued-at timestamp (for verify_timestamps)."
                },
                "exp": {
                    "type": "integer",
                    "description": "Expiration timestamp (for verify_timestamps)."
                },
                "constraints": {
                    "type": "array",
                    "description": "Constraint array (for evaluate_constraints)."
                },
                "fulfillment": {
                    "type": "object",
                    "description": "Fulfillment data to evaluate against (for evaluate_constraints)."
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "vi_verify")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");

        match operation {
            "verify_binding" => execute_verify_binding(&args),
            "evaluate_constraints" => execute_evaluate_constraints(&args, self.strictness),
            "verify_timestamps" => execute_verify_timestamps(&args),
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("unknown operation: {operation}")),
            }),
        }
    }
}

fn execute_verify_binding(args: &serde_json::Value) -> anyhow::Result<ToolResult> {
    let sd_hash = args
        .get("sd_hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'sd_hash' parameter"))?;
    let serialized_parent = args
        .get("serialized_parent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing 'serialized_parent' parameter"))?;

    match verify_sd_hash_binding(sd_hash, serialized_parent) {
        Ok(()) => Ok(ToolResult {
            success: true,
            output: "sd_hash binding verified".into(),
            error: None,
        }),
        Err(e) => Ok(vi_error_result(&e)),
    }
}

fn execute_evaluate_constraints(
    args: &serde_json::Value,
    strictness: StrictnessMode,
) -> anyhow::Result<ToolResult> {
    let constraints_value = args
        .get("constraints")
        .ok_or_else(|| anyhow::anyhow!("missing 'constraints' parameter"))?;
    let fulfillment_value = args
        .get("fulfillment")
        .ok_or_else(|| anyhow::anyhow!("missing 'fulfillment' parameter"))?;

    let constraints: Vec<Constraint> = serde_json::from_value(constraints_value.clone())?;
    let fulfillment: Fulfillment = serde_json::from_value(fulfillment_value.clone())?;

    let results = check_constraints(&constraints, &fulfillment, strictness);
    let all_satisfied = results.iter().all(|r| r.satisfied);

    let summary: Vec<serde_json::Value> = results.iter().map(constraint_result_json).collect();

    Ok(ToolResult {
        success: all_satisfied,
        output: serde_json::to_string_pretty(&json!({
            "all_satisfied": all_satisfied,
            "results": summary,
        }))?,
        error: if all_satisfied {
            None
        } else {
            Some("one or more constraints violated".into())
        },
    })
}

fn execute_verify_timestamps(args: &serde_json::Value) -> anyhow::Result<ToolResult> {
    let iat = args
        .get("iat")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("missing 'iat' parameter"))?;
    let exp = args
        .get("exp")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("missing 'exp' parameter"))?;

    match verify_timestamps(iat, exp) {
        Ok(()) => Ok(ToolResult {
            success: true,
            output: "timestamps valid".into(),
            error: None,
        }),
        Err(e) => Ok(vi_error_result(&e)),
    }
}

fn vi_error_result(e: &ViError) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!("{}", e)),
    }
}

fn constraint_result_json(r: &ConstraintCheckResult) -> serde_json::Value {
    json!({
        "constraint_type": r.constraint_type,
        "satisfied": r.satisfied,
        "violations": r.violations.iter().map(|v: &ViError| v.to_string()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;

    fn test_tool() -> VerifiableIntentTool {
        let policy = Arc::new(SecurityPolicy::default());
        VerifiableIntentTool::new(policy, StrictnessMode::Strict)
    }

    #[tokio::test]
    async fn verify_timestamps_valid() {
        let tool = test_tool();
        let now = chrono::Utc::now().timestamp();
        let args = json!({
            "operation": "verify_timestamps",
            "iat": now - 60,
            "exp": now + 3600,
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn verify_timestamps_expired() {
        let tool = test_tool();
        let args = json!({
            "operation": "verify_timestamps",
            "iat": 1_000_000,
            "exp": 1_000_001,
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn evaluate_constraints_empty() {
        let tool = test_tool();
        let args = json!({
            "operation": "evaluate_constraints",
            "constraints": [],
            "fulfillment": {},
        });
        let result = tool.execute(args).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn unknown_operation_fails() {
        let tool = test_tool();
        let args = json!({ "operation": "bad_op" });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }
}
