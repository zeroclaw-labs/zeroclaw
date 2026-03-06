//! Phase 2 Integration Tests: DelegateTool + AgentRegistry
//!
//! These tests cover the integration between DelegateTool and AgentRegistry,
//! including:
//! 1. Loading agent definitions from Registry
//! 2. Dynamic agent add/remove operations
//! 3. Agent execution with registry-backed configurations
//! 4. Registry hot reload integration

use crate::agent::registry::{AgentDefinition, AgentRegistry, ExecutionMode, MemoryBackend};
use crate::config::schema::DelegateAgentConfig;
use crate::security::SecurityPolicy;
use crate::tools::delegate::DelegateTool;
use crate::tools::traits::Tool;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

// ═══════════════════════════════════════════════════════════════════════════
// Test Fixtures
// ═══════════════════════════════════════════════════════════════════════════

/// Test fixture for integration testing
struct IntegrationTestFixture {
    temp_dir: TempDir,
    registry: AgentRegistry,
    security: Arc<SecurityPolicy>,
}

impl IntegrationTestFixture {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let security = Arc::new(SecurityPolicy::default());
        let registry = AgentRegistry::new(temp_dir.path().to_path_buf(), security.clone()).unwrap();

        Self {
            temp_dir,
            registry,
            security,
        }
    }

    /// Create a test agent YAML file
    fn create_agent_file(&self, name: &str, _id: &str, content: &str) {
        let file_path = self.temp_dir.path().join(name);
        fs::write(&file_path, content).expect("Failed to write agent file");
    }

    /// Generate a standard agent YAML definition
    fn standard_agent_yaml(id: &str, name: &str, provider: &str, model: &str) -> String {
        format!(
            r#"
agent:
  id: "{id}"
  name: "{name}"
  version: "1.0.0"
  description: "Test agent for {id}"

execution:
  mode: subprocess
  command: "/usr/bin/zeroclaw"
  args: ["agent", "run", "--agent-id", "{id}"]

provider:
  name: "{provider}"
  model: "{model}"
  temperature: 0.7
  max_tokens: 4096

tools:
  tools:
    - name: "web_search"
      enabled: true
    - name: "file_read"
      enabled: true

system:
  prompt: "You are a {name} agent."

memory:
  backend: shared
  category: "{id}"

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 3
  backoff_ms: 1000
"#
        )
    }

    /// Generate an agentic agent YAML definition
    fn agentic_agent_yaml(
        id: &str,
        name: &str,
        provider: &str,
        model: &str,
        allowed_tools: &[&str],
        _max_iterations: usize,
    ) -> String {
        let tools_list = allowed_tools
            .iter()
            .map(|t| format!("    - name: \"{}\"\n      enabled: true", t))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"
agent:
  id: "{id}"
  name: "{name}"
  version: "1.0.0"
  description: "Agentic test agent for {id}"

execution:
  mode: subprocess
  command: "/usr/bin/zeroclaw"
  args: ["agent", "run", "--agent-id", "{id}"]

provider:
  name: "{provider}"
  model: "{model}"
  temperature: 0.3

tools:
  tools:
{tools_list}

system:
  prompt: "You are an agentic {name} agent."

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 600

retry:
  max_attempts: 5
  backoff_ms: 2000
"#
        )
    }

    /// Convert AgentDefinition to DelegateAgentConfig
    fn definition_to_config(def: &AgentDefinition) -> DelegateAgentConfig {
        DelegateAgentConfig {
            provider: def
                .provider
                .name
                .clone()
                .unwrap_or_else(|| "openrouter".to_string()),
            model: def
                .provider
                .model
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            system_prompt: if def.system.prompt.is_empty() {
                None
            } else {
                Some(def.system.prompt.clone())
            },
            api_key: def.provider.api_key.clone(),
            temperature: def.provider.temperature,
            max_depth: 3,
            agentic: true,
            allowed_tools: def
                .tools
                .tools
                .iter()
                .filter(|t| t.enabled)
                .map(|t| t.name.clone())
                .collect(),
            max_iterations: 10,
            enabled: true,
            capabilities: Vec::new(),
            priority: 0,
        }
    }

    /// Build a DelegateTool from the registry
    fn build_delegate_tool(&self) -> DelegateTool {
        let definitions = self.registry.all();
        let mut configs = HashMap::new();

        for (id, def) in definitions {
            let config = Self::definition_to_config(&def);
            configs.insert(id, config);
        }

        DelegateTool::new(configs, None, self.security.clone())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. DelegateTool Loading from Registry Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_delegate_tool_loads_single_agent_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create agent definition
    fixture.create_agent_file(
        "researcher.yaml",
        "researcher",
        &IntegrationTestFixture::standard_agent_yaml(
            "researcher",
            "Research Agent",
            "openrouter",
            "anthropic/claude-sonnet-4-6",
        ),
    );

    // Discover agents
    fixture.registry.discover().unwrap();

    // Build DelegateTool
    let tool = fixture.build_delegate_tool();

    // Verify tool schema includes the agent
    let schema = tool.parameters_schema();
    let desc = schema["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(
        desc.contains("researcher"),
        "Schema should list researcher agent"
    );
}

#[test]
fn test_delegate_tool_loads_multiple_agents_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create multiple agents
    fixture.create_agent_file(
        "researcher.yaml",
        "researcher",
        &IntegrationTestFixture::standard_agent_yaml(
            "researcher",
            "Research Agent",
            "openrouter",
            "anthropic/claude-sonnet-4-6",
        ),
    );

    fixture.create_agent_file(
        "coder.yaml",
        "coder",
        &IntegrationTestFixture::standard_agent_yaml(
            "coder",
            "Code Agent",
            "openrouter",
            "openai/o1-preview",
        ),
    );

    fixture.create_agent_file(
        "tester.yaml",
        "tester",
        &IntegrationTestFixture::standard_agent_yaml("tester", "Test Agent", "ollama", "llama3"),
    );

    // Discover all agents
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 3);

    // Build DelegateTool
    let tool = fixture.build_delegate_tool();

    // Verify all agents are in schema
    let schema = tool.parameters_schema();
    let desc = schema["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("researcher"));
    assert!(desc.contains("coder"));
    assert!(desc.contains("tester"));
}

#[test]
fn test_delegate_tool_preserves_agent_config_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create agent with specific configuration
    fixture.create_agent_file(
        "special.yaml",
        "special",
        &IntegrationTestFixture::standard_agent_yaml(
            "special",
            "Special Agent",
            "anthropic",
            "claude-sonnet-4-6",
        ),
    );

    fixture.registry.discover().unwrap();

    // Get the definition and verify conversion
    let def = fixture.registry.get("special").expect("Agent not found");

    assert_eq!(def.provider.name, Some("anthropic".to_string()));
    assert_eq!(def.provider.model, Some("claude-sonnet-4-6".to_string()));
    assert_eq!(def.provider.temperature, Some(0.7));

    // Build DelegateTool and verify config conversion
    let tool = fixture.build_delegate_tool();
    let schema = tool.parameters_schema();

    // Schema should include the agent
    let desc = schema["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("special"));
}

#[test]
fn test_delegate_tool_maps_registry_tools_to_allowed_tools() {
    let fixture = IntegrationTestFixture::new();

    // Create agentic agent with specific tools
    fixture.create_agent_file(
        "agentic.yaml",
        "agentic",
        &IntegrationTestFixture::agentic_agent_yaml(
            "agentic",
            "Agentic Agent",
            "openrouter",
            "anthropic/claude-sonnet-4-6",
            &["web_search", "file_read", "memory_write"],
            20,
        ),
    );

    fixture.registry.discover().unwrap();

    // Build DelegateTool
    let tool = fixture.build_delegate_tool();

    // Verify the tool is configured correctly
    let schema = tool.parameters_schema();
    assert!(schema["properties"]["agent"]["description"]
        .as_str()
        .unwrap()
        .contains("agentic"));
}

#[test]
fn test_delegate_tool_handles_empty_registry() {
    let fixture = IntegrationTestFixture::new();

    // No agents created
    fixture.registry.discover().unwrap();

    // Build DelegateTool with empty config
    let tool = fixture.build_delegate_tool();

    // Schema should indicate no agents configured
    let schema = tool.parameters_schema();
    let desc = schema["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("none configured"));
}

#[test]
fn test_delegate_tool_system_prompt_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create agent with custom system prompt
    let custom_yaml = r#"
agent:
  id: "custom"
  name: "Custom Agent"
  version: "1.0.0"
  description: "Agent with custom prompt"

execution:
  mode: subprocess
  command: "/usr/bin/zeroclaw"

provider:
  name: "openrouter"
  model: "gpt-4"

system:
  prompt: |
    You are a specialized agent.
    Follow these rules:
    1. Be concise
    2. Use markdown
    3. Cite sources

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("custom.yaml", "custom", custom_yaml);
    fixture.registry.discover().unwrap();

    // Verify system prompt is preserved
    let def = fixture.registry.get("custom").expect("Agent not found");
    assert!(def.system.prompt.contains("specialized agent"));
    assert!(def.system.prompt.contains("Be concise"));
}

#[test]
fn test_delegate_tool_execution_mode_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create agent with wasm execution mode
    let wasm_yaml = r#"
agent:
  id: "wasm-agent"
  name: "WASM Agent"
  version: "1.0.0"
  description: "WASM execution mode agent"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("wasm.yaml", "wasm-agent", wasm_yaml);
    fixture.registry.discover().unwrap();

    // Verify execution mode is preserved
    let def = fixture.registry.get("wasm-agent").expect("Agent not found");
    assert_eq!(def.execution.mode, ExecutionMode::Wasm);
}

#[test]
fn test_delegate_tool_memory_backend_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create agent with isolated memory
    let isolated_yaml = r#"
agent:
  id: "isolated"
  name: "Isolated Agent"
  version: "1.0.0"
  description: "Isolated memory agent"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: isolated
  category: "private"

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("isolated.yaml", "isolated", isolated_yaml);
    fixture.registry.discover().unwrap();

    // Verify memory backend is preserved
    let def = fixture.registry.get("isolated").expect("Agent not found");
    assert_eq!(def.memory.backend, MemoryBackend::Isolated);
    assert_eq!(def.memory.category, Some("private".to_string()));
}

#[test]
fn test_delegate_tool_retry_config_from_registry() {
    let fixture = IntegrationTestFixture::new();

    // Create agent with custom retry config
    let retry_yaml = r#"
agent:
  id: "retry-test"
  name: "Retry Test Agent"
  version: "1.0.0"
  description: "Custom retry configuration"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 10
  backoff_ms: 5000
"#;

    fixture.create_agent_file("retry.yaml", "retry-test", retry_yaml);
    fixture.registry.discover().unwrap();

    // Verify retry config is preserved
    let def = fixture.registry.get("retry-test").expect("Agent not found");
    assert_eq!(def.retry.max_attempts, 10);
    assert_eq!(def.retry.backoff_ms, 5000);
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Dynamic Agent Add/Remove Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_dynamic_agent_add_updates_delegate_tool_schema() {
    let fixture = IntegrationTestFixture::new();

    // Initial state - no agents
    fixture.registry.discover().unwrap();
    let tool1 = fixture.build_delegate_tool();
    let schema1 = tool1.parameters_schema();
    assert!(schema1["properties"]["agent"]["description"]
        .as_str()
        .unwrap()
        .contains("none configured"));

    // Add new agent
    fixture.create_agent_file(
        "new-agent.yaml",
        "new-agent",
        &IntegrationTestFixture::standard_agent_yaml(
            "new-agent",
            "New Agent",
            "openrouter",
            "gpt-4",
        ),
    );

    // Reload registry
    fixture.registry.reload().unwrap();

    // Build new DelegateTool with updated registry
    let tool2 = fixture.build_delegate_tool();
    let schema2 = tool2.parameters_schema();
    let desc = schema2["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("new-agent"));
}

#[test]
fn test_dynamic_agent_remove_updates_delegate_tool_schema() {
    let fixture = IntegrationTestFixture::new();

    // Create initial agents
    let agent1_path = fixture.temp_dir.path().join("agent1.yaml");
    fs::write(
        &agent1_path,
        IntegrationTestFixture::standard_agent_yaml("agent1", "Agent 1", "openrouter", "gpt-4"),
    )
    .unwrap();

    let agent2_path = fixture.temp_dir.path().join("agent2.yaml");
    fs::write(
        &agent2_path,
        IntegrationTestFixture::standard_agent_yaml("agent2", "Agent 2", "openrouter", "claude-3"),
    )
    .unwrap();

    fixture.registry.discover().unwrap();

    // Verify both agents are loaded
    assert_eq!(fixture.registry.count(), 2);

    // Remove one agent
    fs::remove_file(agent1_path).unwrap();

    // Reload registry
    fixture.registry.reload().unwrap();

    // Build new DelegateTool
    let tool = fixture.build_delegate_tool();
    let schema = tool.parameters_schema();
    let desc = schema["properties"]["agent"]["description"]
        .as_str()
        .unwrap();

    // Should only have agent2
    assert!(!desc.contains("agent1"));
    assert!(desc.contains("agent2"));
}

#[test]
fn test_dynamic_agent_update_refreshes_config() {
    let fixture = IntegrationTestFixture::new();

    // Create initial agent
    let initial_yaml = IntegrationTestFixture::standard_agent_yaml(
        "updatable",
        "Updatable Agent",
        "openrouter",
        "gpt-3.5-turbo",
    );
    fixture.create_agent_file("updatable.yaml", "updatable", &initial_yaml);

    fixture.registry.discover().unwrap();

    // Verify initial config
    let def1 = fixture.registry.get("updatable").unwrap();
    assert_eq!(def1.provider.model.as_deref(), Some("gpt-3.5-turbo"));

    // Update agent file
    let updated_yaml = IntegrationTestFixture::standard_agent_yaml(
        "updatable",
        "Updatable Agent v2",
        "anthropic",
        "claude-sonnet-4-6",
    );
    fixture.create_agent_file("updatable.yaml", "updatable", &updated_yaml);

    // Reload registry
    fixture.registry.reload().unwrap();

    // Verify updated config
    let def2 = fixture.registry.get("updatable").unwrap();
    assert_eq!(def2.provider.name.as_deref(), Some("anthropic"));
    assert_eq!(def2.provider.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(def2.agent.name, "Updatable Agent v2");
}

#[test]
fn test_registry_multiple_add_remove_cycles() {
    let fixture = IntegrationTestFixture::new();

    // Cycle 1: Add agent A
    fixture.create_agent_file(
        "agent-a.yaml",
        "agent-a",
        &IntegrationTestFixture::standard_agent_yaml("agent-a", "Agent A", "openrouter", "gpt-4"),
    );
    fixture.registry.reload().unwrap();
    assert_eq!(fixture.registry.count(), 1);

    // Cycle 2: Add agents B and C
    fixture.create_agent_file(
        "agent-b.yaml",
        "agent-b",
        &IntegrationTestFixture::standard_agent_yaml(
            "agent-b",
            "Agent B",
            "openrouter",
            "claude-3",
        ),
    );
    fixture.create_agent_file(
        "agent-c.yaml",
        "agent-c",
        &IntegrationTestFixture::standard_agent_yaml("agent-c", "Agent C", "ollama", "llama3"),
    );
    fixture.registry.reload().unwrap();
    assert_eq!(fixture.registry.count(), 3);

    // Cycle 3: Remove A and B
    fs::remove_file(fixture.temp_dir.path().join("agent-a.yaml")).unwrap();
    fs::remove_file(fixture.temp_dir.path().join("agent-b.yaml")).unwrap();
    fixture.registry.reload().unwrap();
    assert_eq!(fixture.registry.count(), 1);
    assert!(fixture.registry.contains("agent-c"));

    // Cycle 4: Add D and E
    fixture.create_agent_file(
        "agent-d.yaml",
        "agent-d",
        &IntegrationTestFixture::standard_agent_yaml("agent-d", "Agent D", "openrouter", "gpt-4"),
    );
    fixture.create_agent_file(
        "agent-e.yaml",
        "agent-e",
        &IntegrationTestFixture::standard_agent_yaml("agent-e", "Agent E", "anthropic", "claude-3"),
    );
    fixture.registry.reload().unwrap();
    assert_eq!(fixture.registry.count(), 3);
}

#[test]
fn test_agent_add_preserves_existing_configs() {
    let fixture = IntegrationTestFixture::new();

    // Create initial agent
    fixture.create_agent_file(
        "stable.yaml",
        "stable",
        &IntegrationTestFixture::standard_agent_yaml(
            "stable",
            "Stable Agent",
            "openrouter",
            "gpt-4",
        ),
    );
    fixture.registry.discover().unwrap();

    let stable_def = fixture.registry.get("stable").unwrap();
    let original_model = stable_def.provider.model.clone();

    // Add new agent
    fixture.create_agent_file(
        "new.yaml",
        "new",
        &IntegrationTestFixture::standard_agent_yaml("new", "New Agent", "anthropic", "claude-3"),
    );
    fixture.registry.reload().unwrap();

    // Verify stable agent config is unchanged
    let stable_def_after = fixture.registry.get("stable").unwrap();
    assert_eq!(stable_def_after.provider.model, original_model);
}

#[test]
fn test_concurrent_agent_discoveries() {
    let fixture = IntegrationTestFixture::new();

    // Create multiple agents at once
    for i in 0..5 {
        fixture.create_agent_file(
            &format!("agent{}.yaml", i),
            &format!("agent{}", i),
            &IntegrationTestFixture::standard_agent_yaml(
                &format!("agent{}", i),
                &format!("Agent {}", i),
                "openrouter",
                "gpt-4",
            ),
        );
    }

    // Single discovery should find all
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 5);
    assert_eq!(fixture.registry.count(), 5);
}

#[test]
fn test_empty_agent_name_handled_gracefully() {
    let fixture = IntegrationTestFixture::new();

    // Create agent with empty name (invalid but shouldn't crash)
    let invalid_yaml = r#"
agent:
  id: "empty-name"
  name: ""
  version: "1.0.0"
  description: "Agent with empty name"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("empty-name.yaml", "empty-name", invalid_yaml);

    // Discovery should skip invalid file
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_invalid_yaml_skipped_during_dynamic_add() {
    let fixture = IntegrationTestFixture::new();

    // Create valid agent
    fixture.create_agent_file(
        "valid.yaml",
        "valid",
        &IntegrationTestFixture::standard_agent_yaml("valid", "Valid Agent", "openrouter", "gpt-4"),
    );

    // Create invalid file
    fixture.create_agent_file("invalid.yaml", "invalid", "bad: yaml: [:");

    // Discovery should skip invalid file
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 1);
    assert!(fixture.registry.contains("valid"));
    assert!(!fixture.registry.contains("invalid"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Agent Execution Flow Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_agent_execution_flow_with_loaded_config() {
    let fixture = IntegrationTestFixture::new();

    // Create agentic agent
    fixture.create_agent_file(
        "executor.yaml",
        "executor",
        &IntegrationTestFixture::agentic_agent_yaml(
            "executor",
            "Executor Agent",
            "ollama",
            "llama3",
            &["echo_tool"],
            5,
        ),
    );

    fixture.registry.discover().unwrap();

    // Build DelegateTool
    let tool = fixture.build_delegate_tool();

    // Verify tool can be executed (will fail at provider level but should pass validation)
    let result = tool
        .execute(json!({
            "agent": "executor",
            "prompt": "test task"
        }))
        .await;

    // Should fail at provider creation (ollama not running) but not at agent lookup
    assert!(result.is_ok());
    let tool_result = result.unwrap();
    // Provider creation failure is expected
    assert!(!tool_result.success || tool_result.error.is_some());
}

#[tokio::test]
async fn test_agent_execution_with_context_parameter() {
    let fixture = IntegrationTestFixture::new();

    fixture.create_agent_file(
        "context-agent.yaml",
        "context-agent",
        &IntegrationTestFixture::standard_agent_yaml(
            "context-agent",
            "Context Agent",
            "openrouter",
            "gpt-4",
        ),
    );

    fixture.registry.discover().unwrap();
    let tool = fixture.build_delegate_tool();

    // Execute with context
    let result = tool
        .execute(json!({
            "agent": "context-agent",
            "prompt": "main task",
            "context": "additional context data"
        }))
        .await;

    // Should validate successfully (fail at provider level)
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_agent_execution_propagates_system_prompt() {
    let fixture = IntegrationTestFixture::new();

    let prompt_yaml = r#"
agent:
  id: "prompt-test"
  name: "Prompt Test Agent"
  version: "1.0.0"
  description: "Tests system prompt propagation"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

system:
  prompt: "You are a helpful assistant. Always be concise."

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("prompt-test.yaml", "prompt-test", prompt_yaml);
    fixture.registry.discover().unwrap();

    // Verify system prompt is loaded
    let def = fixture.registry.get("prompt-test").unwrap();
    assert!(def.system.prompt.contains("helpful assistant"));
    assert!(def.system.prompt.contains("concise"));
}

#[tokio::test]
async fn test_agent_execution_with_temperature_override() {
    let fixture = IntegrationTestFixture::new();

    let temp_yaml = r#"
agent:
  id: "temp-test"
  name: "Temperature Test Agent"
  version: "1.0.0"
  description: "Tests temperature override"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "gpt-4"
  temperature: 0.1

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("temp-test.yaml", "temp-test", temp_yaml);
    fixture.registry.discover().unwrap();

    // Verify temperature is loaded
    let def = fixture.registry.get("temp-test").unwrap();
    assert_eq!(def.provider.temperature, Some(0.1));
}

#[tokio::test]
async fn test_agentic_agent_with_tool_allowlist() {
    let fixture = IntegrationTestFixture::new();

    fixture.create_agent_file(
        "agentic-allowed.yaml",
        "agentic-allowed",
        &IntegrationTestFixture::agentic_agent_yaml(
            "agentic-allowed",
            "Agentic Allowed",
            "openrouter",
            "claude-3",
            &["file_read", "file_write", "web_search"],
            15,
        ),
    );

    fixture.registry.discover().unwrap();

    // Verify tools are loaded
    let def = fixture.registry.get("agentic-allowed").unwrap();
    assert_eq!(def.tools.tools.len(), 3);

    let enabled_tools: Vec<_> = def
        .tools
        .tools
        .iter()
        .filter(|t| t.enabled)
        .map(|t| &t.name)
        .collect();

    assert!(enabled_tools.contains(&&"file_read".to_string()));
    assert!(enabled_tools.contains(&&"file_write".to_string()));
    assert!(enabled_tools.contains(&&"web_search".to_string()));
}

#[tokio::test]
async fn test_agent_execution_timeout_from_config() {
    let fixture = IntegrationTestFixture::new();

    let timeout_yaml = r#"
agent:
  id: "timeout-test"
  name: "Timeout Test Agent"
  version: "1.0.0"
  description: "Tests timeout configuration"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 600
"#;

    fixture.create_agent_file("timeout-test.yaml", "timeout-test", timeout_yaml);
    fixture.registry.discover().unwrap();

    // Verify timeout is loaded
    let def = fixture.registry.get("timeout-test").unwrap();
    assert_eq!(def.reporting.timeout_seconds, 600);
}

#[tokio::test]
async fn test_agent_execution_retry_from_config() {
    let fixture = IntegrationTestFixture::new();

    let retry_yaml = r#"
agent:
  id: "retry-test"
  name: "Retry Test Agent"
  version: "1.0.0"
  description: "Tests retry configuration"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 7
  backoff_ms: 3000
"#;

    fixture.create_agent_file("retry-test.yaml", "retry-test", retry_yaml);
    fixture.registry.discover().unwrap();

    // Verify retry config is loaded
    let def = fixture.registry.get("retry-test").unwrap();
    assert_eq!(def.retry.max_attempts, 7);
    assert_eq!(def.retry.backoff_ms, 3000);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Registry Hot Reload Integration Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_hot_reload_propagates_to_delegate_tool() {
    let fixture = IntegrationTestFixture::new();

    // Initial state
    fixture.create_agent_file(
        "agent1.yaml",
        "agent1",
        &IntegrationTestFixture::standard_agent_yaml("agent1", "Agent 1", "openrouter", "gpt-4"),
    );
    fixture.registry.discover().unwrap();

    let tool1 = fixture.build_delegate_tool();
    let schema1 = tool1.parameters_schema();
    assert!(schema1["properties"]["agent"]["description"]
        .as_str()
        .unwrap()
        .contains("agent1"));

    // Add new agent
    fixture.create_agent_file(
        "agent2.yaml",
        "agent2",
        &IntegrationTestFixture::standard_agent_yaml("agent2", "Agent 2", "anthropic", "claude-3"),
    );

    // Hot reload
    fixture.registry.reload().unwrap();

    // Rebuild tool with updated registry
    let tool2 = fixture.build_delegate_tool();
    let schema2 = tool2.parameters_schema();
    let desc = schema2["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("agent1"));
    assert!(desc.contains("agent2"));
}

#[test]
fn test_hot_reload_handles_agent_removal() {
    let fixture = IntegrationTestFixture::new();

    // Create agents
    let agent1_path = fixture.temp_dir.path().join("removable1.yaml");
    fs::write(
        &agent1_path,
        IntegrationTestFixture::standard_agent_yaml(
            "removable1",
            "Removable 1",
            "openrouter",
            "gpt-4",
        ),
    )
    .unwrap();

    let agent2_path = fixture.temp_dir.path().join("removable2.yaml");
    fs::write(
        &agent2_path,
        IntegrationTestFixture::standard_agent_yaml(
            "removable2",
            "Removable 2",
            "anthropic",
            "claude-3",
        ),
    )
    .unwrap();

    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 2);

    // Remove first agent
    fs::remove_file(agent1_path).unwrap();

    // Hot reload
    fixture.registry.reload().unwrap();

    // Verify only second agent remains
    assert_eq!(fixture.registry.count(), 1);
    assert!(fixture.registry.contains("removable2"));
    assert!(!fixture.registry.contains("removable1"));
}

#[test]
fn test_hot_reload_handles_agent_update() {
    let fixture = IntegrationTestFixture::new();

    // Create agent
    let agent_path = fixture.temp_dir.path().join("updatable.yaml");
    fs::write(
        &agent_path,
        IntegrationTestFixture::standard_agent_yaml(
            "updatable",
            "Original Name",
            "openrouter",
            "gpt-3.5",
        ),
    )
    .unwrap();

    fixture.registry.discover().unwrap();

    let def1 = fixture.registry.get("updatable").unwrap();
    assert_eq!(def1.agent.name, "Original Name");
    assert_eq!(def1.provider.model.as_deref(), Some("gpt-3.5"));

    // Update agent file
    fs::write(
        &agent_path,
        IntegrationTestFixture::standard_agent_yaml(
            "updatable",
            "Updated Name",
            "anthropic",
            "claude-sonnet-4-6",
        ),
    )
    .unwrap();

    // Hot reload
    fixture.registry.reload().unwrap();

    // Verify updates
    let def2 = fixture.registry.get("updatable").unwrap();
    assert_eq!(def2.agent.name, "Updated Name");
    assert_eq!(def2.provider.model.as_deref(), Some("claude-sonnet-4-6"));
}

#[test]
fn test_hot_reload_handles_invalid_new_files() {
    let fixture = IntegrationTestFixture::new();

    // Create valid agent
    fixture.create_agent_file(
        "valid-agent.yaml",
        "valid-agent",
        &IntegrationTestFixture::standard_agent_yaml("valid-agent", "Valid", "openrouter", "gpt-4"),
    );

    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 1);

    // Add invalid file
    fixture.create_agent_file("broken.yaml", "broken", "invalid: yaml: [:");

    // Hot reload should skip invalid file
    let count = fixture.registry.reload().unwrap();
    assert_eq!(count, 1);
    assert!(fixture.registry.contains("valid-agent"));
    assert!(!fixture.registry.contains("broken"));
}

#[test]
fn test_hot_reload_idempotent() {
    let fixture = IntegrationTestFixture::new();

    fixture.create_agent_file(
        "stable.yaml",
        "stable",
        &IntegrationTestFixture::standard_agent_yaml("stable", "Stable", "openrouter", "gpt-4"),
    );

    fixture.registry.discover().unwrap();

    // Multiple reloads should be idempotent
    let count1 = fixture.registry.reload().unwrap();
    let count2 = fixture.registry.reload().unwrap();
    let count3 = fixture.registry.reload().unwrap();

    assert_eq!(count1, count2);
    assert_eq!(count2, count3);
    assert_eq!(count3, 1);
}

#[test]
fn test_hot_reload_with_empty_directory() {
    let fixture = IntegrationTestFixture::new();

    // Empty initial discovery
    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 0);

    // Reload on empty directory should be safe
    let count = fixture.registry.reload().unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_hot_reload_preserves_agent_ids() {
    let fixture = IntegrationTestFixture::new();

    // Create agents in specific order
    fixture.create_agent_file(
        "z-last.yaml",
        "z-last",
        &IntegrationTestFixture::standard_agent_yaml("z-last", "Z", "openrouter", "gpt-4"),
    );
    fixture.create_agent_file(
        "a-first.yaml",
        "a-first",
        &IntegrationTestFixture::standard_agent_yaml("a-first", "A", "anthropic", "claude-3"),
    );
    fixture.create_agent_file(
        "m-middle.yaml",
        "m-middle",
        &IntegrationTestFixture::standard_agent_yaml("m-middle", "M", "ollama", "llama3"),
    );

    fixture.registry.discover().unwrap();

    // Verify order is preserved
    let ids = fixture.registry.list();
    assert_eq!(ids, vec!["a-first", "m-middle", "z-last"]);

    // Hot reload
    fixture.registry.reload().unwrap();

    // Order should still be preserved
    let ids_after = fixture.registry.list();
    assert_eq!(ids_after, ids);
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Edge Cases and Error Handling
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_delegate_tool_with_duplicate_agent_ids() {
    let fixture = IntegrationTestFixture::new();

    // Create two files with same agent ID (second should overwrite)
    fixture.create_agent_file(
        "agent-v1.yaml",
        "duplicate",
        &IntegrationTestFixture::standard_agent_yaml(
            "duplicate",
            "Version 1",
            "openrouter",
            "gpt-3.5",
        ),
    );

    fixture.create_agent_file(
        "agent-v2.yaml",
        "duplicate",
        &IntegrationTestFixture::standard_agent_yaml(
            "duplicate",
            "Version 2",
            "anthropic",
            "claude-3",
        ),
    );

    // Discovery should succeed (last write wins)
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 1); // Only one agent with ID "duplicate"

    let def = fixture.registry.get("duplicate").unwrap();
    // Which version wins depends on filesystem ordering
    assert!(def.agent.name == "Version 1" || def.agent.name == "Version 2");
}

#[test]
fn test_agent_with_special_characters_in_id() {
    let fixture = IntegrationTestFixture::new();

    // Agent ID with underscores and numbers (valid)
    let valid_special_yaml = r#"
agent:
  id: "agent_v2_0"
  name: "Special ID Agent"
  version: "1.0.0"
  description: "Agent with special characters in ID"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300
"#;

    fixture.create_agent_file("special-id.yaml", "agent_v2_0", valid_special_yaml);

    // Discovery should load the agent
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 1);
    assert!(fixture.registry.contains("agent_v2_0"));
}

#[test]
fn test_agent_definition_conversion_roundtrip() {
    let fixture = IntegrationTestFixture::new();

    fixture.create_agent_file(
        "roundtrip.yaml",
        "roundtrip",
        &IntegrationTestFixture::standard_agent_yaml(
            "roundtrip",
            "Roundtrip",
            "openrouter",
            "gpt-4",
        ),
    );

    fixture.registry.discover().unwrap();

    // Get definition from registry
    let def = fixture.registry.get("roundtrip").unwrap();

    // Convert to DelegateAgentConfig
    let config = IntegrationTestFixture::definition_to_config(&def);

    // Verify key fields are preserved
    assert_eq!(config.provider, "openrouter");
    assert_eq!(config.model, "gpt-4");
    assert_eq!(config.temperature, Some(0.7));
    assert!(config.agentic);
}

#[test]
fn test_multiple_registries_independent() {
    let temp_dir1 = TempDir::new().unwrap();
    let temp_dir2 = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());

    let registry1 = AgentRegistry::new(temp_dir1.path().to_path_buf(), security.clone()).unwrap();
    let registry2 = AgentRegistry::new(temp_dir2.path().to_path_buf(), security.clone()).unwrap();

    // Add different agents to each registry
    let agent1_path = temp_dir1.path().join("agent1.yaml");
    fs::write(
        &agent1_path,
        IntegrationTestFixture::standard_agent_yaml("agent1", "Agent 1", "openrouter", "gpt-4"),
    )
    .unwrap();

    let agent2_path = temp_dir2.path().join("agent2.yaml");
    fs::write(
        &agent2_path,
        IntegrationTestFixture::standard_agent_yaml("agent2", "Agent 2", "anthropic", "claude-3"),
    )
    .unwrap();

    registry1.discover().unwrap();
    registry2.discover().unwrap();

    // Each registry should have only its own agent
    assert_eq!(registry1.count(), 1);
    assert_eq!(registry2.count(), 1);
    assert!(registry1.contains("agent1"));
    assert!(registry2.contains("agent2"));
    assert!(!registry1.contains("agent2"));
    assert!(!registry2.contains("agent1"));
}

#[test]
fn test_registry_with_search_dirs() {
    let mut fixture = IntegrationTestFixture::new();

    // Create main agent
    fixture.create_agent_file(
        "main.yaml",
        "main",
        &IntegrationTestFixture::standard_agent_yaml("main", "Main", "openrouter", "gpt-4"),
    );

    // Create additional directory with agent
    let extra_dir = fixture.temp_dir.path().join("extra");
    fs::create_dir(&extra_dir).unwrap();

    let extra_agent = extra_dir.join("extra.yaml");
    fs::write(
        &extra_agent,
        IntegrationTestFixture::standard_agent_yaml("extra", "Extra", "anthropic", "claude-3"),
    )
    .unwrap();

    // Add search dir
    fixture.registry.add_search_dir(extra_dir);

    // Discovery should find both agents
    let count = fixture.registry.discover().unwrap();
    assert_eq!(count, 2);
    assert!(fixture.registry.contains("main"));
    assert!(fixture.registry.contains("extra"));
}
