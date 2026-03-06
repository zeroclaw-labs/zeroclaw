# File-Based Multi-Agent Architecture Design

**Status:** Design Proposal
**Author:** multi-agent-architect
**Date:** 2026-02-24
**Type:** Alternative Architecture (Process-Per-Agent)

---

## Executive Summary

This document defines a **file-based, process-per-agent** architecture where each agent is defined as a separate configuration file and runs as an independent process. Agents are invoked on-demand and report results back to a main coordinator agent.

### Core Principles

1. **File-Based Agent Definition**: Each agent is a `.toml` file in `agents/`
2. **Process Isolation**: Each agent runs in its own process
3. **On-Demand Invocation**: Main agent spawns subprocess agents as needed
4. **Reporting Protocol**: Structured result reporting via IPC
5. **Zero-Config Discovery**: Auto-discovery of agent definitions

---

## 1. Agent Definition File Structure

### 1.1 Directory Layout

```
~/.zeroclaw/
├── config.toml              # Main configuration
├── agents/                  # Agent definitions directory
│   ├── researcher.toml      # Research agent
│   ├── coder.toml           # Code generation agent
│   ├── tester.toml          # Testing agent
│   ├── reviewer.toml        # Code review agent
│   └── summarizer.toml      # Summarization agent
├── agents.d/                # Optional: additional agent dirs
│   └── custom/
│       └── my_agent.toml
└── workspace/               # Shared workspace
```

### 1.2 Agent File Schema

```toml
# agents/researcher.toml

# Agent metadata
[agent]
id = "researcher"
name = "Research Agent"
version = "1.0.0"
description = "Conducts research on given topics using web search and knowledge bases"

# Execution configuration
[agent.execution]
# How to run this agent: "subprocess" | "wasm" | "docker"
mode = "subprocess"

# Command to spawn (template variables: {agent_id}, {workspace}, {config_dir})
command = "zeroclaw"
args = [
    "agent",
    "run",
    "--agent-id", "{agent_id}",
    "--config", "{config}/agents/researcher.toml",
    "--workspace", "{workspace}"
]

# Working directory for the subprocess
working_dir = "{workspace}"

# Environment variables for the subprocess
[agent.execution.env]
ZEROCLAW_AGENT_MODE = "worker"
ZEROCLAW_AGENT_ID = "researcher"

# Provider configuration (overrides main config)
[provider]
name = "openrouter"
model = "anthropic/claude-sonnet-4-6"
api_key = null  # Inherit from main, or set agent-specific key
temperature = 0.3
max_tokens = 4096

# Tools available to this agent
[[tools]]
name = "web_search"
enabled = true

[[tools]]
name = "web_fetch"
enabled = true

[[tools]]
name = "memory_read"
enabled = true

[[tools]]
name = "memory_write"
enabled = true

# Tools explicitly denied to this agent
[[tools.deny]]
name = "shell"
reason = "Research agent should not execute shell commands"

[[tools.deny]]
name = "file_write"
reason = "Research agent is read-only"

# System prompt
[system]
prompt = """
You are a Research Agent. Your role is to:
1. Search for and gather information from credible sources
2. Synthesize findings into structured reports
3. Cite sources and provide references
4. Avoid speculation - stick to verified information

You have access to web search and fetch tools.
Use memory_read to access prior research context.
"""

# Memory configuration
[memory]
# Use shared memory or agent-specific
backend = "shared"  # "shared" | "isolated"
category = "research"

# Reporting configuration
[reporting]
# How to report results: "stdout" | "file" | "ipc" | "http"
mode = "ipc"

# Output format
format = "json"  # "json" | "markdown" | "both"

# Timeout for agent execution
timeout_seconds = 300

# Retry configuration
[retry]
max_attempts = 3
backoff_ms = 1000
```

### 1.3 Rust Configuration Struct

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Agent definition file structure
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentDefinition {
    /// Agent metadata
    pub agent: AgentMetadata,

    /// Execution configuration
    #[serde(default)]
    pub execution: AgentExecution,

    /// Provider configuration (optional overrides)
    #[serde(default)]
    pub provider: AgentProvider,

    /// Tools configuration
    #[serde(default)]
    pub tools: AgentTools,

    /// System prompt
    #[serde(default)]
    pub system: AgentSystem,

    /// Memory configuration
    #[serde(default)]
    pub memory: AgentMemory,

    /// Reporting configuration
    #[serde(default)]
    pub reporting: AgentReporting,

    /// Retry configuration
    #[serde(default)]
    pub retry: AgentRetry,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentMetadata {
    /// Unique agent identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Semantic version
    #[serde(default = "default_version")]
    pub version: String,

    /// Description of agent's purpose
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentExecution {
    /// Execution mode
    #[serde(default = "default_execution_mode")]
    pub mode: ExecutionMode,

    /// Command template
    #[serde(default)]
    pub command: Option<String>,

    /// Command arguments
    #[serde(default)]
    pub args: Vec<String>,

    /// Working directory template
    #[serde(default)]
    pub working_dir: Option<String>,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Spawn as subprocess
    Subprocess,
    /// Run as WebAssembly module
    Wasm,
    /// Run in Docker container
    Docker,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        Self::Subprocess
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentProvider {
    /// Provider name
    #[serde(default)]
    pub name: Option<String>,

    /// Model identifier
    #[serde(default)]
    pub model: Option<String>,

    /// API key override (null = inherit)
    #[serde(default)]
    pub api_key: Option<String>,

    /// Temperature
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Max tokens
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentTools {
    /// Allowed tools
    #[serde(default)]
    pub tools: Vec<AgentToolConfig>,

    /// Denied tools
    #[serde(default)]
    pub deny: Vec<AgentToolDeny>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolConfig {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolDeny {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentSystem {
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentMemory {
    #[serde(default = "default_memory_backend")]
    pub backend: MemoryBackend,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBackend {
    /// Share main agent's memory
    Shared,
    /// Isolated memory per agent
    Isolated,
    /// No memory
    None,
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::Shared
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentReporting {
    /// Reporting mode
    #[serde(default = "default_reporting_mode")]
    pub mode: ReportingMode,

    /// Output format
    #[serde(default = "default_output_format")]
    pub format: OutputFormat,

    /// Execution timeout
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportingMode {
    /// Report via stdout
    Stdout,
    /// Write to result file
    File,
    /// Inter-process communication
    Ipc,
    /// HTTP callback
    Http,
}

impl Default for ReportingMode {
    fn default() -> Self {
        Self::Ipc
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Json,
    Markdown,
    Both,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Json
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentRetry {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,

    #[serde(default = "default_backoff")]
    pub backoff_ms: u64,
}

impl Default for AgentRetry {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_ms: 1000,
        }
    }
}
```

---

## 2. Agent Invocation Interface

### 2.1 Agent Registry

```rust
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Registry for discovering and managing agent definitions
pub struct AgentRegistry {
    /// Base directory for agent definitions
    agents_dir: PathBuf,

    /// Loaded agent definitions
    agents: HashMap<String, AgentDefinition>,

    /// Security policy for agent execution
    security: Arc<SecurityPolicy>,
}

impl AgentRegistry {
    /// Create a new registry from the agents directory
    pub fn new(agents_dir: PathBuf, security: Arc<SecurityPolicy>) -> Result<Self> {
        let mut registry = Self {
            agents_dir,
            agents: HashMap::new(),
            security,
        };
        registry.discover()?;
        Ok(registry)
    }

    /// Discover all agent definitions in the agents directory
    pub fn discover(&mut self) -> Result<()> {
        let entries = std::fs::read_dir(&self.agents_dir)
            .map_err(|e| anyhow::anyhow!("Failed to read agents directory: {}", e))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Ok(def) = self.load_definition(&path) {
                    let id = def.agent.id.clone();
                    self.agents.insert(id, def);
                }
            }
        }

        tracing::info!("Discovered {} agent definitions", self.agents.len());
        Ok(())
    }

    /// Load a single agent definition from file
    pub fn load_definition(&self, path: &Path) -> Result<AgentDefinition> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read agent file {}: {}", path.display(), e))?;

        let def: AgentDefinition = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse agent file {}: {}", path.display(), e))?;

        // Validate the definition
        self.validate(&def)?;

        Ok(def)
    }

    /// Validate an agent definition
    fn validate(&self, def: &AgentDefinition) -> Result<()> {
        // Check ID format
        if def.agent.id.is_empty() || def.agent.id.contains('/') {
            bail!("Invalid agent ID: '{}'", def.agent.id);
        }

        // Check command for subprocess mode
        if def.execution.mode == ExecutionMode::Subprocess {
            if def.execution.command.is_none() {
                bail!("Agent '{}' requires 'command' for subprocess mode", def.agent.id);
            }
        }

        // Check for conflicting tool permissions
        let allowed: std::collections::HashSet<_> = def.tools.tools.iter()
            .filter(|t| t.enabled)
            .map(|t| &t.name)
            .collect();

        for denied in &def.tools.deny {
            if allowed.contains(&denied.name) {
                bail!("Agent '{}' has tool '{}' both allowed and denied",
                      def.agent.id, denied.name);
            }
        }

        Ok(())
    }

    /// Get an agent definition by ID
    pub fn get(&self, id: &str) -> Option<&AgentDefinition> {
        self.agents.get(id)
    }

    /// List all available agent IDs
    pub fn list(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    /// Check if an agent exists
    pub fn contains(&self, id: &str) -> bool {
        self.agents.contains_key(id)
    }
}
```

### 2.2 Agent Spawner

```rust
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Spawns and manages agent subprocesses
pub struct AgentSpawner {
    registry: Arc<AgentRegistry>,
    runtime: Arc<dyn RuntimeAdapter>,
    config: Arc<Config>,
}

impl AgentSpawner {
    pub fn new(
        registry: Arc<AgentRegistry>,
        runtime: Arc<dyn RuntimeAdapter>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            registry,
            runtime,
            config,
        }
    }

    /// Invoke an agent with a task
    pub async fn invoke(&self, agent_id: &str, task: AgentTask) -> Result<AgentResult> {
        let def = self.registry.get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent_id))?;

        // Security check
        self.security.check_agent_spawn(agent_id, def)?;

        match def.execution.mode {
            ExecutionMode::Subprocess => self.invoke_subprocess(def, task).await,
            ExecutionMode::Wasm => self.invoke_wasm(def, task).await,
            ExecutionMode::Docker => self.invoke_docker(def, task).await,
        }
    }

    /// Invoke agent as subprocess
    async fn invoke_subprocess(&self, def: &AgentDefinition, task: AgentTask) -> Result<AgentResult> {
        let command_template = def.execution.command.as_ref()
            .ok_or_else(|| anyhow::anyhow!("No command specified for agent '{}'", def.agent.id))?;

        // Build command with template variables
        let cmd_str = self.expand_template(command_template, def);

        // Build args with task input
        let mut args: Vec<String> = def.execution.args.iter()
            .map(|a| self.expand_template(a, def))
            .collect();

        // Add task as arguments or stdin
        args.push("--task-json".to_string());
        args.push(serde_json::to_string(&task)?);

        let mut cmd = Command::new(&cmd_str);
        cmd.args(&args);

        // Set environment
        for (key, value) in &def.execution.env {
            cmd.env(key, self.expand_template(value, def));
        }

        // Set working directory
        let work_dir = self.expand_template(
            def.execution.working_dir.as_ref().map(|s| s.as_str()).unwrap_or("{workspace}"),
            def
        );
        cmd.current_dir(&work_dir);

        // Spawn with timeout
        let duration = Duration::from_secs(def.reporting.timeout_seconds);
        let output = timeout(duration, cmd.output()).await
            .map_err(|_| anyhow::anyhow!("Agent '{}' timed out after {}s",
                                          def.agent.id, def.reporting.timeout_seconds))?
            .map_err(|e| anyhow::anyhow!("Failed to spawn agent '{}': {}", def.agent.id, e))?;

        // Parse result based on reporting mode
        self.parse_result(def, output).await
    }

    /// Expand template variables in a string
    fn expand_template(&self, input: &str, def: &AgentDefinition) -> String {
        input
            .replace("{agent_id}", &def.agent.id)
            .replace("{workspace}", self.config.workspace_dir.to_string_lossy().as_ref())
            .replace("{config}", self.config.config_dir.to_string_lossy().as_ref())
            .replace("{config_dir}", self.config.config_dir.to_string_lossy().as_ref())
    }

    /// Parse agent output into structured result
    async fn parse_result(&self, def: &AgentDefinition, output: tokio::process::Output) -> Result<AgentResult> {
        match def.reporting.mode {
            ReportingMode::Ipc => self.parse_ipc_result(def, output).await,
            ReportingMode::Stdout => self.parse_stdout_result(def, output).await,
            ReportingMode::File => self.parse_file_result(def, output).await,
            ReportingMode::Http => self.parse_http_result(def, output).await,
        }
    }

    /// Parse IPC-based result (structured JSON over stdout)
    async fn parse_ipc_result(&self, def: &AgentDefinition, output: tokio::process::Output) -> Result<AgentResult> {
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Agent '{}' failed: {}", def.agent.id, stderr));
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| anyhow::anyhow!("Failed to read agent output: {}", e))?;

        // Parse AgentResult envelope
        let result: AgentResult = serde_json::from_str(&stdout)
            .map_err(|e| anyhow::anyhow!("Failed to parse agent result: {}", e))?;

        Ok(result)
    }

    // Other parsing methods...
}
```

---

## 3. Inter-Process Communication (IPC)

### 3.1 Task Message (Main → Agent)

```rust
/// Task sent from main agent to worker agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    /// Unique task identifier
    pub task_id: String,

    /// Main agent ID
    pub from_agent: String,

    /// Target agent ID
    pub to_agent: String,

    /// Task description/prompt
    pub prompt: String,

    /// Additional context
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,

    /// Input data
    #[serde(default)]
    pub input: Option<serde_json::Value>,

    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Deadline
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
}
```

### 3.2 Result Message (Agent → Main)

```rust
/// Result sent from worker agent back to main agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    /// Task identifier (matches AgentTask.task_id)
    pub task_id: String,

    /// Agent that produced this result
    pub agent_id: String,

    /// Execution status
    pub status: AgentStatus,

    /// Result data
    #[serde(default)]
    pub data: Option<serde_json::Value>,

    /// Human-readable output (for display)
    #[serde(default)]
    pub output: String,

    /// Error message if failed
    #[serde(default)]
    pub error: Option<String>,

    /// Execution metrics
    #[serde(default)]
    pub metrics: AgentMetrics,

    /// Artifacts produced (file paths, etc.)
    #[serde(default)]
    pub artifacts: Vec<Artifact>,

    /// Timestamp when result was produced
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Agent execution status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Task completed successfully
    Success,
    /// Task failed with error
    Failed,
    /// Task timed out
    Timeout,
    /// Task was cancelled
    Cancelled,
}

/// Execution metrics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentMetrics {
    /// Execution time in milliseconds
    #[serde(default)]
    pub duration_ms: u64,

    /// Number of LLM calls made
    #[serde(default)]
    pub llm_calls: u32,

    /// Number of tool executions
    #[serde(default)]
    pub tool_calls: u32,

    /// Tokens used (input)
    #[serde(default)]
    pub tokens_input: u32,

    /// Tokens used (output)
    #[serde(default)]
    pub tokens_output: u32,

    /// Memory consumed (bytes)
    #[serde(default)]
    pub memory_bytes: u64,
}

/// Artifact produced by agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Artifact type
    pub kind: ArtifactKind,

    /// Artifact reference (file path, URL, etc.)
    pub reference: String,

    /// Human-readable description
    #[serde(default)]
    pub description: String,

    /// Size in bytes
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Directory,
    Url,
    Data,
}
```

### 3.3 IPC Methods

#### Method A: Stdout/Stderr with JSON Envelope

```bash
# Main agent spawns worker
zeroclaw agent run --agent-id researcher --task-json '{"task_id":"...","prompt":"..."}' \
    > /tmp/agent_researcher_out.json \
    2> /tmp/agent_researcher_err.log

# Exit code indicates status
# 0 = success, 1 = failure, 124 = timeout
```

#### Method B: Unix Domain Socket

```rust
use tokio::net::UnixStream;

/// Unix socket-based IPC
pub struct UnixSocketIpc {
    socket_path: PathBuf,
}

impl UnixSocketIpc {
    /// Send task and receive result
    pub async fn call(&self, task: &AgentTask) -> Result<AgentResult> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;

        // Send task
        let task_json = serde_json::to_vec(task)?;
        let len = task_json.len() as u32;
        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(&task_json).await?;

        // Receive result
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        let mut result_buf = vec![0u8; len];
        stream.read_exact(&mut result_buf).await?;

        let result: AgentResult = serde_json::from_slice(&result_buf)?;
        Ok(result)
    }
}
```

#### Method C: Shared Memory + Signal

```rust
/// Shared memory-based IPC for large data transfer
pub struct SharedMemoryIpc {
    shm_path: PathBuf,
}

impl SharedMemoryIpc {
    /// Create shared memory region and send task
    pub async fn call(&self, task: &AgentTask) -> Result<AgentResult> {
        // Write task to shared memory
        // Signal worker with condition variable
        // Wait for result in shared memory
        // Clean up
        todo!()
    }
}
```

---

## 4. Reporting Protocol

### 4.1 Main Agent CLI Integration

```rust
/// CLI command to invoke an agent
#[derive(Debug, Parser)]
pub struct AgentInvokeCommand {
    /// Agent ID to invoke
    #[arg(long)]
    agent_id: String,

    /// Task as JSON string
    #[arg(long)]
    task_json: String,

    /// Output file for result
    #[arg(long)]
    output: Option<PathBuf>,
}

impl AgentInvokeCommand {
    pub async fn execute(self) -> Result<()> {
        // Parse task
        let task: AgentTask = serde_json::from_str(&self.task_json)?;

        // Load agent definition
        let config = Config::load()?;
        let registry = AgentRegistry::new(
            config.config_dir.join("agents"),
            Arc::new(SecurityPolicy::default()),
        )?;

        // Spawn and run
        let spawner = AgentSpawner::new(
            Arc::new(registry),
            Arc::new(NativeRuntime::new()),
            Arc::new(config),
        );

        let result = spawner.invoke(&self.agent_id, task).await?;

        // Output result
        let output_json = serde_json::to_string_pretty(&result)?;
        match &self.output {
            Some(path) => tokio::fs::write(path, output_json).await?,
            None => println!("{output_json}"),
        }

        Ok(())
    }
}
```

### 4.2 New Tool: InvokeAgentTool

```rust
/// Tool for main agent to invoke worker agents
pub struct InvokeAgentTool {
    spawner: Arc<AgentSpawner>,
    current_agent_id: String,
    security: Arc<SecurityPolicy>,
}

#[async_trait]
impl Tool for InvokeAgentTool {
    fn name(&self) -> &str {
        "invoke_agent"
    }

    fn description(&self) -> &str {
        "Invoke a specialized worker agent to perform a task. The agent runs as a separate process \
         and reports back results."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Agent ID to invoke (e.g., 'researcher', 'coder')"
                },
                "task": {
                    "type": "string",
                    "description": "Task description or prompt for the agent"
                },
                "context": {
                    "type": "object",
                    "description": "Additional context as key-value pairs"
                },
                "timeout_seconds": {
                    "type": "number",
                    "description": "Override default timeout",
                    "default": 300
                }
            },
            "required": ["agent", "task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let agent_id = args.get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'agent' parameter"))?;

        let task_description = args.get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'task' parameter"))?;

        // Check security
        self.security.enforce_tool_operation(ToolOperation::Act, "invoke_agent")?;

        // Build task
        let context = args.get("context")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| serde_json::from_value(v.clone()).ok().map(|val| (k.clone(), val)))
                    .collect()
            })
            .unwrap_or_default();

        let task = AgentTask {
            task_id: format!("{}-{}", agent_id, uuid::Uuid::new_v4()),
            from_agent: self.current_agent_id.clone(),
            to_agent: agent_id.to_string(),
            prompt: task_description.to_string(),
            context,
            input: args.get("input").cloned(),
            timestamp: chrono::Utc::now(),
            deadline: args.get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64)),
        };

        // Invoke agent
        let result = self.spawner.invoke(agent_id, task).await?;

        // Format result
        Ok(ToolResult {
            success: result.status == AgentStatus::Success,
            output: result.output,
            error: result.error,
        })
    }
}
```

---

## 5. Implementation Plan

### 5.1 File Structure

```
src/
├── agents/                   # New module
│   ├── mod.rs
│   ├── registry.rs          # AgentRegistry
│   ├── spawner.rs           # AgentSpawner
│   ├── definition.rs        # AgentDefinition structs
│   └── ipc.rs               # IPC implementations
├── tools/
│   └── invoke_agent.rs      # InvokeAgentTool
└── cli/
    └── agent.rs             # agent run/invocation commands

config/
├── agents/                  # Default agent definitions
│   ├── researcher.toml
│   ├── coder.toml
│   └── template.toml        # Template for new agents
```

### 5.2 Dependencies

```toml
# New dependencies (minimal)
tokio = { version = "1", features = ["process", "net", "io-util"] }
serde_json = "1"
toml = "0.8"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }

# Optional for advanced IPC
shared_memory = { version = "0.12", optional = true }
```

### 5.3 CLI Commands

```bash
# List available agents
zeroclaw agent list

# Show agent details
zeroclaw agent show <agent_id>

# Run an agent directly (for testing)
zeroclaw agent run --agent-id researcher --prompt "Research X"

# Create a new agent from template
zeroclaw agent create --id my-agent --name "My Agent"

# Validate agent definition
zeroclaw agent validate <agent_id>
```

---

## 6. Example Workflow

### Scenario: Research → Code → Test Pipeline

```bash
# Main agent receives: "Build a web scraper for site X"
# Main agent decides to delegate to specialized agents

# Step 1: Invoke researcher agent
zeroclaw agent run researcher \
    --task 'Analyze the target website structure and identify scraping requirements'

# Researcher returns JSON:
{
  "status": "success",
  "output": "...",
  "data": {
    "requirements": [...],
    "technologies": ["reqwest", "select"],
    "complexity": "medium"
  }
}

# Step 2: Invoke coder agent with research context
zeroclaw agent run coder \
    --task 'Implement the web scraper' \
    --context-from researcher

# Coder returns:
{
  "status": "success",
  "output": "...",
  "artifacts": [
    {"kind": "file", "reference": "scraper.rs"}
  ]
}

# Step 3: Invoke tester agent
zeroclaw agent run tester \
    --task 'Write tests for the scraper' \
    --context-from coder

# All results collected and synthesized by main agent
```

---

## 7. Security Considerations

### 7.1 Agent Sandboxing

| Execution Mode | Isolation | Use Case |
|----------------|-----------|----------|
| Subprocess | Process-level | Trusted agents |
| Docker | Container | Untrusted, file operations |
| Wasm | Memory-only | High security needs |

### 7.2 Permission Model

```toml
# Agent-specific permissions
[permissions]
# Allow network access
network = true

# Allowed domains (allowlist)
allowed_domains = ["api.github.com", "crates.io"]

# File access scope
file_scope = "workspace"  # "none" | "workspace" | "full"

# Maximum execution time
max_execution_seconds = 300

# Maximum memory
max_memory_mb = 512
```

### 7.3 Validation

- Agent definition validation before loading
- Command template sanitization
- Tool allowlist/denylist enforcement
- Resource quota enforcement

---

## 8. Comparison with Phase 1 Design

| Aspect | Phase 1 (In-Process) | File-Based (This Design) |
|--------|---------------------|-------------------------|
| Isolation | Thread-level | Process-level |
| Overhead | Low | Higher (spawn cost) |
| Fault tolerance | Crash affects all | Isolated failures |
| Complexity | Moderate | Higher |
| Scalability | Single machine | Multi-machine possible |
| Configuration | Single file | Per-agent files |
| Use case | Tight coordination | Independent tasks |

---

## 9. Migration Strategy

1. **Phase 1**: Implement core infrastructure (registry, spawner, IPC)
2. **Phase 2**: Add default agent templates (researcher, coder, tester)
3. **Phase 3**: Add Docker/Wasm execution modes
4. **Phase 4**: Add agent marketplace/distribution

---

## 10. Success Criteria

- [ ] Agent definition files validated on load
- [ ] Subprocess agents spawn and complete tasks
- [ ] IPC protocol supports tasks >1GB data
- [ ] Main agent can invoke worker agents via tool
- [ ] Failed agents don't crash main agent
- [ ] Security policy enforced per-agent
- [ ] CLI commands for agent management
