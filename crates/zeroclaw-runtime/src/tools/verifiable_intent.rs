//! Verifiable Intent primitives exposed as a tool.
//!
//! These operations check individual values the caller supplies. They do not
//! authenticate a credential chain: nothing here establishes that a constraint
//! or a fulfillment came from a signed credential. This type is currently not
//! registered for the model for that reason, and an embedder constructing it
//! directly gets the same limitation.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use crate::verifiable_intent::error::ViError;
use crate::verifiable_intent::types::{Constraint, Fulfillment};
use crate::verifiable_intent::verification::{
    ConstraintCheckResult, StrictnessMode, check_constraints, verify_sd_hash_binding,
    verify_timestamps,
};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Evaluates Verifiable Intent constraints and checks binding and timestamp
/// primitives against values the caller provides.
///
/// It does not verify a credential chain, so a satisfied result is not evidence
/// that the inputs were signed or authorized.
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
        "Check Verifiable Intent primitives against values you supply. This does \
         NOT verify a credential chain, so a passing result does not show the \
         inputs were signed or authorized. Operations: 'verify_binding' checks an \
         sd_hash against a serialized parent; 'evaluate_constraints' evaluates \
         constraints against fulfillment data; 'verify_timestamps' checks an \
         iat/exp pair."
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
                output: ToolOutput::default(),
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
                output: ToolOutput::default(),
                error: Some(format!("unknown operation: {operation}")),
            }),
        }
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

fn execute_evaluate_constraints(
    args: &serde_json::Value,
    strictness: StrictnessMode,
) -> anyhow::Result<ToolResult> {
    let constraints_value = args.get("constraints").ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"param": "constraints"})),
            "tool argument validation failed"
        );

        anyhow::Error::msg("missing 'constraints' parameter")
    })?;
    let fulfillment_value = args.get("fulfillment").ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"param": "fulfillment"})),
            "tool argument validation failed"
        );

        anyhow::Error::msg("missing 'fulfillment' parameter")
    })?;

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
        }))?
        .into(),
        error: if all_satisfied {
            None
        } else {
            Some("one or more constraints violated".into())
        },
    })
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

    /// The reachable surface for a constraint check is this tool call: caller
    /// JSON is deserialized into `Fulfillment`, dispatched through
    /// `check_constraints`, and returned as a `ToolResult`. Every `Fulfillment`
    /// field is `Option` with `Default` derived, so `"fulfillment": {}`
    /// deserializes to all-`None`; this asserts the whole path fails closed
    /// rather than only the constraint helper.
    #[tokio::test]
    async fn empty_fulfillment_does_not_satisfy_allowed_merchant() {
        let tool = test_tool();
        let args = json!({
            "operation": "evaluate_constraints",
            "constraints": [{
                "type": "mandate.checkout.allowed_merchant",
                "allowed_merchants": [
                    { "name": "Store A", "website": "https://store-a.example.com" }
                ]
            }],
            "fulfillment": {},
        });

        let result = tool.execute(args).await.unwrap();

        assert!(
            !result.success,
            "an empty fulfillment must not satisfy an allowed-merchant constraint"
        );
        assert_eq!(
            result.error.as_deref(),
            Some("one or more constraints violated")
        );

        let output: serde_json::Value =
            serde_json::from_str(result.output.as_str()).expect("tool output is JSON");
        assert_eq!(output["all_satisfied"], false);
        let entry = &output["results"][0];
        assert_eq!(
            entry["constraint_type"],
            "mandate.checkout.allowed_merchant"
        );
        assert_eq!(entry["satisfied"], false);
        let violation = entry["violations"][0]
            .as_str()
            .expect("violation is a string");
        assert!(
            violation.starts_with("VI/MerchantNotAllowed:"),
            "unexpected violation: {violation}"
        );
    }

    /// The same for the payee allowlist.
    #[tokio::test]
    async fn empty_fulfillment_does_not_satisfy_allowed_payee() {
        let tool = test_tool();
        let args = json!({
            "operation": "evaluate_constraints",
            "constraints": [{
                "type": "payment.allowed_payee",
                "allowed_payees": [
                    { "name": "Payee A", "website": "https://payee-a.example.com" }
                ]
            }],
            "fulfillment": {},
        });

        let result = tool.execute(args).await.unwrap();

        assert!(
            !result.success,
            "an empty fulfillment must not satisfy an allowed-payee constraint"
        );
        assert_eq!(
            result.error.as_deref(),
            Some("one or more constraints violated")
        );

        let output: serde_json::Value =
            serde_json::from_str(result.output.as_str()).expect("tool output is JSON");
        assert_eq!(output["all_satisfied"], false);
        let entry = &output["results"][0];
        assert_eq!(entry["constraint_type"], "payment.allowed_payee");
        assert_eq!(entry["satisfied"], false);
        let violation = entry["violations"][0]
            .as_str()
            .expect("violation is a string");
        assert!(
            violation.starts_with("VI/PayeeNotAllowed:"),
            "unexpected violation: {violation}"
        );
    }

    /// Positive control for the two above: a disclosed merchant that is on the
    /// allowlist still satisfies the constraint through the same tool path, so
    /// the fail-closed arms cannot be satisfied by over-blocking.
    #[tokio::test]
    async fn disclosed_merchant_on_allowlist_still_satisfies_constraint() {
        let tool = test_tool();
        let args = json!({
            "operation": "evaluate_constraints",
            "constraints": [{
                "type": "mandate.checkout.allowed_merchant",
                "allowed_merchants": [
                    { "name": "Store A", "website": "https://store-a.example.com" }
                ]
            }],
            "fulfillment": {
                "merchant": { "name": "Store A", "website": "https://store-a.example.com" }
            },
        });

        let result = tool.execute(args).await.unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let output: serde_json::Value =
            serde_json::from_str(result.output.as_str()).expect("tool output is JSON");
        assert_eq!(output["all_satisfied"], true);
    }

    #[tokio::test]
    async fn unknown_operation_fails() {
        let tool = test_tool();
        let args = json!({ "operation": "bad_op" });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }
}
