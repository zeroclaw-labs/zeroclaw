//! Meta-tool for managing dynamic tools at runtime via the DynamicRegistry.
//!
//! Exposes add/remove/enable/disable/list/get operations so the LLM can
//! manage the dynamic tool surface without restarting the agent.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::tools::dynamic_registry::{DynamicRegistry, DynamicToolDef};
use crate::tools::traits::{Tool, ToolResult};

/// Monotonic counter to ensure unique IDs even within the same millisecond.
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A meta-tool that delegates to [`DynamicRegistry`] for runtime tool
/// management (add, remove, enable, disable, list, get).
pub struct ManageToolsTool {
    registry: Arc<DynamicRegistry>,
}

impl ManageToolsTool {
    pub fn new(registry: Arc<DynamicRegistry>) -> Self {
        Self { registry }
    }
}

/// Extract a required string field from the args object, returning an error
/// `ToolResult` on missing or non-string values.
fn require_str<'a>(args: &'a serde_json::Value, field: &str) -> Result<&'a str, ToolResult> {
    args.get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Missing required parameter: '{field}'")),
        })
}

#[async_trait]
impl Tool for ManageToolsTool {
    fn name(&self) -> &str {
        "manage_tools"
    }

    fn description(&self) -> &str {
        "Meta-tool for managing dynamic tools at runtime. Supports adding, \
         removing, enabling, disabling, listing, and inspecting dynamically \
         registered tools without restarting the agent."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "remove", "enable", "disable", "list", "get"],
                    "description": "The management action to perform."
                },
                "name": {
                    "type": "string",
                    "description": "Tool name (required for add)."
                },
                "description": {
                    "type": "string",
                    "description": "Tool description (required for add)."
                },
                "kind": {
                    "type": "string",
                    "description": "Tool kind, e.g. 'shell_command' or 'http_endpoint' (required for add)."
                },
                "config": {
                    "type": "object",
                    "description": "Kind-specific configuration (required for add)."
                },
                "id": {
                    "type": "string",
                    "description": "Tool ID (required for remove/enable/disable/get)."
                },
                "expected_revision": {
                    "type": "integer",
                    "description": "Optional optimistic concurrency revision check (used by add)."
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Initial enabled state for add (defaults to true)."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: 'action'".into()),
                });
            }
        };

        match action {
            "add" => self.handle_add(&args),
            "remove" => self.handle_remove(&args),
            "enable" => self.handle_enable(&args),
            "disable" => self.handle_disable(&args),
            "list" => self.handle_list(),
            "get" => self.handle_get(&args),
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: '{other}'")),
            }),
        }
    }
}

impl ManageToolsTool {
    fn handle_add(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = match require_str(args, "name") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let description = match require_str(args, "description") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let kind = match require_str(args, "kind") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let config = args.get("config").cloned().unwrap_or_else(|| json!({}));

        let enabled = args
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let expected_revision = args.get("expected_revision").and_then(|v| v.as_u64());

        let now = Utc::now();
        let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("dyn-tool-{}-{seq}", now.timestamp_millis());

        let def = DynamicToolDef {
            id: id.clone(),
            name: name.clone(),
            description,
            kind,
            config,
            enabled,
            created_at: now,
            updated_at: now,
            created_by: None,
        };

        match self.registry.add_tool(def, expected_revision) {
            Ok(revision) => Ok(ToolResult {
                success: true,
                output: json!({
                    "action": "add",
                    "ok": true,
                    "id": id,
                    "name": name,
                    "revision": revision,
                })
                .to_string(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    /// Note: `remove`, `enable`, and `disable` do not accept `expected_revision`
    /// because `DynamicRegistry` applies these mutations unconditionally on
    /// existing IDs. The registry returns `NotFound` if the ID doesn't exist,
    /// which is sufficient for idempotent operations.
    fn handle_remove(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = match require_str(args, "id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self.registry.remove_tool(id) {
            Ok(revision) => Ok(ToolResult {
                success: true,
                output: json!({
                    "action": "remove",
                    "ok": true,
                    "id": id,
                    "revision": revision,
                })
                .to_string(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    fn handle_enable(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = match require_str(args, "id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self.registry.enable_tool(id, true) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: json!({
                    "action": "enable",
                    "ok": true,
                    "id": id,
                })
                .to_string(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    fn handle_disable(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = match require_str(args, "id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self.registry.enable_tool(id, false) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: json!({
                    "action": "disable",
                    "ok": true,
                    "id": id,
                })
                .to_string(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    fn handle_list(&self) -> anyhow::Result<ToolResult> {
        let tools = self.registry.list_tools();
        let summaries: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "name": t.name,
                    "kind": t.kind,
                    "enabled": t.enabled,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({
                "action": "list",
                "ok": true,
                "tools": summaries,
            })
            .to_string(),
            error: None,
        })
    }

    fn handle_get(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = match require_str(args, "id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self.registry.get_tool(id) {
            Some(def) => {
                let output = json!({
                    "action": "get",
                    "ok": true,
                    "tool": {
                        "id": def.id,
                        "name": def.name,
                        "description": def.description,
                        "kind": def.kind,
                        "config": def.config,
                        "enabled": def.enabled,
                        "created_at": def.created_at.to_rfc3339(),
                        "updated_at": def.updated_at.to_rfc3339(),
                        "created_by": def.created_by,
                    },
                });
                Ok(ToolResult {
                    success: true,
                    output: output.to_string(),
                    error: None,
                })
            }
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("not found: '{id}'")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DynamicRegistryConfig;

    /// Helper: build an Arc<DynamicRegistry> for testing.
    fn test_registry() -> Arc<DynamicRegistry> {
        Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()))
    }

    /// Helper: build the ManageToolsTool backed by a test registry.
    fn test_tool() -> (ManageToolsTool, Arc<DynamicRegistry>) {
        let reg = test_registry();
        let tool = ManageToolsTool::new(reg.clone());
        (tool, reg)
    }

    /// Helper: JSON args for adding a shell_command tool.
    fn add_args(name: &str) -> serde_json::Value {
        json!({
            "action": "add",
            "name": name,
            "description": format!("Dynamic tool {name}"),
            "kind": "shell_command",
            "config": {
                "command": "echo",
                "args": [{"Fixed": "hello"}]
            }
        })
    }

    /// Helper: extract the "id" field from a successful ToolResult output.
    fn extract_id(result: &ToolResult) -> String {
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    // ------------------------------------------------------------------
    // 1. manage_tools_name_and_schema
    // ------------------------------------------------------------------
    #[test]
    fn manage_tools_name_and_schema() {
        let (tool, _reg) = test_tool();
        assert_eq!(tool.name(), "manage_tools");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert_eq!(schema["required"][0], "action");
    }

    // ------------------------------------------------------------------
    // 2. manage_tools_add_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_add_success() {
        let (tool, _reg) = test_tool();
        let result = tool.execute(add_args("zeroclaw_echo")).await.unwrap();

        assert!(result.success, "expected success: {:?}", result);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["action"], "add");
        assert_eq!(v["ok"], true);
        assert_eq!(v["name"], "zeroclaw_echo");
        assert!(v["id"].as_str().unwrap().starts_with("dyn-tool-"));
        assert!(v["revision"].as_u64().unwrap() > 0);
    }

    // ------------------------------------------------------------------
    // 3. manage_tools_list_empty
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_list_empty() {
        let (tool, _reg) = test_tool();
        let result = tool.execute(json!({"action": "list"})).await.unwrap();

        assert!(result.success);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["action"], "list");
        assert_eq!(v["ok"], true);
        assert_eq!(v["tools"].as_array().unwrap().len(), 0);
    }

    // ------------------------------------------------------------------
    // 4. manage_tools_list_after_add
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_list_after_add() {
        let (tool, _reg) = test_tool();

        // Add a tool first.
        let add_result = tool.execute(add_args("zeroclaw_lister")).await.unwrap();
        assert!(add_result.success);

        // Now list.
        let list_result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(list_result.success);

        let v: serde_json::Value = serde_json::from_str(&list_result.output).unwrap();
        let tools = v["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "zeroclaw_lister");
        assert_eq!(tools[0]["kind"], "shell_command");
        assert_eq!(tools[0]["enabled"], true);
    }

    // ------------------------------------------------------------------
    // 5. manage_tools_get_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_get_success() {
        let (tool, _reg) = test_tool();

        let add_result = tool.execute(add_args("zeroclaw_getter")).await.unwrap();
        assert!(add_result.success);
        let id = extract_id(&add_result);

        let get_result = tool
            .execute(json!({"action": "get", "id": id}))
            .await
            .unwrap();
        assert!(get_result.success);

        let v: serde_json::Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(v["action"], "get");
        assert_eq!(v["ok"], true);
        assert_eq!(v["tool"]["name"], "zeroclaw_getter");
        assert_eq!(v["tool"]["kind"], "shell_command");
        assert_eq!(v["tool"]["enabled"], true);
        assert!(v["tool"]["created_at"].as_str().is_some());
    }

    // ------------------------------------------------------------------
    // 6. manage_tools_get_not_found
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_get_not_found() {
        let (tool, _reg) = test_tool();

        let result = tool
            .execute(json!({"action": "get", "id": "nonexistent-id"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not found"));
    }

    // ------------------------------------------------------------------
    // 7. manage_tools_remove_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_remove_success() {
        let (tool, reg) = test_tool();

        let add_result = tool.execute(add_args("zeroclaw_removable")).await.unwrap();
        assert!(add_result.success);
        let id = extract_id(&add_result);

        // Verify it exists.
        assert!(reg.get_tool(&id).is_some());

        let remove_result = tool
            .execute(json!({"action": "remove", "id": id}))
            .await
            .unwrap();
        assert!(remove_result.success);

        let v: serde_json::Value = serde_json::from_str(&remove_result.output).unwrap();
        assert_eq!(v["action"], "remove");
        assert_eq!(v["ok"], true);

        // Verify it was removed.
        assert!(reg.get_tool(&id).is_none());
    }

    // ------------------------------------------------------------------
    // 8. manage_tools_enable_disable
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_enable_disable() {
        let (tool, reg) = test_tool();

        let add_result = tool.execute(add_args("zeroclaw_toggle")).await.unwrap();
        assert!(add_result.success);
        let id = extract_id(&add_result);
        assert!(reg.get_tool(&id).unwrap().enabled);

        // Disable.
        let disable_result = tool
            .execute(json!({"action": "disable", "id": id}))
            .await
            .unwrap();
        assert!(disable_result.success);
        assert!(!reg.get_tool(&id).unwrap().enabled);

        // Enable.
        let enable_result = tool
            .execute(json!({"action": "enable", "id": id}))
            .await
            .unwrap();
        assert!(enable_result.success);
        assert!(reg.get_tool(&id).unwrap().enabled);
    }

    // ------------------------------------------------------------------
    // 9. manage_tools_unknown_action
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_unknown_action() {
        let (tool, _reg) = test_tool();

        let result = tool.execute(json!({"action": "explode"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
        assert!(result.error.as_ref().unwrap().contains("explode"));
    }

    // ------------------------------------------------------------------
    // 10. manage_tools_add_missing_name
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_add_missing_name() {
        let (tool, _reg) = test_tool();

        let result = tool
            .execute(json!({
                "action": "add",
                "description": "no name",
                "kind": "shell_command",
                "config": {}
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("'name'"));
    }

    // ------------------------------------------------------------------
    // 11. manage_tools_add_missing_kind
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_add_missing_kind() {
        let (tool, _reg) = test_tool();

        let result = tool
            .execute(json!({
                "action": "add",
                "name": "zeroclaw_no_kind",
                "description": "missing kind",
                "config": {}
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("'kind'"));
    }

    // ------------------------------------------------------------------
    // 12. manage_tools cannot remove/disable itself (static tool guard)
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_cannot_remove_static_tool() {
        let (tool, _reg) = test_tool();

        // "manage_tools" is a static tool, never in the dynamic registry.
        // Attempting to remove it by any ID should fail with NotFound.
        let result = tool
            .execute(json!({"action": "remove", "id": "manage_tools"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not found"));
    }

    // ------------------------------------------------------------------
    // 13. IDs are unique across rapid successive adds
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_tools_add_unique_ids() {
        let (tool, _reg) = test_tool();

        let r1 = tool.execute(add_args("zeroclaw_unique_a")).await.unwrap();
        let r2 = tool.execute(add_args("zeroclaw_unique_b")).await.unwrap();
        assert!(r1.success);
        assert!(r2.success);

        let id1 = extract_id(&r1);
        let id2 = extract_id(&r2);
        assert_ne!(id1, id2, "IDs must be unique across rapid adds");
    }
}
