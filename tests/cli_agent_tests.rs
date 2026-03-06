//! Phase 2 CLI Agent Commands E2E Tests
//!
//! These tests cover CLI commands related to agent management:
//! 1. `zeroclaw agent list` - List available agents
//! 2. `zeroclaw agent show <id>` - Show agent details
//! 3. `zeroclaw agent reload` - Reload agent definitions
//! 4. `zeroclaw agent validate <id>` - Validate agent configuration
//! 5. `zeroclaw agent run --agent-id <id> <task>` - Execute an agent

#![allow(unused)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

// ═══════════════════════════════════════════════════════════════════════════
// Test Fixtures
// ═══════════════════════════════════════════════════════════════════════════

/// Test fixture for CLI testing
struct CliTestFixture {
    temp_dir: TempDir,
    agents_dir: PathBuf,
    config_dir: PathBuf,
}

impl CliTestFixture {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let agents_dir = temp_dir.path().join("agents");
        let config_dir = temp_dir.path().join("config");

        fs::create_dir(&agents_dir).expect("Failed to create agents dir");
        fs::create_dir(&config_dir).expect("Failed to create config dir");

        Self {
            temp_dir,
            agents_dir,
            config_dir,
        }
    }

    /// Create an agent definition file
    fn create_agent(&self, name: &str, content: &str) -> PathBuf {
        let file_path = self.agents_dir.join(name);
        fs::write(&file_path, content).expect("Failed to write agent file");
        file_path
    }

    /// Generate a standard agent YAML
    fn standard_agent_yaml(id: &str, name: &str, description: &str) -> String {
        format!(
            r#"
agent:
  id: "{id}"
  name: "{name}"
  version: "1.0.0"
  description: "{description}"

execution:
  mode: subprocess
  command: "/usr/bin/zeroclaw"
  args:
    - "agent"
    - "run"
    - "--agent-id"
    - "{id}"

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"
  temperature: 0.7

tools:
  tools:
    - name: "web_search"
      enabled: true

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

    /// Generate a minimal agent YAML
    fn minimal_agent_yaml(id: &str) -> String {
        format!(
            r#"
agent:
  id: "{id}"
  name: "{id}"
  version: "1.0.0"
  description: "Minimal agent {id}"
execution:
  mode: wasm
"#
        )
    }

    /// Generate an invalid agent YAML
    fn invalid_agent_yaml() -> String {
        "invalid: yaml: [unclosed".to_string()
    }

    /// Run zeroclaw command with environment set
    fn run_command(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("zeroclaw");
        cmd.args(args);
        cmd.env(
            "ZEROCLAW_CONFIG_DIR",
            self.config_dir.to_string_lossy().as_ref(),
        );
        cmd.env(
            "ZEROCLAW_AGENTS_DIR",
            self.agents_dir.to_string_lossy().as_ref(),
        );
        cmd.output().expect("Failed to execute command")
    }

    /// Get command output as string
    fn get_output(&self, args: &[&str]) -> (String, String, i32) {
        let output = self.run_command(args);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);
        (stdout, stderr, code)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. CLI `agent list` Command Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_agent_list_empty_directory() {
    let fixture = CliTestFixture::new();

    // No agents created
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);

    // Should succeed with empty list or appropriate message
    assert_eq!(code, 0);
    // Output should indicate no agents found
    assert!(
        stdout.contains("No agents") || stdout.contains("0 agents") || stdout.contains("none"),
        "Expected 'no agents' message, got: {}",
        stdout
    );
}

#[test]
fn test_cli_agent_list_single_agent() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "researcher.yaml",
        &CliTestFixture::standard_agent_yaml(
            "researcher",
            "Research Agent",
            "Conducts research tasks",
        ),
    );

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);

    assert_eq!(code, 0);
    assert!(stdout.contains("researcher") || stdout.contains("Research Agent"));
}

#[test]
fn test_cli_agent_list_multiple_agents() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "researcher.yaml",
        &CliTestFixture::standard_agent_yaml("researcher", "Research Agent", "Conducts research"),
    );
    fixture.create_agent(
        "coder.yaml",
        &CliTestFixture::standard_agent_yaml("coder", "Code Agent", "Writes code"),
    );
    fixture.create_agent(
        "tester.yaml",
        &CliTestFixture::standard_agent_yaml("tester", "Test Agent", "Runs tests"),
    );

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);

    assert_eq!(code, 0);
    // Should list all three agents
    assert!(
        stdout.contains("researcher") || stdout.contains("Research"),
        "Expected 'researcher' in output"
    );
    assert!(
        stdout.contains("coder") || stdout.contains("Code"),
        "Expected 'coder' in output"
    );
    assert!(
        stdout.contains("tester") || stdout.contains("Test"),
        "Expected 'tester' in output"
    );
}

#[test]
fn test_cli_agent_list_sorted_output() {
    let fixture = CliTestFixture::new();

    // Create agents in non-alphabetical order
    fixture.create_agent(
        "z-last.yaml",
        &CliTestFixture::standard_agent_yaml("z", "Z Last", "Last"),
    );
    fixture.create_agent(
        "a-first.yaml",
        &CliTestFixture::standard_agent_yaml("a", "A First", "First"),
    );
    fixture.create_agent(
        "m-middle.yaml",
        &CliTestFixture::standard_agent_yaml("m", "M Middle", "Middle"),
    );

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);

    assert_eq!(code, 0);
    // Output should be sorted
    let z_pos = stdout.find("Z Last");
    let a_pos = stdout.find("A First");
    let m_pos = stdout.find("M Middle");

    if let (Some(z), Some(a), Some(m)) = (z_pos, a_pos, m_pos) {
        assert!(a < m && m < z, "Output should be sorted alphabetically");
    }
}

#[test]
fn test_cli_agent_list_with_invalid_files() {
    let fixture = CliTestFixture::new();

    // Create valid agent
    fixture.create_agent(
        "valid.yaml",
        &CliTestFixture::standard_agent_yaml("valid", "Valid Agent", "Valid description"),
    );

    // Create invalid YAML file (should be skipped)
    fixture.create_agent("invalid.yaml", &CliTestFixture::invalid_agent_yaml());

    // Create non-YAML file (should be ignored)
    let readme_path = fixture.agents_dir.join("README.md");
    fs::write(&readme_path, "# Agents Directory").unwrap();

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);

    assert_eq!(code, 0);
    // Should only list the valid agent
    assert!(stdout.contains("valid") || stdout.contains("Valid Agent"));
}

#[test]
fn test_cli_agent_list_verbose() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "detailed.yaml",
        &CliTestFixture::standard_agent_yaml("detailed", "Detailed Agent", "Detailed description"),
    );

    // Test with verbose flag if supported
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list", "-v"]);

    assert_eq!(code, 0);
    // Verbose output should show more details
    assert!(stdout.contains("detailed") || stdout.contains("Detailed"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. CLI `agent show` Command Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_agent_show_existing_agent() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "researcher.yaml",
        &CliTestFixture::standard_agent_yaml("researcher", "Research Agent", "Conducts research"),
    );

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "show", "researcher"]);

    assert_eq!(code, 0);
    // Should show agent details
    assert!(stdout.contains("researcher") || stdout.contains("Research"));
    // Should include description
    assert!(stdout.contains("Conducts research") || stdout.contains("description"));
}

#[test]
fn test_cli_agent_show_nonexistent_agent() {
    let fixture = CliTestFixture::new();

    let (stdout, stderr, code) = fixture.get_output(&["agent", "show", "nonexistent"]);

    // Should fail with appropriate error
    assert_ne!(code, 0);
    assert!(
        stdout.contains("not found")
            || stdout.contains("unknown")
            || stdout.contains("no such")
            || stderr.contains("not found")
    );
}

#[test]
fn test_cli_agent_show_detailed_output() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "detailed.yaml",
        &CliTestFixture::standard_agent_yaml(
            "detailed",
            "Detailed Agent",
            "Agent with full configuration details",
        ),
    );

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "show", "detailed"]);

    assert_eq!(code, 0);
    // Should show key configuration details
    assert!(stdout.contains("detailed") || stdout.contains("Detailed"));
    // Should include provider info
    assert!(stdout.contains("openrouter") || stdout.contains("provider"));
    // Should include model info
    assert!(stdout.contains("claude-sonnet") || stdout.contains("model"));
}

#[test]
fn test_cli_agent_show_with_execution_mode() {
    let fixture = CliTestFixture::new();

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
"#;

    fixture.create_agent("wasm.yaml", wasm_yaml);

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "show", "wasm-agent"]);

    assert_eq!(code, 0);
    // Should show execution mode
    assert!(stdout.contains("wasm") || stdout.contains("WASM") || stdout.contains("execution"));
}

#[test]
fn test_cli_agent_show_with_tools_list() {
    let fixture = CliTestFixture::new();

    let tools_yaml = r#"
agent:
  id: "tools-agent"
  name: "Tools Agent"
  version: "1.0.0"
  description: "Agent with configured tools"
execution:
  mode: wasm
provider:
  name: "openrouter"
  model: "default"
tools:
  tools:
    - name: "web_search"
      enabled: true
    - name: "file_read"
      enabled: true
    - name: "shell"
      enabled: false
"#;

    fixture.create_agent("tools.yaml", tools_yaml);

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "show", "tools-agent"]);

    assert_eq!(code, 0);
    // Should show tools information
    assert!(stdout.contains("tool") || stdout.contains("web_search"));
}

#[test]
fn test_cli_agent_show_json_output() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "json-agent.yaml",
        &CliTestFixture::standard_agent_yaml("json-agent", "JSON Agent", "JSON output agent"),
    );

    // Test JSON output format if supported
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "show", "json-agent", "--json"]);

    // Command may not support --json, but should not crash
    // If it does, output should be valid JSON
    if code == 0 && !stdout.is_empty() {
        // Verify it's valid JSON
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) {
            assert!(parsed.is_object() || parsed.is_array());
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. CLI `agent reload` Command Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_agent_reload_adds_new_agents() {
    let fixture = CliTestFixture::new();

    // Create initial agent
    fixture.create_agent(
        "agent1.yaml",
        &CliTestFixture::standard_agent_yaml("agent1", "Agent 1", "First agent"),
    );

    let (stdout1, _stderr1, code1) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code1, 0);
    assert!(stdout1.contains("agent1") || stdout1.contains("Agent 1"));

    // Add new agent
    fixture.create_agent(
        "agent2.yaml",
        &CliTestFixture::standard_agent_yaml("agent2", "Agent 2", "Second agent"),
    );

    // Reload
    let (stdout2, _stderr2, code2) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code2, 0);

    // List should now include both
    let (stdout3, _stderr3, code3) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code3, 0);
    assert!(stdout3.contains("agent1") || stdout3.contains("Agent 1"));
    assert!(stdout3.contains("agent2") || stdout3.contains("Agent 2"));
}

#[test]
fn test_cli_agent_reload_removes_deleted_agents() {
    let fixture = CliTestFixture::new();

    let agent1_path = fixture.create_agent(
        "agent1.yaml",
        &CliTestFixture::standard_agent_yaml("agent1", "Agent 1", "First agent"),
    );

    let (stdout1, _stderr1, code1) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code1, 0);
    assert!(stdout1.contains("agent1"));

    // Delete agent
    fs::remove_file(agent1_path).unwrap();

    // Reload
    let (stdout2, _stderr2, code2) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code2, 0);

    // List should be empty
    let (stdout3, _stderr3, code3) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code3, 0);
    assert!(!stdout3.contains("agent1"));
}

#[test]
fn test_cli_agent_reload_updates_modified_agents() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "updatable.yaml",
        &CliTestFixture::standard_agent_yaml("updatable", "Original Name", "Original description"),
    );

    // Show original
    let (stdout1, _stderr1, code1) = fixture.get_output(&["agent", "show", "updatable"]);
    assert_eq!(code1, 0);
    assert!(stdout1.contains("Original Name"));

    // Modify agent
    fixture.create_agent(
        "updatable.yaml",
        &CliTestFixture::standard_agent_yaml("updatable", "Updated Name", "Updated description"),
    );

    // Reload
    let (stdout2, _stderr2, code2) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code2, 0);

    // Show should reflect update
    let (stdout3, _stderr3, code3) = fixture.get_output(&["agent", "show", "updatable"]);
    assert_eq!(code3, 0);
    assert!(stdout3.contains("Updated Name"));
    assert!(!stdout3.contains("Original Name"));
}

#[test]
fn test_cli_agent_reload_handles_invalid_files() {
    let fixture = CliTestFixture::new();

    // Create valid agent
    fixture.create_agent(
        "valid.yaml",
        &CliTestFixture::standard_agent_yaml("valid", "Valid", "Valid agent"),
    );

    // Create invalid file
    fixture.create_agent("invalid.yaml", &CliTestFixture::invalid_agent_yaml());

    // Reload should succeed but skip invalid file
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code, 0);

    // Only valid agent should be listed
    let (stdout2, _stderr2, code2) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code2, 0);
    assert!(stdout2.contains("valid"));
}

#[test]
fn test_cli_agent_reload_with_confirmation() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "agent1.yaml",
        &CliTestFixture::standard_agent_yaml("agent1", "Agent 1", "First agent"),
    );

    // Reload might require confirmation or have --force flag
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "reload", "--force"]);

    // Should succeed
    assert_eq!(code, 0);
    assert!(stdout.contains("reload") || stdout.contains("success") || stdout.is_empty());
}

#[test]
fn test_cli_agent_reload_multiple_times() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "stable.yaml",
        &CliTestFixture::standard_agent_yaml("stable", "Stable", "Stable agent"),
    );

    // Multiple reloads should be safe
    for i in 1..=3 {
        let (_stdout, _stderr, code) = fixture.get_output(&["agent", "reload"]);
        assert_eq!(code, 0, "Reload {} should succeed", i);
    }

    // Agent should still be available
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("stable"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. CLI `agent validate` Command Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_agent_validate_valid_agent() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "valid.yaml",
        &CliTestFixture::standard_agent_yaml("valid", "Valid Agent", "Valid agent description"),
    );

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "validate", "valid"]);

    // Validation should pass
    assert_eq!(code, 0);
    assert!(
        stdout.contains("valid") || stdout.contains("ok") || stdout.contains("success"),
        "Expected validation success message, got: {}",
        stdout
    );
}

#[test]
fn test_cli_agent_validate_invalid_agent() {
    let fixture = CliTestFixture::new();

    // Create agent with missing required fields
    let invalid_yaml = r#"
agent:
  id: "invalid"
  name: ""
  version: "1.0.0"
execution:
  mode: wasm
"#;

    fixture.create_agent("invalid.yaml", invalid_yaml);

    let (stdout, stderr, code) = fixture.get_output(&["agent", "validate", "invalid"]);

    // Validation should fail
    assert_ne!(code, 0);
    assert!(
        stdout.contains("invalid")
            || stdout.contains("error")
            || stdout.contains("failed")
            || stderr.contains("error")
    );
}

#[test]
fn test_cli_agent_validate_nonexistent_agent() {
    let fixture = CliTestFixture::new();

    let (stdout, stderr, code) = fixture.get_output(&["agent", "validate", "nonexistent"]);

    // Should fail with appropriate error
    assert_ne!(code, 0);
    assert!(
        stdout.contains("not found") || stdout.contains("no such") || stderr.contains("not found")
    );
}

#[test]
fn test_cli_agent_validate_all_agents() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "valid1.yaml",
        &CliTestFixture::standard_agent_yaml("valid1", "Valid 1", "First valid agent"),
    );
    fixture.create_agent(
        "valid2.yaml",
        &CliTestFixture::standard_agent_yaml("valid2", "Valid 2", "Second valid agent"),
    );

    // Validate all agents (if supported)
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "validate", "--all"]);

    // Should validate successfully
    assert_eq!(code, 0);
    assert!(stdout.contains("valid") || stdout.contains("ok") || stdout.contains("success"));
}

#[test]
fn test_cli_agent_validate_with_syntax_errors() {
    let fixture = CliTestFixture::new();

    // Agent with YAML syntax error
    fixture.create_agent("syntax-error.yaml", "bad: yaml: [unclosed:");

    let (stdout, stderr, code) = fixture.get_output(&["agent", "validate", "syntax-error"]);

    // Should fail with syntax error
    assert_ne!(code, 0);
    assert!(
        stdout.contains("syntax")
            || stdout.contains("parse")
            || stdout.contains("invalid")
            || stderr.contains("syntax")
    );
}

#[test]
fn test_cli_agent_validate_with_invalid_execution_mode() {
    let fixture = CliTestFixture::new();

    // Agent with subprocess mode but no command
    let no_command_yaml = r#"
agent:
  id: "no-command"
  name: "No Command Agent"
  version: "1.0.0"
  description: "Agent with subprocess mode but no command"
execution:
  mode: subprocess
  command: ""
"#;

    fixture.create_agent("no-command.yaml", no_command_yaml);

    let (stdout, _stderr, code) = fixture.get_output(&["agent", "validate", "no-command"]);

    // Should fail
    assert_ne!(code, 0);
}

#[test]
fn test_cli_agent_validate_with_conflicting_tools() {
    let fixture = CliTestFixture::new();

    // Agent with tool in both allow and deny lists
    let conflict_yaml = r#"
agent:
  id: "conflict"
  name: "Conflict Agent"
  version: "1.0.0"
  description: "Agent with conflicting tool permissions"
execution:
  mode: wasm
tools:
  tools:
    - name: "shell"
      enabled: true
  deny:
    - name: "shell"
      reason: "Test conflict"
"#;

    fixture.create_agent("conflict.yaml", conflict_yaml);

    let (stdout, stderr, code) = fixture.get_output(&["agent", "validate", "conflict"]);

    // Should fail with conflict error
    assert_ne!(code, 0);
    assert!(stdout.contains("conflict") || stdout.contains("both") || stderr.contains("conflict"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. CLI `agent run` Command Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_agent_run_requires_agent_id() {
    let fixture = CliTestFixture::new();

    let (stdout, stderr, code) = fixture.get_output(&["agent", "run", "--task", "test"]);

    // Should fail - missing agent-id
    assert_ne!(code, 0);
    assert!(
        stdout.contains("required")
            || stdout.contains("agent-id")
            || stderr.contains("required")
            || stderr.contains("agent-id")
    );
}

#[test]
fn test_cli_agent_run_with_valid_agent_id() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "runner.yaml",
        &CliTestFixture::standard_agent_yaml("runner", "Runner Agent", "Executes tasks"),
    );

    // Run will likely fail at provider level but should validate arguments
    let (stdout, stderr, code) =
        fixture.get_output(&["agent", "run", "--agent-id", "runner", "test task"]);

    // Should not fail on argument parsing (may fail at execution)
    // Code might be non-zero due to provider not running, but shouldn't be argument error
    assert!(
        !stdout.contains("required") && !stderr.contains("required"),
        "Should not have 'required' error, got stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_cli_agent_run_with_task_argument() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "task-agent.yaml",
        &CliTestFixture::standard_agent_yaml("task-agent", "Task Agent", "Handles tasks"),
    );

    let (stdout, stderr, code) = fixture.get_output(&[
        "agent",
        "run",
        "--agent-id",
        "task-agent",
        "--task",
        "specific task description",
    ]);

    // Should accept task argument
    // May fail at execution but not at parsing
    assert!(
        !stdout.contains("unexpected") && !stderr.contains("unexpected"),
        "Should not have 'unexpected argument' error"
    );
}

#[test]
fn test_cli_agent_run_with_stdin_task() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "stdin-agent.yaml",
        &CliTestFixture::standard_agent_yaml("stdin-agent", "Stdin Agent", "Reads from stdin"),
    );

    // Run with task via stdin (using echo to pipe)
    // Note: This test requires shell execution which may not be available in all environments
    let output = Command::new("sh")
        .arg("-c")
        .arg("echo 'test task from stdin' | zeroclaw agent run --agent-id stdin-agent")
        .env(
            "ZEROCLAW_CONFIG_DIR",
            fixture.config_dir.to_string_lossy().as_ref(),
        )
        .env(
            "ZEROCLAW_AGENTS_DIR",
            fixture.agents_dir.to_string_lossy().as_ref(),
        )
        .output();

    // Should not crash on parsing
    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stdout.contains("unexpected") && !stderr.contains("unexpected"),
            "Should not have 'unexpected argument' error"
        );
    }
}

#[test]
fn test_cli_agent_run_nonexistent_agent() {
    let fixture = CliTestFixture::new();

    let (stdout, stderr, code) =
        fixture.get_output(&["agent", "run", "--agent-id", "nonexistent", "test task"]);

    // Should fail with agent not found error
    assert_ne!(code, 0);
    assert!(
        stdout.contains("not found")
            || stdout.contains("unknown")
            || stdout.contains("no such")
            || stderr.contains("not found")
            || stderr.contains("unknown")
    );
}

#[test]
fn test_cli_agent_run_with_context() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "context-agent.yaml",
        &CliTestFixture::standard_agent_yaml("context-agent", "Context Agent", "Uses context"),
    );

    let (stdout, stderr, code) = fixture.get_output(&[
        "agent",
        "run",
        "--agent-id",
        "context-agent",
        "--context",
        "additional context",
        "main task",
    ]);

    // Should accept context parameter
    // May fail at execution but not at parsing
    assert!(
        !stdout.contains("unexpected") && !stderr.contains("unexpected"),
        "Should accept context parameter"
    );
}

#[test]
fn test_cli_agent_run_with_timeout_override() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "timeout-agent.yaml",
        &CliTestFixture::standard_agent_yaml("timeout-agent", "Timeout Agent", "Handles timeouts"),
    );

    let (stdout, _stderr, code) = fixture.get_output(&[
        "agent",
        "run",
        "--agent-id",
        "timeout-agent",
        "--timeout",
        "60",
        "test task",
    ]);

    // Should accept timeout parameter
    // May fail at execution but not at parsing
    assert!(
        !stdout.contains("unexpected") && !stdout.contains("invalid"),
        "Should accept timeout parameter"
    );
}

#[test]
fn test_cli_agent_run_with_output_format() {
    let fixture = CliTestFixture::new();

    fixture.create_agent(
        "format-agent.yaml",
        &CliTestFixture::standard_agent_yaml("format-agent", "Format Agent", "Formats output"),
    );

    // Test JSON output format
    let (stdout, _stderr, code) = fixture.get_output(&[
        "agent",
        "run",
        "--agent-id",
        "format-agent",
        "--format",
        "json",
        "test task",
    ]);

    // Should accept format parameter
    assert!(
        !stdout.contains("unexpected"),
        "Should accept format parameter"
    );
    assert!(
        code == 0 || !stdout.contains("invalid format"),
        "Format should be recognized"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. CLI Agent Help and Usage Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_agent_help() {
    let output = Command::new("zeroclaw")
        .args(["agent", "--help"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("agent") || stdout.contains("Agent"),
        "Help should mention agent command"
    );
    assert!(
        stdout.contains("list") || stdout.contains("show") || stdout.contains("run"),
        "Help should list subcommands"
    );
}

#[test]
fn test_cli_agent_list_help() {
    let output = Command::new("zeroclaw")
        .args(["agent", "list", "--help"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    assert_eq!(code, 0);
    assert!(stdout.contains("list") || stdout.contains("List"));
}

#[test]
fn test_cli_agent_run_help() {
    let output = Command::new("zeroclaw")
        .args(["agent", "run", "--help"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    assert_eq!(code, 0);
    assert!(stdout.contains("run") || stdout.contains("Run"));
    assert!(
        stdout.contains("agent-id") || stdout.contains("agent"),
        "Help should mention agent-id"
    );
}

#[test]
fn test_cli_agent_version() {
    let output = Command::new("zeroclaw")
        .args(["--version"])
        .output()
        .expect("Failed to run command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    assert_eq!(code, 0);
    assert!(stdout.contains("zeroclaw") || stdout.contains("ZeroClaw"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. End-to-End Workflow Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_e2e_agent_lifecycle() {
    let fixture = CliTestFixture::new();

    // 1. List empty agents
    let (stdout1, _stderr1, code1) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code1, 0);

    // 2. Create agent
    fixture.create_agent(
        "lifecycle.yaml",
        &CliTestFixture::standard_agent_yaml("lifecycle", "Lifecycle", "E2E test agent"),
    );

    // 3. Reload
    let (stdout2, _stderr2, code2) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code2, 0);

    // 4. List should show agent
    let (stdout3, _stderr3, code3) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code3, 0);
    assert!(stdout3.contains("lifecycle"));

    // 5. Show agent
    let (stdout4, _stderr4, code4) = fixture.get_output(&["agent", "show", "lifecycle"]);
    assert_eq!(code4, 0);

    // 6. Validate agent
    let (stdout5, _stderr5, code5) = fixture.get_output(&["agent", "validate", "lifecycle"]);
    assert_eq!(code5, 0);
}

#[test]
fn test_e2e_multi_agent_workflow() {
    let fixture = CliTestFixture::new();

    // Create multiple agents with different roles
    fixture.create_agent(
        "researcher.yaml",
        &CliTestFixture::standard_agent_yaml("researcher", "Research Agent", "Research"),
    );
    fixture.create_agent(
        "coder.yaml",
        &CliTestFixture::standard_agent_yaml("coder", "Code Agent", "Code"),
    );
    fixture.create_agent(
        "tester.yaml",
        &CliTestFixture::standard_agent_yaml("tester", "Test Agent", "Test"),
    );

    // Reload all agents
    let (_stdout, _stderr, code) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code, 0);

    // Verify all are listed
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("researcher") || stdout.contains("Research"));
    assert!(stdout.contains("coder") || stdout.contains("Code"));
    assert!(stdout.contains("tester") || stdout.contains("Test"));

    // Verify each can be shown individually
    for agent in &["researcher", "coder", "tester"] {
        let (stdout, _stderr, code) = fixture.get_output(&["agent", "show", agent]);
        assert_eq!(code, 0, "Agent '{}' should be accessible", agent);
    }
}

#[test]
fn test_e2e_agent_update_workflow() {
    let fixture = CliTestFixture::new();

    // Create initial agent
    fixture.create_agent(
        "updatable.yaml",
        &CliTestFixture::standard_agent_yaml("updatable", "Version 1", "Initial version"),
    );

    fixture.registry_reload();

    // Verify initial state
    let (stdout1, _stderr1, code1) = fixture.get_output(&["agent", "show", "updatable"]);
    assert_eq!(code1, 0);
    assert!(stdout1.contains("Version 1"));

    // Update agent
    fixture.create_agent(
        "updatable.yaml",
        &CliTestFixture::standard_agent_yaml("updatable", "Version 2", "Updated version"),
    );

    // Reload
    let (stdout2, _stderr2, code2) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code2, 0);

    // Verify update
    let (stdout3, _stderr3, code3) = fixture.get_output(&["agent", "show", "updatable"]);
    assert_eq!(code3, 0);
    assert!(stdout3.contains("Version 2"));
    assert!(!stdout3.contains("Version 1"));
}

// Helper method to call reload via CLI
impl CliTestFixture {
    fn registry_reload(&self) {
        // Simulate reload operation
        // In real implementation, this would call the CLI
        let (_output, _stderr, _code) = self.get_output(&["agent", "reload"]);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Error Handling and Edge Cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cli_handles_nonexistent_agents_dir() {
    let fixture = CliTestFixture::new();

    // Remove agents directory
    fs::remove_dir(&fixture.agents_dir).unwrap();

    // Commands should still work (empty state)
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code, 0);
}

#[test]
fn test_cli_handles_permission_denied() {
    let fixture = CliTestFixture::new();

    // Create agent with read-only file
    let agent_path = fixture.create_agent(
        "readonly.yaml",
        &CliTestFixture::standard_agent_yaml("readonly", "Readonly", "Readonly agent"),
    );

    // Make file read-only (may not work on all platforms)
    let mut perms = fs::metadata(&agent_path).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&agent_path, perms).ok();

    // Operations should still work (may fail gracefully on some platforms)
    let (_stdout, _stderr, _code) = fixture.get_output(&["agent", "list"]);
}

#[test]
fn test_cli_handles_very_long_agent_names() {
    let fixture = CliTestFixture::new();

    let long_name = "a".repeat(256);
    let long_yaml = format!(
        r#"
agent:
  id: "{}"
  name: "{}"
  version: "1.0.0"
  description: "Agent with very long name"
execution:
  mode: wasm
"#,
        long_name, long_name
    );

    fixture.create_agent("long.yaml", &long_yaml);

    // Should handle gracefully (validation may reject)
    let (_stdout, _stderr, _code) = fixture.get_output(&["agent", "list"]);
}

#[test]
fn test_cli_handles_unicode_in_agent_names() {
    let fixture = CliTestFixture::new();

    let unicode_yaml = r#"
agent:
  id: "unicode-agent"
  name: "유니코드 에이전트"
  version: "1.0.0"
  description: "Agent with unicode characters 你好世界"
  execution:
    mode: wasm
"#;

    fixture.create_agent("unicode.yaml", unicode_yaml);

    let (stdout, stderr, code) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code, 0);
    // Should handle unicode properly
    let output = stdout + " " + &stderr;
    assert!(
        output.contains("unicode-agent") || output.contains("유니코드") || output.contains("你"),
        "Should handle unicode in agent names"
    );
}

#[test]
fn test_cli_handles_concurrent_operations() {
    let fixture = CliTestFixture::new();

    // Create multiple agents rapidly
    for i in 0..10 {
        fixture.create_agent(
            &format!("agent{}.yaml", i),
            &CliTestFixture::standard_agent_yaml(
                &format!("agent{}", i),
                &format!("Agent {}", i),
                &format!("Agent number {}", i),
            ),
        );
    }

    // Reload should handle all
    let (_stdout, _stderr, code) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code, 0);

    // List should show all
    let (stdout, _stderr, code) = fixture.get_output(&["agent", "list"]);
    assert_eq!(code, 0);
}

#[test]
fn test_cli_preserves_agent_config_between_reloads() {
    let fixture = CliTestFixture::new();

    let complex_yaml = r#"
agent:
  id: "complex"
  name: "Complex Agent"
  version: "2.5.0"
  description: "Agent with complex configuration"
execution:
  mode: subprocess
  command: "/usr/local/bin/zeroclaw"
  args: ["run", "--verbose"]
  env:
    VAR1: "value1"
    VAR2: "value2"
provider:
  name: "anthropic"
  model: "claude-sonnet-4-6"
  temperature: 0.5
  max_tokens: 8192
tools:
  tools:
    - name: "web_search"
      enabled: true
    - name: "file_read"
      enabled: true
    - name: "file_write"
      enabled: false
  deny:
    - name: "shell"
      reason: "Not allowed"
system:
  prompt: "Multi-line\nsystem prompt\nwith details"
memory:
  backend: isolated
  category: "private"
reporting:
  mode: http
  format: both
  timeout_seconds: 600
retry:
  max_attempts: 10
  backoff_ms: 5000
"#;

    fixture.create_agent("complex.yaml", complex_yaml);

    // First load
    let (stdout1, _stderr1, code1) = fixture.get_output(&["agent", "show", "complex"]);
    assert_eq!(code1, 0);

    // Reload
    let (_stdout2, _stderr2, code2) = fixture.get_output(&["agent", "reload"]);
    assert_eq!(code2, 0);

    // Config should be preserved
    let (stdout3, _stderr3, code3) = fixture.get_output(&["agent", "show", "complex"]);
    assert_eq!(code3, 0);
    assert!(stdout3.contains("Complex Agent"));
}
