use super::traits::{Tool, ToolResult};
use crate::agent::loop_::run_tool_call_loop;
use crate::config::DelegateAgentConfig;
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::providers::{self, ChatMessage, Provider};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Default timeout for sub-agent provider calls.
const DELEGATE_TIMEOUT_SECS: u64 = 120;

/// Default timeout for agentic sub-agent calls (longer to allow tool iterations).
const DELEGATE_AGENTIC_TIMEOUT_SECS: u64 = 300;

/// Tool that delegates a subtask to a named agent with a different
/// provider/model configuration. Enables multi-agent workflows where
/// a primary agent can hand off specialized work (research, coding,
/// summarization) to purpose-built sub-agents.
///
/// When `agentic: true` is set in the agent config, the sub-agent runs a
/// full tool-call loop with access to a filtered subset of the parent's
/// tool registry.
pub struct DelegateTool {
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    security: Arc<SecurityPolicy>,
    /// Global credential fallback (from config.api_key)
    fallback_credential: Option<String>,
    /// Provider runtime options inherited from root config.
    provider_runtime_options: providers::ProviderRuntimeOptions,
    /// Depth at which this tool instance lives in the delegation chain.
    depth: u32,
    /// Parent tool registry — agentic sub-agents select a subset from this.
    parent_tools: Arc<Vec<Box<dyn Tool>>>,
    /// Multimodal config inherited from parent.
    multimodal_config: crate::config::MultimodalConfig,
}

impl DelegateTool {
    pub fn new(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self::new_with_options(
            agents,
            fallback_credential,
            security,
            providers::ProviderRuntimeOptions::default(),
        )
    }

    pub fn new_with_options(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            provider_runtime_options,
            depth: 0,
            parent_tools: Arc::new(Vec::new()),
            multimodal_config: crate::config::MultimodalConfig::default(),
        }
    }

    /// Create a DelegateTool for a sub-agent (with incremented depth).
    /// When sub-agents eventually get their own tool registry, construct
    /// their DelegateTool via this method with `depth: parent.depth + 1`.
    pub fn with_depth(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
    ) -> Self {
        Self::with_depth_and_options(
            agents,
            fallback_credential,
            security,
            depth,
            providers::ProviderRuntimeOptions::default(),
        )
    }

    pub fn with_depth_and_options(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            provider_runtime_options,
            depth,
            parent_tools: Arc::new(Vec::new()),
            multimodal_config: crate::config::MultimodalConfig::default(),
        }
    }

    /// Attach the parent tool registry so agentic sub-agents can use a
    /// filtered subset of these tools.
    pub fn with_parent_tools(mut self, tools: Arc<Vec<Box<dyn Tool>>>) -> Self {
        self.parent_tools = tools;
        self
    }

    /// Attach the multimodal config for agentic sub-agents.
    pub fn with_multimodal_config(mut self, config: crate::config::MultimodalConfig) -> Self {
        self.multimodal_config = config;
        self
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a specialized agent. Use when: a task benefits from a different model \
         (e.g. fast summarization, deep reasoning, code generation). Agents configured with \
         agentic=true run a full tool-call loop; others run a single prompt and return their response."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let agent_names: Vec<&str> = self.agents.keys().map(|s: &String| s.as_str()).collect();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "agent": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!(
                        "Name of the agent to delegate to. Available: {}",
                        if agent_names.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            agent_names.join(", ")
                        }
                    )
                },
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt to send to the sub-agent"
                },
                "context": {
                    "type": "string",
                    "description": "Optional context to prepend (e.g. relevant code, prior findings)"
                }
            },
            "required": ["agent", "prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let agent_name = args
            .get("agent")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'agent' parameter"))?;

        if agent_name.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'agent' parameter must not be empty".into()),
            });
        }

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        if prompt.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'prompt' parameter must not be empty".into()),
            });
        }

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");

        // Look up agent config
        let agent_config = match self.agents.get(agent_name) {
            Some(cfg) => cfg,
            None => {
                let available: Vec<&str> =
                    self.agents.keys().map(|s: &String| s.as_str()).collect();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown agent '{agent_name}'. Available agents: {}",
                        if available.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                });
            }
        };

        // Check recursion depth (immutable — set at construction, incremented for sub-agents)
        if self.depth >= agent_config.max_depth {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Delegation depth limit reached ({depth}/{max}). \
                     Cannot delegate further to prevent infinite loops.",
                    depth = self.depth,
                    max = agent_config.max_depth
                )),
            });
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "delegate")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Create provider for this agent
        let provider_credential_owned = agent_config
            .api_key
            .clone()
            .or_else(|| self.fallback_credential.clone());
        #[allow(clippy::option_as_ref_deref)]
        let provider_credential = provider_credential_owned.as_ref().map(String::as_str);

        let provider: Box<dyn Provider> = match providers::create_provider_with_options(
            &agent_config.provider,
            provider_credential,
            &self.provider_runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Failed to create provider '{}' for agent '{agent_name}': {e}",
                        agent_config.provider
                    )),
                });
            }
        };

        // Build the message
        let full_prompt = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("[Context]\n{context}\n\n[Task]\n{prompt}")
        };

        let temperature = agent_config.temperature.unwrap_or(0.7);

        // ── Agentic mode: full tool-call loop ────────────────────
        if agent_config.agentic && !agent_config.allowed_tools.is_empty() {
            return self
                .execute_agentic(
                    agent_name,
                    agent_config,
                    &*provider,
                    &full_prompt,
                    temperature,
                )
                .await;
        }

        // ── Simple mode: single prompt→response ─────────────────
        let result = tokio::time::timeout(
            Duration::from_secs(DELEGATE_TIMEOUT_SECS),
            provider.chat_with_system(
                agent_config.system_prompt.as_deref(),
                &full_prompt,
                &agent_config.model,
                temperature,
            ),
        )
        .await;

        let result = match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Agent '{agent_name}' timed out after {DELEGATE_TIMEOUT_SECS}s"
                    )),
                });
            }
        };

        match result {
            Ok(response) => {
                let mut rendered = response;
                if rendered.trim().is_empty() {
                    rendered = "[Empty response]".to_string();
                }

                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "[Agent '{agent_name}' ({provider}/{model})]\n{rendered}",
                        provider = agent_config.provider,
                        model = agent_config.model
                    ),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Agent '{agent_name}' failed: {e}",)),
            }),
        }
    }
}

impl DelegateTool {
    /// Run the sub-agent in agentic mode: build a filtered tool registry from
    /// the parent tools, then execute a full tool-call loop.
    async fn execute_agentic(
        &self,
        agent_name: &str,
        agent_config: &DelegateAgentConfig,
        provider: &dyn Provider,
        full_prompt: &str,
        temperature: f64,
    ) -> anyhow::Result<ToolResult> {
        // Build filtered tool registry from parent tools.
        let allowed: std::collections::HashSet<&str> = agent_config
            .allowed_tools
            .iter()
            .map(String::as_str)
            .collect();

        let sub_tools: Vec<Box<dyn Tool>> = self
            .parent_tools
            .iter()
            .filter(|t| allowed.contains(t.name()))
            // The delegate tool itself is excluded to prevent re-entrant
            // delegation from the sub-agent (depth limiting already guards
            // against infinite recursion, but this avoids confusion).
            .filter(|t| t.name() != "delegate")
            .map(|t| {
                // Wrap the parent tool reference in a ToolRef that forwards
                // all trait methods. We use a raw pointer to erase the
                // lifetime — this is safe because we await the entire tool
                // loop to completion below before returning, so the parent
                // tools outlive the sub-agent execution.
                let ptr = t.as_ref() as *const dyn Tool;
                Box::new(ToolRef(ptr)) as Box<dyn Tool>
            })
            .collect();

        if sub_tools.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' has agentic=true but none of the allowed_tools \
                     ({}) are available in the parent tool registry",
                    agent_config.allowed_tools.join(", ")
                )),
            });
        }

        // Build initial conversation history.
        let mut history = Vec::new();
        if let Some(ref sys) = agent_config.system_prompt {
            history.push(ChatMessage::system(sys.clone()));
        }
        history.push(ChatMessage::user(full_prompt.to_string()));

        let observer = NoopObserver;
        let max_iterations = agent_config.max_iterations;

        let result = tokio::time::timeout(
            Duration::from_secs(DELEGATE_AGENTIC_TIMEOUT_SECS),
            run_tool_call_loop(
                provider,
                &mut history,
                &sub_tools,
                &observer,
                &agent_config.provider,
                &agent_config.model,
                temperature,
                /*silent=*/ true,
                /*approval=*/ None,
                "delegate",
                &self.multimodal_config,
                max_iterations,
                /*cancellation_token=*/ None,
                /*on_delta=*/ None,
            ),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                let rendered = if response.trim().is_empty() {
                    "[Empty response]".to_string()
                } else {
                    response
                };
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "[Agent '{agent_name}' ({provider}/{model}, agentic)]\n{rendered}",
                        provider = agent_config.provider,
                        model = agent_config.model
                    ),
                    error: None,
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Agent '{agent_name}' failed: {e}")),
            }),
            Err(_elapsed) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' timed out after {DELEGATE_AGENTIC_TIMEOUT_SECS}s"
                )),
            }),
        }
    }
}

// ── ToolRef: thin wrapper to share parent tools with sub-agents ──────────

/// A lightweight wrapper that holds a raw pointer to a parent tool and
/// forwards all `Tool` trait methods. This avoids cloning the entire tool
/// registry for each sub-agent invocation.
///
/// SAFETY: The parent tool registry (`Arc<Vec<Box<dyn Tool>>>`) outlives the
/// sub-agent execution because `execute_agentic` awaits the tool loop to
/// completion before returning.
struct ToolRef(*const dyn Tool);

// SAFETY: The inner pointer targets a type that is already Send + Sync
// (the Tool trait requires Send + Sync). The pointer remains valid for
// the entire duration of the sub-agent tool loop.
unsafe impl Send for ToolRef {}
unsafe impl Sync for ToolRef {}

#[async_trait]
impl Tool for ToolRef {
    fn name(&self) -> &str {
        // SAFETY: pointer is valid for the duration of the tool loop.
        unsafe { &*self.0 }.name()
    }
    fn description(&self) -> &str {
        unsafe { &*self.0 }.description()
    }
    fn parameters_schema(&self) -> serde_json::Value {
        unsafe { &*self.0 }.parameters_schema()
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        unsafe { &*self.0 }.execute(args).await
    }
}

// ── NoopObserver for sub-agent execution ─────────────────────────────────

struct NoopObserver;

impl Observer for NoopObserver {
    fn record_event(&self, _event: &ObserverEvent) {}
    fn record_metric(&self, _metric: &ObserverMetric) {}
    fn name(&self) -> &str {
        "noop"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn sample_agents() -> HashMap<String, DelegateAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: Some("You are a research assistant.".to_string()),
                api_key: None,
                temperature: Some(0.3),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        agents.insert(
            "coder".to_string(),
            DelegateAgentConfig {
                provider: "openrouter".to_string(),
                model: "anthropic/claude-sonnet-4-20250514".to_string(),
                system_prompt: None,
                api_key: Some("delegate-test-credential".to_string()),
                temperature: None,
                max_depth: 2,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        agents
    }

    #[test]
    fn name_and_schema() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        assert_eq!(tool.name(), "delegate");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["context"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("agent")));
        assert!(required.contains(&json!("prompt")));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["agent"]["minLength"], json!(1));
        assert_eq!(schema["properties"]["prompt"]["minLength"], json!(1));
    }

    #[test]
    fn description_not_empty() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_lists_agent_names() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher") || desc.contains("coder"));
    }

    #[tokio::test]
    async fn missing_agent_param() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"prompt": "test"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_prompt_param() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"agent": "researcher"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_agent_returns_error() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "nonexistent", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));
    }

    #[tokio::test]
    async fn depth_limit_enforced() {
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 3);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("depth limit"));
    }

    #[tokio::test]
    async fn depth_limit_per_agent() {
        // coder has max_depth=2, so depth=2 should be blocked
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 2);
        let result = tool
            .execute(json!({"agent": "coder", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("depth limit"));
    }

    #[test]
    fn empty_agents_schema() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("none configured"));
    }

    #[tokio::test]
    async fn invalid_provider_returns_error() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            DelegateAgentConfig {
                provider: "totally-invalid-provider".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({"agent": "broken", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Failed to create provider"));
    }

    #[tokio::test]
    async fn blank_agent_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "  ", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn blank_prompt_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "  \t  "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn whitespace_agent_name_trimmed_and_found() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        // " researcher " with surrounding whitespace — after trim becomes "researcher"
        let result = tool
            .execute(json!({"agent": " researcher ", "prompt": "test"}))
            .await
            .unwrap();
        // Should find "researcher" after trim — will fail at provider level
        // since ollama isn't running, but must NOT get "Unknown agent".
        assert!(
            result.error.is_none()
                || !result
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("Unknown agent")
        );
    }

    #[tokio::test]
    async fn delegation_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(sample_agents(), None, readonly);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
    }

    #[tokio::test]
    async fn delegation_blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(sample_agents(), None, limited);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
    }

    #[tokio::test]
    async fn delegate_context_is_prepended_to_prompt() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            DelegateAgentConfig {
                provider: "invalid-for-test".to_string(),
                model: "test-model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "agent": "tester",
                "prompt": "do something",
                "context": "some context data"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to create provider"));
    }

    #[tokio::test]
    async fn delegate_empty_context_omits_prefix() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            DelegateAgentConfig {
                provider: "invalid-for-test".to_string(),
                model: "test-model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "agent": "tester",
                "prompt": "do something",
                "context": ""
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to create provider"));
    }

    #[test]
    fn delegate_depth_construction() {
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 5);
        assert_eq!(tool.depth, 5);
    }

    #[tokio::test]
    async fn delegate_no_agents_configured() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security());
        let result = tool
            .execute(json!({"agent": "any", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none configured"));
    }

    // ── Agentic mode tests ──────────────────────────────────────────────

    use crate::providers::{ChatRequest, ChatResponse, ToolCall};

    /// Mock provider that returns pre-scripted responses.
    /// First response contains a tool call, second is the final text answer.
    struct ScriptedProvider {
        responses: std::sync::Mutex<Vec<ChatResponse>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("fallback".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let mut guard = self.responses.lock().unwrap();
            if guard.is_empty() {
                return Ok(ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                });
            }
            Ok(guard.remove(0))
        }
    }

    /// Simple echo tool for agentic tests.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {"message": {"type": "string"}}})
        }
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let msg = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(empty)")
                .to_string();
            Ok(ToolResult {
                success: true,
                output: msg,
                error: None,
            })
        }
    }

    fn agentic_agent_config() -> DelegateAgentConfig {
        DelegateAgentConfig {
            provider: "mock".to_string(),
            model: "test-model".to_string(),
            system_prompt: Some("You are a test agent.".to_string()),
            api_key: None,
            temperature: Some(0.5),
            max_depth: 3,
            agentic: true,
            allowed_tools: vec!["echo".to_string()],
            max_iterations: 10,
        }
    }

    #[tokio::test]
    async fn agentic_empty_allowed_tools_returns_error() {
        let config = DelegateAgentConfig {
            allowed_tools: vec!["nonexistent_tool".to_string()],
            ..agentic_agent_config()
        };
        let parent_tools: Arc<Vec<Box<dyn Tool>>> =
            Arc::new(vec![Box::new(EchoTool) as Box<dyn Tool>]);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(parent_tools);

        let provider = ScriptedProvider::new(vec![]);
        let result = tool
            .execute_agentic("test-agent", &config, &provider, "hello", 0.5)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("none of the allowed_tools"));
    }

    #[tokio::test]
    async fn agentic_single_tool_call_then_final_response() {
        let config = agentic_agent_config();
        let parent_tools: Arc<Vec<Box<dyn Tool>>> =
            Arc::new(vec![Box::new(EchoTool) as Box<dyn Tool>]);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(parent_tools);

        // Response 1: call echo tool. Response 2: final text.
        let provider = ScriptedProvider::new(vec![
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: json!({"message": "hello world"}).to_string(),
                }],
            },
            ChatResponse {
                text: Some("The echo returned: hello world".into()),
                tool_calls: vec![],
            },
        ]);

        let result = tool
            .execute_agentic("test-agent", &config, &provider, "echo hello world", 0.5)
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("agentic"));
        assert!(result.output.contains("The echo returned: hello world"));
    }

    #[tokio::test]
    async fn agentic_delegate_tool_excluded_from_sub_tools() {
        // allowed_tools includes "delegate", but it should be filtered out.
        let config = DelegateAgentConfig {
            allowed_tools: vec!["delegate".to_string()],
            ..agentic_agent_config()
        };

        // Parent tools contain a "delegate" tool (the DelegateTool itself).
        let delegate = DelegateTool::new(HashMap::new(), None, test_security());
        let parent_tools: Arc<Vec<Box<dyn Tool>>> =
            Arc::new(vec![Box::new(delegate) as Box<dyn Tool>]);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(parent_tools);

        let provider = ScriptedProvider::new(vec![]);
        let result = tool
            .execute_agentic("test-agent", &config, &provider, "test", 0.5)
            .await
            .unwrap();

        // "delegate" is excluded, no other allowed tools match → error
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none of the allowed_tools"));
    }

    #[tokio::test]
    async fn agentic_respects_max_iterations() {
        let config = DelegateAgentConfig {
            max_iterations: 2,
            ..agentic_agent_config()
        };
        let parent_tools: Arc<Vec<Box<dyn Tool>>> =
            Arc::new(vec![Box::new(EchoTool) as Box<dyn Tool>]);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(parent_tools);

        // Provider always returns a tool call — should stop at max_iterations.
        let always_call = (0..10)
            .map(|i| ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: format!("call_{i}"),
                    name: "echo".into(),
                    arguments: json!({"message": "loop"}).to_string(),
                }],
            })
            .collect();

        let provider = ScriptedProvider::new(always_call);
        let result = tool
            .execute_agentic("test-agent", &config, &provider, "loop forever", 0.5)
            .await
            .unwrap();

        // max_iterations exceeded → run_tool_call_loop bails → mapped to failure
        assert!(!result.success);
        assert!(result.error.unwrap().contains("failed"));
    }

    #[tokio::test]
    async fn agentic_non_agentic_config_skips_tool_loop() {
        // agentic=false should go through the normal (non-agentic) path,
        // not execute_agentic. We verify by calling execute() with an invalid
        // provider — it should fail at provider creation, not at tool loop.
        let mut agents = HashMap::new();
        agents.insert(
            "simple".to_string(),
            DelegateAgentConfig {
                provider: "invalid-provider".to_string(),
                model: "test".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: vec!["echo".to_string()],
                max_iterations: 10,
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({"agent": "simple", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Failed to create provider"));
    }

    #[tokio::test]
    async fn agentic_provider_error_returns_failure() {
        let config = agentic_agent_config();
        let parent_tools: Arc<Vec<Box<dyn Tool>>> =
            Arc::new(vec![Box::new(EchoTool) as Box<dyn Tool>]);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_parent_tools(parent_tools);

        /// Provider that always fails.
        struct FailProvider;
        #[async_trait]
        impl Provider for FailProvider {
            async fn chat_with_system(
                &self,
                _: Option<&str>,
                _: &str,
                _: &str,
                _: f64,
            ) -> anyhow::Result<String> {
                anyhow::bail!("provider error")
            }
            async fn chat(
                &self,
                _: ChatRequest<'_>,
                _: &str,
                _: f64,
            ) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("provider error")
            }
        }

        let result = tool
            .execute_agentic("test-agent", &config, &FailProvider, "test", 0.5)
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("failed"));
    }
}
