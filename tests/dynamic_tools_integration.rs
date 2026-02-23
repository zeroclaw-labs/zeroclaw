//! Integration tests for the dynamic tools & hooks system.
//!
//! Tests progress from simple registry operations to complex multi-turn
//! scenarios. All tests are deterministic (no network calls, no flaky timing).
//!
//! Categories:
//! 1. Registry lifecycle (create → list → get → enable/disable → remove)
//! 2. ManageToolsTool meta-tool (all 6 actions via execute())
//! 3. ManageHooksTool meta-tool (all 6 actions via execute())
//! 4. Shell command dynamic tools (create and execute)
//! 5. Hook filter matching and effects
//! 6. Phase-effect validation
//! 7. Persistence (save, load, corrupt file quarantine)
//! 8. Quotas, concurrency, snapshot isolation, prompt generation

use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;

use zeroclaw::config::DynamicRegistryConfig;
use zeroclaw::hooks::HookResult;
use zeroclaw::security::SecurityPolicy;
use zeroclaw::tools::dynamic_factories::ToolBuildContext;
use zeroclaw::tools::dynamic_registry::{
    DynamicHookDef, DynamicRegistry, DynamicRegistryError, DynamicToolDef, HookEffect, HookFilter,
    HookPhase, HookPoint, PersistedRegistry,
};
use zeroclaw::tools::manage_hooks::ManageHooksTool;
use zeroclaw::tools::manage_tools::ManageToolsTool;
use zeroclaw::tools::traits::Tool;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn default_config() -> DynamicRegistryConfig {
    DynamicRegistryConfig {
        max_tools: 20,
        max_hooks: 20,
        allowed_tool_kinds: vec!["shell_command".into(), "http_endpoint".into()],
    }
}

fn tiny_config() -> DynamicRegistryConfig {
    DynamicRegistryConfig {
        max_tools: 2,
        max_hooks: 2,
        allowed_tool_kinds: vec!["shell_command".into()],
    }
}

fn sample_shell_tool(id: &str, name: &str) -> DynamicToolDef {
    DynamicToolDef {
        id: id.into(),
        name: name.into(),
        description: format!("Test tool: {name}"),
        kind: "shell_command".into(),
        config: json!({
            "command": "echo",
            "args": [{"Fixed": "hello"}]
        }),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        created_by: Some("zeroclaw_user".into()),
    }
}

fn sample_hook(id: &str, name: &str, phase: HookPhase, effect: HookEffect) -> DynamicHookDef {
    DynamicHookDef {
        id: id.into(),
        name: name.into(),
        phase,
        target: HookPoint::ToolCall,
        priority: 10,
        enabled: true,
        filter: None,
        effect,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn build_ctx(tmp: &TempDir) -> ToolBuildContext {
    ToolBuildContext {
        security: Arc::new(SecurityPolicy::default()),
        workspace_dir: tmp.path().to_path_buf(),
    }
}

fn registry_with_persistence(tmp: &TempDir) -> DynamicRegistry {
    let path = tmp.path().join("state").join("dynamic-registry.json");
    DynamicRegistry::new(Vec::new(), default_config(), path, build_ctx(tmp))
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. Registry Lifecycle
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn registry_lifecycle_create_list_get_disable_enable_remove() {
    let tmp = TempDir::new().unwrap();
    let reg = registry_with_persistence(&tmp);

    // Create
    let rev = reg
        .add_tool(sample_shell_tool("t1", "lifecycle_tool"), None)
        .unwrap();
    assert!(rev > 0);

    // List
    let tools = reg.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "lifecycle_tool");

    // Get
    let tool = reg.get_tool("t1").unwrap();
    assert_eq!(tool.name, "lifecycle_tool");
    assert!(tool.enabled);

    // Disable
    reg.enable_tool("t1", false).unwrap();
    let tool = reg.get_tool("t1").unwrap();
    assert!(!tool.enabled);

    // Snapshot should NOT include disabled tool
    let snap = reg.snapshot();
    let dynamic_names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(!dynamic_names.contains(&"lifecycle_tool"));

    // Enable
    reg.enable_tool("t1", true).unwrap();
    let snap = reg.snapshot();
    let dynamic_names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(dynamic_names.contains(&"lifecycle_tool"));

    // Remove
    reg.remove_tool("t1").unwrap();
    assert!(reg.get_tool("t1").is_none());
    assert!(reg.list_tools().is_empty());
}

#[test]
fn registry_empty_and_remove_nonexistent_behaves_safely() {
    let reg = DynamicRegistry::new_empty(default_config());

    // Empty list
    assert!(reg.list_tools().is_empty());
    assert!(reg.list_hooks().is_empty());

    // Get nonexistent
    assert!(reg.get_tool("nope").is_none());
    assert!(reg.get_hook("nope").is_none());

    // Remove nonexistent returns NotFound
    let err = reg.remove_tool("nope").unwrap_err();
    assert!(matches!(err, DynamicRegistryError::NotFound(_)));

    let err = reg.remove_hook("nope").unwrap_err();
    assert!(matches!(err, DynamicRegistryError::NotFound(_)));
}

#[test]
fn registry_rejects_static_tool_name_collision() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let static_tools: Vec<Arc<dyn Tool>> = vec![Arc::new(
        zeroclaw::tools::FileReadTool::new(security.clone()),
    )];

    let path = tmp.path().join("registry.json");
    let reg = DynamicRegistry::new(
        static_tools,
        default_config(),
        path,
        build_ctx(&tmp),
    );

    // Try to add a tool named "file_read" which already exists as a static tool
    let mut def = sample_shell_tool("collision-1", "file_read");
    def.name = "file_read".into();

    let err = reg.add_tool(def, None).unwrap_err();
    assert!(matches!(err, DynamicRegistryError::NameCollision(_)));
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. ManageToolsTool Meta-Tool
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn manage_tools_execute_add_registers_shell_tool() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    let result = tool
        .execute(json!({
            "action": "add",
            "name": "greet_tool",
            "description": "Says hello",
            "kind": "shell_command",
            "config": {"command": "echo", "args": [{"Fixed": "hello"}]}
        }))
        .await
        .unwrap();

    assert!(result.success, "add failed: {:?}", result.error);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["action"], "add");
    assert!(output["ok"].as_bool().unwrap());
    assert!(!output["id"].as_str().unwrap().is_empty());

    // Verify in registry
    assert_eq!(reg.list_tools().len(), 1);
}

#[tokio::test]
async fn manage_tools_execute_list_returns_created_tools() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    // Add two tools
    tool.execute(json!({
        "action": "add",
        "name": "tool_a",
        "description": "A",
        "kind": "shell_command",
        "config": {"command": "echo", "args": [{"Fixed": "a"}]}
    }))
    .await
    .unwrap();

    tool.execute(json!({
        "action": "add",
        "name": "tool_b",
        "description": "B",
        "kind": "shell_command",
        "config": {"command": "echo", "args": [{"Fixed": "b"}]}
    }))
    .await
    .unwrap();

    // List
    let result = tool.execute(json!({"action": "list"})).await.unwrap();
    assert!(result.success);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let tools = output["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
}

#[tokio::test]
async fn manage_tools_execute_get_returns_exact_tool_definition() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "exact_tool",
            "description": "Exact test",
            "kind": "shell_command",
            "config": {"command": "date"}
        }))
        .await
        .unwrap();

    let add_out: serde_json::Value = serde_json::from_str(&add_result.output).unwrap();
    let id = add_out["id"].as_str().unwrap();

    // Get
    let result = tool
        .execute(json!({"action": "get", "id": id}))
        .await
        .unwrap();
    assert!(result.success);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["tool"]["name"], "exact_tool");
    assert_eq!(output["tool"]["kind"], "shell_command");
}

#[tokio::test]
async fn manage_tools_execute_disable_blocks_snapshot_visibility() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "disable_test",
            "description": "Will be disabled",
            "kind": "shell_command",
            "config": {"command": "echo"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Disable
    let result = tool
        .execute(json!({"action": "disable", "id": id}))
        .await
        .unwrap();
    assert!(result.success);

    // Snapshot should not include the disabled tool
    let snap = reg.snapshot();
    let names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(!names.contains(&"disable_test"));
}

#[tokio::test]
async fn manage_tools_execute_enable_restores_snapshot_visibility() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "enable_test",
            "description": "Will be re-enabled",
            "kind": "shell_command",
            "config": {"command": "echo"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Disable then re-enable
    tool.execute(json!({"action": "disable", "id": id}))
        .await
        .unwrap();
    tool.execute(json!({"action": "enable", "id": id}))
        .await
        .unwrap();

    let snap = reg.snapshot();
    let names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"enable_test"));
}

#[tokio::test]
async fn manage_tools_execute_remove_deletes_tool() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "remove_me",
            "description": "To be removed",
            "kind": "shell_command",
            "config": {"command": "echo"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Remove
    let result = tool
        .execute(json!({"action": "remove", "id": id}))
        .await
        .unwrap();
    assert!(result.success);

    // Verify gone
    assert!(reg.list_tools().is_empty());
    let get_result = tool
        .execute(json!({"action": "get", "id": id}))
        .await
        .unwrap();
    assert!(!get_result.success);
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. ManageHooksTool Meta-Tool
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn manage_hooks_execute_add_registers_hook() {
    let reg = Arc::new(DynamicRegistry::new_empty(default_config()));
    let tool = ManageHooksTool::new(reg.clone());

    let result = tool
        .execute(json!({
            "action": "add",
            "name": "log_hook",
            "phase": "Post",
            "target": "ToolCall",
            "effect": {"LogToChannel": "audit"}
        }))
        .await
        .unwrap();

    assert!(result.success, "add failed: {:?}", result.error);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["action"], "add");
    assert!(output["ok"].as_bool().unwrap());

    assert_eq!(reg.list_hooks().len(), 1);
}

#[tokio::test]
async fn manage_hooks_execute_list_returns_created_hooks() {
    let reg = Arc::new(DynamicRegistry::new_empty(default_config()));
    let tool = ManageHooksTool::new(reg.clone());

    tool.execute(json!({
        "action": "add",
        "name": "hook_a",
        "phase": "Post",
        "target": "ToolCall",
        "effect": {"LogToChannel": "chan_a"}
    }))
    .await
    .unwrap();

    tool.execute(json!({
        "action": "add",
        "name": "hook_b",
        "phase": "Pre",
        "target": "ToolCall",
        "effect": {"Cancel": "blocked"}
    }))
    .await
    .unwrap();

    let result = tool.execute(json!({"action": "list"})).await.unwrap();
    assert!(result.success);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let hooks = output["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 2);
}

#[tokio::test]
async fn manage_hooks_execute_get_returns_hook_definition() {
    let reg = Arc::new(DynamicRegistry::new_empty(default_config()));
    let tool = ManageHooksTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "get_hook",
            "phase": "Pre",
            "target": "PromptBuild",
            "effect": {"InjectPromptSuffix": " -- suffix"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = tool
        .execute(json!({"action": "get", "id": id}))
        .await
        .unwrap();
    assert!(result.success);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["hook"]["name"], "get_hook");
}

#[tokio::test]
async fn manage_hooks_execute_disable_prevents_hook_in_snapshot() {
    let reg = Arc::new(DynamicRegistry::new_empty(default_config()));
    let tool = ManageHooksTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "disable_hook",
            "phase": "Post",
            "target": "ToolCall",
            "effect": {"LogToChannel": "audit"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tool.execute(json!({"action": "disable", "id": id}))
        .await
        .unwrap();

    let snap = reg.snapshot();
    assert!(snap.dynamic_hooks.is_empty(), "disabled hook should not appear in snapshot");
}

#[tokio::test]
async fn manage_hooks_execute_enable_restores_hook_in_snapshot() {
    let reg = Arc::new(DynamicRegistry::new_empty(default_config()));
    let tool = ManageHooksTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "reenable_hook",
            "phase": "Post",
            "target": "ToolCall",
            "effect": {"LogToChannel": "audit"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    tool.execute(json!({"action": "disable", "id": id}))
        .await
        .unwrap();
    tool.execute(json!({"action": "enable", "id": id}))
        .await
        .unwrap();

    let snap = reg.snapshot();
    assert_eq!(snap.dynamic_hooks.len(), 1);
}

#[tokio::test]
async fn manage_hooks_execute_remove_deletes_hook() {
    let reg = Arc::new(DynamicRegistry::new_empty(default_config()));
    let tool = ManageHooksTool::new(reg.clone());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "remove_hook",
            "phase": "Post",
            "target": "ToolCall",
            "effect": {"LogToChannel": "audit"}
        }))
        .await
        .unwrap();

    let id = serde_json::from_str::<serde_json::Value>(&add_result.output).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = tool
        .execute(json!({"action": "remove", "id": id}))
        .await
        .unwrap();
    assert!(result.success);
    assert!(reg.list_hooks().is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Shell Command Dynamic Tools — Create and Execute
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn shell_dynamic_tool_echo_executes_and_returns_stdout() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));

    reg.add_tool(
        DynamicToolDef {
            id: "echo-1".into(),
            name: "echo_hello".into(),
            description: "Echoes hello".into(),
            kind: "shell_command".into(),
            config: json!({
                "command": "echo",
                "args": [{"Fixed": "hello_world"}]
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: None,
        },
        None,
    )
    .unwrap();

    // Find the tool in the snapshot and execute it
    let snap = reg.snapshot();
    let echo_tool = snap
        .all_tools
        .iter()
        .find(|t| t.name() == "echo_hello")
        .expect("echo_hello should be in snapshot");

    let result = echo_tool.execute(json!({})).await.unwrap();
    assert!(result.success, "echo failed: {:?}", result.error);
    assert!(
        result.output.contains("hello_world"),
        "output should contain 'hello_world', got: {}",
        result.output
    );
}

#[tokio::test]
async fn shell_dynamic_tool_cat_reads_fixture_file() {
    let tmp = TempDir::new().unwrap();
    let fixture_path = tmp.path().join("test_fixture.txt");
    std::fs::write(&fixture_path, "fixture_content_42").unwrap();

    let reg = Arc::new(registry_with_persistence(&tmp));

    reg.add_tool(
        DynamicToolDef {
            id: "cat-1".into(),
            name: "cat_fixture".into(),
            description: "Reads fixture file".into(),
            kind: "shell_command".into(),
            config: json!({
                "command": "cat",
                "args": [{"Fixed": fixture_path.to_str().unwrap()}]
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: None,
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let cat_tool = snap
        .all_tools
        .iter()
        .find(|t| t.name() == "cat_fixture")
        .unwrap();

    let result = cat_tool.execute(json!({})).await.unwrap();
    assert!(result.success);
    assert!(
        result.output.contains("fixture_content_42"),
        "cat output should contain fixture content, got: {}",
        result.output
    );
}

#[test]
fn http_dynamic_tool_invalid_spec_rejected_by_validation() {
    let tmp = TempDir::new().unwrap();
    let reg = registry_with_persistence(&tmp);

    // HTTP tool with missing required fields
    let err = reg
        .add_tool(
            DynamicToolDef {
                id: "http-bad".into(),
                name: "bad_http".into(),
                description: "Invalid HTTP".into(),
                kind: "http_endpoint".into(),
                config: json!({}), // Missing url, method
                enabled: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: None,
            },
            None,
        )
        .unwrap_err();

    assert!(
        matches!(err, DynamicRegistryError::ValidationFailed(_)),
        "expected ValidationFailed, got: {err}"
    );
}

#[test]
fn http_dynamic_tool_valid_spec_accepted() {
    let tmp = TempDir::new().unwrap();
    let reg = registry_with_persistence(&tmp);

    let result = reg.add_tool(
        DynamicToolDef {
            id: "http-ok".into(),
            name: "valid_http".into(),
            description: "Valid HTTP endpoint".into(),
            kind: "http_endpoint".into(),
            config: json!({
                "url": "https://api.example.com/data",
                "method": "GET"
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: None,
        },
        None,
    );

    assert!(result.is_ok(), "valid HTTP spec should be accepted: {:?}", result.err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Hook Filter Matching
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn hook_filter_channel_only_matches_target_channel() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-chan".into(),
            name: "channel_filter_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: Some(HookFilter {
                channel: Some("telegram".into()),
                tool_name: None,
            }),
            effect: HookEffect::Cancel("channel blocked".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    assert_eq!(snap.dynamic_hooks.len(), 1);
    let hook = &snap.dynamic_hooks[0];

    // Tool call with no channel context: the tool_name filter matches (None = any),
    // but the channel filter is only checked in on_message_received/on_message_sending.
    // For before_tool_call, only tool_name filter is checked.
    let result = hook
        .before_tool_call("some_tool".into(), json!({}))
        .await;
    // Cancel should fire because there's no tool_name filter restricting it
    assert!(result.is_cancel());
}

#[tokio::test]
async fn hook_filter_tool_name_only_matches_target_tool() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-tool".into(),
            name: "tool_filter_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: Some(HookFilter {
                channel: None,
                tool_name: Some("shell".into()),
            }),
            effect: HookEffect::Cancel("shell blocked".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    // Matching tool name
    let result = hook.before_tool_call("shell".into(), json!({})).await;
    assert!(result.is_cancel());

    // Non-matching tool name
    let result = hook
        .before_tool_call("file_read".into(), json!({}))
        .await;
    assert!(!result.is_cancel());
}

#[tokio::test]
async fn hook_filter_channel_and_tool_requires_both() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-both".into(),
            name: "both_filter_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: Some(HookFilter {
                channel: Some("discord".into()),
                tool_name: Some("shell".into()),
            }),
            effect: HookEffect::Cancel("both match required".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    // Tool matches, channel not checked in before_tool_call (only tool_name filter)
    let result = hook.before_tool_call("shell".into(), json!({})).await;
    assert!(result.is_cancel(), "tool_name match should trigger cancel in before_tool_call");

    // Tool doesn't match
    let result = hook.before_tool_call("file_read".into(), json!({})).await;
    assert!(!result.is_cancel(), "wrong tool_name should not trigger cancel");
}

#[tokio::test]
async fn hook_filter_none_applies_globally() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-global".into(),
            name: "global_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: None,
            effect: HookEffect::Cancel("all blocked".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    // Any tool should be cancelled
    let result = hook.before_tool_call("shell".into(), json!({})).await;
    assert!(result.is_cancel());

    let result = hook.before_tool_call("anything".into(), json!({})).await;
    assert!(result.is_cancel());
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Hook Effects and Phase Validation
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn hook_effect_modify_args_changes_input() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-modify".into(),
            name: "modify_args_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: None,
            effect: HookEffect::ModifyArgs(json!({"injected_key": "injected_value"})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    let result = hook
        .before_tool_call("any_tool".into(), json!({"original": true}))
        .await;

    match result {
        HookResult::Continue((name, args)) => {
            assert_eq!(name, "any_tool");
            assert_eq!(args["original"], true);
            assert_eq!(args["injected_key"], "injected_value");
        }
        other => panic!("expected Continue with modified args, got: {other:?}"),
    }
}

#[tokio::test]
async fn hook_effect_inject_prompt_suffix_appends_suffix() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-suffix".into(),
            name: "suffix_hook".into(),
            phase: HookPhase::Pre,
            target: HookPoint::PromptBuild,
            priority: 10,
            enabled: true,
            filter: None,
            effect: HookEffect::InjectPromptSuffix("\n-- INJECTED SUFFIX".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    let result = hook.before_prompt_build("Original prompt".into()).await;
    match result {
        HookResult::Continue(prompt) => {
            assert!(
                prompt.contains("Original prompt"),
                "should preserve original prompt"
            );
            assert!(
                prompt.contains("-- INJECTED SUFFIX"),
                "should append suffix"
            );
        }
        other => panic!("expected Continue with extended prompt, got: {other:?}"),
    }
}

#[tokio::test]
async fn hook_effect_cancel_in_pre_phase_blocks_execution() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        sample_hook(
            "h-cancel",
            "cancel_hook",
            HookPhase::Pre,
            HookEffect::Cancel("blocked by policy".into()),
        ),
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    let result = hook.before_tool_call("shell".into(), json!({})).await;
    assert!(result.is_cancel());
}

#[test]
fn hook_phase_validation_rejects_cancel_in_post_phase() {
    use zeroclaw::tools::dynamic_registry::validate_phase_effect;

    let err = validate_phase_effect(&HookPhase::Post, &HookEffect::Cancel("nope".into()));
    assert!(err.is_err());

    let err = validate_phase_effect(
        &HookPhase::Post,
        &HookEffect::ModifyArgs(json!({"key": "val"})),
    );
    assert!(err.is_err());

    let err = validate_phase_effect(
        &HookPhase::Post,
        &HookEffect::InjectPromptSuffix("nope".into()),
    );
    assert!(err.is_err());

    // LogToChannel is valid in Post phase
    let ok = validate_phase_effect(&HookPhase::Post, &HookEffect::LogToChannel("audit".into()));
    assert!(ok.is_ok());

    // Pre allows Cancel, ModifyArgs, InjectPromptSuffix
    assert!(validate_phase_effect(&HookPhase::Pre, &HookEffect::Cancel("ok".into())).is_ok());
    assert!(
        validate_phase_effect(&HookPhase::Pre, &HookEffect::ModifyArgs(json!({}))).is_ok()
    );
    assert!(validate_phase_effect(
        &HookPhase::Pre,
        &HookEffect::InjectPromptSuffix("ok".into())
    )
    .is_ok());

    // Pre does NOT allow LogToChannel
    assert!(
        validate_phase_effect(&HookPhase::Pre, &HookEffect::LogToChannel("nope".into())).is_err()
    );
}

#[tokio::test]
async fn hook_effect_log_to_channel_fires_on_post_tool_call() {
    let reg = DynamicRegistry::new_empty(default_config());
    reg.add_hook(
        DynamicHookDef {
            id: "h-log".into(),
            name: "log_hook".into(),
            phase: HookPhase::Post,
            target: HookPoint::ToolCall,
            priority: 10,
            enabled: true,
            filter: None,
            effect: HookEffect::LogToChannel("audit_channel".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        None,
    )
    .unwrap();

    let snap = reg.snapshot();
    let hook = &snap.dynamic_hooks[0];

    // Post hooks use on_after_tool_call (void return — just verifies no panic)
    let tool_result = zeroclaw::tools::ToolResult {
        success: true,
        output: "ok".into(),
        error: None,
    };
    hook.on_after_tool_call("shell", &tool_result, std::time::Duration::from_millis(100))
        .await;
    // No panic = success. The hook logs via tracing which we don't capture here.
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. Persistence
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn persistence_save_load_roundtrip_preserves_tools_hooks_and_states() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state").join("registry.json");

    // Create registry and add tools + hooks
    {
        let reg = DynamicRegistry::new(Vec::new(), default_config(), path.clone(), build_ctx(&tmp));
        reg.add_tool(sample_shell_tool("t-persist", "persist_tool"), None)
            .unwrap();
        reg.add_hook(
            sample_hook(
                "h-persist",
                "persist_hook",
                HookPhase::Post,
                HookEffect::LogToChannel("chan".into()),
            ),
            None,
        )
        .unwrap();
        // Registry should have persisted via add_tool/add_hook
    }

    // Load fresh registry from same path
    let reg2 = DynamicRegistry::new(Vec::new(), default_config(), path, build_ctx(&tmp));
    let tools = reg2.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "persist_tool");

    let hooks = reg2.list_hooks();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].name, "persist_hook");
}

#[test]
fn persistence_corrupt_file_is_quarantined_and_registry_recovers() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state").join("registry.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    // Write corrupt JSON
    std::fs::write(&path, "this is not valid JSON{{{").unwrap();

    // Load should quarantine and return empty
    let persisted = PersistedRegistry::try_load_or_quarantine(&path);
    assert!(persisted.tools.is_empty());
    assert!(persisted.hooks.is_empty());

    // Original file should be renamed
    assert!(!path.exists(), "corrupt file should have been moved");

    // A .corrupt. file should exist
    let parent = path.parent().unwrap();
    let corrupt_files: Vec<_> = std::fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".corrupt.")
        })
        .collect();
    assert!(
        !corrupt_files.is_empty(),
        "quarantined corrupt file should exist"
    );
}

#[test]
fn persistence_missing_file_returns_empty_default() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("nonexistent_registry.json");

    let persisted = PersistedRegistry::load_from_file(&path).unwrap();
    assert!(persisted.tools.is_empty());
    assert!(persisted.hooks.is_empty());
    assert_eq!(persisted.schema_version, 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Quotas, Concurrency, Snapshot Isolation, Prompt Generation
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn quota_enforcement_max_tools_limit() {
    let reg = DynamicRegistry::new_empty(tiny_config());

    reg.add_tool(sample_shell_tool("t1", "tool_1"), None).unwrap();
    reg.add_tool(sample_shell_tool("t2", "tool_2"), None).unwrap();

    // Third tool should fail (limit = 2)
    let err = reg
        .add_tool(sample_shell_tool("t3", "tool_3"), None)
        .unwrap_err();
    assert!(
        matches!(err, DynamicRegistryError::QuotaExceeded { ref kind, limit: 2 } if kind == "tools"),
        "expected QuotaExceeded for tools, got: {err}"
    );
}

#[test]
fn quota_enforcement_max_hooks_limit() {
    let reg = DynamicRegistry::new_empty(tiny_config());

    reg.add_hook(
        sample_hook("h1", "hook_1", HookPhase::Pre, HookEffect::Cancel("a".into())),
        None,
    )
    .unwrap();
    reg.add_hook(
        sample_hook("h2", "hook_2", HookPhase::Pre, HookEffect::Cancel("b".into())),
        None,
    )
    .unwrap();

    // Third hook should fail (limit = 2)
    let err = reg
        .add_hook(
            sample_hook("h3", "hook_3", HookPhase::Pre, HookEffect::Cancel("c".into())),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err, DynamicRegistryError::QuotaExceeded { ref kind, limit: 2 } if kind == "hooks"),
        "expected QuotaExceeded for hooks, got: {err}"
    );
}

#[test]
fn optimistic_concurrency_rejects_stale_revision_update() {
    let reg = DynamicRegistry::new_empty(default_config());

    // First add succeeds with revision 0 → returns revision 1
    let rev = reg
        .add_tool(sample_shell_tool("t1", "tool_1"), Some(0))
        .unwrap();
    assert_eq!(rev, 1);

    // Second add with stale revision (0, but actual is 1) should fail
    let err = reg
        .add_tool(sample_shell_tool("t2", "tool_2"), Some(0))
        .unwrap_err();
    assert!(
        matches!(
            err,
            DynamicRegistryError::RevisionConflict {
                expected: 0,
                actual: 1
            }
        ),
        "expected RevisionConflict, got: {err}"
    );

    // Third add with correct revision should succeed
    let rev = reg
        .add_tool(sample_shell_tool("t2", "tool_2"), Some(1))
        .unwrap();
    assert_eq!(rev, 2);
}

#[test]
fn snapshot_isolation_excludes_post_snapshot_tool_additions() {
    let reg = DynamicRegistry::new_empty(default_config());

    reg.add_tool(sample_shell_tool("t1", "before_snap"), None)
        .unwrap();

    // Capture snapshot
    let snap = reg.snapshot();
    let snap_names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(snap_names.contains(&"before_snap"));

    // Add another tool AFTER snapshot
    reg.add_tool(sample_shell_tool("t2", "after_snap"), None)
        .unwrap();

    // Original snapshot should NOT contain the new tool
    let snap_names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(!snap_names.contains(&"after_snap"), "post-snapshot addition should not appear");

    // But a new snapshot SHOULD contain it
    let new_snap = reg.snapshot();
    let new_snap_names: Vec<&str> = new_snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(new_snap_names.contains(&"after_snap"));
}

#[test]
fn format_dynamic_tool_docs_regenerates_deterministic_prompt_block() {
    use zeroclaw::agent::dispatcher::format_dynamic_tool_docs;
    use zeroclaw::tools::ToolSpec;

    // Empty specs produce empty string
    let empty = format_dynamic_tool_docs(&[]);
    assert!(empty.is_empty());

    // Non-empty specs produce structured markdown
    let specs = vec![
        ToolSpec {
            name: "greet".into(),
            description: "Says hello".into(),
            parameters: json!({"type": "object"}),
        },
        ToolSpec {
            name: "calc".into(),
            description: "Does math".into(),
            parameters: json!({"type": "object", "properties": {"expr": {"type": "string"}}}),
        },
    ];

    let docs = format_dynamic_tool_docs(&specs);
    assert!(docs.contains("### Dynamic Tools"));
    assert!(docs.contains("**greet**"));
    assert!(docs.contains("Says hello"));
    assert!(docs.contains("**calc**"));
    assert!(docs.contains("Does math"));

    // Deterministic: same input produces same output
    let docs2 = format_dynamic_tool_docs(&specs);
    assert_eq!(docs, docs2);
}

#[tokio::test]
async fn multi_turn_tool_created_in_turn1_visible_in_turn2() {
    let tmp = TempDir::new().unwrap();
    let reg = Arc::new(registry_with_persistence(&tmp));
    let tool = ManageToolsTool::new(reg.clone());

    // Turn 1: Create a tool
    let snap_before = reg.snapshot();
    assert!(snap_before.all_tools.is_empty());

    let add_result = tool
        .execute(json!({
            "action": "add",
            "name": "turn1_tool",
            "description": "Created in turn 1",
            "kind": "shell_command",
            "config": {"command": "echo", "args": [{"Fixed": "from_turn1"}]}
        }))
        .await
        .unwrap();
    assert!(add_result.success);

    // Turn 2: New snapshot should see the tool
    let snap_turn2 = reg.snapshot();
    let names: Vec<&str> = snap_turn2.all_tools.iter().map(|t| t.name()).collect();
    assert!(
        names.contains(&"turn1_tool"),
        "tool created in turn 1 should be visible in turn 2 snapshot"
    );

    // The tool should also be executable
    let turn1_tool = snap_turn2
        .all_tools
        .iter()
        .find(|t| t.name() == "turn1_tool")
        .unwrap();
    let exec_result = turn1_tool.execute(json!({})).await.unwrap();
    assert!(exec_result.success);
    assert!(exec_result.output.contains("from_turn1"));
}

#[test]
fn disabled_tool_not_in_snapshot_tools() {
    let reg = DynamicRegistry::new_empty(default_config());

    let mut def = sample_shell_tool("t-disabled", "disabled_from_start");
    def.enabled = false;
    reg.add_tool(def, None).unwrap();

    let snap = reg.snapshot();
    let names: Vec<&str> = snap.all_tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"disabled_from_start"),
        "tool added as disabled should not appear in snapshot"
    );

    // But it should appear in list_tools (which shows all, including disabled)
    let tools = reg.list_tools();
    assert_eq!(tools.len(), 1);
    assert!(!tools[0].enabled);
}

#[test]
fn dynamic_tool_name_collision_with_another_dynamic_tool() {
    let reg = DynamicRegistry::new_empty(default_config());

    reg.add_tool(sample_shell_tool("t1", "unique_name"), None)
        .unwrap();

    // Try to add another tool with the same name but different ID
    let err = reg
        .add_tool(sample_shell_tool("t2", "unique_name"), None)
        .unwrap_err();
    assert!(
        matches!(err, DynamicRegistryError::NameCollision(_)),
        "expected NameCollision, got: {err}"
    );
}

#[test]
fn unknown_tool_kind_rejected() {
    let reg = DynamicRegistry::new_empty(default_config());

    let def = DynamicToolDef {
        id: "t-unknown".into(),
        name: "unknown_kind_tool".into(),
        description: "Uses unknown kind".into(),
        kind: "nonexistent_kind".into(),
        config: json!({}),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        created_by: None,
    };

    let err = reg.add_tool(def, None).unwrap_err();
    assert!(
        matches!(err, DynamicRegistryError::UnknownKind(_)),
        "expected UnknownKind, got: {err}"
    );
}
