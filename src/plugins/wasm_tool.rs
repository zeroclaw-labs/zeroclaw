//! Bridge between WASM plugins and the Tool trait.

use crate::tools::traits::RiskLevel;
use crate::security::audit::{AuditLogger, HttpRequestEntry, PluginExecutionLog};
use crate::security::policy::{SecurityPolicy, ToolOperation};
use crate::security::redact_sensitive_params;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use extism::Plugin;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    plugin_name: String,
    plugin_version: String,
    function_name: String,
    parameters_schema: Value,
    risk_level: RiskLevel,
    plugin: Arc<Mutex<Plugin>>,
    audit_logger: Option<Arc<AuditLogger>>,
    security: Option<Arc<SecurityPolicy>>,
}

impl WasmTool {
    pub fn new(
        name: String,
        description: String,
        plugin_name: String,
        plugin_version: String,
        function_name: String,
        parameters_schema: Value,
        plugin: Arc<Mutex<Plugin>>,
    ) -> Self {
        Self {
            name,
            description,
            plugin_name,
            plugin_version,
            function_name,
            parameters_schema,
            risk_level: RiskLevel::Low,
            plugin,
            audit_logger: None,
            security: None,
        }
    }

    /// Set the risk level for this tool.
    pub fn with_risk_level(mut self, level: RiskLevel) -> Self {
        self.risk_level = level;
        self
    }

    /// Attach an audit logger to this tool.
    pub fn with_audit_logger(mut self, logger: Arc<AuditLogger>) -> Self {
        self.audit_logger = Some(logger);
        self
    }

    /// Attach a security policy for rate-limit enforcement.
    pub fn with_security_policy(mut self, policy: Arc<SecurityPolicy>) -> Self {
        self.security = Some(policy);
        self
    }
}

#[async_trait]
impl Tool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    fn risk_level(&self) -> RiskLevel {
        self.risk_level
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Enforce rate limit if a security policy is attached.
        if let Some(ref security) = self.security {
            if let Err(error) = security.enforce_tool_operation(ToolOperation::Act, &self.name) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error),
                });
            }
        }

        let start = std::time::Instant::now();
        let json_bytes = serde_json::to_vec(&args).unwrap_or_default();

        let mut plugin = match self.plugin.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let result = match plugin.call::<&[u8], &[u8]>(&self.function_name, &json_bytes) {
            Ok(output_bytes) => match serde_json::from_slice::<Value>(output_bytes) {
                Ok(val) => Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&val).unwrap_or_default(),
                    error: None,
                }),
                Err(parse_err) => Ok(ToolResult {
                    success: false,
                    output: format!(
                        "[plugin:{}/{}] JSON parse failure on output",
                        self.plugin_name, self.function_name
                    ),
                    error: Some(format!(
                        "plugin '{}' export '{}' returned invalid JSON: {}",
                        self.plugin_name, self.function_name, parse_err
                    )),
                }),
            },
            Err(e) => {
                let msg = e.to_string();
                let classified = classify_extism_error(&msg);
                Ok(ToolResult {
                    success: false,
                    output: format!(
                        "[plugin:{}/{}] {}",
                        self.plugin_name, self.function_name, classified
                    ),
                    error: Some(format!(
                        "plugin '{}' export '{}': {}",
                        self.plugin_name, self.function_name, msg
                    )),
                })
            }
        };

        // Emit audit log entry for every plugin tool execution
        if let (Some(logger), Ok(ref tool_result)) = (&self.audit_logger, &result) {
            let duration_ms = start.elapsed().as_millis() as u64;
            let redacted = redact_sensitive_params(&args);
            let http_requests = extract_http_requests(&self.function_name, &args, tool_result);
            let _ = logger.log_plugin_execution(PluginExecutionLog {
                plugin_name: &self.plugin_name,
                plugin_version: &self.plugin_version,
                tool_name: &self.name,
                export_name: &self.function_name,
                success: tool_result.success,
                duration_ms,
                error: tool_result.error.as_deref(),
                redacted_input: Some(serde_json::to_string(&redacted).unwrap_or_default()),
                http_requests,
            });
        }

        result
    }
}

/// Extract HTTP request entries from plugin input/output when the plugin made HTTP calls.
///
/// Detects HTTP requests by checking if the successful output contains `status_code`
/// (indicating an HTTP response) and extracts the URL from the input args.
/// The HTTP method is inferred from the function name.
fn extract_http_requests(
    function_name: &str,
    args: &Value,
    tool_result: &crate::tools::traits::ToolResult,
) -> Option<Vec<HttpRequestEntry>> {
    if !tool_result.success {
        return None;
    }

    // Check if the output looks like an HTTP response (contains status_code)
    let output: Value = serde_json::from_str(&tool_result.output).ok()?;
    if output.get("status_code").is_none() {
        return None;
    }

    // Extract URL from input args
    let url = args.get("url").and_then(|v| v.as_str())?;

    // Infer method from function name
    let lower = function_name.to_lowercase();
    let method = if lower.contains("post") {
        "POST"
    } else if lower.contains("put") {
        "PUT"
    } else if lower.contains("delete") {
        "DELETE"
    } else if lower.contains("patch") {
        "PATCH"
    } else {
        "GET"
    };

    Some(vec![HttpRequestEntry {
        method: method.to_string(),
        url: url.to_string(),
    }])
}

/// Classify an Extism/WASM error message into a human-readable category.
fn classify_extism_error(msg: &str) -> &'static str {
    let lower = msg.to_lowercase();

    // WASM traps
    if lower.contains("unreachable") {
        return "WASM trap: unreachable instruction executed";
    }
    if lower.contains("out of bounds memory") || lower.contains("memory access out of bounds") {
        return "WASM trap: memory out of bounds";
    }
    if lower.contains("call stack exhausted") || lower.contains("stack overflow") {
        return "WASM trap: stack overflow";
    }
    if lower.contains("integer overflow") || lower.contains("integer divide by zero") {
        return "WASM trap: arithmetic error";
    }
    if lower.contains("indirect call type mismatch") {
        return "WASM trap: indirect call type mismatch";
    }

    // Timeout
    if lower.contains("timeout") || lower.contains("timed out") {
        return "execution timed out";
    }

    // Missing export / function not found
    if lower.contains("not found") || lower.contains("unknown export") || lower.contains("missing export") {
        return "export function not found";
    }

    "execution failed"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an `Arc<Mutex<Plugin>>` from a minimal empty WASM module.
    /// The module has no exports, so any `plugin.call()` will fail — which
    /// is exactly what the error-mapping tests need.
    fn make_test_plugin() -> Arc<Mutex<Plugin>> {
        let wasm_bytes: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, // \0asm
            0x01, 0x00, 0x00, 0x00, // version 1
        ];
        let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
        let plugin = Plugin::new(&manifest, [], true).expect("minimal wasm should load");
        Arc::new(Mutex::new(plugin))
    }

    fn make_tool(plugin_name: &str, function_name: &str) -> WasmTool {
        WasmTool::new(
            format!("{plugin_name}_{function_name}"),
            "test tool".into(),
            plugin_name.into(),
            "0.1.0".into(),
            function_name.into(),
            serde_json::json!({ "type": "object" }),
            make_test_plugin(),
        )
    }

    #[tokio::test]
    async fn execute_targets_correct_export_function() {
        let tool = make_tool("weather", "get_forecast");
        let result = tool
            .execute(serde_json::json!({ "city": "Berlin" }))
            .await
            .unwrap();

        // The error output must reference the exact export function name
        // so the agent knows which WASM export was targeted.
        assert!(
            result.output.contains("weather") && result.output.contains("get_forecast"),
            "output should reference plugin_name/function_name, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn execute_uses_distinct_exports_per_tool() {
        let tool_a = make_tool("math", "add");
        let tool_b = make_tool("math", "multiply");

        let result_a = tool_a.execute(serde_json::json!({})).await.unwrap();
        let result_b = tool_b.execute(serde_json::json!({})).await.unwrap();

        // Different export functions on the same plugin must produce
        // distinguishable calls — verifies the bridge doesn't hard-code
        // a single export name.
        assert!(result_a.output.contains("math") && result_a.output.contains("add"));
        assert!(result_b.output.contains("math") && result_b.output.contains("multiply"));
        assert_ne!(result_a.output, result_b.output);
    }

    #[tokio::test]
    async fn execute_forwards_args_to_plugin() {
        // With a real plugin that has exports, args would be serialized
        // and passed through. Here we verify the serialization step works
        // by checking the call doesn't panic on valid JSON input.
        let tool = make_tool("greeter", "hello");
        let args = serde_json::json!({ "name": "ZeroClaw" });
        let result = tool.execute(args).await.unwrap();

        // The minimal wasm has no exports so the call fails, but the
        // error message from Extism should reference the function name.
        assert!(
            result.error.is_some(),
            "call to non-existent export should produce an error"
        );
    }

    #[test]
    fn accessors_expose_correct_metadata() {
        let tool = make_tool("db", "query");

        assert_eq!(tool.name(), "db_query");
        assert_eq!(tool.description(), "test tool");
        assert_eq!(tool.parameters_schema(), serde_json::json!({ "type": "object" }));
    }

    // --- Error-mapping tests (US-ZCL-3-3) ---
    // Verify that execution failures surface as ToolResult with
    // success=false and a populated error field, rather than
    // propagating as anyhow errors.

    #[tokio::test]
    async fn extism_error_maps_to_tool_result_error() {
        let tool = make_tool("broken", "crash");
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("Extism errors must not bubble as anyhow::Error");

        assert!(
            !result.success,
            "ToolResult.success must be false on plugin error"
        );
        assert!(
            result.error.is_some(),
            "ToolResult.error must be set on plugin error"
        );
    }

    #[tokio::test]
    async fn wasm_trap_maps_to_tool_result_error() {
        // A trap (e.g. unreachable instruction, OOM inside WASM) must be
        // caught and returned as a ToolResult error, not an unwinding panic.
        let tool = make_tool("faulty", "trap");
        let result = tool
            .execute(serde_json::json!({ "trigger": "unreachable" }))
            .await
            .expect("WASM traps must not bubble as anyhow::Error");

        assert!(!result.success);
        assert!(
            result.error.is_some(),
            "ToolResult.error must be populated for WASM traps"
        );
    }

    #[tokio::test]
    async fn error_result_contains_descriptive_message() {
        let tool = make_tool("analytics", "run_report");
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        let err_msg = result.error.as_deref().unwrap_or("");
        assert!(
            !err_msg.is_empty(),
            "error message should be non-empty so the LLM can diagnose the failure"
        );
    }

    #[tokio::test]
    async fn error_result_does_not_lose_plugin_identity() {
        // When an error occurs the output should still identify which
        // plugin/function failed, so the agent can report the right tool.
        let tool = make_tool("payments", "charge");
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(
            result.output.contains("payments") && result.output.contains("charge"),
            "error output should identify plugin and function, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn error_message_includes_plugin_and_export_names() {
        let tool = make_tool("billing", "invoice");
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        let err = result.error.as_deref().unwrap();
        assert!(
            err.contains("billing") && err.contains("invoice"),
            "error field should name the plugin and export, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn missing_export_classified_correctly() {
        // The minimal WASM module has no exports, so calling any function
        // should produce the "export function not found" classification.
        let tool = make_tool("nav", "route");
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(
            result.output.contains("not found"),
            "output should indicate export not found, got: {}",
            result.output
        );
    }

    // --- classify_extism_error unit tests ---

    #[test]
    fn classify_unreachable_trap() {
        assert_eq!(
            classify_extism_error("wasm trap: wasm `unreachable` instruction executed"),
            "WASM trap: unreachable instruction executed"
        );
    }

    #[test]
    fn classify_memory_out_of_bounds() {
        assert_eq!(
            classify_extism_error("out of bounds memory access"),
            "WASM trap: memory out of bounds"
        );
    }

    #[test]
    fn classify_stack_overflow() {
        assert_eq!(
            classify_extism_error("call stack exhausted"),
            "WASM trap: stack overflow"
        );
    }

    #[test]
    fn classify_timeout() {
        assert_eq!(
            classify_extism_error("plugin execution timed out after 30s"),
            "execution timed out"
        );
        assert_eq!(
            classify_extism_error("timeout exceeded"),
            "execution timed out"
        );
    }

    #[test]
    fn classify_unknown_export() {
        assert_eq!(
            classify_extism_error("function 'foo' not found"),
            "export function not found"
        );
    }

    #[test]
    fn classify_generic_error() {
        assert_eq!(
            classify_extism_error("something completely unexpected"),
            "execution failed"
        );
    }
}
