use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::registry::SopCapabilityRegistry;
use super::types::{CapabilityContext, CapabilityInfo, CapabilityResult, SopCapability};

const MAX_WAIT_MS: u64 = 60_000;

pub(super) fn register(registry: &mut SopCapabilityRegistry) {
    registry.register(NoopCapability);
    registry.register(WaitCapability);
    registry.register(ApprovalWaitCapability);
    registry.register(JsonValidateCapability);
    registry.register(ShellExecCapability);
    registry.register(GitStatusCapability);
    registry.register(GitDiffCapability);
    registry.register(NotifyChannelCapability);
    // Injected-adapter capabilities, registered here as FAIL-CLOSED placeholders
    // (adapter = None) so SOPs referencing them pass load-time validation
    // everywhere; `build_sop_engine` re-registers them with real adapters when
    // the daemon supplies one (a later `register` for the same id overwrites).
    registry.register(super::forge_comment::ForgeCommentCapability::new(None));
    registry.register(super::llm_generate::LlmGenerateCapability::new(None));
}

struct NoopCapability;

impl SopCapability for NoopCapability {
    fn id(&self) -> &'static str {
        "noop"
    }

    fn describe(&self) -> CapabilityInfo {
        info(self.id(), "Return the input unchanged")
    }

    fn execute(&self, _ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::success(input))
    }
}

struct WaitCapability;

impl SopCapability for WaitCapability {
    fn id(&self) -> &'static str {
        "wait"
    }

    fn describe(&self) -> CapabilityInfo {
        CapabilityInfo {
            id: self.id(),
            description: "Wait for a bounded duration",
            deterministic: true,
            idempotent: false,
            reversible: false,
            supports_retry: true,
            required_permissions: Vec::new(),
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "milliseconds": { "type": "integer" },
                    "seconds": { "type": "number" }
                }
            })),
            output_schema: Some(json!({
                "type": "object",
                "required": ["waited_ms"],
                "properties": {
                    "waited_ms": { "type": "integer" }
                }
            })),
        }
    }

    fn execute(&self, _ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        let millis = input
            .get("milliseconds")
            .and_then(Value::as_u64)
            .or_else(|| {
                input
                    .get("seconds")
                    .and_then(Value::as_f64)
                    .map(|seconds| (seconds.max(0.0) * 1000.0) as u64)
            })
            .unwrap_or(0);
        if millis > MAX_WAIT_MS {
            bail!("wait capability duration exceeds {MAX_WAIT_MS}ms");
        }
        if millis > 0 {
            std::thread::sleep(Duration::from_millis(millis));
        }
        Ok(CapabilityResult::success(json!({ "waited_ms": millis })))
    }
}

struct ApprovalWaitCapability;

impl SopCapability for ApprovalWaitCapability {
    fn id(&self) -> &'static str {
        "approval.wait"
    }

    fn describe(&self) -> CapabilityInfo {
        let mut info = info(
            self.id(),
            "Fail-closed placeholder for the approval capability",
        );
        info.idempotent = false;
        info
    }

    fn execute(&self, _ctx: CapabilityContext, _input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::failure(
            "approval.wait is registered but must route through checkpoint/resolve_gate wiring",
        ))
    }
}

struct JsonValidateCapability;

impl SopCapability for JsonValidateCapability {
    fn id(&self) -> &'static str {
        "json.validate"
    }

    fn describe(&self) -> CapabilityInfo {
        CapabilityInfo {
            id: self.id(),
            description: "Validate a JSON value against the SOP schema subset",
            deterministic: true,
            idempotent: true,
            reversible: true,
            supports_retry: true,
            required_permissions: Vec::new(),
            input_schema: Some(json!({
                "type": "object",
                "required": ["schema", "value"],
                "properties": {
                    "schema": { "type": "object" },
                    "value": {}
                }
            })),
            output_schema: Some(json!({
                "type": "object",
                "required": ["valid"],
                "properties": {
                    "valid": { "type": "boolean" }
                }
            })),
        }
    }

    fn execute(&self, _ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        let schema = input
            .get("schema")
            .context("json.validate missing schema")?;
        let value = input.get("value").context("json.validate missing value")?;
        crate::sop::schema::validate_value(schema, value)?;
        Ok(CapabilityResult::success(json!({
            "valid": true,
            "value": value,
        })))
    }
}

struct ShellExecCapability;

impl SopCapability for ShellExecCapability {
    fn id(&self) -> &'static str {
        "shell.exec"
    }

    fn describe(&self) -> CapabilityInfo {
        CapabilityInfo {
            id: self.id(),
            description: "Fail-closed shell execution placeholder",
            deterministic: true,
            idempotent: false,
            reversible: false,
            supports_retry: false,
            required_permissions: vec!["shell.exec"],
            input_schema: Some(json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string" }
                }
            })),
            output_schema: None,
        }
    }

    fn execute(&self, _ctx: CapabilityContext, _input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::failure(
            "shell.exec capability requires an injected shell-policy adapter",
        ))
    }
}

struct GitStatusCapability;

impl SopCapability for GitStatusCapability {
    fn id(&self) -> &'static str {
        "git.status"
    }

    fn describe(&self) -> CapabilityInfo {
        git_info(self.id(), "Fail-closed placeholder for reading git status")
    }

    fn execute(&self, _ctx: CapabilityContext, _input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::failure(
            "git.status capability requires an injected tool/policy adapter (workspace-scoped, \
             sandboxed git access) before it can execute",
        ))
    }
}

struct GitDiffCapability;

impl SopCapability for GitDiffCapability {
    fn id(&self) -> &'static str {
        "git.diff"
    }

    fn describe(&self) -> CapabilityInfo {
        git_info(self.id(), "Fail-closed placeholder for reading git diff")
    }

    fn execute(&self, _ctx: CapabilityContext, _input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::failure(
            "git.diff capability requires an injected tool/policy adapter (workspace-scoped, \
             sandboxed git access) before it can execute",
        ))
    }
}

struct NotifyChannelCapability;

impl SopCapability for NotifyChannelCapability {
    fn id(&self) -> &'static str {
        "notify.channel"
    }

    fn describe(&self) -> CapabilityInfo {
        info(
            self.id(),
            "Fail-closed placeholder until a real channel delivery adapter is injected",
        )
    }

    fn execute(&self, _ctx: CapabilityContext, _input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::failure(
            "notify.channel capability requires an injected channel-delivery adapter",
        ))
    }
}

fn info(id: &'static str, description: &'static str) -> CapabilityInfo {
    CapabilityInfo {
        id,
        description,
        deterministic: true,
        idempotent: true,
        reversible: true,
        supports_retry: true,
        required_permissions: Vec::new(),
        input_schema: None,
        output_schema: None,
    }
}

fn git_info(id: &'static str, description: &'static str) -> CapabilityInfo {
    CapabilityInfo {
        id,
        description,
        deterministic: true,
        idempotent: true,
        reversible: true,
        supports_retry: true,
        required_permissions: Vec::new(),
        input_schema: Some(json!({
            "type": "object",
            "properties": {
                "cwd": { "type": "string" },
                "require_clean": { "type": "boolean" },
                "stat": { "type": "boolean" },
                "cached": { "type": "boolean" }
            }
        })),
        output_schema: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_contains_required_ids() {
        let registry = SopCapabilityRegistry::with_builtins();
        for id in [
            "noop",
            "wait",
            "approval.wait",
            "json.validate",
            "shell.exec",
            "git.status",
            "git.diff",
            "notify.channel",
        ] {
            assert!(registry.contains(id), "{id} should be registered");
        }
    }

    #[test]
    fn noop_returns_input() {
        let registry = SopCapabilityRegistry::with_builtins();
        let step = crate::sop::types::SopStep {
            number: 1,
            title: "noop".into(),
            body: String::new(),
            kind: crate::sop::types::SopStepKind::Capability,
            capability: Some("noop".into()),
            ..crate::sop::types::SopStep::default()
        };
        let output = registry
            .execute_step(context(), &step, json!({"ok": true}))
            .unwrap();
        assert_eq!(output, CapabilityResult::success(json!({"ok": true})));
    }

    #[test]
    fn json_validate_fails_bad_value() {
        let registry = SopCapabilityRegistry::with_builtins();
        let step = crate::sop::types::SopStep {
            number: 1,
            title: "validate".into(),
            body: String::new(),
            kind: crate::sop::types::SopStepKind::Capability,
            capability: Some("json.validate".into()),
            capability_input: Some(json!({
                "schema": { "type": "object", "required": ["ok"] },
                "value": {}
            })),
            ..crate::sop::types::SopStep::default()
        };
        let err = registry
            .execute_step(context(), &step, Value::Null)
            .unwrap_err();
        assert!(
            err.to_string().contains("required key missing"),
            "expected a schema required-key error, got: {err}"
        );
    }

    fn context() -> CapabilityContext {
        CapabilityContext {
            run_id: "run-1".into(),
            sop_name: "sop".into(),
            step_number: 1,
            sop_location: None,
        }
    }
}
