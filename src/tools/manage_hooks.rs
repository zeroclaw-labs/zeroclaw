//! Meta-tool for managing dynamic hooks at runtime via the DynamicRegistry.
//!
//! Exposes add/remove/enable/disable/list/get operations so the LLM can
//! manage the dynamic hook surface without restarting the agent.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::tools::dynamic_registry::{
    DynamicHookDef, DynamicRegistry, HookEffect, HookFilter, HookPhase, HookPoint,
};
use crate::tools::traits::{Tool, ToolResult};

/// Monotonic counter to ensure unique IDs even within the same millisecond.
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A meta-tool that delegates to [`DynamicRegistry`] for runtime hook
/// management (add, remove, enable, disable, list, get).
pub struct ManageHooksTool {
    registry: Arc<DynamicRegistry>,
}

impl ManageHooksTool {
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

/// Parse a string into a `HookPhase` enum (case-sensitive: "Pre" / "Post").
fn parse_phase(s: &str) -> Result<HookPhase, ToolResult> {
    match s {
        "Pre" => Ok(HookPhase::Pre),
        "Post" => Ok(HookPhase::Post),
        other => Err(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Invalid phase: '{other}'. Must be \"Pre\" or \"Post\"."
            )),
        }),
    }
}

/// Parse a string into a `HookPoint` enum (case-sensitive).
fn parse_target(s: &str) -> Result<HookPoint, ToolResult> {
    match s {
        "ToolCall" => Ok(HookPoint::ToolCall),
        "LlmCall" => Ok(HookPoint::LlmCall),
        "MessageReceived" => Ok(HookPoint::MessageReceived),
        "MessageSending" => Ok(HookPoint::MessageSending),
        "PromptBuild" => Ok(HookPoint::PromptBuild),
        other => Err(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Invalid target: '{other}'. Must be one of: \
                 \"ToolCall\", \"LlmCall\", \"MessageReceived\", \
                 \"MessageSending\", \"PromptBuild\"."
            )),
        }),
    }
}

#[async_trait]
impl Tool for ManageHooksTool {
    fn name(&self) -> &str {
        "manage_hooks"
    }

    fn description(&self) -> &str {
        "Meta-tool for managing dynamic hooks at runtime. Supports adding, \
         removing, enabling, disabling, listing, and inspecting dynamically \
         registered hooks without restarting the agent."
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
                    "description": "Hook name (required for add)."
                },
                "phase": {
                    "type": "string",
                    "enum": ["Pre", "Post"],
                    "description": "Hook phase (required for add)."
                },
                "target": {
                    "type": "string",
                    "enum": ["ToolCall", "LlmCall", "MessageReceived", "MessageSending", "PromptBuild"],
                    "description": "Hook target event (required for add)."
                },
                "effect": {
                    "type": "object",
                    "description": "Hook effect specification (required for add). Shape varies by type: {\"InjectPromptSuffix\": \"text\"}, {\"ModifyArgs\": {...}}, {\"Cancel\": \"reason\"}, {\"LogToChannel\": \"channel\"}."
                },
                "filter": {
                    "type": "object",
                    "description": "Optional filter with 'channel' and 'tool_name' fields.",
                    "properties": {
                        "channel": { "type": "string" },
                        "tool_name": { "type": "string" }
                    }
                },
                "priority": {
                    "type": "integer",
                    "description": "Hook priority (higher = runs first). Defaults to 0."
                },
                "id": {
                    "type": "string",
                    "description": "Hook ID (required for remove/enable/disable/get)."
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

impl ManageHooksTool {
    fn handle_add(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = match require_str(args, "name") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let phase_str = match require_str(args, "phase") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let phase = match parse_phase(phase_str) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let target_str = match require_str(args, "target") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let target = match parse_target(target_str) {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };

        let effect_value = match args.get("effect") {
            Some(v) if v.is_object() => v.clone(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: 'effect'".into()),
                });
            }
        };
        let effect: HookEffect = match serde_json::from_value(effect_value) {
            Ok(e) => e,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Invalid effect: {e}")),
                });
            }
        };

        let filter: Option<HookFilter> = args
            .get("filter")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let priority =
            i32::try_from(args.get("priority").and_then(|v| v.as_i64()).unwrap_or(0)).unwrap_or(0);

        let enabled = args
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let expected_revision = args.get("expected_revision").and_then(|v| v.as_u64());

        let now = Utc::now();
        let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("dyn-hook-{}-{seq}", now.timestamp_millis());

        let def = DynamicHookDef {
            id: id.clone(),
            name: name.clone(),
            phase,
            target,
            priority,
            enabled,
            filter,
            effect,
            created_at: now,
            updated_at: now,
        };

        match self.registry.add_hook(def, expected_revision) {
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

    fn handle_remove(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = match require_str(args, "id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self.registry.remove_hook(id) {
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

        match self.registry.enable_hook(id, true) {
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

        match self.registry.enable_hook(id, false) {
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
        let hooks = self.registry.list_hooks();
        let summaries: Vec<serde_json::Value> = hooks
            .iter()
            .map(|h| {
                let phase_str = match &h.phase {
                    HookPhase::Pre => "Pre",
                    HookPhase::Post => "Post",
                };
                let target_str = match &h.target {
                    HookPoint::ToolCall => "ToolCall",
                    HookPoint::LlmCall => "LlmCall",
                    HookPoint::MessageReceived => "MessageReceived",
                    HookPoint::MessageSending => "MessageSending",
                    HookPoint::PromptBuild => "PromptBuild",
                };
                json!({
                    "id": h.id,
                    "name": h.name,
                    "phase": phase_str,
                    "target": target_str,
                    "enabled": h.enabled,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({
                "action": "list",
                "ok": true,
                "hooks": summaries,
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

        match self.registry.get_hook(id) {
            Some(def) => {
                let output = json!({
                    "action": "get",
                    "ok": true,
                    "hook": {
                        "id": def.id,
                        "name": def.name,
                        "phase": def.phase,
                        "target": def.target,
                        "priority": def.priority,
                        "enabled": def.enabled,
                        "filter": def.filter,
                        "effect": def.effect,
                        "created_at": def.created_at.to_rfc3339(),
                        "updated_at": def.updated_at.to_rfc3339(),
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

    /// Helper: build the ManageHooksTool backed by a test registry.
    fn test_tool() -> (ManageHooksTool, Arc<DynamicRegistry>) {
        let reg = test_registry();
        let tool = ManageHooksTool::new(reg.clone());
        (tool, reg)
    }

    /// Helper: JSON args for adding a Pre+ToolCall+Cancel hook.
    fn add_pre_cancel_args(name: &str) -> serde_json::Value {
        json!({
            "action": "add",
            "name": name,
            "phase": "Pre",
            "target": "ToolCall",
            "effect": {"Cancel": "blocked by policy"}
        })
    }

    /// Helper: JSON args for adding a Post+ToolCall+LogToChannel hook.
    fn add_post_log_args(name: &str) -> serde_json::Value {
        json!({
            "action": "add",
            "name": name,
            "phase": "Post",
            "target": "ToolCall",
            "effect": {"LogToChannel": "zeroclaw_audit"}
        })
    }

    /// Helper: extract the "id" field from a successful ToolResult output.
    fn extract_id(result: &ToolResult) -> String {
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    // ------------------------------------------------------------------
    // 1. manage_hooks_name_and_schema
    // ------------------------------------------------------------------
    #[test]
    fn manage_hooks_name_and_schema() {
        let (tool, _reg) = test_tool();
        assert_eq!(tool.name(), "manage_hooks");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert_eq!(schema["required"][0], "action");
    }

    // ------------------------------------------------------------------
    // 2. manage_hooks_add_pre_hook_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_add_pre_hook_success() {
        let (tool, _reg) = test_tool();
        let result = tool
            .execute(add_pre_cancel_args("zeroclaw_pre_cancel"))
            .await
            .unwrap();

        assert!(result.success, "expected success: {:?}", result);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["action"], "add");
        assert_eq!(v["ok"], true);
        assert_eq!(v["name"], "zeroclaw_pre_cancel");
        assert!(v["id"].as_str().unwrap().starts_with("dyn-hook-"));
        assert!(v["revision"].as_u64().unwrap() > 0);
    }

    // ------------------------------------------------------------------
    // 3. manage_hooks_add_post_hook_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_add_post_hook_success() {
        let (tool, _reg) = test_tool();
        let result = tool
            .execute(add_post_log_args("zeroclaw_post_log"))
            .await
            .unwrap();

        assert!(result.success, "expected success: {:?}", result);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["action"], "add");
        assert_eq!(v["ok"], true);
        assert_eq!(v["name"], "zeroclaw_post_log");
        assert!(v["id"].as_str().unwrap().starts_with("dyn-hook-"));
    }

    // ------------------------------------------------------------------
    // 4. manage_hooks_list_empty
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_list_empty() {
        let (tool, _reg) = test_tool();
        let result = tool.execute(json!({"action": "list"})).await.unwrap();

        assert!(result.success);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["action"], "list");
        assert_eq!(v["ok"], true);
        assert_eq!(v["hooks"].as_array().unwrap().len(), 0);
    }

    // ------------------------------------------------------------------
    // 5. manage_hooks_list_after_add
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_list_after_add() {
        let (tool, _reg) = test_tool();

        // Add a hook first.
        let add_result = tool
            .execute(add_pre_cancel_args("zeroclaw_lister"))
            .await
            .unwrap();
        assert!(add_result.success);

        // Now list.
        let list_result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(list_result.success);

        let v: serde_json::Value = serde_json::from_str(&list_result.output).unwrap();
        let hooks = v["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["name"], "zeroclaw_lister");
        assert_eq!(hooks[0]["phase"], "Pre");
        assert_eq!(hooks[0]["target"], "ToolCall");
        assert_eq!(hooks[0]["enabled"], true);
    }

    // ------------------------------------------------------------------
    // 6. manage_hooks_get_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_get_success() {
        let (tool, _reg) = test_tool();

        let add_result = tool
            .execute(add_pre_cancel_args("zeroclaw_getter"))
            .await
            .unwrap();
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
        assert_eq!(v["hook"]["name"], "zeroclaw_getter");
        assert_eq!(v["hook"]["priority"], 0);
        assert_eq!(v["hook"]["enabled"], true);
        assert!(v["hook"]["created_at"].as_str().is_some());
    }

    // ------------------------------------------------------------------
    // 7. manage_hooks_get_not_found
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_get_not_found() {
        let (tool, _reg) = test_tool();

        let result = tool
            .execute(json!({"action": "get", "id": "nonexistent-id"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not found"));
    }

    // ------------------------------------------------------------------
    // 8. manage_hooks_remove_success
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_remove_success() {
        let (tool, reg) = test_tool();

        let add_result = tool
            .execute(add_pre_cancel_args("zeroclaw_removable"))
            .await
            .unwrap();
        assert!(add_result.success);
        let id = extract_id(&add_result);

        // Verify it exists.
        assert!(reg.get_hook(&id).is_some());

        let remove_result = tool
            .execute(json!({"action": "remove", "id": id}))
            .await
            .unwrap();
        assert!(remove_result.success);

        let v: serde_json::Value = serde_json::from_str(&remove_result.output).unwrap();
        assert_eq!(v["action"], "remove");
        assert_eq!(v["ok"], true);

        // Verify it was removed.
        assert!(reg.get_hook(&id).is_none());
    }

    // ------------------------------------------------------------------
    // 9. manage_hooks_enable_disable
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_enable_disable() {
        let (tool, reg) = test_tool();

        let add_result = tool
            .execute(add_pre_cancel_args("zeroclaw_toggle"))
            .await
            .unwrap();
        assert!(add_result.success);
        let id = extract_id(&add_result);
        assert!(reg.get_hook(&id).unwrap().enabled);

        // Disable.
        let disable_result = tool
            .execute(json!({"action": "disable", "id": id}))
            .await
            .unwrap();
        assert!(disable_result.success);
        assert!(!reg.get_hook(&id).unwrap().enabled);

        // Enable.
        let enable_result = tool
            .execute(json!({"action": "enable", "id": id}))
            .await
            .unwrap();
        assert!(enable_result.success);
        assert!(reg.get_hook(&id).unwrap().enabled);
    }

    // ------------------------------------------------------------------
    // 10. manage_hooks_unknown_action
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_unknown_action() {
        let (tool, _reg) = test_tool();

        let result = tool.execute(json!({"action": "explode"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
        assert!(result.error.as_ref().unwrap().contains("explode"));
    }

    // ------------------------------------------------------------------
    // 11. manage_hooks_add_missing_name
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_add_missing_name() {
        let (tool, _reg) = test_tool();

        let result = tool
            .execute(json!({
                "action": "add",
                "phase": "Pre",
                "target": "ToolCall",
                "effect": {"Cancel": "reason"}
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("'name'"));
    }

    // ------------------------------------------------------------------
    // 12. manage_hooks_add_invalid_phase_effect
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_add_invalid_phase_effect() {
        let (tool, _reg) = test_tool();

        // Post + Cancel is not a valid combination.
        let result = tool
            .execute(json!({
                "action": "add",
                "name": "zeroclaw_bad_combo",
                "phase": "Post",
                "target": "ToolCall",
                "effect": {"Cancel": "should fail"}
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let err_msg = result.error.as_ref().unwrap();
        assert!(
            err_msg.contains("Cancel") && err_msg.contains("Post"),
            "expected phase-effect validation error, got: {err_msg}"
        );
    }

    // ------------------------------------------------------------------
    // 13. manage_hooks_add_invalid_phase
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_add_invalid_phase() {
        let (tool, _reg) = test_tool();

        let result = tool
            .execute(json!({
                "action": "add",
                "name": "zeroclaw_bad_phase",
                "phase": "During",
                "target": "ToolCall",
                "effect": {"Cancel": "reason"}
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let err_msg = result.error.as_ref().unwrap();
        assert!(
            err_msg.contains("Invalid phase") && err_msg.contains("During"),
            "expected invalid phase error, got: {err_msg}"
        );
    }

    // ------------------------------------------------------------------
    // 14. manage_hooks_add_unique_ids
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn manage_hooks_add_unique_ids() {
        let (tool, _reg) = test_tool();

        let r1 = tool
            .execute(add_pre_cancel_args("zeroclaw_unique_a"))
            .await
            .unwrap();
        let r2 = tool
            .execute(add_pre_cancel_args("zeroclaw_unique_b"))
            .await
            .unwrap();
        assert!(r1.success);
        assert!(r2.success);

        let id1 = extract_id(&r1);
        let id2 = extract_id(&r2);
        assert_ne!(id1, id2, "IDs must be unique across rapid adds");
    }
}
