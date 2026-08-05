//! Verifiable Intent tool — exposes VI verification and constraint evaluation
//! to the agent orchestration loop.
//!
//! # Trust boundary
//!
//! The `verify_chain` operation is the runtime trust boundary for #9328: it
//! takes ONLY the serialized credential chain (L1/L2/L3a/L3b) from the model.
//! The issuer trust anchor (the JWK that signed L1) and the strictness mode
//! come from operator config, and constraints + fulfillment are recovered
//! from the verified chain itself — never from model arguments. The result is
//! opaque: verified + satisfied, or a fail-closed error list.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use crate::verifiable_intent::chain::{ChainVerifyRequest, verify_chain};
use crate::verifiable_intent::error::ViError;
use crate::verifiable_intent::types::Jwk;
use crate::verifiable_intent::verification::{
    ConstraintCheckResult, StrictnessMode, check_constraints, verify_sd_hash_binding,
    verify_timestamps,
};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Tool for verifying Verifiable Intent credential chains and evaluating
/// constraints against fulfillment data.
pub struct VerifiableIntentTool {
    security: Arc<SecurityPolicy>,
    strictness: StrictnessMode,
    /// Runtime-owned issuer trust anchor (the key that signed L1).
    issuer_jwk: Option<Jwk>,
}

impl VerifiableIntentTool {
    pub fn new(security: Arc<SecurityPolicy>, strictness: StrictnessMode) -> Self {
        Self {
            security,
            strictness,
            issuer_jwk: None,
        }
    }

    /// Attach the runtime-owned issuer trust anchor from operator config.
    /// Without it, `verify_chain` is unavailable and the tool answers with an
    /// explicit "unconfigured trust anchor" failure rather than accepting one
    /// from model arguments.
    pub fn with_issuer_jwk(mut self, issuer_jwk: Jwk) -> Self {
        self.issuer_jwk = Some(issuer_jwk);
        self
    }
}

#[async_trait]
impl Tool for VerifiableIntentTool {
    fn name(&self) -> &str {
        "vi_verify"
    }

    fn description(&self) -> &str {
        "Verify a Verifiable Intent credential chain. Supports two operations: \
         'verify_chain' verifies the full L1→L2→L3 chain and evaluates the \
         constraints recovered from it (opaque result); 'verify_binding' \
         checks sd_hash binding between credential layers."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["verify_chain", "verify_binding", "verify_timestamps"],
                    "description": "The VI operation to perform."
                },
                "serialized_l1": {
                    "type": "string",
                    "description": "Serialized L1 SD-JWT (for verify_chain)."
                },
                "serialized_l2": {
                    "type": "string",
                    "description": "Serialized L2 KB-SD-JWT (for verify_chain)."
                },
                "serialized_l3a": {
                    "type": "string",
                    "description": "Serialized L3a payment JWS (for verify_chain, Autonomous mode)."
                },
                "serialized_l3b": {
                    "type": "string",
                    "description": "Serialized L3b checkout JWS (for verify_chain, Autonomous mode)."
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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");

        match operation {
            "verify_chain" => {
                execute_verify_chain(&args, self.strictness, self.issuer_jwk.as_ref())
            }
            "verify_binding" => execute_verify_binding(&args),
            "verify_timestamps" => execute_verify_timestamps(&args),
            _ => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("unknown operation: {operation}")),
            }),
        }
    }
}

/// The runtime trust-boundary operation: verify the full chain with the
/// runtime-owned issuer key and evaluate the constraints recovered from the
/// verified chain. Constraints and fulfillment are NEVER accepted from model
/// arguments — they are derived from the signed chain inside the verifier.
fn execute_verify_chain(
    args: &serde_json::Value,
    strictness: StrictnessMode,
    issuer_jwk: Option<&Jwk>,
) -> anyhow::Result<ToolResult> {
    let Some(issuer) = issuer_jwk else {
        return Ok(ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(
                "vi_verify: no runtime-owned issuer trust anchor configured \
                 (verifiable_intent.issuer_jwk); refusing to accept one from model arguments"
                    .into(),
            ),
        });
    };

    let need = |param: &str| -> anyhow::Result<&str> {
        args.get(param)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg(format!("missing '{param}' parameter")))
    };

    let serialized_l1 = need("serialized_l1")?;
    let serialized_l2 = need("serialized_l2")?;
    let serialized_l3a = args.get("serialized_l3a").and_then(|v| v.as_str());
    let serialized_l3b = args.get("serialized_l3b").and_then(|v| v.as_str());

    let req = ChainVerifyRequest {
        serialized_l1,
        serialized_l2,
        serialized_l3a,
        serialized_l3b,
    };

    match verify_chain(&req, issuer, strictness) {
        Ok(verified) => {
            // Evaluate the constraints recovered from the verified chain.
            let results =
                check_constraints(&verified.constraints, &verified.fulfillment, strictness);
            let all_satisfied = results.iter().all(|r| r.satisfied);
            let summary: Vec<serde_json::Value> =
                results.iter().map(constraint_result_json).collect();
            Ok(ToolResult {
                success: all_satisfied,
                output: serde_json::to_string_pretty(&json!({
                    "verified": true,
                    "mode": format!("{:?}", verified.mode),
                    "all_satisfied": all_satisfied,
                    "results": summary,
                }))?
                .into(),
                error: if all_satisfied {
                    None
                } else {
                    Some("chain verified but constraints not satisfied".into())
                },
            })
        }
        Err(errors) => Ok(ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(format!(
                "chain verification failed: {}",
                errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
        }),
    }
}

fn execute_verify_binding(args: &serde_json::Value) -> anyhow::Result<ToolResult> {
    let sd_hash = args
        .get("sd_hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "sd_hash"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("missing 'sd_hash' parameter")
        })?;
    let serialized_parent = args
        .get("serialized_parent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "serialized_parent"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("missing 'serialized_parent' parameter")
        })?;

    match verify_sd_hash_binding(sd_hash, serialized_parent) {
        Ok(()) => Ok(ToolResult {
            success: true,
            output: "sd_hash binding verified".into(),
            error: None,
        }),
        Err(e) => Ok(vi_error_result(&e)),
    }
}

fn execute_verify_timestamps(args: &serde_json::Value) -> anyhow::Result<ToolResult> {
    let iat = args.get("iat").and_then(|v| v.as_i64()).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"param": "iat"})),
            "tool argument validation failed"
        );

        anyhow::Error::msg("missing 'iat' parameter")
    })?;
    let exp = args.get("exp").and_then(|v| v.as_i64()).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"param": "exp"})),
            "tool argument validation failed"
        );

        anyhow::Error::msg("missing 'exp' parameter")
    })?;

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
        output: ToolOutput::default(),
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
    async fn verify_chain_without_trust_anchor_refuses() {
        // The runtime trust boundary (review #9762): verify_chain must never
        // accept the issuer key from model arguments. Without the operator-
        // configured anchor, the tool answers with an explicit failure.
        let tool = test_tool();
        let args = json!({
            "operation": "verify_chain",
            "serialized_l1": "x",
            "serialized_l2": "y",
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("issuer trust anchor") && err.contains("refusing"),
            "unexpected error: {err}"
        );
    }

    /// The fail-closed constraint surface now lives behind verify_chain:
    /// constraints and fulfillment are recovered from the verified chain, and
    /// the operation is unavailable without the runtime-owned trust anchor —
    /// so a caller can no longer hand the evaluator arbitrary facts.
    #[tokio::test]
    async fn evaluate_constraints_is_no_longer_reachable() {
        let tool = test_tool();
        let args = json!({
            "operation": "evaluate_constraints",
            "constraints": [],
            "fulfillment": {},
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("unknown operation"),
            "evaluate_constraints must be removed from the reachable surface"
        );
    }

    #[tokio::test]
    async fn unknown_operation_fails() {
        let tool = test_tool();
        let args = json!({ "operation": "bad_op" });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }
}
