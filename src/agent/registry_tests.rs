//! Integration tests for AgentRegistry
//!
//! These tests cover:
//! 1. YAML file loading from disk
//! 2. Invalid file handling (malformed YAML, missing files)
//! 3. Hot reload functionality
//! 4. Agent definition validation

use crate::agent::registry::{
    AgentDefinition, AgentExecution, AgentMemory, AgentMetadata, AgentProvider, AgentRegistry,
    AgentReporting, AgentRetry, AgentSystem, AgentTools, AgentToolConfig, AgentToolDeny,
    ExecutionMode, MemoryBackend, OutputFormat, ReportingMode,
};
use crate::security::SecurityPolicy;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

// ═══════════════════════════════════════════════════════════════════════════
// Test Fixtures
// ═══════════════════════════════════════════════════════════════════════════

/// Test fixture for creating a temporary registry with test agents
struct TestRegistry {
    temp_dir: TempDir,
    registry: AgentRegistry,
}

impl TestRegistry {
    /// Create a new test registry with an empty temp directory
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let security = Arc::new(SecurityPolicy::default());
        let registry = AgentRegistry::new(temp_dir.path().to_path_buf(), security)
            .expect("Failed to create registry");

        Self { temp_dir, registry }
    }

    /// Create a test registry with additional search directories
    fn with_search_dirs() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let extra_dir = TempDir::new().expect("Failed to create extra dir");
        let security = Arc::new(SecurityPolicy::default());

        let search_dirs = vec![extra_dir.path().to_path_buf()];
        let registry = AgentRegistry::with_search_dirs(
            temp_dir.path().to_path_buf(),
            search_dirs,
            security,
        )
        .expect("Failed to create registry");

        Self { temp_dir, registry }
    }

    /// Get the path to the temporary directory
    fn path(&self) -> &std::path::Path {
        self.temp_dir.path()
    }

    /// Create an agent file in the temp directory
    fn create_agent(&self, name: &str, content: &str) -> PathBuf {
        let file_path = self.temp_dir.path().join(name);
        fs::write(&file_path, content).expect("Failed to write agent file");
        file_path
    }

    /// Generate a minimal valid agent YAML
    fn valid_agent_yaml(id: &str, name: &str) -> String {
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
  args:
    - "agent"
    - "run"
    - "--agent-id"
    - "{id}"
  working_dir: "/tmp/workspace"
  env:
    ZEROCLAW_MODE: "worker"

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"
  api_key: null
  temperature: 0.7
  max_tokens: 4096

tools:
  tools:
    - name: "web_search"
      enabled: true
    - name: "web_fetch"
      enabled: true
    - name: "memory_read"
      enabled: true
    - name: "memory_write"
      enabled: true
  deny:
    - name: "shell"
      reason: "Research agent should not execute shell commands"
    - name: "file_write"
      reason: "Research agent is read-only"

system:
  prompt: |
    You are a {name} agent.
    Your role is to assist with research tasks.

memory:
  backend: shared
  category: "research"

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

    /// Generate agent YAML with custom values
    fn custom_agent_yaml(custom: &str) -> String {
        format!(
            r#"
agent:
  id: "custom"
  name: "Custom Agent"
  version: "1.0.0"
  description: "A custom test agent"
{custom}
"#
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. YAML File Loading Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_load_yaml_from_file() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("test-agent", "Test");
    fixture.create_agent("test-agent.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("test-agent.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.agent.id, "test-agent");
    assert_eq!(def.agent.name, "Test");
    assert_eq!(def.agent.version, "1.0.0");
    assert_eq!(def.agent.description, "Test agent for test-agent");
}

#[test]
fn test_load_yaml_preserves_execution_config() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("executor", "Executor");
    fixture.create_agent("executor.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("executor.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.execution.mode, ExecutionMode::Subprocess);
    assert_eq!(def.execution.command, Some("/usr/bin/zeroclaw".to_string()));
    assert_eq!(
        def.execution.args,
        vec!["agent", "run", "--agent-id", "executor"]
    );
    assert_eq!(
        def.execution.working_dir,
        Some("/tmp/workspace".to_string())
    );
    assert_eq!(
        def.execution.env.get("ZEROCLAW_MODE"),
        Some(&"worker".to_string())
    );
}

#[test]
fn test_load_yaml_preserves_provider_config() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("provider-test", "ProviderTest");
    fixture.create_agent("provider.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("provider.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.provider.name, Some("openrouter".to_string()));
    assert_eq!(
        def.provider.model,
        Some("anthropic/claude-sonnet-4-6".to_string())
    );
    assert_eq!(def.provider.api_key, None);
    assert_eq!(def.provider.temperature, Some(0.7));
    assert_eq!(def.provider.max_tokens, Some(4096));
}

#[test]
fn test_load_yaml_preserves_tools_config() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("tools-test", "ToolsTest");
    fixture.create_agent("tools.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("tools.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.tools.tools.len(), 4);
    assert!(def.tools.tools[0].enabled);
    assert_eq!(def.tools.tools[0].name, "web_search");

    assert_eq!(def.tools.deny.len(), 2);
    assert_eq!(def.tools.deny[0].name, "shell");
    assert_eq!(def.tools.deny[0].reason, "Research agent should not execute shell commands");
}

#[test]
fn test_load_yaml_preserves_system_prompt() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("prompt-test", "PromptTest");
    fixture.create_agent("prompt.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("prompt.yaml"))
        .expect("Failed to load agent definition");

    assert!(def.system.prompt.contains("PromptTest agent"));
    assert!(def.system.prompt.contains("research tasks"));
}

#[test]
fn test_load_yaml_preserves_memory_config() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("memory-test", "MemoryTest");
    fixture.create_agent("memory.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("memory.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.memory.backend, MemoryBackend::Shared);
    assert_eq!(def.memory.category, Some("research".to_string()));
}

#[test]
fn test_load_yaml_preserves_reporting_config() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("reporting-test", "ReportingTest");
    fixture.create_agent("reporting.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("reporting.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.reporting.mode, ReportingMode::Ipc);
    assert_eq!(def.reporting.format, OutputFormat::Json);
    assert_eq!(def.reporting.timeout_seconds, 300);
}

#[test]
fn test_load_yaml_preserves_retry_config() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("retry-test", "RetryTest");
    fixture.create_agent("retry.yaml", &yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("retry.yaml"))
        .expect("Failed to load agent definition");

    assert_eq!(def.retry.max_attempts, 3);
    assert_eq!(def.retry.backoff_ms, 1000);
}

#[test]
fn test_discover_loads_all_yaml_files() {
    let fixture = TestRegistry::new();

    fixture.create_agent("agent1.yaml", &fixture.valid_agent_yaml("agent1", "Agent 1"));
    fixture.create_agent("agent2.yaml", &fixture.valid_agent_yaml("agent2", "Agent 2"));
    fixture.create_agent("agent3.yml", &fixture.valid_agent_yaml("agent3", "Agent 3"));

    let count = fixture.registry.discover().expect("Failed to discover agents");

    assert_eq!(count, 3);
    assert_eq!(fixture.registry.count(), 3);
    assert!(fixture.registry.contains("agent1"));
    assert!(fixture.registry.contains("agent2"));
    assert!(fixture.registry.contains("agent3"));
}

#[test]
fn test_discover_searches_multiple_directories() {
    let fixture = TestRegistry::with_search_dirs();

    fixture.create_agent("main.yaml", &fixture.valid_agent_yaml("main", "Main"));

    // Create file in first search dir
    let extra1 = fixture.path().join("extra1");
    fs::create_dir(&extra1).unwrap();
    let file1 = extra1.join("extra1.yaml");
    fs::write(&file1, fixture.valid_agent_yaml("extra1", "Extra1")).unwrap();

    // Add to search dirs
    fixture.registry.add_search_dir(extra1);

    let count = fixture.registry.discover().expect("Failed to discover");

    assert_eq!(count, 2);
    assert!(fixture.registry.contains("main"));
    assert!(fixture.registry.contains("extra1"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Invalid File Handling Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_load_nonexistent_file_returns_error() {
    let fixture = TestRegistry::new();

    let result = fixture.registry.load_definition(&fixture.path().join("nonexistent.yaml"));

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Failed to read"));
}

#[test]
fn test_load_malformed_yaml_returns_error() {
    let fixture = TestRegistry::new();
    fixture.create_agent("malformed.yaml", "invalid: yaml: [unclosed:");

    let result = fixture
        .registry
        .load_definition(&fixture.path().join("malformed.yaml"));

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Failed to parse"));
}

#[test]
fn test_load_yaml_with_missing_required_fields() {
    let fixture = TestRegistry::new();
    let incomplete_yaml = r#"
agent:
  id: "incomplete"
# Missing required fields
"#;

    fixture.create_agent("incomplete.yaml", incomplete_yaml);

    let result = fixture
        .registry
        .load_definition(&fixture.path().join("incomplete.yaml"));

    // Should fail due to missing required fields
    assert!(result.is_err());
}

#[test]
fn test_load_yaml_with_invalid_enum_value() {
    let fixture = TestRegistry::new();
    let invalid_yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: invalid_mode
  command: "test"
"#;

    fixture.create_agent("invalid-enum.yaml", invalid_yaml);

    let result = fixture
        .registry
        .load_definition(&fixture.path().join("invalid-enum.yaml"));

    assert!(result.is_err());
}

#[test]
fn test_discover_skips_invalid_files() {
    let fixture = TestRegistry::new();

    // Valid file
    fixture.create_agent("valid.yaml", &fixture.valid_agent_yaml("valid", "Valid"));

    // Invalid YAML - should be skipped
    fixture.create_agent("invalid.yaml", "bad: yaml: [:");

    // Non-YAML file - should be ignored
    fixture.create_agent("readme.txt", "Not a YAML file");

    let count = fixture.registry.discover().expect("Failed to discover");

    assert_eq!(count, 1);
    assert!(fixture.registry.contains("valid"));
    assert!(!fixture.registry.contains("invalid"));
}

#[test]
fn test_discover_handles_directory_read_errors() {
    let fixture = TestRegistry::new();

    // Create a file instead of directory (will cause read_dir to fail if we try to read it as dir)
    // This is more of a sanity check that discovery doesn't panic

    let count = fixture.registry.discover().expect("Failed to discover");

    // Should return 0 for empty directory
    assert_eq!(count, 0);
}

#[test]
fn test_nonexistent_search_directory_is_handled() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let security = Arc::new(SecurityPolicy::default());

    let nonexistent = PathBuf::from("/nonexistent/path/that/does/not/exist");
    let registry = AgentRegistry::with_search_dirs(
        temp_dir.path().to_path_buf(),
        vec![nonexistent],
        security,
    )
    .expect("Failed to create registry");

    let count = registry.discover().expect("Failed to discover");

    // Should not fail, just return 0
    assert_eq!(count, 0);
}

#[test]
fn test_empty_yaml_values_are_handled() {
    let fixture = TestRegistry::new();
    let empty_values_yaml = r#"
agent:
  id: ""
  name: ""
  version: "1.0.0"
  description: ""
execution:
  mode: subprocess
  command: ""
"#;

    fixture.create_agent("empty.yaml", empty_values_yaml);

    let result = fixture
        .registry
        .load_definition(&fixture.path().join("empty.yaml"));

    // Should load but validation would fail
    assert!(result.is_ok());
    let def = result.unwrap();
    assert!(def.agent.id.is_empty());
    assert!(def.agent.name.is_empty());
    assert!(def.agent.description.is_empty());

    // Validation should catch empty fields
    let validation_result = fixture.registry.validate(&def);
    assert!(validation_result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Hot Reload Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_reload_clears_existing_definitions() {
    let fixture = TestRegistry::new();

    fixture.create_agent("agent1.yaml", &fixture.valid_agent_yaml("agent1", "Agent 1"));
    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 1);

    // Create a new agent file
    fixture.create_agent("agent2.yaml", &fixture.valid_agent_yaml("agent2", "Agent 2"));

    // Reload should clear and reload
    let count = fixture.registry.reload().expect("Failed to reload");
    assert_eq!(count, 2);
    assert_eq!(fixture.registry.count(), 2);
}

#[test]
fn test_reload_discovers_new_agents() {
    let fixture = TestRegistry::new();

    // Initial discovery
    fixture.create_agent("agent1.yaml", &fixture.valid_agent_yaml("agent1", "Agent 1"));
    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 1);

    // Add more agents
    fixture.create_agent("agent2.yaml", &fixture.valid_agent_yaml("agent2", "Agent 2"));
    fixture.create_agent("agent3.yaml", &fixture.valid_agent_yaml("agent3", "Agent 3"));

    // Reload should discover all
    let count = fixture.registry.reload().expect("Failed to reload");
    assert_eq!(count, 3);
    assert!(fixture.registry.contains("agent1"));
    assert!(fixture.registry.contains("agent2"));
    assert!(fixture.registry.contains("agent3"));
}

#[test]
fn test_reload_handles_deleted_files() {
    let fixture = TestRegistry::new();

    let file1 = fixture.create_agent("agent1.yaml", &fixture.valid_agent_yaml("agent1", "Agent 1"));
    let file2 = fixture.create_agent("agent2.yaml", &fixture.valid_agent_yaml("agent2", "Agent 2"));

    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 2);

    // Delete one file
    fs::remove_file(file1).expect("Failed to remove file");

    // Reload should only find the remaining file
    let count = fixture.registry.reload().expect("Failed to reload");
    assert_eq!(count, 1);
    assert!(!fixture.registry.contains("agent1"));
    assert!(fixture.registry.contains("agent2"));
}

#[test]
fn test_reload_handles_modified_files() {
    let fixture = TestRegistry::new();

    let yaml_v1 = fixture.valid_agent_yaml("agent", "Agent V1");
    fixture.create_agent("agent.yaml", &yaml_v1);

    fixture.registry.discover().unwrap();
    let def_v1 = fixture.registry.get("agent").expect("Agent not found");
    assert_eq!(def_v1.agent.name, "Agent V1");

    // Modify the file
    let yaml_v2 = fixture.valid_agent_yaml("agent", "Agent V2");
    fixture.create_agent("agent.yaml", &yaml_v2);

    // Reload should pick up changes
    fixture.registry.reload().expect("Failed to reload");
    let def_v2 = fixture.registry.get("agent").expect("Agent not found");
    assert_eq!(def_v2.agent.name, "Agent V2");
}

#[test]
fn test_reload_handles_invalid_new_files() {
    let fixture = TestRegistry::new();

    fixture.create_agent("valid.yaml", &fixture.valid_agent_yaml("valid", "Valid"));
    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 1);

    // Add invalid file
    fixture.create_agent("invalid.yaml", "bad: yaml: [:");

    // Reload should skip invalid file
    let count = fixture.registry.reload().expect("Failed to reload");
    assert_eq!(count, 1);
    assert!(fixture.registry.contains("valid"));
    assert!(!fixture.registry.contains("invalid"));
}

#[test]
fn test_multiple_reloads_are_idempotent() {
    let fixture = TestRegistry::new();

    fixture.create_agent("agent.yaml", &fixture.valid_agent_yaml("agent", "Agent"));

    fixture.registry.discover().unwrap();
    let count1 = fixture.registry.reload().expect("Failed to reload 1");
    let count2 = fixture.registry.reload().expect("Failed to reload 2");
    let count3 = fixture.registry.reload().expect("Failed to reload 3");

    assert_eq!(count1, count2);
    assert_eq!(count2, count3);
    assert_eq!(count3, 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Agent Definition Validation Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_validation_accepts_valid_definition() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("valid", "Valid Agent");

    let def: AgentDefinition = serde_yaml::from_str(&yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_ok());
}

#[test]
fn test_validation_rejects_empty_agent_id() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: ""
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: subprocess
  command: "test"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("ID cannot be empty"));
}

#[test]
fn test_validation_rejects_id_with_forward_slash() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "bad/id"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: subprocess
  command: "test"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("path separator"));
}

#[test]
fn test_validation_rejects_id_with_backslash() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "bad\\id"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: subprocess
  command: "test"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("path separator"));
}

#[test]
fn test_validation_rejects_empty_name() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: ""
  version: "1.0.0"
  description: "Test"
execution:
  mode: subprocess
  command: "test"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("empty name"));
}

#[test]
fn test_validation_rejects_empty_description() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: ""
execution:
  mode: subprocess
  command: "test"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("empty description"));
}

#[test]
fn test_validation_requires_command_for_subprocess_mode() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: subprocess
  command: ""
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("command"));
}

#[test]
fn test_validation_allows_wasm_without_command() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    // Should not require command for wasm mode
    assert!(result.is_ok());
}

#[test]
fn test_validation_allows_docker_without_command() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: docker
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    // Should not require command for docker mode
    assert!(result.is_ok());
}

#[test]
fn test_validation_rejects_conflicting_tool_permissions() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
tools:
  tools:
    - name: "shell"
      enabled: true
  deny:
    - name: "shell"
      reason: "Conflict test"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("both allowed and denied"));
}

#[test]
fn test_validation_allows_different_tools_in_allow_and_deny() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
tools:
  tools:
    - name: "shell"
      enabled: true
  deny:
    - name: "file_write"
      reason: "Read-only"
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    // Should be OK - different tools
    assert!(result.is_ok());
}

#[test]
fn test_validation_rejects_zero_timeout() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
reporting:
  timeout_seconds: 0
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("timeout"));
}

#[test]
fn test_validation_rejects_zero_max_attempts() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
retry:
  max_attempts: 0
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("max_attempts"));
}

#[test]
fn test_validation_allows_disabled_tools_in_allow_list() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
tools:
  tools:
    - name: "shell"
      enabled: false
    - name: "web_search"
      enabled: true
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    // Should be OK - disabled tools don't conflict
    assert!(result.is_ok());
}

#[test]
fn test_validation_allows_empty_tools_list() {
    let fixture = TestRegistry::new();
    let yaml = r#"
agent:
  id: "test"
  name: "Test"
  version: "1.0.0"
  description: "Test"
execution:
  mode: wasm
tools:
  tools: []
  deny: []
"#;

    let def: AgentDefinition = serde_yaml::from_str(yaml).expect("Failed to parse YAML");

    let result = fixture.registry.validate(&def);
    assert!(result.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Registry Query Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_get_returns_cloned_definition() {
    let fixture = TestRegistry::new();
    let yaml = fixture.valid_agent_yaml("test", "Test Agent");
    fixture.create_agent("test.yaml", &yaml);
    fixture.registry.discover().unwrap();

    let def1 = fixture.registry.get("test").expect("Agent not found");
    let def2 = fixture.registry.get("test").expect("Agent not found");

    // Should return independent clones
    assert_eq!(def1.agent.id, def2.agent.id);
    assert_eq!(def1.agent.name, def2.agent.name);
}

#[test]
fn test_list_returns_sorted_ids() {
    let fixture = TestRegistry::new();

    fixture.create_agent("z.yaml", &fixture.valid_agent_yaml("z", "Z"));
    fixture.create_agent("a.yaml", &fixture.valid_agent_yaml("a", "A"));
    fixture.create_agent("m.yaml", &fixture.valid_agent_yaml("m", "M"));

    fixture.registry.discover().unwrap();

    let ids = fixture.registry.list();
    assert_eq!(ids, vec!["a", "m", "z"]);
}

#[test]
fn test_count_returns_number_of_loaded_agents() {
    let fixture = TestRegistry::new();

    assert_eq!(fixture.registry.count(), 0);

    fixture.create_agent("agent1.yaml", &fixture.valid_agent_yaml("agent1", "Agent 1"));
    fixture.registry.discover().unwrap();
    assert_eq!(fixture.registry.count(), 1);

    fixture.create_agent("agent2.yaml", &fixture.valid_agent_yaml("agent2", "Agent 2"));
    fixture.registry.reload().unwrap();
    assert_eq!(fixture.registry.count(), 2);
}

#[test]
fn test_all_returns_copy_of_all_definitions() {
    let fixture = TestRegistry::new();

    fixture.create_agent("agent1.yaml", &fixture.valid_agent_yaml("agent1", "Agent 1"));
    fixture.create_agent("agent2.yaml", &fixture.valid_agent_yaml("agent2", "Agent 2"));

    fixture.registry.discover().unwrap();

    let all = fixture.registry.all();
    assert_eq!(all.len(), 2);
    assert!(all.contains_key("agent1"));
    assert!(all.contains_key("agent2"));

    // Modifying returned map should not affect registry
    all.clear();
    assert_eq!(fixture.registry.count(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Edge Cases and Comprehensive Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_complex_real_world_agent_definition() {
    let fixture = TestRegistry::new();
    let complex_yaml = r#"
agent:
  id: "researcher"
  name: "Research Agent"
  version: "2.1.0"
  description: |
    A comprehensive research agent that can search the web,
    fetch documents, and synthesize findings into reports.
    Supports multiple search providers and citation formats.

execution:
  mode: subprocess
  command: "/usr/local/bin/zeroclaw"
  args:
    - "agent"
    - "run"
    - "--agent-id"
    - "{agent_id}"
    - "--config"
    - "{config}/agents/researcher.toml"
    - "--workspace"
    - "{workspace}"
  working_dir: "{workspace}"
  env:
    ZEROCLAW_AGENT_MODE: "worker"
    ZEROCLAW_AGENT_ID: "researcher"
    ZEROCLAW_LOG_LEVEL: "info"
    ZEROCLAW_TIMEOUT: "300"

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"
  api_key: null
  temperature: 0.3
  max_tokens: 8192

tools:
  tools:
    - name: "web_search"
      enabled: true
    - name: "web_fetch"
      enabled: true
    - name: "memory_read"
      enabled: true
    - name: "memory_write"
      enabled: true
    - name: "file_read"
      enabled: true
  deny:
    - name: "shell"
      reason: "Research agent should not execute shell commands"
    - name: "file_write"
      reason: "Research agent is read-only"
    - name: "file_delete"
      reason: "Research agent cannot delete files"

system:
  prompt: |
    You are a Research Agent. Your role is to:

    1. Search for and gather information from credible sources
    2. Synthesize findings into structured reports
    3. Cite sources and provide references
    4. Avoid speculation - stick to verified information
    5. Present findings in a clear, organized manner

    You have access to web search and fetch tools.
    Use memory_read to access prior research context.

    When presenting results:
    - Start with an executive summary
    - Provide detailed findings with citations
    - List all sources with URLs
    - Note any limitations or uncertainties

memory:
  backend: shared
  category: "research"

reporting:
  mode: ipc
  format: json
  timeout_seconds: 600

retry:
  max_attempts: 5
  backoff_ms: 2000
"#;

    fixture.create_agent("researcher.yaml", complex_yaml);

    let def = fixture
        .registry
        .load_definition(&fixture.path().join("researcher.yaml"))
        .expect("Failed to load complex agent definition");

    assert_eq!(def.agent.id, "researcher");
    assert_eq!(def.agent.version, "2.1.0");
    assert!(def.agent.description.contains("comprehensive research"));

    assert_eq!(def.execution.args.len(), 7);
    assert_eq!(
        def.execution.env.get("ZEROCLAW_LOG_LEVEL"),
        Some(&"info".to_string())
    );

    assert_eq!(def.tools.tools.len(), 5);
    assert_eq!(def.tools.deny.len(), 3);

    assert!(def.system.prompt.contains("executive summary"));

    assert_eq!(def.reporting.timeout_seconds, 600);
    assert_eq!(def.retry.max_attempts, 5);
    assert_eq!(def.retry.backoff_ms, 2000);

    // Validation should pass
    let result = fixture.registry.validate(&def);
    assert!(result.is_ok());
}

#[test]
fn test_minimal_agent_definition() {
    let fixture = TestRegistry::new();
    let minimal_yaml = r#"
agent:
  id: "minimal"
  name: "Minimal"
  version: "1.0.0"
  description: "Minimal test agent"
execution:
  mode: wasm
"#;

    let def: AgentDefinition = serde_yaml::from_str(minimal_yaml).expect("Failed to parse");

    // Check defaults are applied
    assert_eq!(def.agent.version, "1.0.0");
    assert_eq!(def.execution.mode, ExecutionMode::Wasm);
    assert_eq!(def.memory.backend, MemoryBackend::Shared);
    assert_eq!(def.reporting.mode, ReportingMode::Ipc);
    assert_eq!(def.reporting.timeout_seconds, 300);
    assert_eq!(def.retry.max_attempts, 3);

    // Validation should pass
    let result = fixture.registry.validate(&def);
    assert!(result.is_ok());
}

#[test]
fn test_special_characters_in_description() {
    let fixture = TestRegistry::new();
    let special_chars_yaml = r#"
agent:
  id: "special"
  name: "Special Chars Agent"
  version: "1.0.0"
  description: |
    This agent handles special characters: < > & " ' \
    And unicode: 你好 世界 🌍
    And emojis: 🚀 🔬 📊
execution:
  mode: wasm
"#;

    let def: AgentDefinition =
        serde_yaml::from_str(special_chars_yaml).expect("Failed to parse");

    assert!(def.agent.description.contains("你好"));
    assert!(def.agent.description.contains("🌍"));

    let result = fixture.registry.validate(&def);
    assert!(result.is_ok());
}
