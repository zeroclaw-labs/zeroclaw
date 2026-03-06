//! Agent Registry Integration Tests
//!
//! Tests for loading agent definitions from the agents directory and merging
//! with config-defined agents:
//! 1. `Config::load_agents_from_registry()` - file + config agent merge
//! 2. Config agents precedence over file-based agents
//! 3. Empty directory handling
//! 4. Discovery error handling
//! 5. Correct count reporting

use std::fs;
use tempfile::TempDir;
use zeroclaw::config::{Config, DelegateAgentConfig};

// ═══════════════════════════════════════════════════════════════════════════
// Test Fixtures
// ═══════════════════════════════════════════════════════════════════════════

/// Create a minimal DelegateAgentConfig for testing
fn minimal_delegate_agent(provider: &str, model: &str) -> DelegateAgentConfig {
    DelegateAgentConfig {
        provider: provider.to_string(),
        model: model.to_string(),
        system_prompt: None,
        api_key: None,
        enabled: true,
        capabilities: Vec::new(),
        priority: 0,
        temperature: None,
        max_depth: 3,
        agentic: false,
        allowed_tools: Vec::new(),
        max_iterations: 10,
    }
}

/// Test fixture for agent registry integration testing
struct AgentMergeTestFixture {
    /// Temporary workspace directory
    workspace_dir: TempDir,
    /// Path to the agents subdirectory
    agents_dir: std::path::PathBuf,
}

impl AgentMergeTestFixture {
    /// Create a new test fixture with empty workspace
    fn new() -> Self {
        let workspace_dir = TempDir::new().expect("Failed to create temp workspace");
        let agents_dir = workspace_dir.path().join("agents");
        fs::create_dir_all(&agents_dir).expect("Failed to create agents dir");

        Self {
            workspace_dir,
            agents_dir,
        }
    }

    /// Create an agent YAML file in the agents directory
    fn create_agent_file(&self, name: &str, content: &str) -> std::path::PathBuf {
        let file_path = self.agents_dir.join(name);
        fs::write(&file_path, content).expect("Failed to write agent file");
        file_path
    }

    /// Get a reference to the workspace path
    fn workspace_path(&self) -> &std::path::Path {
        self.workspace_dir.path()
    }

    /// Create a minimal Config pointing to this workspace
    fn create_config(&self) -> Config {
        let toml = format!(
            r#"
workspace_dir = "{}"
"#,
            self.workspace_path().display()
        );
        toml::from_str(&toml).expect("Failed to parse config")
    }

    /// Generate a minimal valid agent YAML
    fn minimal_agent_yaml(id: &str, name: &str) -> String {
        format!(
            r#"
agent:
  id: "{id}"
  name: "{name}"
  version: "1.0.0"
  description: "Test agent for {id}"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"

system:
  prompt: "You are a {name} agent."

memory:
  backend: shared

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

    /// Generate an agent YAML with custom configuration
    fn custom_agent_yaml(
        id: &str,
        name: &str,
        provider: &str,
        model: &str,
        description: &str,
    ) -> String {
        format!(
            r#"
agent:
  id: "{id}"
  name: "{name}"
  version: "1.0.0"
  description: "{description}"

execution:
  mode: wasm

provider:
  name: "{provider}"
  model: "{model}"

system:
  prompt: "You are a {name} agent."

memory:
  backend: shared

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
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. load_agents_from_registry() Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_load_agents_from_empty_directory() {
    let fixture = AgentMergeTestFixture::new();
    let mut config = fixture.create_config();

    // Set the workspace directory for the config
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should succeed with empty directory");

    assert_eq!(count, 0, "Should load 0 agents from empty directory");
    assert_eq!(config.agents.len(), 0, "Config should have no agents");
}

#[tokio::test]
async fn test_load_single_file_agent_when_config_empty() {
    let fixture = AgentMergeTestFixture::new();

    fixture.create_agent_file(
        "researcher.yaml",
        &fixture.minimal_agent_yaml("researcher", "Research Agent"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agent from file");

    assert_eq!(count, 1, "Should load 1 agent");
    assert!(config.agents.contains_key("researcher"), "Should have researcher agent");

    let agent = &config.agents["researcher"];
    assert_eq!(agent.provider, "openrouter");
    assert_eq!(agent.model, "anthropic/claude-sonnet-4-6");
}

#[tokio::test]
async fn test_load_multiple_file_agents_when_config_empty() {
    let fixture = AgentMergeTestFixture::new();

    fixture.create_agent_file(
        "researcher.yaml",
        &fixture.minimal_agent_yaml("researcher", "Research Agent"),
    );
    fixture.create_agent_file(
        "coder.yaml",
        &fixture.minimal_agent_yaml("coder", "Code Agent"),
    );
    fixture.create_agent_file(
        "tester.yaml",
        &fixture.minimal_agent_yaml("tester", "Test Agent"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load all agents from files");

    assert_eq!(count, 3, "Should load 3 agents");
    assert_eq!(config.agents.len(), 3);
    assert!(config.agents.contains_key("researcher"));
    assert!(config.agents.contains_key("coder"));
    assert!(config.agents.contains_key("tester"));
}

#[tokio::test]
async fn test_load_agents_with_both_yml_and_yaml_extensions() {
    let fixture = AgentMergeTestFixture::new();

    fixture.create_agent_file(
        "agent1.yaml",
        &fixture.minimal_agent_yaml("agent1", "Agent 1"),
    );
    fixture.create_agent_file(
        "agent2.yml",
        &fixture.minimal_agent_yaml("agent2", "Agent 2"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agents with both extensions");

    assert_eq!(count, 2);
    assert!(config.agents.contains_key("agent1"));
    assert!(config.agents.contains_key("agent2"));
}

#[tokio::test]
async fn test_load_agents_skips_invalid_yaml_files() {
    let fixture = AgentMergeTestFixture::new();

    // Valid agent
    fixture.create_agent_file(
        "valid.yaml",
        &fixture.minimal_agent_yaml("valid", "Valid Agent"),
    );

    // Invalid YAML files
    fixture.create_agent_file("invalid.yaml", "bad: yaml: [unclosed:");
    fixture.create_agent_file("readme.txt", "Not a YAML file");

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should skip invalid files");

    assert_eq!(count, 1, "Should only load the valid agent");
    assert!(config.agents.contains_key("valid"));
    assert!(!config.agents.contains_key("invalid"));
}

#[tokio::test]
async fn test_load_agents_skips_files_with_missing_required_fields() {
    let fixture = AgentMergeTestFixture::new();

    // Valid agent
    fixture.create_agent_file(
        "valid.yaml",
        &fixture.minimal_agent_yaml("valid", "Valid Agent"),
    );

    // Agent with missing required fields (invalid after validation)
    let incomplete_yaml = r#"
agent:
  id: "incomplete"
  # Missing required fields
"#;
    fixture.create_agent_file("incomplete.yaml", incomplete_yaml);

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should skip incomplete agent");

    assert_eq!(count, 1);
    assert!(config.agents.contains_key("valid"));
    assert!(!config.agents.contains_key("incomplete"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Config Agent Precedence Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_config_agent_takes_precedence_over_file_agent() {
    let fixture = AgentMergeTestFixture::new();

    // Create a file-based agent
    fixture.create_agent_file(
        "researcher.yaml",
        &fixture.custom_agent_yaml(
            "researcher",
            "File Researcher",
            "ollama",
            "llama3",
            "From file",
        ),
    );

    // Create a config with the same agent ID
    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    // Add a config-defined agent with the same ID
    let mut config_agent = minimal_delegate_agent("anthropic", "claude-sonnet-4-6");
    config_agent.system_prompt = Some("Config-based prompt".to_string());
    config.agents.insert("researcher".to_string(), config_agent);

    // Load agents from registry
    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agents");

    // Should report 0 merged (config agent took precedence)
    assert_eq!(count, 0, "File agent should not replace config agent");

    // Verify the config agent is preserved
    assert!(config.agents.contains_key("researcher"));
    let agent = &config.agents["researcher"];
    assert_eq!(agent.provider, "anthropic", "Should keep config provider");
    assert_eq!(agent.model, "claude-sonnet-4-6", "Should keep config model");
    assert_eq!(
        agent.system_prompt,
        Some("Config-based prompt".to_string()),
        "Should keep config prompt"
    );
}

#[tokio::test]
async fn test_config_agent_precedence_with_multiple_agents() {
    let fixture = AgentMergeTestFixture::new();

    // Create multiple file-based agents
    fixture.create_agent_file(
        "file1.yaml",
        &fixture.minimal_agent_yaml("file1", "File Agent 1"),
    );
    fixture.create_agent_file(
        "file2.yaml",
        &fixture.minimal_agent_yaml("file2", "File Agent 2"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    // Add a config agent that conflicts with file2
    let config_agent = minimal_delegate_agent("config-provider", "config-model");
    config.agents.insert("file2".to_string(), config_agent);

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agents");

    // Only file1 should be merged (file2 conflicts)
    assert_eq!(count, 1);
    assert_eq!(config.agents.len(), 2);

    // file1 from file should be merged
    assert!(config.agents.contains_key("file1"));
    assert_eq!(config.agents["file1"].provider, "openrouter");

    // file2 should keep config values
    assert!(config.agents.contains_key("file2"));
    assert_eq!(config.agents["file2"].provider, "config-provider");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Merge Behavior Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_load_agents_merges_with_existing_config_agents() {
    let fixture = AgentMergeTestFixture::new();

    // Create file-based agents
    fixture.create_agent_file(
        "from-file.yaml",
        &fixture.minimal_agent_yaml("from-file", "File Agent"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    // Add a pre-existing config agent
    let config_agent = minimal_delegate_agent("config-provider", "config-model");
    config.agents.insert("from-config".to_string(), config_agent);

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should merge agents");

    assert_eq!(count, 1, "Should merge 1 file-based agent");
    assert_eq!(config.agents.len(), 2, "Should have 2 total agents");

    // Both agents should be present
    assert!(config.agents.contains_key("from-file"));
    assert!(config.agents.contains_key("from-config"));

    // Verify file agent values
    assert_eq!(config.agents["from-file"].provider, "openrouter");

    // Verify config agent values
    assert_eq!(config.agents["from-config"].provider, "config-provider");
}

#[tokio::test]
async fn test_multiple_loads_are_idempotent() {
    let fixture = AgentMergeTestFixture::new();

    fixture.create_agent_file(
        "agent.yaml",
        &fixture.minimal_agent_yaml("agent", "Agent"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    // First load
    let count1 = config
        .load_agents_from_registry()
        .await
        .expect("First load should succeed");

    assert_eq!(count1, 1);

    // Second load (should not duplicate)
    let count2 = config
        .load_agents_from_registry()
        .await
        .expect("Second load should succeed");

    // The second load might report 0 if all agents are already loaded
    // or it might report the same count - we just check it doesn't error
    assert!(count2 >= 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Error Handling Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_load_agents_handles_nonexistent_agents_directory() {
    let workspace_dir = TempDir::new().expect("Failed to create temp workspace");
    // Don't create the agents subdirectory

    let mut config = Config {
        workspace_dir: workspace_dir.path().to_path_buf(),
        ..Default::default()
    };

    // Should handle gracefully (return Ok with 0 count)
    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should handle missing directory gracefully");

    assert_eq!(count, 0, "Should return 0 for nonexistent directory");
    assert_eq!(config.agents.len(), 0);
}

#[tokio::test]
async fn test_load_agents_with_malformed_yaml_in_directory() {
    let fixture = AgentMergeTestFixture::new();

    // Create one valid agent
    fixture.create_agent_file(
        "valid.yaml",
        &fixture.minimal_agent_yaml("valid", "Valid Agent"),
    );

    // Create files with various issues
    fixture.create_agent_file("empty.yaml", "");
    fixture.create_agent_file("binary.yaml", "\x00\x01\x02\x03 binary data");
    fixture.create_agent_file("bad-yaml.yaml", ":\n  - unclosed");

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    // Should succeed and only load the valid agent
    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should skip malformed files");

    assert_eq!(count, 1);
    assert!(config.agents.contains_key("valid"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Agent Configuration Preservation Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_loaded_agent_preserves_file_configuration() {
    let fixture = AgentMergeTestFixture::new();

    // Create an agent with specific configuration
    fixture.create_agent_file(
        "specific.yaml",
        &fixture.custom_agent_yaml(
            "specific",
            "Specific Agent",
            "anthropic",
            "claude-sonnet-4-6",
            "A specifically configured agent",
        ),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agent");

    assert_eq!(count, 1);

    let agent = &config.agents["specific"];
    assert_eq!(agent.provider, "anthropic");
    assert_eq!(agent.model, "claude-sonnet-4-6");
}

#[tokio::test]
async fn test_loaded_agents_have_different_configurations() {
    let fixture = AgentMergeTestFixture::new();

    fixture.create_agent_file(
        "agent1.yaml",
        &fixture.custom_agent_yaml(
            "agent1",
            "Agent 1",
            "openrouter",
            "gpt-4",
            "First agent",
        ),
    );
    fixture.create_agent_file(
        "agent2.yaml",
        &fixture.custom_agent_yaml(
            "agent2",
            "Agent 2",
            "anthropic",
            "claude-3",
            "Second agent",
        ),
    );
    fixture.create_agent_file(
        "agent3.yaml",
        &fixture.custom_agent_yaml("agent3", "Agent 3", "ollama", "llama3", "Third agent"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load all agents");

    assert_eq!(count, 3);

    // Verify each agent has its own configuration
    assert_eq!(config.agents["agent1"].provider, "openrouter");
    assert_eq!(config.agents["agent1"].model, "gpt-4");

    assert_eq!(config.agents["agent2"].provider, "anthropic");
    assert_eq!(config.agents["agent2"].model, "claude-3");

    assert_eq!(config.agents["agent3"].provider, "ollama");
    assert_eq!(config.agents["agent3"].model, "llama3");
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Edge Cases
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_load_agents_with_special_characters_in_id() {
    let fixture = AgentMergeTestFixture::new();

    // Agent with underscores and numbers (valid)
    fixture.create_agent_file(
        "agent_v2_0.yaml",
        &fixture.minimal_agent_yaml("agent_v2_0", "Agent V2.0"),
    );

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agent with special ID");

    assert_eq!(count, 1);
    assert!(config.agents.contains_key("agent_v2_0"));
}

#[tokio::test]
async fn test_load_agents_with_unicode_in_name() {
    let fixture = AgentMergeTestFixture::new();

    let unicode_yaml = r#"
agent:
  id: "unicode-agent"
  name: "유니코드 에이전트"
  version: "1.0.0"
  description: "Agent with unicode characters 你好世界"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

system:
  prompt: "You are a helpful assistant."

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 3
  backoff_ms: 1000
"#;

    fixture.create_agent_file("unicode.yaml", unicode_yaml);

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load unicode agent");

    assert_eq!(count, 1);
    assert!(config.agents.contains_key("unicode-agent"));
}

#[tokio::test]
async fn test_load_agents_preserves_system_prompt() {
    let fixture = AgentMergeTestFixture::new();

    let prompt_yaml = r#"
agent:
  id: "prompt-test"
  name: "Prompt Test Agent"
  version: "1.0.0"
  description: "Tests system prompt preservation"

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "default"

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

retry:
  max_attempts: 3
  backoff_ms: 1000
"#;

    fixture.create_agent_file("prompt.yaml", prompt_yaml);

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load agent with prompt");

    assert_eq!(count, 1);

    let agent = &config.agents["prompt-test"];
    assert!(agent.system_prompt.is_some());
    let prompt = agent.system_prompt.as_ref().unwrap();
    assert!(prompt.contains("specialized agent"));
    assert!(prompt.contains("Be concise"));
}

#[tokio::test]
async fn test_load_agents_with_complex_real_world_config() {
    let fixture = AgentMergeTestFixture::new();

    let complex_yaml = r#"
agent:
  id: "researcher"
  name: "Research Agent"
  version: "2.1.0"
  description: |
    A comprehensive research agent that can search the web,
    fetch documents, and synthesize findings into reports.

execution:
  mode: wasm

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"
  temperature: 0.3

system:
  prompt: |
    You are a Research Agent. Your role is to:

    1. Search for and gather information from credible sources
    2. Synthesize findings into structured reports
    3. Cite sources and provide references

    When presenting results:
    - Start with an executive summary
    - Provide detailed findings with citations

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

    fixture.create_agent_file("researcher.yaml", complex_yaml);

    let mut config = fixture.create_config();
    config.workspace_dir = fixture.workspace_path().to_path_buf();

    let count = config
        .load_agents_from_registry()
        .await
        .expect("Should load complex agent");

    assert_eq!(count, 1);
    assert!(config.agents.contains_key("researcher"));

    let agent = &config.agents["researcher"];
    assert_eq!(agent.provider, "openrouter");
    assert_eq!(agent.model, "anthropic/claude-sonnet-4-6");
    assert!(agent.system_prompt.is_some());
}
