use super::*;
use crate::providers::{Provider, ChatRequest};
use crate::tools::Tool;
use crate::approval::ApprovalManager;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn test_scrub_credentials() {
        let input = "API_KEY=sk-1234567890abcdef; token: 1234567890; password=\"secret123456\"";
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("API_KEY=sk-1*[REDACTED]"));
        assert!(scrubbed.contains("token: 1234*[REDACTED]"));
        assert!(scrubbed.contains("password=\"secr*[REDACTED]\""));
        assert!(!scrubbed.contains("abcdef"));
        assert!(!scrubbed.contains("secret123456"));
}

#[test]
fn test_scrub_credentials_json() {
        let input = r#"{"api_key": "sk-1234567890", "other": "public"}"#;
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("\"api_key\": \"sk-1*[REDACTED]\""));
        assert!(scrubbed.contains("public"));
    }
use crate::memory::{Memory, MemoryCategory, SqliteMemory};
use crate::observability::NoopObserver;
use crate::providers::traits::ProviderCapabilities;
use crate::providers::ChatResponse;
use tempfile::TempDir;

    struct NonVisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for NonVisionProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    struct VisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for VisionProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: true,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let marker_count = crate::multimodal::count_image_markers(request.messages);
            if marker_count == 0 {
                anyhow::bail!("expected image markers in request messages");
            }

            if request.tools.is_some() {
                anyhow::bail!("no tools should be attached for this test");
            }

            Ok(ChatResponse {
                text: Some("vision-ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    struct ScriptedProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        capabilities: ProviderCapabilities,
    }

    impl ScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
                .collect();
            Self {
                responses: Arc::new(Mutex::new(scripted)),
                capabilities: ProviderCapabilities::default(),
            }
        }

        fn with_native_tool_support(mut self) -> Self {
            self.capabilities.native_tool_calling = true;
            self
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in scripted provider tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted provider exhausted responses"))
        }
    }

    struct CountingTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Counts executions for loop-stability tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("counted:{value}"),
                error: None,
            })
        }
    }

    struct DelayTool {
        name: String,
        delay_ms: u64,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl DelayTool {
        fn new(
            name: &str,
            delay_ms: u64,
            active: Arc<AtomicUsize>,
            max_active: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                name: name.to_string(),
                delay_ms,
                active,
                max_active,
            }
        }
    }

    #[async_trait]
    impl Tool for DelayTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Delay tool for testing parallel tool execution"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"]
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now_active, Ordering::SeqCst);

            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;

            self.active.fetch_sub(1, Ordering::SeqCst);

            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();

            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("ok:{value}"),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_structured_error_for_non_vision_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
        )
        .await
        .expect_err("provider without vision support should fail");

        assert!(err.to_string().contains("provider_capability_error"));
        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_rejects_oversized_image_payload() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let oversized_payload = STANDARD.encode(vec![0_u8; (1024 * 1024) + 1]);
        let mut history = vec![ChatMessage::user(format!(
            "[IMAGE:data:image/png;base64,{oversized_payload}]"
        ))];

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;
        let multimodal = crate::config::MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 1,
            allow_remote_fetch: false,
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
        )
        .await
        .expect_err("oversized payload must fail");

        assert!(err
            .to_string()
            .contains("multimodal image size limit exceeded"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_valid_multimodal_request_flow() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "Analyze this [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
        )
        .await
        .expect("valid multimodal payload should pass");

        assert_eq!(result, "vision-ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_for_single_call() {
        let calls = vec![ParsedToolCall {
            name: "file_read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
            tool_call_id: None,
        }];

        assert!(!should_execute_tools_in_parallel(&calls, None));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_when_approval_is_required() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig::default();
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(!should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_true_when_cli_has_no_interactive_approvals() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_multiple_tools_with_ordered_results() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"delay_a","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"delay_b","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(DelayTool::new(
                "delay_a",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
            Box::new(DelayTool::new(
                "delay_b",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
        ];

        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
        )
        .await
        .expect("parallel execution should complete");

        assert_eq!(result, "done");
        assert!(
            max_active.load(Ordering::SeqCst) >= 1,
            "tools should execute successfully"
        );

        let tool_results_message = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        let idx_a = tool_results_message
            .content
            .find("name=\"delay_a\"")
            .expect("delay_a result should be present");
        let idx_b = tool_results_message
            .content
            .find("name=\"delay_b\"")
            .expect("delay_b result should be present");
        assert!(
            idx_a < idx_b,
            "tool results should preserve input order for tool call mapping"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_deduplicates_repeated_tool_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
        )
        .await
        .expect("loop should finish after deduplicating repeated calls");

        assert_eq!(result, "done");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate tool call with same args should not execute twice"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(tool_results.content.contains("counted:A"));
        assert!(tool_results.content.contains("Skipped duplicate tool call"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_allows_repeated_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let exempt = vec!["count_tool".to_string()];

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &exempt,
        )
        .await
        .expect("loop should finish with exempt tool executing twice");

        assert_eq!(result, "done");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            2,
            "exempt tool should execute both duplicate calls"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(
            !tool_results.content.contains("Skipped duplicate tool call"),
            "exempt tool calls should not be suppressed"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_only_affects_listed_tools() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"other_tool","arguments":{"value":"B"}}
</tool_call>
<tool_call>
{"name":"other_tool","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let count_invocations = Arc::new(AtomicUsize::new(0));
        let other_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(CountingTool::new(
                "count_tool",
                Arc::clone(&count_invocations),
            )),
            Box::new(CountingTool::new(
                "other_tool",
                Arc::clone(&other_invocations),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let exempt = vec!["count_tool".to_string()];

        let _result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &exempt,
        )
        .await
        .expect("loop should complete");

        assert_eq!(
            count_invocations.load(Ordering::SeqCst),
            2,
            "exempt tool should execute both calls"
        );
        assert_eq!(
            other_invocations.load(Ordering::SeqCst),
            1,
            "non-exempt tool should still be deduped"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_native_mode_preserves_fallback_tool_call_ids() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"content":"Need to call tool","tool_calls":[{"id":"call_abc","name":"count_tool","arguments":"{\"value\":\"X\"}"}]}"#,
            "done",
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
        )
        .await
        .expect("native fallback id flow should complete");

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_abc\"")
            }),
            "tool result should preserve parsed fallback tool_call_id in native mode"
        );
        assert!(
            history
                .iter()
                .all(|msg| !(msg.role == "user" && msg.content.starts_with("[Tool results]"))),
            "native mode should use role=tool history instead of prompt fallback wrapper"
        );
    }

    #[test]
    fn resolve_display_text_hides_raw_payload_for_tool_only_turns() {
        let display = resolve_display_text(
            "<tool_call>{\"name\":\"memory_store\"}</tool_call>",
            "",
            true,
        );
        assert!(display.is_empty());
    }

    #[test]
    fn resolve_display_text_keeps_plain_text_for_tool_turns() {
        let display = resolve_display_text(
            "<tool_call>{\"name\":\"shell\"}</tool_call>",
            "Let me check that.",
            true,
        );
        assert_eq!(display, "Let me check that.");
    }

    #[test]
    fn resolve_display_text_uses_response_text_for_final_turns() {
        let display = resolve_display_text("Final answer", "", false);
        assert_eq!(display, "Final answer");
    }

    #[test]
    fn parse_tool_calls_extracts_single_call() {
        let response = r#"Let me check that.
<tool_call>
{"name": "shell", "arguments": {"command": "ls -la"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_extracts_multiple_calls() {
        let response = r#"<tool_call>
{"name": "file_read", "arguments": {"path": "a.txt"}}
</tool_call>
<tool_call>
{"name": "file_read", "arguments": {"path": "b.txt"}}
</tool_call>"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_returns_text_only_when_no_calls() {
        let response = "Just a normal response with no tools.";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Just a normal response with no tools.");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_malformed_json() {
        let response = r#"<tool_call>
not valid json
</tool_call>
Some text after."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.contains("Some text after."));
    }

    #[test]
    fn parse_tool_calls_text_before_and_after() {
        let response = r#"Before text.
<tool_call>
{"name": "shell", "arguments": {"command": "echo hi"}}
</tool_call>
After text."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Before text."));
        assert!(text.contains("After text."));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn parse_tool_calls_handles_openai_format() {
        // OpenAI-style response with tool_calls array
        let response = r#"{"content": "Let me check that for you.", "tool_calls": [{"type": "function", "function": {"name": "shell", "arguments": "{\"command\": \"ls -la\"}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that for you.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_openai_format_multiple_calls() {
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"a.txt\"}"}}, {"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"b.txt\"}"}}]}"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_openai_format_without_content() {
        // Some providers don't include content field with tool_calls
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "memory_recall", "arguments": "{}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty()); // No content field
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
    }

    #[test]
    fn parse_tool_calls_preserves_openai_tool_call_ids() {
        let response = r#"{"tool_calls":[{"id":"call_42","function":{"name":"shell","arguments":"{\"command\":\"pwd\"}"}}]}"#;
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("call_42"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_json_inside_tool_call_tag() {
        let response = r#"<tool_call>
```json
{"name": "file_write", "arguments": {"path": "test.py", "content": "print('ok')"}}
```
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "test.py"
        );
    }

    #[test]
    fn parse_tool_calls_handles_noisy_tool_call_tag_body() {
        let response = r#"<tool_call>
I will now call the tool with this payload:
{"name": "shell", "arguments": {"command": "pwd"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_inline_attributes_with_send_message_alias() {
        let response = r#"<tool_call>send_message channel="user_channel" message="Hello! How can I assist you today?"</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "user_channel"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "Hello! How can I assist you today?"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_function_style_arguments() {
        let response = r#"<tool_call>message_send(channel="general", message="test")</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "general"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "test"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_nested_tool_payload() {
        let response = r#"<tool_call>
<memory_recall>
<query>project roadmap</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "project roadmap"
        );
    }

    #[test]
    fn parse_tool_calls_ignores_xml_thinking_wrapper() {
        let response = r#"<tool_call>
<thinking>Need to inspect memory first</thinking>
<memory_recall>
<query>recent deploy notes</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "recent deploy notes"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_with_json_arguments() {
        let response = r#"<tool_call>
<shell>{"command":"pwd"}</shell>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_fence() {
        let response = r#"I'll check that.
```tool_call
{"name": "shell", "arguments": {"command": "pwd"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
        assert!(text.contains("I'll check that."));
        assert!(text.contains("Done."));
        assert!(!text.contains("```tool_call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_hybrid_close_tag() {
        let response = r#"Preface
```tool-call
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert!(!text.contains("```tool-call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_invoke_fence() {
        let response = r#"Checking.
```invoke
{"name": "shell", "arguments": {"command": "date"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Checking."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_format() {
        // Issue #1420: xAI grok models use ```tool <name> format
        let response = r#"I'll write a test file.
```tool file_write
{"path": "/home/user/test.txt", "content": "Hello world"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "/home/user/test.txt"
        );
        assert!(text.contains("I'll write a test file."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_shell() {
        // Issue #1420: Test shell command in ```tool shell format
        let response = r#"```tool shell
{"command": "ls -la"}
```"#;

        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_multiple_tool_name_fences() {
        // Multiple tool calls in ```tool <name> format
        let response = r#"First, I'll write a file.
```tool file_write
{"path": "/tmp/a.txt", "content": "A"}
```
Then read it.
```tool file_read
{"path": "/tmp/a.txt"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(calls[1].name, "file_read");
        assert!(text.contains("First, I'll write a file."));
        assert!(text.contains("Then read it."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_toolcall_tag_alias() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_dash_call_tag_alias() {
        let response = r#"<tool-call>
{"name": "shell", "arguments": {"command": "whoami"}}
</tool-call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "whoami"
        );
    }

    #[test]
    fn parse_tool_calls_handles_invoke_tag_alias() {
        let response = r#"<invoke>
{"name": "shell", "arguments": {"command": "uptime"}}
</invoke>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_parameter_format() {
        let response = r#"<minimax:tool_call>
<invoke name="shell">
<parameter name="command">sqlite3 /tmp/test.db ".tables"</parameter>
</invoke>
</minimax:tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            r#"sqlite3 /tmp/test.db ".tables""#
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_with_surrounding_text() {
        let response = r#"Preface
<minimax:tool_call>
<invoke name='http_request'>
<parameter name='url'>https://example.com</parameter>
<parameter name='method'>GET</parameter>
</invoke>
</minimax:tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "http_request");
        assert_eq!(
            calls[0].arguments.get("url").unwrap().as_str().unwrap(),
            "https://example.com"
        );
        assert_eq!(
            calls[0].arguments.get("method").unwrap().as_str().unwrap(),
            "GET"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_toolcall_alias_and_cross_close_tag() {
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"date"}}
</minimax:toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_perl_style_tool_call_blocks() {
        let response = r#"TOOL_CALL
{tool => "shell", args => { --command "uname -a" }}}
/TOOL_CALL"#;

        let calls = parse_perl_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uname -a"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_unclosed_tool_call_with_json() {
        let response = r#"I will call the tool now.
<tool_call>
{"name": "shell", "arguments": {"command": "uptime -p"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I will call the tool now."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime -p"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_mismatched_close_tag() {
        let response = r#"<tool_call>
{"name": "shell", "arguments": {"command": "uptime"}}
</arg_value>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_cross_alias_closing_tags() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn parse_tool_calls_rejects_raw_tool_json_without_tags() {
        // SECURITY: Raw JSON without explicit wrappers should NOT be parsed
        // This prevents prompt injection attacks where malicious content
        // could include JSON that mimics a tool call.
        let response = r#"Sure, creating the file now.
{"name": "file_write", "arguments": {"path": "hello.py", "content": "print('hello')"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Sure, creating the file now."));
        assert_eq!(
            calls.len(),
            0,
            "Raw JSON without wrappers should not be parsed"
        );
    }

    #[test]
    fn build_tool_instructions_includes_all_tools() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let instructions = build_tool_instructions(&tools);

        assert!(instructions.contains("## Tool Use Protocol"));
        assert!(instructions.contains("<tool_call>"));
        assert!(instructions.contains("shell"));
        assert!(instructions.contains("file_read"));
        assert!(instructions.contains("file_write"));
    }

    #[test]
    fn tools_to_openai_format_produces_valid_schema() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let formatted = tools_to_openai_format(&tools);

        assert!(!formatted.is_empty());
        for tool_json in &formatted {
            assert_eq!(tool_json["type"], "function");
            assert!(tool_json["function"]["name"].is_string());
            assert!(tool_json["function"]["description"].is_string());
            assert!(!tool_json["function"]["name"].as_str().unwrap().is_empty());
        }
        // Verify known tools are present
        let names: Vec<&str> = formatted
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"file_read"));
    }

    #[test]
    fn trim_history_preserves_system_prompt() {
        let mut history = vec![ChatMessage::system("system prompt")];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 20 {
            history.push(ChatMessage::user(format!("msg {i}")));
        }
        let original_len = history.len();
        assert!(original_len > DEFAULT_MAX_HISTORY_MESSAGES + 1);

        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);

        // System prompt preserved
        assert_eq!(history[0].role, "system");
        assert_eq!(history[0].content, "system prompt");
        // Trimmed to limit
        assert_eq!(history.len(), DEFAULT_MAX_HISTORY_MESSAGES + 1); // +1 for system
                                                                     // Most recent messages preserved
        let last = &history[history.len() - 1];
        assert_eq!(
            last.content,
            format!("msg {}", DEFAULT_MAX_HISTORY_MESSAGES + 19)
        );
    }

    #[test]
    fn trim_history_noop_when_within_limit() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn build_compaction_transcript_formats_roles() {
        let messages = vec![
            ChatMessage::user("I like dark mode"),
            ChatMessage::assistant("Got it"),
        ];
        let transcript = build_compaction_transcript(&messages);
        assert!(transcript.contains("USER: I like dark mode"));
        assert!(transcript.contains("ASSISTANT: Got it"));
    }

    #[test]
    fn apply_compaction_summary_replaces_old_segment() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old 1"),
            ChatMessage::assistant("old 2"),
            ChatMessage::user("recent 1"),
            ChatMessage::assistant("recent 2"),
        ];

        apply_compaction_summary(&mut history, 1, 3, "- user prefers concise replies");

        assert_eq!(history.len(), 4);
        assert!(history[1].content.contains("Compaction summary"));
        assert!(history[2].content.contains("recent 1"));
        assert!(history[3].content.contains("recent 2"));
    }

    #[test]
    fn autosave_memory_key_has_prefix_and_uniqueness() {
        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        assert!(key1.starts_with("user_msg_"));
        assert!(key2.starts_with("user_msg_"));
        assert_ne!(key1, key2);
    }

    #[tokio::test]
    async fn autosave_memory_keys_preserve_multiple_turns() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        mem.store(&key1, "I'm Paul", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        mem.store(&key2, "I'm 45", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let recalled = mem.recall("45", 5, None).await.unwrap();
        assert!(recalled.iter().any(|entry| entry.content.contains("45")));
    }

    #[tokio::test]
    async fn build_context_ignores_legacy_assistant_autosave_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store(
            "assistant_resp_poisoned",
            "User suffered a fabricated event",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_msg_real",
            "User asked for concise status updates",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, "status updates", 0.0).await;
        assert!(context.contains("user_msg_real"));
        assert!(!context.contains("assistant_resp_poisoned"));
        assert!(!context.contains("fabricated event"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Tool Call Parsing Edge Cases
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_calls_handles_empty_tool_result() {
        // Recovery: Empty tool_result tag should be handled gracefully
        let response = r#"I'll run that command.
<tool_result name="shell">

</tool_result>
Done."#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Done."));
        assert!(calls.is_empty());
    }

    #[test]
    fn strip_tool_result_blocks_removes_single_block() {
        let input = r#"<tool_result name="memory_recall" status="ok">
{"matches":["hello"]}
</tool_result>
Here is my answer."#;
        assert_eq!(strip_tool_result_blocks(input), "Here is my answer.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_multiple_blocks() {
        let input = r#"<tool_result name="memory_recall" status="ok">
{"matches":[]}
</tool_result>
<tool_result name="shell" status="ok">
done
</tool_result>
Final answer."#;
        assert_eq!(strip_tool_result_blocks(input), "Final answer.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_prefix() {
        let input =
            "[Tool results]\n<tool_result name=\"shell\" status=\"ok\">\nok\n</tool_result>\nDone.";
        assert_eq!(strip_tool_result_blocks(input), "Done.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_thinking() {
        let input = "<thinking>\nLet me think...\n</thinking>\nHere is the answer.";
        assert_eq!(strip_tool_result_blocks(input), "Here is the answer.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_think_tags() {
        let input = "<think>\nLet me reason...\n</think>\nHere is the answer.";
        assert_eq!(strip_tool_result_blocks(input), "Here is the answer.");
    }

    #[test]
    fn strip_think_tags_removes_single_block() {
        assert_eq!(strip_think_tags("<think>reasoning</think>Hello"), "Hello");
    }

    #[test]
    fn strip_think_tags_removes_multiple_blocks() {
        assert_eq!(strip_think_tags("<think>a</think>X<think>b</think>Y"), "XY");
    }

    #[test]
    fn strip_think_tags_handles_unclosed_block() {
        assert_eq!(strip_think_tags("visible<think>hidden"), "visible");
    }

    #[test]
    fn strip_think_tags_preserves_text_without_tags() {
        assert_eq!(strip_think_tags("plain text"), "plain text");
    }

    #[test]
    fn parse_tool_calls_strips_think_before_tool_call() {
        // Qwen regression: <think> tags before <tool_call> tags should be
        // stripped, allowing the tool call to be parsed correctly.
        let response = "<think>I need to list files to understand the project</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n</tool_call>";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should parse tool call after stripping think tags"
        );
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls"
        );
        assert!(text.is_empty(), "think content should not appear as text");
    }

    #[test]
    fn parse_tool_calls_strips_think_only_returns_empty() {
        // When response is only <think> tags with no tool calls, should
        // return empty text and no calls.
        let response = "<think>Just thinking, no action needed</think>";
        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_qwen_think_with_multiple_tool_calls() {
        let response = "<think>I need to check two things</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n</tool_call>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n</tool_call>";
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert_eq!(
            calls[1].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn strip_tool_result_blocks_preserves_clean_text() {
        let input = "Hello, this is a normal response.";
        assert_eq!(strip_tool_result_blocks(input), input);
    }

    #[test]
    fn strip_tool_result_blocks_returns_empty_for_only_tags() {
        let input = "<tool_result name=\"memory_recall\" status=\"ok\">\n{}\n</tool_result>";
        assert_eq!(strip_tool_result_blocks(input), "");
    }

    #[test]
    fn parse_arguments_value_handles_null() {
        // Recovery: null arguments are returned as-is (Value::Null)
        let value = serde_json::json!(null);
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_null());
    }

    #[test]
    fn parse_tool_calls_handles_empty_tool_calls_array() {
        // Recovery: Empty tool_calls array returns original response (no tool parsing)
        let response = r#"{"content": "Hello", "tool_calls": []}"#;
        let (text, calls) = parse_tool_calls(response);
        // When tool_calls is empty, the entire JSON is returned as text
        assert!(text.contains("Hello"));
        assert!(calls.is_empty());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_malformed_payloads() {
        let response =
            "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}</tool_call>";
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(
            issue.is_some(),
            "malformed tool payload should be flagged for diagnostics"
        );
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_normal_text() {
        let issue = detect_tool_call_parse_issue("Thanks, done.", &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn parse_tool_calls_handles_whitespace_only_name() {
        // Recovery: Whitespace-only tool name should return None
        let value = serde_json::json!({"function": {"name": "   ", "arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_calls_handles_empty_string_arguments() {
        // Recovery: Empty string arguments should be handled
        let value = serde_json::json!({"name": "test", "arguments": ""});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - History Management
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn trim_history_with_no_system_prompt() {
        // Recovery: History without system prompt should trim correctly
        let mut history = vec![];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 20 {
            history.push(ChatMessage::user(format!("msg {i}")));
        }
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), DEFAULT_MAX_HISTORY_MESSAGES);
    }

    #[test]
    fn trim_history_preserves_role_ordering() {
        // Recovery: After trimming, role ordering should remain consistent
        let mut history = vec![ChatMessage::system("system")];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 10 {
            history.push(ChatMessage::user(format!("user {i}")));
            history.push(ChatMessage::assistant(format!("assistant {i}")));
        }
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[history.len() - 1].role, "assistant");
    }

    #[test]
    fn trim_history_with_only_system_prompt() {
        // Recovery: Only system prompt should not be trimmed
        let mut history = vec![ChatMessage::system("system prompt")];
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Arguments Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_arguments_value_handles_invalid_json_string() {
        // Recovery: Invalid JSON string should return empty object
        let value = serde_json::Value::String("not valid json".to_string());
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_arguments_value_handles_none() {
        // Recovery: None arguments should return empty object
        let result = parse_arguments_value(None);
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - JSON Extraction
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn extract_json_values_handles_empty_string() {
        // Recovery: Empty input should return empty vec
        let result = extract_json_values("");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_whitespace_only() {
        // Recovery: Whitespace only should return empty vec
        let result = extract_json_values("   \n\t  ");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_multiple_objects() {
        // Recovery: Multiple JSON objects should all be extracted
        let input = r#"{"a": 1}{"b": 2}{"c": 3}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn extract_json_values_handles_arrays() {
        // Recovery: JSON arrays should be extracted
        let input = r#"[1, 2, 3]{"key": "value"}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 2);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Constants Validation
    // ═══════════════════════════════════════════════════════════════════════

    const _: () = {
        assert!(DEFAULT_MAX_TOOL_ITERATIONS > 0);
        assert!(DEFAULT_MAX_TOOL_ITERATIONS <= 100);
        assert!(DEFAULT_MAX_HISTORY_MESSAGES > 0);
        assert!(DEFAULT_MAX_HISTORY_MESSAGES <= 1000);
    };

    #[test]
    fn constants_bounds_are_compile_time_checked() {
        // Bounds are enforced by the const assertions above.
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Tool Call Value Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_call_value_handles_missing_name_field() {
        // Recovery: Missing name field should return None
        let value = serde_json::json!({"function": {"arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_call_value_handles_top_level_name() {
        // Recovery: Tool call with name at top level (non-OpenAI format)
        let value = serde_json::json!({"name": "test_tool", "arguments": {}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test_tool");
    }

    #[test]
    fn parse_tool_call_value_accepts_top_level_parameters_alias() {
        let value = serde_json::json!({
            "name": "schedule",
            "parameters": {"action": "create", "message": "test"}
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "schedule");
        assert_eq!(
            result.arguments.get("action").and_then(|v| v.as_str()),
            Some("create")
        );
    }

    #[test]
    fn parse_tool_call_value_accepts_function_parameters_alias() {
        let value = serde_json::json!({
            "function": {
                "name": "shell",
                "parameters": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("date")
        );
    }

    #[test]
    fn parse_tool_call_value_preserves_tool_call_id_aliases() {
        let value = serde_json::json!({
            "call_id": "legacy_1",
            "function": {
                "name": "shell",
                "arguments": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.tool_call_id.as_deref(), Some("legacy_1"));
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_empty_array() {
        // Recovery: Empty tool_calls array should return empty vec
        let value = serde_json::json!({"tool_calls": []});
        let result = parse_tool_calls_from_json_value(&value);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_missing_tool_calls() {
        // Recovery: Missing tool_calls field should fall through
        let value = serde_json::json!({"name": "test", "arguments": {}});
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_top_level_array() {
        // Recovery: Top-level array of tool calls
        let value = serde_json::json!([
            {"name": "tool_a", "arguments": {}},
            {"name": "tool_b", "arguments": {}}
        ]);
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 2);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // GLM-Style Tool Call Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_glm_style_browser_open_url() {
        let response = "browser_open/url>https://example.com";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert!(calls[0].1["command"].as_str().unwrap().contains("curl"));
        assert!(calls[0].1["command"]
            .as_str()
            .unwrap()
            .contains("example.com"));
    }

    #[test]
    fn parse_glm_style_shell_command() {
        let response = "shell/command>ls -la";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "ls -la");
    }

    #[test]
    fn parse_glm_style_http_request() {
        let response = "http_request/url>https://api.example.com/data";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "http_request");
        assert_eq!(calls[0].1["url"], "https://api.example.com/data");
        assert_eq!(calls[0].1["method"], "GET");
    }

    #[test]
    fn parse_glm_style_plain_url() {
        let response = "https://example.com/api";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert!(calls[0].1["command"].as_str().unwrap().contains("curl"));
    }

    #[test]
    fn parse_glm_style_json_args() {
        let response = r#"shell/{"command": "echo hello"}"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "echo hello");
    }

    #[test]
    fn parse_glm_style_multiple_calls() {
        let response = r#"shell/command>ls
browser_open/url>https://example.com"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn parse_glm_style_tool_call_integration() {
        // Integration test: GLM format should be parsed in parse_tool_calls
        let response = "Checking...\nbrowser_open/url>https://example.com\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(text.contains("Checking"));
        assert!(text.contains("Done"));
    }

    #[test]
    fn parse_glm_style_rejects_non_http_url_param() {
        let response = "browser_open/url>javascript:alert(1)";
        let calls = parse_glm_style_tool_calls(response);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_unclosed_tool_call_tag() {
        let response = "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert_eq!(text, "Done");
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): parse_tool_calls robustness — malformed/edge-case inputs
    // Prevents: Pattern 4 issues #746, #418, #777, #848
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_tool_calls_empty_input_returns_empty() {
        let (text, calls) = parse_tool_calls("");
        assert!(calls.is_empty(), "empty input should produce no tool calls");
        assert!(text.is_empty(), "empty input should produce no text");
    }

    #[test]
    fn parse_tool_calls_whitespace_only_returns_empty_calls() {
        let (text, calls) = parse_tool_calls("   \n\t  ");
        assert!(calls.is_empty());
        assert!(text.is_empty() || text.trim().is_empty());
    }

    #[test]
    fn parse_tool_calls_nested_xml_tags_handled() {
        // Double-wrapped tool call should still parse the inner call
        let response = r#"<tool_call><tool_call>{"name":"echo","arguments":{"msg":"hi"}}</tool_call></tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        // Should find at least one tool call
        assert!(
            !calls.is_empty(),
            "nested XML tags should still yield at least one tool call"
        );
    }

    #[test]
    fn parse_tool_calls_truncated_json_no_panic() {
        // Incomplete JSON inside tool_call tags
        let response = r#"<tool_call>{"name":"shell","arguments":{"command":"ls"</tool_call>"#;
        let (_text, _calls) = parse_tool_calls(response);
        // Should not panic — graceful handling of truncated JSON
    }

    #[test]
    fn parse_tool_calls_empty_json_object_in_tag() {
        let response = "<tool_call>{}</tool_call>";
        let (_text, calls) = parse_tool_calls(response);
        // Empty JSON object has no name field — should not produce valid tool call
        assert!(
            calls.is_empty(),
            "empty JSON object should not produce a tool call"
        );
    }

    #[test]
    fn parse_tool_calls_closing_tag_only_returns_text() {
        let response = "Some text </tool_call> more text";
        let (text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "closing tag only should not produce calls"
        );
        assert!(
            !text.is_empty(),
            "text around orphaned closing tag should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_very_large_arguments_no_panic() {
        let large_arg = "x".repeat(100_000);
        let response = format!(
            r#"<tool_call>{{"name":"echo","arguments":{{"message":"{}"}}}}</tool_call>"#,
            large_arg
        );
        let (_text, calls) = parse_tool_calls(&response);
        assert_eq!(calls.len(), 1, "large arguments should still parse");
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_special_characters_in_arguments() {
        let response = r#"<tool_call>{"name":"echo","arguments":{"message":"hello \"world\" <>&'\n\t"}}</tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_text_with_embedded_json_not_extracted() {
        // Raw JSON without any tags should NOT be extracted as a tool call
        let response = r#"Here is some data: {"name":"echo","arguments":{"message":"hi"}} end."#;
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "raw JSON in text without tags should not be extracted"
        );
    }

    #[test]
    fn parse_tool_calls_multiple_formats_mixed() {
        // Mix of text and properly tagged tool call
        let response = r#"I'll help you with that.

<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>

Let me check the result."#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should extract one tool call from mixed content"
        );
        assert_eq!(calls[0].name, "shell");
        assert!(
            text.contains("help you"),
            "text before tool call should be preserved"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): scrub_credentials edge cases
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn scrub_credentials_empty_input() {
        let result = scrub_credentials("");
        assert_eq!(result, "");
    }

    #[test]
    fn scrub_credentials_no_sensitive_data() {
        let input = "normal text without any secrets";
        let result = scrub_credentials(input);
        assert_eq!(
            result, input,
            "non-sensitive text should pass through unchanged"
        );
    }

    #[test]
    fn scrub_credentials_multibyte_chars_no_panic() {
        // Regression test for #3024: byte index 4 is not a char boundary
        // when the captured value contains multi-byte UTF-8 characters.
        // The regex only matches quoted values for non-ASCII content, since
        // capture group 4 is restricted to [a-zA-Z0-9_\-\.].
        let input = "password=\"\u{4f60}\u{7684}WiFi\u{5bc6}\u{7801}ab\"";
        let result = scrub_credentials(input);
        assert!(
            result.contains("[REDACTED]"),
            "multi-byte quoted value should be redacted without panic, got: {result}"
        );
    }

    #[test]
    fn scrub_credentials_short_values_not_redacted() {
        // Values shorter than 8 chars should not be redacted
        let input = r#"api_key="short""#;
        let result = scrub_credentials(input);
        assert_eq!(result, input, "short values should not be redacted");
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): trim_history edge cases
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn trim_history_empty_history() {
        let mut history: Vec<crate::providers::ChatMessage> = vec![];
        trim_history(&mut history, 10);
        assert!(history.is_empty());
    }

    #[test]
    fn trim_history_system_only() {
        let mut history = vec![crate::providers::ChatMessage::system("system prompt")];
        trim_history(&mut history, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "system");
    }

    #[test]
    fn trim_history_exactly_at_limit() {
        let mut history = vec![
            crate::providers::ChatMessage::system("system"),
            crate::providers::ChatMessage::user("msg 1"),
            crate::providers::ChatMessage::assistant("reply 1"),
        ];
        trim_history(&mut history, 2); // 2 non-system messages = exactly at limit
        assert_eq!(history.len(), 3, "should not trim when exactly at limit");
    }

    #[test]
    fn trim_history_removes_oldest_non_system() {
        let mut history = vec![
            crate::providers::ChatMessage::system("system"),
            crate::providers::ChatMessage::user("old msg"),
            crate::providers::ChatMessage::assistant("old reply"),
            crate::providers::ChatMessage::user("new msg"),
            crate::providers::ChatMessage::assistant("new reply"),
        ];
        trim_history(&mut history, 2);
        assert_eq!(history.len(), 3); // system + 2 kept
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].content, "new msg");
    }

    /// When `build_system_prompt_with_mode` is called with `native_tools = true`,
    /// the output must contain ZERO XML protocol artifacts. In the native path
    /// `build_tool_instructions` is never called, so the system prompt alone
    /// must be clean of XML tool-call protocol.
    #[test]
    fn native_tools_system_prompt_contains_zero_xml() {
        use crate::channels::build_system_prompt_with_mode;

        let tool_summaries: Vec<(&str, &str)> = vec![
            ("shell", "Execute shell commands"),
            ("file_read", "Read files"),
        ];

        let system_prompt = build_system_prompt_with_mode(
            std::path::Path::new("/tmp"),
            "test-model",
            &tool_summaries,
            &[],  // no skills
            None, // no identity config
            None, // no bootstrap_max_chars
            true, // native_tools
            crate::config::SkillsPromptInjectionMode::Full,
        );

        // Must contain zero XML protocol artifacts
        assert!(
            !system_prompt.contains("<tool_call>"),
            "Native prompt must not contain <tool_call>"
        );
        assert!(
            !system_prompt.contains("</tool_call>"),
            "Native prompt must not contain </tool_call>"
        );
        assert!(
            !system_prompt.contains("<tool_result>"),
            "Native prompt must not contain <tool_result>"
        );
        assert!(
            !system_prompt.contains("</tool_result>"),
            "Native prompt must not contain </tool_result>"
        );
        assert!(
            !system_prompt.contains("## Tool Use Protocol"),
            "Native prompt must not contain XML protocol header"
        );

        // Positive: native prompt should still list tools and contain task instructions
        assert!(
            system_prompt.contains("shell"),
            "Native prompt must list tool names"
        );
        assert!(
            system_prompt.contains("## Your Task"),
            "Native prompt should contain task instructions"
        );
    }

    // ── Cross-Alias & GLM Shortened Body Tests ──────────────────────────

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_json() {
        // <tool_call> opened but closed with </invoke> — JSON body
        let input = r#"<tool_call>{"name": "shell", "arguments": {"command": "ls"}}</invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_glm_shortened() {
        // <tool_call>shell>uname -a</invoke> — GLM shortened inside cross-alias tags
        let input = "<tool_call>shell>uname -a</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_shortened_body_in_matched_tags() {
        // <tool_call>shell>pwd</tool_call> — GLM shortened in matched tags
        let input = "<tool_call>shell>pwd</tool_call>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_yaml_style_in_tags() {
        // <tool_call>shell>\ncommand: date\napproved: true</invoke>
        let input = "<tool_call>shell>\ncommand: date\napproved: true</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert_eq!(calls[0].arguments["approved"], true);
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_attribute_style_in_tags() {
        // <tool_call>shell command="date" /></tool_call>
        let input = r#"<tool_call>shell command="date" /></tool_call>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_file_read_shortened_in_cross_alias() {
        // <tool_call>file_read path=".env" /></invoke>
        let input = r#"<tool_call>file_read path=".env" /></invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments["path"], ".env");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_unclosed_glm_shortened_no_close_tag() {
        // <tool_call>shell>ls -la (no close tag at all)
        let input = "<tool_call>shell>ls -la";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_text_before_cross_alias() {
        // Text before and after cross-alias tool call
        let input = "Let me check that.\n<tool_call>shell>uname -a</invoke>\nDone.";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.contains("Let me check that."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_glm_shortened_body_url_to_curl() {
        // URL values for shell should be wrapped in curl
        let call = parse_glm_shortened_body("shell>https://example.com/api").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_browser_open_maps_to_shell_command() {
        // browser_open aliases to shell, and shortened calls must still emit
        // shell's canonical "command" argument.
        let call = parse_glm_shortened_body("browser_open>https://example.com").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_memory_recall() {
        // memory_recall>some query — default param is "query"
        let call = parse_glm_shortened_body("memory_recall>recent meetings").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "recent meetings");
    }

    #[test]
    fn parse_glm_shortened_body_function_style_alias_maps_to_message_send() {
        let call =
            parse_glm_shortened_body(r#"sendmessage(channel="alerts", message="hi")"#).unwrap();
        assert_eq!(call.name, "message_send");
        assert_eq!(call.arguments["channel"], "alerts");
        assert_eq!(call.arguments["message"], "hi");
    }

    #[test]
    fn map_tool_name_alias_direct_coverage() {
        assert_eq!(map_tool_name_alias("bash"), "shell");
        assert_eq!(map_tool_name_alias("filelist"), "file_list");
        assert_eq!(map_tool_name_alias("memorystore"), "memory_store");
        assert_eq!(map_tool_name_alias("memoryforget"), "memory_forget");
        assert_eq!(map_tool_name_alias("http"), "http_request");
        assert_eq!(
            map_tool_name_alias("totally_unknown_tool"),
            "totally_unknown_tool"
        );
    }

    #[test]
    fn default_param_for_tool_coverage() {
        assert_eq!(default_param_for_tool("shell"), "command");
        assert_eq!(default_param_for_tool("bash"), "command");
        assert_eq!(default_param_for_tool("file_read"), "path");
        assert_eq!(default_param_for_tool("memory_recall"), "query");
        assert_eq!(default_param_for_tool("memory_store"), "content");
        assert_eq!(default_param_for_tool("http_request"), "url");
        assert_eq!(default_param_for_tool("browser_open"), "url");
        assert_eq!(default_param_for_tool("unknown_tool"), "input");
    }

    #[test]
    fn parse_glm_shortened_body_rejects_empty() {
        assert!(parse_glm_shortened_body("").is_none());
        assert!(parse_glm_shortened_body("   ").is_none());
    }

    #[test]
    fn parse_glm_shortened_body_rejects_invalid_tool_name() {
        // Tool names with special characters should be rejected
        assert!(parse_glm_shortened_body("not-a-tool>value").is_none());
        assert!(parse_glm_shortened_body("tool name>value").is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests for history builders
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_native_assistant_history_includes_reasoning_content() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }];
        let result = build_native_assistant_history("answer", &calls, Some("thinking step"));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("thinking step"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_omits_reasoning_content_when_none() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }];
        let result = build_native_assistant_history("answer", &calls, None);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_includes_reasoning_content() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls(
            "answer",
            &calls,
            Some("deep thought"),
        );
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("deep thought"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_omits_reasoning_content_when_none() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls("answer", &calls, None);
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }
