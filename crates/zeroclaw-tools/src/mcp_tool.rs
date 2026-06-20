//! Wraps a discovered MCP tool as a zeroclaw [`Tool`] so it is dispatched
//! through the existing tool registry and agent loop without modification.

use std::sync::{Arc, Weak};

use async_trait::async_trait;

use crate::mcp_client::McpRegistry;
use crate::mcp_protocol::McpToolDef;
use zeroclaw_api::tool::{Tool, ToolResult};

/// A zeroclaw [`Tool`] backed by an MCP server tool.
///
/// The `prefixed_name` (e.g. `filesystem__read_file`) is what the agent loop
/// sees. The registry knows how to route it to the correct server.
///
/// Uses `Weak<McpRegistry>` so the registry can be dropped and stdio child
/// processes reaped (via `kill_on_drop`) when the agent run ends, even if
/// wrappers are still alive in skill-elevation or other transient handles.
pub struct McpToolWrapper {
    /// Prefixed name: `<server_name>__<tool_name>`.
    prefixed_name: String,
    /// Description extracted from the MCP tool definition. Stored as an owned
    /// String so that `description()` can return `&str` with self's lifetime.
    description: String,
    /// JSON schema for the tool's input parameters.
    input_schema: serde_json::Value,
    /// Shared registry (weak reference) — used to dispatch actual tool calls.
    /// Upgraded to Arc at call time so the registry can be dropped when all
    /// agent runs end, allowing stdio child processes to be reaped.
    registry: Weak<McpRegistry>,
}

impl McpToolWrapper {
    pub fn new(prefixed_name: String, def: McpToolDef, registry: Arc<McpRegistry>) -> Self {
        let description = def.description.unwrap_or_else(|| "MCP tool".to_string());
        Self {
            prefixed_name,
            description,
            input_schema: def.input_schema,
            registry: Arc::downgrade(&registry),
        }
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Strip the `approved` field before forwarding to the MCP server.
        // ZeroClaw's security model injects `approved: bool` into built-in tool
        // calls for supervised-mode confirmation. MCP servers have no knowledge
        // of this field and will reject calls that include it as an unexpected
        // parameter. We strip it here so MCP servers always receive clean args.
        let args = match args {
            serde_json::Value::Object(mut map) => {
                map.remove("approved");
                serde_json::Value::Object(map)
            }
            other => other,
        };
        let registry = match self.registry.upgrade() {
            Some(r) => r,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "MCP registry dropped; cannot call tool `{}`",
                        self.prefixed_name
                    )),
                });
            }
        };
        match registry.call_tool(&self.prefixed_name, args).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_def(name: &str, description: Option<&str>, schema: serde_json::Value) -> McpToolDef {
        McpToolDef {
            name: name.to_string(),
            description: description.map(str::to_string),
            input_schema: schema,
        }
    }

    async fn empty_registry() -> Arc<McpRegistry> {
        Arc::new(
            McpRegistry::connect_all(&[])
                .await
                .expect("empty connect_all should succeed"),
        )
    }

    // ── Accessor tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn name_returns_prefixed_name() {
        let registry = empty_registry().await;
        let def = make_def("read_file", Some("Reads a file"), json!({}));
        let wrapper = McpToolWrapper::new("filesystem__read_file".to_string(), def, registry);
        assert_eq!(wrapper.name(), "filesystem__read_file");
    }

    #[tokio::test]
    async fn description_returns_def_description() {
        let registry = empty_registry().await;
        let def = make_def("navigate", Some("Navigate browser"), json!({}));
        let wrapper = McpToolWrapper::new("playwright__navigate".to_string(), def, registry);
        assert_eq!(wrapper.description(), "Navigate browser");
    }

    #[tokio::test]
    async fn description_falls_back_to_mcp_tool_when_none() {
        let registry = empty_registry().await;
        let def = make_def("mystery", None, json!({}));
        let wrapper = McpToolWrapper::new("srv__mystery".to_string(), def, registry);
        assert_eq!(wrapper.description(), "MCP tool");
    }

    #[tokio::test]
    async fn parameters_schema_returns_input_schema() {
        let registry = empty_registry().await;
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        });
        let def = make_def("read_file", Some("Read"), schema.clone());
        let wrapper = McpToolWrapper::new("fs__read_file".to_string(), def, registry);
        assert_eq!(wrapper.parameters_schema(), schema);
    }

    #[tokio::test]
    async fn spec_returns_all_three_fields() {
        let registry = empty_registry().await;
        let schema = json!({ "type": "object", "properties": {} });
        let def = make_def("list_dir", Some("List directory"), schema.clone());
        let wrapper = McpToolWrapper::new("fs__list_dir".to_string(), def, registry);
        let spec = wrapper.spec();
        assert_eq!(spec.name, "fs__list_dir");
        assert_eq!(spec.description, "List directory");
        assert_eq!(spec.parameters, schema);
    }

    // ── execute() error path ───────────────────────────────────────────────

    #[tokio::test]
    async fn execute_returns_non_fatal_error_for_unknown_tool() {
        // An empty registry has no tools — execute must return Ok(ToolResult { success: false })
        // rather than propagating an Err (non-fatal by design).
        let registry = empty_registry().await;
        let def = make_def("ghost", Some("Ghost tool"), json!({}));
        // Keep a strong `Arc` alive across the call: the wrapper now holds only a
        // `Weak`, mirroring the agent runtime that owns the registry while its
        // tools execute. Without this the registry would drop before `execute()`
        // and the `Weak` would fail to upgrade (yielding a "registry dropped"
        // error instead of the expected unknown-tool error).
        let wrapper = McpToolWrapper::new("nowhere__ghost".to_string(), def, registry.clone());
        let result = wrapper
            .execute(json!({}))
            .await
            .expect("execute should be non-fatal");
        drop(registry);
        assert!(!result.success);
        let err_msg = result.error.expect("error message should be present");
        assert!(
            err_msg.contains("unknown MCP tool"),
            "unexpected error: {err_msg}"
        );
        assert!(result.output.is_empty());
    }

    #[tokio::test]
    async fn eager_wrapper_dispatches_while_run_owner_holds_registry() {
        // Eager-path lifetime contract (#5903 / #8023): the wrapper holds only a
        // `Weak`, so the agent run must keep one strong `Arc<McpRegistry>` alive
        // (now stored on `Agent`, dropped in `Agent::drop`). While that owner
        // lives, the wrapper upgrades and dispatches; once it drops, the wrapper
        // fails gracefully and the registry (and its kill_on_drop stdio children)
        // can be reaped. This is the scenario the unknown-tool test does not
        // cover and that a `Weak`-only wrapper regressed.
        let run_owner = empty_registry().await; // the strong Arc the agent holds
        let def = make_def("ghost", Some("Ghost tool"), json!({}));
        // Built as the eager path does: new() downgrades internally, so the Arc
        // passed here is consumed and `run_owner` is left as the sole strong
        // reference — exactly the post-init state.
        let wrapper =
            McpToolWrapper::new("nowhere__ghost".to_string(), def, Arc::clone(&run_owner));

        // Owner alive -> upgrade succeeds -> dispatch reaches the registry and
        // returns the genuine unknown-tool error, NOT "registry dropped".
        let ok = wrapper.execute(json!({})).await.expect("execute non-fatal");
        assert!(!ok.success);
        assert!(
            ok.error
                .as_deref()
                .unwrap_or("")
                .contains("unknown MCP tool"),
            "while the run owner lives the wrapper must dispatch; got: {:?}",
            ok.error
        );

        // Drop the run owner (mirrors Agent::drop) -> no strong refs remain ->
        // the Weak can no longer upgrade -> graceful "registry dropped".
        drop(run_owner);
        let after = wrapper.execute(json!({})).await.expect("execute non-fatal");
        assert!(!after.success);
        assert!(
            after
                .error
                .as_deref()
                .unwrap_or("")
                .contains("registry dropped"),
            "after the run owner drops the wrapper must fail gracefully; got: {:?}",
            after.error
        );
    }

    #[tokio::test]
    async fn execute_success_sets_success_true_and_output() {
        // Verify the ToolResult success-branch struct shape compiles correctly.
        // A real happy-path requires a live MCP server; that is covered by E2E tests.
        let _: ToolResult = ToolResult {
            success: true,
            output: "hello".to_string(),
            error: None,
        };
    }

    // ── approved-field stripping ───────────────────────────────────────────
    // ZeroClaw's security model injects `approved: bool` into built-in tool args.
    // MCP servers are unaware of this field and reject calls that include it.
    // execute() must strip it before forwarding.

    #[tokio::test]
    async fn execute_strips_approved_field_from_object_args() {
        // The wrapper should remove `approved` before forwarding to the registry.
        // We use an empty registry (returns "unknown MCP tool" error), but the key
        // assertion is that the call does not fail due to an unexpected `approved` arg.
        let registry = empty_registry().await;
        let def = make_def("do_thing", Some("Do a thing"), json!({}));
        let wrapper = McpToolWrapper::new("srv__do_thing".to_string(), def, registry);
        // With `approved` present the call must not propagate an Err — non-fatal.
        let result = wrapper
            .execute(json!({ "approved": true, "param": "value" }))
            .await
            .expect("execute must be non-fatal even with approved field");
        // The registry returns a non-fatal error (unknown tool), not a panic/Err.
        assert!(!result.success);
        // Crucially: error must not mention `approved` as the cause.
        let err = result.error.unwrap_or_default();
        assert!(
            !err.to_lowercase().contains("approved"),
            "approved field should have been stripped, but got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_handles_non_object_args_without_panic() {
        // Non-object args (string, null, array) must pass through without panicking
        // or returning an Err — the registry error path covers the failure case.
        let registry = empty_registry().await;
        let def = make_def("noop", None, json!({}));
        let wrapper = McpToolWrapper::new("srv__noop".to_string(), def, registry);
        for non_obj in [json!(null), json!("a string"), json!([1, 2, 3])] {
            let result = wrapper
                .execute(non_obj.clone())
                .await
                .expect("non-object args must not propagate Err");
            assert!(!result.success, "expected non-fatal failure for {non_obj}");
        }
    }
}
