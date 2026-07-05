use std::path::{Path, PathBuf};
use std::process::Command;
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
        let mut info = git_info(self.id(), "Read git status");
        info.output_schema = Some(json!({
            "type": "object",
            "required": ["clean", "status"],
            "properties": {
                "clean": { "type": "boolean" },
                "status": { "type": "string" }
            }
        }));
        info
    }

    fn execute(&self, ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        let cwd = resolve_cwd(&ctx, &input)?;
        let output = run_git(&cwd, &["status", "--short"])?;
        if !output.success {
            return Ok(output);
        }
        let status = output
            .output
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let clean = status.trim().is_empty();
        let require_clean = input
            .get("require_clean")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if require_clean && !clean {
            return Ok(CapabilityResult::failure("git working tree is not clean"));
        }
        Ok(CapabilityResult::success(json!({
            "clean": clean,
            "status": status,
            "cwd": cwd.display().to_string(),
        })))
    }
}

struct GitDiffCapability;

impl SopCapability for GitDiffCapability {
    fn id(&self) -> &'static str {
        "git.diff"
    }

    fn describe(&self) -> CapabilityInfo {
        git_info(self.id(), "Read git diff")
    }

    fn execute(&self, ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        let cwd = resolve_cwd(&ctx, &input)?;
        let mut args = vec!["diff", "--no-ext-diff"];
        if input.get("stat").and_then(Value::as_bool).unwrap_or(false) {
            args.push("--stat");
        }
        if input
            .get("cached")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            args.push("--cached");
        }
        run_git(&cwd, &args)
    }
}

struct NotifyChannelCapability;

impl SopCapability for NotifyChannelCapability {
    fn id(&self) -> &'static str {
        "notify.channel"
    }

    fn describe(&self) -> CapabilityInfo {
        info(self.id(), "No-op notification adapter")
    }

    fn execute(&self, _ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        Ok(CapabilityResult::success(json!({
            "delivered": false,
            "adapter": "noop",
            "input": input,
        })))
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
        output_schema: Some(json!({
            "type": "object",
            "required": ["success", "stdout", "stderr"],
            "properties": {
                "success": { "type": "boolean" },
                "stdout": { "type": "string" },
                "stderr": { "type": "string" }
            }
        })),
    }
}

fn resolve_cwd(ctx: &CapabilityContext, input: &Value) -> Result<PathBuf> {
    if let Some(raw) = input.get("cwd").and_then(Value::as_str) {
        return Ok(PathBuf::from(raw));
    }
    if let Some(location) = ctx.sop_location.as_ref() {
        return Ok(location.clone());
    }
    std::env::current_dir().context("resolve current working directory")
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<CapabilityResult> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Ok(CapabilityResult::failure(if stderr.trim().is_empty() {
            format!("git exited with status {}", output.status)
        } else {
            stderr
        }));
    }
    Ok(CapabilityResult::success(json!({
        "success": true,
        "stdout": stdout,
        "stderr": stderr,
        "cwd": cwd.display().to_string(),
    })))
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
